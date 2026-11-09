use super::*;
use serde_json::json;

fn context() -> HostContext {
    HostContext::new(
        "@owner:example.test".into(),
        1,
        "b".repeat(64),
        1,
        "original_device".into(),
        1,
    )
    .unwrap()
}
fn permissions() -> [Permission; 4] {
    [
        Permission::ReadIntents,
        Permission::AcknowledgeIntents,
        Permission::ContributeResources,
        Permission::EnrollResources,
    ]
}
fn authority() -> ManagementAuthority {
    ManagementAuthority::new(
        context(),
        permissions(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn binding() -> ArchiveJournalBinding {
    ArchiveJournalBinding::new(
        "migration_original".into(),
        "d".repeat(64),
        "a".repeat(64),
        "@owner:example.test".into(),
        "e".repeat(64),
        crate::DOMAIN_SCHEMA_VERSION,
    )
    .unwrap()
}
fn intent(id: &str) -> Intent {
    Intent::new(IntentOperation::Contribute,id.into(),&json!({"commandId":id,"resourceId":"original_resource","coordinatorMxid":"@coordinator:example.test"})).unwrap()
}
fn success() -> IntentReply {
    IntentReply::Success(
        json!({"admission":{"contribution":{"id":"con_original","state":"admitted"}}}),
    )
}
struct Fixture {
    _root: tempfile::TempDir,
    path: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().canonicalize().unwrap().join("source");
        let mut repo = DomainRepository::open_managed(&path, &"a".repeat(64)).unwrap();
        let resource=serde_json::from_value(json!({"presetId":"original_preset","seatId":"original_seat","framework":"codex","model":"gpt-5.6-sol","published":false,"ceiling":{"tokens":5000,"period":"monthly"}})).unwrap();
        repo.put_resource(&resource).unwrap();
        let store = DomainStore::start(repo, 8).unwrap();
        let session = Session::new(
            store.clone(),
            context(),
            permissions(),
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        session
            .prepare_intent(&context(), intent("original_admission"))
            .await
            .unwrap();
        session
            .record_intent_reply(&context(), intent("original_admission"), success())
            .await
            .unwrap();
        store.shutdown().await.unwrap();
        Self { _root: root, path }
    }
    fn source(&self) -> Connection {
        let db = Connection::open_with_flags(
            self.path.join("domain.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .unwrap();
        db.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
            .unwrap();
        db
    }
    fn fence(&self) {
        let db = self.source();
        db.execute_batch("BEGIN IMMEDIATE;CREATE TABLE management_archive_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),manifest TEXT NOT NULL);").unwrap();
        db.execute(
            "INSERT INTO management_archive_identity VALUES(1,?1)",
            [serde_json::to_string(&binding().0).unwrap()],
        )
        .unwrap();
        db.pragma_update(None, "application_id", MANAGEMENT_ARCHIVE_APPLICATION_ID)
            .unwrap();
        db.execute_batch("COMMIT").unwrap();
    }
    fn open(&self) -> PrivateManagementJournal {
        PrivateManagementJournal::open_existing(&self.path, binding()).unwrap()
    }
}

#[tokio::test]
async fn archive_preserves_original_admission_custody_and_restart_tombstones() {
    let f = Fixture::new().await;
    let before: (String, String) = f
        .source()
        .query_row(
            "SELECT digest,reply FROM management_intents WHERE command_id='original_admission'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    f.fence();
    let journal = f.open();
    let session = journal.session(authority(), &"a".repeat(64)).unwrap();
    let rows = session.pending_intents(&context()).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].reply, Some(success()));
    assert!(
        !session
            .prepare_intent(&context(), intent("original_admission"))
            .await
            .unwrap()
            .is_first_attempt()
    );
    assert!(matches!(
        session
            .record_intent_reply(
                &context(),
                intent("original_admission"),
                IntentReply::Failure {
                    status: 409,
                    code: "different".into()
                }
            )
            .await,
        Err(Error::Conflict)
    ));
    session
        .acknowledge_intent(&context(), "original_admission".into())
        .await
        .unwrap();
    let first = session
        .prepare_intent(&context(), intent("new_admission"))
        .await
        .unwrap();
    assert!(first.is_first_attempt());
    assert!(matches!(
        session
            .acknowledge_intent(&context(), "new_admission".into())
            .await,
        Err(Error::Conflict)
    ));
    journal.shutdown().await.unwrap();
    assert!(!journal.writer_open());
    drop(session);
    drop(journal);
    let journal = f.open();
    let session = journal.session(authority(), &"a".repeat(64)).unwrap();
    let retry = session
        .prepare_intent(&context(), intent("new_admission"))
        .await
        .unwrap();
    assert!(!retry.is_first_attempt() && retry.reply.is_none());
    session
        .record_intent_reply(&context(), intent("new_admission"), success())
        .await
        .unwrap();
    assert!(matches!(
        session
            .prepare_intent(&context(), intent("original_admission"))
            .await,
        Err(Error::Conflict)
    ));
    journal.shutdown().await.unwrap();
    let db = f.source();
    let after:(String,Option<String>,String)=db.query_row("SELECT digest,reply,state FROM management_intents WHERE command_id='original_admission'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(after, (before.0, None, "acknowledged".into()));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn archive_rejects_unfenced_wrong_manifest_profile_and_owner() {
    let f = Fixture::new().await;
    assert!(matches!(
        PrivateManagementJournal::open_existing(&f.path, binding()),
        Err(Error::Schema)
    ));
    f.fence();
    let mut wrong = binding();
    wrong.0.registry_id = "f".repeat(64);
    assert!(matches!(
        PrivateManagementJournal::open_existing(&f.path, wrong),
        Err(Error::LocalAuthority)
    ));
    let journal = f.open();
    assert!(matches!(
        journal.session(authority(), &"f".repeat(64)),
        Err(Error::LocalAuthority)
    ));
    let mut wrong = context();
    wrong.account = "@other:example.test".into();
    let wrong = ManagementAuthority::new(
        wrong,
        permissions(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(matches!(
        journal.session(wrong, &"a".repeat(64)),
        Err(Error::LocalAuthority)
    ));
    journal.shutdown().await.unwrap();
}

#[tokio::test]
async fn archive_requires_original_permission_and_exact_package_context() {
    let f = Fixture::new().await;
    f.fence();
    let journal = f.open();
    let limited = ManagementAuthority::new(
        context(),
        [Permission::ReadIntents, Permission::AcknowledgeIntents],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let session = journal.session(limited, &"a".repeat(64)).unwrap();
    assert!(
        session
            .pending_intents(&context())
            .await
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        session
            .acknowledge_intent(&context(), "original_admission".into())
            .await,
        Err(Error::LocalAuthority)
    ));
    let session = journal.session(authority(), &"a".repeat(64)).unwrap();
    let mut changed = context();
    changed.account_epoch += 1;
    assert!(matches!(
        session.pending_intents(&changed).await,
        Err(Error::LocalAuthority)
    ));
    let mut changed = context();
    changed.package_digest = "f".repeat(64);
    assert!(matches!(
        session.prepare_intent(&changed, intent("denied")).await,
        Err(Error::LocalAuthority)
    ));
    let mut changed = context();
    changed.device = "different_device".into();
    assert!(matches!(
        session.pending_intents(&changed).await,
        Err(Error::LocalAuthority)
    ));
    journal.shutdown().await.unwrap();
}

#[tokio::test]
async fn archive_retirement_while_queued_cannot_prepare_a_command() {
    let f = Fixture::new().await;
    f.fence();
    let journal = f.open();
    let authority = authority();
    let session = journal.session(authority.clone(), &"a".repeat(64)).unwrap();
    let (entered, rx) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    journal
        .tx
        .send(Job::Pause {
            entered,
            release: wait,
        })
        .await
        .unwrap();
    rx.await.unwrap();
    let pending = tokio::spawn(async move {
        session
            .prepare_intent(&context(), intent("queued_denied"))
            .await
    });
    while journal.tx.capacity() == 8 {
        tokio::task::yield_now().await;
    }
    authority.retire();
    release.send(()).unwrap();
    assert!(matches!(pending.await.unwrap(), Err(Error::LocalAuthority)));
    journal.shutdown().await.unwrap();
    assert_eq!(
        f.source()
            .query_row(
                "SELECT COUNT(*) FROM management_intents WHERE command_id='queued_denied'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn archive_sql_guard_denies_resource_writes_and_trigger_side_effects() {
    let f = Fixture::new().await;
    f.fence();
    {
        let archive = Archive::open(&f.path, &binding()).unwrap();
        assert!(archive.db.execute("DELETE FROM resources", []).is_err());
        assert!(
            archive
                .db
                .execute("UPDATE management_commands SET result='changed'", [])
                .is_err()
        );
        assert!(
            archive
                .db
                .execute_batch("CREATE TABLE attacker(x);ATTACH ':memory:' AS other;")
                .is_err()
        );
        assert!(
            archive
                .db
                .query_row("SELECT config FROM resources", [], |r| r
                    .get::<_, String>(0))
                .is_err()
        );
    }
    f.source().execute_batch("CREATE TRIGGER bad_custody_write AFTER INSERT ON management_intents BEGIN DELETE FROM resources;END;").unwrap();
    let journal = f.open();
    let session = journal.session(authority(), &"a".repeat(64)).unwrap();
    assert!(
        session
            .prepare_intent(&context(), intent("trigger_denied"))
            .await
            .is_err()
    );
    journal.shutdown().await.unwrap();
    assert_eq!(
        f.source()
            .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        f.source()
            .query_row(
                "SELECT COUNT(*) FROM management_intents WHERE command_id='trigger_denied'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn archive_retains_original_writer_lock_and_normal_openers_refuse() {
    let f = Fixture::new().await;
    f.fence();
    let files = || {
        ["domain.sqlite3", "domain.sqlite3-wal"]
            .map(|name| std::fs::read(f.path.join(name)).unwrap())
    };
    let before = files();
    assert!(matches!(
        DomainRepository::open_managed(&f.path, &"a".repeat(64)),
        Err(Error::Schema)
    ));
    assert!(matches!(
        DomainRepository::open_for_migration(&f.path),
        Err(Error::Schema)
    ));
    assert_eq!(
        files(),
        before,
        "rejected normal open must preserve archive WAL and main bytes"
    );
    let journal = f.open();
    assert!(matches!(
        PrivateManagementJournal::open_existing(&f.path, binding()),
        Err(Error::Locked)
    ));
    assert!(matches!(
        DomainRepository::open(&f.path),
        Err(Error::Locked)
    ));
    journal.shutdown().await.unwrap();
    let journal = f.open();
    journal.shutdown().await.unwrap();
}

#[tokio::test]
async fn archive_never_creates_missing_source_or_wal_companions() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().canonicalize().unwrap().join("absent");
    assert!(PrivateManagementJournal::open_existing(&path, binding()).is_err());
    assert!(!path.exists());
    let f = Fixture::new().await;
    f.fence();
    std::fs::remove_file(f.path.join("domain.sqlite3-shm")).unwrap();
    assert!(PrivateManagementJournal::open_existing(&f.path, binding()).is_err());
    assert!(!f.path.join("domain.sqlite3-shm").exists());
}

#[tokio::test]
async fn authority_retirement_is_shared_with_existing_resource_session() {
    let root = tempfile::tempdir().unwrap();
    let store = DomainStore::start(
        DomainRepository::open(&root.path().join("source")).unwrap(),
        8,
    )
    .unwrap();
    let session = Session::new(
        store.clone(),
        context(),
        [Permission::ReadResources],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let authority = session.authority();
    assert!(authority.is_live());
    authority.retire();
    assert!(!session.is_live());
    assert!(matches!(
        session
            .resources(&context(), ResourceQuery::default())
            .await,
        Err(Error::LocalAuthority)
    ));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn archive_deadline_expiry_while_queued_cannot_prepare_a_command() {
    let f = Fixture::new().await;
    f.fence();
    let journal = f.open();
    let expires = Instant::now() + Duration::from_millis(100);
    let authority = ManagementAuthority::new(context(), permissions(), expires).unwrap();
    let session = journal.session(authority, &"a".repeat(64)).unwrap();
    let (entered, rx) = oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    journal
        .tx
        .send(Job::Pause {
            entered,
            release: wait,
        })
        .await
        .unwrap();
    rx.await.unwrap();
    let pending = tokio::spawn(async move {
        session
            .prepare_intent(&context(), intent("expired_denied"))
            .await
    });
    tokio::time::sleep_until((expires + Duration::from_millis(2)).into()).await;
    release.send(()).unwrap();
    assert!(matches!(pending.await.unwrap(), Err(Error::LocalAuthority)));
    journal.shutdown().await.unwrap();
    assert_eq!(
        f.source()
            .query_row(
                "SELECT COUNT(*) FROM management_intents WHERE command_id='expired_denied'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn archive_recovers_an_original_native_receipt_without_resource_execution() {
    let f = Fixture::new().await;
    // Seed through the same original scope/digest and receipt writer used by
    // native effects; no provider, retry mutation or replacement identity runs.
    let input = json!({"commandId":"original_enrollment","modelId":"sol"});
    let original = Intent::new(
        IntentOperation::EnrollResource,
        "original_enrollment".into(),
        &input,
    )
    .unwrap();
    let mut db = f.source();
    execute_intent(
        &mut db,
        JournalCommand::new(
            authority(),
            context(),
            JournalAction::Prepare(original.clone()),
        ),
    )
    .unwrap();
    CommandReceipt::new(&context(), "resource.enroll", "original_enrollment", &input)
        .unwrap()
        .record(
            &db,
            &json!({"resourceId":"existing_resource","revision":"original_revision"}),
        )
        .unwrap();
    let before: String = db
        .query_row("SELECT config FROM resources", [], |r| r.get(0))
        .unwrap();
    drop(db);
    f.fence();
    let journal = f.open();
    let session = journal.session(authority(), &"a".repeat(64)).unwrap();
    let recovered = session.prepare_intent(&context(), original).await.unwrap();
    assert!(!recovered.is_first_attempt());
    assert_eq!(
        recovered.reply,
        Some(IntentReply::Success(
            json!({"resourceId":"existing_resource","revision":"original_revision"})
        ))
    );
    journal.shutdown().await.unwrap();
    assert_eq!(
        f.source()
            .query_row("SELECT config FROM resources", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        before
    );
    assert_eq!(
        f.source()
            .query_row(
                "SELECT COUNT(*) FROM management_commands WHERE command_id='original_enrollment'",
                [],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn archive_rejects_replaced_file_before_delivering_private_rows() {
    let f = Fixture::new().await;
    f.fence();
    let journal = f.open();
    let session = journal.session(authority(), &"a".repeat(64)).unwrap();
    let path = f.path.join("domain.sqlite3");
    let moved = f.path.join("moved.sqlite3");
    std::fs::rename(&path, &moved).unwrap();
    crate::private::open(&path, true).unwrap();
    assert!(matches!(
        session.pending_intents(&context()).await,
        Err(Error::Private)
    ));
    // Restore only to let the original fixture close; never substitute a new
    // database or treat the replacement as an empty source.
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(moved, path).unwrap();
    journal.shutdown().await.unwrap();
}

#[tokio::test]
async fn archive_validation_deadline_interrupts_existing_rows_without_mutation() {
    let f = Fixture::new().await;
    f.fence();
    let db = f.source();
    db.execute_batch("WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000) INSERT INTO management_commands SELECT printf('%064d',0),'bulk_'||i,'resource.enroll',printf('%064d',0),'{}' FROM n;").unwrap();
    drop(db);
    let files = || {
        ["domain.sqlite3", "domain.sqlite3-wal"]
            .map(|name| std::fs::read(f.path.join(name)).unwrap())
    };
    let before = files();
    let mut db = f.source();
    let (entered, observing) = std::sync::mpsc::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let task = std::thread::spawn(move || {
        // Start this deadline on the worker, after it has actually been
        // scheduled. A busy test host must not expire it before SQL starts.
        let deadline = Instant::now() + Duration::from_millis(100);
        let mut entered = Some(entered);
        db.authorizer(Some(move |action: AuthContext<'_>| {
            if matches!(action.action, AuthAction::Select)
                && let Some(entered) = entered.take()
            {
                let _ = entered.send(deadline);
                let _ = blocked.recv();
            }
            Authorization::Allow
        }))
        .unwrap();
        let changes = db.total_changes();
        let result = validate_bounded(&mut db, &binding(), deadline);
        assert!(db.is_autocommit());
        assert_eq!(db.total_changes(), changes);
        result
    });
    let deadline = observing.recv_timeout(Duration::from_secs(5)).unwrap();
    tokio::time::sleep_until((deadline + Duration::from_millis(2)).into()).await;
    release.send(()).unwrap();
    assert!(matches!(task.join().unwrap(), Err(Error::Busy)));
    assert_eq!(files(), before);
    let mut db = f.source();
    assert!(matches!(
        validate_bounded(&mut db, &binding(), Instant::now()),
        Err(Error::Busy)
    ));
}

#[tokio::test]
async fn archive_grant_publication_complete_history_restriction_is_native_only_and_fail_closed() {
    let f = Fixture::new().await;
    f.fence();
    let journal = f.open();
    let mut grants = permissions().to_vec();
    grants.push(Permission::PublishGrants);
    let authority =
        ManagementAuthority::new(context(), grants, Instant::now() + Duration::from_secs(60))
            .unwrap();
    let session = journal.session(authority, &"a".repeat(64)).unwrap();
    session
        .require_unused_grant_publication(&context(), "grant_original".into())
        .await
        .unwrap();
    let original=Intent::new(IntentOperation::PublishGrant,"publish_original".into(),&json!({"commandId":"publish_original","expectedContributionRevision":1,"receipt":{"grantId":"grant_original"}})).unwrap().require_unused_grant_publication();
    assert!(
        session
            .prepare_intent(&context(), original.clone())
            .await
            .unwrap()
            .is_first_attempt()
    );
    assert!(
        !session
            .prepare_intent(&context(), original.clone())
            .await
            .unwrap()
            .is_first_attempt()
    );
    assert!(matches!(
        session
            .require_unused_grant_publication(&context(), "grant_original".into())
            .await,
        Err(Error::Conflict)
    ));
    session
        .record_intent_reply(
            &context(),
            original,
            IntentReply::Failure {
                status: 409,
                code: "management_revision_changed".into(),
            },
        )
        .await
        .unwrap();
    session
        .acknowledge_intent(&context(), "publish_original".into())
        .await
        .unwrap();
    // The scrubbed row cannot establish which grant was used, so even another
    // grant's new owner handoff refuses to guess. Legacy explicit correction is
    // unchanged and cannot opt the owner handoff out of its native restriction.
    assert!(matches!(
        session
            .require_unused_grant_publication(&context(), "grant_other".into())
            .await,
        Err(Error::Conflict)
    ));
    let corrected=Intent::new(IntentOperation::PublishGrant,"publish_corrected".into(),&json!({"commandId":"publish_corrected","expectedContributionRevision":2,"receipt":{"grantId":"grant_original"}})).unwrap();
    assert!(matches!(
        session
            .prepare_intent(
                &context(),
                corrected.clone().require_unused_grant_publication()
            )
            .await,
        Err(Error::Conflict)
    ));
    assert!(
        session
            .prepare_intent(&context(), corrected)
            .await
            .unwrap()
            .is_first_attempt()
    );
    journal.shutdown().await.unwrap();
    assert_eq!(
        f.source()
            .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}
