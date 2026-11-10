use super::*;

#[test]
fn claude_native_provider_binding_preserves_quota_identity_without_email_or_path_keys() {
    use hagency_runtime::claude::account::{AccountBinding, PROVIDER};
    let binding = |account: &str, email: &str| AccountBinding::from_authenticated_profile(
        account, "89abcdef-0123-4567-89ab-cdef01234567", email,
        Instant::now() + Duration::from_secs(30),
    ).unwrap();
    let original = binding("01234567-89AB-CDEF-0123-456789ABCDEF", "before@example.test");
    let proof = VerifiedProviderAccount::from_claude_binding(original.clone()).unwrap();
    assert_eq!(proof.digest, original.provider_digest());
    assert_eq!(proof.runtime, "claude");
    assert!(proof.claude_binding().is_ok());
    let renamed = VerifiedProviderAccount::from_claude_binding(
        binding("01234567-89ab-cdef-0123-456789abcdef", "after@example.test")
    ).unwrap();
    assert_eq!(proof.digest, renamed.digest, "email rename does not create another capacity account");
    let other = VerifiedProviderAccount::from_claude_binding(
        binding("01234567-89ab-cdef-0123-456789abcdee", "before@example.test")
    ).unwrap();
    assert_ne!(proof.digest, other.digest);
    let unbound = VerifiedProviderAccount::new("claude", PROVIDER, original.subject(), original.expires()).unwrap();
    assert_eq!(unbound.digest, proof.digest);
    assert!(unbound.claude_binding().is_err(), "restored identity is not a fresh native credential observation");
}
#[path = "managed_agent_tests.rs"]
mod managed_agents;
#[path = "parent_tests.rs"]
mod parent;
#[path = "provenance_tests.rs"]
mod provenance_history;
#[path = "readmission_tests.rs"]
mod readmission;

/// A database notes that it was created empty, under the name the server
/// will know it by. Only that database can claim the note, and it stops
/// claiming it once it has to report to a server it could not declare to.
#[tokio::test]
async fn a_database_created_empty_says_so_until_it_reports_undeclared() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let note = root.canonicalize().unwrap().join("ledger-created-empty");
    assert!(registry.ledger_created_empty());
    assert_eq!(std::fs::read(&note).unwrap(), registry.ledger_id().as_bytes());
    // The same database after a restart is still the one that was created empty.
    let name = registry.ledger_id();
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(registry.ledger_id(), name);
    assert!(registry.ledger_created_empty());
    // A note left for another database is not this one's.
    crate::private::replace(&note, "0".repeat(64).as_bytes()).unwrap();
    assert!(!registry.ledger_created_empty());
    crate::private::replace(&note, name.as_bytes()).unwrap();
    assert!(registry.ledger_created_empty());
    // Reporting undeclared ends the claim for good, and may be repeated.
    registry.ledger_reports_undeclared().unwrap();
    assert!(!note.exists());
    assert!(!registry.ledger_created_empty());
    registry.ledger_reports_undeclared().unwrap();
    // Opening the existing database again never writes the note: a database
    // from before this note existed, or one that reported undeclared, stays so.
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(registry.ledger_id(), name);
    assert!(!note.exists());
    assert!(!registry.ledger_created_empty());
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_owner_can_allocate_a_legacy_delegated_grant_without_rewriting_it() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "owner_coordination_resource").await;
    let g = grant(&registry, &a, &resource.resource_id, "legacy_parent", 700).await.unwrap();
    assert_ne!(g.coordinator_mxid, g.owner_mxid);
    let original = canonical::digest(&serde_json::to_value(&g).unwrap()).unwrap();
    let mut reserve = command(&g, "owner_approval", 200);
    reserve.decision_actor = g.owner_mxid.clone();
    let accepted = apply(&registry, &a, reserve.clone()).await;
    assert_eq!(accepted.state, "reserved");
    assert_eq!(apply(&registry, &a, reserve).await.ledger_revision, accepted.ledger_revision);
    let saved = registry.recover_capacity_grant(a.clone(), g.executor_device_id.clone(), g.grant_id.clone()).await.unwrap().unwrap();
    assert_eq!(canonical::digest(&serde_json::to_value(&saved).unwrap()).unwrap(), original);
    let mut foreign = command(&g, "unrelated_approval", 200);
    foreign.decision_actor = "@unrelated:a.test".into();
    assert_eq!(apply(&registry, &a, foreign).await.reason.as_deref(), Some("authority_changed"));
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_host_fence_denies_a_grant_queued_before_account_retirement() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let callback = live.clone();
    let a = authority('a')
        .with_native_gate(move || callback.load(Ordering::Acquire))
        .unwrap();
    let resource = enroll(&registry, &a, "enroll").await;
    let consent = registry
        .prepare_capacity_grant(
            enrollment(&registry, &a, &resource.resource_id, 1).await,
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: None,
                command_id: "queued_consent".into(),
                grant_id: "queued_grant".into(),
                coordinator_mxid: "@coordinator:a.test".into(),
                delegation_expires_at_ms: graphs::now_ms().unwrap() + 60_000,
                allow_self_approval: false,
                eligible_managers: vec!["@manager:a.test".into()],
                allocated_tokens: 500,
            },
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
    let store = registry.0.store.clone();
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
    let mut pending = Box::pin(consent.approve());
    tokio::select! {
        biased;
        result=&mut pending=>panic!("command bypassed occupied writer: {result:?}"),
        _=tokio::task::yield_now()=>{}
    }
    live.store(false, Ordering::Release);
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    assert!(
        registry
            .resource(&a, resource.resource_id.clone())
            .await
            .is_err()
    );
    let fresh = authority('a');
    assert!(
        registry
            .recover_capacity_grant(fresh.clone(), "device_a".into(), "queued_grant".into())
            .await
            .unwrap()
            .is_none()
    );
    let after = registry
        .resource(&fresh, resource.resource_id)
        .await
        .unwrap();
    assert_eq!(after.budget.pool.remaining.map(u64::from), Some(1000));
    registry.shutdown().await.unwrap();
}

/// The resource pool budget evaluated at native time `at`: (remaining, committed).
async fn pool_at(registry: &OwnerResourceRegistry, resource: &str, at: u64) -> (Option<u64>, u64) {
    let resource = resource.to_owned();
    registry
        .0
        .store
        .call(4096, move |db| {
            let resource = read_resource(&db.db, &resource)?;
            let budget = crate::domain::budget_at(&db.db, &resource, None, false, at)?;
            Ok((
                budget.pool.remaining.map(u64::from),
                u64::from(budget.pool.committed),
            ))
        })
        .await
        .unwrap()
}

async fn enrollment(
    registry: &OwnerResourceRegistry,
    profile: &ProfileAuthority,
    resource: &str,
    generation: u64,
) -> VerifiedManagementEnrollment {
    let revision = registry
        .resource(profile, resource.into())
        .await
        .unwrap()
        .revision;
    enrollment_from(profile, resource, &revision, generation)
}
fn enrollment_from(
    profile: &ProfileAuthority,
    resource: &str,
    revision: &str,
    generation: u64,
) -> VerifiedManagementEnrollment {
    VerifiedManagementEnrollment::new(
        profile.clone(),
        format!("enrollment_{}", &profile.digest[..1]),
        format!("contribution_{}", &profile.digest[..1]),
        resource.into(),
        revision.into(),
        format!(
            "@coordinator:{}",
            profile.account.split_once(':').unwrap().1
        ),
        format!("device_{}", &profile.digest[..1]),
        generation,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
}
async fn grant(
    registry: &OwnerResourceRegistry,
    profile: &ProfileAuthority,
    resource: &str,
    id: &str,
    tokens: u64,
) -> Result<NativeGrantReceipt, Error> {
    let domain = profile.account.split_once(':').unwrap().1;
    let selection = OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
        calendar: None,
        command_id: format!("consent_{id}"),
        grant_id: id.into(),
        coordinator_mxid: format!("@coordinator:{domain}"),
        delegation_expires_at_ms: graphs::now_ms().unwrap() + 60_000,
        allow_self_approval: false,
        eligible_managers: vec![format!("@manager:{domain}")],
        allocated_tokens: tokens,
    };
    registry
        .prepare_capacity_grant(
            enrollment(registry, profile, resource, 1).await,
            selection,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
}
fn command(grant: &NativeGrantReceipt, id: &str, tokens: u64) -> AllocationCommand {
    let now = graphs::now_ms().unwrap();
    AllocationCommand {
        version: 1,
        command_id: format!("reserve_{id}"),
        operation: "project.reserve".into(),
        allocation_id: format!("allocation_{id}"),
        project_id: format!("project_{id}"),
        project_revision: 1,
        project_definition_digest: "a".repeat(64),
        request_id: format!("request_{id}"),
        request_revision: 1,
        decision_actor: grant.coordinator_mxid.clone(),
        manager_mxid: grant.eligible_managers[0].clone(),
        grant_receipt: grant.clone(),
        requested_tokens: tokens,
        reserve_command_id: None,
        issued_at_ms: now.saturating_sub(1),
        expires_at_ms: now + 60_000,
    }
}
async fn apply(
    registry: &OwnerResourceRegistry,
    profile: &ProfileAuthority,
    command: AllocationCommand,
) -> NativeAllocationReceipt {
    let e = enrollment_from(
        profile,
        &command.grant_receipt.resource_id,
        &command.grant_receipt.resource_revision,
        1,
    );
    let digest = canonical::digest(&serde_json::to_value(&command).unwrap()).unwrap();
    let delivery = VerifiedAllocationDelivery::new(
        e,
        command,
        digest,
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    registry.apply_project_allocation(delivery).await.unwrap()
}

#[test]
fn server_native_allocation_contract_canonical_digests_agree() {
    for source in [include_str!("management-allocation-v1.json"), include_str!("management-allocation-v2.json")] {
        let f: serde_json::Value = serde_json::from_str(source).unwrap();
        let grant: NativeGrantReceipt = serde_json::from_value(f["grant"].clone()).unwrap();
        let command: AllocationCommand = serde_json::from_value(f["command"].clone()).unwrap();
        let receipt: NativeAllocationReceipt = serde_json::from_value(f["receipt"].clone()).unwrap();
        for (value, key) in [
            (serde_json::to_value(grant).unwrap(), "grantDigest"),
            (serde_json::to_value(command).unwrap(), "commandDigest"),
            (serde_json::to_value(receipt).unwrap(), "receiptDigest"),
            (f["projectDefinition"].clone(), "projectDefinitionDigest"),
        ] {
            assert_eq!(canonical::digest(&value).unwrap(), f[key].as_str().unwrap());
        }
    }
}

#[tokio::test]
async fn oversized_or_unpublishable_owner_grant_rolls_back_before_holding_capacity() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    for oversized in [true, false] {
        let selection = OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
            calendar: None,
            command_id: format!("invalid_{oversized}"),
            grant_id: format!("grant_{oversized}"),
            coordinator_mxid: "@coordinator:a.test".into(),
            delegation_expires_at_ms: graphs::now_ms().unwrap()
                + if oversized {
                    60_000
                } else {
                    367 * 24 * 60 * 60 * 1000
                },
            allow_self_approval: false,
            eligible_managers: if oversized {
                (0..64)
                    .map(|i| format!("@manager{i:02}_{}:a.test", "x".repeat(220)))
                    .collect()
            } else {
                vec!["@manager:a.test".into()]
            },
            allocated_tokens: 500,
        };
        let consent = registry
            .prepare_capacity_grant(
                enrollment(&registry, &a, &resource.resource_id, 1).await,
                selection,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap();
        let result = consent.approve().await;
        if oversized {
            assert!(matches!(result, Err(Error::Capacity)));
        } else {
            assert!(matches!(result, Err(Error::LocalAuthority)));
        }
    }
    let counts = registry
        .0
        .store
        .call(1024, |db| {
            Ok((
                db.db
                    .query_row("SELECT COUNT(*) FROM owner_capacity_grants", [], |r| {
                        r.get::<_, u64>(0)
                    })?,
                db.db
                    .query_row("SELECT COUNT(*) FROM owner_capacity_receipts", [], |r| {
                        r.get::<_, u64>(0)
                    })?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(counts, (0, 0));
    assert_eq!(
        registry
            .resource(&a, resource.resource_id)
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(1000)
    );
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn profiles_and_models_use_their_own_pool_limits_under_concurrent_grants() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let b = authority('b');
    let ra = enroll(&registry, &a, "enroll_a").await;
    let proof = VerifiedProviderAccount::new(
        "claude",
        "https://provider.test",
        "quota-account-fixture",
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let rb = registry
        .prepare_enrollment(
            b.clone(),
            "enroll_b".into(),
            proof,
            "claude-opus-4-6".into(),
            None,
            1000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert_ne!(ra.resource_id, rb.resource_id);
    let (ga, gb) = tokio::join!(
        grant(&registry, &a, &ra.resource_id, "grant_a", 700),
        grant(&registry, &b, &rb.resource_id, "grant_b", 700)
    );
    assert!(ga.is_ok() && gb.is_ok());
    let total = registry
        .0
        .store
        .call(1024, |db| {
            Ok(db.db.query_row(
                "SELECT SUM(allocated_tokens) FROM owner_capacity_grants",
                [],
                |r| r.get::<_, u64>(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(total, 1400);
    for (p, id) in [(&a, &ra.resource_id), (&b, &rb.resource_id)] {
        assert_eq!(
            registry
                .resource(p, id.clone())
                .await
                .unwrap()
                .budget
                .pool
                .remaining
                .map(u64::from),
            Some(300)
        );
    }
    assert!(matches!(
        registry.resource(&a, rb.resource_id).await,
        Err(Error::LocalAuthority)
    ));
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn child_conservation_restart_receipts_and_release_tombstone() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
        .await
        .unwrap();
    let first = command(&g, "first", 200);
    let digest = canonical::digest(&serde_json::to_value(&first).unwrap()).unwrap();
    let applied = apply(&registry, &a, first.clone()).await;
    assert_eq!(applied.state, "reserved");
    let mut duplicate = first.clone();
    duplicate.command_id = "replacement_command".into();
    duplicate.allocation_id = "replacement_allocation".into();
    assert_eq!(
        apply(&registry, &a, duplicate).await.reason.as_deref(),
        Some("request_conflict")
    );
    let over = apply(&registry, &a, command(&g, "over", 501)).await;
    assert_eq!(over.reason.as_deref(), Some("insufficient_capacity"));
    let report = registry
        .resource(&a, resource.resource_id.clone())
        .await
        .unwrap();
    assert_eq!(report.budget.seat.remaining, None);
    assert_eq!(report.budget.pool.remaining.map(u64::from), Some(300));
    assert_eq!(report.usage.reserved, 700);
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let parent = registry
        .recover_capacity_grant(a.clone(), g.executor_device_id.clone(), g.grant_id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(parent).unwrap(),
        serde_json::to_value(&g).unwrap()
    );
    assert!(matches!(
        registry
            .recover_capacity_grant(
                authority('b'),
                g.executor_device_id.clone(),
                g.grant_id.clone()
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    let recovered = registry
        .recover_project_allocation(
            a.clone(),
            g.executor_device_id.clone(),
            first.command_id.clone(),
            digest.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::to_value(&recovered).unwrap(),
        serde_json::to_value(&applied).unwrap()
    );
    assert_eq!(
        apply(&registry, &a, first).await.ledger_revision,
        applied.ledger_revision
    );
    assert!(matches!(
        registry
            .recover_project_allocation(
                authority('b'),
                g.executor_device_id.clone(),
                recovered.command_id.clone(),
                digest
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    let unseen = command(&g, "unseen", 100);
    let mut release = unseen.clone();
    release.command_id = "release_before_reserve".into();
    release.operation = "project.release".into();
    release.reserve_command_id = Some(unseen.command_id.clone());
    assert_eq!(apply(&registry, &a, release).await.state, "released");
    assert_eq!(
        apply(&registry, &a, unseen).await.reason.as_deref(),
        Some("project_unavailable")
    );
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
    assert_eq!(held, 200);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn authority_changes_refuse_new_work_but_cleanup_and_historical_recovery_survive() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
        .await
        .unwrap();
    let first = command(&g, "first", 500);
    assert_eq!(apply(&registry, &a, first.clone()).await.state, "reserved");
    let mut manager = command(&g, "wrong_manager", 1);
    manager.manager_mxid = "@someone:a.test".into();
    assert_eq!(
        apply(&registry, &a, manager).await.reason.as_deref(),
        Some("authority_changed")
    );
    let mut own = command(&g, "self_approval", 1);
    own.manager_mxid = g.coordinator_mxid.clone();
    assert_eq!(
        apply(&registry, &a, own).await.reason.as_deref(),
        Some("authority_changed")
    );
    let changed = command(&g, "stale_generation", 1);
    let digest = canonical::digest(&serde_json::to_value(&changed).unwrap()).unwrap();
    let e = enrollment(&registry, &a, &resource.resource_id, 2).await;
    let delivery = VerifiedAllocationDelivery::new(
        e,
        changed,
        digest,
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(
        registry
            .apply_project_allocation(delivery)
            .await
            .unwrap()
            .reason
            .as_deref(),
        Some("authority_changed")
    );
    registry
        .prepare_revoke(
            a.clone(),
            "revoke_access".into(),
            resource.resource_id.clone(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert_eq!(
        apply(&registry, &a, command(&g, "revoked", 1))
            .await
            .reason
            .as_deref(),
        Some("authority_changed")
    );
    let mut release = first;
    release.reserve_command_id = Some(release.command_id.clone());
    release.command_id = "release_after_revoke".into();
    release.operation = "project.release".into();
    let e = enrollment_from(&a, &resource.resource_id, &"b".repeat(64), 2);
    let digest = canonical::digest(&serde_json::to_value(&release).unwrap()).unwrap();
    let delivery = VerifiedAllocationDelivery::new(
        e,
        release,
        digest,
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert_eq!(
        registry
            .apply_project_allocation(delivery)
            .await
            .unwrap()
            .state,
        "released"
    );
    registry.shutdown().await.unwrap();
}

fn authority(which: char) -> ProfileAuthority {
    ProfileAuthority::new(
        which.to_string().repeat(64),
        format!("@owner:{which}.test"),
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap()
}
fn provider() -> VerifiedProviderAccount {
    VerifiedProviderAccount::new(
        "codex",
        "https://provider.test",
        "quota-account-fixture",
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
}
async fn enroll(
    registry: &OwnerResourceRegistry,
    profile: &ProfileAuthority,
    command: &str,
) -> OwnerResourceReceipt {
    registry
        .prepare_enrollment(
            profile.clone(),
            command.into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            1000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
}

#[tokio::test]
async fn two_profiles_share_account_and_private_access_requires_consumed_owner_consent() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let b = authority('b');
    let first = enroll(&registry, &a, "enroll_a").await;
    assert!(matches!(
        registry.resource(&b, first.resource_id.clone()).await,
        Err(Error::LocalAuthority)
    ));
    let pending = registry
        .prepare_link(
            b.clone(),
            "link_b".into(),
            first.resource_id.clone(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
    assert!(matches!(
        registry.resource(&b, first.resource_id.clone()).await,
        Err(Error::LocalAuthority)
    ));
    pending.approve().await.unwrap();
    let same = enroll(&registry, &b, "enroll_b").await;
    assert_eq!(first.resource_id, same.resource_id);
    let seen = registry
        .resource(&b, first.resource_id.clone())
        .await
        .unwrap();
    assert_eq!(seen.budget.seat.remaining, None);
    assert_eq!(seen.budget.pool.remaining.map(u64::from), Some(1000));
    let counts = registry
        .0
        .store
        .call(1024, |db| {
            Ok((
                db.db
                    .query_row("SELECT COUNT(*) FROM seats", [], |r| r.get::<_, u64>(0))?,
                db.db
                    .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, u64>(0))?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(counts, (1, 1));
    registry
        .prepare_revoke(
            b.clone(),
            "revoke_b".into(),
            first.resource_id.clone(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert!(matches!(
        registry.resource(&b, first.resource_id.clone()).await,
        Err(Error::LocalAuthority)
    ));
    assert!(registry.resource(&a, first.resource_id).await.is_ok());
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn receipts_replay_after_restart_but_conflicting_budget_and_retired_consent_fail() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let first = enroll(&registry, &a, "stable_enrollment").await;
    let conflict = registry
        .prepare_enrollment(
            a.clone(),
            "different_ceiling".into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            2000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
    assert!(matches!(conflict.approve().await, Err(Error::Conflict)));
    let retired = authority('b');
    let pending = registry
        .prepare_link(
            retired.clone(),
            "retired_confirmation".into(),
            first.resource_id.clone(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
    retired.retire();
    assert!(matches!(
        pending.approve().await,
        Err(Error::LocalAuthority)
    ));
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let replay = enroll(&registry, &authority('a'), "stable_enrollment").await;
    assert_eq!(
        serde_json::to_value(&replay).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    assert_eq!(
        registry
            .resource(&authority('a'), first.resource_id)
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(1000)
    );
    registry.shutdown().await.unwrap();
}

#[test]
fn second_process_owner_registry_lock_child() {
    let Some(path) = std::env::var_os("HAGENCY_OWNER_REGISTRY_LOCK_FIXTURE") else {
        return;
    };
    if std::env::var_os("HAGENCY_OWNER_REGISTRY_REOPEN_FIXTURE").is_some() {
        let registry = OwnerResourceRegistry::open_root(Path::new(&path)).unwrap();
        let resource = std::env::var("HAGENCY_OWNER_RESOURCE_FIXTURE").unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                assert_eq!(
                    registry
                        .resource(&authority('a'), resource.clone())
                        .await
                        .unwrap()
                        .budget
                        .pool
                        .remaining
                        .map(u64::from),
                    Some(300)
                );
                assert!(
                    grant(
                        &registry,
                        &authority('a'),
                        &resource,
                        "second_process_overflow",
                        301
                    )
                    .await
                    .is_err()
                );
                registry.shutdown().await.unwrap();
            });
    } else {
        assert!(matches!(
            OwnerResourceRegistry::open_root(Path::new(&path)),
            Err(Error::Locked)
        ));
    }
}

#[tokio::test]
async fn separate_process_cannot_create_an_independent_writer_for_shared_owner_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let first = enroll(&registry, &authority('a'), "enroll").await;
    grant(
        &registry,
        &authority('a'),
        &first.resource_id,
        "original_parent",
        700,
    )
    .await
    .unwrap();
    let alias = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(alias.id(), registry.id());
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("domain::owner_resources::tests::second_process_owner_registry_lock_child")
        .arg("--exact")
        .arg("--nocapture")
        .env("HAGENCY_OWNER_REGISTRY_LOCK_FIXTURE", &root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        registry
            .resource(&authority('a'), first.resource_id.clone())
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(300)
    );
    registry.shutdown().await.unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .arg("domain::owner_resources::tests::second_process_owner_registry_lock_child")
        .arg("--exact")
        .arg("--nocapture")
        .env("HAGENCY_OWNER_REGISTRY_LOCK_FIXTURE", &root)
        .env("HAGENCY_OWNER_REGISTRY_REOPEN_FIXTURE", "1")
        .env("HAGENCY_OWNER_RESOURCE_FIXTURE", first.resource_id)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[tokio::test]
async fn original_legacy_holds_survive_restart_without_an_implicit_shared_cap() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let original = enroll(&registry, &a, "enroll").await;
    let id = original.resource_id.clone();
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
            db.approve("original_approval", &proof, 1000)?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(
        grant(&registry, &a, &original.resource_id, "too_much", 1001)
            .await
            .is_err()
    );
    grant(&registry, &a, &original.resource_id, "remaining", 1000)
        .await
        .unwrap();
    assert_eq!(
        registry
            .resource(&a, original.resource_id.clone())
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(0)
    );
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(
        registry
            .resource(&a, original.resource_id)
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(0)
    );
    let held=registry.0.store.call(1024,|db|Ok(db.db.query_row("SELECT COALESCE(allocated_tokens,tokens) FROM engagements WHERE state IN('reserved','active')",[],|r|r.get::<_,u64>(0))?)).await.unwrap();
    assert_eq!(held, 700);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn expired_grant_refuses_new_reserve_but_releases_original_child_and_recovery_is_read_only() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let resource = enroll(&registry, &a, "enroll").await;
    let g = grant(&registry, &a, &resource.resource_id, "parent", 700)
        .await
        .unwrap();
    let original = command(&g, "original", 200);
    let applied = apply(&registry, &a, original.clone()).await;
    assert_eq!(applied.state, "reserved");
    let next = command(&g, "late", 1);
    let digest = canonical::digest(&serde_json::to_value(&next).unwrap()).unwrap();
    let e = enrollment_from(&a, &resource.resource_id, &g.resource_revision, 2);
    assert!(matches!(
        VerifiedAllocationDelivery::new(
            e,
            next.clone(),
            digest.clone(),
            false,
            Instant::now() + Duration::from_secs(20)
        ),
        Err(Error::LocalAuthority)
    ));
    let e = enrollment_from(&a, &resource.resource_id, &g.resource_revision, 1);
    let fence = registry.0.clone();
    let future = g.period_end_ms + 1;
    let late = registry
        .0
        .store
        .call(32 * 1024, move |db| {
            db.owner_project_allocation(
                e,
                next,
                digest,
                fence,
                Instant::now() + Duration::from_secs(20),
                future,
            )
        })
        .await
        .unwrap();
    assert_eq!(late.reason.as_deref(), Some("authority_changed"));
    let mut release = original.clone();
    release.operation = "project.release".into();
    release.reserve_command_id = Some(original.command_id.clone());
    release.command_id = "cleanup_after_period".into();
    let digest = canonical::digest(&serde_json::to_value(&release).unwrap()).unwrap();
    let e = enrollment_from(&a, &resource.resource_id, &"b".repeat(64), 2);
    let fence = registry.0.clone();
    let released = registry
        .0
        .store
        .call(32 * 1024, move |db| {
            db.owner_project_allocation(
                e,
                release,
                digest,
                fence,
                Instant::now() + Duration::from_secs(20),
                future,
            )
        })
        .await
        .unwrap();
    assert_eq!(released.state, "released");
    assert!(released.ledger_revision > applied.ledger_revision);
    let digest = canonical::digest(&serde_json::to_value(&original).unwrap()).unwrap();
    let recovered = registry
        .recover_project_allocation(
            a.clone(),
            g.executor_device_id.clone(),
            original.command_id,
            digest,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.ledger_revision, applied.ledger_revision);
    // Parent accounting does not auto-renew or free unknown/unused grant holds.
    assert_eq!(
        registry
            .resource(&a, resource.resource_id)
            .await
            .unwrap()
            .usage
            .reserved,
        700
    );
    registry.shutdown().await.unwrap();
}

#[test]
fn legacy_ledger_is_never_silently_adopted_or_recovered() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("legacy");
    let mut db = DomainRepository::open(&root).unwrap();
    let resource = crate::domain_worker::clock_fixtures::resource("legacy", "seat", 1000);
    db.register(&crate::domain_worker::clock_fixtures::registration())
        .unwrap();
    db.put_resource(&resource).unwrap();
    let proof = crate::domain_worker::clock_fixtures::proof(
        &crate::domain_worker::clock_fixtures::request("original", "Original", &resource, 700),
    );
    db.admit(&proof, 1000).unwrap();
    db.approve("original_approval", &proof, 1000).unwrap();
    drop(db);
    let read = || {
        let db = Connection::open(root.join("domain.sqlite3")).unwrap();
        db.query_row(
            "SELECT id,state,COALESCE(allocated_tokens,tokens) FROM engagements",
            [],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            },
        )
        .unwrap()
    };
    let before = read();
    assert_eq!(before.2, 700);
    assert!(matches!(
        OwnerResourceRegistry::open_root(&root),
        Err(Error::LocalAuthority)
    ));
    assert_eq!(read(), before);
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_and_copied_owner_directory_cannot_claim_the_same_authority() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let link = temp.path().join("link");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    std::os::unix::fs::symlink(&root, &link).unwrap();
    assert!(matches!(
        OwnerResourceRegistry::open_root(&link),
        Err(Error::Private)
    ));
    registry
        .0
        .store
        .call(1024, |db| {
            db.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
            Ok(())
        })
        .await
        .unwrap();
    registry.shutdown().await.unwrap();
    drop(registry);
    let copy = temp.path().join("copy");
    crate::private::directory(&copy).unwrap();
    std::fs::copy(root.join("domain.sqlite3"), copy.join("domain.sqlite3")).unwrap();
    match OwnerResourceRegistry::open_root(&copy) {
        Err(Error::Private) => {}
        Err(error) => panic!("copied ledger error: {error:?}"),
        Ok(_) => panic!("copied ledger accepted"),
    }
}
