use super::*;
use crate::owner_resources::budget::*;
fn hash_value<T: Serialize>(v: &T) -> String {
    canonical::digest(&serde_json::to_value(v).unwrap()).unwrap()
}
fn delta(
    f: &Fixture,
    c: &ManagedAgentReserveCommand,
    r: &ManagedAgentReservationReceipt,
    id: &str,
    add: u64,
) -> AgentBudgetCommand {
    let now = graphs::now_ms().unwrap();
    AgentBudgetCommand { owner_funded: false,
        version: 1,
        capability: AGENT_BUDGET_CAPABILITY.into(),
        operation: "agent.budget.increase".into(),
        command_id: format!("budget_{id}"),
        top_up_id: format!("topup_{id}"),
        top_up_request_digest: "a".repeat(64),
        approval_command_id: format!("approve_{id}"),
        approval_digest: "b".repeat(64),
        approving_administrator_mxid: "@assigned_admin:a.test".into(),
        authority_revision: 1,
        assignment_id: "central_assignment".into(),
        assignment_generation: 1,
        reserve_command: c.clone(),
        reserve_command_digest: hash_value(c),
        reserve_receipt: r.clone(),
        reserve_receipt_digest: hash_value(r),
        expected_budget_revision: 1,
        expected_budget_digest: initial_budget_digest(r).unwrap(),
        expected_ceiling_tokens: r.allocated_tokens,
        add_tokens: add,
        issued_at_ms: now.saturating_sub(1),
        expires_at_ms: c.expires_at_ms.min(f.grant.delegation_expires_at_ms),
    }
}
fn proof(
    f: &Fixture,
    c: AgentBudgetCommand,
    cancel: bool,
    generation: u64,
) -> VerifiedAgentBudgetDelivery {
    VerifiedAgentBudgetDelivery::new(
        enrollment_from(
            &f.profile,
            &f.resource,
            &f.grant.resource_revision,
            generation,
        ),
        c.clone(),
        hash_value(&c),
        !cancel,
        cancel,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
async fn reserve_qualified(
    f: &Fixture,
    id: &str,
    tokens: u64,
) -> (ManagedAgentReserveCommand, ManagedAgentReservationReceipt) {
    let c = agent(f, id, tokens);
    let r = reserve(f, c.clone()).await;
    qualify(f, r.agent_reservation_id.as_ref().unwrap()).await;
    (c, r)
}
async fn snapshot(f: &Fixture, child: &str) -> AgentBudgetSnapshot {
    f.registry
        .managed_agent_budget(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            child.into(),
        )
        .await
        .unwrap()
}
#[test]
fn managed_budget_canonical_contract_is_closed_and_hash_chain_has_no_receipt_cycle() {
    let v: serde_json::Value = serde_json::from_str(include_str!(
        "capacity/managed_agents/custody/budget/management-agent-budget-v1.json"
    ))
    .unwrap();
    let c: AgentBudgetCommand = serde_json::from_value(v["command"].clone()).unwrap();
    c.validate("@owner:example.test").unwrap();
    let r: AgentBudgetReceipt = serde_json::from_value(v["receipt"].clone()).unwrap();
    r.validate_for(&c).unwrap();
    assert_eq!(hash_value(&c), v["commandDigest"]);
    assert_eq!(hash_value(&r), v["receiptDigest"]);
    assert_eq!(
        initial_budget_digest(&c.reserve_receipt).unwrap(),
        v["genesisDigest"]
    );
    assert_eq!(
        next_budget_digest(&c, r.ledger_revision).unwrap(),
        v["nextHeadDigest"]
    );
    let mut bad = v["command"].clone();
    bad["ownerApproved"] = true.into();
    assert!(serde_json::from_value::<AgentBudgetCommand>(bad).is_err());
    let mut bad = c.clone();
    bad.add_tokens = JSON_SAFE_MAX;
    assert!(bad.validate("@owner:example.test").is_err());
    let mut bad = c.clone();
    bad.expires_at_ms += 1;
    assert!(bad.validate("@owner:example.test").is_err());
    let mut bad = r.clone();
    bad.budget_digest = hash_value(&r);
    assert!(bad.validate_for(&c).is_err());
}
#[tokio::test]
async fn managed_budget_apply_replay_later_head_and_restart_preserve_original_bytes() {
    let mut f = fixture().await;
    let (c, r) = reserve_qualified(&f, "budget", 150).await;
    let child = r.agent_reservation_id.as_ref().unwrap();
    let first = delta(&f, &c, &r, "first", 50);
    let (left, right) = tokio::join!(
        f.registry
            .apply_managed_budget(proof(&f, first.clone(), false, 1)),
        f.registry
            .apply_managed_budget(proof(&f, first.clone(), false, 1))
    );
    let a = left.unwrap();
    assert_eq!(a.state, "applied");
    assert_eq!(hash_value(&a), hash_value(&right.unwrap()));
    let mut second = delta(&f, &c, &r, "second", 25);
    second.expected_budget_revision = a.budget_revision;
    second.expected_budget_digest = a.budget_digest.clone();
    second.expected_ceiling_tokens = a.effective_ceiling_tokens;
    let b = f
        .registry
        .apply_managed_budget(proof(&f, second, false, 1))
        .await
        .unwrap();
    assert_eq!(b.effective_ceiling_tokens, 225);
    assert_eq!(
        hash_value(
            &f.registry
                .apply_managed_budget(proof(&f, first.clone(), true, 2))
                .await
                .unwrap()
        ),
        hash_value(&a)
    );
    let h = snapshot(&f, child).await;
    assert_eq!(
        (
            h.original_ceiling_tokens,
            h.effective_ceiling_tokens,
            h.budget_revision
        ),
        (150, 225, 3)
    );
    assert!(!h.execution_supported);
    let stored = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            c.command_id.clone(),
            hash_value(&c),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hash_value(&stored), hash_value(&r));
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    let recovered = f
        .registry
        .recover_managed_budget(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2),
            first.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(hash_value(&recovered), hash_value(&a));
    let mut changed = first;
    changed.add_tokens += 1;
    assert!(matches!(
        f.registry
            .apply_managed_budget(proof(&f, changed, false, 1))
            .await,
        Err(Error::Conflict)
    ));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_budget_cancellation_race_is_first_disposition_without_refund() {
    let f = fixture().await;
    let (c, r) = reserve_qualified(&f, "cancel", 150).await;
    let cmd = delta(&f, &c, &r, "race", 50);
    let (a, b) = tokio::join!(
        f.registry
            .apply_managed_budget(proof(&f, cmd.clone(), true, 1)),
        f.registry
            .apply_managed_budget(proof(&f, cmd.clone(), false, 1))
    );
    let a = a.unwrap();
    assert_eq!(hash_value(&a), hash_value(&b.unwrap()));
    assert!(matches!(
        a.state.as_str(),
        "applied" | "cancelled_before_apply"
    ));
    let h = snapshot(&f, c_child(&r)).await;
    assert_eq!(h.effective_ceiling_tokens, 150 + a.applied_delta_tokens);
    let mut other = cmd.clone();
    other.command_id = "replacement_cancel".into();
    assert!(matches!(
        f.registry
            .apply_managed_budget(proof(&f, other, true, 1))
            .await,
        Err(Error::Conflict)
    ));
    f.registry.shutdown().await.unwrap();
}
fn c_child(r: &ManagedAgentReservationReceipt) -> &str {
    r.agent_reservation_id.as_deref().unwrap()
}
#[tokio::test]
async fn managed_budget_aggregate_poststate_counts_siblings_and_refusal_rolls_back_delta() {
    let f = fixture().await;
    let c = agent(&f, "one", 150);
    let r = reserve(&f, c.clone()).await;
    let other = reserve(&f, agent(&f, "two", 150)).await;
    qualify(&f, c_child(&r)).await;
    assert_eq!(other.state, "reserved");
    let a = f
        .registry
        .apply_managed_budget(proof(&f, delta(&f, &c, &r, "fit", 100), false, 1))
        .await
        .unwrap();
    assert_eq!(a.state, "applied");
    let mut no = delta(&f, &c, &r, "overflow", 1);
    no.expected_budget_revision = a.budget_revision;
    no.expected_budget_digest = a.budget_digest;
    no.expected_ceiling_tokens = 250;
    let refusal = f
        .registry
        .apply_managed_budget(proof(&f, no, false, 1))
        .await
        .unwrap();
    assert_eq!(refusal.state, "refused");
    assert_eq!(refusal.reason.as_deref(), Some("insufficient_capacity"));
    let h = snapshot(&f, c_child(&r)).await;
    assert_eq!((h.effective_ceiling_tokens, h.budget_revision), (250, 2));
    let child = c_child(&r).to_owned();
    let count = f
        .registry
        .0
        .store
        .call(1024, move |db| {
            Ok(db.db.query_row(
                "SELECT COUNT(*) FROM owner_managed_budget_deltas WHERE child_id=?1",
                [child],
                |r| r.get::<_, u64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(count, 1);
    f.registry.shutdown().await.unwrap();
}
async fn large_fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
    let resource = registry
        .prepare_enrollment(
            profile.clone(),
            "large_enroll".into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            20000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id;
    let grant = grant(&registry, &profile, &resource, "large_parent", 15000)
        .await
        .unwrap();
    let project = command(&grant, "large_project", 10000);
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
#[tokio::test]
async fn managed_budget_717_overrun_is_charged_before_added_headroom_and_old_calls_keep_original_cap()
 {
    let f = large_fixture().await;
    let (c, r) = reserve_qualified(&f, "overrun", 4096).await;
    let child = c_child(&r);
    let call = issue(&f, child, "original_call", 4096).await;
    f.registry
        .record_managed_usage(&call, reported("complete", 4813, true, true))
        .await
        .unwrap();
    let before = snapshot(&f, child).await;
    assert_eq!((before.retained_tokens, before.remaining_tokens), (4813, 0));
    let a = f
        .registry
        .apply_managed_budget(proof(&f, delta(&f, &c, &r, "cover", 1024), false, 1))
        .await
        .unwrap();
    assert_eq!(a.state, "applied");
    let after = snapshot(&f, child).await;
    assert_eq!(
        (
            after.original_ceiling_tokens,
            after.effective_ceiling_tokens,
            after.known_consumed_tokens,
            after.retained_tokens,
            after.remaining_tokens
        ),
        (4096, 5120, 4813, 4813, 307)
    );
    assert!(after.usage_complete);
    assert!(!after.execution_supported);
    assert!(matches!(
        f.registry
            .prepare_managed_call(super::call(
                &f,
                child,
                "old_version_must_not_spend_delta",
                1
            ))
            .await,
        Err(Error::Capacity)
    ));
    let too_large = reserve(&f, agent(&f, "sibling_overcommits", 4881)).await;
    assert_eq!(too_large.state, "refused");
    let exact = reserve(&f, agent(&f, "sibling_fits_poststate", 4880)).await;
    assert_eq!(exact.state, "reserved");
    assert_eq!(5120 - before.retained_tokens, 307);
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_budget_stale_head_generation_and_unknown_usage_never_rebind() {
    let f = fixture().await;
    let (c, r) = reserve_qualified(&f, "unknown", 150).await;
    let child = c_child(&r);
    let call = issue(&f, child, "old_unknown", 150).await;
    f.registry
        .record_managed_usage(&call, reported("partial", 40, false, false))
        .await
        .unwrap();
    let a = f
        .registry
        .apply_managed_budget(proof(&f, delta(&f, &c, &r, "bounded", 50), false, 1))
        .await
        .unwrap();
    assert_eq!(a.state, "applied");
    let h = snapshot(&f, child).await;
    assert_eq!(
        (
            h.known_consumed_tokens,
            h.retained_tokens,
            h.remaining_tokens
        ),
        (40, 40, 160)
    );
    assert!(!h.usage_complete);
    let stale = f
        .registry
        .apply_managed_budget(proof(&f, delta(&f, &c, &r, "stale", 10), false, 1))
        .await
        .unwrap();
    assert_eq!(stale.reason.as_deref(), Some("budget_changed"));
    let mut newer = delta(&f, &c, &r, "new_device_generation", 10);
    newer.expected_budget_revision = a.budget_revision;
    newer.expected_budget_digest = a.budget_digest;
    newer.expected_ceiling_tokens = a.effective_ceiling_tokens;
    let denied = f
        .registry
        .apply_managed_budget(proof(&f, newer, false, 2))
        .await
        .unwrap();
    assert_eq!(denied.reason.as_deref(), Some("authority_changed"));
    assert_eq!(snapshot(&f, child).await.effective_ceiling_tokens, 200);
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_budget_queued_retirement_and_atomic_failure_leave_no_credit() {
    let f = fixture().await;
    let (c, r) = reserve_qualified(&f, "queued", 150).await;
    let command = delta(&f, &c, &r, "queued", 50);
    let live = Arc::new(AtomicBool::new(true));
    let gate = live.clone();
    let scoped = f
        .profile
        .clone()
        .with_native_gate(move || gate.load(Ordering::Acquire))
        .unwrap();
    let delivery = VerifiedAgentBudgetDelivery::new(
        enrollment_from(&scoped, &f.resource, &f.grant.resource_revision, 1),
        command.clone(),
        hash_value(&command),
        true,
        false,
        Instant::now() + Duration::from_secs(20),
    )
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
            .unwrap()
    });
    ready.await.unwrap();
    let mut pending = Box::pin(f.registry.apply_managed_budget(delivery));
    tokio::select! {biased;r=&mut pending=>panic!("writer bypass {r:?}"),_=tokio::task::yield_now()=>{}}
    live.store(false, Ordering::Release);
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER fail_budget_delta BEFORE INSERT ON owner_managed_budget_deltas BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .apply_managed_budget(proof(&f, command.clone(), false, 1))
            .await
            .is_err()
    );
    assert!(
        f.registry
            .recover_managed_budget(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                command
            )
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        snapshot(&f, c_child(&r)).await.effective_ceiling_tokens,
        150
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_budget_migration_88_to_89_rolls_back_and_reopens_without_credit() {
    let mut f = fixture().await;
    let id = f.registry.id().to_owned();
    let root = f._temp.path().join("owner");
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE owner_managed_budget_deltas;DROP TABLE owner_managed_budget_commands;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=88;CREATE TABLE owner_managed_budget_deltas(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        88
    );
    assert!(
        !db.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='owner_managed_budget_commands')",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap()
    );
    db.execute_batch("DROP TABLE owner_managed_budget_deltas;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(f.registry.id(), id);
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db
                    .pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))?,
                u64::try_from(crate::DOMAIN_SCHEMA_VERSION).unwrap()
            );
            assert_eq!(
                db.db.query_row(
                    "SELECT count(*) FROM owner_managed_budget_commands",
                    [],
                    |r| r.get::<_, u64>(0)
                )?,
                0
            );
            assert_eq!(
                db.db.query_row(
                    "SELECT count(*) FROM owner_managed_budget_deltas",
                    [],
                    |r| r.get::<_, u64>(0)
                )?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_budget_fenced_and_unrepresentable_children_refuse_without_refunding() {
    let f = fixture().await;
    let (c, r) = reserve_qualified(&f, "unrepresentable", 150).await;
    let call = issue(&f, c_child(&r), "overflow_call", 150).await;
    f.registry
        .record_managed_usage(
            &call,
            ManagedUsageObservation::fixture("unrepresentable", None, true, false, true),
        )
        .await
        .unwrap();
    let refused = f
        .registry
        .apply_managed_budget(proof(
            &f,
            delta(&f, &c, &r, "no_overflow_credit", 50),
            false,
            1,
        ))
        .await
        .unwrap();
    assert_eq!(refused.state, "refused");
    assert_eq!(refused.applied_delta_tokens, 0);
    assert!(
        f.registry
            .managed_agent_budget(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                c_child(&r).into()
            )
            .await
            .is_err()
    );
    f.registry
        .fence_managed_agent(clean(&f, c_child(&r), 1), "budget_fence".into())
        .await
        .unwrap();
    let later = f
        .registry
        .apply_managed_budget(proof(&f, delta(&f, &c, &r, "fenced_credit", 50), false, 1))
        .await
        .unwrap();
    assert_eq!(later.state, "refused");
    assert_eq!(later.applied_delta_tokens, 0);
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db.query_row(
                    "SELECT count(*) FROM owner_managed_budget_deltas",
                    [],
                    |r| r.get::<_, u64>(0)
                )?,
                0
            );
            assert!(db.db.query_row(
                "SELECT unrepresentable_usage FROM owner_managed_calls",
                [],
                |r| r.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_budget_chain_is_append_only_and_corrupt_node_cannot_revert_to_original_ceiling() {
    let f = fixture().await;
    let (c, r) = reserve_qualified(&f, "immutable", 150).await;
    let a = f
        .registry
        .apply_managed_budget(proof(&f, delta(&f, &c, &r, "immutable", 50), false, 1))
        .await
        .unwrap();
    assert_eq!(a.state, "applied");
    f.registry.0.store.call(1,|db|{
        assert!(db.db.execute("UPDATE owner_managed_budget_deltas SET effective_ceiling_tokens=151",[]).is_err());
        assert!(db.db.execute("DELETE FROM owner_managed_budget_commands",[]).is_err());
        // Deliberate on-disk corruption fixture, outside all supported writers.
        let mut q=db.db.prepare("SELECT name FROM sqlite_schema WHERE type='trigger' AND tbl_name='owner_managed_budget_deltas'")?;
        let names=q.query_map([],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
        drop(q);
        for name in names {db.db.execute_batch(&format!("DROP TRIGGER \"{}\"",name.replace('"',"\"\"")))?;}
        db.db.execute("DELETE FROM owner_managed_budget_deltas",[])?;
        Ok(())
    }).await.unwrap();
    assert!(
        f.registry
            .managed_agent_budget(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                c_child(&r).into()
            )
            .await
            .is_err()
    );
    // Fresh sibling reserve cannot treat the missing applied node as zero credit.
    assert!(
        f.registry
            .reserve_managed_agent(delivery(
                &f,
                agent(&f, "corrupt_sibling", 1),
                f.project.clone()
            ))
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
}

#[cfg(feature = "shared-budget-execution")]
#[path = "budget_execution_head_tests.rs"]
mod execution_head;

#[tokio::test]
async fn managed_settlement_uses_applied_topup_head_and_keeps_unknown_usage_held() {
    let f = fixture().await;
    let (c, r) = reserve_qualified(&f, "settlement_budget", 150).await;
    let child = c_child(&r);
    let call = issue(&f, child, "settlement_call", 100).await;
    f.registry
        .record_managed_usage(&call, reported("incomplete_usage", 40, true, false))
        .await
        .unwrap();
    let increase = f
        .registry
        .apply_managed_budget(proof(
            &f,
            delta(&f, &c, &r, "settlement_topup", 50),
            false,
            1,
        ))
        .await
        .unwrap();
    let fc = fences::command(&f, c.clone(), "settlement_budget_fence");
    let fr = fences::apply_fence(&f, fc.clone()).await;
    let sc = settlement_tests::command(&fc, &fr, "settlement_budget_pending");
    let first = settlement_tests::settle(&f, sc).await;
    assert!(!first.usage_complete);
    assert_eq!(
        (first.retained_tokens, first.known_consumed_tokens),
        (200, 40)
    );
    assert_eq!(first.budget_revision, Some(increase.budget_revision));
    assert_eq!(
        first.budget_digest.as_deref(),
        Some(increase.budget_digest.as_str())
    );
    assert_eq!(first.effective_ceiling_tokens, Some(200));
    f.registry
        .record_late_managed_usage(
            clean(&f, child, 1),
            call.call_id().into(),
            reported("complete_usage", 90, true, true),
        )
        .await
        .unwrap();
    let done = settlement_tests::settle(
        &f,
        settlement_tests::command(&fc, &fr, "settlement_budget_complete"),
    )
    .await;
    assert!(done.usage_complete);
    assert_eq!((done.retained_tokens, done.known_consumed_tokens), (90, 90));
    assert_eq!(done.budget_digest, first.budget_digest);
    assert_eq!(hash_value(&reserve(&f, c).await), hash_value(&r));
}
