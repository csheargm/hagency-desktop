//! Real supervised offline children: no provider credentials or model calls.
use super::*;
#[path = "shared_successor_call_tests.rs"]
mod calls;
#[path = "shared_claude_session_tests.rs"]
mod claude_sessions;
use hagency_runtime::{
    codex::{session::Settings, transport::Limits},
    owned::{
        MANAGED_DISABLED_FEATURES, OwnedSession, SHARED_TEXT_INSTRUCTIONS,
        SHARED_TEXT_INSTRUCTIONS_FILE,
    },
};

async fn text_fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
    let provider = VerifiedProviderAccount::new(
        "codex",
        "openai.chatgpt",
        "fixture_account",
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let resource = registry
        .prepare_enrollment(
            profile.clone(),
            "text_provider".into(),
            provider,
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            1000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id;
    let grant = grant(&registry, &profile, &resource, "parent", 700)
        .await
        .unwrap();
    let project = super::super::super::command(&grant, "project", 400);
    let receipt = apply(&registry, &profile, project.clone()).await;
    Fixture {
        _temp: temp,
        registry,
        profile,
        resource,
        grant,
        project,
        receipt,
    }
}
async fn spawn(f: &Fixture, id: &str) -> (OwnedSession, PathBuf) {
    spawn_mode(f, id, "text").await
}
async fn spawn_mode(f: &Fixture, id: &str, mode: &str) -> (OwnedSession, PathBuf) {
    let binary = PathBuf::from(
        std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("supervised offline fixture"),
    );
    let root = f._temp.path().join(id);
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let instructions = root.join(SHARED_TEXT_INSTRUCTIONS_FILE);
    std::fs::write(&instructions, SHARED_TEXT_INSTRUCTIONS).unwrap();
    let features: serde_json::Map<String, serde_json::Value> = MANAGED_DISABLED_FEATURES
        .iter()
        .map(|key| ((*key).into(), false.into()))
        .collect();
    let config = json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{},"model_instructions_file":instructions,"instructions":"","notify":[],"web_search":"disabled","project_doc_max_bytes":0,"skills":{"include_instructions":false,"bundled":{"enabled":false}},"developer_instructions":"","include_apps_instructions":false,"include_environment_context":false,"mcp_servers":{},"features":features});
    std::fs::write(
        root.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let launch = hagency_platform::Launch {
        executable: binary.clone(),
        arguments: vec![mode.into(), root.as_os_str().into()],
        directory: root.clone(),
        environment: std::collections::BTreeMap::new(),
        require_crash_containment: false,
    };
    let settings = Settings::new(root, "fixture".into(), "medium".into())
        .unwrap()
        .read_only();
    let mut process = OwnedSession::spawn(
        &binary,
        &launch,
        settings,
        Limits {
            write_timeout_ms: 500,
            event_wait_ms: 1500,
            lifetime_ms: 10_000,
        },
        1500,
    )
    .unwrap();
    process.initialize().await.unwrap();
    (process, instructions)
}
fn no_model_calls(instructions: &std::path::Path) {
    let methods = std::fs::read_to_string(instructions.parent().unwrap().join("methods")).unwrap();
    assert!(!methods.contains("thread/start") && !methods.contains("turn/start"));
}

async fn original_stopped() -> (
    Fixture,
    SharedRuntimeCommand,
    SharedProcessReceipt,
    hagency_runtime::owned::SharedTextProcessSource,
) {
    let f = text_fixture().await;
    let original = command(&f).await;
    let SharedRuntimeClaim::Issued(custody) = f
        .registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap()
    else {
        panic!("original custody")
    };
    let (mut process, instructions) = spawn(&f, "original").await;
    let source = process.shared_text_process_source();
    let sink = (*custody).bind_process(source.clone()).unwrap();
    sink.record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    let stopped = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    no_model_calls(&instructions);
    (f, original, stopped, source)
}
async fn next_attempt(
    f: &Fixture,
    previous: &SharedAttemptCommand,
    stopped: &SharedAttemptProcessReceipt,
) -> SharedAttemptCommand {
    let budget = f
        .registry
        .shared_attempt_successor_budget(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            previous.clone(),
        )
        .await
        .unwrap();
    let now = graphs::now_ms().unwrap();
    let mut next = previous.clone();
    next.command_id = format!("successor_{}", previous.process_attempt + 1);
    next.process_attempt += 1;
    next.start_intent_id = format!("intent_{}", next.process_attempt);
    next.predecessor = SharedAttemptPredecessor {
        command_id: previous.command_id.clone(),
        command_digest: digest(previous),
        process_attempt: previous.process_attempt,
        stopped_receipt_digest: digest(stopped),
        process_digest: stopped.process_digest.clone().unwrap(),
    };
    next.budget_observation = SharedBudgetObservation {
        observed_ledger_revision: budget.observed_ledger_revision,
        allocated_tokens: budget.allocated_tokens,
        known_consumed_tokens: budget.known_consumed_tokens,
        retained_tokens: budget.retained_tokens,
        remaining_tokens: budget.remaining_tokens,
        usage_complete: budget.usage_complete,
        liability_digest: budget.liability_digest,
    };
    next.issued_at_ms = now;
    next.expires_at_ms = now + 20_000;
    next
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn shared_successor_actual_process_stop_allows_attempt_three_without_rebinding_old_work() {
    let (mut f, original, original_stop, _) = original_stopped().await;
    call(&f, &original, "old_unknown", "unknown", 40, 7, false, false).await;
    let second = attempt(&f, &original).await;
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, second.clone()))
        .await
        .unwrap()
    else {
        panic!("successor custody")
    };
    let (mut process, instructions) = spawn(&f, "second").await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    let (mut foreign, foreign_instructions) = spawn(&f, "foreign_successor_source").await;
    assert!(
        sink.record(foreign.stop_shared_text_observation())
            .await
            .is_err()
    );
    no_model_calls(&foreign_instructions);
    let qualified = sink
        .record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    assert_eq!(qualified.state, "qualified");
    assert_eq!(qualified.process_attempt, 2);
    assert!(qualified.ledger_revision > second.budget_observation.observed_ledger_revision);
    assert!(
        f.registry
            .shared_attempt_successor_budget(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                second.clone()
            )
            .await
            .is_err(),
        "a running predecessor is never a successor authority"
    );
    let stopped = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    assert!(stopped.ledger_revision > qualified.ledger_revision);
    let saved_qualified = f
        .registry
        .recover_shared_attempt_process(
            cleanup(&f, &original, 1).await,
            second.clone(),
            Some(qualified.observation_id.clone()),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&saved_qualified), digest(&qualified));
    let third = next_attempt(&f, &second, &stopped).await;
    assert_eq!(
        (
            third.budget_observation.known_consumed_tokens,
            third.budget_observation.retained_tokens,
            third.budget_observation.remaining_tokens
        ),
        (7, 7, 143)
    );
    // Stopping proves there is no live spender. It does not turn partial
    // usage into a final bill; postpaid admission still permits the next call.
    assert!(!third.budget_observation.usage_complete);
    assert_eq!(third.original_runtime_digest, digest(&original));
    let SharedAttemptClaim::Issued(third_custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, third.clone()))
        .await
        .unwrap()
    else {
        panic!("third custody")
    };
    third_custody.check().unwrap();
    assert!(
        f.registry
            .shared_attempt_successor_budget(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                second.clone()
            )
            .await
            .is_err(),
        "an old stopped head cannot bypass the issued newer process"
    );
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, third.clone()))
            .await
            .unwrap(),
        SharedAttemptClaim::History(_)
    ));
    let original_saved = f
        .registry
        .recover_shared_process(
            cleanup(&f, &original, 1).await,
            original.clone(),
            Some(original_stop.observation_id.clone()),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&original_saved), digest(&original_stop));
    no_model_calls(&instructions);
    drop(third_custody);
    drop(sink);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let late = f
        .registry
        .recover_shared_attempt_process(cleanup(&f, &original, 2).await, second, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&late), digest(&stopped));
    assert!(
        f.registry
            .recover_shared_attempt_process(cleanup(&f, &original, 2).await, third.clone(), None)
            .await
            .unwrap()
            .is_none(),
        "issued third attempt is not assumed stopped after restart"
    );
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, third))
            .await
            .unwrap(),
        SharedAttemptClaim::History(_)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn shared_successor_cannot_reuse_the_original_actual_process_source() {
    let (f, original, original_stop, source) = original_stopped().await;
    let second = attempt(&f, &original).await;
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, second.clone()))
        .await
        .unwrap()
    else {
        panic!("successor custody")
    };
    assert!((*custody).bind_process(source).is_err());
    assert!(
        f.registry
            .recover_shared_attempt_process(cleanup(&f, &original, 1).await, second.clone(), None)
            .await
            .unwrap()
            .is_none(),
        "original stop cannot be copied to the successor"
    );
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, second))
            .await
            .unwrap(),
        SharedAttemptClaim::History(_)
    ));
    let saved = f
        .registry
        .recover_shared_process(cleanup(&f, &original, 1).await, original, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&saved), digest(&original_stop));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn shared_successor_fence_and_retirement_deny_qualification_but_keep_exact_late_stop() {
    let (mut f, original, _, _) = original_stopped().await;
    let second = attempt(&f, &original).await;
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, second.clone()))
        .await
        .unwrap()
    else {
        panic!("successor custody")
    };
    let (mut process, instructions) = spawn(&f, "fenced_second").await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    let observed = process.qualify_shared_text(&instructions).await.unwrap();
    let fence = f
        .registry
        .fence_shared_successor(cleanup(&f, &original, 1).await, second.clone())
        .await
        .unwrap();
    assert!(sink.record(observed).await.is_err());
    f.profile.retire();
    let stopped = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    assert!(stopped.ledger_revision > fence.fence_revision);
    f.profile = authority('a');
    let saved = f
        .registry
        .recover_shared_attempt_process(
            cleanup(&f, &original, 2).await,
            second.clone(),
            Some(stopped.observation_id.clone()),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&saved), digest(&stopped));
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, second))
            .await
            .unwrap(),
        SharedAttemptClaim::History(_)
    ));
    let rows = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT count(*) FROM owner_shared_successor_observations",
                [],
                |r| r.get::<_, u64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(
        rows, 1,
        "fenced qualification never became a durable receipt"
    );
    no_model_calls(&instructions);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn shared_successor_queued_observer_retirement_rolls_back_qualification_and_keeps_stop() {
    let (mut f, original, _, _) = original_stopped().await;
    let second = attempt(&f, &original).await;
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, second.clone()))
        .await
        .unwrap()
    else {
        panic!("successor custody")
    };
    let (mut process, instructions) = spawn(&f, "queued_second").await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    let observed = process.qualify_shared_text(&instructions).await.unwrap();
    let store = f.registry.0.store.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (unblock, blocked) = std::sync::mpsc::channel();
    let blocker = tokio::spawn(async move {
        store
            .call(1024, move |_| {
                started.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let mut pending = Box::pin(sink.record(observed));
    tokio::select! { biased; _=&mut pending=>panic!("qualification bypassed writer"), _=tokio::task::yield_now()=>{} }
    f.profile.retire();
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(pending.await.is_err());
    let stopped = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    f.profile = authority('a');
    let saved = f
        .registry
        .recover_shared_attempt_process(cleanup(&f, &original, 2).await, second, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&saved), digest(&stopped));
    let rows = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT count(*) FROM owner_shared_successor_observations",
                [],
                |r| r.get::<_, u64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(rows, 1);
    no_model_calls(&instructions);
    f.registry.shutdown().await.unwrap();
}

#[cfg(feature = "shared-budget-execution")]
#[path = "shared_budget_process_tests.rs"]
mod budget_execution;

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE; real supervised offline successor, no provider"]
async fn managed_settlement_old_stop_cannot_release_a_live_successor() {
    use super::super::super::{custody::settlement_tests, fences};
    let (f, original, _, _) = original_stopped().await;
    let second = attempt(&f, &original).await;
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, second.clone()))
        .await
        .unwrap()
    else {
        panic!("successor custody")
    };
    let (mut process, instructions) = spawn(&f, "settlement_successor").await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    sink.record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    let fc = fences::command(
        &f,
        original.reserve_command.clone(),
        "settlement_successor_fence",
    );
    let fr = fences::apply_fence(&f, fc.clone()).await;
    let pending = settlement_tests::settle(
        &f,
        settlement_tests::command(&fc, &fr, "settlement_successor_pending"),
    )
    .await;
    assert!(!pending.usage_complete);
    assert_eq!(pending.retained_tokens, 150);
    sink.record(process.stop_shared_text_observation())
        .await
        .unwrap();
    let done = settlement_tests::settle(
        &f,
        settlement_tests::command(&fc, &fr, "settlement_successor_done"),
    )
    .await;
    assert!(done.usage_complete);
    assert_eq!((done.retained_tokens, done.known_consumed_tokens), (0, 0));
    assert_eq!(done.process_state, "stopped");
    no_model_calls(&instructions);
}
