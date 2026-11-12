use super::*;
use super::super::provenance::{self, History};

async fn history(registry: &OwnerResourceRegistry, resource: &str) -> History {
    let id = registry.id().to_owned();
    let resource = resource.to_owned();
    registry
        .0
        .store
        .call(1024, move |db| provenance::history(&db.db, &id, &resource))
        .await
        .unwrap()
}
async fn offer(
    registry: &OwnerResourceRegistry,
    profile: &ProfileAuthority,
    resource: &str,
    command: &str,
) -> Result<(), Error> {
    let revision = registry.resource(profile, resource.into()).await?.revision;
    registry
        .record_contribution_attempt(profile.clone(), resource.into(), revision, command.into())
        .await
}
async fn evidence(registry: &OwnerResourceRegistry, resource: &str) -> Value {
    let id = resource.to_owned();
    registry.0.store.call(1024, move |db| Ok(db.db.query_row(
        "SELECT creation_command_id,creation_digest,creation_profile,first_offer_kind,first_offer_profile,first_offer_command_id,first_offer_revision,first_offer_at_ms FROM owner_resource_provenance WHERE resource_id=?1",
        [id], |r| Ok(json!([r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<u64>>(7)?])),
    )?)).await.unwrap()
}

#[tokio::test]
async fn creation_and_first_offer_survive_aliases_revoke_and_restart() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let b = authority('b');
    let first = enroll(&registry, &a, "create_a").await;
    let id = &first.resource_id;
    assert_eq!(history(&registry, id).await, History::NeverOffered);
    let original = evidence(&registry, id).await;
    assert_eq!(original[0], "create_a");
    assert_eq!(original[2], a.digest);
    assert_eq!(enroll(&registry, &b, "alias_b").await.resource_id, *id);
    assert_eq!(evidence(&registry, id).await, original);
    offer(&registry, &b, id, "first_offer_b").await.unwrap();
    let offered = evidence(&registry, id).await;
    assert_eq!(offered[3], "contribution");
    assert_eq!(offered[4], b.digest);
    assert_eq!(offered[5], "first_offer_b");
    for command in ["first_offer_b", "later_offer_a"] {
        offer(&registry, &a, id, command).await.unwrap();
        assert_eq!(evidence(&registry, id).await, offered);
    }
    registry
        .prepare_revoke(
            b,
            "revoke_b".into(),
            id.clone(),
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert_eq!(history(&registry, id).await, History::Offered);
    registry.shutdown().await.unwrap();
    drop(registry);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(enroll(&registry, &a, "create_a").await.resource_id, *id);
    assert_eq!(history(&registry, id).await, History::Offered);
    assert_eq!(evidence(&registry, id).await, offered);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn schema92_resources_never_gain_creation_proof_from_reenrollment() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owner");
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let a = authority('a');
    let first = enroll(&registry, &a, "old_creation").await;
    registry.shutdown().await.unwrap();
    drop(registry);
    // Construct the exact previous schema, preserving the existing resource,
    // provider/seat identity and original receipt. Only this synthetic DB changes.
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE owned_dispatch_execution_policies; DROP TABLE approval_binding_incarnations; DROP TRIGGER owner_resource_provenance_immutable; DROP TRIGGER owner_resource_provenance_retained; DROP TABLE owner_resource_provenance; DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=92;").unwrap();
    drop(db);
    let registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let id = &first.resource_id;
    assert_eq!(history(&registry, id).await, History::Unknown);
    assert_eq!(enroll(&registry, &a, "old_creation").await.resource_id, *id);
    assert_eq!(enroll(&registry, &a, "new_alias").await.resource_id, *id);
    assert_eq!(history(&registry, id).await, History::Unknown);
    offer(&registry, &a, id, "offer_old").await.unwrap();
    assert_eq!(history(&registry, id).await, History::Offered);
    let saved = evidence(&registry, id).await;
    assert!(saved[0].is_null());
    assert!(saved[1].is_null());
    assert!(saved[2].is_null());
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn failed_creation_proof_rolls_back_resource_seat_grant_and_receipt() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    registry.0.store.call(1024, |db| {db.db.execute_batch("CREATE TRIGGER reject_creation BEFORE INSERT ON owner_resource_provenance BEGIN SELECT RAISE(ABORT,'fixture storage failure'); END;")?;Ok(())}).await.unwrap();
    let result = registry
        .prepare_enrollment(
            authority('a'),
            "creation".into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            1000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await;
    assert!(result.is_err());
    registry
        .0
        .store
        .call(1024, |db| {
            for table in [
                "resources",
                "seats",
                "owner_resource_accounts",
                "owner_resource_grants",
                "owner_resource_receipts",
                "owner_resource_provenance",
            ] {
                assert_eq!(
                    db.db
                        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                            .get::<_, u64>(0))?,
                    0,
                    "{table}"
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn stale_revision_wrong_profile_and_retired_writer_do_not_record_offer() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let first = enroll(&registry, &a, "create").await;
    let id = &first.resource_id;
    let revision = registry.resource(&a, id.clone()).await.unwrap().revision;
    assert!(matches!(
        registry
            .record_contribution_attempt(a.clone(), id.clone(), "0".repeat(64), "stale".into())
            .await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        registry
            .record_contribution_attempt(
                authority('b'),
                id.clone(),
                revision.clone(),
                "wrong_profile".into()
            )
            .await,
        Err(Error::LocalAuthority)
    ));
    let (started, ready) = tokio::sync::oneshot::channel();
    let (unblock, blocked) = std::sync::mpsc::channel();
    let store = registry.0.store.clone();
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
    let mut pending = Box::pin(registry.record_contribution_attempt(
        a.clone(),
        id.clone(),
        revision,
        "queued_offer".into(),
    ));
    tokio::select! {biased;result=&mut pending=>panic!("bypassed occupied writer: {result:?}"),_=tokio::task::yield_now()=>{}}
    a.retire();
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    assert_eq!(history(&registry, id).await, History::NeverOffered);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn capacity_grant_exposure_commits_with_receipt_and_rolls_back_on_failure() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let first = enroll(&registry, &a, "create").await;
    let id = &first.resource_id;
    registry.0.store.call(1024, |db| {db.db.execute_batch("CREATE TRIGGER reject_grant BEFORE INSERT ON owner_capacity_grants BEGIN SELECT RAISE(ABORT,'fixture storage failure'); END;")?;Ok(())}).await.unwrap();
    assert!(grant(&registry, &a, id, "failed", 500).await.is_err());
    assert_eq!(history(&registry, id).await, History::NeverOffered);
    registry
        .0
        .store
        .call(1024, |db| {
            db.db.execute_batch("DROP TRIGGER reject_grant;")?;
            Ok(())
        })
        .await
        .unwrap();
    let receipt = grant(&registry, &a, id, "approved", 500).await.unwrap();
    assert_eq!(history(&registry, id).await, History::Offered);
    let saved = evidence(&registry, id).await;
    assert_eq!(saved[3], "capacity_grant");
    assert_eq!(saved[5], "consent_approved");
    assert_eq!(saved[6], receipt.resource_revision);
    let replay = registry.prepare_capacity_grant(
        enrollment(&registry, &a, id, 1).await,
        OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
            calendar: None,
            command_id: "consent_approved".into(),
            grant_id: receipt.grant_id.clone(),
            coordinator_mxid: receipt.coordinator_mxid.clone(),
            delegation_expires_at_ms: receipt.delegation_expires_at_ms,
            allow_self_approval: receipt.allow_self_approval,
            eligible_managers: receipt.eligible_managers.clone(),
            allocated_tokens: receipt.allocated_tokens,
        },
        Instant::now() + Duration::from_secs(30),
    ).unwrap().approve().await.unwrap();
    assert_eq!(
        serde_json::to_value(receipt).unwrap(),
        serde_json::to_value(replay).unwrap()
    );
    assert_eq!(evidence(&registry, id).await, saved);
    registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn database_preserves_first_offer_and_creation_identity() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let a = authority('a');
    let first = enroll(&registry, &a, "create").await;
    offer(&registry, &a, &first.resource_id, "offer")
        .await
        .unwrap();
    let before = evidence(&registry, &first.resource_id).await;
    registry.0.store.call(1024, |db| {
        for statement in [
            "DELETE FROM owner_resource_provenance",
            "UPDATE owner_resource_provenance SET creation_command_id=NULL,creation_digest=NULL,creation_profile=NULL",
            "UPDATE owner_resource_provenance SET first_offer_kind=NULL,first_offer_profile=NULL,first_offer_command_id=NULL,first_offer_revision=NULL,first_offer_at_ms=NULL",
            "UPDATE owner_resource_provenance SET first_offer_command_id='replacement'",
        ] { assert!(db.db.execute(statement, []).is_err(),"{statement}"); }
        Ok(())
    }).await.unwrap();
    assert_eq!(evidence(&registry, &first.resource_id).await, before);
    registry.shutdown().await.unwrap();
}
