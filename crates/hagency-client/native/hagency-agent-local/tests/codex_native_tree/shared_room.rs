use super::*;
use hagency_runtime::codex::{
    native_arguments,
    session::{Outcome, ResumeThreadId, SessionDriver, Settings, Update},
    transport::Limits,
};

#[tokio::test]
#[ignore = "explicit installed Codex; isolated localhost model and host IO fixtures"]
async fn shared_native_matrix_tools_survive_cold_resume_and_charge_only_this_turn() {
    let (_tmp, profile, mut model) = native_fixture_mode("shared_room").await;
    let mut thread: Option<String> = None;
    for round in 0..2 {
        // No tool here runs an external process: the fixture calls only the
        // native dynamic history/file callbacks, answered below by this host.
        let mut child = tokio::process::Command::new(&profile.executable)
            .args(native_arguments(false, Some(false)))
            .env_clear()
            .env("HOME", &profile.home)
            .env("CODEX_HOME", &profile.codex_home)
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&profile.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let settings = Settings::new(
            profile.cwd.clone(),
            profile.model.clone(),
            profile.effort.clone(),
        )
        .unwrap();
        let mut session = SessionDriver::new(
            child.stdout.take().unwrap(),
            child.stdin.take().unwrap(),
            child.stderr.take().unwrap(),
            settings,
            Limits {
                write_timeout_ms: 5000,
                event_wait_ms: 20000,
                lifetime_ms: 60000,
            },
            5000,
        )
        .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(30), async {
            session.initialize().await.unwrap();
            session.enable_room_history().unwrap();
            session.enable_room_files().unwrap();
            thread = Some(match thread.take() {
                Some(id) => session.resume_thread(ResumeThreadId::new(id).unwrap()).await.unwrap(),
                None => session.start_thread().await.unwrap(),
            });
            session.start_turn("Read the room and send the result file".into()).await.unwrap();
            let mut calls = Vec::new();
            loop {
                let (update, observed) = session.next_observed_update().await.unwrap();
                match update {
                    Update::RoomHistory(call) => {
                        calls.push("history");
                        session.respond_room_history(&call, "ROOM_HISTORY_RECEIPT".into()).await.unwrap();
                    }
                    Update::RoomFile(call) => {
                        calls.push("file");
                        session.respond_room_file(&call, "ROOM_FILE_RECEIPT".into()).await.unwrap();
                    }
                    Update::TurnEnded => {
                        assert_eq!(calls, ["history", "file"], "round {round}");
                        assert!(matches!(session.outcome(), Some(Outcome::Completed { text }) if text == "ROOM_REPLY"));
                        assert_eq!(observed.terminal_usage().unwrap().total().total_tokens(), Some(42), "round {round}");
                        break;
                    }
                    _ => {}
                }
            }
        }).await;
        let _ = child.kill().await;
        let _ = child.wait().await;
        result.unwrap();
    }
    model.kill().await.unwrap();
}
