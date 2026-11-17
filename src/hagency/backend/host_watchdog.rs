//! Agent host watchdog. The per-Agent service watcher in the native owner ends
//! for good on any authorization-class reply (401/403/404/409, or a device
//! check that is not plainly "unavailable"); a server hiccup overnight was
//! enough to stop it, and with it every Room worker and `rinx guardian`
//! child, while the saved start consent stayed. Nothing restarted it, so
//! requests only queued. This worker re-enters the same consented startup
//! recovery as window launch: a live watcher answers
//! `agent_runtime_already_active`, a dead one is started again through every
//! fresh device/binding/provider gate. Takeover is never requested.
use super::*;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// How often a healthy host is checked.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// The longest wait after repeated failed restarts.
const MAX_BACKOFF: Duration = Duration::from_secs(300);
/// The reply of a start that found its watcher still running.
const ALREADY_RUNNING: &str = "agent_runtime_already_active";

/// What one recovery pass found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Pass {
    /// Agents whose host had stopped and was started again.
    pub restarted: Vec<String>,
    /// Agents whose host is stopped and could not start, with the code.
    pub failed: Vec<(String, String)>,
}

/// Read a `restore_runtime_intents` reply. Only engine codes are kept.
pub(super) fn read_pass(value: &Value) -> Pass {
    let mut pass = Pass::default();
    for item in value["recoveries"].as_array().into_iter().flatten() {
        let agent = item["agentId"].as_str().unwrap_or("").to_owned();
        if item["restored"] == true {
            pass.restarted.push(agent);
        } else {
            let code = item["error"].as_str().unwrap_or("");
            if code != ALREADY_RUNNING {
                pass.failed.push((agent, recovery_error_code(code).to_owned()));
            }
        }
    }
    pass
}

/// The wait before the next pass: the normal period while healthy, doubling
/// per consecutive failing pass up to `MAX_BACKOFF`.
pub(super) fn next_delay(failures: u32) -> Duration {
    CHECK_EVERY.saturating_mul(2u32.saturating_pow(failures.min(4))).min(MAX_BACKOFF)
}

#[derive(Default)]
struct Control {
    retired: AtomicBool,
    wake: tokio::sync::Notify,
}

#[derive(Default)]
pub(super) struct Worker {
    control: Arc<Control>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Worker {
    pub(super) fn retire(&self) {
        self.control.retired.store(true, Ordering::Release);
        self.control.wake.notify_one();
    }
    pub(super) async fn drain(&self) {
        let mut task = self.task.lock().await;
        if let Some(handle) = task.as_mut() {
            // A join error means the task panicked or was aborted; either way
            // it no longer runs, which is all draining needs.
            let _ = handle.await;
        }
        task.take();
    }
}

impl Service {
    /// Start the watchdog for this window's in-process personal owner.
    pub(super) async fn start_host_watchdog(&self, owner: &NativeOwner) {
        let control = self.0.host_watchdog.control.clone();
        if self.check().is_err() || control.retired.load(Ordering::Acquire) {
            return;
        }
        let mut task = self.0.host_watchdog.task.lock().await;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let owner = owner.clone();
        let alive = move || weak.upgrade().is_some_and(|inner| Service(inner).check().is_ok());
        *task = Some(crate::sliding_sync::backend_runtime().spawn(async move {
            watch(control, alive, next_delay, move || {
                let owner = owner.clone();
                async move { owner.restore_runtime_intents().await.map_err(|e| e.code) }
            })
            .await;
        }));
    }
}

/// The watchdog loop, over any recovery pass so the flow is testable.
async fn watch<F, Fut>(control: Arc<Control>, alive: impl Fn() -> bool, delay: fn(u32) -> Duration, recover: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let mut failures = 0u32;
    loop {
        tokio::select! { biased;
            _ = control.wake.notified() => {},
            _ = tokio::time::sleep(delay(failures)) => {},
        }
        if control.retired.load(Ordering::Acquire) || !alive() {
            break;
        }
        match recover().await {
            Ok(value) => {
                let pass = read_pass(&value);
                for agent in &pass.restarted {
                    makepad_widgets::warning!("Agent host for {agent} was not running on this Mac; restarted it");
                }
                for (agent, code) in &pass.failed {
                    makepad_widgets::warning!("Agent host for {agent} is not running and could not restart: {code}");
                }
                failures = if pass.failed.is_empty() { 0 } else { failures.saturating_add(1) };
                if !pass.restarted.is_empty() { makepad_widgets::SignalToUI::set_ui_signal(); }
            }
            Err(code) => {
                let code = recovery_error_code(&code);
                makepad_widgets::warning!("Agent host check unavailable: {code}");
                failures = failures.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
#[path = "host_watchdog_tests.rs"]
mod tests;
