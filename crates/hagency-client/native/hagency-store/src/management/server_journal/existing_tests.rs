use super::*;
use serde_json::json;

fn context(account: &str) -> HostContext {
    HostContext::new(account.into(), 1, "b".repeat(64), 1, "device".into(), 1).unwrap()
}
fn permissions() -> Vec<Permission> {
    vec![
        Permission::ReadIntents,
        Permission::AcknowledgeIntents,
        Permission::RequestProjects,
        Permission::ReadResources,
        Permission::EnrollResources,
    ]
}
fn access(journal: &ServerManagementJournal) -> Session {
    Session::new_server(
        journal.clone(),
        context("@admin:test"),
        permissions(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
}
fn intent(id: &str) -> Intent {
    Intent::new(
        IntentOperation::ProjectRequest,
        id.into(),
        &json!({"commandId":id,"requestedTokens":17}),
    )
    .unwrap()
}
fn snapshot(path: &Path) -> Vec<(String, Vec<u8>)> {
    ["domain.sqlite3", "domain.sqlite3-wal"]
        .into_iter()
        .map(|name| (name.into(), std::fs::read(path.join(name)).unwrap()))
        .collect()
}

#[test]
fn original_server_journal_isolated_regressions() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "management::server_journal::existing_tests::original_server_journal_child",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_ORIGINAL_SERVER_JOURNAL_TEST", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[tokio::test]
async fn original_server_journal_child() {
    if std::env::var("HAGENCY_ORIGINAL_SERVER_JOURNAL_TEST").as_deref() != Ok("1") {
        return;
    }
    let installation =
        hagency_platform::legacy_writer::test_support::initialize_process_root().unwrap();
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().canonicalize().unwrap();
    let profile = "a".repeat(64);
    let owner = context("@admin:test");
    let path = root_path.join("original");
    let repository = DomainRepository::open_managed(&path, &profile).unwrap();
    let store = DomainStore::start(repository, 16).unwrap();
    let source = Session::new(
        store.clone(),
        owner.clone(),
        permissions(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    for id in ["pending", "replied", "acknowledged"] {
        source.prepare_intent(&owner, intent(id)).await.unwrap();
    }
    for id in ["replied", "acknowledged"] {
        source
            .record_intent_reply(
                &owner,
                intent(id),
                IntentReply::Success(json!({"original":id})),
            )
            .await
            .unwrap();
    }
    source
        .acknowledge_intent(&owner, "acknowledged".into())
        .await
        .unwrap();
    source.retire();
    store.shutdown().await.unwrap();
    assert!(matches!(
        ServerManagementJournal::open(&path, &profile, "@admin:test"),
        Err(Error::ResourceSetupRequired)
    ));

    // Prepare independent real schema fixtures before publishing refusal.
    let mutations = [
        (
            "resource",
            "INSERT INTO resources VALUES('resource','preset','{}')",
        ),
        (
            "local_receipt",
            "INSERT INTO management_commands VALUES(printf('%064d',0),'local','resource.enroll',printf('%064d',0),'{}')",
        ),
        (
            "local_pending",
            "INSERT INTO management_intents VALUES(printf('%064d',0),'local','\"enroll_resource\"',printf('%064d',0),'{}',NULL,'pending')",
        ),
        (
            "unknown_intent",
            "INSERT INTO management_intents VALUES(printf('%064d',0),'unknown','\"future_operation\"',printf('%064d',0),'{}',NULL,'pending')",
        ),
        (
            "lost_primary_key",
            "DROP INDEX management_intents_pending; DROP TABLE management_intents; CREATE TABLE management_intents(actor_scope TEXT, command_id TEXT, operation TEXT, digest TEXT, input TEXT, reply TEXT, state TEXT)",
        ),
        (
            "trigger",
            "CREATE TRIGGER hidden_write AFTER INSERT ON management_intents BEGIN DELETE FROM resources; END",
        ),
        ("future_schema", "PRAGMA user_version=2147483647"),
        (
            "foreign_identity",
            "UPDATE management_identity SET profile_digest=printf('%064d',0)",
        ),
    ];
    let mut rejected = Vec::new();
    for (name, sql) in mutations {
        let rejected_path = root_path.join(name);
        drop(DomainRepository::open_managed(&rejected_path, &profile).unwrap());
        let db = Connection::open(&rejected_path.join("domain.sqlite3")).unwrap();
        db.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .unwrap();
        db.execute_batch(sql).unwrap();
        drop(db);
        rejected.push((rejected_path.clone(), snapshot(&rejected_path)));
    }
    let split = root_path.join("split");
    drop(DomainRepository::open_managed(&split, &profile).unwrap());
    crate::private::write_new(
        &split.join("server-management.sqlite3"),
        b"existing custody",
    )
    .unwrap();
    let exclusive = hagency_platform::legacy_writer::ExclusiveWriterGuard::acquire().unwrap();
    exclusive
        .publish_refusal(&"c".repeat(64), "other_account", &"d".repeat(64))
        .unwrap();
    drop(exclusive);
    let marker = installation.join("legacy-resource-refusal.json");
    let refusal = std::fs::read(&marker).unwrap();
    for (rejected_path, before) in rejected {
        assert!(
            ServerManagementJournal::open(&rejected_path, &profile, "@admin:test").is_err(),
            "{} admitted",
            rejected_path.display()
        );
        assert_eq!(snapshot(&rejected_path), before, "rejected source changed");
        assert!(!rejected_path.join("server-management.sqlite3").exists());
    }
    assert!(matches!(
        ServerManagementJournal::open(&split, &profile, "@admin:test"),
        Err(Error::ResourceSetupRequired)
    ));
    assert!(matches!(
        ServerManagementJournal::open(&path, &"e".repeat(64), "@admin:test"),
        Err(Error::LocalAuthority)
    ));

    let original = snapshot(&path);
    let journal = ServerManagementJournal::open(&path, &profile, "@admin:test").unwrap();
    assert_eq!(
        snapshot(&path),
        original,
        "opening must not migrate or recover"
    );
    assert!(matches!(
        ServerManagementJournal::open(&path, &profile, "@admin:test"),
        Err(Error::Locked)
    ));
    let session = access(&journal);
    assert!(
        Session::new_server(
            journal.clone(),
            context("@other:test"),
            permissions(),
            Instant::now() + Duration::from_secs(30)
        )
        .is_err()
    );
    let rows = session.pending_intents(&owner).await.unwrap();
    assert_eq!(
        rows.iter()
            .map(|r| r.command_id.as_str())
            .collect::<Vec<_>>(),
        vec!["pending", "replied"]
    );
    assert!(
        !session
            .prepare_intent(&owner, intent("pending"))
            .await
            .unwrap()
            .is_first_attempt()
    );
    assert!(
        session
            .prepare_intent(&owner, intent("replied"))
            .await
            .unwrap()
            .reply
            .is_some()
    );
    assert!(matches!(
        session.prepare_intent(&owner, intent("acknowledged")).await,
        Err(Error::Conflict)
    ));
    assert!(matches!(
        session.resources(&owner, ResourceQuery::default()).await,
        Err(Error::ResourceSetupRequired)
    ));
    let local = Intent::new(
        IntentOperation::EnrollResource,
        "denied_local".into(),
        &json!({"commandId":"denied_local"}),
    )
    .unwrap();
    assert!(matches!(
        session.prepare_intent(&owner, local).await,
        Err(Error::ResourceSetupRequired)
    ));
    session
        .record_intent_reply(
            &owner,
            intent("pending"),
            IntentReply::Success(json!({"original":"pending"})),
        )
        .await
        .unwrap();
    session.retire();
    assert!(matches!(
        session.pending_intents(&owner).await,
        Err(Error::LocalAuthority)
    ));
    journal.shutdown().await.unwrap();
    let journal = ServerManagementJournal::open(&path, &profile, "@admin:test").unwrap();
    let session = access(&journal);
    let saved = session
        .prepare_intent(&owner, intent("pending"))
        .await
        .unwrap();
    assert!(!saved.is_first_attempt());
    assert!(saved.reply.is_some());
    let changed = Intent::new(
        IntentOperation::ProjectRequest,
        "pending".into(),
        &json!({"commandId":"pending","requestedTokens":19}),
    )
    .unwrap();
    assert!(matches!(
        session.prepare_intent(&owner, changed).await,
        Err(Error::Conflict)
    ));
    // Replacing the original lock while the worker is alive invalidates it.
    std::fs::rename(path.join("domain.lock"), path.join("domain.lock.retained")).unwrap();
    crate::private::write_new(&path.join("domain.lock"), b"").unwrap();
    assert!(
        session
            .prepare_intent(&owner, intent("after_replacement"))
            .await
            .is_err()
    );
    journal.shutdown().await.unwrap();
    assert!(!path.join("server-management.sqlite3").exists());
    let db = Connection::open_with_flags(
        path.join("domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM management_intents", [], |r| r
            .get::<_, u32>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        crate::DOMAIN_SCHEMA_VERSION
    );
    assert_eq!(
        db.pragma_query_value(None, "application_id", |r| r.get::<_, i32>(0))
            .unwrap(),
        0x48414732
    );
    assert_eq!(std::fs::read(marker).unwrap(), refusal);
    assert!(matches!(
        hagency_platform::legacy_writer::LegacyWriterGuard::acquire(),
        Err(hagency_platform::legacy_writer::Error::Refused)
    ));
}
