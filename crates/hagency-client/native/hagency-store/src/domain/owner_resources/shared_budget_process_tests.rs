//! Actual supervised offline process evidence; never a provider/model request.
use super::*;
use crate::owner_resources::budget::*;

fn enrollment(f: &Fixture) -> VerifiedManagementEnrollment {
    enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1)
}
async fn increase(f: &Fixture, c: &SharedRuntimeCommand, id: &str, add: u64) -> AgentBudgetReceipt {
    let receipt = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            c.reserve_command.command_id.clone(),
            c.reserve_command_digest.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let head = f
        .registry
        .managed_agent_budget(enrollment(f), c.agent_reservation_id.clone())
        .await
        .unwrap();
    let command = AgentBudgetCommand { owner_funded: false,
        version: 1,
        capability: AGENT_BUDGET_CAPABILITY.into(),
        operation: "agent.budget.increase".into(),
        command_id: format!("budget_{id}"),
        top_up_id: format!("topup_{id}"),
        top_up_request_digest: "a".repeat(64),
        approval_command_id: format!("approval_{id}"),
        approval_digest: "b".repeat(64),
        approving_administrator_mxid: "@admin:a.test".into(),
        authority_revision: 1,
        assignment_id: c.assignment_id.clone(),
        assignment_generation: 1,
        reserve_command: c.reserve_command.clone(),
        reserve_command_digest: c.reserve_command_digest.clone(),
        reserve_receipt: receipt.clone(),
        reserve_receipt_digest: digest(&receipt),
        expected_budget_revision: head.budget_revision,
        expected_budget_digest: head.budget_digest,
        expected_ceiling_tokens: head.effective_ceiling_tokens,
        add_tokens: add,
        issued_at_ms: graphs::now_ms().unwrap(),
        expires_at_ms: c.reserve_command.expires_at_ms,
    };
    let value = f
        .registry
        .apply_managed_budget(
            VerifiedAgentBudgetDelivery::new(
                enrollment(f),
                command.clone(),
                digest(&command),
                true,
                false,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(value.state, "applied");
    value
}
async fn next(f: &Fixture, c: &SharedRuntimeCommand, id: &str) -> SharedBudgetAttemptCommand {
    let selected = f
        .registry
        .shared_budget_successor_selection(enrollment(f), c.clone())
        .await
        .unwrap();
    let b = selected.budget;
    assert_eq!(b.state, SharedSuccessorBudgetState::Available);
    let now = graphs::now_ms().unwrap();
    let value = SharedBudgetAttemptCommand {
        web_search: false, tool_policy: None,
        version: 3,
        capability: SHARED_BUDGET_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: id.into(),
        assignment_id: c.assignment_id.clone(),
        assignment_generation: 1,
        agent_reservation_id: c.agent_reservation_id.clone(),
        process_attempt: selected.predecessor.process_attempt + 1,
        original_runtime_command: c.clone(),
        original_runtime_digest: digest(c),
        predecessor: selected.predecessor,
        execution_budget: selected.execution_budget,
        budget_observation: SharedBudgetObservation {
            observed_ledger_revision: b.observed_ledger_revision,
            allocated_tokens: b.allocated_tokens,
            known_consumed_tokens: b.known_consumed_tokens,
            retained_tokens: b.retained_tokens,
            remaining_tokens: b.remaining_tokens,
            usage_complete: b.usage_complete,
            liability_digest: b.liability_digest,
        },
        start_intent_id: format!("intent_{id}"),
        issued_at_ms: now,
        expires_at_ms: now + 20_000,
    };
    value.validate(&f.profile.account).unwrap();
    value
}
fn delivery(f: &Fixture, c: SharedBudgetAttemptCommand) -> VerifiedSharedBudgetAttemptDelivery {
    VerifiedSharedBudgetAttemptDelivery::new(
        enrollment(f),
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(15),
    )
    .unwrap()
}
async fn synthetic() -> (Fixture, SharedRuntimeCommand) {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    increase(&f, &c, "initial", 50).await;
    (f, c)
}
#[tokio::test]
async fn budget_execution_v3_claim_is_one_shot_and_rechecks_full_applied_head() {
    let (f, c) = synthetic().await;
    call(&f, &c, "old_held", "unknown", 40, 7, false, false).await;
    let stale = next(&f, &c, "stale").await;
    increase(&f, &c, "newer", 25).await;
    assert!(
        f.registry
            .claim_shared_budget_successor(delivery(&f, stale))
            .await
            .is_err()
    );
    let current = next(&f, &c, "current").await;
    assert_eq!(
        (
            current.budget_observation.allocated_tokens,
            current.budget_observation.retained_tokens,
            current.budget_observation.remaining_tokens
        ),
        (225, 7, 218)
    );
    let (a, b) = tokio::join!(
        f.registry
            .claim_shared_budget_successor(delivery(&f, current.clone())),
        f.registry
            .claim_shared_budget_successor(delivery(&f, current.clone()))
    );
    let results = [a.unwrap(), b.unwrap()];
    assert_eq!(
        results
            .iter()
            .filter(|v| matches!(v, SharedBudgetAttemptClaim::Issued(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|v| matches!(v, SharedBudgetAttemptClaim::History(_)))
            .count(),
        1
    );
    assert!(
        f.registry
            .shared_budget_successor_selection(enrollment(&f), c.clone())
            .await
            .is_err(),
        "unknown latest process cannot be bypassed by old stopped evidence"
    );
    increase(&f, &c, "late", 25).await;
    assert!(
        matches!(
            f.registry
                .claim_shared_budget_successor(delivery(&f, current))
                .await
                .unwrap(),
            SharedBudgetAttemptClaim::History(_)
        ),
        "exact original claim remains historical after a later applied head"
    );
    let original = f
        .registry
        .managed_agent_budget(enrollment(&f), c.agent_reservation_id)
        .await
        .unwrap();
    assert_eq!(original.original_ceiling_tokens, 150);
    assert_eq!(original.known_consumed_tokens, 7);
    assert_eq!(
        original.retained_tokens, 250,
        "unknown process holds effective ceiling"
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn budget_execution_v3_cancel_before_claim_preserves_unknown_and_original_replay() {
    let (mut f, c) = synthetic().await;
    let next = next(&f, &c, "cancelled").await;
    let queued = delivery(&f, next.clone());
    let fence = f
        .registry
        .fence_shared_budget_successor(cleanup(&f, &c, 1).await, next.clone())
        .await
        .unwrap();
    assert!(fence.claimed_revision.is_none());
    assert!(
        f.registry
            .claim_shared_budget_successor(queued)
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let mut changed = next.clone();
    changed.command_id = "replacement".into();
    changed.start_intent_id = "replacement_intent".into();
    assert!(
        f.registry
            .claim_shared_budget_successor(delivery(&f, changed))
            .await
            .is_err()
    );
    assert!(
        f.registry
            .recover_shared_budget_attempt_process(cleanup(&f, &c, 2).await, next, None)
            .await
            .unwrap()
            .is_none()
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn budget_execution_v3_actual_stop_and_later_head_preserve_v1_v2_v3_chain() {
    let (f, c, original_stop, _) = original_stopped().await;
    // First prove v2 -> v3 selection with actual v2 stop, not injected evidence.
    let v2 = attempt(&f, &c).await;
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, v2.clone()))
        .await
        .unwrap()
    else {
        panic!("v2")
    };
    let (mut p2, i2) = spawn(&f, "v2").await;
    let sink2 = (*custody)
        .bind_process(p2.shared_text_process_source())
        .unwrap();
    sink2
        .record(p2.qualify_shared_text(&i2).await.unwrap())
        .await
        .unwrap();
    let stop2 = sink2
        .record(p2.stop_shared_text_observation())
        .await
        .unwrap();
    increase(&f, &c, "actual", 50).await;
    let third = next(&f, &c, "third").await;
    assert_eq!(third.process_attempt, 3);
    assert_eq!(third.predecessor.stopped_receipt_digest, digest(&stop2));
    let SharedBudgetAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_budget_successor(delivery(&f, third.clone()))
        .await
        .unwrap()
    else {
        panic!("v3")
    };
    let (mut p3, i3) = spawn(&f, "v3").await;
    let source = p3.shared_text_process_source();
    let sink3 = (*custody).bind_process(source.clone()).unwrap();
    let q = sink3
        .record(p3.qualify_shared_text(&i3).await.unwrap())
        .await
        .unwrap();
    assert_eq!(q.version, 3);
    assert_eq!(q.budget_digest, third.execution_budget.budget_digest);
    increase(&f, &c, "after_qualification", 25).await;
    let stop3 = sink3
        .record(p3.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(
        stop3.budget_digest, q.budget_digest,
        "new head never rewrites original process evidence"
    );
    let fourth = next(&f, &c, "fourth").await;
    assert_eq!(fourth.predecessor.stopped_receipt_digest, digest(&stop3));
    assert_eq!(fourth.execution_budget.effective_ceiling_tokens, 225);
    let SharedBudgetAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_budget_successor(delivery(&f, fourth))
        .await
        .unwrap()
    else {
        panic!("fourth")
    };
    assert!(
        (*custody).bind_process(source).is_err(),
        "one actual source cannot produce another attempt"
    );
    let saved = f
        .registry
        .recover_shared_process(cleanup(&f, &c, 2).await, c.clone(), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&saved), digest(&original_stop));
    let recovered = f
        .registry
        .recover_shared_budget_attempt_process(
            cleanup(&f, &c, 2).await,
            third,
            Some(q.observation_id.clone()),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&recovered), digest(&q));
    no_model_calls(&i2);
    no_model_calls(&i3);
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn budget_execution_v3_expired_custody_can_bind_only_actual_late_stop() {
    let (mut f, c, _, _) = original_stopped().await;
    increase(&f, &c, "expired", 50).await;
    let next = next(&f, &c, "expired").await;
    let SharedBudgetAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_budget_successor(delivery(&f, next.clone()))
        .await
        .unwrap()
    else {
        panic!("v3")
    };
    let (mut process, instructions) = spawn(&f, "late_bind").await;
    let qualified = process.qualify_shared_text(&instructions).await.unwrap();
    f.profile.retire();
    let sink = (*custody)
        .bind_process(process.shared_text_process_source())
        .unwrap();
    assert!(sink.record(qualified).await.is_err());
    let stop = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stop.state, "stopped");
    f.profile = authority('a');
    let saved = f
        .registry
        .recover_shared_budget_attempt_process(cleanup(&f, &c, 2).await, next, None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&saved), digest(&stop));
    no_model_calls(&instructions);
    f.registry.shutdown().await.unwrap();
}

#[path = "shared_budget_call_tests.rs"]
mod work;

#[tokio::test]
async fn budget_execution_v3_start_journal_recovers_original_head_and_rejects_rebasing() {
    let (mut f,c)=synthetic().await;
    let intent=f.registry.prepare_shared_budget_attempt_start(enrollment(&f),c.clone(),"accepted_v3".into()).await.unwrap();
    let original_input=digest(intent.input());
    let mut command=next(&f,&c,"accepted_command").await;
    command.start_intent_id=intent.input().start_intent_id.clone();
    intent.matches_command(&command).unwrap();
    assert!(f.registry.require_shared_budget_attempt_start(cleanup(&f,&c,1).await,command.clone()).await.unwrap());
    let mut wrong=command.clone();wrong.execution_budget.budget_digest="f".repeat(64);
    assert!(intent.matches_command(&wrong).is_err());
    increase(&f,&c,"after_start_intent",25).await;
    let retry=f.registry.prepare_shared_budget_attempt_start(enrollment(&f),c.clone(),"accepted_v3".into()).await.unwrap();
    assert_eq!(digest(retry.input()),original_input);
    assert!(f.registry.claim_shared_budget_successor(delivery(&f,command)).await.is_err());
    assert!(f.registry.prepare_shared_budget_attempt_start(enrollment(&f),c.clone(),"replacement_v3".into()).await.is_err());
    assert!(f.registry.prepare_shared_attempt_start(enrollment(&f),c.clone(),None,"replacement_v2".into()).await.is_err());
    f.registry.shutdown().await.unwrap();
    f.registry=OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let saved=f.registry.recover_shared_budget_attempt_start(cleanup(&f,&c,2).await,c.clone(),digest(&c)).await.unwrap().unwrap();
    assert_eq!(digest(saved.input()),original_input);
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn budget_execution_v3_does_not_bypass_existing_v2_start_intent() {
    let (f,c)=synthetic().await;
    f.registry.prepare_shared_attempt_start(enrollment(&f),c.clone(),None,"original_v2".into()).await.unwrap();
    assert!(f.registry.prepare_shared_budget_attempt_start(enrollment(&f),c,"new_v3".into()).await.is_err());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn budget_execution_v3_schema90_upgrade_is_atomic_and_keeps_original_ledger() {
    let (mut f,c)=synthetic().await;
    let id=f.registry.id().to_owned();
    let root=f._temp.path().join("owner");
    f.registry.shutdown().await.unwrap();
    let db=Connection::open(root.join("domain.sqlite3")).unwrap();
    let original:String=db.query_row("SELECT command_payload FROM owner_shared_runtime_lifetimes",[],|r|r.get(0)).unwrap();
    let delta:String=db.query_row("SELECT receipt_payload FROM owner_managed_budget_commands",[],|r|r.get(0)).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE owner_namespace_enrollments;DROP TABLE owner_resource_namespaces;DROP TABLE management_account_profiles;DROP TABLE owner_shared_budget_demand_decisions;DROP TABLE owner_shared_budget_work_refusals;DROP TABLE owner_shared_budget_start_intents;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=89;CREATE TABLE owner_shared_budget_work_refusals(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db=Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(db.pragma_query_value(None,"user_version",|r|r.get::<_,i32>(0)).unwrap(),89);
    assert!(!db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='owner_shared_budget_start_intents')",[],|r|r.get::<_,bool>(0)).unwrap());
    assert_eq!(db.query_row("SELECT command_payload FROM owner_shared_runtime_lifetimes",[],|r|r.get::<_,String>(0)).unwrap(),original);
    assert_eq!(db.query_row("SELECT receipt_payload FROM owner_managed_budget_commands",[],|r|r.get::<_,String>(0)).unwrap(),delta);
    db.execute_batch("DROP TABLE owner_shared_budget_work_refusals;").unwrap();drop(db);
    f.registry=OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(f.registry.id(),id);
    let budget=f.registry.managed_agent_budget(enrollment(&f),c.agent_reservation_id).await.unwrap();
    assert_eq!((budget.original_ceiling_tokens,budget.effective_ceiling_tokens,budget.known_consumed_tokens),(150,200,0));
    f.registry.shutdown().await.unwrap();
}

#[path="shared_budget_demand_tests.rs"]
mod budget_demand;

#[tokio::test]
async fn budget_execution_v3_unstarted_fence_recovers_without_replaying_or_releasing_unknown() {
    let (mut f,c)=synthetic().await;
    call(&f,&c,"old_unknown","unknown",40,7,false,false).await;
    let failed=next(&f,&c,"failed_preflight").await;
    assert!(f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,1).await,failed.clone()).await.is_err());
    let fence=f.registry.fence_shared_budget_successor(cleanup(&f,&c,1).await,failed.clone()).await.unwrap();
    assert!(fence.claimed_revision.is_none());
    let r=f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,1).await,failed.clone()).await.unwrap().unwrap();
    assert_eq!(r.state,"not_started");assert!(r.process_digest.is_none());assert!(r.policy_digest.is_none());
    assert!(r.ledger_revision>fence.fence_revision);
    assert_eq!(digest(&r),digest(&f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,1).await,failed.clone()).await.unwrap().unwrap()));
    assert!(f.registry.claim_shared_budget_successor(delivery(&f,failed.clone())).await.is_err());
    f.registry.shutdown().await.unwrap();f.registry=OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let recovered=f.registry.recover_shared_budget_attempt_process(cleanup(&f,&c,2).await,failed.clone(),Some(r.observation_id.clone())).await.unwrap().unwrap();
    assert_eq!(digest(&r),digest(&recovered));
    let successor=next(&f,&c,"after_failed_preflight").await;
    assert_eq!(successor.process_attempt,failed.process_attempt+1);
    assert_eq!(successor.predecessor.command_digest,digest(&failed));
    assert!(successor.predecessor.process_digest.is_empty());
    assert_eq!(successor.predecessor.stopped_receipt_digest,digest(&r));
    assert_eq!((successor.budget_observation.known_consumed_tokens,successor.budget_observation.retained_tokens),(7,7));
    assert!(matches!(f.registry.claim_shared_budget_successor(delivery(&f,successor.clone())).await.unwrap(),SharedBudgetAttemptClaim::Issued(_)));
    f.registry.fence_shared_budget_successor(cleanup(&f,&c,2).await,successor.clone()).await.unwrap();
    assert!(f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,2).await,successor).await.unwrap().is_none(),"one-shot custody cannot be relabelled as never started");
    assert!(f.registry.shared_budget_successor_selection(enrollment(&f),c.clone()).await.is_err(),"an unknown claimed head cannot be bypassed");
    let mut forged=failed;forged.start_intent_id="changed".into();
    assert!(f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,2).await,forged).await.is_err());
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn budget_execution_v3_unstarted_evidence_rolls_back_and_is_retained() {
    let(f,c)=synthetic().await;let failed=next(&f,&c,"atomic_unstarted").await;
    f.registry.fence_shared_budget_successor(cleanup(&f,&c,1).await,failed.clone()).await.unwrap();
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER fail_unstarted BEFORE INSERT ON owner_shared_unstarted_attempts BEGIN SELECT RAISE(ABORT,'injected'); END;")?;Ok(())}).await.unwrap();
    assert!(f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,1).await,failed.clone()).await.is_err());
    assert!(f.registry.recover_shared_budget_attempt_process(cleanup(&f,&c,1).await,failed.clone(),None).await.unwrap().is_none());
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("DROP TRIGGER fail_unstarted;")?;Ok(())}).await.unwrap();
    f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,1).await,failed).await.unwrap().unwrap();
    f.registry.0.store.call(1024,|db|{assert!(db.db.execute("DELETE FROM owner_shared_unstarted_attempts",[]).is_err());assert!(db.db.execute("UPDATE owner_shared_unstarted_attempts SET digest='changed'",[]).is_err());Ok(())}).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn budget_execution_v3_schema101_upgrade_retains_unclaimed_fence_and_rolls_back(){
    let(mut f,c)=synthetic().await;let failed=next(&f,&c,"migration_unclaimed").await;
    f.registry.fence_shared_budget_successor(cleanup(&f,&c,1).await,failed.clone()).await.unwrap();
    let root=f._temp.path().join("owner");f.registry.shutdown().await.unwrap();
    let db=Connection::open(root.join("domain.sqlite3")).unwrap();
    let fence:String=db.query_row("SELECT command_payload FROM owner_shared_successor_fences",[],|r|r.get(0)).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE owner_shared_budget_demand_terminals;DROP TABLE owner_shared_unstarted_attempts;PRAGMA user_version=101;CREATE TABLE owner_shared_budget_demand_terminals(collision INTEGER);").unwrap();drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db=Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(db.pragma_query_value(None,"user_version",|r|r.get::<_,i32>(0)).unwrap(),101);
    assert!(!db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='owner_shared_unstarted_attempts')",[],|r|r.get::<_,bool>(0)).unwrap());
    assert_eq!(db.query_row("SELECT command_payload FROM owner_shared_successor_fences",[],|r|r.get::<_,String>(0)).unwrap(),fence);
    db.execute_batch("DROP TABLE owner_shared_budget_demand_terminals").unwrap();drop(db);
    f.registry=OwnerResourceRegistry::open_root(&root).unwrap();
    let r=f.registry.record_unstarted_shared_budget_attempt(cleanup(&f,&c,1).await,failed).await.unwrap().unwrap();assert_eq!(r.state,"not_started");
    f.registry.shutdown().await.unwrap();f.registry=OwnerResourceRegistry::open_root(&root).unwrap();
    assert!(next(&f,&c,"after_migration").await.predecessor.process_digest.is_empty());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn budget_execution_v3_uncertain_claim_blocks_only_until_grace_then_chain_continues() {
    use std::sync::{Arc, atomic::AtomicU64};
    let (f, c) = synthetic().await;
    call(&f, &c, "old_unknown", "unknown", 40, 7, false, false).await;
    let claimed = next(&f, &c, "claimed_then_lost").await;
    assert!(matches!(f.registry.claim_shared_budget_successor(delivery(&f, claimed.clone())).await.unwrap(), SharedBudgetAttemptClaim::Issued(_)));
    f.registry.fence_shared_budget_successor(cleanup(&f, &c, 1).await, claimed.clone()).await.unwrap();
    assert!(f.registry.record_shared_budget_attempt_timeout(cleanup(&f, &c, 1).await, claimed.clone()).await.unwrap().is_none(), "within the grace period");
    assert!(f.registry.shared_budget_successor_selection(enrollment(&f), c.clone()).await.is_err(), "no second process before the bound");
    let clock = Arc::new(AtomicU64::new(claimed.expires_at_ms + 30_000 + UNCERTAIN_GRACE_MS));
    let worker = clock.clone();
    f.registry.0.store.call(1024, move |_| { graphs::install_test_clock(&worker); Ok(()) }).await.unwrap();
    let timeout = f.registry.record_shared_budget_attempt_timeout(cleanup(&f, &c, 1).await, claimed.clone()).await.unwrap().unwrap();
    assert_eq!((timeout.state.as_str(), timeout.reason.as_deref()), ("stopped", Some(UNCERTAIN_TIMEOUT)));
    assert_eq!(timeout.budget_digest, claimed.execution_budget.budget_digest);
    drop(clock);
    let following = next(&f, &c, "after_uncertain").await;
    assert_eq!(following.predecessor.command_id, claimed.command_id);
    assert_eq!(Some(&following.predecessor.process_digest), timeout.process_digest.as_ref());
    assert_eq!(following.predecessor.stopped_receipt_digest, digest(&timeout));
    assert_eq!((following.budget_observation.known_consumed_tokens, following.budget_observation.retained_tokens), (7, 7));
    assert!(matches!(f.registry.claim_shared_budget_successor(delivery(&f, following)).await.unwrap(), SharedBudgetAttemptClaim::Issued(_)));
    f.registry.shutdown().await.unwrap();
}
