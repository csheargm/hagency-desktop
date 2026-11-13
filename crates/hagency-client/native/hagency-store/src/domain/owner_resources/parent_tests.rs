use super::*;

fn closure(
    g: &NativeGrantReceipt,
    id: &str,
    previous: Option<&NativeGrantSettlementReceipt>,
) -> GrantClosureCommand {
    GrantClosureCommand {
        version: 1,
        command_id: id.into(),
        operation: if previous.is_some() {
            "grant.settle"
        } else {
            "grant.close"
        }
        .into(),
        grant_id: g.grant_id.clone(),
        grant_receipt_digest: canonical::digest(&serde_json::to_value(g).unwrap()).unwrap(),
        previous_settlement_digest: previous
            .map(|p| canonical::digest(&serde_json::to_value(p).unwrap()).unwrap()),
        reason: if previous.is_some() {
            "settlement_progress"
        } else {
            "owner_withdrawal"
        }
        .into(),
        requested_at_ms: graphs::now_ms().unwrap(),
    }
}
fn delivery(
    a: &ProfileAuthority,
    g: &NativeGrantReceipt,
    c: GrantClosureCommand,
) -> VerifiedParentDelivery {
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    VerifiedParentDelivery::new(
        enrollment_from(
            a,
            &g.resource_id,
            &g.resource_revision,
            g.executor_generation,
        ),
        c,
        digest,
        true,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
}
async fn close(
    registry: &OwnerResourceRegistry,
    a: &ProfileAuthority,
    g: &NativeGrantReceipt,
    c: GrantClosureCommand,
) -> NativeGrantSettlementReceipt {
    registry
        .apply_parent_settlement(delivery(a, g, c))
        .await
        .unwrap()
}
fn release(c: &AllocationCommand) -> AllocationCommand {
    let mut r = c.clone();
    r.operation = "project.release".into();
    r.reserve_command_id = Some(c.command_id.clone());
    r.command_id = format!("release_{}", c.allocation_id);
    r
}
async fn remaining(registry: &OwnerResourceRegistry, a: &ProfileAuthority, resource: &str) -> u64 {
    registry
        .resource(a, resource.into())
        .await
        .unwrap()
        .budget
        .pool
        .remaining
        .map(u64::from)
        .unwrap()
}

#[test]
fn parent_server_wire_canonical_fixtures_match_and_closed_can_retain_consumed() {
    let f: serde_json::Value =
        serde_json::from_str(include_str!("management-parent-settlement-v1.json")).unwrap();
    let c: GrantClosureCommand = serde_json::from_value(f["command"].clone()).unwrap();
    let r: NativeGrantSettlementReceipt = serde_json::from_value(f["receipt"].clone()).unwrap();
    assert_eq!(
        canonical::digest(&serde_json::to_value(c).unwrap()).unwrap(),
        f["commandDigest"]
    );
    assert_eq!(
        canonical::digest(&serde_json::to_value(&r).unwrap()).unwrap(),
        f["receiptDigest"]
    );
    assert_eq!(r.state, "closed");
    assert_eq!(r.retained_tokens, 123);
    assert_eq!(r.known_consumed_tokens, 123);
    assert_eq!(r.unresolved_tokens, 0);
}

#[tokio::test]
async fn parent_close_retains_children_until_explicit_release_and_latest_settlement() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
        .await
        .unwrap();
    let child = command(&g, "first", 200);
    let reserved = apply(&registry, &a, child.clone()).await;
    let c = closure(&g, "close", None);
    let first = close(&registry, &a, &g, c.clone()).await;
    assert_eq!(
        (
            first.state.as_str(),
            first.retained_tokens,
            first.unresolved_tokens
        ),
        ("retained", 200, 200)
    );
    assert_eq!(first.known_consumed_tokens, 0);
    assert!(first.fence_revision > reserved.ledger_revision);
    assert_eq!(first.fence_revision, first.ledger_revision);
    assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 800);
    // Active is not the permanent fence: even a future accidental flag reset
    // cannot make a delayed/unseen reserve executable.
    registry
        .0
        .store
        .call(1024, |db| {
            db.db
                .execute("UPDATE owner_capacity_grants SET active=1", [])?;
            Ok(())
        })
        .await
        .unwrap();
    let late = apply(&registry, &a, command(&g, "late", 1)).await;
    assert_eq!(late.reason.as_deref(), Some("authority_changed"));
    let released = apply(&registry, &a, release(&child)).await;
    assert_eq!(
        (released.state.as_str(), released.retained_tokens),
        ("released", 0)
    );
    assert!(released.ledger_revision > first.fence_revision);
    assert_eq!(
        remaining(&registry, &a, &resource.resource_id).await,
        800,
        "a child reply alone cannot refund the parent"
    );
    let settle = closure(&g, "settle", Some(&first));
    let final_result = close(&registry, &a, &g, settle.clone()).await;
    assert_eq!(
        (final_result.state.as_str(), final_result.retained_tokens),
        ("closed", 0)
    );
    assert_eq!(final_result.fence_revision, first.fence_revision);
    assert!(final_result.ledger_revision > released.ledger_revision);
    assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 1000);
    let replay = close(&registry, &a, &g, c.clone()).await;
    assert_eq!(
        serde_json::to_value(replay).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    let latest = registry
        .parent_settlement(a.clone(), g.executor_device_id.clone(), g.grant_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(latest.ledger_revision, final_result.ledger_revision);
    assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 1000);
    assert_eq!(
        registry
            .recover_capacity_grant(a.clone(), g.executor_device_id.clone(), g.grant_id.clone())
            .await
            .unwrap()
            .unwrap()
            .allocated_tokens,
        700
    );
    let mut stale = settle;
    stale.command_id = "settle_stale".into();
    assert!(matches!(
        registry
            .apply_parent_settlement(delivery(&a, &g, stale))
            .await,
        Err(Error::Conflict)
    ));
    let mut changed = c;
    changed.requested_at_ms += 1;
    assert!(matches!(
        registry
            .apply_parent_settlement(delivery(&a, &g, changed))
            .await,
        Err(Error::Conflict)
    ));
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn ended_settled_grant_frees_its_allocation_but_keeps_an_unresolved_child() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await.resource_id;
    let g = grant(&registry, &a, &resource, "monthly", 700)
        .await
        .unwrap();
    let end = g.period_end_ms;
    let child = command(&g, "month_child", 200);
    assert_eq!(apply(&registry, &a, child.clone()).await.state, "reserved");
    let settled = close(&registry, &a, &g, closure(&g, "close_month", None)).await;
    assert_eq!(
        (settled.state.as_str(), settled.retained_tokens),
        ("retained", 200)
    );
    // The reserved child is unresolved liability in and after its month.
    assert_eq!(
        pool_at(&registry, &resource, end - 1).await,
        (Some(800), 200)
    );
    assert_eq!(pool_at(&registry, &resource, end).await, (Some(800), 200));
    let released = apply(&registry, &a, release(&child)).await;
    assert_eq!(
        (released.state.as_str(), released.retained_tokens),
        ("released", 0)
    );
    // Within the month the settlement still retains its amount until it is
    // settled again; after the month the settled grant frees its allocation.
    assert_eq!(
        pool_at(&registry, &resource, end - 1).await,
        (Some(800), 200)
    );
    assert_eq!(pool_at(&registry, &resource, end).await, (Some(1000), 0));
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn parent_fence_covers_unseen_reserves_and_distinct_empty_commands_get_distinct_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
        .await
        .unwrap();
    let unseen = command(&g, "unseen", 200);
    let first = close(&registry, &a, &g, closure(&g, "close", None)).await;
    assert_eq!(first.retained_tokens, 0);
    let release = apply(&registry, &a, release(&unseen)).await;
    assert_eq!(release.state, "released");
    let late = apply(&registry, &a, unseen).await;
    assert_eq!(late.state, "refused");
    let second = close(&registry, &a, &g, closure(&g, "settle", Some(&first))).await;
    assert!(
        first.ledger_revision < release.ledger_revision
            && release.ledger_revision < late.ledger_revision
            && late.ledger_revision < second.ledger_revision
    );
    assert_eq!(first.fence_revision, second.fence_revision);
    assert_eq!(second.retained_tokens, 0);
    let held = registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT SUM(retained_tokens) FROM owner_project_reservations",
                [],
                |r| r.get::<_, u64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(held, 0);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn parent_concurrent_close_and_reserve_are_one_serial_accounting_decision() {
    for reverse in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
        let a = authority('a');
        let resource = enroll(&registry, &a, "enroll").await;
        let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
            .await
            .unwrap();
        let c = command(&g, "concurrent", 200);
        let p = closure(&g, "close", None);
        let (parent, child) = if reverse {
            let (child, parent) =
                tokio::join!(apply(&registry, &a, c), close(&registry, &a, &g, p));
            (parent, child)
        } else {
            tokio::join!(close(&registry, &a, &g, p), apply(&registry, &a, c))
        };
        if child.state == "reserved" {
            assert!(child.ledger_revision < parent.fence_revision);
            assert_eq!(parent.retained_tokens, 200);
        } else {
            assert_eq!(child.state, "refused");
            assert!(child.ledger_revision > parent.fence_revision);
            assert_eq!(parent.retained_tokens, 0);
        }
        assert_eq!(
            remaining(&registry, &a, &resource.resource_id).await,
            1000 - parent.retained_tokens
        );
        registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn parent_unknown_accounting_retains_original_hold_and_refuses_zero_release() {
    for unknown in ["parent", "child", "inconsistent_reservation"] {
        let temp = tempfile::tempdir().unwrap();
        let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
        let a = authority('a');
        let resource = enroll(&registry, &a, "enroll").await;
        let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
            .await
            .unwrap();
        let c = command(&g, "unknown", 200);
        apply(&registry, &a, c.clone()).await;
        registry
            .0
            .store
            .call(1024, move |db| {
                db.db.execute(match unknown {
                    "parent" => "UPDATE owner_capacity_grants SET accounting_mode='legacy_unknown'",
                    "child" => "UPDATE owner_project_reservations SET accounting_mode='future_execution'",
                    _ => "UPDATE owner_project_reservations SET retained_tokens=100",
                }, [])?;
                Ok(())
            })
            .await
            .unwrap();
        let first = close(&registry, &a, &g, closure(&g, "close", None)).await;
        assert_eq!(
            (
                first.state.as_str(),
                first.retained_tokens,
                first.unresolved_tokens
            ),
            ("retained", 700, 700)
        );
        assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 300);
        let refused = apply(&registry, &a, release(&c)).await;
        assert_eq!(refused.reason.as_deref(), Some("unsettled_usage"));
        let later = close(&registry, &a, &g, closure(&g, "settle", Some(&first))).await;
        assert_eq!(later.retained_tokens, 700);
        assert_eq!(later.fence_revision, first.fence_revision);
        let child = registry
            .0
            .store
            .call(1024, |db| {
                Ok(db.db.query_row(
                    "SELECT retained_tokens FROM owner_project_reservations",
                    [],
                    |r| r.get::<_, u64>(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(
            child,
            if unknown == "inconsistent_reservation" {
                100
            } else {
                200
            }
        );
        registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn parent_cleanup_allows_fresh_generation_after_access_loss_but_recovery_stays_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = registry
        .prepare_capacity_grant(
            enrollment(&registry, &a, &resource.resource_id, 1).await,
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: None,
                command_id: "consent_parent".into(),
                grant_id: "parent".into(),
                coordinator_mxid: "@coordinator:a.test".into(),
                delegation_expires_at_ms: graphs::now_ms().unwrap() + 2000,
                allow_self_approval: false,
                eligible_managers: vec!["@manager:a.test".into()],
                allocated_tokens: 700,
            },
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(
        g.delegation_expires_at_ms
            .saturating_sub(graphs::now_ms().unwrap())
            + 1,
    ))
    .await;
    assert!(graphs::now_ms().unwrap() >= g.delegation_expires_at_ms);
    registry
        .prepare_revoke(
            a.clone(),
            "revoke".into(),
            resource.resource_id.clone(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    let c = closure(&g, "close", None);
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    // Fresh verified generation/configuration permits cleanup, without current
    // resource access, old coordinator consent, or renewing the expired grant.
    let e = VerifiedManagementEnrollment::new(
        a.clone(),
        g.enrollment_id.clone(),
        g.contribution_id.clone(),
        g.resource_id.clone(),
        "b".repeat(64),
        "@replacement:a.test".into(),
        g.executor_device_id.clone(),
        2,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let p = VerifiedParentDelivery::new(
        e,
        c.clone(),
        digest.clone(),
        true,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let first = registry.apply_parent_settlement(p).await.unwrap();
    assert_eq!(first.retained_tokens, 0);
    assert!(matches!(
        registry
            .parent_settlement(
                authority('b'),
                g.executor_device_id.clone(),
                g.grant_id.clone()
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        registry
            .recover_parent_settlement(
                a.clone(),
                "another_device".into(),
                c.command_id.clone(),
                digest.clone()
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        VerifiedParentDelivery::new(
            enrollment_from(&a, &g.resource_id, &g.resource_revision, 1),
            c.clone(),
            digest.clone(),
            false,
            Instant::now() + Duration::from_secs(30)
        ),
        Err(Error::LocalAuthority)
    ));
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let old = registry
        .recover_parent_settlement(
            a.clone(),
            g.executor_device_id.clone(),
            c.command_id.clone(),
            digest,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(old).unwrap(),
        serde_json::to_value(first).unwrap()
    );
    let replay = close(&registry, &a, &g, c).await;
    assert_eq!(replay.retained_tokens, 0);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn parent_schema_upgrade_preserves_existing_grants_as_unknown_and_rolls_back_failed_migration()
 {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
        .await
        .unwrap();
    apply(&registry, &a, command(&g, "old", 200)).await;
    registry.shutdown().await.unwrap();
    drop(registry);
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    super::managed_agents::remove_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS owner_octos_model_resources;DROP TABLE IF EXISTS owner_octos_enrollments;DROP TABLE IF EXISTS owner_octos_profiles;DROP TABLE IF EXISTS owner_octos_key;DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE owner_parent_settlements; ALTER TABLE owner_project_reservations DROP COLUMN accounting_mode; ALTER TABLE owner_capacity_grants DROP COLUMN accounting_mode; DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=73;").unwrap();
    db.execute_batch("CREATE TABLE owner_parent_settlements(collision INTEGER);")
        .unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        73
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM pragma_table_info('owner_capacity_grants') WHERE name='accounting_mode'",[],|r|r.get::<_,u64>(0)).unwrap(),0);
    assert_eq!(
        db.query_row(
            "SELECT allocated_tokens FROM owner_capacity_grants",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        700
    );
    db.execute_batch("DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE owner_parent_settlements;")
        .unwrap();
    drop(db);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let modes = registry
        .0
        .store
        .call(1024, |db| {
            Ok((
                db.db.query_row(
                    "SELECT accounting_mode FROM owner_capacity_grants",
                    [],
                    |r| r.get::<_, String>(0),
                )?,
                db.db.query_row(
                    "SELECT accounting_mode FROM owner_project_reservations",
                    [],
                    |r| r.get::<_, String>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(modes, ("unknown".into(), "unknown".into()));
    let result = close(&registry, &a, &g, closure(&g, "close", None)).await;
    assert_eq!(result.retained_tokens, 700);
    assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 300);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn parent_rechecks_lease_and_profile_after_writer_queue_before_committing() {
    for retire_profile in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
        let a = authority('a');
        let resource = enroll(&registry, &a, "enroll").await;
        let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
            .await
            .unwrap();
        let c = closure(&g, "close", None);
        let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
        let store = registry.0.store.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (unblock, blocked) = std::sync::mpsc::channel();
        let blocker = tokio::spawn(async move {
            store
                .call(1024, move |_db| {
                    started.send(()).unwrap();
                    blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                    Ok(())
                })
                .await
                .unwrap()
        });
        ready.await.unwrap();
        let e = enrollment_from(&a, &g.resource_id, &g.resource_revision, 1);
        let lease = if retire_profile {
            Duration::from_secs(20)
        } else {
            Duration::from_millis(200)
        };
        let p =
            VerifiedParentDelivery::new(e, c.clone(), digest.clone(), true, Instant::now() + lease)
                .unwrap();
        let mut pending = Box::pin(registry.apply_parent_settlement(p));
        tokio::select! {
            biased;
            result=&mut pending=>panic!("command bypassed occupied writer: {result:?}"),
            _=tokio::task::yield_now()=>{}
        }
        if retire_profile {
            a.retire();
        } else {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        unblock.send(()).unwrap();
        blocker.await.unwrap();
        assert!(matches!(pending.await, Err(Error::LocalAuthority)));
        let fresh = authority('a');
        assert!(
            registry
                .recover_parent_settlement(
                    fresh.clone(),
                    g.executor_device_id.clone(),
                    c.command_id,
                    digest
                )
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            registry
                .parent_settlement(
                    fresh.clone(),
                    g.executor_device_id.clone(),
                    g.grant_id.clone()
                )
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            remaining(&registry, &fresh, &resource.resource_id).await,
            300
        );
        registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn parent_close_cannot_release_an_independent_legacy_hold_on_same_provider_account() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let id = resource.resource_id.clone();
    registry
        .0
        .store
        .call(4096, move |db| {
            let owner_resource = read_resource(&db.db, &id)?;
            let legacy = crate::domain_worker::clock_fixtures::resource(
                "legacy_pool",
                &owner_resource.seat_id,
                1000,
            );
            db.register(&crate::domain_worker::clock_fixtures::registration())?;
            db.put_resource(&legacy)?;
            let proof = crate::domain_worker::clock_fixtures::proof(
                &crate::domain_worker::clock_fixtures::request(
                    "legacy_held",
                    "Original",
                    &legacy,
                    700,
                ),
            );
            db.admit(&proof, 1000)?;
            db.approve("legacy_approval", &proof, 1000)?;
            Ok(())
        })
        .await
        .unwrap();
    let g = grant(&registry, &a, &resource.resource_id, "parent", 300)
        .await
        .unwrap();
    assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 700);
    let p = close(&registry, &a, &g, closure(&g, "close", None)).await;
    assert_eq!(p.retained_tokens, 0);
    assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 1000);
    assert!(
        grant(&registry, &a, &resource.resource_id, "overbook", 1001)
            .await
            .is_err()
    );
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn parent_unknown_future_accounting_preserves_consumption_and_cannot_claim_no_uncertainty() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
        .await
        .unwrap();
    let mut prior = close(&registry, &a, &g, closure(&g, "close", None)).await;
    // Synthetic persisted future-version proof. This implementation cannot
    // generate consumed usage; it must preserve such evidence on a downgrade,
    // while refusing to infer that the unsupported mode has no uncertainty.
    prior.known_consumed_tokens = 900;
    prior.unresolved_tokens = 20;
    prior.retained_tokens = 920;
    prior.liability_count = 2;
    prior.state = "retained".into();
    let saved = prior.clone();
    registry.0.store.call(4096, move |db| {
        db.db.execute("UPDATE owner_capacity_grants SET accounting_mode='future_execution'", [])?;
        db.db.execute("UPDATE owner_parent_settlements SET receipt=?1,receipt_digest=?2,retained_tokens=920",params![serde_json::to_string(&saved)?,canonical::digest(&serde_json::to_value(&saved)?)?])?;
        Ok(())
    }).await.unwrap();
    let later = close(&registry, &a, &g, closure(&g, "settle", Some(&prior))).await;
    assert_eq!(later.known_consumed_tokens, 900);
    assert_eq!(later.unresolved_tokens, 700);
    assert_eq!(later.retained_tokens, 1600);
    assert_eq!(later.state, "retained");
    assert_eq!(remaining(&registry, &a, &resource.resource_id).await, 0);
    assert!(
        grant(&registry, &a, &resource.resource_id, "overbook", 1)
            .await
            .is_err()
    );
    registry.shutdown().await.unwrap();
}
