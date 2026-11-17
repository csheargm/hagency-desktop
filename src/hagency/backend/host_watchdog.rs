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
const MAX_BACKOFF: Duration = Duration::from_secs(120);
/// The reply of a start that found its watcher still running.
const ALREADY_RUNNING: &str = "agent_runtime_already_active";

/// What one recovery pass found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Pass {
    /// Agents whose host had stopped and was started again.
    pub restarted: Vec<String>,
    /// Agents whose check or restart failed, with the code. A transient
    /// server error lands here too, so this is not proof the host is down.
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
    /// Whether the cached owner is this window's personal owner, the only
    /// one whose saved consent startup recovery restores.
    personal: Arc<AtomicBool>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Worker {
    pub(super) fn retire(&self) {
        self.control.retired.store(true, Ordering::Release);
        self.control.wake.notify_waiters();
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
    /// Record which kind of owner was just opened, and start the watchdog for
    /// a personal one. The task holds no owner between passes: each pass
    /// borrows the currently cached owner, so a stale owner's root is never
    /// kept open and a replacement owner is watched without a new task.
    pub(super) async fn start_host_watchdog(&self, personal: bool) {
        let worker = &self.0.host_watchdog;
        worker.personal.store(personal, Ordering::Release);
        if !personal || self.check().is_err() || worker.control.retired.load(Ordering::Acquire) {
            return;
        }
        let mut task = worker.task.lock().await;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return;
        }
        let (control, weak) = (worker.control.clone(), Arc::downgrade(&self.0));
        let alive = {
            let weak = weak.clone();
            move || weak.upgrade().is_some_and(|inner| Service(inner).check().is_ok())
        };
        *task = Some(crate::sliding_sync::backend_runtime().spawn(watch(control, alive, next_delay, move || {
            let weak = weak.clone();
            async move {
                let Some(inner) = weak.upgrade() else { return Ok(serde_json::json!({})) };
                let owner = inner.native.lock().await.clone();
                let personal = inner.host_watchdog.personal.load(Ordering::Acquire);
                drop(inner);
                match owner {
                    Some(owner) if personal && !owner.is_retired() => owner.restore_runtime_intents().await.map_err(|e| e.code),
                    // No open owner (or a management-only one): nothing to watch now.
                    _ => Ok(serde_json::json!({})),
                }
            }
        })));
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
        if control.retired.load(Ordering::Acquire) {
            break;
        }
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
                    makepad_widgets::warning!("Agent host check for {agent} failed: {code}");
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
