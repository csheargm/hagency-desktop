fn provenance_fixture_reply(body: &Value, publications: &PublicationFixture) -> Option<Value> {
    let body = if body.get("offer").is_some() {
        assert_eq!(body["expectedRevision"], 3);
        assert_eq!(body["offer"]["runtime"], "codex");
        assert!(body["offer"].get("allocatedTokens").is_none());
        &body["offer"]
    } else {
        body
    };
    let mut state = publications.lock().unwrap();
    let db = rusqlite::Connection::open_with_flags(
        state.connections.provenance_path.as_ref().unwrap(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let (kind, command, revision): (String, String, String) = db.query_row(
        "SELECT first_offer_kind,first_offer_command_id,first_offer_revision FROM owner_resource_provenance WHERE resource_id=?1",
        [body["resourceId"].as_str().unwrap()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).expect("exposure must already be durably visible when HTTP arrives");
    assert_eq!(kind, "contribution");
    assert_eq!(command, "first_offer");
    assert_eq!(revision, body["resourceRevision"]);
    state.connections.contribution_posts += 1;
    if matches!(
        body["commandId"].as_str(),
        Some("lost_offer_reply" | "lost_reoffer_reply")
    ) {
        return None;
    }
    assert!(matches!(
        body["commandId"].as_str(),
        Some("first_offer" | "reoffer_refused")
    ));
    Some(json!({"code":"fixture_admission_refused"}))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_contribution_provenance_is_durable_before_http_and_survives_rejection_and_lost_reply()
 {
    run_current_owner_contract("provenance").await;
}

fn saved_provenance(path: &std::path::Path, resource: &str) -> Value {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    db.query_row(
        "SELECT registry_id,creation_command_id,first_offer_profile,first_offer_command_id,first_offer_revision,first_offer_at_ms FROM owner_resource_provenance WHERE resource_id=?1",
        [resource],|r|Ok(json!([r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,u64>(5)?])),
    ).unwrap()
}

async fn verify_current_provenance_contract(
    manager: &NativeManagement,
    owner: &NativeOwner,
    access: &OwnerGrantAccess,
    path: &std::path::Path,
    resource: &hagency_core::project::Resource,
) {
    use super::super::ManagementCommand;
    let revision = hagency_store::resource_publication_revision(resource).unwrap();
    let command = |id: &str, revision: String| ManagementCommand::Contribute {
        publication_first: false,
        command_id: id.into(),
        resource_id: resource.id(),
        expected_revision: revision,
        label: "Current resource".into(),
        coordinator_mxid: "@owner:test".into(),
    };
    let mut limited_package = current_owner_package(97);
    limited_package
        .permissions
        .retain(|p| *p != Permission::ContributeResources);
    let limited = owner.management(limited_package).await.unwrap();
    assert_eq!(
        limited
            .central(command("denied_offer", revision.clone()))
            .await
            .err()
            .unwrap()
            .code,
        "management_permission_required"
    );
    limited.retire();
    assert_eq!(
        manager
            .central(command("stale_offer", "0".repeat(64)))
            .await
            .err()
            .unwrap()
            .code,
        "management_revision_conflict"
    );
    let db = rusqlite::Connection::open(path).unwrap();
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    let creation: (String, String, String) = db.query_row(
        "SELECT creation_command_id,creation_digest,creation_profile FROM owner_resource_provenance WHERE resource_id=?1 AND first_offer_kind IS NULL",
        [resource.id()], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(creation.0, "current_connection_primary");
    db.execute_batch("CREATE TRIGGER reject_offer BEFORE UPDATE OF first_offer_kind ON owner_resource_provenance BEGIN SELECT RAISE(ABORT,'fixture disk failure'); END;").unwrap();
    assert!(
        manager
            .central(command("storage_failed_offer", revision.clone()))
            .await
            .is_err()
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM owner_resource_provenance WHERE first_offer_kind IS NOT NULL",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(db.query_row(
        "SELECT creation_command_id,creation_digest,creation_profile FROM owner_resource_provenance WHERE resource_id=?1",
        [resource.id()], |row| Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?, row.get::<_,String>(2)?)),
    ).unwrap(), creation, "failed exposure retains genuine current enrollment proof");
    db.execute_batch("DROP TRIGGER reject_offer;").unwrap();
    assert_eq!(
        manager
            .central(command("first_offer", revision.clone()))
            .await
            .err()
            .unwrap()
            .code,
        "fixture_admission_refused"
    );
    let original = saved_provenance(path, &resource.id());
    assert_eq!(
        original[1], "current_connection_primary",
        "offering retains genuine enrollment creation proof"
    );
    assert_eq!(original[3], "first_offer");
    assert!(
        manager
            .central(command("lost_offer_reply", revision.clone()))
            .await
            .is_err()
    );
    let reoffer = |id: &str| ManagementCommand::Reoffer {
        command_id: id.into(),
        contribution_id: "fixture_contribution".into(),
        expected_contribution_revision: 3,
        resource_id: resource.id(),
        expected_revision: revision.clone(),
        label: "Updated offer".into(),
        coordinator_mxid: "@owner:test".into(),
    };
    let service = access.service();
    let proof = service
        .verified_enrollment_with(&access.registry, "fixture_contribution".into(), |_| async {
            Ok(provider_fixture("synthetic-account"))
        })
        .await
        .unwrap();
    let review = service
        .prepare(&access.registry, proof, Fixture::choices())
        .await
        .unwrap();
    let id = review.summary().intent_id;
    drop(review);
    assert_eq!(
        manager
            .central(reoffer("blocked_reoffer"))
            .await
            .err()
            .unwrap()
            .code,
        "owner_grant_prior_intent_required"
    );
    let cancelled = match access.review_cancellation(id).await.unwrap() {
        OwnerGrantCancellationResult::Review(review) => *review,
        _ => panic!("expected pending original cancellation"),
    };
    access.confirm_cancellation(cancelled).await.unwrap();
    // Replaying the refused command retains its original result. A separate
    // deliberate submit uses another command only after explicit cancellation.
    assert_eq!(
        manager
            .central(reoffer("blocked_reoffer"))
            .await
            .err()
            .unwrap()
            .code,
        "owner_grant_prior_intent_required"
    );

    assert_eq!(
        manager
            .central(reoffer("reoffer_refused"))
            .await
            .err()
            .unwrap()
            .code,
        "fixture_admission_refused"
    );
    assert!(
        manager
            .central(reoffer("lost_reoffer_reply"))
            .await
            .is_err()
    );
    assert_eq!(saved_provenance(path, &resource.id()), original);
}
