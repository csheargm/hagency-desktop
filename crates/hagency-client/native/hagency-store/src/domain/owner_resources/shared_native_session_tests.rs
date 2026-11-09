//! Two actual supervised processes share only native context, never call custody.
use super::*;

async fn advance(a: Active) -> Active {
    advance_with_restart(a, false).await
}
async fn advance_with_restart(mut a: Active, restart: bool) -> Active {
    let stopped = a
        .sink
        .record(a.process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    if restart {
        a.f.registry.shutdown().await.unwrap();
        a.f.registry = OwnerResourceRegistry::open_root(&a.f._temp.path().join("owner")).unwrap();
    }
    let command = next_attempt(&a.f, &a.command, &stopped).await;
    let SharedAttemptClaim::Issued(custody) =
        a.f.registry
            .claim_shared_successor(attempt_proof(&a.f, command.clone()))
            .await
            .unwrap()
    else {
        panic!("new custody")
    };
    let (mut process, instructions) =
        spawn_mode(&a.f, "call_successor", "text-native-session").await;
    let sink = Arc::new(
        (*custody)
            .bind_process(process.shared_text_process_source())
            .unwrap(),
    );
    sink.record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    let expires = Instant::now() + Duration::from_secs(8);
    process
        .install_shared_text_activity(1, expires, Instant::now() + Duration::from_secs(30))
        .unwrap();
    let lease = SharedActivityLease {
        authority: SharedActivityRef {
            runtime_command_id: command.command_id.clone(),
            epoch: 1,
            token: "a".repeat(64),
        },
        command_digest: digest(&command),
        process_digest: sink.process_digest().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + 9000,
    };
    Active {
        f: a.f,
        original: a.original,
        command,
        process,
        sink,
        lease,
        expires,
    }
}
fn encrypted(a: &Active, id: &str) -> SharedWorkItem {
    let mut w = a.work(id);
    w.encrypted = true;
    w.body = w.event_id.clone();
    w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
    w
}
async fn turn(
    a: &mut Active,
    id: &str,
    text: &str,
    other: bool,
    resume: bool,
) -> SharedWorkReceipt {
    turn_open(a, id, text, other, resume, resume).await
}
async fn turn_open(
    a: &mut Active,
    id: &str,
    text: &str,
    other: bool,
    expected_resume: bool,
    resumed: bool,
) -> SharedWorkReceipt {
    let mut w = encrypted(a, id);
    if other {
        w.requester_mxid = "@other:test".into();
    }
    a.f.registry
        .receive_shared_work(a.authority(), w.clone())
        .await
        .unwrap();
    let mut context =
        a.f.registry
            .prepare_shared_native_session(a.authority(), w.clone(), &mut a.process)
            .await
            .unwrap();
    assert_eq!(context.is_resuming(), expected_resume);
    // Replaying this preparation must never reopen a consumed context slot.
    assert!(
        a.f.registry
            .prepare_shared_native_session(a.authority(), w.clone(), &mut a.process)
            .await
            .is_err()
    );
    let input =
        a.f.registry
            .prepare_shared_decrypted_input(a.authority(), w.clone(), text.into())
            .await
            .unwrap();
    let h =
        a.f.registry
            .prepare_shared_input_call(a.authority(), w.clone(), input.clone())
            .await
            .unwrap();
    context.open_codex(&mut a.process).await.unwrap();
    assert_eq!(context.is_resuming(), resumed);
    let start =
        a.f.registry
            .begin_shared_input_start(a.authority(), w.clone(), h.call_id, input)
            .await
            .unwrap();
    let custody =
        a.f.registry
            .issue_shared_call(
                start,
                SharedWorkStart {
                    work: w.clone(),
                    work_digest: digest(&w),
                    activity_epoch: 1,
                    newly_started: true,
                },
            )
            .await
            .unwrap();
    custody.start_turn(&mut a.process).await.unwrap();
    let mut capture = custody.bind_started_turn(&a.process).await.unwrap();
    let receipt = loop {
        let event = a.process.next_shared_text_event().await.unwrap();
        let done = matches!(
            event.observation().kind(),
            hagency_runtime::codex::session::ObservationKind::TurnEnded(_)
        );
        let receipt = capture.observe(event).await.unwrap();
        if done {
            break receipt.unwrap();
        }
    };
    assert_eq!(receipt.state, "completed");
    assert!(receipt.usage_complete);
    assert_eq!(
        receipt.known_consumed_tokens, 11,
        "old native context totals are not new usage"
    );
    capture
        .retain_native_session(context, &a.process)
        .await
        .unwrap();
    receipt
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_native_session_restores_context_per_requester_and_bills_only_new_response() {
    let mut a = Box::pin(active_mode(false, "text-native-session")).await;
    let first = turn(&mut a, "remember", "Remember blue comet", false, false).await;
    assert_eq!(first.body.as_deref(), Some("Previous native inputs: "));
    a = advance(a).await;
    let other = turn(&mut a, "other", "Different person", true, false).await;
    assert_eq!(other.body.as_deref(), Some("Previous native inputs: "));
    a = advance(a).await;
    let follow = turn(&mut a, "recall", "What did I say?", false, true).await;
    assert_eq!(
        follow.body.as_deref(),
        Some("Previous native inputs: Remember blue comet")
    );
    assert_ne!(first.call_id, follow.call_id);
    assert_ne!(first.process_digest, follow.process_digest);
    let methods = std::fs::read_to_string(a.f._temp.path().join("call_successor/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m| *m == "thread/start").count(), 2);
    assert_eq!(methods.lines().filter(|m| *m == "thread/resume").count(), 1);
    assert_eq!(methods.lines().filter(|m| *m == "turn/start").count(), 3);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_native_session_unfinished_opening_prevents_old_checkpoint_reuse() {
    let mut a = Box::pin(active_mode(false, "text-native-session")).await;
    turn(&mut a, "first", "Keep this original", false, false).await;
    a = advance(a).await;
    let w = encrypted(&a, "unfinished");
    a.f.registry
        .receive_shared_work(a.authority(), w.clone())
        .await
        .unwrap();
    let mut context =
        a.f.registry
            .prepare_shared_native_session(a.authority(), w, &mut a.process)
            .await
            .unwrap();
    assert!(context.is_resuming());
    context.open_codex(&mut a.process).await.unwrap();
    // No complete native result or usage receipt exists for this opening.
    a = advance(a).await;
    let next = turn(&mut a, "after_interruption", "New work", false, false).await;
    assert_eq!(next.body.as_deref(), Some("Previous native inputs: "));
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_native_session_missing_rollout_rebuilds_before_input_without_replaying_work() {
    let mut a = Box::pin(active_mode(false, "text-native-session")).await;
    turn(&mut a, "remember", "Original work", false, false).await;
    a = advance(a).await;
    std::fs::remove_file(a.f._temp.path().join("call_successor/native-thread-1.json")).unwrap();
    let next = turn_open(&mut a, "fresh", "New request", false, true, false).await;
    assert_eq!(next.body.as_deref(), Some("Previous native inputs: "));
    let methods = std::fs::read_to_string(a.f._temp.path().join("call_successor/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m| *m == "thread/start").count(), 2);
    assert_eq!(methods.lines().filter(|m| *m == "thread/resume").count(), 1);
    assert_eq!(methods.lines().filter(|m| *m == "turn/start").count(), 2);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_native_session_resume_errors_do_not_replay_or_switch_context() {
    for error in [
        "authentication failed",
        "no rollout found for thread id another-thread",
    ] {
        let mut a = Box::pin(active_mode(false, "text-native-session")).await;
        turn(&mut a, "remember", "Original work", false, false).await;
        a = advance(a).await;
        std::fs::write(
            a.f._temp.path().join("call_successor/native-resume-error"),
            error,
        )
        .unwrap();
        let w = encrypted(&a, "failed-resume");
        a.f.registry
            .receive_shared_work(a.authority(), w.clone())
            .await
            .unwrap();
        let mut context =
            a.f.registry
                .prepare_shared_native_session(a.authority(), w, &mut a.process)
                .await
                .unwrap();
        assert!(context.is_resuming());
        assert!(context.open_codex(&mut a.process).await.is_err());
        let methods =
            std::fs::read_to_string(a.f._temp.path().join("call_successor/methods")).unwrap();
        assert_eq!(methods.lines().filter(|m| *m == "thread/start").count(), 1);
        assert_eq!(methods.lines().filter(|m| *m == "thread/resume").count(), 1);
        assert_eq!(methods.lines().filter(|m| *m == "turn/start").count(), 1);
        a.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_native_session_survives_registry_restart_with_fresh_call_custody() {
    let mut a = Box::pin(active_mode(false, "text-native-session")).await;
    let original = turn(
        &mut a,
        "remember",
        "Retain across host restart",
        false,
        false,
    )
    .await;
    a = advance_with_restart(a, true).await;
    let next = turn(&mut a, "reopened", "Continue", false, true).await;
    assert_eq!(
        next.body.as_deref(),
        Some("Previous native inputs: Retain across host restart")
    );
    assert_ne!(original.call_id, next.call_id);
    assert_ne!(original.process_digest, next.process_digest);
    a.stop().await;
}
