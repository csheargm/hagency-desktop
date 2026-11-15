//! The owner's flow: Stop all, restart the app, Resume all.
use super::*;
use std::sync::Mutex;

#[derive(Default)]
struct Recorder {
    agents: Vec<String>,
    approvals: Vec<Value>,
    fail_start: Vec<String>,
    unreadable: bool,
    calls: Mutex<Vec<String>>,
}
impl Recorder {
    fn log(&self, entry: String) {
        self.calls.lock().unwrap().push(entry);
    }
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}
impl Agents for Recorder {
    async fn local_agents(&self) -> Result<Vec<String>, String> {
        Ok(self.agents.clone())
    }
    async fn pending_approvals(&self) -> Result<Vec<Value>, String> {
        if self.unreadable { Err("down".into()) } else { Ok(self.approvals.clone()) }
    }
    async fn deny(&self, row: &Value) -> Result<(), String> {
        self.log(format!("deny {}", row["id"].as_str().unwrap()));
        Ok(())
    }
    async fn set_running(&self, agent: &str, running: bool) -> Result<(), String> {
        self.log(format!("{} {agent}", if running { "start" } else { "stop" }));
        if running && self.fail_start.iter().any(|a| a == agent) { Err("busy".into()) } else { Ok(()) }
    }
    fn stop_runs(&self) -> usize {
        self.log("stop runs".into());
        2
    }
}
/// The switch is process-wide: its tests run one at a time.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}
fn block<T>(f: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
}
fn recorder() -> Recorder {
    Recorder {
        agents: vec!["codex".into(), "claude".into()],
        approvals: vec![json!({"id": "ap1"})],
        ..Recorder::default()
    }
}

#[test]
fn stop_all_stops_runs_denies_approvals_then_stops_every_agent_and_persists() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let agents = recorder();
    let report = block(stop_all(&agents, &path, "@me:x")).unwrap();
    assert_eq!(report, Report { agents: 2, runs: 2, denied: 2, failed: vec![] });
    // Runs and approvals are answered while the runtimes still exist.
    assert_eq!(agents.calls(), ["stop runs", "deny ap1", "stop codex", "stop claude", "deny ap1"]);
    // A restart reads the same state: both stay paused, for this owner only.
    assert!(is_paused(&path, "@me:x", "codex") && is_paused(&path, "@me:x", "claude"));
    assert!(!is_paused(&path, "@other:x", "codex"));
}

#[test]
fn resume_all_starts_only_the_agents_the_switch_stopped_and_clears_the_state() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    block(stop_all(&recorder(), &path, "@me:x")).unwrap();
    let later = Recorder { agents: vec!["unrelated".into()], ..Recorder::default() };
    let report = block(resume_all(&later, &path, "@me:x")).unwrap();
    assert_eq!(report.agents, 2);
    assert_eq!(later.calls(), ["start codex", "start claude"]);
    assert!(load(&path, "@me:x").is_none(), "resumed: nothing stays paused");
    // Resuming again is a no-op.
    assert_eq!(block(resume_all(&later, &path, "@me:x")).unwrap(), Report::default());
}

#[test]
fn an_agent_that_fails_to_start_stays_paused_for_a_retry() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    block(stop_all(&recorder(), &path, "@me:x")).unwrap();
    let flaky = Recorder { fail_start: vec!["claude".into()], ..Recorder::default() };
    let report = block(resume_all(&flaky, &path, "@me:x")).unwrap();
    assert_eq!(report.failed, ["claude"]);
    assert_eq!(load(&path, "@me:x").unwrap().agents, ["claude"]);
}

#[test]
fn the_owner_stop_wins_even_when_the_pause_cannot_be_saved() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing-folder").join(STATE_FILE);
    let agents = recorder();
    let report = block(stop_all(&agents, &path, "@me:x")).unwrap();
    assert_eq!(report.failed, [PAUSE_UNSAVED]);
    assert_eq!(&agents.calls()[..4], ["stop runs", "deny ap1", "stop codex", "stop claude"]);
}

#[test]
fn here_lists_only_active_agents_on_this_unrevoked_device() {
    let devices = json!({"currentDeviceId": "d1", "devices": [{"id": "d1", "revoked": false}, {"id": "d2", "revoked": false}]});
    let agents = json!({"agents": [
        {"id": "a", "state": "active", "executionDeviceId": "d1"},
        {"id": "b", "state": "active", "executionDeviceId": "d2"},
        {"id": "c", "state": "retired", "executionDeviceId": "d1"},
    ]});
    assert_eq!(here(&agents, &devices), ["a"]);
    let revoked = json!({"currentDeviceId": "d1", "devices": [{"id": "d1", "revoked": true}]});
    assert!(here(&agents, &revoked).is_empty());
}

#[test]
fn unreadable_approvals_are_reported_and_the_agents_still_stop() {
    let _serial = serial();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    let agents = Recorder { unreadable: true, ..recorder() };
    let report = block(stop_all(&agents, &path, "@me:x")).unwrap();
    assert_eq!(report.failed, [APPROVALS_UNANSWERED]);
    assert_eq!(agents.calls(), ["stop runs", "stop codex", "stop claude"]);
}

#[test]
fn a_second_switch_while_one_runs_is_refused() {
    let _serial = serial();
    let held = Busy::take().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(STATE_FILE);
    assert_eq!(block(resume_all(&recorder(), &path, "@me:x")), Err(BUSY_CODE.to_owned()));
    drop(held);
    assert!(block(resume_all(&recorder(), &path, "@me:x")).is_ok());
}
