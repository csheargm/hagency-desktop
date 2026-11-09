//! Synthetic ledger evidence exercises the read-only boundary. Actual stopped
//! evidence provenance remains covered by the supervised process writer tests.
use super::*;
#[path = "shared_successor_inventory_tests.rs"]
mod inventory;
#[path = "shared_successor_process_tests.rs"]
mod observed;
#[path = "shared_successor_unstarted_tests.rs"]
mod unstarted;

#[test]
fn shared_successor_v2_matches_central_fixture_and_does_not_reuse_old_spawn_window() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "capacity/managed_agents/custody/shared_execution/management-shared-attempt-v2.json"
    ))
    .unwrap();
    let c: SharedAttemptCommand = serde_json::from_value(fixture["command"].clone()).unwrap();
    c.validate(&c.original_runtime_command.reserve_command.manager_mxid)
        .unwrap();
    assert_eq!(digest(&c), fixture["commandDigest"]);
    assert!(c.issued_at_ms > c.original_runtime_command.expires_at_ms);
    let receipt: SharedAttemptProcessReceipt =
        serde_json::from_value(fixture["receipt"].clone()).unwrap();
    receipt.validate_for(&c).unwrap();
    assert_eq!(digest(&receipt), fixture["receiptDigest"]);
    let mut invalid = c.clone();
    invalid.budget_observation.usage_complete = true;
    assert!(
        invalid
            .validate(&c.original_runtime_command.reserve_command.manager_mxid)
            .is_err()
    );
    let mut wrong = serde_json::to_value(&c).unwrap();
    wrong["executionAllowed"] = true.into();
    assert!(serde_json::from_value::<SharedAttemptCommand>(wrong).is_err());
    assert!(
        serde_json::from_value::<SharedProcessReceipt>(serde_json::to_value(receipt).unwrap())
            .is_err()
    );
}

fn fixture_revision(db: &Connection) -> Result<u64, Error> {
    Ok(db.query_row(
        "UPDATE owner_resource_identity SET revision=revision+1 WHERE singleton=1 RETURNING revision",
        [], |r| r.get(0),
    )?)
}

async fn stopped(f: &Fixture, c: &SharedRuntimeCommand, state: &str) {
    let c = c.clone();
    let state = state.to_owned();
    f.registry.0.store.call(8192, move |db| {
        let tx = db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (incarnation, provider): (String, String) = tx.query_row(
            "SELECT incarnation,provider_digest FROM owner_shared_runtime_lifetimes WHERE child_id=?1",[&c.agent_reservation_id],|r| Ok((r.get(0)?,r.get(1)?)),
        )?;
        let revision = fixture_revision(&tx)?;
        let command_digest = digest(&c);
        let value = SharedProcessReceipt {
            version:1,observation_id:format!("fixture_{state}"),command_id:c.command_id.clone(),
            command_digest:command_digest.clone(),assignment_id:c.assignment_id.clone(),
            agent_reservation_id:c.agent_reservation_id.clone(),ledger_revision:revision,
            process_digest:Some(canonical::digest(&json!({"commandDigest":command_digest,"incarnation":incarnation,"providerDigest":provider}))?),
            state,policy_digest:None,
        };
        value.validate_for(&c)?;
        tx.execute("INSERT INTO owner_shared_process_observations VALUES(?1,?2,?3,?4,?5)",params![value.observation_id,c.agent_reservation_id,canonical::encode(&serde_json::to_value(&value)?)?,digest(&value),revision])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
}

#[allow(clippy::too_many_arguments)]
async fn call(
    f: &Fixture,
    c: &SharedRuntimeCommand,
    id: &str,
    state: &str,
    reserved: u64,
    consumed: u64,
    complete: bool,
    overflow: bool,
) {
    let child = c.agent_reservation_id.clone();
    let id = id.to_owned();
    let state = state.to_owned();
    f.registry.0.store.call(4096,move |db| {
        let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let binding:String=tx.query_row("SELECT binding_digest FROM owner_shared_runtime_lifetimes WHERE child_id=?1",[&child],|r|r.get(0))?;
        let immutable=canonical::digest(&json!({"child":child,"assignmentDigest":binding,"execution":id,"executionDigest":"d".repeat(64),"estimatedTokens":reserved}))?;
        tx.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete,unrepresentable_usage) VALUES(?1,?2,?1,?3,1,?4,?5,?6,?7,?8)",params![id,child,immutable,state,reserved,consumed,complete,overflow])?;
        let prepared=fixture_revision(&tx)?;
        let issued=if matches!(state.as_str(),"prepared"|"cancelled_before_issue"){None}else{Some(fixture_revision(&tx)?)};
        tx.execute("INSERT INTO owner_managed_call_pins VALUES(?1,?2,?3,?4,?5)",params![id,binding,"d".repeat(64),prepared,issued])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
}

async fn budget(f: &Fixture, c: &SharedRuntimeCommand) -> Result<SharedSuccessorBudget, Error> {
    f.registry
        .shared_successor_budget(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            c.clone(),
        )
        .await
}

async fn attempt(f: &Fixture, c: &SharedRuntimeCommand) -> SharedAttemptCommand {
    let budget = budget(f, c).await.unwrap();
    let child = c.agent_reservation_id.clone();
    let process = f.registry.0.store.call(1024, move |db| {
        Ok(db.db.query_row("SELECT json_extract(payload,'$.processDigest') FROM owner_shared_process_observations WHERE child_id=?1 AND json_extract(payload,'$.state')='stopped'", [child], |r| r.get::<_,String>(0))?)
    }).await.unwrap();
    let now = graphs::now_ms().unwrap();
    SharedAttemptCommand {
        web_search: false, tool_policy: None,
        version: 2,
        capability: SHARED_ATTEMPT_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "successor_two".into(),
        assignment_id: c.assignment_id.clone(),
        assignment_generation: 1,
        agent_reservation_id: c.agent_reservation_id.clone(),
        process_attempt: 2,
        original_runtime_command: c.clone(),
        original_runtime_digest: digest(c),
        predecessor: SharedAttemptPredecessor {
            command_id: c.command_id.clone(),
            command_digest: digest(c),
            process_attempt: 1,
            stopped_receipt_digest: budget.stopped_receipt_digest,
            process_digest: process,
        },
        budget_observation: SharedBudgetObservation {
            observed_ledger_revision: budget.observed_ledger_revision,
            allocated_tokens: budget.allocated_tokens,
            known_consumed_tokens: budget.known_consumed_tokens,
            retained_tokens: budget.retained_tokens,
            remaining_tokens: budget.remaining_tokens,
            usage_complete: budget.usage_complete,
            liability_digest: budget.liability_digest,
        },
        start_intent_id: "new_start_intent".into(),
        issued_at_ms: now,
        expires_at_ms: now + 20_000,
    }
}
fn attempt_proof(f: &Fixture, c: SharedAttemptCommand) -> VerifiedSharedAttemptDelivery {
    VerifiedSharedAttemptDelivery::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(15),
    )
    .unwrap()
}

#[tokio::test]
async fn shared_successor_claim_is_one_shot_and_keeps_original_unknown_charge() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    call(&f, &c, "old_unknown", "unknown", 40, 7, false, false).await;
    let next = attempt(&f, &c).await;
    let (left, right) = tokio::join!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, next.clone())),
        f.registry
            .claim_shared_successor(attempt_proof(&f, next.clone()))
    );
    let mut issued = 0;
    let mut historical = 0;
    for result in [left.unwrap(), right.unwrap()] {
        match result {
            SharedAttemptClaim::Issued(custody) => {
                custody.check().unwrap();
                assert_eq!(custody.command().command_id, next.command_id);
                assert!(!custody.incarnation().is_empty());
                issued += 1;
            }
            SharedAttemptClaim::History(saved) => {
                assert_eq!(digest(&saved.command), digest(&next));
                historical += 1;
            }
        }
    }
    assert_eq!((issued, historical), (1, 1));
    let current = budget(&f, &c).await.unwrap();
    assert_eq!(
        (
            current.known_consumed_tokens,
            current.retained_tokens,
            current.remaining_tokens
        ),
        (7, 7, 143)
    );
    let mut replacement = next.clone();
    replacement.command_id = "new_delivery_id".into();
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, replacement))
            .await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        f.registry.claim_shared_runtime(proof(&f, c)).await.unwrap(),
        SharedRuntimeClaim::History(_)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_claim_rechecks_stale_budget_and_rolls_back_failed_insert() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    let stale = attempt(&f, &c).await;
    call(&f, &c, "late", "settled", 50, 12, true, false).await;
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, stale))
            .await,
        Err(Error::Conflict)
    ));
    let next = attempt(&f, &c).await;
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER fixture_reject_successor BEFORE INSERT ON owner_shared_successor_attempts BEGIN SELECT RAISE(ABORT,'fixture');END;")?;Ok(())}).await.unwrap();
    let before = budget(&f, &c).await.unwrap().observed_ledger_revision;
    assert!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, next.clone()))
            .await
            .is_err()
    );
    assert_eq!(
        budget(&f, &c).await.unwrap().observed_ledger_revision,
        before
    );
    f.registry
        .0
        .store
        .call(1024, |db| {
            db.db
                .execute_batch("DROP TRIGGER fixture_reject_successor;")?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, next))
            .await
            .unwrap(),
        SharedAttemptClaim::Issued(_)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_delivery_refuses_history_and_changed_predecessor() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    let mut next = attempt(&f, &c).await;
    assert!(
        VerifiedSharedAttemptDelivery::new(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            next.clone(),
            digest(&next),
            false,
            Instant::now() + Duration::from_secs(10)
        )
        .is_err()
    );
    next.predecessor.process_digest = "a".repeat(64);
    assert!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, next))
            .await
            .is_err()
    );
    let next = attempt(&f, &c).await;
    f.registry
        .claim_shared_successor(attempt_proof(&f, next))
        .await
        .unwrap();
    let immutable = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok((
                db.db
                    .execute("DELETE FROM owner_shared_successor_attempts", [])
                    .is_err(),
                db.db
                    .execute(
                        "UPDATE owner_shared_successor_attempts SET start_intent_id='different'",
                        [],
                    )
                    .is_err(),
            ))
        })
        .await
        .unwrap();
    assert_eq!(immutable, (true, true));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_requires_exact_stopped_custody_not_unknown_or_replacement_claim() {
    let f = fixture().await;
    let c = command(&f).await;
    assert!(budget(&f, &c).await.is_err());
    let claim = f
        .registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    assert!(matches!(claim, SharedRuntimeClaim::Issued(_)));
    assert!(matches!(budget(&f, &c).await, Err(Error::OutcomeUnknown)));
    stopped(&f, &c, "unknown").await;
    assert!(matches!(budget(&f, &c).await, Err(Error::OutcomeUnknown)));
    stopped(&f, &c, "stopped").await;
    let b = budget(&f, &c).await.unwrap();
    assert_eq!(b.state, SharedSuccessorBudgetState::Available);
    assert_eq!((b.retained_tokens, b.remaining_tokens), (0, 150));
    let mut replacement = c.clone();
    replacement.command_id = "replacement".into();
    assert!(budget(&f, &replacement).await.is_err());
    assert!(
        f.registry
            .shared_successor_budget(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2),
                c.clone(),
            )
            .await
            .is_err()
    );
    // Readiness cannot revive the old process right.
    assert!(matches!(
        f.registry.claim_shared_runtime(proof(&f, c)).await.unwrap(),
        SharedRuntimeClaim::History(_)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_queue_rechecks_profile_retirement_before_snapshot() {
    let mut f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    let initial = budget(&f, &c).await.unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let check = live.clone();
    f.profile = f
        .profile
        .with_native_gate(move || check.load(Ordering::Acquire))
        .unwrap();
    let enrollment = enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1);
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
    let mut pending = Box::pin(f.registry.shared_successor_budget(enrollment, c.clone()));
    tokio::select! {biased; _=&mut pending=>panic!("read bypassed writer"),_=tokio::task::yield_now()=>{}}
    live.store(false, Ordering::Release);
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(pending.await.is_err());
    f.profile = authority('a');
    let current = budget(&f, &c).await.unwrap();
    assert_eq!(
        initial.observed_ledger_revision,
        current.observed_ledger_revision
    );
    assert_eq!(initial.liability_digest, current.liability_digest);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_incomplete_calls_hold_known_usage_without_blanket_history_refusal() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    call(&f, &c, "partial", "settled", 40, 10, false, false).await;
    let a = budget(&f, &c).await.unwrap();
    assert_eq!(
        (
            a.known_consumed_tokens,
            a.retained_tokens,
            a.remaining_tokens
        ),
        (10, 10, 140)
    );
    assert_eq!(a.state, SharedSuccessorBudgetState::Available);
    assert!(!a.usage_complete);
    call(&f, &c, "uncertain_start", "unknown", 60, 0, false, false).await;
    let b = budget(&f, &c).await.unwrap();
    assert_eq!((b.retained_tokens, b.remaining_tokens), (10, 140));
    assert_ne!(a.liability_digest, b.liability_digest);
    call(&f, &c, "complete", "settled", 50, 12, true, false).await;
    call(
        &f,
        &c,
        "cancelled",
        "cancelled_before_issue",
        50,
        0,
        false,
        false,
    )
    .await;
    let before = budget(&f, &c).await.unwrap();
    let after = budget(&f, &c).await.unwrap();
    assert_eq!(
        (
            after.known_consumed_tokens,
            after.retained_tokens,
            after.remaining_tokens
        ),
        (22, 22, 128)
    );
    assert_eq!(
        before.observed_ledger_revision,
        after.observed_ledger_revision
    );
    assert_eq!(before.liability_digest, after.liability_digest);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_exhaustion_overrun_and_unrepresentable_usage_never_refund() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    // Known usage, not the reservation, exhausts the Agent.
    call(&f, &c, "all_headroom", "unknown", 150, 150, false, false).await;
    let b = budget(&f, &c).await.unwrap();
    assert_eq!((b.retained_tokens, b.remaining_tokens), (150, 0));
    assert_eq!(b.state, SharedSuccessorBudgetState::Exhausted);
    call(&f, &c, "late_overrun", "settled", 1, 51, true, false).await;
    let b = budget(&f, &c).await.unwrap();
    assert_eq!(
        (
            b.known_consumed_tokens,
            b.retained_tokens,
            b.remaining_tokens
        ),
        (201, 201, 0)
    );
    call(&f, &c, "unrepresentable", "unknown", 1, 0, false, true).await;
    assert_eq!(
        budget(&f, &c).await.unwrap().state,
        SharedSuccessorBudgetState::Unrepresentable
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_missing_call_custody_and_aggregate_overflow_fail_closed() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    let child = c.agent_reservation_id.clone();
    f.registry.0.store.call(2048,move|db|{
        db.db.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES('missing',?1,'missing',?2,1,'settled',40,1,1)",params![child,"f".repeat(64)])?;Ok(())
    }).await.unwrap();
    let b = budget(&f, &c).await.unwrap();
    assert_eq!(b.state, SharedSuccessorBudgetState::Unrepresentable);
    assert_eq!((b.retained_tokens, b.remaining_tokens), (1, 0));
    call(
        &f,
        &c,
        "overflow",
        "unknown",
        0,
        JSON_SAFE_MAX,
        false,
        false,
    )
    .await;
    assert!(matches!(budget(&f, &c).await, Err(Error::Capacity)));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_schema81_rollback_reopen_retains_old_holds_and_history_only() {
    let mut f = fixture().await;
    let original = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap();
    stopped(&f, &original, "stopped").await;
    call(&f, &original, "old_unknown", "unknown", 40, 7, false, false).await;
    let before = budget(&f, &original).await.unwrap();
    let path = f._temp.path().join("owner/domain.sqlite3");
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE owner_shared_successor_fences;DROP TABLE owner_shared_successor_call_pins;DROP TABLE owner_shared_successor_observations;DROP TABLE owner_shared_successor_attempts;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=80;CREATE TABLE owner_shared_successor_observations(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        80
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='owner_shared_successor_attempts'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TABLE owner_shared_successor_observations;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let after = budget(&f, &original).await.unwrap();
    assert_eq!(before.liability_digest, after.liability_digest);
    assert_eq!(
        (after.known_consumed_tokens, after.retained_tokens),
        (7, 7)
    );
    let next = attempt(&f, &original).await;
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, next.clone()))
            .await
            .unwrap(),
        SharedAttemptClaim::Issued(_)
    ));
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, next.clone()))
            .await
            .unwrap(),
        SharedAttemptClaim::History(_)
    ));
    let cleanup = ManagedCleanupAuthority::fixture(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2),
        original.agent_reservation_id.clone(),
        Instant::now() + Duration::from_secs(10),
    );
    let observed = f
        .registry
        .shared_successor_history(cleanup, next.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(observed.command_digest, digest(&next));
    assert!(
        VerifiedSharedAttemptDelivery::new(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2),
            next.clone(),
            digest(&next),
            true,
            Instant::now() + Duration::from_secs(10)
        )
        .is_err()
    );
    let released = apply(&f.registry, &f.profile, release(&f, "after_successor")).await;
    assert_eq!(released.reason.as_deref(), Some("unsettled_usage"));
    f.registry.shutdown().await.unwrap();
}

async fn cleanup(
    f: &Fixture,
    c: &SharedRuntimeCommand,
    generation: u64,
) -> ManagedCleanupAuthority {
    f.registry
        .shared_cleanup_authority(
            enrollment_from(
                &f.profile,
                &f.resource,
                &f.grant.resource_revision,
                generation,
            ),
            c.clone(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn shared_successor_cancel_before_claim_survives_restart_and_never_invents_stopped() {
    let mut f = fixture().await;
    let original = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap();
    stopped(&f, &original, "stopped").await;
    call(&f, &original, "old_unknown", "unknown", 40, 7, false, false).await;
    let c = attempt(&f, &original).await;
    let delayed = attempt_proof(&f, c.clone());
    let a = cleanup(&f, &original, 1).await;
    let fence = f
        .registry
        .fence_shared_successor(a, c.clone())
        .await
        .unwrap();
    assert_eq!(fence.command_digest, digest(&c));
    assert_eq!(fence.claimed_revision, None);
    assert!(f.registry.claim_shared_successor(delayed).await.is_err());
    let mut replacement = c.clone();
    replacement.command_id = "replacement_after_stop".into();
    replacement.start_intent_id = "replacement_start".into();
    assert!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, replacement))
            .await
            .is_err()
    );
    let root = f._temp.path().join("owner");
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let replay = f
        .registry
        .fence_shared_successor(cleanup(&f, &original, 2).await, c.clone())
        .await
        .unwrap();
    assert_eq!(replay.fence_revision, fence.fence_revision);
    assert_eq!(replay.claimed_revision, None);
    assert!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, c))
            .await
            .is_err()
    );
    let rows=f.registry.0.store.call(1024,|db|Ok(db.db.query_row("SELECT (SELECT count(*) FROM owner_shared_successor_attempts),(SELECT count(*) FROM owner_shared_successor_observations),(SELECT count(*) FROM owner_shared_successor_fences),(SELECT reserved_tokens FROM owner_managed_calls WHERE id='old_unknown'),(SELECT known_consumed_tokens FROM owner_managed_calls WHERE id='old_unknown')",[],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?,r.get::<_,u64>(2)?,r.get::<_,u64>(3)?,r.get::<_,u64>(4)?)))?)).await.unwrap();
    assert_eq!(rows, (0, 0, 1, 40, 7));
    // The SQL fence remains effective even if a future writer accidentally
    // omits the Rust preflight. No existing attempt/uniqueness error masks it.
    let child = original.agent_reservation_id.clone();
    let profile = f.profile.digest.clone();
    let error = f.registry.0.store.call(2048, move |db| {
        let provider:String=db.db.query_row("SELECT provider_digest FROM owner_shared_runtime_lifetimes WHERE child_id=?1",[&child],|r|r.get(0))?;
        Ok(db.db.execute("INSERT INTO owner_shared_successor_attempts VALUES('raw_delayed',?1,?2,2,'shared_original',?3,?3,'raw_start','{}',?3,?3,?3,?4,1,900,'raw_incarnation')",params![child,profile,"d".repeat(64),provider]).unwrap_err().to_string())
    }).await.unwrap();
    assert!(error.contains("successor permanently fenced"));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_cancel_after_claim_keeps_claim_unknown_and_exact_replay_historical() {
    let f = fixture().await;
    let original = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap();
    stopped(&f, &original, "stopped").await;
    let c = attempt(&f, &original).await;
    let issued = f
        .registry
        .claim_shared_successor(attempt_proof(&f, c.clone()))
        .await
        .unwrap();
    assert!(matches!(issued, SharedAttemptClaim::Issued(_)));
    let fence = f
        .registry
        .fence_shared_successor(cleanup(&f, &original, 1).await, c.clone())
        .await
        .unwrap();
    assert!(
        fence
            .claimed_revision
            .is_some_and(|r| r < fence.fence_revision)
    );
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, c.clone()))
            .await
            .unwrap(),
        SharedAttemptClaim::History(_)
    ));
    let rows = f
        .registry
        .0
        .store
        .call(1024, |db| {
            let observations: u64 = db.db.query_row(
                "SELECT count(*) FROM owner_shared_successor_observations",
                [],
                |r| r.get(0),
            )?;
            let immutable = db
                .db
                .execute(
                    "UPDATE owner_shared_successor_fences SET claimed_revision=NULL",
                    [],
                )
                .is_err();
            let retained = db
                .db
                .execute("DELETE FROM owner_shared_successor_fences", [])
                .is_err();
            Ok((observations, immutable, retained))
        })
        .await
        .unwrap();
    assert_eq!(rows, (0, true, true));
    let receipt = apply(
        &f.registry,
        &f.profile,
        release(&f, "after_successor_fence"),
    )
    .await;
    assert_eq!(receipt.reason.as_deref(), Some("unsettled_usage"));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_cancel_and_claim_serialize_without_second_custody() {
    let f = fixture().await;
    let original = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap();
    stopped(&f, &original, "stopped").await;
    let c = attempt(&f, &original).await;
    let a = cleanup(&f, &original, 1).await;
    let (fence, claim) = tokio::join!(
        f.registry.fence_shared_successor(a, c.clone()),
        f.registry
            .claim_shared_successor(attempt_proof(&f, c.clone()))
    );
    let fence = fence.unwrap();
    match claim {
        Ok(SharedAttemptClaim::Issued(_)) => assert!(fence.claimed_revision.is_some()),
        Err(_) => assert!(fence.claimed_revision.is_none()),
        Ok(SharedAttemptClaim::History(_)) => panic!("no previous claim existed"),
    }
    assert!(!matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, c))
            .await,
        Ok(SharedAttemptClaim::Issued(_))
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_start_intent_replays_original_body_after_budget_changes_and_restart() {
    let mut f = fixture().await;
    let original = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap();
    stopped(&f, &original, "stopped").await;
    let e = || enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1);
    let saved = f
        .registry
        .prepare_shared_attempt_start(e(), original.clone(), None, "accepted_start".into())
        .await
        .unwrap();
    let before = digest(saved.input());
    let rev = saved.input().budget_observation.observed_ledger_revision;
    call(&f, &original, "late_old", "unknown", 40, 7, false, false).await;
    let replay = f
        .registry
        .prepare_shared_attempt_start(e(), original.clone(), None, "accepted_start".into())
        .await
        .unwrap();
    assert_eq!(digest(replay.input()), before);
    assert_eq!(
        replay.input().budget_observation.observed_ledger_revision,
        rev
    );
    assert!(
        f.registry
            .prepare_shared_attempt_start(e(), original.clone(), None, "another_start".into())
            .await
            .is_err(),
        "poll/start cannot replace an uncertain original intent"
    );
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let restored = f
        .registry
        .prepare_shared_attempt_start(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            original.clone(),
            None,
            "accepted_start".into(),
        )
        .await
        .unwrap();
    assert_eq!(digest(restored.input()), before);
    assert!(
        f.registry
            .prepare_shared_attempt_start(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2),
                original,
                None,
                "accepted_start".into()
            )
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_successor_start_intent_does_not_stale_its_own_command_and_is_immutable() {
    let f = fixture().await;
    let original = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap();
    stopped(&f, &original, "stopped").await;
    let saved = f
        .registry
        .prepare_shared_attempt_start(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            original.clone(),
            None,
            "accepted_start".into(),
        )
        .await
        .unwrap();
    let mut c = attempt(&f, &original).await;
    c.start_intent_id = saved.input().start_intent_id.clone();
    c.predecessor = saved.input().predecessor.clone();
    c.budget_observation = saved.input().budget_observation.clone();
    saved.matches_command(&c).unwrap();
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, c))
            .await
            .unwrap(),
        SharedAttemptClaim::Issued(_)
    ));
    f.registry
        .0
        .store
        .call(1024, |db| {
            assert!(
                db.db
                    .execute("DELETE FROM owner_shared_start_intents", [])
                    .is_err()
            );
            assert!(
                db.db
                    .execute(
                        "UPDATE owner_shared_start_intents SET device_generation=2",
                        []
                    )
                    .is_err()
            );
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_schema82_failed_upgrade_retains_original_stop_and_charge() {
    let mut f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    call(&f, &c, "old_unknown", "unknown", 40, 7, false, false).await;
    let before = budget(&f, &c).await.unwrap();
    let path = f._temp.path().join("owner/domain.sqlite3");
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE owner_shared_start_intents;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=81;CREATE TABLE owner_shared_start_intents(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        81
    );
    assert_eq!(
        db.query_row(
            "SELECT known_consumed_tokens FROM owner_managed_calls WHERE id='old_unknown'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        7
    );
    db.execute_batch("DROP TABLE owner_shared_start_intents")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let after = budget(&f, &c).await.unwrap();
    assert_eq!(after.liability_digest, before.liability_digest);
    assert_eq!((after.retained_tokens, after.remaining_tokens), (7, 143));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_successor_settled_incomplete_calls_keep_parent_and_seat_overrun_floor() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    call(&f, &c, "partial_one", "settled", 600, 600, false, false).await;
    call(&f, &c, "partial_two", "settled", 600, 600, false, false).await;
    let child = budget(&f, &c).await.unwrap();
    assert_eq!(
        (
            child.known_consumed_tokens,
            child.retained_tokens,
            child.remaining_tokens
        ),
        (1200, 1200, 0)
    );
    let resource = f
        .registry
        .resource(&f.profile, f.resource.clone())
        .await
        .unwrap();
    assert_eq!(u64::from(resource.budget.pool.committed), 1200);
    assert_eq!(u64::from(resource.budget.seat.committed), 1200);
    assert_eq!(resource.budget.pool.remaining, None);
    f.registry.shutdown().await.unwrap();
}

#[cfg(feature = "shared-intake")]
#[path = "shared_demand_tests.rs"]
mod demand_tests;

#[cfg(not(feature = "shared-intake"))]
#[path = "shared_demand_disabled_tests.rs"]
mod demand_disabled_tests;
