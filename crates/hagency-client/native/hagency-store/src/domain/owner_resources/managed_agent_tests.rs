use super::*;
#[path = "pool_agent_tests.rs"]
mod pools;
#[path = "managed_custody_tests.rs"]
mod custody;
#[path = "managed_fence_tests.rs"]
mod fences;
#[path = "managed_matrix_tests.rs"]
mod matrix;
#[path = "managed_runtime_tests.rs"]
mod runtime;
#[path = "shared_execution_tests.rs"]
mod shared_text;

struct Fixture {
    _temp: tempfile::TempDir,
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    resource: String,
    grant: NativeGrantReceipt,
    project: AllocationCommand,
    receipt: NativeAllocationReceipt,
}
async fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
    let resource = enroll(&registry, &profile, "enroll").await.resource_id;
    let grant = grant(&registry, &profile, &resource, "parent", 700)
        .await
        .unwrap();
    let project = command(&grant, "project", 400);
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
fn agent(f: &Fixture, id: &str, tokens: u64) -> ManagedAgentReserveCommand {
    let now = graphs::now_ms().unwrap();
    let profile = hagency_core::qualification::ModelProfile {
        framework: "codex".into(),
        model: "gpt-5.6-sol".into(),
        provider: None,
        reasoning: Some("medium".into()),
    };
    let role = hagency_core::qualification::roles()
        .find(|r| hagency_core::qualification::qualifies(&profile, r, None))
        .unwrap();
    let definition = ManagedAgentDefinition {
        name: "Native helper".into(),
        capability: role.into(),
        description: "Text-only test".into(),
        tool_mode: "none".into(),
    };
    ManagedAgentReserveCommand {
        renewal_mode: None,
        allocation_generation: None,
        version: 1,
        pool_grant: None,
        capability: AGENT_CAPABILITY.into(),
        operation: "agent.reserve".into(),
        command_id: format!("reserve_agent_{id}"),
        request_id: format!("request_agent_{id}"),
        request_revision: 2,
        proposed_agent_id: format!("agent_{id}"),
        manager_mxid: f.project.manager_mxid.clone(),
        project_id: f.project.project_id.clone(),
        project_revision: f.project.project_revision,
        setup_id: "setup".into(),
        setup_binding_revision: 1,
        domain_project_id: "domain_project".into(),
        room_id: "!discussion:a.test".into(),
        authority_revision: 1,
        decision_actor_mxid: "@assigned_admin:a.test".into(),
        decision_digest: "d".repeat(64),
        allocation_id: f.project.allocation_id.clone(),
        allocation_receipt_digest: canonical::digest(&serde_json::to_value(&f.receipt).unwrap())
            .unwrap(),
        grant_receipt_digest: canonical::digest(&serde_json::to_value(&f.grant).unwrap()).unwrap(),
        enrollment_id: f.grant.enrollment_id.clone(),
        executor_device_id: f.grant.executor_device_id.clone(),
        executor_generation: f.grant.executor_generation,
        resource_id: f.resource.clone(),
        resource_revision: f.grant.resource_revision.clone(),
        definition_digest: canonical::digest(&serde_json::to_value(&definition).unwrap()).unwrap(),
        definition,
        requested_tokens: tokens,
        period_key: f.grant.period_key.clone(),
        issued_at_ms: now.saturating_sub(1),
        expires_at_ms: now + 60_000,
    }
}
fn delivery(
    f: &Fixture,
    c: ManagedAgentReserveCommand,
    original: AllocationCommand,
) -> VerifiedManagedAgentDelivery {
    let e = enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1);
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    VerifiedManagedAgentDelivery::new(
        e,
        c,
        digest,
        original,
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
async fn reserve(f: &Fixture, c: ManagedAgentReserveCommand) -> ManagedAgentReservationReceipt {
    f.registry
        .reserve_managed_agent(delivery(f, c, f.project.clone()))
        .await
        .unwrap()
}
async fn count(f: &Fixture) -> u64 {
    f.registry
        .0
        .store
        .call(1024, |db| {
            Ok(db
                .db
                .query_row("SELECT COUNT(*) FROM owner_managed_agents", [], |r| {
                    r.get(0)
                })?)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn web_workforce_join_uses_immutable_child_and_profile_not_display_name() {
    let f = fixture().await;
    let original = agent(&f,"web_read",100);
    reserve(&f,original.clone()).await;
    let profile = f.profile.digest.clone();
    let resource = f.resource.clone();
    f.registry.0.store.call(16*1024, move |db| {
        let before = db.db.total_changes();
        let row = db.management_web_managed_link(&profile,&original.request_id)?.unwrap();
        assert_eq!(row.agent_id,original.proposed_agent_id);
        assert_eq!(row.project_id,original.project_id);
        assert_eq!(row.allocation_id,original.allocation_id);
        assert_eq!(row.resource_id,resource);
        assert_eq!(row.allocated_tokens,100);
        assert_eq!(row.known_consumed_tokens,0);
        assert_eq!(row.process_state,"not_started");
        assert!(row.model_revision_matches);
        assert!(row.account_id.as_ref().is_some_and(|s|s.starts_with("binding_")));
        assert!(db.management_web_managed_link(&"b".repeat(64),&original.request_id)?.is_none());
        assert!(db.management_web_managed_link(&profile,&original.definition.name)?.is_none());
        let usage = db.management_web_managed_usage(&profile)?;
        assert!(usage.complete);
        assert_eq!(usage.items.len(),1);
        assert_eq!(usage.items[0].project_id,original.project_id);
        assert_eq!(usage.items[0].known_consumed_tokens,row.known_consumed_tokens);
        assert_eq!(usage.items[0].retained_tokens,row.retained_tokens);
        assert_eq!(usage.items[0].call_count,0);
        assert!(db.management_web_managed_usage(&"b".repeat(64))?.items.is_empty());
        assert_eq!(db.management_web_managed_usage_page(&profile,"",1)?.items.len(),1);
        assert!(db.management_web_managed_usage_page(&profile,&original.request_id,1)?.items.is_empty());
        assert_eq!(db.db.total_changes(),before,"read projection must not mutate custody");
        db.db.execute("UPDATE owner_resource_grants SET active=0 WHERE resource_id=?1 AND profile=?2",params![resource,profile])?;
        assert!(db.management_web_managed_link(&profile,&original.request_id)?.is_none());
        assert!(db.management_web_managed_usage(&profile)?.items.is_empty());
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}
fn release(f: &Fixture, id: &str) -> AllocationCommand {
    let mut c = f.project.clone();
    c.command_id = id.into();
    c.operation = "project.release".into();
    c.reserve_command_id = Some(f.project.command_id.clone());
    c
}
async fn parent_close(
    f: &Fixture,
    id: &str,
    previous: Option<&NativeGrantSettlementReceipt>,
) -> NativeGrantSettlementReceipt {
    let c = GrantClosureCommand {
        version: 1,
        command_id: id.into(),
        operation: if previous.is_some() {
            "grant.settle"
        } else {
            "grant.close"
        }
        .into(),
        grant_id: f.grant.grant_id.clone(),
        grant_receipt_digest: canonical::digest(&serde_json::to_value(&f.grant).unwrap()).unwrap(),
        previous_settlement_digest: previous
            .map(|v| canonical::digest(&serde_json::to_value(v).unwrap()).unwrap()),
        reason: if previous.is_some() {
            "settlement_progress"
        } else {
            "owner_withdrawal"
        }
        .into(),
        requested_at_ms: graphs::now_ms().unwrap(),
    };
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    let d = VerifiedParentDelivery::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        c,
        digest,
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    f.registry.apply_parent_settlement(d).await.unwrap()
}

#[test]
fn managed_agent_wire_matches_central_canonical_fixture_and_closed_fields() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "capacity/managed_agents/management-agent-admission-v1.json"
    ))
    .unwrap();
    let c: ManagedAgentReserveCommand = serde_json::from_value(fixture["command"].clone()).unwrap();
    c.validate("@owner:example.test").unwrap();
    let r: ManagedAgentReservationReceipt =
        serde_json::from_value(fixture["receipt"].clone()).unwrap();
    assert_eq!(
        canonical::digest(&serde_json::to_value(c).unwrap()).unwrap(),
        fixture["commandDigest"]
    );
    assert_eq!(
        canonical::digest(&serde_json::to_value(r).unwrap()).unwrap(),
        fixture["receiptDigest"]
    );
    let mut bad = fixture["command"].clone();
    bad["coordinatorApproved"] = json!(true);
    assert!(serde_json::from_value::<ManagedAgentReserveCommand>(bad).is_err());
    let mut bad = fixture["command"].clone();
    bad["requestedTokens"] = json!(JSON_SAFE_MAX + 1);
    assert!(
        serde_json::from_value::<ManagedAgentReserveCommand>(bad)
            .unwrap()
            .validate("@owner:example.test")
            .is_err()
    );
}

#[tokio::test]
async fn managed_agent_exact_reserve_replay_semantic_uniqueness_and_parent_count_once() {
    let f = fixture().await;
    let mut c = agent(&f, "first", 150);
    // The assigned admin may be the manager; coordinator policy is unrelated.
    c.decision_actor_mxid = c.manager_mxid.clone();
    let r = reserve(&f, c.clone()).await;
    assert_eq!(r.state, "reserved");
    assert_eq!(r.retained_tokens, 150);
    assert_eq!(r.budget_revision, 1);
    let replay = reserve(&f, c.clone()).await;
    assert_eq!(
        serde_json::to_value(&r).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    let recovered = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            c.command_id.clone(),
            digest,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.ledger_revision, r.ledger_revision);
    let mut changed = c.clone();
    changed.requested_tokens += 1;
    assert!(matches!(
        f.registry
            .reserve_managed_agent(delivery(&f, changed, f.project.clone()))
            .await,
        Err(Error::Conflict)
    ));
    c.command_id = "semantic_duplicate".into();
    assert_eq!(
        reserve(&f, c).await.reason.as_deref(),
        Some("request_conflict")
    );
    assert_eq!(count(&f).await, 1);
    let remaining = f
        .registry
        .resource(&f.profile, f.resource.clone())
        .await
        .unwrap()
        .budget
        .pool
        .remaining
        .map(u64::from);
    assert_eq!(remaining, Some(300));
    let (a, b) = tokio::join!(
        reserve(&f, agent(&f, "concurrent_a", 200)),
        reserve(&f, agent(&f, "concurrent_b", 200))
    );
    assert_ne!(a.state, b.state);
    assert_eq!(count(&f).await, 2);
    assert!(f.registry.0.store.call(1024,|db|Ok(db.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM engagements) AND NOT EXISTS(SELECT 1 FROM registrations)",[],|r|r.get::<_,bool>(0))?)).await.unwrap());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_agent_wrong_bindings_expiry_and_native_evidence_never_hold_capacity() {
    let f = fixture().await;
    for case in 0..8 {
        let mut c = agent(&f, &format!("bad{case}"), 100);
        match case {
            0 => c.manager_mxid = "@other:a.test".into(),
            1 => c.resource_revision = "f".repeat(64),
            2 => c.executor_generation += 1,
            3 => c.allocation_receipt_digest = "e".repeat(64),
            4 => c.grant_receipt_digest = "e".repeat(64),
            5 => c.executor_device_id = "another_device".into(),
            6 => {
                c.expires_at_ms = graphs::now_ms().unwrap() - 1000;
                c.issued_at_ms = c.expires_at_ms - 1000;
            }
            _ => c.enrollment_id = "another_enrollment".into(),
        }
        assert_eq!(reserve(&f, c).await.state, "refused");
    }
    let mut original = f.project.clone();
    original.manager_mxid = "@other:a.test".into();
    let mut c = agent(&f, "wrong_original", 100);
    c.manager_mxid = original.manager_mxid.clone();
    assert_eq!(
        f.registry
            .reserve_managed_agent(delivery(&f, c, original))
            .await
            .unwrap()
            .state,
        "refused"
    );
    assert_eq!(count(&f).await, 0);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_mode_keeps_sibling_allocations_and_cleanup_safe() {
    let f = fixture().await;
    reserve(&f, agent(&f, "first", 150)).await;
    let sibling = command(&f.grant, "sibling", 200);
    assert_eq!(
        apply(&f.registry, &f.profile, sibling.clone()).await.state,
        "reserved"
    );
    let mut sibling_release = sibling.clone();
    sibling_release.command_id = "release_sibling".into();
    sibling_release.operation = "project.release".into();
    sibling_release.reserve_command_id = Some(sibling.command_id);
    assert_eq!(
        apply(&f.registry, &f.profile, sibling_release)
            .await
            .retained_tokens,
        0
    );
    let closed = parent_close(&f, "close_parent", None).await;
    assert_eq!(closed.retained_tokens, 400);
    assert_eq!(reserve(&f, agent(&f, "late", 50)).await.state, "refused");
    let released = apply(&f.registry, &f.profile, release(&f, "release_managed")).await;
    assert_eq!(released.state, "released");
    assert_eq!(released.retained_tokens, 0);
    let settled = parent_close(&f, "settle_parent", Some(&closed)).await;
    assert_eq!(settled.retained_tokens, 0);
    assert_eq!(
        f.registry
            .resource(&f.profile, f.resource.clone())
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(1000)
    );
    assert!(
        f.registry
            .0
            .store
            .call(1024, |db| Ok(db.db.query_row(
                "SELECT state='fenced' AND permanent_fence=1 FROM owner_managed_agents",
                [],
                |r| r.get::<_, bool>(0)
            )?))
            .await
            .unwrap()
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn derived_unknown_and_late_overrun_block_new_capacity_and_survive_old_settlement() {
    let f = fixture().await;
    let child = reserve(&f, agent(&f, "first", 150))
        .await
        .agent_reservation_id
        .unwrap();
    let own = child.clone();
    // Seed native evidence only to test the collector. This is not a runtime
    // adapter test; no production call admission API is connected yet.
    f.registry.0.store.call(1024,move|db|{
        db.db.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES('call',?1,'execution',?2,1,'unknown',120,0,0)",params![own,"a".repeat(64)])?;Ok(())
    }).await.unwrap();
    assert!(
        f.registry
            .resource(&f.profile, f.resource.clone())
            .await
            .unwrap()
            .budget
            .remaining_tokens
            .is_none()
    );
    assert!(
        grant(&f.registry, &f.profile, &f.resource, "blocked", 10)
            .await
            .is_err()
    );
    assert_eq!(
        reserve(&f, agent(&f, "uncertain", 10))
            .await
            .reason
            .as_deref(),
        Some("unsettled_usage")
    );
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "release_unknown"))
            .await
            .reason
            .as_deref(),
        Some("unsettled_usage")
    );
    f.registry.0.store.call(1024,|db|{db.db.execute("UPDATE owner_managed_calls SET state='settled',known_consumed_tokens=100,usage_complete=1 WHERE id='call'",[])?;Ok(())}).await.unwrap();
    let released = apply(&f.registry, &f.profile, release(&f, "release_known")).await;
    assert_eq!(released.retained_tokens, 100);
    let closed = parent_close(&f, "close_known", None).await;
    assert_eq!(closed.known_consumed_tokens, 100);
    assert_eq!(closed.retained_tokens, 100);
    f.registry
        .0
        .store
        .call(1024, |db| {
            db.db.execute(
                "UPDATE owner_managed_calls SET known_consumed_tokens=900 WHERE id='call'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .resource(&f.profile, f.resource.clone())
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(100)
    );
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "release_overrun"))
            .await
            .reason
            .as_deref(),
        Some("unsettled_usage")
    );
    let late = parent_close(&f, "settle_overrun", Some(&closed)).await;
    assert_eq!(late.retained_tokens, 900);
    assert_eq!(late.known_consumed_tokens, 900);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn automatic_account_quota_does_not_couple_pools_or_erase_unknown_usage() {
    let f = fixture().await;
    let original = f.registry.resource(&f.profile, f.resource.clone()).await.unwrap();
    let child = reserve(&f, agent(&f, "first", 150)).await.agent_reservation_id.unwrap();
    let resource_id = f.resource.clone();
    f.registry.0.store.call(4096, move |db| {
        let resource = read_resource(&db.db, &resource_id)?;
        // Reproduce the old enrollment's inferred provider-wide quota.
        db.db.execute("UPDATE seats SET config=json_set(config,'$.declaration',json(?2)) WHERE id=?1",
            params![resource.seat_id, r#"{"quotaTokens":1000,"period":"monthly"}"#])?;
        db.db.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES('unsettled_original',?1,'execution',?2,1,'unknown',120,30,0)", params![child,"a".repeat(64)])?;
        Ok(())
    }).await.unwrap();
    let other = f.registry.prepare_enrollment(f.profile.clone(), "terra_pool".into(), provider(),
        "gpt-6.1-terra".into(), Some("high".into()), 2000, Instant::now()+Duration::from_secs(30))
        .unwrap().approve().await.unwrap();
    let pool = f.registry.resource(&f.profile, other.resource_id.clone()).await.unwrap();
    assert_eq!(pool.budget.seat.quota, None);
    assert_eq!(pool.budget.remaining_tokens.map(u64::from), Some(2000));
    let published = grant(&f.registry, &f.profile, &other.resource_id, "terra_capacity", 1500).await.unwrap();
    assert_eq!(published.allocated_tokens, 1500);
    assert!(grant(&f.registry, &f.profile, &other.resource_id, "above_pool_maximum", 600).await.is_err());
    let unchanged = f.registry.resource(&f.profile, f.resource.clone()).await.unwrap();
    assert_eq!(unchanged.revision, original.revision);
    assert!(unchanged.budget.remaining_tokens.is_none(), "this pool's own unknown usage still blocks it");
    let saved = f.registry.recover_capacity_grant(f.profile.clone(), f.grant.executor_device_id.clone(), f.grant.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(serde_json::to_value(saved).unwrap(), serde_json::to_value(&f.grant).unwrap());
    f.registry.0.store.call(4096, |db| {
        assert!(db.db.query_row("SELECT state='unknown' AND reserved_tokens=120 AND known_consumed_tokens=30 AND usage_complete=0 FROM owner_managed_calls WHERE id='unsettled_original'", [], |r| r.get::<_,bool>(0))?);
        assert!(db.db.query_row("SELECT EXISTS(SELECT 1 FROM seats WHERE json_extract(config,'$.declaration.quotaTokens')=1000)", [], |r| r.get::<_,bool>(0))?, "original stored declaration is not rewritten");
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

pub(super) fn remove_schema(db: &Connection) {
    db.execute_batch("DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE IF EXISTS owner_managed_runtime_observations;DROP TABLE IF EXISTS owner_managed_runtime_stops;DROP TABLE IF EXISTS owner_managed_runtime_lifetimes;DROP TABLE IF EXISTS owner_resource_adoptions;DROP TABLE IF EXISTS owner_managed_fences;DROP TABLE IF EXISTS owner_managed_observations;DROP TABLE IF EXISTS owner_managed_call_pins;DROP TABLE IF EXISTS owner_managed_assignment_pins;DROP TABLE owner_managed_calls;DROP TABLE owner_managed_effects;DROP TABLE owner_managed_agents;DROP TABLE owner_project_authorities;").unwrap();
}
#[tokio::test]
async fn schema75_upgrade_is_atomic_and_old_project_authority_requires_exact_evidence() {
    let mut f = fixture().await;
    let root = f._temp.path().join("owner");
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    remove_schema(&db);
    db.execute_batch(
        "DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=74;CREATE TABLE owner_managed_agents(collision INTEGER);",
    )
    .unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        74
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='owner_project_authorities'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT requested_tokens FROM owner_project_reservations",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        400
    );
    db.execute_batch("DROP TABLE owner_managed_agents;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let mut original = f.project.clone();
    original.project_definition_digest = "b".repeat(64);
    assert_eq!(
        f.registry
            .reserve_managed_agent(delivery(&f, agent(&f, "bad_history", 100), original))
            .await
            .unwrap()
            .state,
        "refused"
    );
    assert_eq!(
        reserve(&f, agent(&f, "valid_history", 100)).await.state,
        "reserved"
    );
    assert!(
        f.registry
            .0
            .store
            .call(1024, |db| Ok(db.db.query_row(
                "SELECT count(*)=1 FROM owner_project_authorities",
                [],
                |r| r.get::<_, bool>(0)
            )?))
            .await
            .unwrap()
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_agent_commit_failure_rolls_back_every_hold_and_authority_effect() {
    let f = fixture().await;
    let c = agent(&f, "atomic", 150);
    f.registry.0.store.call(1024, |db| {
        db.db.execute_batch("CREATE TRIGGER fail_agent_receipt BEFORE INSERT ON owner_capacity_receipts WHEN NEW.command_id='reserve_agent_atomic' BEGIN SELECT RAISE(ABORT,'injected receipt failure'); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(
        f.registry
            .reserve_managed_agent(delivery(&f, c.clone(), f.project.clone()))
            .await
            .is_err()
    );
    assert_eq!(count(&f).await, 0);
    assert!(f.registry.0.store.call(1024, |db| {
        Ok(db.db.query_row("SELECT NOT EXISTS(SELECT 1 FROM owner_managed_effects) AND NOT EXISTS(SELECT 1 FROM owner_capacity_grants WHERE accounting_mode<>'reservation_only_v1') AND NOT EXISTS(SELECT 1 FROM owner_project_reservations WHERE accounting_mode<>'reservation_only_v1')", [], |r| r.get::<_, bool>(0))?)
    }).await.unwrap());
    f.registry
        .0
        .store
        .call(1024, |db| {
            db.db.execute_batch("DROP TRIGGER fail_agent_receipt;")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(reserve(&f, c).await.state, "reserved");
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_agent_queued_native_retirement_cannot_create_or_recover_an_effect() {
    let mut f = fixture().await;
    let live = Arc::new(AtomicBool::new(true));
    let callback = live.clone();
    f.profile = f
        .profile
        .with_native_gate(move || callback.load(Ordering::Acquire))
        .unwrap();
    let c = agent(&f, "queued", 150);
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    let d = delivery(&f, c.clone(), f.project.clone());
    assert!(
        VerifiedManagedAgentDelivery::new(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            c.clone(),
            digest.clone(),
            f.project.clone(),
            false,
            Instant::now() + Duration::from_secs(10),
        )
        .is_err()
    );
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
    let mut pending = Box::pin(f.registry.reserve_managed_agent(d));
    tokio::select! {
        biased;
        result=&mut pending=>panic!("reservation bypassed occupied writer: {result:?}"),
        _=tokio::task::yield_now()=>{}
    }
    live.store(false, Ordering::Release);
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    assert_eq!(count(&f).await, 0);
    assert!(
        f.registry
            .recover_managed_agent_reservation(
                f.profile.clone(),
                f.grant.executor_device_id.clone(),
                c.command_id.clone(),
                digest.clone()
            )
            .await
            .is_err()
    );
    assert!(
        f.registry
            .recover_managed_agent_reservation(
                authority('a'),
                f.grant.executor_device_id.clone(),
                c.command_id,
                digest
            )
            .await
            .unwrap()
            .is_none()
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_agent_unknown_execution_blocks_other_profile_and_cannot_erase_evidence() {
    let f = fixture().await;
    let other = authority('b');
    let shared = enroll(&f.registry, &other, "second_profile").await;
    assert_eq!(shared.resource_id, f.resource);
    let child = reserve(&f, agent(&f, "first", 150))
        .await
        .agent_reservation_id
        .unwrap();
    f.registry.0.store.call(1024, move |db| {
        // Even a reported partial amount cannot turn an unknown execution into
        // a terminal, freeable hold. This tests the collector, not a runtime.
        db.db.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES('unknown_call',?1,'execution',?2,1,'unknown',120,0,1)",params![child,"a".repeat(64)])?;
        for sql in [
            "DELETE FROM owner_managed_agents",
            "DELETE FROM owner_managed_calls",
            "DELETE FROM owner_managed_effects",
            "UPDATE owner_managed_calls SET execution_id='replacement'",
            "UPDATE owner_managed_agents SET allocated_tokens=1",
            "UPDATE owner_managed_effects SET payload_digest='0000000000000000000000000000000000000000000000000000000000000000'",
        ] { assert!(db.db.execute(sql,[]).is_err(), "{sql}"); }
        Ok(())
    }).await.unwrap();
    assert!(
        f.registry
            .resource(&other, shared.resource_id.clone())
            .await
            .unwrap()
            .budget
            .remaining_tokens
            .is_none()
    );
    assert!(
        grant(&f.registry, &other, &shared.resource_id, "other_blocked", 1)
            .await
            .is_err()
    );
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "unknown_release"))
            .await
            .reason
            .as_deref(),
        Some("unsettled_usage")
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_agent_post_worker_return_expiry_preserves_original_unknown_receipt() {
    let mut f = fixture().await;
    let caller = std::thread::current().id();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed = calls.clone();
    let pause = Arc::new(Mutex::new(None::<Instant>));
    let boundary = pause.clone();
    f.profile = f
        .profile
        .with_native_gate(move || {
            if std::thread::current().id() == caller
                && observed.fetch_add(1, Ordering::AcqRel) == 1
                && let Some(deadline) = *boundary.lock().unwrap()
            {
                std::thread::sleep(
                    deadline.saturating_duration_since(Instant::now()) + Duration::from_millis(5),
                );
            }
            true
        })
        .unwrap();
    let c = agent(&f, "return_expiry", 100);
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    let expires = Instant::now() + Duration::from_millis(300);
    let d = VerifiedManagedAgentDelivery::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        c.clone(),
        digest.clone(),
        f.project.clone(),
        true,
        expires,
    )
    .unwrap();
    *pause.lock().unwrap() = Some(expires);
    calls.store(0, Ordering::Release);
    assert!(matches!(
        f.registry.reserve_managed_agent(d).await,
        Err(Error::OutcomeUnknown)
    ));
    assert_eq!(count(&f).await, 1);
    let original = f
        .registry
        .recover_managed_agent_reservation(
            authority('a'),
            f.grant.executor_device_id.clone(),
            c.command_id,
            digest,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.state, "reserved");
    assert_eq!(original.allocated_tokens, 100);
    f.registry.shutdown().await.unwrap();
}

async fn web_offer(f: &Fixture, profile: String, published: bool, count: Option<i64>, cap: Option<i64>) {
    let role = agent(f,"role",1).definition.capability;
    let offer = crate::management::WebRoleOffer {role:role.clone(),published,count,budget_cap_per_engagement:cap,rate_cap:None,qualified:true,catalog_published:published};
    f.registry.0.store.call(4096,move|db|{
        db.db.execute("INSERT INTO owner_web_catalog_settings(profile,kind,entity,value) VALUES(?1,'offer',?2,?3) ON CONFLICT(profile,kind,entity) DO UPDATE SET value=excluded.value",params![profile,role,serde_json::to_string(&offer)?])?;
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn web_offer_new_reservations_obey_current_owner_terms_but_original_receipts_are_immutable() {
    let f=fixture().await;
    web_offer(&f,"f".repeat(64),false,Some(1),Some(1)).await;
    let original=agent(&f,"before_terms",100);
    let first=reserve(&f,original.clone()).await;
    assert_eq!(first.state,"reserved","a different owner's settings cannot change this admission");
    web_offer(&f,f.profile.digest.clone(),true,None,Some(80)).await;
    let over=agent(&f,"over_offer",81);
    let refused=reserve(&f,over.clone()).await;
    assert_eq!(refused.reason.as_deref(),Some("insufficient_capacity"));
    let at_cap=reserve(&f,agent(&f,"at_offer",80)).await;
    assert_eq!(at_cap.state,"reserved");
    web_offer(&f,f.profile.digest.clone(),false,None,None).await;
    assert_eq!(reserve(&f,agent(&f,"after_withdrawal",1)).await.reason.as_deref(),Some("resource_unavailable"));
    assert_eq!(serde_json::to_value(reserve(&f,original).await).unwrap(),serde_json::to_value(first).unwrap());
    assert_eq!(serde_json::to_value(reserve(&f,over).await).unwrap(),serde_json::to_value(refused).unwrap());
    assert_eq!(count(&f).await,2);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn web_offer_count_is_atomic_and_fenced_children_free_only_future_admission_slots() {
    let f=fixture().await;
    web_offer(&f,f.profile.digest.clone(),true,Some(1),Some(100)).await;
    let a=agent(&f,"concurrent_a",100); let b=agent(&f,"concurrent_b",100);
    let (ra,rb)=tokio::join!(reserve(&f,a.clone()),reserve(&f,b.clone()));
    assert_eq!([&ra,&rb].iter().filter(|r|r.state=="reserved").count(),1);
    let (winner,loser,refused)=if ra.state=="reserved"{(a,b,rb)}else{(b,a,ra)};
    assert_eq!(refused.reason.as_deref(),Some("insufficient_capacity"));
    fences::apply_fence(&f,fences::command(&f,winner,"offer_free_slot")).await;
    assert_eq!(reserve(&f,agent(&f,"new_after_fence",100)).await.state,"reserved");
    assert_eq!(serde_json::to_value(reserve(&f,loser).await).unwrap(),serde_json::to_value(refused).unwrap(),"a freed slot must not replay a refused command as a new reservation");
    f.registry.shutdown().await.unwrap();
}

#[test]
fn direct_pool_agent_contract_matches_server_and_has_no_project() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!("capacity/managed_agents/pool-agent-v1.json")).unwrap();
    let command: ManagedAgentReserveCommand = serde_json::from_value(fixture["command"].clone()).unwrap();
    command.validate("@owner:example.test").unwrap();
    assert_eq!(canonical::digest(&serde_json::to_value(&command).unwrap()).unwrap(), fixture["commandDigest"]);
    assert!(serde_json::to_value(&command).unwrap().get("projectId").is_none());
    let mut wrong = command;
    wrong.decision_actor_mxid = "@admin:example.test".into();
    assert!(wrong.validate("@owner:example.test").is_err());
}
