use super::*;
use hagency_agent_local::{
    questions::question_queue,
    room_tools::{room_queue, RoomReply},
    run_controls::{self, Action, Outcome},
};
use hagency_runtime::{
    questions::Answer,
    room_tools::{RoomOperation, RoomTools},
};
use std::time::Duration;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct FileGate {
    active: AtomicBool,
    calls: AtomicUsize,
}
impl codex::HostToolGate for FileGate {
    fn authorize<'a>(
        &'a self,
        dispatch: &'a str,
        execution: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            dispatch == "dispatch" && execution == "call" && self.active.load(Ordering::SeqCst)
        })
    }
}

fn shared_workspace(f: &Fixture) -> hagency_agent_local::room_files::Workspace {
    let owner_dir = f
        .root
        .path()
        .canonicalize()
        .unwrap()
        .join(format!("owner_{}", "a".repeat(64)));
    fs::create_dir(&owner_dir).unwrap();
    fs::set_permissions(&owner_dir, fs::Permissions::from_mode(0o700)).unwrap();
    hagency_agent_local::room_files::Workspace::open_shared(
        &owner_dir,
        &f.profile.cwd,
        "@owner:test",
        &f.scope,
    )
    .unwrap()
}

fn file_gate() -> Arc<FileGate> {
    Arc::new(FileGate {
        active: AtomicBool::new(true),
        calls: AtomicUsize::new(0),
    })
}

#[tokio::test]
async fn octos_refuses_a_kernel_that_cannot_confirm_scoped_startup() {
    let mut f = Fixture::new(Kind::Octos, "octos_scope_legacy", ToolPolicy::Deny);
    let guardian = std::env::var_os("HAGENCY_TEST_GUARDIAN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hagency-local-probe")));
    let mut process = spawn_for_scope_with_guardian(
        &f.profile, &f.reference, &mut f.ledger, &f.scope, Some(&guardian),
    ).await.unwrap();
    assert!(matches!(process.session.initialize().await, Err(Error::Protocol("Octos scoped profile unsupported"))));
    let argv: Vec<String> = serde_json::from_str(fs::read_to_string(f.path("argv")).unwrap().lines().next().unwrap()).unwrap();
    assert!(argv.windows(2).any(|pair| pair == ["--host-scoped-profile", "coding"]));
    process.stop().await.unwrap();
}

/// Opt-in process test: the actual Hagency host and an actual Octos binary
/// talk to a local synthetic model. No Matrix account or provider is used.
#[cfg(target_os = "macos")]
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE pointing to a scoped Octos build"]
async fn octos_real_process_routes_room_tools_and_questions_offline() {
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    let binary = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap())
        .canonicalize().unwrap();
    let mut f = Fixture::new(Kind::Octos, "normal", ToolPolicy::Deny);
    let local_binary = f.path("octos-real");
    fs::copy(binary, &local_binary).unwrap();
    let model_file = f.path("model.py");
    fs::write(&model_file, include_str!("../octos_offline_model.py")).unwrap();
    let mut model = tokio::process::Command::new("python3")
        .arg(&model_file).arg(f.path(""))
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let policy = format!("(version 1)\n(allow default)\n(deny file-read* file-write* (subpath \"/Users\"))\n(deny network-outbound)\n(allow network-outbound (remote tcp \"localhost:{port}\"))\n(deny mach-lookup (global-name \"com.apple.securityd\"))");
    let encoded_policy = serde_json::to_string(&policy).unwrap();
    let encoded_binary = serde_json::to_string(&local_binary).unwrap();
    fs::write(&f.profile.executable, format!(
        "#!/usr/bin/env python3\nimport os,sys\nos.execv('/usr/bin/sandbox-exec', ['sandbox-exec','-p',{encoded_policy},{encoded_binary}]+sys.argv[1:])\n"
    )).unwrap();
    let primary = octos::ProfileModel { family: "local".into(), model: "test-model".into() };
    fs::write(f.profile.codex_home.join("profiles/coding.json"), json!({
        "id":"coding", "name":"Fixture", "enabled":true,
        "created_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}}}
    }).to_string()).unwrap();
    f.reference = local_reference(Kind::Octos, &f.profile.codex_home, Some(("coding", &primary)));
    let mut process = f.process().await;
    process.session.enable_room_tools(RoomTools::history_and_files()).await.unwrap();
    process.session.open_context(&mut f.ledger, &f.scope, &f.profile).await.unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let (room, mut incoming_room) = room_queue(&f.scope, "dispatch").unwrap();
    let (questions, mut incoming_questions) = question_queue();
    let (controls, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
    let queue = queue.with_room_tools(room).with_questions(questions).with_run_controls(inbox);
    let run = process.session.run_with_started(
        &mut f.ledger, "@owner:test", &f.scope, "call", "dispatch", "Read the room and ask.",
        BudgetMode::Postpaid, &queue, None,
    );
    let host = async {
        let request = incoming_room.recv().await.unwrap();
        assert!(matches!(request.operation, RoomOperation::History { .. }));
        request.reply(RoomReply::new("OFFLINE_ROOM_RECEIPT".into(), false).unwrap());
        let question = incoming_questions.recv().await.unwrap();
        assert_eq!(question.questions[0].question, "Which option?");
        let steer = controls.submit(Action::Steer { text: "Also check the offline follow-up.".into() }).unwrap();
        let receipt = question.reply([("q0".into(), Answer::Option("One".into()))].into());
        assert!(receipt.accepted().await);
        steer.receipt.await.unwrap()
    };
    let (result, steer) = tokio::time::timeout(Duration::from_secs(75), async { tokio::join!(run, host) }).await.unwrap();
    assert_eq!(result.unwrap().text, "Offline integration complete.");
    let requests = fs::read_to_string(f.path("offline-model-requests.json")).unwrap();
    assert!(requests.contains("OFFLINE_ROOM_RECEIPT"));
    assert!(matches!(steer, Outcome::Accepted | Outcome::Unavailable));
    println!("offline steer outcome: {steer:?}");
    if steer == Outcome::Accepted { assert!(requests.contains("Also check the offline follow-up.")); }
    process.stop().await.unwrap();
    model.kill().await.unwrap();
}

#[tokio::test]
async fn octos_shared_workspace_requires_owner_and_rechecks_the_dispatch() {
    for (mode, allow, revoke) in [
        ("octos_files", true, false),
        ("octos_files", false, false),
        ("octos_files_revoke", true, true),
    ] {
        let mut f = Fixture::new(Kind::Octos, mode, ToolPolicy::AskOwner);
        let workspace = shared_workspace(&f);
        let gate = file_gate();
        let mut process = f.process().await;
        process
            .session
            .enable_host_files(workspace, gate.clone())
            .unwrap();
        process
            .session
            .enable_room_tools(RoomTools::history_and_files())
            .await
            .unwrap();
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, mut approvals) = codex::approval_queue(1).unwrap();
        let (room, _incoming) = room_queue(&f.scope, "dispatch").unwrap();
        let queue = queue.with_room_tools(room);
        let path = f.profile.cwd.join("plan.txt");
        let run = process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "write code",
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let host = async {
            let pending = approvals.recv().await.unwrap();
            assert_eq!(pending.proposal.tool, "room.create");
            assert_eq!(pending.proposal.arguments["file"]["path"], "plan.txt");
            assert!(!path.exists());
            if revoke {
                gate.active.store(false, Ordering::SeqCst);
            }
            pending.decide(allow);
        };
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
                .await
                .unwrap();
        assert_eq!(done.unwrap().text, "offline reply");
        let response: serde_json::Value =
            serde_json::from_slice(&fs::read(f.path("file-result")).unwrap()).unwrap();
        assert_eq!(response["ok"], allow && !revoke);
        if allow && !revoke {
            assert_eq!(fs::read_to_string(&path).unwrap(), "owner-approved code");
            assert!(gate.calls.load(Ordering::SeqCst) >= 2);
        } else {
            assert!(!path.exists());
        }
        let tools: serde_json::Value =
            serde_json::from_slice(&fs::read(f.path("room-tools")).unwrap()).unwrap();
        assert!(
            tools
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "hagency.file_update")
        );
        assert!(
            tools
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "hagency.read_room_history")
        );
        let prompt = fs::read_to_string(f.path("turn-input")).unwrap();
        assert!(
            prompt.contains("hagency_file_update") && prompt.contains("hagency_read_room_history")
        );
        assert!(prompt.ends_with("write code"));
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn octos_shared_workspace_withdraws_cancelled_approval() {
    let mut f = Fixture::new(Kind::Octos, "octos_files_cancel", ToolPolicy::AskOwner);
    let workspace = shared_workspace(&f);
    let mut process = f.process().await;
    process
        .session
        .enable_host_files(workspace, file_gate())
        .unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let marker = f.path("file-ready");
    let path = f.profile.cwd.join("plan.txt");
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "write code",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let pending = approvals.recv().await.unwrap();
        fs::write(marker, "ready").unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while !pending.is_closed() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        pending.decide(true);
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
            .await
            .unwrap();
    assert_eq!(done.unwrap().text, "offline reply");
    assert!(!path.exists());
    assert!(!f.path("file-result").exists());
    process.stop().await.unwrap();
}

#[tokio::test]
async fn octos_shared_workspace_rejects_traversal_before_approval() {
    let mut f = Fixture::new(Kind::Octos, "octos_files_traversal", ToolPolicy::AskOwner);
    let workspace = shared_workspace(&f);
    let mut process = f.process().await;
    process
        .session
        .enable_host_files(workspace, file_gate())
        .unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let done = tokio::time::timeout(
        Duration::from_secs(10),
        process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "write code",
            BudgetMode::Postpaid,
            &queue,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(done.text, "offline reply");
    assert!(approvals.try_recv().is_err());
    assert!(!f.path("outside/escape.txt").exists());
    let response: serde_json::Value =
        serde_json::from_slice(&fs::read(f.path("file-result")).unwrap()).unwrap();
    assert_eq!(response["ok"], false);
    process.stop().await.unwrap();
}

#[tokio::test]
async fn octos_question_and_steer_share_the_live_turn() {
    for (mode, outcome) in [
        ("octos_question_steer", Outcome::Accepted),
        ("octos_question_steer_drop", Outcome::Unavailable),
        ("octos_steer_legacy", Outcome::Unsupported),
    ] {
        let mut f = Fixture::new(Kind::Octos, mode, ToolPolicy::Deny);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
        let (questions, mut incoming) = question_queue();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let live = hagency_runtime::live_text::LiveText::default();
        let queue = queue
            .with_run_controls(inbox)
            .with_questions(questions)
            .with_live_text(live.clone());
        let run = process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "ask",
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let host = async {
            let pending = incoming.recv().await.unwrap();
            assert_eq!(pending.questions.len(), 2);
            let steer = sender
                .submit(Action::Steer {
                    text: "also test".into(),
                })
                .unwrap();
            let receipt = pending.reply(
                [
                    (
                        "q0".into(),
                        Answer::Options(vec!["One".into(), "Two".into()]),
                    ),
                    ("q1".into(), Answer::Text("custom".into())),
                ]
                .into(),
            );
            assert!(receipt.accepted().await);
            assert_eq!(steer.receipt.await.unwrap(), outcome);
        };
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
                .await
                .unwrap();
        assert_eq!(done.unwrap().text, "offline reply");
        assert_eq!(live.since(0).unwrap().1, "offline reply");
        let answers: serde_json::Value =
            serde_json::from_slice(&fs::read(f.path("question-answer")).unwrap()).unwrap();
        assert_eq!(answers[0]["selected_labels"], json!(["One", "Two"]));
        assert_eq!(answers[1]["free_text"], "custom");
        assert_eq!(f.path("steer").exists(), mode != "octos_steer_legacy");
        assert_eq!(f.spent(), (24, 0));
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn octos_duplicate_steers_restore_the_unconsumed_last_submission() {
    let mut f = Fixture::new(Kind::Octos, "octos_question_two_steers", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
    let (questions, mut incoming) = question_queue();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox).with_questions(questions);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "ask",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let pending = incoming.recv().await.unwrap();
        let first = sender
            .submit(Action::Steer {
                text: "also test".into(),
            })
            .unwrap();
        let second = sender
            .submit(Action::Steer {
                text: "also test".into(),
            })
            .unwrap();
        let receipt = pending.reply(
            [
                (
                    "q0".into(),
                    Answer::Options(vec!["One".into(), "Two".into()]),
                ),
                ("q1".into(), Answer::Text("custom".into())),
            ]
            .into(),
        );
        assert!(receipt.accepted().await);
        assert_eq!(first.receipt.await.unwrap(), Outcome::Accepted);
        assert_eq!(second.receipt.await.unwrap(), Outcome::Unavailable);
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
            .await
            .unwrap();
    assert_eq!(done.unwrap().text, "offline reply");
    process.stop().await.unwrap();
}

#[tokio::test]
async fn octos_interrupt_withdraws_the_pending_question_and_settles_usage() {
    let mut f = Fixture::new(Kind::Octos, "octos_question_stop", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
    let (questions, mut incoming) = question_queue();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox).with_questions(questions);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "ask",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let pending = incoming.recv().await.unwrap();
        let stop = sender.submit(Action::Interrupt).unwrap();
        assert_eq!(stop.receipt.await.unwrap(), Outcome::Accepted);
        tokio::time::timeout(Duration::from_secs(2), async {
            while !pending.is_closed() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .unwrap();
        assert!(!pending.reply(Default::default()).accepted().await);
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
            .await
            .unwrap();
    assert_eq!(done.unwrap().status, codex::TurnStatus::Interrupted);
    assert!(!f.path("question-answer").exists());
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
async fn octos_room_tools_use_the_original_scoped_host_channel() {
    let mut f = Fixture::new(Kind::Octos, "octos_room", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .enable_room_tools(RoomTools::history_and_files())
        .await
        .unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let (room, mut incoming) = room_queue(&f.scope, "dispatch").unwrap();
    let queue = queue.with_room_tools(room);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "read the room",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let history = incoming.recv().await.unwrap();
        assert!(matches!(history.operation, RoomOperation::History { .. }));
        history.reply(RoomReply::new("ROOM_HISTORY".into(), false).unwrap());
        let files = incoming.recv().await.unwrap();
        assert!(matches!(files.operation, RoomOperation::File(_)));
        files.reply(RoomReply::new("ROOM_FILES".into(), false).unwrap());
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
            .await
            .unwrap();
    assert_eq!(done.unwrap().text, "offline reply");
    let results = fs::read_to_string(f.path("room-results")).unwrap();
    assert!(results.contains("ROOM_HISTORY") && results.contains("ROOM_FILES"));
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
async fn octos_cancelled_room_tool_withdraws_the_host_operation() {
    let mut f = Fixture::new(Kind::Octos, "octos_room_cancel", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .enable_room_tools(RoomTools::history_and_files())
        .await
        .unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let (room, mut incoming) = room_queue(&f.scope, "dispatch").unwrap();
    let queue = queue.with_room_tools(room);
    let marker = f.path("room-ready");
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "read the room",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let mut pending = incoming.recv().await.unwrap();
        fs::write(marker, "ready").unwrap();
        pending.closed().await;
        assert!(pending.is_closed());
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
            .await
            .unwrap();
    assert_eq!(done.unwrap().text, "offline reply");
    assert!(!f.path("room-results").exists());
    process.stop().await.unwrap();
}

#[tokio::test]
async fn octos_long_stream_keeps_preview_bounded_without_ending_the_turn() {
    let mut f = Fixture::new(Kind::Octos, "octos_long_stream", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let live = hagency_runtime::live_text::LiveText::default();
    let queue = queue.with_live_text(live.clone());
    let done = tokio::time::timeout(
        Duration::from_secs(30),
        process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "stream",
            BudgetMode::Postpaid,
            &queue,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(done.text, "offline reply");
    assert_eq!(live.since(0).unwrap().1, "offline reply");
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}
