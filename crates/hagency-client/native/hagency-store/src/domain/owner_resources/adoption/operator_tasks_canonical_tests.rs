use super::*;
use crate::management::{OperatorTaskDetailQuery, OperatorTaskQuery};
const DROP_TASK_SCHEMA: &str = "DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE owner_operator_task_comments;DROP TABLE owner_operator_tasks;DROP TABLE owner_operator_task_histories;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=83;";
async fn old_fixture() -> Fixture {
    let f = fixture().await;
    let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    rewind_pool_schema(&db);
    db.execute_batch(DROP_TASK_SCHEMA).unwrap();
    drop(db);
    f
}
fn tasks(backing: AdoptedManagementStore, f: &Fixture) -> Session {
    Session::new_adopted(
        backing,
        context(f),
        [Permission::ReadTasks, Permission::ReadTaskBoard],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn files(path: &Path) -> Vec<Option<Vec<u8>>> {
    ["domain.sqlite3", "domain.sqlite3-wal", "domain.sqlite3-shm"]
        .iter()
        .map(|name| std::fs::read(path.join(name)).ok())
        .collect()
}
async fn receipt(f: &Fixture) -> String {
    f.registry
        .0
        .store
        .call(1, |db| {
            Ok(db.db.query_row(
                "SELECT receipt FROM owner_operator_task_histories",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}
#[tokio::test]
async fn canonical_operator_tasks_original_active_v1_receipt_and_no_archive_read() {
    let f = old_fixture().await;
    let backing = active(&f).await;
    let c = context(&f);
    let s = tasks(backing.clone(), &f);
    let original = receipt(&f).await;
    let value: Value = serde_json::from_str(&original).unwrap();
    assert_eq!(value["kind"], "empty_resource_adoption_v1");
    assert_eq!(value["sourceSchema"], 83);
    assert_eq!(value["originalTasks"], 0);
    assert_eq!(value["originalComments"], 0);
    let before = files(&f.source);
    let canonical = files(&f._root.path().join("owner"));
    assert!(canonical[0].as_ref().is_some_and(|v| !v.is_empty()));
    assert!(canonical[1].as_ref().is_some_and(|v| v.len() > 32));
    // The public read must not need the former source pathname to exist.
    let moved = f._root.path().join("archived-source-not-at-original-path");
    std::fs::rename(&f.source, &moved).unwrap();
    let page = s.operator_task_page(&c, Default::default()).await.unwrap();
    assert!(page.supported && page.tasks.is_empty());
    let board = s.operator_board(&c, Default::default()).await.unwrap();
    assert!(board.supported);
    assert_eq!(board.project_count, 0);
    assert!(files(&moved) == before, "archived source bytes changed");
    assert!(
        files(&f._root.path().join("owner")) == canonical,
        "canonical database bytes changed"
    );
    std::fs::rename(&moved, &f.source).unwrap();
    s.retire();
    drop(s);
    backing.shutdown().await.unwrap();
    drop(backing);
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let reopened = transfer.adopted_management(journal).await.unwrap();
    assert_eq!(
        receipt(&f).await,
        original,
        "reopen recovers original provenance bytes"
    );
    let s = tasks(reopened.clone(), &f);
    assert!(
        s.operator_task_page(&c, Default::default())
            .await
            .unwrap()
            .supported
    );
    reopened.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn canonical_operator_tasks_newer_source_is_unavailable_without_receipt_creation() {
    let f = fixture().await;
    let backing = active(&f).await;
    let c = context(&f);
    let s = tasks(backing.clone(), &f);
    let page = s.operator_task_page(&c, Default::default()).await.unwrap();
    assert!(!page.supported);
    assert_eq!(page.reason, Some("task_history_not_migrated"));
    assert_eq!(
        f.registry
            .0
            .store
            .call(1, |db| Ok(db.db.query_row(
                "SELECT COUNT(*) FROM owner_operator_task_histories",
                [],
                |r| r.get::<_, i64>(0)
            )?))
            .await
            .unwrap(),
        0
    );
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn canonical_operator_tasks_exact_receipt_read_preserves_fields_comments_and_profile_scope() {
    let f = old_fixture().await;
    let backing = active(&f).await;
    let c = context(&f);
    let s = tasks(backing.clone(), &f);
    let profile = f.profile.digest.clone();
    f.registry.0.store.call(1,move|db|{
        db.db.execute("INSERT INTO owner_operator_tasks(profile,id,revision,title,description,status,priority,granularity,assignee,created_by,created_at,updated_at,started_at,waiting_reason,waiting_until,parent_id,labels) VALUES(?1,'task_original',1,'Original title','Original description','blocked','p1','subtask','Agent A','Historical author',1000,3000,2000,'Original reason','later','task_parent','[\"review\",\"标签\"]')",[&profile])?;
        for i in 0..3 {db.db.execute("INSERT INTO owner_operator_task_comments VALUES(?1,'task_original',?2,'Historical comment author',?3,?4)",params![profile,i,format!("Comment {i}"),4000+i])?;}
        for i in 0..17 {db.db.execute("INSERT INTO owner_operator_tasks(profile,id,revision,title,description,status,priority,granularity,created_at,updated_at,labels) VALUES(?1,?2,1,'Other original task','','created','p2','task',?3,?3,'[]')",params![profile,format!("task_{i:03}"),5000+i])?;}
        // Deliberately foreign fixture rows must not be admitted by an ID/name
        // match. They are not a claim of qualified migration for that profile.
        db.db.execute("INSERT INTO owner_resource_adoptions(id,profile,plan_digest,plan,phase) VALUES('foreign',?1,?2,'{}',0)",params!["b".repeat(64),"c".repeat(64)])?;
        db.db.execute("INSERT INTO owner_operator_task_histories VALUES(?1,'foreign',?2,'{}')",params!["b".repeat(64),"d".repeat(64)])?;
        db.db.execute("INSERT INTO owner_operator_tasks(profile,id,revision,title,description,status,priority,granularity,assignee,created_at,updated_at,labels) VALUES(?1,'task_original',1,'Private other profile','','done','p0','task','Agent A',1,1,'[]')",["b".repeat(64)])?;
        Ok(())
    }).await.unwrap();
    let before = files(&f.source);
    let page = s
        .operator_task_page(
            &c,
            OperatorTaskQuery {
                assignee: Some("Agent A".into()),
                label: Some("标签".into()),
                status: Some("blocked".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.tasks.len(), 1);
    assert_eq!(page.tasks[0].title, "Original title");
    let detail = s
        .operator_task_detail(
            &c,
            OperatorTaskDetailQuery {
                id: "task_original".into(),
                comment_offset: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(detail.next, ["in_progress"]);
    assert_eq!(detail.comment_count, 3);
    assert_eq!(detail.next_comment_offset, Some(2));
    let t = detail.task.unwrap();
    assert_eq!(t.description, "Original description");
    assert_eq!(t.created_by.as_deref(), Some("Historical author"));
    assert_eq!(t.waiting_reason.as_deref(), Some("Original reason"));
    assert_eq!(t.parent_id.as_deref(), Some("task_parent"));
    assert_eq!(
        t.comments
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        ["Comment 0", "Comment 1"]
    );
    let last = s
        .operator_task_detail(
            &c,
            OperatorTaskDetailQuery {
                id: t.id,
                comment_offset: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(last.task.unwrap().comments[0].text, "Comment 2");
    let board = s.operator_board(&c, Default::default()).await.unwrap();
    assert_eq!(board.task_totals["blocked"], 1);
    assert_eq!(board.task_totals["done"], 0);
    assert_eq!(board.task_totals["created"], 17);
    let first = s.operator_task_page(&c, Default::default()).await.unwrap();
    assert_eq!(first.tasks.len(), 16);
    assert_eq!(first.next_offset, Some(16));
    let last = s
        .operator_task_page(
            &c,
            OperatorTaskQuery {
                offset: 16,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(last.tasks.len(), 2);
    assert_eq!(last.next_offset, None);
    assert!(
        first
            .tasks
            .iter()
            .chain(last.tasks.iter())
            .all(|t| t.title != "Private other profile")
    );
    assert!(files(&f.source) == before, "archived source bytes changed");
    s.retire();
    assert!(matches!(
        s.operator_task_page(&c, Default::default()).await,
        Err(Error::LocalAuthority)
    ));
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn canonical_operator_tasks_corrupt_original_evidence_refuses_read_without_repair() {
    for column in ["source_evidence", "custody_evidence", "result"] {
        let f = old_fixture().await;
        let backing = active(&f).await;
        let c = context(&f);
        let s = tasks(backing.clone(), &f);
        let before = receipt(&f).await;
        f.registry
            .0
            .store
            .call(1, move |db| {
                db.db
                    .execute_batch("DROP TRIGGER owner_resource_adoptions_identity")?;
                db.db.execute(
                    &format!("UPDATE owner_resource_adoptions SET {column}=?1"),
                    [if column == "result" {
                        "{}".into()
                    } else {
                        "f".repeat(64)
                    }],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            s.operator_task_page(&c, Default::default()).await,
            Err(Error::Quarantined)
        ));
        assert_eq!(receipt(&f).await, before);
        backing.shutdown().await.unwrap();
        f.registry.shutdown().await.unwrap();
    }
}
#[test]
fn canonical_operator_tasks_actual_83_to_84_migration_is_atomic_and_does_not_infer_import() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let path = root.path().join("domain");
    drop(DomainRepository::open(&path).unwrap());
    let db = Connection::open(path.join("domain.sqlite3")).unwrap();
    rewind_pool_schema(&db);
    db.execute_batch(DROP_TASK_SCHEMA).unwrap();
    db.execute_batch("CREATE TABLE owner_operator_task_comments(collision INTEGER)")
        .unwrap();
    drop(db);
    assert!(DomainRepository::open(&path).is_err());
    let db = Connection::open(path.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        83
    );
    assert_eq!(db.query_row("SELECT COUNT(*) FROM sqlite_schema WHERE name IN ('owner_operator_tasks','owner_operator_task_histories')",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    db.execute_batch("DROP TABLE owner_operator_task_comments")
        .unwrap();
    drop(db);
    let db = DomainRepository::open(&path).unwrap();
    assert_eq!(
        db.db
            .pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        crate::DOMAIN_SCHEMA_VERSION
    );
    assert_eq!(
        db.db
            .query_row(
                "SELECT COUNT(*) FROM owner_operator_task_histories",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

async fn remove_fixture_history(f: &Fixture) {
    f.registry.0.store.call(1, |db| {
        db.db.execute_batch("DROP TRIGGER owner_operator_task_history_retained;DELETE FROM owner_operator_task_histories;CREATE TRIGGER owner_operator_task_history_retained BEFORE DELETE ON owner_operator_task_histories BEGIN SELECT RAISE(ABORT,'retain original operator task history provenance'); END;")?;
        Ok(())
    }).await.unwrap();
}
async fn no_history(f: &Fixture) -> bool {
    f.registry
        .0
        .store
        .call(1, |db| {
            Ok(db.db.query_row(
                "SELECT COUNT(*)=0 FROM owner_operator_task_histories",
                [],
                |r| r.get(0),
            )?)
        })
        .await
        .unwrap()
}
thread_local! {
    static SQLITE_WAIT: std::cell::RefCell<Option<tokio::sync::oneshot::Sender<()>>> = const { std::cell::RefCell::new(None) };
}
fn signal_sqlite_wait(attempt: i32) -> bool {
    SQLITE_WAIT.with(|signal| {
        if let Some(signal) = signal.borrow_mut().take() {
            let _ = signal.send(());
        }
    });
    std::thread::sleep(Duration::from_millis(2));
    attempt < 2500
}
#[tokio::test]
async fn canonical_operator_tasks_sqlite_wait_retirement_creates_no_receipt() {
    let f = old_fixture().await;
    let backing = active(&f).await;
    backing.shutdown().await.unwrap();
    drop(backing);
    remove_fixture_history(&f).await;
    let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let check = live.clone();
    let auth = AdoptionAuthority::new(
        f.registry.clone(),
        f.profile.clone(),
        Instant::now() + Duration::from_secs(30),
        move || check.load(Ordering::Acquire),
    )
    .unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(auth, "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let retained_journal = journal.clone();
    let (wait_tx, wait_rx) = tokio::sync::oneshot::channel();
    f.registry
        .0
        .store
        .call(1, move |db| {
            SQLITE_WAIT.with(|signal| *signal.borrow_mut() = Some(wait_tx));
            db.db.busy_handler(Some(signal_sqlite_wait))?;
            Ok(())
        })
        .await
        .unwrap();
    let lock = Connection::open(f._root.path().join("owner/domain.sqlite3")).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let attempt = tokio::spawn(async move { transfer.adopted_management(journal).await });
    tokio::time::timeout(Duration::from_secs(5), wait_rx)
        .await
        .unwrap()
        .unwrap();
    live.store(false, Ordering::Release);
    lock.execute_batch("ROLLBACK").unwrap();
    drop(lock);
    assert!(matches!(attempt.await.unwrap(), Err(Error::LocalAuthority)));
    assert!(no_history(&f).await);
    f.registry
        .0
        .store
        .call(1, |db| {
            db.db.busy_handler(None)?;
            Ok(())
        })
        .await
        .unwrap();
    retained_journal.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
async fn original_resource_rows(f: &Fixture) -> Vec<(String, Vec<Vec<String>>)> {
    f.registry
        .0
        .store
        .call(1, |db| {
            let mut result = Vec::new();
            for table in [
                "resources",
                "seats",
                "owner_resource_accounts",
                "owner_resource_grants",
                "owner_capacity_grants",
                "owner_resource_adoptions",
            ] {
                let mut stmt = db
                    .db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))?;
                let columns = stmt.column_count();
                let rows = stmt
                    .query_map([], |row| {
                        (0..columns)
                            .map(|i| Ok(format!("{:?}", row.get_ref(i)?)))
                            .collect::<rusqlite::Result<Vec<_>>>()
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                result.push((table.into(), rows));
            }
            Ok(result)
        })
        .await
        .unwrap()
}
#[tokio::test]
async fn canonical_operator_tasks_postcommit_retirement_recovers_exact_receipt() {
    let f = old_fixture().await;
    let backing = active(&f).await;
    backing.shutdown().await.unwrap();
    drop(backing);
    remove_fixture_history(&f).await;
    let original_rows = original_resource_rows(&f).await;
    // A separate real reader cannot see the new row before COMMIT. The gate
    // therefore retires exactly at the first post-commit authority check.
    let observer = Arc::new(std::sync::Mutex::new(
        Connection::open_with_flags(
            f._root.path().join("owner/domain.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
    ));
    let check = observer.clone();
    let auth = AdoptionAuthority::new(
        f.registry.clone(),
        f.profile.clone(),
        Instant::now() + Duration::from_secs(30),
        move || {
            check
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*)=0 FROM owner_operator_task_histories",
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap()
        },
    )
    .unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(auth, "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let retained_journal = journal.clone();
    assert!(matches!(
        transfer.adopted_management(journal).await,
        Err(Error::OutcomeUnknown)
    ));
    let original = receipt(&f).await;
    assert_eq!(original_resource_rows(&f).await, original_rows);
    retained_journal.shutdown().await.unwrap();
    drop(retained_journal);
    let fresh = f
        .registry
        .resource_adoption_transfer(authority(&f), "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, fresh.archive_binding().unwrap())
            .unwrap();
    let recovered = fresh.adopted_management(journal).await.unwrap();
    assert_eq!(receipt(&f).await, original);
    let s = tasks(recovered.clone(), &f);
    assert!(
        s.operator_task_page(&context(&f), Default::default())
            .await
            .unwrap()
            .supported
    );
    recovered.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn canonical_operator_tasks_missing_provenance_never_grants_read_or_infers_import() {
    let f = old_fixture().await;
    let backing = active(&f).await;
    let s = tasks(backing.clone(), &f);
    remove_fixture_history(&f).await;
    let before = files(&f._root.path().join("owner"));
    assert!(
        !s.operator_task_page(&context(&f), Default::default())
            .await
            .unwrap()
            .supported
    );
    // SQLite may update the SHM read-mark (offset104) after the fixture's
    // schema change. That is reader coordination, not a persisted data write.
    let after = files(&f._root.path().join("owner"));
    assert!(
        after[..2] == before[..2],
        "canonical main/WAL changed during missing-history read"
    );
    backing.shutdown().await.unwrap();
    drop(s);
    drop(backing);
    let profile = f.profile.digest.clone();
    f.registry.0.store.call(1,move|db|{
        // Simulate a damaged canonical file, not an authorized writer.
        db.db.execute_batch("PRAGMA foreign_keys=OFF")?;
        db.db.execute("INSERT INTO owner_operator_tasks(profile,id,revision,title,description,status,priority,granularity,created_at,updated_at,labels) VALUES(?1,'orphan',1,'Retain original','','created','p2','task',1,1,'[]')",[profile])?;
        db.db.execute_batch("PRAGMA foreign_keys=ON")?;Ok(())
    }).await.unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let retained = journal.clone();
    assert!(matches!(
        transfer.adopted_management(journal).await,
        Err(Error::Quarantined)
    ));
    assert!(no_history(&f).await);
    retained.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn canonical_operator_tasks_populated_source_history_blocks_new_adoption() {
    let f = fixture().await;
    let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    db.execute("INSERT INTO owner_resource_adoptions(id,profile,plan_digest,plan,phase) VALUES('prior',?1,?2,'{}',0)",params![f.profile.digest,"c".repeat(64)]).unwrap();
    db.execute(
        "INSERT INTO owner_operator_task_histories VALUES(?1,'prior',?2,'{}')",
        params![f.profile.digest, "d".repeat(64)],
    )
    .unwrap();
    assert!(matches!(inventory(&f), Err(Error::Quarantined)));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn canonical_operator_tasks_unknown_plan_contract_is_unavailable() {
    let f = old_fixture().await;
    let backing = active(&f).await;
    backing.shutdown().await.unwrap();
    drop(backing);
    remove_fixture_history(&f).await;
    f.registry.0.store.call(1, |db| {
        let raw:String=db.db.query_row("SELECT plan FROM owner_resource_adoptions WHERE id='name_transfer'",[],|r|r.get(0))?;
        let mut plan:Plan=serde_json::from_str(&raw)?;
        plan.version=4; // v3 now has an explicit alert-history contract.
        db.db.execute_batch("DROP TRIGGER owner_resource_adoptions_identity")?;
        db.db.execute("UPDATE owner_resource_adoptions SET plan=?1,plan_digest=?2 WHERE id='name_transfer'",params![serde_json::to_string(&plan)?,plan.digest()?])?;
        Ok(())
    }).await.unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let backing = transfer.adopted_management(journal).await.unwrap();
    let s = tasks(backing.clone(), &f);
    assert!(
        !s.operator_task_page(&context(&f), Default::default())
            .await
            .unwrap()
            .supported
    );
    assert!(no_history(&f).await);
    backing.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
