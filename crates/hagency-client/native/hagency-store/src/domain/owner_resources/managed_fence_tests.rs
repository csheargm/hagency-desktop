use super::*;
pub(super) fn command(
    _f: &Fixture,
    c: ManagedAgentReserveCommand,
    id: &str,
) -> ManagedAgentFenceCommand {
    ManagedAgentFenceCommand {
        version: 1,
        capability: AGENT_FENCE_CAPABILITY.into(),
        operation: "agent.fence".into(),
        command_id: id.into(),
        reserve_command_digest: canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap(),
        reserve_command: c,
        issued_at_ms: graphs::now_ms().unwrap(),
    }
}
fn proof(f: &Fixture, c: ManagedAgentFenceCommand, generation: u64) -> VerifiedManagedAgentFence {
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    VerifiedManagedAgentFence::new(
        enrollment_from(
            &f.profile,
            &f.resource,
            &f.grant.resource_revision,
            generation,
        ),
        c,
        digest,
        f.project.clone(),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
pub(super) async fn apply_fence(
    f: &Fixture,
    c: ManagedAgentFenceCommand,
) -> ManagedAgentFenceReceipt {
    f.registry
        .apply_managed_agent_fence(proof(f, c, 1))
        .await
        .unwrap()
}
#[test]
fn managed_fence_closed_canonical_fixture_matches_server_schema9() {
    let f: serde_json::Value = serde_json::from_str(include_str!(
        "capacity/managed_agents/management-agent-fence-v1.json"
    ))
    .unwrap();
    let c: ManagedAgentFenceCommand = serde_json::from_value(f["command"].clone()).unwrap();
    c.validate("@owner:example.test").unwrap();
    let r: ManagedAgentFenceReceipt = serde_json::from_value(f["receipt"].clone()).unwrap();
    assert_eq!(
        canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap(),
        f["commandDigest"]
    );
    assert_eq!(
        canonical::digest(&serde_json::to_value(&r).unwrap()).unwrap(),
        f["receiptDigest"]
    );
    let mut bad = f["command"].clone();
    bad["authorized"] = json!(true);
    assert!(serde_json::from_value::<ManagedAgentFenceCommand>(bad).is_err());
}
#[tokio::test]
async fn managed_fence_absent_tombstone_prevents_delayed_and_semantic_replacement_reserve() {
    let f = fixture().await;
    let reserve_command = agent(&f, "absent", 150);
    let c = command(&f, reserve_command.clone(), "fence_absent");
    let r = apply_fence(&f, c.clone()).await;
    assert_eq!(r.state, "absent_fenced");
    assert_eq!(r.retained_tokens, Some(0));
    assert!(r.usage_complete);
    assert_eq!(count(&f).await, 0);
    let old = apply_fence(&f, c).await;
    assert_eq!(old.ledger_revision, r.ledger_revision);
    let delayed = reserve(&f, reserve_command.clone()).await;
    assert_eq!(delayed.state, "refused");
    assert_eq!(count(&f).await, 0);
    let mut replacement = reserve_command.clone();
    replacement.command_id = "replacement".into();
    replacement.request_id = "replacement_request".into();
    assert_eq!(reserve(&f, replacement).await.state, "refused");
    let mut changed = reserve_command;
    changed.proposed_agent_id = "replacement_agent".into();
    changed.command_id = "replacement2".into();
    assert_eq!(reserve(&f, changed).await.state, "refused");
    f.registry
        .0
        .store
        .call(1024, |db| {
            assert!(
                db.db
                    .execute("DELETE FROM owner_managed_fences", [])
                    .is_err()
            );
            assert!(
                db.db
                    .execute(
                        "UPDATE owner_managed_fences SET fence_revision=fence_revision+1",
                        []
                    )
                    .is_err()
            );
            Ok(())
        })
        .await
        .unwrap();
}
#[tokio::test]
async fn managed_fence_before_effect_is_known_unused_and_original_reserve_receipt_is_retained() {
    let f = fixture().await;
    let c = agent(&f, "unused", 150);
    let original = reserve(&f, c.clone()).await;
    let fenced = apply_fence(&f, command(&f, c.clone(), "fence_unused")).await;
    assert_eq!(fenced.agent_reservation_id, original.agent_reservation_id);
    assert_eq!(fenced.retained_tokens, Some(0));
    assert_eq!(fenced.process_state, "not_started");
    assert!(fenced.usage_complete);
    let old = reserve(&f, c).await;
    assert_eq!(old.ledger_revision, original.ledger_revision);
    assert_eq!(old.retained_tokens, 150);
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "release_project"))
            .await
            .retained_tokens,
        0
    );
}
#[tokio::test]
async fn managed_fence_issued_provision_retains_unknown_and_never_claims_stopped() {
    let f = fixture().await;
    let c = agent(&f, "unknown", 150);
    let original = reserve(&f, c.clone()).await;
    let child = original.agent_reservation_id.unwrap();
    let a = ManagedAssignmentAuthority::fixture(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        child.clone(),
        "assignment".into(),
        1,
        Instant::now() + Duration::from_secs(20),
    );
    assert!(matches!(
        f.registry.claim_managed_provision(a).await.unwrap(),
        ManagedProvisionClaim::Issued(_)
    ));
    let r = apply_fence(&f, command(&f, c, "fence_issued")).await;
    assert_eq!(r.retained_tokens, None);
    assert_eq!(r.process_state, "unknown");
    assert!(!r.usage_complete);
    let release = apply(&f.registry, &f.profile, release(&f, "release_unknown")).await;
    assert_eq!(release.state, "refused");
    assert_eq!(release.reason.as_deref(), Some("unsettled_usage"));
}
#[tokio::test]
async fn managed_fence_generation_rotation_and_resource_retirement_allow_cleanup_only() {
    let f = fixture().await;
    let c = agent(&f, "old", 150);
    reserve(&f, c.clone()).await;
    let resource = f.resource.clone();
    f.registry
        .0
        .store
        .call(1024, move |db| {
            db.db.execute(
                "UPDATE owner_resource_grants SET active=0 WHERE resource_id=?1",
                [resource],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let r = f
        .registry
        .apply_managed_agent_fence(proof(&f, command(&f, c, "fence_rotated"), 2))
        .await
        .unwrap();
    assert_eq!(r.state, "fenced");
    assert_eq!(r.retained_tokens, Some(0));
    let next = reserve(&f, agent(&f, "new", 100)).await;
    assert_eq!(next.state, "refused");
}
#[tokio::test]
async fn managed_fence_exact_project_history_and_wrong_profile_fail_closed() {
    let f = fixture().await;
    let receipt = canonical::digest(&serde_json::to_value(&f.receipt).unwrap()).unwrap();
    let history = f
        .registry
        .managed_agent_project_history(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            f.project.allocation_id.clone(),
            receipt.clone(),
        )
        .await
        .unwrap();
    assert_eq!(history.command_id, f.project.command_id);
    assert!(
        f.registry
            .managed_agent_project_history(
                authority('b'),
                f.grant.executor_device_id.clone(),
                f.project.allocation_id.clone(),
                receipt
            )
            .await
            .is_err()
    );
    let c = command(&f, agent(&f, "bad", 100), "fence_bad");
    let mut wrong = c.clone();
    wrong.reserve_command.manager_mxid = "@other:a.test".into();
    wrong.reserve_command_digest =
        canonical::digest(&serde_json::to_value(&wrong.reserve_command).unwrap()).unwrap();
    assert!(
        f.registry
            .apply_managed_agent_fence(proof(&f, wrong, 1))
            .await
            .is_err()
    );
    assert!(
        f.registry
            .recover_managed_agent_fence(
                f.profile.clone(),
                f.grant.executor_device_id.clone(),
                c.command_id.clone(),
                canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap()
            )
            .await
            .unwrap()
            .is_none()
    );
}
#[tokio::test]
async fn managed_fence_receipt_failure_rolls_back_tombstone_and_child_state() {
    let f = fixture().await;
    let c = agent(&f, "rollback", 150);
    reserve(&f, c.clone()).await;
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER fail_fence_result BEFORE INSERT ON owner_capacity_receipts WHEN NEW.command_id='fence_rollback' BEGIN SELECT RAISE(ABORT,'fixture');END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .apply_managed_agent_fence(proof(&f, command(&f, c, "fence_rollback"), 1))
            .await
            .is_err()
    );
    f.registry
        .0
        .store
        .call(1024, |db| {
            let count: u64 =
                db.db
                    .query_row("SELECT COUNT(*) FROM owner_managed_fences", [], |r| {
                        r.get(0)
                    })?;
            assert_eq!(count, 0);
            let state: String =
                db.db
                    .query_row("SELECT state FROM owner_managed_agents", [], |r| r.get(0))?;
            assert_eq!(state, "reserved");
            Ok(())
        })
        .await
        .unwrap();
}
#[tokio::test]
async fn managed_fence_schema77_upgrade_rollback_reopen_preserves_original_hold() {
    let mut f = fixture().await;
    let c = agent(&f, "upgrade", 150);
    let r = reserve(&f, c.clone()).await;
    let root = f._temp.path().join("owner");
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_octos_model_resources;DROP TABLE IF EXISTS owner_octos_enrollments;DROP TABLE IF EXISTS owner_octos_profiles;DROP TABLE IF EXISTS owner_octos_key;DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE IF EXISTS owner_managed_runtime_observations;DROP TABLE IF EXISTS owner_managed_runtime_stops;DROP TABLE IF EXISTS owner_managed_runtime_lifetimes;DROP TABLE IF EXISTS owner_resource_adoptions;DROP TABLE owner_managed_fences;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=76;CREATE TABLE owner_managed_fences(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        76
    );
    assert_eq!(
        db.query_row(
            "SELECT allocated_tokens FROM owner_managed_agents",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        150
    );
    db.execute_batch("DROP TABLE owner_managed_fences").unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let old = reserve(&f, c.clone()).await;
    assert_eq!(old.ledger_revision, r.ledger_revision);
    let fence = command(&f, c, "after_upgrade");
    let first = apply_fence(&f, fence.clone()).await;
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let replay = apply_fence(&f, fence).await;
    assert_eq!(replay.ledger_revision, first.ledger_revision);
    f.registry.shutdown().await.unwrap();
}
