//! The owner's kill switch: one action that stops every Agent this device
//! runs for the signed-in owner, and one that resumes them.
//!
//! It adds no stop mechanism of its own. Stop all, in this order:
//! 1. records "paused by you" on disk first, so a crash or restart mid-way
//!    still leaves the Agents paused;
//! 2. sends "Stop this run" for every active run card the owner may stop;
//! 3. answers Deny to each waiting approval of this device's Agents, while
//!    their runtimes are still there to receive it;
//! 4. stops each Agent's service (`StopAgentService`, the Agent page's Stop),
//!    which ends its watcher and runtime and removes its saved start intent,
//!    so nothing takes new requests or restarts it until the owner resumes.
//!
//! Resume all starts exactly the Agents the switch stopped and forgets them.
use crate::hagency::model::{Command, LocalAction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The file in the app data folder that keeps the paused state.
pub const STATE_FILE: &str = "agents-paused-by-owner.json";
/// The approval decision the switch sends.
const DENY: &str = "deny";
/// Reported when the paused state could not be written.
pub const PAUSE_UNSAVED: &str = "agents_pause_unsaved";
/// Reported when a waiting approval could not be listed or denied.
pub const APPROVALS_UNANSWERED: &str = "agents_approvals_unanswered";
/// Returned while another Stop all or Resume all runs.
pub const BUSY_CODE: &str = "agents_switch_busy";
/// Returned when an Agent is started while the owner paused it.
pub use ::hagency::console::start_fence::PAUSED_CODE;

/// Which of the owner's Agents the switch stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paused {
    pub owner: String,
    pub agents: Vec<String>,
}

/// What one Stop all or Resume all did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub agents: usize,
    pub runs: usize,
    pub denied: usize,
    pub failed: Vec<String>,
}
impl Report {
    pub fn to_json(&self, paused: bool) -> Value {
        json!({"paused": paused, "agents": self.agents, "runs": self.runs, "denied": self.denied, "failed": self.failed})
    }
}

/// What the switch drives. `Live` is the app; tests use a recorder.
pub trait Agents {
    /// Active Agents of this owner that run on this device.
    fn local_agents(&self) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    /// Waiting approval rows of this device's Agents.
    fn pending_approvals(&self) -> impl Future<Output = Result<Vec<Value>, String>> + Send;
    fn deny(&self, row: &Value) -> impl Future<Output = Result<(), String>> + Send;
    fn set_running(&self, agent: &str, running: bool) -> impl Future<Output = Result<(), String>> + Send;
    /// Send "Stop this run" for every active run; returns how many.
    fn stop_runs(&self) -> usize;
}

/// The pause file lives in the owner daemon's home, which Desktop and the
/// daemon share; the daemon's own `--home` argument wins inside the daemon.
pub fn default_path() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let args: Vec<_> = std::env::args_os().collect();
        let home = args.windows(2).find(|pair| pair[0] == "--home").map(|pair| PathBuf::from(&pair[1]));
        home.or_else(|| ::hagency::service::default_daemon_dir().ok())
            .unwrap_or_else(|| crate::app_data_dir().to_owned())
            .join(STATE_FILE)
    }).clone()
}

/// The paused state of `owner`, or None when the owner is not paused.
pub fn load(path: &Path, owner: &str) -> Option<Paused> {
    let bytes = std::fs::read(path).ok()?;
    // No file means not paused; a file of another owner never pauses this one.
    let paused: Paused = serde_json::from_slice(&bytes).ok()?;
    (paused.owner == owner).then_some(paused)
}

fn save(path: &Path, paused: &Paused) -> Result<(), String> {
    let bytes = serde_json::to_vec(paused).map_err(|_| PAUSE_UNSAVED)?;
    let temp = path.with_extension("json.tmp");
    // The owner daemon's home may not exist yet (the in-process owner never
    // creates it): make it, or the pause is never saved (Nov 15 on screen).
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| PAUSE_UNSAVED)?;
    }
    std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, path)).map_err(|_| PAUSE_UNSAVED.into())
}

/// Whether the file at `path` pauses `agent`, whichever owner wrote it:
/// Agent IDs are the owner's own, and a start must fail closed.
/// A file that exists but cannot be read or parsed refuses every start.
pub fn paused_in(path: &Path, agent: &str) -> bool {
    match std::fs::read(path) {
        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        Ok(bytes) => serde_json::from_slice::<Paused>(&bytes).map_or(true, |p| p.agents.iter().any(|a| a == agent)),
    }
}
/// Whether this device's pause file pauses `agent` (unreadable counts as paused).
pub(crate) fn paused_here(agent: &str) -> bool {
    paused_in(&default_path(), agent)
}
/// The one start check: `command` would start an Agent the owner paused.
/// Desktop's owner service applies it to every command it sends (the Agent
/// panel's Start, Create agent, assignment, My Agents); the native engine
/// applies the same check through `install_fence`, boot recovery included.
pub fn refuses(command: &Command) -> bool {
    refuses_in(&default_path(), command)
}
fn refuses_in(path: &Path, command: &Command) -> bool {
    matches!(command, Command::Local { agent_id, action: LocalAction::StartAgentService | LocalAction::RuntimeStart, .. } if paused_in(path, agent_id))
}
/// Install the check in the native owner engine of this process (Desktop
/// or the owner daemon). Call once, before anything starts an Agent.
pub fn install_fence() {
    ::hagency::console::start_fence::install(paused_here);
}

/// Whether the switch stopped `agent` of `owner`.
pub fn is_paused(path: &Path, owner: &str, agent: &str) -> bool {
    load(path, owner).is_some_and(|p| p.agents.iter().any(|a| a == agent))
}

/// Stop every local Agent of `owner`. The paused state is saved before any
/// Agent is touched. The owner's stop wins even when it cannot be saved: the
/// Agents still stop, and the report names `PAUSE_UNSAVED`.
pub async fn stop_all(agents: &impl Agents, path: &Path, owner: &str) -> Result<Report, String> {
    let _busy = Busy::take()?;
    let ids = agents.local_agents().await?;
    let mut paused = load(path, owner).unwrap_or(Paused { owner: owner.into(), agents: Vec::new() });
    for id in &ids {
        if !paused.agents.contains(id) {
            paused.agents.push(id.clone());
        }
    }
    let saved = save(path, &paused);
    let mut report = Report { agents: ids.len(), runs: agents.stop_runs(), ..Report::default() };
    if saved.is_err() {
        report.failed.push(PAUSE_UNSAVED.into());
    }
    deny_pending(agents, &mut report).await;
    for id in &ids {
        if agents.set_running(id, false).await.is_err() {
            report.failed.push(id.clone());
        }
    }
    // A request that arrived while the services were stopping: answer it too.
    deny_pending(agents, &mut report).await;
    Ok(report)
}

/// Deny every waiting approval; a failed listing or denial is reported.
async fn deny_pending(agents: &impl Agents, report: &mut Report) {
    match agents.pending_approvals().await {
        Ok(rows) => for row in rows {
            match agents.deny(&row).await {
                Ok(()) => report.denied += 1,
                Err(_) => report.failed.push(APPROVALS_UNANSWERED.into()),
            }
        },
        Err(_) => report.failed.push(APPROVALS_UNANSWERED.into()),
    }
    report.failed.dedup();
}

/// One Stop all or Resume all at a time, whatever surface asked.
struct Busy;
static BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
impl Busy {
    fn take() -> Result<Self, String> {
        use std::sync::atomic::Ordering;
        BUSY.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).map(|_| Busy).map_err(|_| BUSY_CODE.into())
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Start again the Agents the switch stopped. An Agent that fails to start
/// stays recorded as paused, so Resume all can be tried again.
pub async fn resume_all(agents: &impl Agents, path: &Path, owner: &str) -> Result<Report, String> {
    let _busy = Busy::take()?;
    let Some(mut paused) = load(path, owner) else { return Ok(Report::default()) };
    let mut report = Report { agents: paused.agents.len(), ..Report::default() };
    // One Agent at a time: only the Agent being started leaves the pause, and
    // the file always records what is still paused, so a crash fails closed.
    for id in paused.agents.clone() {
        paused.agents.retain(|a| *a != id);
        save_or_forget(path, &paused)?;
        if agents.set_running(&id, true).await.is_err() {
            report.failed.push(id.clone());
            paused.agents.push(id);
            save_or_forget(path, &paused)?;
        }
    }
    Ok(report)
}
fn save_or_forget(path: &Path, paused: &Paused) -> Result<(), String> {
    if paused.agents.is_empty() {
        std::fs::remove_file(path).map_err(|_| PAUSE_UNSAVED.into())
    } else {
        save(path, paused)
    }
}

/// At sign-in: stop again every Agent the file says is paused, in case the
/// app stopped mid-way through Stop all and a saved start intent survived.
pub async fn enforce_on_boot(agents: &impl Agents, path: &Path, owner: &str) -> Report {
    let ids = load(path, owner).map(|p| p.agents).unwrap_or_default();
    let mut report = Report { agents: ids.len(), ..Report::default() };
    for id in &ids {
        if agents.set_running(id, false).await.is_err() {
            report.failed.push(id.clone());
        }
    }
    report
}

/// Run `enforce_on_boot` once per signed-in owner, off the UI thread.
/// A failed stop is retried on a later tick; the start check holds meanwhile.
pub fn enforce_at_sign_in() {
    static DONE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let Some(owner) = crate::sliding_sync::current_user_id().map(|id| id.to_string()) else { return };
    let service = {
        let mut done = DONE.lock().unwrap_or_else(|e| e.into_inner());
        if done.as_deref() == Some(owner.as_str()) || load(&default_path(), &owner).is_none() {
            return;
        }
        let Ok(service) = crate::hagency::backend::Service::for_current() else { return };
        *done = Some(owner.clone());
        service
    };
    crate::sliding_sync::spawn_async_task(async move {
        // Nothing to show: the list already says "Paused by you". A failed
        // stop is tried again on a later tick.
        if !enforce_on_boot(&Live(service), &default_path(), &owner).await.failed.is_empty() {
            *DONE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    });
}

/// The active Agents of `agents` (a `Command::Agents` answer) assigned to
/// this device (a `Command::Devices` answer).
pub fn here(agents: &Value, devices: &Value) -> Vec<String> {
    let current = devices["currentDeviceId"].as_str().unwrap_or_default();
    let live = |id: &str| devices["devices"].as_array().is_some_and(|all| all.iter().any(|d| d["id"] == id && d["revoked"] == false));
    if current.is_empty() || !live(current) {
        return Vec::new();
    }
    agents["agents"].as_array().into_iter().flatten()
        .filter(|a| a["state"] == "active" && a["executionDeviceId"] == current)
        .filter_map(|a| a["id"].as_str().map(str::to_owned))
        .collect()
}

/// The app's own Agents, through the same native owner service as the
/// Agent page, its room approval cards and its run cards.
pub struct Live(pub crate::hagency::backend::Service);
impl Agents for Live {
    async fn local_agents(&self) -> Result<Vec<String>, String> {
        let agents = self.0.execute(Command::Agents).await?;
        let devices = self.0.execute(Command::Devices).await?;
        Ok(here(&agents, &devices))
    }
    async fn pending_approvals(&self) -> Result<Vec<Value>, String> {
        // An unreadable queue is logged by `read`; stopping the Agent's
        // service below ends its waiting requests anyway.
        crate::hagency::local_approvals::read(&self.0).await.map(|(rows, _)| rows)
    }
    async fn deny(&self, row: &Value) -> Result<(), String> {
        let command = crate::hagency::local_approvals::decision_command(row, DENY).ok_or("approval_not_local")?;
        self.0.execute(command).await.map(|_| ())
    }
    async fn set_running(&self, agent: &str, running: bool) -> Result<(), String> {
        let action = if running { LocalAction::StartAgentService } else { LocalAction::StopAgentService };
        let input = running.then(|| json!({}));
        self.0.execute(Command::Local { agent_id: agent.into(), action, input, binding_id: None, requester: None }).await.map(|_| ())
    }
    fn stop_runs(&self) -> usize {
        crate::hagency::run_controls::stop_all_running()
    }
}

#[cfg(test)]
#[path = "kill_switch_tests.rs"]
mod tests;
