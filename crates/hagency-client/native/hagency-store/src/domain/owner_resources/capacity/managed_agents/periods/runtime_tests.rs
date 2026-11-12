//! Real offline supervised processes across a natural-period boundary. The
//! only clock seam is test-only and weak/thread-local; no receipt is rewritten.
use super::*;
use hagency_runtime::{
    codex::{session::Settings, transport::Limits},
    owned::{
        MANAGED_DISABLED_FEATURES, OwnedSession, SHARED_TEXT_INSTRUCTIONS,
        SHARED_TEXT_INSTRUCTIONS_FILE,
    },
};
use shared_execution::*;

async fn spawn(f: &Fixture, id: &str, tools: bool) -> (OwnedSession, std::path::PathBuf) {
    let binary = std::path::PathBuf::from(
        std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("offline supervised fixture"),
    );
    let root = f._temp.path().join(id);
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let instructions = root.join(if tools {hagency_runtime::owned::SHARED_TOOLS_INSTRUCTIONS_FILE} else {SHARED_TEXT_INSTRUCTIONS_FILE});
    std::fs::write(&instructions, if tools {hagency_runtime::owned::SHARED_TOOLS_INSTRUCTIONS} else {SHARED_TEXT_INSTRUCTIONS}).unwrap();
    let features: serde_json::Map<String, serde_json::Value> = MANAGED_DISABLED_FEATURES
        .iter()
        .map(|key| ((*key).into(), (tools && matches!(*key, "shell_tool" | "unified_exec" | "code_mode_host")).into()))
        .collect();
    let config = json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{},"model_instructions_file":instructions,"instructions":"","notify":[],"web_search":"disabled","project_doc_max_bytes":0,"skills":{"include_instructions":false,"bundled":{"enabled":false}},"developer_instructions":"","include_apps_instructions":false,"include_environment_context":false,"mcp_servers":{},"features":features});
    std::fs::write(
        root.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let launch = hagency_platform::Launch {
        executable: binary.clone(),
        arguments: vec!["text".into(), root.as_os_str().into()],
        directory: root.clone(),
        environment: std::collections::BTreeMap::new(),
        require_crash_containment: false,
    };
    let mut process = OwnedSession::spawn(
        &binary,
        &launch,
        {
            let settings = Settings::new(root, "fixture".into(), "medium".into()).unwrap();
            if tools {settings.with_execution_policy(false)} else {settings.read_only()}
        },
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
pub(super) fn observed(id: &str, total: u64) -> ManagedUsageObservation {
    observed_capture(id, total, true)
}
fn observed_capture(id: &str, total: u64, complete_capture: bool) -> ManagedUsageObservation {
    use hagency_metering::{
        observation::UsageObservation,
        runtime_usage::{CodexUsage, CounterBreakdown, ProjectionDiagnostics},
    };
    let counters = CounterBreakdown {
        total_tokens: Some(total),
        input_tokens: Some(total),
        cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0),
        output_tokens: Some(0),
        reasoning_output_tokens: Some(0),
    };
    let usage = UsageObservation::codex_runtime(CodexUsage {
        total: counters,
        last: counters,
        context_window: Some(100_000),
        diagnostics: ProjectionDiagnostics::default(),
    })
    .unwrap();
    ManagedUsageObservation::fixture(id, Some(usage), true, complete_capture, false)
}
fn offer(f: &Fixture, runtime: &SharedRuntimeCommand, id: &str) -> VerifiedSharedDemand {
    let now = graphs::now_ms().unwrap();
    let value = SharedDemandOffer {
        version: 1,
        demand_id: id.into(),
        assignment_id: runtime.assignment_id.clone(),
        event_id: format!("${id}"),
        event_digest: "b".repeat(64),
        control_revision: 1,
        received_at_ms: now,
        predecessor_command_id: runtime.command_id.clone(),
        predecessor_command_digest: digest(runtime),
    };
    VerifiedSharedDemand::new(
        enrollment(f),
        value.clone(),
        digest(&value),
        now,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
fn attempt(
    runtime: &SharedRuntimeCommand,
    input: &SharedAttemptStartInput,
    id: &str,
    issued: u64,
) -> SharedAttemptCommand {
    SharedAttemptCommand {
        version: 2,
        capability: SHARED_ATTEMPT_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: id.into(),
        assignment_id: runtime.assignment_id.clone(),
        assignment_generation: 1,
        agent_reservation_id: runtime.agent_reservation_id.clone(),
        process_attempt: 2,
        original_runtime_command: runtime.clone(),
        original_runtime_digest: digest(runtime),
        predecessor: input.predecessor.clone(),
        budget_observation: input.budget_observation.clone(),
        start_intent_id: input.start_intent_id.clone(),
        web_search: input.web_search,
        tool_policy: input.tool_policy.clone(),
        issued_at_ms: issued,
        expires_at_ms: issued + 20_000,
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised fixture"]
async fn recurring_native_actual_stop_renew_new_demand_and_call() {
    let calendar = Calendar {
        cadence: Cadence::Daily,
        time_zone: "America/Los_Angeles".into(),
    };
    let original_period = calendar.period_at(graphs::now_ms().unwrap()).unwrap();
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(
        original_period.end_ms - 60_000,
    ));
    graphs::install_test_clock(&clock);
    let f = fixture_kind(RenewalMode::Automatic, false, Some(clock.clone())).await;
    let saved = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            f.command.command_id.clone(),
            digest(&f.command),
        )
        .await
        .unwrap()
        .unwrap();
    let now = graphs::now_ms().unwrap();
    let runtime = SharedRuntimeCommand {
        version: 1,
        capability: SHARED_EXECUTION_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "period_runtime_original".into(),
        assignment_id: "period_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: f.child.clone(),
        reserve_command: f.command.clone(),
        reserve_command_digest: digest(&f.command),
        reserve_receipt_digest: digest(&saved),
        policy: SHARED_TEXT_POLICY.into(),
        issued_at_ms: now,
        expires_at_ms: now + 20_000,
    };
    let proof = VerifiedSharedRuntimeDelivery::new(
        enrollment(&f),
        runtime.clone(),
        digest(&runtime),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    let SharedRuntimeClaim::Issued(custody) = f.registry.claim_shared_runtime(proof).await.unwrap()
    else {
        panic!("first process")
    };
    let (mut process, instructions) = spawn(&f, "first_process", false).await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    sink.record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    let stopped = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    // A crash before POST must not strand the next period behind this saved
    // request. Old liability digest permanently pins its original allowance.
    let old_start = f
        .registry
        .prepare_shared_attempt_start(
            enrollment(&f),
            runtime.clone(),
            None,
            "old_unsubmitted_start".into(),
        )
        .await
        .unwrap();
    f.registry
        .prepare_managed_call(call(&f, "first_period_spend", 200))
        .await
        .unwrap();
    let ManagedCallClaim::Issued(spend) = f
        .registry
        .issue_managed_call(call(&f, "first_period_spend", 200))
        .await
        .unwrap()
    else {
        panic!("spend")
    };
    f.registry
        .record_managed_usage(&spend, observed("first_spent", 200))
        .await
        .unwrap();
    let blocked = f
        .registry
        .prepare_shared_demand(
            offer(&f, &runtime, "old_exhausted_message"),
            runtime.clone(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        blocked.disposition(),
        &SharedDemandDisposition::BudgetBlocked
    );
    clock.store(
        original_period.end_ms + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    let next = next_grant(
        &f,
        f.grant.clone(),
        graphs::now_ms().unwrap(),
        "runtime_next_period",
    )
    .await;
    let renew = renewal(&f, next, "runtime_period_renew");
    f.registry
        .renew_managed_agent_period(
            VerifiedManagedAgentPeriodDelivery::new(
                enrollment(&f),
                renew.clone(),
                digest(&renew),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let next_demand = f
        .registry
        .prepare_shared_demand(offer(&f, &runtime, "fresh_message"), runtime.clone(), None)
        .await
        .unwrap();
    assert_eq!(
        next_demand.disposition(),
        &SharedDemandDisposition::StartPrepared
    );
    let input = next_demand.input().unwrap().start.clone();
    assert_eq!(input.budget_observation.remaining_tokens, 200);
    assert_ne!(
        input.budget_observation.liability_digest,
        old_start.input().budget_observation.liability_digest
    );
    let delayed = attempt(
        &runtime,
        old_start.input(),
        "old_delayed_command",
        original_period.end_ms - 1,
    );
    assert!(
        f.registry
            .claim_shared_successor(
                VerifiedSharedAttemptDelivery::new(
                    enrollment(&f),
                    delayed.clone(),
                    digest(&delayed),
                    true,
                    Instant::now() + Duration::from_secs(15)
                )
                .unwrap()
            )
            .await
            .is_err()
    );
    let next = attempt(
        &runtime,
        &input,
        "new_period_process",
        graphs::now_ms().unwrap(),
    );
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(
            VerifiedSharedAttemptDelivery::new(
                enrollment(&f),
                next.clone(),
                digest(&next),
                true,
                Instant::now() + Duration::from_secs(15),
            )
            .unwrap(),
        )
        .await
        .unwrap()
    else {
        panic!("new process")
    };
    let (mut process, instructions) = spawn(&f, "next_process", true).await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    sink.record(process.qualify_shared_tools(&instructions, next.web_search, next.tool_policy.as_ref().map(|p|p.mode.as_str())).await.unwrap())
        .await
        .unwrap();
    let call = f
        .registry
        .prepare_managed_call(call(&f, "new_period_message", 7))
        .await
        .unwrap();
    assert_eq!(call.reserved_tokens, 7);
    let ManagedCallClaim::Issued(spend) = f
        .registry
        .issue_managed_call(super::call(&f, "new_period_message", 7))
        .await
        .unwrap()
    else {
        panic!("new call")
    };
    f.registry
        .record_managed_usage(&spend, observed("next_spent", 7))
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .managed_agent_budget(enrollment(&f), f.child.clone())
            .await
            .unwrap()
            .remaining_tokens,
        193
    );
    sink.record(process.stop_shared_text_observation())
        .await
        .unwrap();
    let identity = f.child.clone();
    f.registry
        .0
        .store
        .call(1024, move |db| {
            let fenced: bool = db.db.query_row(
                "SELECT permanent_fence FROM owner_managed_agents WHERE id=?1",
                [identity],
                |r| r.get(0),
            )?;
            assert!(!fenced);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised fixture"]
async fn recurring_native_expired_unissued_initial_proves_absence_and_replacement_starts() {
    let calendar = Calendar {
        cadence: Cadence::Daily,
        time_zone: "America/Los_Angeles".into(),
    };
    let period = calendar.period_at(graphs::now_ms().unwrap()).unwrap();
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(period.end_ms - 60_000));
    graphs::install_test_clock(&clock);
    let f = fixture_kind(RenewalMode::Automatic, false, Some(clock.clone())).await;
    let saved = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            f.command.command_id.clone(),
            digest(&f.command),
        )
        .await
        .unwrap()
        .unwrap();
    let now = graphs::now_ms().unwrap();
    let initial = SharedRuntimeCommand {
        version: 1,
        capability: SHARED_EXECUTION_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "expired_original_runtime".into(),
        assignment_id: "period_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: f.child.clone(),
        reserve_command: f.command.clone(),
        reserve_command_digest: digest(&f.command),
        reserve_receipt_digest: digest(&saved),
        policy: SHARED_TEXT_POLICY.into(),
        issued_at_ms: now,
        expires_at_ms: now + 20_000,
    };
    let cleanup = || {
        ManagedCleanupAuthority::fixture(
            enrollment(&f),
            f.child.clone(),
            Instant::now() + Duration::from_secs(20),
        )
    };
    assert!(
        f.registry
            .record_unstarted_shared_runtime(cleanup(), initial.clone())
            .await
            .is_err(),
        "not expired"
    );
    clock.store(period.end_ms + 1, std::sync::atomic::Ordering::SeqCst);
    let proof = f
        .registry
        .record_unstarted_shared_runtime(cleanup(), initial.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(proof.state, "not_started");
    assert!(proof.process_digest.is_none());
    assert_eq!(
        digest(&proof),
        digest(
            &f.registry
                .record_unstarted_shared_runtime(cleanup(), initial.clone())
                .await
                .unwrap()
                .unwrap()
        )
    );
    let mut wrong = enrollment(&f);
    wrong.profile = ProfileAuthority::new(
        "b".repeat(64),
        f.profile.account.clone(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    assert!(
        f.registry
            .record_unstarted_shared_runtime(
                ManagedCleanupAuthority::fixture(
                    wrong,
                    f.child.clone(),
                    Instant::now() + Duration::from_secs(20)
                ),
                initial.clone()
            )
            .await
            .is_err()
    );
    // Wall-clock reversal cannot resurrect the explicitly void original.
    clock.store(now + 1, std::sync::atomic::Ordering::SeqCst);
    assert!(
        f.registry
            .claim_shared_runtime(
                VerifiedSharedRuntimeDelivery::new(
                    enrollment(&f),
                    initial.clone(),
                    digest(&initial),
                    true,
                    Instant::now() + Duration::from_secs(15)
                )
                .unwrap()
            )
            .await
            .is_err()
    );
    clock.store(period.end_ms + 1, std::sync::atomic::Ordering::SeqCst);
    let next = next_grant(
        &f,
        f.grant.clone(),
        graphs::now_ms().unwrap(),
        "unissued_next",
    )
    .await;
    let renew = renewal(&f, next, "unissued_renew");
    f.registry
        .renew_managed_agent_period(
            VerifiedManagedAgentPeriodDelivery::new(
                enrollment(&f),
                renew.clone(),
                digest(&renew),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let mut replacement = initial.clone();
    replacement.command_id = "replacement_initial_runtime".into();
    replacement.issued_at_ms = graphs::now_ms().unwrap();
    replacement.expires_at_ms = replacement.issued_at_ms + 20_000;
    let SharedRuntimeClaim::Issued(custody) = f
        .registry
        .claim_shared_runtime(
            VerifiedSharedRuntimeDelivery::new(
                enrollment(&f),
                replacement.clone(),
                digest(&replacement),
                true,
                Instant::now() + Duration::from_secs(15),
            )
            .unwrap(),
        )
        .await
        .unwrap()
    else {
        panic!("fresh initial process")
    };
    let (mut process, instructions) = spawn(&f, "replacement_process", false).await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    sink.record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    sink.record(process.stop_shared_text_observation())
        .await
        .unwrap();
    clock.store(
        replacement.expires_at_ms + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    assert!(
        f.registry
            .record_unstarted_shared_runtime(cleanup(), replacement)
            .await
            .unwrap()
            .is_none(),
        "even stopped actual issuance cannot become never-issued"
    );
    assert_eq!(
        digest(&proof),
        digest(
            &f.registry
                .record_unstarted_shared_runtime(cleanup(), initial.clone())
                .await
                .unwrap()
                .unwrap()
        )
    );
    let recovered=f.registry.recover_shared_process(cleanup(),initial,None).await.unwrap().unwrap();
    assert_eq!(digest(&recovered),digest(&proof));
    let child = f.child.clone();
    f.registry
        .0
        .store
        .call(1024, move |db| {
            assert!(!db.db.query_row(
                "SELECT permanent_fence FROM owner_managed_agents WHERE id=?1",
                [child],
                |r| r.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .await
        .unwrap();
}

/// Claims and qualifies the original process of a fresh recurring Agent.
async fn qualified_process(
    f: &Fixture,
    name: &str,
) -> (SharedRuntimeCommand, SharedProcessSink, OwnedSession) {
    let saved = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            f.command.command_id.clone(),
            digest(&f.command),
        )
        .await
        .unwrap()
        .unwrap();
    let now = graphs::now_ms().unwrap();
    let runtime = SharedRuntimeCommand {
        version: 1,
        capability: SHARED_EXECUTION_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: format!("{name}_runtime"),
        assignment_id: "period_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: f.child.clone(),
        reserve_command: f.command.clone(),
        reserve_command_digest: digest(&f.command),
        reserve_receipt_digest: digest(&saved),
        policy: SHARED_TEXT_POLICY.into(),
        issued_at_ms: now,
        expires_at_ms: now + 20_000,
    };
    let proof = VerifiedSharedRuntimeDelivery::new(
        enrollment(f),
        runtime.clone(),
        digest(&runtime),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    let SharedRuntimeClaim::Issued(custody) = f.registry.claim_shared_runtime(proof).await.unwrap()
    else {
        panic!("first process")
    };
    let (mut process, instructions) = spawn(f, name, false).await;
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    sink.record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    (runtime, sink, process)
}
async fn agent_budget(f: &Fixture) -> budget::AgentBudgetSnapshot {
    f.registry
        .managed_agent_budget(enrollment(f), f.child.clone())
        .await
        .unwrap()
}
async fn issued_call(f: &Fixture, id: &str, tokens: u64) -> ManagedCallCustody {
    f.registry
        .prepare_managed_call(call(f, id, tokens))
        .await
        .unwrap();
    let ManagedCallClaim::Issued(custody) = f
        .registry
        .issue_managed_call(call(f, id, tokens))
        .await
        .unwrap()
    else {
        panic!("call")
    };
    custody
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised fixture"]
async fn recurring_interrupted_call_keeps_unknown_usage_after_stop_without_blocking_next_event() {
    let f = fixture_kind(RenewalMode::Automatic, false, None).await;
    let (runtime, sink, mut process) = qualified_process(&f, "interrupted").await;
    let allocated = f.command.requested_tokens;

    // A running call reserves the remainder, but holds nothing up front.
    let spend = issued_call(&f, "interrupted_call", allocated).await;
    let open = agent_budget(&f).await;
    assert_eq!((open.retained_tokens, open.remaining_tokens), (0, allocated));
    assert!(!open.usage_complete);

    // Partial usage stays unresolved even after the process stops. Physical
    // cleanup cannot prove that all provider usage has arrived.
    f.registry
        .record_managed_usage(&spend, observed_capture("interrupted_usage", 30, false))
        .await
        .unwrap();
    let interrupted = agent_budget(&f).await;
    assert_eq!(
        (interrupted.retained_tokens, interrupted.remaining_tokens),
        (30, allocated - 30)
    );
    assert!(!interrupted.usage_complete);

    let stopped = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    let closed = agent_budget(&f).await;
    assert_eq!(
        (closed.retained_tokens, closed.remaining_tokens),
        (30, allocated - 30)
    );
    assert!(!closed.usage_complete);

    // Late known usage still counts without inventing a final receipt.
    f.registry
        .record_managed_usage(&spend, observed_capture("late_usage", 50, false))
        .await
        .unwrap();
    let late = agent_budget(&f).await;
    assert_eq!(
        (late.retained_tokens, late.remaining_tokens),
        (50, allocated - 50)
    );
    assert!(!late.usage_complete);

    // Reconciliation remains visible, but postpaid admission can start the
    // next event with the remaining known budget.
    let open_calls = f
        .registry
        .interrupted_calls(
            f.profile.clone(),
            InterruptedCallQuery {
                agent_id: f.command.proposed_agent_id.clone(),
            },
        )
        .await
        .unwrap();
    assert_eq!(open_calls.calls.len(), 1);
    let next = f
        .registry
        .prepare_shared_demand(offer(&f, &runtime, "next_message"), runtime.clone(), None)
        .await
        .unwrap();
    assert_eq!(next.disposition(), &SharedDemandDisposition::StartPrepared);
    let observation = &next.input().unwrap().start.budget_observation;
    assert_eq!(observation.remaining_tokens, allocated - 50);
    assert!(!observation.usage_complete);
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised fixture"]
async fn recurring_blocked_event_does_not_hold_back_later_events() {
    let f = fixture_kind(RenewalMode::Automatic, false, None).await;
    let (runtime, sink, mut process) = qualified_process(&f, "blocked_head").await;
    let allocated = f.command.requested_tokens;
    let spend = issued_call(&f, "blocked_head_call", allocated).await;
    f.registry
        .record_managed_usage(&spend, observed("blocked_head_usage", allocated))
        .await
        .unwrap();
    sink.record(process.stop_shared_text_observation())
        .await
        .unwrap();

    // Spent allowance still refuses, and each event gets its own answer.
    for id in ["a_first_message", "b_later_message"] {
        let demand = f
            .registry
            .prepare_shared_demand(offer(&f, &runtime, id), runtime.clone(), None)
            .await
            .unwrap();
        assert_eq!(demand.disposition(), &SharedDemandDisposition::BudgetBlocked);
    }
}
