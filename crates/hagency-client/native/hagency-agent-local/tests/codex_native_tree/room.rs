use super::*;
use hagency_agent_local::room_tools::{room_queue, RoomReply};
use hagency_runtime::room_tools::{RoomTools, RoomOperation, RoomFileOperation};

#[tokio::test]
#[ignore = "requires explicit installed Codex; localhost Responses fixture, no paid model"]
async fn matrix_tools_survive_warm_and_cold_native_resume_without_replacing_context() {
    let (tmp, profile, mut model) = native_fixture_mode("room").await;
    let scope = Scope {
        agent: "agent".into(),
        binding: "binding".into(),
        room: "!room:test".into(),
        requester: "@member:test".into(),
        thread: "root".into(),
    };
    let mut ledger = Ledger::open(tmp.path().join("ledger"), "@owner:test").unwrap();
    ledger.register_binding("@owner:test", &scope).unwrap();
    let policy = Policy {
        budget: Budget {
            limit: Limit::Unlimited,
            period: Period::Lifetime,
        },
        requests: RequestPolicy::Allow,
        high_risk: ToolPolicy::Deny,
    };
    ledger
        .set_agent_policy("@owner:test", "agent", 0, &policy)
        .unwrap();
    for layer in [Layer::Room, Layer::Requester] {
        ledger
            .set_policy("@owner:test", &scope, layer, 0, &policy)
            .unwrap();
    }
    let guardian = Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut process = runtime::spawn_for_scope_with_guardian(
        &profile,
        "offline-test-only",
        &mut ledger,
        &scope,
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
        .open_context(&mut ledger, &scope, &profile)
        .await
        .unwrap();
    let original = ledger.context_session(&scope).unwrap();
    assert!(original.is_some());
    for round in 0..4 {
        if round == 1 || round == 3 {
            assert!(process.session.can_continue());
            process.session.park();
            process
                .session
                .continue_context(&mut ledger, &scope, &profile)
                .await
                .unwrap();
        } else if round == 2 {
            process.stop().await.unwrap();
            process = runtime::spawn_for_scope_with_guardian(
                &profile,
                "offline-test-only",
                &mut ledger,
                &scope,
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
                .open_context(&mut ledger, &scope, &profile)
                .await
                .unwrap();
        }
        assert_eq!(ledger.context_session(&scope).unwrap(), original);
        let dispatch = format!("call-{round}");
        let (queue, mut approvals) = codex::approval_queue(1).unwrap();
        let (room, mut incoming) = room_queue(&scope, &dispatch).unwrap();
        let queue = queue.with_room_tools(room);
        let run = process.session.run_with_started(
            &mut ledger,
            "@owner:test",
            &scope,
            &dispatch,
            &dispatch,
            "Read the room and send the result file",
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let host = async {
            let history = incoming.recv().await.unwrap();
            assert!(matches!(
                history.operation,
                RoomOperation::History { cursor: None }
            ));
            history.reply(RoomReply::new("ROOM_HISTORY_RECEIPT".into(), false).unwrap());
            let file = incoming.recv().await.unwrap();
            assert!(
                matches!(&file.operation, RoomOperation::File(RoomFileOperation::SendFile { path, .. }) if path == "result.txt")
            );
            file.reply(RoomReply::new("ROOM_FILE_RECEIPT".into(), false).unwrap());
        };
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(25), async { tokio::join!(run, host) })
                .await
                .unwrap();
        let done = done.unwrap();
        assert_eq!(done.text, "ROOM_REPLY");
        assert!(approvals.try_recv().is_err());
        assert_eq!(ledger.context_session(&scope).unwrap(), original);
        assert!(done.usage.is_reported());
        assert_eq!((done.usage.input, done.usage.output, done.usage.cached_input), (30, 12, 6));
        assert!(process.session.can_continue());
        assert_eq!(ledger.account(&scope, Layer::Agent, Period::Lifetime,
            time::OffsetDateTime::now_utc().unix_timestamp()).unwrap(), (42 * (round + 1), 0));
    }
    process.stop().await.unwrap();
    model.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "explicit installed Codex; config only, no paid model"]
async fn matrix_tools_never_replace_an_owner_mcp_server_with_the_same_name() {
    use std::io::Write;
    let (tmp, profile, mut model) = native_fixture_mode("room").await;
    std::fs::OpenOptions::new()
        .append(true)
        .open(profile.codex_home.join("config.toml"))
        .unwrap()
        .write_all(b"\n[mcp_servers.hagency_matrix]\ncommand=\"/usr/bin/false\"\nenabled=false\n")
        .unwrap();
    let scope = Scope {
        agent: "agent".into(),
        binding: "binding".into(),
        room: "!room:test".into(),
        requester: "@member:test".into(),
        thread: "root".into(),
    };
    let mut ledger = Ledger::open(tmp.path().join("ledger"), "@owner:test").unwrap();
    ledger.register_binding("@owner:test", &scope).unwrap();
    let guardian = Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut process = runtime::spawn_for_scope_with_guardian(
        &profile,
        "offline-test-only",
        &mut ledger,
        &scope,
        Some(guardian),
    )
    .await
    .unwrap();
    process.session.initialize().await.unwrap();
    process
        .session
        .enable_room_tools(RoomTools::history())
        .await
        .unwrap();
    assert!(matches!(
        process
            .session
            .open_context(&mut ledger, &scope, &profile)
            .await,
        Err(codex::Error::Profile(_))
    ));
    assert!(ledger.context_session(&scope).unwrap().is_none());
    process.stop().await.unwrap();
    model.kill().await.unwrap();
}
