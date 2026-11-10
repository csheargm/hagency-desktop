use super::*;
use hagency_agent_local::room_tools::{room_queue, RoomReply};
use hagency_runtime::room_tools::{RoomTools, RoomOperation};

#[tokio::test]
async fn claude_slow_room_operation_returns_an_error_before_native_callback_expires() {
    let mut f = Fixture::new(Kind::Claude, "room_timeout", ToolPolicy::Deny);
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
    let (room, mut requests) = room_queue(&f.scope, "dispatch").unwrap();
    let queue = queue.with_room_tools(room);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "Read slow history",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let mut request = requests.recv().await.unwrap();
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(55)).await;
        tokio::time::resume();
        request.closed().await;
        assert!(request.is_closed());
    };
    let (done, ()) = tokio::join!(run, host);
    assert_eq!(done.unwrap().text, "offline reply");
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_room_tools_use_each_dispatch_channel_and_preserve_external_approvals() {
    let mut f = Fixture::new(Kind::Claude, "continuous:room", ToolPolicy::Deny);
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
    for round in 0..2 {
        if round != 0 {
            assert!(process.session.can_continue());
            process.session.park();
            process
                .session
                .continue_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
        }
        let dispatch = format!("call-{round}");
        let (queue, mut approvals) = codex::approval_queue(1).unwrap();
        let (room, mut requests) = room_queue(&f.scope, &dispatch).unwrap();
        let queue = queue.with_room_tools(room);
        let run = process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            &dispatch,
            &dispatch,
            "Read the uploaded file and send output",
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let host = async {
            let history = requests.recv().await.unwrap();
            assert!(matches!(history.operation, RoomOperation::History { .. }));
            history.reply(RoomReply::new("ROOM_HISTORY_RECEIPT".into(), false).unwrap());
            let file = requests.recv().await.unwrap();
            assert!(matches!(file.operation, RoomOperation::File(_)));
            file.reply(RoomReply::new("ROOM_FILE_RECEIPT".into(), false).unwrap());
        };
        let (done, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            tokio::join!(run, host)
        })
        .await
        .unwrap();
        assert_eq!(done.unwrap().text, "offline reply");
        assert!(approvals.try_recv().is_err());
    }
    assert_eq!(f.spent(), (108, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_cancelled_room_callback_withdraws_host_operation() {
    let mut f = Fixture::new(Kind::Claude, "room_cancel", ToolPolicy::Deny);
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
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let (room, mut requests) = room_queue(&f.scope, "dispatch").unwrap();
    let queue = queue.with_room_tools(room);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "Read history",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let mut request = requests.recv().await.unwrap();
        request.closed().await;
        assert!(request.is_closed());
    };
    let (done, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(run, host)
    })
    .await
    .unwrap();
    assert_eq!(done.unwrap().text, "offline reply");
    assert!(approvals.try_recv().is_err());
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
#[ignore = "explicit installed Claude; isolated localhost model, no provider account or paid request"]
async fn installed_claude_personal_room_roundtrip_uses_the_production_adapter() {
    installed_claude_personal_room_roundtrip(false).await;
}

/// An owner's Claude home usually has hooks. The native CLI reports a hook's
/// start and result as `system` frames, the first ones before it answers
/// `initialize`. They are the owner's own configuration at work and must not
/// end the session.
#[tokio::test]
#[ignore = "explicit installed Claude; isolated localhost model, no provider account or paid request"]
async fn installed_claude_personal_room_roundtrip_runs_with_the_owners_hooks() {
    installed_claude_personal_room_roundtrip(true).await;
}

async fn installed_claude_personal_room_roundtrip(hooks: bool) {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use std::process::Stdio;
    let executable = PathBuf::from(
        std::env::var_os("HAGENCY_TEST_CLAUDE_EXECUTABLE").expect("explicit Claude executable"),
    )
    .canonicalize()
    .unwrap();
    let mut f = Fixture::new(Kind::Claude, "room", ToolPolicy::Deny);
    f.profile.model = "sonnet".into();
    if hooks {
        let hook = |text: &str| json!([{"hooks":[{"type":"command","command":format!("echo {text}")}]}]);
        let tool = |text: &str| json!([{"matcher":"*","hooks":[{"type":"command","command":format!("echo {text}")}]}]);
        fs::write(
            f.profile.codex_home.join("settings.json"),
            serde_json::to_vec(&json!({"hooks":{
                "SessionStart": hook("owner-session-start"),
                "UserPromptSubmit": hook("owner-prompt"),
                "PreToolUse": tool("owner-before-tool"),
                "PostToolUse": tool("owner-after-tool"),
                "Stop": hook("owner-stop"),
            }}))
            .unwrap(),
        )
        .unwrap();
    }
    let model_source = include_str!("../../../hagency-runtime/tests/claude_question_fixture.py")
        .replace("AskUserQuestion", "mcp__hagency_matrix__read_room_history")
        .replace(
            "\"input\": {\"questions\": questions}",
            "\"input\": {\"cursor\": None}",
        );
    let model_source = model_source
        .replace(r#""cache_read_input_tokens": 0, "cache_creation_input_tokens": 0"#,
            r#""cache_read_input_tokens": 3, "cache_creation_input_tokens": 7, "cache_creation":{"ephemeral_5m_input_tokens":5,"ephemeral_1h_input_tokens":2}, "output_tokens_details":{"thinking_tokens":4}"#)
        .replace(r#""usage":{"output_tokens":9}"#, r#""usage":{"output_tokens":9,"output_tokens_details":{"thinking_tokens":4}}"#)
        .replace("        results = [block", "        messages = data.get('messages', [])\n        (root / 'native-context.json').write_text(json.dumps(messages))\n        turn = max((i for i, message in enumerate(messages) if message.get('role') == 'user' and 'Read room history round' in json.dumps(message)), default=0)\n        data['messages'] = messages[turn:]\n        results = [block")
        .replace("\"question-native\"", "f\"question-native-{turn}\"")
        .replace("\"msg_question\" if asking else \"msg_done\"", "f\"msg_{turn}_{asking}\"");
    let model_file = f.path("model.py");
    fs::write(&model_file, model_source).unwrap();
    let mut model = tokio::process::Command::new("python3")
        .arg(model_file)
        .arg(f.path(""))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap())
        .read_line(&mut port)
        .await
        .unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let binary = serde_json::to_string(&executable).unwrap();
    fs::write(&f.profile.executable, format!("#!/usr/bin/env python3\nimport os,sys\nos.environ['ANTHROPIC_API_KEY']='offline-contract'\nos.environ['ANTHROPIC_BASE_URL']='http://127.0.0.1:{port}'\nos.environ['CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC']='1'\nos.execv({binary}, [{binary}]+sys.argv[1:])\n")).unwrap();
    let guardian = std::path::Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut process = spawn_for_scope_with_guardian(
        &f.profile,
        &f.reference,
        &mut f.ledger,
        &f.scope,
        Some(guardian),
    )
    .await
    .unwrap();
    process.session.initialize().await.unwrap();
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
    for round in 0..4 {
        if round == 1 || round == 3 {
            process
                .session
                .continue_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
        } else if round == 2 {
            process.stop().await.unwrap();
            process = spawn_for_scope_with_guardian(
                &f.profile,
                &f.reference,
                &mut f.ledger,
                &f.scope,
                Some(guardian),
            )
            .await
            .unwrap();
            process.session.initialize().await.unwrap();
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
        }
        let dispatch = format!("room-round-{round}");
        let (queue, mut approvals) = codex::approval_queue(1).unwrap();
        let (room, mut requests) = room_queue(&f.scope, &dispatch).unwrap();
        let queue = queue.with_room_tools(room);
        let prompt = format!("Read room history round {round}");
        let run = process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            &dispatch,
            &dispatch,
            &prompt,
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let host = async {
            let request = requests.recv().await.unwrap();
            assert!(matches!(
                request.operation,
                RoomOperation::History { cursor: None }
            ));
            request.reply(RoomReply::new("ROOM_HISTORY_RECEIPT".into(), false).unwrap());
        };
        let (done, ()) = tokio::time::timeout(std::time::Duration::from_secs(45), async {
            tokio::join!(run, host)
        })
        .await
        .unwrap();
        let done = done.unwrap();
        assert_eq!(done.text, "Offline native question completed.");
        assert!(done.usage.is_reported());
        assert_eq!(
            (done.usage.input, done.usage.output, done.usage.cached_input),
            (54, 18, 6),
            "round {round}"
        );
        assert_eq!(done.usage.cache_write_input, Some(14));
        assert_eq!(
            done.usage.reasoning_output, 0,
            "thinking remains inside native output total"
        );
        assert_eq!(f.spent(), (72 * (round + 1), 0), "round {round}");
        let context = fs::read_to_string(f.path("native-context.json")).unwrap();
        for previous in 0..=round {
            assert!(
                context.contains(&format!("Read room history round {previous}")),
                "native context lost round {previous} after round {round}"
            );
        }
        assert!(process.session.can_continue());
        assert!(approvals.try_recv().is_err());
        assert!(
            fs::read_to_string(f.path("native-answer.json"))
                .unwrap()
                .contains("ROOM_HISTORY_RECEIPT")
        );
    }
    process.stop().await.unwrap();
    model.kill().await.unwrap();
}
