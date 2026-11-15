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

pub fn default_path() -> PathBuf {
    crate::app_data_dir().join(STATE_FILE)
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
    std::fs::write(&temp, bytes).and_then(|_| std::fs::rename(&temp, path)).map_err(|_| PAUSE_UNSAVED.into())
}

/// Whether the switch stopped `agent` of `owner`.
pub fn is_paused(path: &Path, owner: &str, agent: &str) -> bool {
    load(path, owner).is_some_and(|p| p.agents.iter().any(|a| a == agent))
}

/// Stop every local Agent of `owner`. The paused state is saved before any
/// Agent is touched. The owner's stop wins even when it cannot be saved: the
/// Agents still stop, and the report names `PAUSE_UNSAVED`.
pub async fn stop_all(agents: &impl Agents, path: &Path, owner: &str) -> Result<Report, String> {
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
    // A listing failure is not fatal: stopping the services below ends every
    // waiting request anyway, and must not be held back by it.
    for row in agents.pending_approvals().await.unwrap_or_default() {
        if agents.deny(&row).await.is_ok() {
            report.denied += 1;
        }
    }
    for id in &ids {
        if agents.set_running(id, false).await.is_err() {
            report.failed.push(id.clone());
        }
    }
    Ok(report)
}

/// Start again the Agents the switch stopped. An Agent that fails to start
/// stays recorded as paused, so Resume all can be tried again.
pub async fn resume_all(agents: &impl Agents, path: &Path, owner: &str) -> Result<Report, String> {
    let Some(mut paused) = load(path, owner) else { return Ok(Report::default()) };
    let mut report = Report { agents: paused.agents.len(), ..Report::default() };
    for id in &paused.agents {
        if agents.set_running(id, true).await.is_err() {
            report.failed.push(id.clone());
        }
    }
    if report.failed.is_empty() {
        std::fs::remove_file(path).map_err(|_| PAUSE_UNSAVED)?;
    } else {
        paused.agents = report.failed.clone();
        save(path, &paused)?;
    }
    Ok(report)
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
        crate::hagency::local_approvals::read(&self.0).await
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
