use super::*;
use crate::management::{
    CommentOperatorTask, OperatorTaskChange, OperatorTaskCommand, OperatorTaskDetailQuery,
};

fn captured(f: &Fixture) -> Result<AdoptionInventory, Error> {
    let mut d = Connection::open_with_flags(
        f.source.join("domain.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut p = Connection::open_with_flags(&f.personal, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    AdoptionInventory::inspect_with_operator_tasks(
        &mut d,
        &mut p,
        &f.profile.digest,
        &f.profile.account,
    )
}
fn populate(f: &Fixture) -> Vec<crate::OperatorTask> {
    let db = DomainRepository::open_managed(&f.source, &f.profile.digest).unwrap();
    for (i, status) in crate::TASK_STATUSES.iter().enumerate() {
        db.db.execute("INSERT INTO operator_tasks(id,title,description,status,priority,granularity,assignee,created_by,created_at,updated_at,started_at,completed_at,heartbeat_at,waiting_reason,waiting_until,parent_id,labels) VALUES(?1,?2,?3,?4,'p1','subtask','Historical assignee','Historical author',1000,3000,2000,2800,2500,'Waiting 原因','tomorrow','deleted_parent','[\"标签\",\"review\"]')",params![format!("task_{i}"),format!("Original {i} 标题"),"  Preserve whitespace\n本文  ",status]).unwrap();
    }
    // Non-contiguous global IDs and equal timestamps must not reorder comments.
    for (seq, task, body) in [
        (3, "task_0", "First 原文"),
        (7, "task_1", "Other task"),
        (11, "task_0", "Second"),
        (20, "task_0", "Third"),
    ] {
        db.db.execute("INSERT INTO operator_task_comments(sequence,task_id,author,body,created_at) VALUES(?1,?2,'Historical commenter',?3,4000)",params![seq,task,body]).unwrap();
    }
    (0..5)
        .map(|i| db.operator_task(&format!("task_{i}")).unwrap())
        .collect()
}
async fn prepare_tasks(f: &Fixture, id: &str) -> Result<AdoptionSummary, Error> {
    f.registry
        .prepare_resource_adoption(
            authority(f),
            id.into(),
            provider(),
            captured(f)?,
            "c".repeat(64),
            700,
        )
        .await
}
async fn transfer(f: &Fixture, id: &str) -> AdoptionTransfer {
    f.registry
        .resource_adoption_transfer(authority(f), id.into())
        .await
        .unwrap()
}
async fn import(f: &Fixture, t: &AdoptionTransfer) {
    fenced_connections(f, t);
    f.registry
        .record_adoption_sources(authority(f), archive_proof(f, t).unwrap())
        .await
        .unwrap();
    f.registry
        .import_resource_adoption(authority(f), archive_proof(f, t).unwrap())
        .await
        .unwrap();
}
async fn activate(f: &Fixture, t: &AdoptionTransfer) -> AdoptedManagementStore {
    let j =
        PrivateManagementJournal::open_existing(&f.source, t.archive_binding().unwrap()).unwrap();
    f.registry
        .record_adoption_custody(authority(f), t.verify_private_journal(&j).await.unwrap())
        .await
        .unwrap();
    f.registry
        .activate_resource_adoption(
            authority(f),
            t.verify_private_journal(&j).await.unwrap(),
            provider(),
        )
        .await
        .unwrap();
    t.adopted_management(j).await.unwrap()
}
fn task_session(b: AdoptedManagementStore, c: HostContext) -> Session {
    Session::new_adopted(
        b,
        c,
        [
            Permission::ReadTasks,
            Permission::ReadTaskBoard,
            Permission::CommentTasks,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
async fn counts(f: &Fixture) -> (u64, u64, u64, u64) {
    f.registry
        .0
        .store
        .call(1, |db| {
            Ok((
                db.db.query_row(
                    "SELECT COUNT(*) FROM owner_operator_task_transfer_tasks",
                    [],
                    |r| r.get(0),
                )?,
                db.db.query_row(
                    "SELECT COUNT(*) FROM owner_operator_task_transfer_comments",
                    [],
                    |r| r.get(0),
                )?,
                db.db
                    .query_row("SELECT COUNT(*) FROM owner_operator_tasks", [], |r| {
                        r.get(0)
                    })?,
                db.db.query_row(
                    "SELECT COUNT(*) FROM owner_operator_task_comments",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap()
}
fn source_files(f: &Fixture) -> Vec<Option<Vec<u8>>> {
    ["domain.sqlite3", "domain.sqlite3-wal"]
        .iter()
        .map(|n| std::fs::read(f.source.join(n)).ok())
        .collect()
}
#[tokio::test]
async fn task_transfer_v2_stages_exact_original_before_fence_and_v1_stays_closed() {
    let f = fixture().await;
    populate(&f);
    let before = source_files(&f);
    assert!(matches!(inventory(&f), Err(Error::Quarantined)));
    let summary = prepare_tasks(&f, "tasks_original").await.unwrap();
    assert_eq!(summary.phase, "prepared");
    assert_eq!(summary.transferred_task_count, Some(5));
    assert_eq!(summary.transferred_comment_count, Some(4));
    assert_eq!(counts(&f).await, (5, 4, 0, 0));
    assert!(source_files(&f) == before);
    let t = transfer(&f, "tasks_original").await;
    assert_eq!(t.plan.version, 2);
    assert_eq!(
        prepare_tasks(&f, "tasks_original").await.unwrap().phase,
        "prepared"
    );
    assert!(prepare_tasks(&f, "replacement").await.is_err());
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    d.execute(
        "UPDATE operator_tasks SET title='Changed after review' WHERE id='task_0'",
        [],
    )
    .unwrap();
    assert!(matches!(
        prepare_tasks(&f, "tasks_original").await,
        Err(Error::Conflict)
    ));
    let mut d = Connection::open_with_flags(
        f.source.join("domain.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut p = Connection::open_with_flags(&f.personal, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    assert!(
        t.inspect_source_state(&mut d, &mut p, &"c".repeat(64))
            .is_err()
    );
    assert_eq!(counts(&f).await, (5, 4, 0, 0));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn task_transfer_v2_restart_import_and_current_edits_use_canonical_rows_only() {
    let mut f = fixture().await;
    let originals = populate(&f);
    prepare_tasks(&f, "tasks_restart").await.unwrap();
    let t = transfer(&f, "tasks_restart").await;
    import(&f, &t).await;
    assert_eq!(counts(&f).await, (5, 4, 5, 4));
    assert_eq!(
        f.registry
            .import_resource_adoption(authority(&f), archive_proof(&f, &t).unwrap())
            .await
            .unwrap()
            .phase,
        "imported_inactive"
    );
    // Persisted staging/receipt survive closing the canonical worker before activation.
    drop(t);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    let t = transfer(&f, "tasks_restart").await;
    let b = activate(&f, &t).await;
    let c = context(&f);
    let s = task_session(b.clone(), c.clone());
    let before = source_files(&f);
    let moved = f._root.path().join("source-unavailable-for-task-reads");
    std::fs::rename(&f.source, &moved).unwrap();
    for original in originals {
        let mut result = s
            .operator_task_detail(
                &c,
                OperatorTaskDetailQuery {
                    id: original.id.clone(),
                    comment_offset: 0,
                },
            )
            .await
            .unwrap();
        assert_eq!(result.revision, Some(1));
        let mut actual = result.task.take().unwrap();
        if actual.comments.len() < original.comments.len() {
            let rest = s
                .operator_task_detail(
                    &c,
                    OperatorTaskDetailQuery {
                        id: original.id.clone(),
                        comment_offset: actual.comments.len(),
                    },
                )
                .await
                .unwrap();
            actual.comments.extend(rest.task.unwrap().comments);
        }
        assert_eq!(actual, original);
    }
    let board = s.operator_board(&c, Default::default()).await.unwrap();
    assert_eq!(board.project_count, 0);
    let input = OperatorTaskCommand {
        command_id: "comment_after_transfer".into(),
        task_id: "task_0".into(),
        change: OperatorTaskChange::Comment(CommentOperatorTask {
            id: "task_0".into(),
            expected_revision: 1,
            text: "Current authenticated comment".into(),
        }),
    };
    let first = s.apply_operator_task(&c, input.clone()).await.unwrap();
    assert_eq!(first.revision, 2);
    assert_eq!(s.apply_operator_task(&c, input).await.unwrap().revision, 2);
    let value = s
        .operator_task_detail(
            &c,
            OperatorTaskDetailQuery {
                id: "task_0".into(),
                comment_offset: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(value.task.unwrap().comments[1].author, f.profile.account);
    assert_eq!(counts(&f).await, (5, 4, 5, 5));
    std::fs::rename(&moved, &f.source).unwrap();
    assert!(
        source_files(&f) == before,
        "task read/write touched archive"
    );
    s.retire();
    drop(s);
    b.shutdown().await.unwrap();
    drop(b);
    let t = transfer(&f, "tasks_restart").await;
    let j =
        PrivateManagementJournal::open_existing(&f.source, t.archive_binding().unwrap()).unwrap();
    let b = t.adopted_management(j).await.unwrap();
    let s = task_session(b.clone(), c.clone());
    let value = s
        .operator_task_detail(
            &c,
            OperatorTaskDetailQuery {
                id: "task_0".into(),
                comment_offset: 2,
            },
        )
        .await
        .unwrap();
    assert_eq!(value.revision, Some(2));
    assert_eq!(value.task.unwrap().comments.len(), 2);
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn task_transfer_v2_archive_payload_is_never_reread_for_import() {
    let f = fixture().await;
    let originals = populate(&f);
    prepare_tasks(&f, "staged_only").await.unwrap();
    let t = transfer(&f, "staged_only").await;
    fenced_connections(&f, &t);
    let mut d = Connection::open_with_flags(
        f.source.join("domain.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut p = Connection::open_with_flags(&f.personal, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    // Deny actual payload columns, while allowing the existing exact schema,
    // count and permanent manifest verification on retained connections.
    d.authorizer(Some(|ctx: rusqlite::hooks::AuthContext<'_>| {
        match ctx.action {
            rusqlite::hooks::AuthAction::Read {
                table_name,
                column_name,
            } if ["operator_tasks", "operator_task_comments"].contains(&table_name)
                && !column_name.is_empty() =>
            {
                rusqlite::hooks::Authorization::Deny
            }
            _ => rusqlite::hooks::Authorization::Allow,
        }
    }))
    .unwrap();
    let proof = t.inspect_archives(&mut d, &mut p).unwrap();
    f.registry
        .record_adoption_sources(authority(&f), proof)
        .await
        .unwrap();
    let proof = t.inspect_archives(&mut d, &mut p).unwrap();
    f.registry
        .import_resource_adoption(authority(&f), proof)
        .await
        .unwrap();
    let b = activate(&f, &t).await;
    let c = context(&f);
    let s = task_session(b.clone(), c.clone());
    assert_eq!(
        s.operator_task_detail(
            &c,
            OperatorTaskDetailQuery {
                id: "task_3".into(),
                comment_offset: 0
            }
        )
        .await
        .unwrap()
        .task
        .unwrap(),
        originals[3]
    );
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn task_transfer_v2_preparation_loss_recovers_original_staging_without_source_fence() {
    let mut f = fixture().await;
    populate(&f);
    let before = source_files(&f);
    let check = Arc::new(std::sync::Mutex::new(
        Connection::open_with_flags(
            f._root.path().join("owner/domain.sqlite3"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
    ));
    let gate = check.clone();
    let a = AdoptionAuthority::new(
        f.registry.clone(),
        f.profile.clone(),
        Instant::now() + Duration::from_secs(30),
        move || {
            gate.lock()
                .unwrap()
                .query_row("SELECT COUNT(*)=0 FROM owner_resource_adoptions", [], |r| {
                    r.get::<_, bool>(0)
                })
                .unwrap()
        },
    )
    .unwrap();
    assert!(matches!(
        f.registry
            .prepare_resource_adoption(
                a,
                "lost_reply".into(),
                provider(),
                captured(&f).unwrap(),
                "c".repeat(64),
                700
            )
            .await,
        Err(Error::OutcomeUnknown)
    ));
    assert_eq!(counts(&f).await, (5, 4, 0, 0));
    assert!(source_files(&f) == before);
    drop(check);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    assert_eq!(
        f.registry
            .resource_adoption(authority(&f), "lost_reply".into())
            .await
            .unwrap()
            .phase,
        "prepared"
    );
    assert_eq!(
        prepare_tasks(&f, "lost_reply").await.unwrap().phase,
        "prepared"
    );
    assert_eq!(counts(&f).await, (5, 4, 0, 0));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn task_transfer_v2_missing_stage_and_destination_collision_fail_atomically() {
    for corrupt in [false, true] {
        let f = fixture().await;
        populate(&f);
        prepare_tasks(&f, "conflict").await.unwrap();
        let t = transfer(&f, "conflict").await;
        fenced_connections(&f, &t);
        f.registry
            .record_adoption_sources(authority(&f), archive_proof(&f, &t).unwrap())
            .await
            .unwrap();
        let profile = f.profile.digest.clone();
        f.registry.0.store.call(1,move|db|{
            if corrupt{db.db.execute_batch("DROP TRIGGER owner_operator_task_transfer_comment_retained;DELETE FROM owner_operator_task_transfer_comments WHERE sequence=11;")?;}
            else{db.db.execute("INSERT INTO owner_operator_task_histories VALUES(?1,'conflict',?2,'{}')",params![profile,"e".repeat(64)])?;}
            Ok(())
        }).await.unwrap();
        assert!(
            f.registry
                .import_resource_adoption(authority(&f), archive_proof(&f, &t).unwrap())
                .await
                .is_err()
        );
        let v = f
            .registry
            .0
            .store
            .call(1, |db| {
                Ok((
                    db.db
                        .query_row("SELECT phase FROM owner_resource_adoptions", [], |r| {
                            r.get::<_, i64>(0)
                        })?,
                    db.db
                        .query_row("SELECT COUNT(*) FROM resources", [], |r| r.get::<_, u64>(0))?,
                ))
            })
            .await
            .unwrap();
        assert_eq!(v, (1, 0));
        assert_eq!(counts(&f).await.2, 0);
        f.registry.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn task_transfer_v2_unsupported_history_malformed_rows_and_bounds_do_not_truncate() {
    for sql in [
        "PRAGMA foreign_keys=OFF;INSERT INTO projects VALUES('legacy','project',1,'!room:test','@owner:a.test','!owner:test')",
        "UPDATE operator_tasks SET labels='{}' WHERE id='task_0'",
        "UPDATE operator_tasks SET assignee=printf('%0600d',0) WHERE id='task_0'",
        "UPDATE operator_tasks SET created_at=-1 WHERE id='task_0'",
        "PRAGMA foreign_keys=OFF;INSERT INTO operator_task_comments VALUES(99,'missing','Old author','Orphan',0)",
        "WITH RECURSIVE n(v) AS(SELECT 100 UNION ALL SELECT v+1 FROM n WHERE v<200) INSERT INTO operator_task_comments SELECT v,'task_0','Old author','Too many',0 FROM n",
    ] {
        let f = fixture().await;
        populate(&f);
        let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
        // Exact schemas for unsupported populated families are verified below;
        // these are source fixture rows, never a production raw writer.
        db.execute_batch(sql).unwrap();
        drop(db);
        assert!(captured(&f).is_err(), "unexpectedly accepted {sql}");
        assert_eq!(counts(&f).await, (0, 0, 0, 0));
        f.registry.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn task_transfer_v1_serialization_and_receipts_remain_unchanged() {
    let f = fixture().await;
    prepare(&f, "legacy_empty").await.unwrap();
    let t = transfer(&f, "legacy_empty").await;
    let raw = serde_json::to_value(&t.plan).unwrap();
    assert!(raw["inventory"].get("taskHistory").is_none());
    assert_eq!(raw["version"], 1);
    let parsed: Plan = serde_json::from_value(raw.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), raw);
    let summary = serde_json::to_value(t.plan.summary(0).unwrap()).unwrap();
    assert!(summary.get("transferredTaskCount").is_none());
    assert!(summary.get("transferredCommentCount").is_none());
    assert_eq!(counts(&f).await, (0, 0, 0, 0));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn task_transfer_v2_queued_retirement_and_failed_staging_leave_no_plan() {
    for retire in [false, true] {
        let f = fixture().await;
        populate(&f);
        let before = source_files(&f);
        let (entered, wait) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let store = f.registry.0.store.clone();
        let blocker = tokio::spawn(async move {
            store.call(1,move|db|{
            if !retire{db.db.execute_batch("CREATE TRIGGER test_reject_stage BEFORE INSERT ON owner_operator_task_transfer_comments BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;")?;}
            entered.send(()).unwrap();blocked.recv().unwrap();Ok(())
        }).await
        });
        wait.await.unwrap();
        let live = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = live.clone();
        let a = AdoptionAuthority::new(
            f.registry.clone(),
            f.profile.clone(),
            Instant::now() + Duration::from_secs(30),
            move || gate.load(Ordering::Acquire),
        )
        .unwrap();
        let operation = f.registry.prepare_resource_adoption(
            a,
            "queued".into(),
            provider(),
            captured(&f).unwrap(),
            "c".repeat(64),
            700,
        );
        tokio::pin!(operation);
        tokio::select! {biased;r=&mut operation=>panic!("unexpected early result {r:?}"),_=tokio::task::yield_now()=>{}}
        if retire {
            live.store(false, Ordering::Release);
        }
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        assert!(operation.await.is_err());
        assert_eq!(counts(&f).await, (0, 0, 0, 0));
        assert_eq!(
            f.registry
                .0
                .store
                .call(1, |db| Ok(db.db.query_row(
                    "SELECT COUNT(*) FROM owner_resource_adoptions",
                    [],
                    |r| r.get::<_, u64>(0)
                )?))
                .await
                .unwrap(),
            0
        );
        assert!(source_files(&f) == before);
        f.registry.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn task_transfer_v2_total_bytes_and_row_limits_reject_before_plan() {
    for sql in [
        "WITH RECURSIVE n(v) AS(SELECT 10 UNION ALL SELECT v+1 FROM n WHERE v<1110) INSERT INTO operator_tasks(id,title,description,status,priority,granularity,created_at,updated_at) SELECT 'bulk_'||v,'Large',printf('%04096d',0),'created','p2','task',0,0 FROM n",
        "WITH RECURSIVE n(v) AS(SELECT 10 UNION ALL SELECT v+1 FROM n WHERE v<10010) INSERT INTO operator_tasks(id,title,status,priority,granularity,created_at,updated_at) SELECT 'bulk_'||v,'Many','created','p2','task',0,0 FROM n",
    ] {
        let f = fixture().await;
        populate(&f);
        let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
        db.execute_batch(sql).unwrap();
        drop(db);
        assert!(captured(&f).is_err());
        assert_eq!(counts(&f).await, (0, 0, 0, 0));
        f.registry.shutdown().await.unwrap();
    }
}
#[tokio::test]
async fn task_transfer_v2_missing_stage_cannot_recover_a_fencing_handle() {
    let f = fixture().await;
    populate(&f);
    prepare_tasks(&f, "missing_stage").await.unwrap();
    f.registry.0.store.call(1,|db|{db.db.execute_batch("DROP TRIGGER owner_operator_task_transfer_comment_retained;DELETE FROM owner_operator_task_transfer_comments WHERE sequence=3;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .resource_adoption_transfer(authority(&f), "missing_stage".into())
            .await
            .is_err()
    );
    assert!(
        f.registry
            .resource_adoption(authority(&f), "missing_stage".into())
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn task_transfer_v2_migration_87_to_88_is_atomic_and_does_not_import() {
    let mut f = fixture().await;
    let original_id = f.registry.id().to_owned();
    let root = f._root.path().join("owner");
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    rewind_pool_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE owner_operator_task_transfer_comments;DROP TABLE owner_operator_task_transfer_tasks;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=87;CREATE TABLE owner_operator_task_transfer_comments(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        87
    );
    assert!(!db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='owner_operator_task_transfer_tasks')",[],|r|r.get::<_,bool>(0)).unwrap());
    rewind_pool_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE owner_operator_task_transfer_comments;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(f.registry.id(), original_id);
    assert_eq!(counts(&f).await, (0, 0, 0, 0));
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db
                    .pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))?,
                crate::DOMAIN_SCHEMA_VERSION
            );
            assert_eq!(
                db.db
                    .query_row("SELECT COUNT(*) FROM owner_resource_adoptions", [], |r| r
                        .get::<_, u64>(
                        0
                    ))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn task_transfer_v2_staged_history_is_retained_and_blocks_adoption_of_another_source() {
    for sql in [
        "INSERT INTO owner_operator_task_transfer_tasks VALUES('original','task',?1,'{}')",
        "INSERT INTO owner_operator_task_transfer_comments VALUES('original',1,'task',0,?1,'{}')",
    ] {
        let f = fixture().await;
        let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        db.execute(sql, ["a".repeat(64)]).unwrap();
        drop(db);
        assert!(matches!(inventory(&f), Err(Error::Quarantined)));
        assert!(matches!(captured(&f), Err(Error::Quarantined)));
        f.registry.shutdown().await.unwrap();
    }
    let f = fixture().await;
    populate(&f);
    prepare_tasks(&f, "retained_staging").await.unwrap();
    f.registry
        .0
        .store
        .call(1, |db| {
            for sql in [
                "UPDATE owner_operator_task_transfer_tasks SET payload='{}'",
                "DELETE FROM owner_operator_task_transfer_tasks",
                "UPDATE owner_operator_task_transfer_comments SET sequence=2",
                "DELETE FROM owner_operator_task_transfer_comments",
            ] {
                assert!(db.db.execute_batch(sql).is_err());
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(counts(&f).await, (5, 4, 0, 0));
    f.registry.shutdown().await.unwrap();
}
