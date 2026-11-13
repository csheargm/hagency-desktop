use super::*;
fn offer(c: &SharedRuntimeCommand, id: &str, revision: u64) -> SharedDemandOffer {
    SharedDemandOffer {
        version: 1,
        demand_id: id.into(),
        assignment_id: c.assignment_id.clone(),
        event_id: format!("${id}"),
        event_digest: "e".repeat(64),
        control_revision: revision,
        received_at_ms: graphs::now_ms().unwrap(),
        predecessor_command_id: c.command_id.clone(),
        predecessor_command_digest: digest(c),
    }
}
fn verified(f: &Fixture, o: SharedDemandOffer) -> VerifiedSharedDemand {
    VerifiedSharedDemand::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        o.clone(),
        digest(&o),
        graphs::now_ms().unwrap(),
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
async fn count(f: &Fixture, table: &'static str) -> u64 {
    f.registry
        .0
        .store
        .call(1024, move |db| {
            Ok(db
                .db
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
        })
        .await
        .unwrap()
}
/// Demand ids in recovery order: the full walk, or only unfinished demands.
async fn recovery_walk(f: &Fixture, device: &str, unfinished: bool) -> Vec<String> {
    let mut after = None;
    let mut ids = Vec::new();
    loop {
        let next = if unfinished {
            f.registry.next_unfinished_shared_demand(f.profile.clone(), device.into(), after.clone()).await
        } else {
            f.registry.next_shared_demand(f.profile.clone(), device.into(), after.clone()).await
        };
        let Some(saved) = next.unwrap() else { return ids };
        let id = saved.intent().offer().demand_id.clone();
        ids.push(id.clone());
        after = Some(id);
    }
}

async fn original(stop: bool) -> (Fixture, SharedRuntimeCommand) {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap();
    if stop {
        stopped(&f, &c, "stopped").await;
    }
    (f, c)
}
#[test]
fn shared_demand_proposed_wire_matches_independent_canonical_fixture() {
    let value:serde_json::Value=serde_json::from_str(include_str!("capacity/managed_agents/custody/shared_execution/demand/management-shared-demand-v1.proposed.json")).unwrap();
    let o: SharedDemandOffer = serde_json::from_value(value["offer"].clone()).unwrap();
    assert_eq!(o.digest().unwrap(), value["demandDigest"]);
    let input: SharedDemandStartInput = serde_json::from_value(value["start"].clone()).unwrap();
    input.validate_for(&o).unwrap();
    assert_eq!(digest(&input), value["startDigest"]);
    let b: SharedDemandBinding = serde_json::from_value(value["binding"].clone()).unwrap();
    let w: SharedWorkItem = serde_json::from_value(value["promotedWork"].clone()).unwrap();
    b.validate_for(&o, &w).unwrap();
    assert_eq!(digest(&b), value["bindingDigest"]);
    let mut wrong = serde_json::to_value(&o).unwrap();
    wrong["executionAllowed"] = true.into();
    assert!(serde_json::from_value::<SharedDemandOffer>(wrong).is_err());
}
#[tokio::test]
async fn shared_demand_atomic_start_replay_restart_never_mints_another_id() {
    let (mut f, c) = original(true).await;
    let o = offer(&c, "durable", 1);
    let saved = f
        .registry
        .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
        .await
        .unwrap();
    assert_eq!(*saved.disposition(), SharedDemandDisposition::StartPrepared);
    let before = digest(saved.input().unwrap());
    call(&f, &c, "old_charge", "unknown", 40, 7, false, false).await;
    let again = f
        .registry
        .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
        .await
        .unwrap();
    assert_eq!(digest(again.input().unwrap()), before);
    f.registry
        .stop_shared_demand_revision(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let recovered = f
        .registry
        .recover_shared_demand(cleanup(&f, &c, 1).await, o.demand_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(recovered.input().unwrap()), before);
    assert_eq!(count(&f, "owner_shared_start_intents").await, 1);
    assert_eq!(count(&f, "owner_shared_demands").await, 1);
    let mut conflict = o.clone();
    conflict.event_digest = "f".repeat(64);
    assert!(
        f.registry
            .prepare_shared_demand(verified(&f, conflict), c.clone(), None)
            .await
            .is_err()
    );
    assert!(
        f.registry
            .prepare_shared_demand(verified(&f, offer(&c, "another", 1)), c, None)
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_demand_decision_failure_rolls_back_offer_and_start_together() {
    let (f, c) = original(true).await;
    let o = offer(&c, "rollback", 1);
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER demand_fail BEFORE INSERT ON owner_shared_demand_decisions BEGIN SELECT RAISE(ABORT,'fixture');END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
            .await
            .is_err()
    );
    for t in [
        "owner_shared_start_intents",
        "owner_shared_demands",
        "owner_shared_demand_controls",
    ] {
        assert_eq!(count(&f, t).await, 0);
    }
    f.registry
        .0
        .store
        .call(1024, |db| {
            db.db.execute_batch("DROP TRIGGER demand_fail")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        *f.registry
            .prepare_shared_demand(verified(&f, o), c, None)
            .await
            .unwrap()
            .disposition(),
        SharedDemandDisposition::StartPrepared
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_demand_exhausted_retains_budget_and_has_no_start_identifier() {
    let (f, c) = original(true).await;
    call(&f, &c, "overrun", "settled", 150, 201, true, false).await;
    let o = offer(&c, "blocked", 1);
    for _ in 0..3 {
        let saved = f
            .registry
            .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
            .await
            .unwrap();
        assert_eq!(*saved.disposition(), SharedDemandDisposition::BudgetBlocked);
        assert!(saved.input().is_none());
        let b = &saved.budget().unwrap().observation.as_ref().unwrap();
        assert_eq!(
            (b.allocated_tokens, b.retained_tokens, b.remaining_tokens),
            (150, 201, 0)
        );
    }
    assert_eq!(count(&f, "owner_shared_start_intents").await, 0);
    assert_eq!(count(&f, "owner_shared_demand_decisions").await, 1);
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_demand_unknown_process_waits_and_stop_revision_blocks_delayed_offer() {
    let (f, c) = original(false).await;
    let o = offer(&c, "pending", 1);
    let pending = f
        .registry
        .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
        .await
        .unwrap();
    assert_eq!(*pending.disposition(), SharedDemandDisposition::Pending);
    assert_eq!(count(&f, "owner_shared_start_intents").await, 0);
    f.registry
        .stop_shared_demand_revision(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    assert!(
        f.registry
            .prepare_shared_demand(verified(&f, o), c.clone(), None)
            .await
            .is_err()
    );
    let fresh = offer(&c, "later_revision", 2);
    assert_eq!(
        *f.registry
            .prepare_shared_demand(verified(&f, fresh), c, None)
            .await
            .unwrap()
            .disposition(),
        SharedDemandDisposition::StartPrepared
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_demand_queued_retirement_cannot_persist_original_request() {
    let (mut f, c) = original(true).await;
    let o = offer(&c, "queued", 1);
    let live = Arc::new(AtomicBool::new(true));
    let check = live.clone();
    f.profile = f
        .profile
        .with_native_gate(move || check.load(Ordering::Acquire))
        .unwrap();
    let p = verified(&f, o);
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
    let mut pending = Box::pin(f.registry.prepare_shared_demand(p, c, None));
    tokio::select! {biased;_=&mut pending=>panic!("bypassed writer"),_=tokio::task::yield_now()=>{}}
    live.store(false, Ordering::Release);
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    assert_eq!(count(&f, "owner_shared_demands").await, 0);
    assert_eq!(count(&f, "owner_shared_start_intents").await, 0);
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_demand_schema85_failed_upgrade_preserves_old_charge() {
    let (mut f, c) = original(true).await;
    call(&f, &c, "old_unknown", "unknown", 40, 7, false, false).await;
    let before = budget(&f, &c).await.unwrap();
    f.registry.shutdown().await.unwrap();
    let path = f._temp.path().join("owner/domain.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_octos_model_resources;DROP TABLE IF EXISTS owner_octos_enrollments;DROP TABLE IF EXISTS owner_octos_profiles;DROP TABLE IF EXISTS owner_octos_key;DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE owner_shared_demand_bindings;DROP TABLE owner_shared_demand_decisions;DROP TABLE owner_shared_demands;DROP TABLE owner_shared_demand_controls;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=84;CREATE TABLE owner_shared_demand_decisions(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        84
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='owner_shared_demands'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TABLE owner_shared_demand_decisions")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let after = budget(&f, &c).await.unwrap();
    assert_eq!(before.liability_digest, after.liability_digest);
    assert_eq!((after.retained_tokens, after.remaining_tokens), (7, 143));
    f.registry.shutdown().await.unwrap();
}

async fn revision(f: &Fixture) -> u64 {
    f.registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT revision FROM owner_resource_identity WHERE singleton=1",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn shared_demand_queue_bound_dedup_and_pending_replay_do_not_churn() {
    let (f, c) = original(false).await;
    let first = offer(&c, "queue_0", 1);
    f.registry
        .prepare_shared_demand(verified(&f, first.clone()), c.clone(), None)
        .await
        .unwrap();
    let rev = revision(&f).await;
    for _ in 0..3 {
        let saved = f
            .registry
            .prepare_shared_demand(verified(&f, first.clone()), c.clone(), None)
            .await
            .unwrap();
        assert_eq!(*saved.disposition(), SharedDemandDisposition::Pending);
        assert!(saved.input().is_none());
    }
    assert_eq!(revision(&f).await, rev);
    let mut duplicate = first.clone();
    duplicate.demand_id = "duplicate_event".into();
    assert!(
        f.registry
            .prepare_shared_demand(verified(&f, duplicate), c.clone(), None)
            .await
            .is_err()
    );
    assert_eq!(revision(&f).await, rev);
    for i in 1..100 {
        let saved = f
            .registry
            .prepare_shared_demand(
                verified(&f, offer(&c, &format!("queue_{i}"), 1)),
                c.clone(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(*saved.disposition(), SharedDemandDisposition::Pending);
    }
    let rev = revision(&f).await;
    assert!(matches!(
        f.registry
            .prepare_shared_demand(
                verified(&f, offer(&c, "queue_overflow", 1)),
                c.clone(),
                None
            )
            .await,
        Err(Error::Capacity)
    ));
    assert_eq!(revision(&f).await, rev);
    assert_eq!(count(&f, "owner_shared_demands").await, 100);
    assert_eq!(count(&f, "owner_shared_start_intents").await, 0);
    // A later event cannot overtake the original after its process stops.
    stopped(&f, &c, "stopped").await;
    let later = f
        .registry
        .prepare_shared_demand(verified(&f, offer(&c, "later", 1)), c.clone(), None)
        .await;
    assert!(matches!(later, Err(Error::Capacity)));
    let oldest = f
        .registry
        .prepare_shared_demand(verified(&f, first), c, None)
        .await
        .unwrap();
    assert_eq!(
        *oldest.disposition(),
        SharedDemandDisposition::StartPrepared
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_start_is_excluded_from_every_generic_recovery_route() {
    let (f, c) = original(true).await;
    let saved = f
        .registry
        .prepare_shared_demand(
            verified(&f, offer(&c, "separate_route", 1)),
            c.clone(),
            None,
        )
        .await
        .unwrap();
    let input = saved.input().unwrap().start.clone();
    assert!(
        f.registry
            .next_shared_attempt_start(f.profile.clone(), None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.registry
            .latest_shared_attempt_start(f.profile.clone(), c.assignment_id.clone())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.registry
            .recover_shared_attempt_start(cleanup(&f, &c, 1).await, c.clone(), None)
            .await
            .unwrap()
            .is_none()
    );
    let inventory = f
        .registry
        .shared_successor_inventory(
            f.profile.clone(),
            c.reserve_command.executor_device_id.clone(),
            1,
            None,
        )
        .await
        .unwrap();
    assert!(!inventory.items[0].saved_start);
    assert!(matches!(
        f.registry
            .prepare_shared_attempt_start(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                c.clone(),
                None,
                input.start_intent_id.clone()
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    // The same already leased exact command is still restricted by local Stop.
    let mut next = attempt(&f, &c).await;
    next.start_intent_id = input.start_intent_id;
    next.predecessor = input.predecessor;
    next.budget_observation = input.budget_observation;
    f.registry
        .stop_shared_demand_revision(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    assert!(matches!(
        f.registry
            .claim_shared_successor(attempt_proof(&f, next))
            .await,
        Err(Error::LocalAuthority)
    ));
    assert_eq!(count(&f, "owner_shared_successor_attempts").await, 0);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_head_change_is_retained_without_rebinding_event() {
    let (f, c) = original(false).await;
    let o = offer(&c, "original_head", 1);
    f.registry
        .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
        .await
        .unwrap();
    stopped(&f, &c, "stopped").await;
    let next = attempt(&f, &c).await;
    let claim = f
        .registry
        .claim_shared_successor(attempt_proof(&f, next))
        .await
        .unwrap();
    assert!(matches!(claim, SharedAttemptClaim::Issued(_)));
    for _ in 0..2 {
        let saved = f
            .registry
            .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
            .await
            .unwrap();
        assert_eq!(*saved.disposition(), SharedDemandDisposition::HeadChanged);
        assert_eq!(saved.offer().predecessor_command_id, c.command_id);
        assert!(saved.input().is_none());
    }
    assert_eq!(count(&f, "owner_shared_demands").await, 1);
    assert_eq!(count(&f, "owner_shared_start_intents").await, 0);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_foreign_generation_and_profile_cannot_replace_or_read() {
    let (f, c) = original(true).await;
    let o = offer(&c, "scope", 1);
    f.registry
        .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
        .await
        .unwrap();
    let proof = VerifiedSharedDemand::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2),
        o.clone(),
        digest(&o),
        graphs::now_ms().unwrap(),
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert!(matches!(
        f.registry
            .prepare_shared_demand(proof, c.clone(), None)
            .await,
        Err(Error::LocalAuthority)
    ));
    let foreign = authority('b');
    assert!(
        f.registry
            .shared_cleanup_authority(
                enrollment_from(&foreign, &f.resource, &f.grant.resource_revision, 1),
                c.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(count(&f, "owner_shared_demands").await, 1);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_submission_and_cursor_preserve_original_scope_and_stop() {
    let (f, c) = original(true).await;
    let o = offer(&c, "submission", 1);
    let saved = f
        .registry
        .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
        .await
        .unwrap();
    let cursor = f
        .registry
        .next_shared_demand(
            f.profile.clone(),
            c.reserve_command.executor_device_id.clone(),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(cursor.original()), digest(&c));
    assert_eq!(
        digest(cursor.intent().input().unwrap()),
        digest(saved.input().unwrap())
    );
    assert!(
        f.registry
            .next_shared_demand(f.profile.clone(), "other_device".into(), None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.registry
            .next_shared_demand(
                f.profile.clone(),
                c.reserve_command.executor_device_id.clone(),
                Some(o.demand_id.clone())
            )
            .await
            .unwrap()
            .is_none()
    );
    let submission = f
        .registry
        .shared_demand_submission(verified(&f, o.clone()), c.clone())
        .await
        .unwrap();
    submission.check().unwrap();
    assert_eq!(digest(submission.input()), digest(saved.input().unwrap()));
    f.registry
        .stop_shared_demand_revision(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    assert!(
        f.registry
            .shared_demand_submission(verified(&f, o.clone()), c.clone())
            .await
            .is_err()
    );
    // Saved request remains descriptive after Stop, never a new ID or permission.
    assert_eq!(
        digest(
            f.registry
                .recover_shared_demand(cleanup(&f, &c, 1).await, o.demand_id)
                .await
                .unwrap()
                .unwrap()
                .input()
                .unwrap()
        ),
        digest(saved.input().unwrap())
    );
    f.registry.shutdown().await.unwrap();
    assert!(submission.check().is_err());
}

#[tokio::test]
async fn shared_demand_unrepresentable_aggregate_is_retained_without_fabricated_budget() {
    let (f, c) = original(true).await;
    // Known usage past the ledger's exact range, not a large reservation.
    call(
        &f,
        &c,
        "overflow_one",
        "unknown",
        1,
        JSON_SAFE_MAX,
        false,
        false,
    )
    .await;
    call(&f, &c, "overflow_two", "unknown", 1, 1, false, false).await;
    let o = offer(&c, "overflow_demand", 1);
    for _ in 0..2 {
        let saved = f
            .registry
            .prepare_shared_demand(verified(&f, o.clone()), c.clone(), None)
            .await
            .unwrap();
        assert_eq!(*saved.disposition(), SharedDemandDisposition::BudgetBlocked);
        assert!(saved.input().is_none());
        let b = saved.budget().unwrap();
        assert_eq!(b.state, "unrepresentable");
        assert!(b.observation.is_none());
    }
    assert_eq!(count(&f, "owner_shared_start_intents").await, 0);
    assert_eq!(count(&f, "owner_managed_calls").await, 2);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_stop_explicit_original_is_distinct_from_observed_cancellation() {
    let (mut f, c) = original(true).await;
    f.registry
        .stop_shared_demand_revision(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    assert!(
        f.registry
            .next_shared_demand_stop(
                f.profile.clone(),
                c.reserve_command.executor_device_id.clone(),
                None
            )
            .await
            .unwrap()
            .is_none()
    );
    let first = f
        .registry
        .prepare_shared_demand_stop(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    let input = serde_json::to_value(first.input()).unwrap();
    let again = f
        .registry
        .prepare_shared_demand_stop(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    assert_eq!(serde_json::to_value(again.input()).unwrap(), input);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let saved = f
        .registry
        .next_shared_demand_stop(
            f.profile.clone(),
            c.reserve_command.executor_device_id.clone(),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(serde_json::to_value(saved.input()).unwrap(), input);
    assert!(
        f.registry
            .next_shared_demand_stop(f.profile.clone(), "different_device".into(), None)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.registry
            .record_shared_demand_stop(
                cleanup(&f, &c, 1).await,
                saved.input().command_id.clone(),
                4
            )
            .await
            .is_err()
    );
    f.registry
        .record_shared_demand_stop(
            cleanup(&f, &c, 1).await,
            saved.input().command_id.clone(),
            2,
        )
        .await
        .unwrap();
    f.registry
        .record_shared_demand_stop(
            cleanup(&f, &c, 1).await,
            saved.input().command_id.clone(),
            2,
        )
        .await
        .unwrap();
    assert!(
        f.registry
            .next_shared_demand_stop(
                f.profile.clone(),
                c.reserve_command.executor_device_id.clone(),
                None
            )
            .await
            .unwrap()
            .is_none()
    );
    let later = f
        .registry
        .prepare_shared_demand_stop(cleanup(&f, &c, 1).await, 3)
        .await
        .unwrap();
    assert_ne!(later.input().command_id, saved.input().command_id);
    assert_eq!(count(&f, "owner_shared_stop_intents").await, 2);
    assert_eq!(count(&f, "owner_shared_stop_receipts").await, 1);
    f.registry
        .0
        .store
        .call(1024, |db| {
            assert!(
                db.db
                    .execute("DELETE FROM owner_shared_stop_intents", [])
                    .is_err()
            );
            assert!(
                db.db
                    .execute(
                        "UPDATE owner_shared_stop_receipts SET applied_revision=4",
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
async fn shared_demand_stop_failed_writer_rolls_back_command_and_restriction() {
    let (f, c) = original(true).await;
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER stop_fail BEFORE INSERT ON owner_shared_demand_controls BEGIN SELECT RAISE(ABORT,'fixture');END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .prepare_shared_demand_stop(cleanup(&f, &c, 1).await, 1)
            .await
            .is_err()
    );
    assert_eq!(count(&f, "owner_shared_stop_intents").await, 0);
    assert_eq!(count(&f, "owner_shared_demand_controls").await, 0);
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_demand_stop_schema86_failed_upgrade_preserves_original_717_overrun() {
    let (mut f, c) = original(true).await;
    call(&f, &c, "overrun86", "settled", 150, 867, true, false).await;
    let before = budget(&f, &c).await.unwrap();
    assert_eq!(before.retained_tokens - before.allocated_tokens, 717);
    f.registry.shutdown().await.unwrap();
    let path = f._temp.path().join("owner/domain.sqlite3");
    let db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_octos_model_resources;DROP TABLE IF EXISTS owner_octos_enrollments;DROP TABLE IF EXISTS owner_octos_profiles;DROP TABLE IF EXISTS owner_octos_key;DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE owner_shared_stop_receipts;DROP TABLE owner_shared_stop_intents;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=85;CREATE TABLE owner_shared_stop_receipts(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        85
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='owner_shared_stop_intents'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TABLE owner_shared_stop_receipts")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let after = budget(&f, &c).await.unwrap();
    assert_eq!(before.liability_digest, after.liability_digest);
    assert_eq!(after.retained_tokens, 867);
    assert_eq!(after.remaining_tokens, 0);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_stop_denies_old_manual_delivery_after_later_run() {
    let (f, c) = original(true).await;
    let input = f
        .registry
        .prepare_shared_attempt_start(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            c.clone(),
            None,
            "manual_before_stop".into(),
        )
        .await
        .unwrap();
    f.registry
        .prepare_shared_demand_stop(cleanup(&f, &c, 1).await, 1)
        .await
        .unwrap();
    let v = input.input();
    let attempt = SharedAttemptCommand {
        web_search: false, tool_policy: None,
        version: 2,
        capability: SHARED_ATTEMPT_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "old_manual_delivery".into(),
        assignment_id: c.assignment_id.clone(),
        assignment_generation: 1,
        agent_reservation_id: c.agent_reservation_id.clone(),
        process_attempt: 2,
        original_runtime_digest: digest(&c),
        original_runtime_command: c.clone(),
        predecessor: v.predecessor.clone(),
        budget_observation: v.budget_observation.clone(),
        start_intent_id: v.start_intent_id.clone(),
        issued_at_ms: graphs::now_ms().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + 20000,
    };
    // A new authenticated Run offer cannot breathe authority into an unrelated
    // cached manual command. The pending competing original remains retained.
    f.registry
        .prepare_shared_demand(verified(&f, offer(&c, "later_run", 3)), c.clone(), None)
        .await
        .unwrap();
    let p = VerifiedSharedAttemptDelivery::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        attempt.clone(),
        digest(&attempt),
        true,
        Instant::now() + Duration::from_secs(15),
    )
    .unwrap();
    assert!(f.registry.claim_shared_successor(p).await.is_err());
    assert_eq!(count(&f, "owner_shared_successor_attempts").await, 0);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_reviewed_stop_exact_id_revision_restart_and_conflict() {
    let (mut f, c) = original(true).await;
    let input = |id: &str, r| SharedDemandStopInput {
        command_id: id.into(),
        expected_revision: r,
        mode: "paused".into(),
    };
    let first = f
        .registry
        .prepare_reviewed_shared_demand_stop(cleanup(&f, &c, 1).await, input("human_original", 1))
        .await
        .unwrap();
    assert_eq!(first.input().command_id, "human_original");
    for (id, rev) in [("human_original", 3), ("different", 1)] {
        assert!(
            f.registry
                .prepare_reviewed_shared_demand_stop(cleanup(&f, &c, 1).await, input(id, rev))
                .await
                .is_err()
        );
    }
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let again = f
        .registry
        .prepare_reviewed_shared_demand_stop(cleanup(&f, &c, 1).await, input("human_original", 1))
        .await
        .unwrap();
    assert_eq!(again.input().expected_revision, 1);
    f.registry
        .record_shared_demand_stop(cleanup(&f, &c, 1).await, "human_original".into(), 2)
        .await
        .unwrap();
    f.registry
        .prepare_reviewed_shared_demand_stop(cleanup(&f, &c, 1).await, input("human_later", 3))
        .await
        .unwrap();
    assert_eq!(count(&f, "owner_shared_stop_intents").await, 2);
    assert_eq!(count(&f, "owner_shared_stop_receipts").await, 1);
}

#[tokio::test]
async fn shared_demand_local_owner_stop_needs_exact_lifetime_profile_device_and_revision() {
    let (f, c) = original(true).await;
    let input = SharedDemandStopInput {
        command_id: "offline_owner_stop".into(),
        expected_revision: 1,
        mode: "paused".into(),
    };
    let device = c.reserve_command.executor_device_id.clone();
    for (p, d, g, command) in [
        (f.profile.clone(), "foreign_device".to_owned(), 1, c.clone()),
        (f.profile.clone(), device.clone(), 0, c.clone()),
        (
            ProfileAuthority::new(
                "f".repeat(64),
                f.profile.account.clone(),
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
            device.clone(),
            1,
            c.clone(),
        ),
        (f.profile.clone(), device.clone(), 1, {
            let mut v = c.clone();
            v.assignment_id = "foreign_assignment".into();
            v
        }),
    ] {
        assert!(
            f.registry
                .prepare_local_owner_demand_stop(p, d, g, command, input.clone())
                .await
                .is_err()
        );
    }
    assert_eq!(count(&f, "owner_shared_stop_intents").await, 0);
    let first = f
        .registry
        .prepare_local_owner_demand_stop(
            f.profile.clone(),
            device.clone(),
            1,
            c.clone(),
            input.clone(),
        )
        .await
        .unwrap();
    assert_eq!(first.input().command_id, input.command_id);
    let mut wrong = input.clone();
    wrong.expected_revision = 3;
    assert!(
        f.registry
            .prepare_local_owner_demand_stop(f.profile.clone(), device.clone(), 1, c.clone(), wrong)
            .await
            .is_err()
    );
    let later_generation = f
        .registry
        .prepare_local_owner_demand_stop(f.profile.clone(), device, 2, c, input)
        .await
        .unwrap();
    assert_eq!(later_generation.input().expected_revision, 1);
    let through: u64 = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT stopped_through FROM owner_shared_demand_controls",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(through, 1);
    assert_eq!(count(&f, "owner_shared_stop_intents").await, 1);
    assert_eq!(count(&f, "owner_shared_stop_receipts").await, 0);
}

#[tokio::test]
async fn shared_demand_local_owner_stop_queued_retirement_and_atomic_rollback() {
    let (f, c) = original(true).await;
    let live = Arc::new(AtomicBool::new(true));
    let check = live.clone();
    let p = f
        .profile
        .clone()
        .with_native_gate(move || check.load(Ordering::Acquire))
        .unwrap();
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
    let input = SharedDemandStopInput {
        command_id: "queued_local_stop".into(),
        expected_revision: 1,
        mode: "paused".into(),
    };
    let mut pending = Box::pin(f.registry.prepare_local_owner_demand_stop(
        p,
        c.reserve_command.executor_device_id.clone(),
        1,
        c.clone(),
        input.clone(),
    ));
    tokio::select! {biased;_=&mut pending=>panic!("bypassed writer"),_=tokio::task::yield_now()=>{}}
    live.store(false, Ordering::Release);
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(pending.await.is_err());
    assert_eq!(count(&f, "owner_shared_stop_intents").await, 0);
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER fail_local_stop BEFORE INSERT ON owner_shared_demand_controls BEGIN SELECT RAISE(ABORT,'fixture');END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .prepare_local_owner_demand_stop(
                f.profile.clone(),
                c.reserve_command.executor_device_id.clone(),
                1,
                c,
                input
            )
            .await
            .is_err()
    );
    assert_eq!(count(&f, "owner_shared_stop_intents").await, 0);
    assert_eq!(count(&f, "owner_shared_demand_controls").await, 0);
}

#[tokio::test]
async fn shared_demand_v2_terminal_unstarted_demand_unblocks_only_new_event() {
    let (mut f, c) = original(true).await;
    call(&f, &c, "retained_unknown", "unknown", 40, 7, false, false).await;
    let old = offer(&c, "old_failed_demand", 1);
    let input = f.registry.prepare_shared_demand(verified(&f, old.clone()), c.clone(), None).await.unwrap().input().unwrap().clone();
    // The issued attempt is never claimed: cleanup fences it permanently.
    let mut failed = attempt(&f, &c).await;
    failed.start_intent_id = input.start.start_intent_id.clone();
    failed.predecessor = input.start.predecessor.clone();
    failed.budget_observation = input.start.budget_observation.clone();
    f.registry.fence_shared_successor(cleanup(&f, &c, 1).await, failed.clone()).await.unwrap();
    let mut new = offer(&c, "new_research", 1);
    new.received_at_ms = old.received_at_ms + 1;
    new.predecessor_command_id = failed.command_id.clone();
    new.predecessor_command_digest = digest(&failed);
    let target = f.registry.shared_successor_selection(f.profile.clone(), c.assignment_id.clone()).await.unwrap().unwrap();
    assert_eq!(digest(target.predecessor().unwrap()), digest(&failed));
    let waiting = f.registry.prepare_shared_demand(verified(&f, new.clone()), c.clone(), target.predecessor().cloned()).await.unwrap();
    assert_eq!(*waiting.disposition(), SharedDemandDisposition::Pending, "the lost event still holds the queue");
    let device = c.reserve_command.executor_device_id.clone();
    let unfinished = recovery_walk(&f, &device, true).await;
    assert!(unfinished.contains(&old.demand_id) && unfinished.contains(&new.demand_id));
    let a = || cleanup(&f, &c, 1);
    assert!(f.registry.retain_shared_demand_terminal(a().await, old.clone(), "start_committed".into(), None).await.is_err());
    f.registry.retain_shared_demand_terminal(a().await, old.clone(), "head_changed".into(), Some(failed.command_id.clone())).await.unwrap();
    f.registry.retain_shared_demand_terminal(a().await, old.clone(), "head_changed".into(), Some(failed.command_id.clone())).await.unwrap();
    assert!(f.registry.retain_shared_demand_terminal(a().await, old.clone(), "expired".into(), None).await.is_err(), "a terminal state never changes");
    assert_eq!(count(&f, "owner_shared_budget_demand_terminals").await, 1);
    // The fast recovery pass skips the finished demand; the full walk still
    // visits it (a bound demand's central state can still turn cancelled).
    let unfinished = recovery_walk(&f, &device, true).await;
    assert!(unfinished.contains(&new.demand_id) && !unfinished.contains(&old.demand_id));
    let all = recovery_walk(&f, &device, false).await;
    assert!(all.contains(&new.demand_id) && all.contains(&old.demand_id));
    assert!(f.registry.shared_demand_submission(verified(&f, old.clone()), c.clone()).await.is_err(), "the interrupted event never starts again");
    assert!(f.registry.prepare_shared_demand(verified(&f, old.clone()), c.clone(), Some(failed.clone())).await.is_err());
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let input = f.registry.prepare_shared_demand(verified(&f, new.clone()), c.clone(), Some(failed.clone())).await.unwrap().input().unwrap().clone();
    assert_eq!(input.demand_digest, digest(&new));
    assert!(input.start.predecessor.process_digest.is_empty());
    assert_eq!(input.start.predecessor.process_attempt, failed.process_attempt);
    assert_eq!((input.start.budget_observation.known_consumed_tokens, input.start.budget_observation.retained_tokens), (7, 7));
    let mut fresh = failed.clone();
    fresh.command_id = "new_research_runtime".into();
    fresh.process_attempt = failed.process_attempt + 1;
    fresh.predecessor = input.start.predecessor.clone();
    fresh.budget_observation = input.start.budget_observation.clone();
    fresh.start_intent_id = input.start.start_intent_id.clone();
    let now = graphs::now_ms().unwrap();
    fresh.issued_at_ms = now;
    fresh.expires_at_ms = now + 20_000;
    assert!(matches!(f.registry.claim_shared_successor(attempt_proof(&f, fresh)).await.unwrap(), SharedAttemptClaim::Issued(_)));
    assert!(f.registry.claim_shared_successor(attempt_proof(&f, failed)).await.is_err());
    assert_eq!(count(&f, "owner_shared_demands").await, 2);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_v2_never_started_attempt_retries_same_event_in_place() {
    let (f, c) = original(true).await;
    call(&f, &c, "retained_unknown", "unknown", 40, 7, false, false).await;
    let lost = offer(&c, "retried_event", 1);
    let input = f.registry.prepare_shared_demand(verified(&f, lost.clone()), c.clone(), None).await.unwrap().input().unwrap().clone();
    let mut later = offer(&c, "later_event", 1);
    later.received_at_ms = lost.received_at_ms + 1;
    let mut failed = attempt(&f, &c).await;
    failed.start_intent_id = input.start.start_intent_id.clone();
    failed.predecessor = input.start.predecessor.clone();
    failed.budget_observation = input.start.budget_observation.clone();
    let retry = |n: u64, predecessor: Option<SharedAttemptCommand>| {
        f.registry.prepare_shared_demand_retry(verified(&f, lost.clone()), c.clone(), predecessor, n)
    };
    assert!(retry(1, Some(failed.clone())).await.is_err(), "no retry without the local unclaimed fence");
    f.registry.fence_shared_successor(cleanup(&f, &c, 1).await, failed.clone()).await.unwrap();
    let target = f.registry.shared_successor_selection(f.profile.clone(), c.assignment_id.clone()).await.unwrap().unwrap();
    assert_eq!(digest(target.predecessor().unwrap()), digest(&failed));
    assert!(retry(2, target.predecessor().cloned()).await.is_err(), "retries are numbered without gaps");
    assert!(retry(MAX_DEMAND_RETRIES + 1, target.predecessor().cloned()).await.is_err(), "the cap holds locally");
    let SharedDemandRetry::Start(first) = retry(1, target.predecessor().cloned()).await.unwrap() else {
        panic!("a retry start");
    };
    let start = first.input().clone();
    assert_eq!(start.retry, Some(1));
    assert_eq!(start.start.start_intent_id, format!("sdr1_{}", lost.demand_id));
    assert_eq!(start.start.predecessor.command_id, failed.command_id);
    assert!(start.start.predecessor.process_digest.is_empty());
    start.validate_for(&lost).unwrap();
    let SharedDemandRetry::Start(again) = retry(1, target.predecessor().cloned()).await.unwrap() else {
        panic!("a retry start");
    };
    assert_eq!(digest(again.input()), digest(&start), "a replay reuses the same start, never a new one");
    // The retried event keeps its FIFO place; manual start recovery never adopts it.
    let waiting = f.registry.prepare_shared_demand(verified(&f, later.clone()), c.clone(), target.predecessor().cloned()).await.unwrap();
    assert_eq!(*waiting.disposition(), SharedDemandDisposition::Pending);
    assert!(f.registry.recover_shared_attempt_start(cleanup(&f, &c, 1).await, c.clone(), Some(failed.clone())).await.unwrap().is_none());
    // The retry attempt is the demand's own, pinned to its Run revision.
    let mut retried = failed.clone();
    retried.command_id = "retried_runtime".into();
    retried.process_attempt = failed.process_attempt + 1;
    retried.predecessor = start.start.predecessor.clone();
    retried.budget_observation = start.start.budget_observation.clone();
    retried.start_intent_id = start.start.start_intent_id.clone();
    let now = graphs::now_ms().unwrap();
    retried.issued_at_ms = now;
    retried.expires_at_ms = now + 20_000;
    let SharedAttemptClaim::Issued(custody) = f.registry.claim_shared_successor(attempt_proof(&f, retried.clone())).await.unwrap() else {
        panic!("the retry is claimed");
    };
    assert_eq!(custody.demand_control_revision(), Some(1));
    let (owner, n) = f.registry.shared_demand_for_attempt(cleanup(&f, &c, 1).await, retried.clone()).await.unwrap().unwrap();
    assert_eq!((owner.demand_id.as_str(), n), (lost.demand_id.as_str(), 1));
    // A claimed attempt may have run: the event is never retried again.
    assert!(retry(2, Some(retried)).await.is_err());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_v2_claimed_attempt_is_never_retried() {
    let (f, c) = original(true).await;
    call(&f, &c, "retained_unknown", "unknown", 40, 7, false, false).await;
    let lost = offer(&c, "claimed_event", 1);
    let input = f.registry.prepare_shared_demand(verified(&f, lost.clone()), c.clone(), None).await.unwrap().input().unwrap().clone();
    let mut claimed = attempt(&f, &c).await;
    claimed.start_intent_id = input.start.start_intent_id.clone();
    claimed.predecessor = input.start.predecessor.clone();
    claimed.budget_observation = input.start.budget_observation.clone();
    let now = graphs::now_ms().unwrap();
    claimed.issued_at_ms = now;
    claimed.expires_at_ms = now + 20_000;
    assert!(matches!(f.registry.claim_shared_successor(attempt_proof(&f, claimed.clone())).await.unwrap(), SharedAttemptClaim::Issued(_)));
    let (owner, n) = f.registry.shared_demand_for_attempt(cleanup(&f, &c, 1).await, claimed.clone()).await.unwrap().unwrap();
    assert_eq!((owner.demand_id, n), (lost.demand_id.clone(), 0));
    for predecessor in [None, Some(claimed.clone())] {
        assert!(matches!(
            f.registry.prepare_shared_demand_retry(verified(&f, lost.clone()), c.clone(), predecessor, 1).await,
            Err(Error::LocalAuthority | Error::Conflict)
        ));
    }
    f.registry.shutdown().await.unwrap();
}

/// The prepared retry R1 of a never-started event, claimed here.
async fn claimed_retry(f: &Fixture, c: &SharedRuntimeCommand, lost: &SharedDemandOffer) -> SharedAttemptCommand {
    let input = f.registry.prepare_shared_demand(verified(f, lost.clone()), c.clone(), None).await.unwrap().input().unwrap().clone();
    let mut failed = attempt(f, c).await;
    failed.start_intent_id = input.start.start_intent_id.clone();
    failed.predecessor = input.start.predecessor.clone();
    failed.budget_observation = input.start.budget_observation.clone();
    f.registry.fence_shared_successor(cleanup(f, c, 1).await, failed.clone()).await.unwrap();
    let SharedDemandRetry::Start(first) = f
        .registry
        .prepare_shared_demand_retry(verified(f, lost.clone()), c.clone(), Some(failed.clone()), 1)
        .await
        .unwrap()
    else {
        panic!("a retry start");
    };
    let start = first.input().start.clone();
    let mut retried = failed.clone();
    retried.command_id = "retried_runtime".into();
    retried.process_attempt = failed.process_attempt + 1;
    retried.predecessor = start.predecessor.clone();
    retried.budget_observation = start.budget_observation.clone();
    retried.start_intent_id = start.start_intent_id.clone();
    let now = graphs::now_ms().unwrap();
    retried.issued_at_ms = now;
    retried.expires_at_ms = now + 20_000;
    assert!(matches!(f.registry.claim_shared_successor(attempt_proof(f, retried.clone())).await.unwrap(), SharedAttemptClaim::Issued(_)));
    retried
}

#[tokio::test]
async fn shared_demand_v2_retry_binding_recovers_after_a_lost_reply() {
    let (f, c) = original(true).await;
    call(&f, &c, "retained_unknown", "unknown", 40, 7, false, false).await;
    let lost = offer(&c, "bound_retry", 1);
    let retried = claimed_retry(&f, &c, &lost).await;
    // The server bound R1's work, then the reply and the local bind were lost
    // and the process ended (its time-bound stop is the stop evidence here).
    f.registry.fence_shared_successor(cleanup(&f, &c, 1).await, retried.clone()).await.unwrap();
    let clock = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(retried.expires_at_ms + 30_000 + UNCERTAIN_GRACE_MS));
    let worker = clock.clone();
    f.registry.0.store.call(1024, move |_| { graphs::install_test_clock(&worker); Ok(()) }).await.unwrap();
    f.registry.record_shared_attempt_timeout(cleanup(&f, &c, 1).await, retried.clone()).await.unwrap().unwrap();
    let binding = SharedDemandBinding {
        version: 1,
        demand_id: lost.demand_id.clone(),
        demand_digest: digest(&lost),
        control_revision: lost.control_revision,
        assignment_id: lost.assignment_id.clone(),
        event_id: lost.event_id.clone(),
        received_at_ms: lost.received_at_ms,
        runtime_command_id: retried.command_id.clone(),
        runtime_command_digest: digest(&retried),
        work_id: "bound_retry_work".into(),
        work_digest: "f".repeat(64),
    };
    let device = c.reserve_command.executor_device_id.clone();
    assert!(recovery_walk(&f, &device, true).await.contains(&lost.demand_id));
    f.registry.recover_shared_demand_binding(cleanup(&f, &c, 1).await, lost.clone(), binding.clone()).await.unwrap();
    f.registry.recover_shared_demand_binding(cleanup(&f, &c, 1).await, lost.clone(), binding).await.unwrap();
    assert_eq!(count(&f, "owner_shared_demand_bindings").await, 1);
    assert!(!recovery_walk(&f, &device, true).await.contains(&lost.demand_id), "the event no longer holds the queue");
    drop(clock);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_demand_v2_blocked_retry_ends_locally_and_releases_the_queue() {
    let (f, c) = original(true).await;
    call(&f, &c, "retained_unknown", "unknown", 40, 7, false, false).await;
    let lost = offer(&c, "blocked_retry", 1);
    let input = f.registry.prepare_shared_demand(verified(&f, lost.clone()), c.clone(), None).await.unwrap().input().unwrap().clone();
    let mut failed = attempt(&f, &c).await;
    failed.start_intent_id = input.start.start_intent_id.clone();
    failed.predecessor = input.start.predecessor.clone();
    failed.budget_observation = input.start.budget_observation.clone();
    f.registry.fence_shared_successor(cleanup(&f, &c, 1).await, failed.clone()).await.unwrap();
    // Usage reported meanwhile leaves no budget for the retry.
    call(&f, &c, "late_overrun", "settled", 150, 201, true, false).await;
    let SharedDemandRetry::Blocked(budget) = f
        .registry
        .prepare_shared_demand_retry(verified(&f, lost.clone()), c.clone(), Some(failed.clone()), 1)
        .await
        .unwrap()
    else {
        panic!("a blocked retry");
    };
    assert_eq!(budget.state, "exhausted");
    assert_eq!(count(&f, "owner_shared_start_intents").await, 1, "no retry start was saved");
    let device = c.reserve_command.executor_device_id.clone();
    assert!(recovery_walk(&f, &device, true).await.contains(&lost.demand_id));
    // Once the server confirms the block, the event ends here too.
    f.registry
        .retain_shared_demand_terminal(cleanup(&f, &c, 1).await, lost.clone(), "budget_blocked".into(), Some(failed.command_id.clone()))
        .await
        .unwrap();
    assert!(!recovery_walk(&f, &device, true).await.contains(&lost.demand_id), "the queue moves on");
    assert!(f.registry.prepare_shared_demand_retry(verified(&f, lost.clone()), c.clone(), Some(failed), 1).await.is_err());
    f.registry.shutdown().await.unwrap();
}
