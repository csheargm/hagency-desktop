use super::*;
use crate::management::{
    CommentOperatorTask, CreateOperatorTask, EditOperatorTask, Intent, IntentReply,
    OperatorTaskChange as Change, OperatorTaskCommand as Command, OperatorTaskDetailQuery,
    OperatorTaskVersion as Version, TaskField, TransitionOperatorTask,
};
async fn setup() -> (Fixture, AdoptedManagementStore, Session, HostContext) {
    let f = fixture().await;
    let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    rewind_pool_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE owner_operator_task_comments;DROP TABLE owner_operator_tasks;DROP TABLE owner_operator_task_histories;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=83;").unwrap();
    drop(db);
    let b = active(&f).await;
    let c = context(&f);
    let s = writer(b.clone(), c.clone());
    (f, b, s, c)
}
fn writer(b: AdoptedManagementStore, c: HostContext) -> Session {
    Session::new_adopted(
        b,
        c,
        [
            Permission::ReadTasks,
            Permission::CreateTasks,
            Permission::EditTasks,
            Permission::DeleteTasks,
            Permission::AcceptTasks,
            Permission::TransitionTasks,
            Permission::CommentTasks,
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn create(command: &str, id: &str) -> Command {
    Command {
        command_id: command.into(),
        task_id: id.into(),
        change: Change::Create(CreateOperatorTask {
            title: " Original task ".into(),
            ..Default::default()
        }),
    }
}
fn command(command: &str, change: Change) -> Command {
    Command {
        command_id: command.into(),
        task_id: change.task_id().unwrap().into(),
        change,
    }
}
fn version(id: &str, revision: u64) -> Version {
    Version {
        id: id.into(),
        expected_revision: revision,
    }
}
async fn detail(s: &Session, c: &HostContext, id: &str) -> crate::management::OperatorTaskDetail {
    s.operator_task_detail(
        c,
        OperatorTaskDetailQuery {
            id: id.into(),
            comment_offset: 0,
        },
    )
    .await
    .unwrap()
}
async fn rows(f: &Fixture) -> (u64, u64, u64) {
    f.registry
        .0
        .store
        .call(1, |db| {
            Ok((
                db.db
                    .query_row("SELECT COUNT(*) FROM owner_operator_tasks", [], |r| {
                        r.get(0)
                    })?,
                db.db.query_row(
                    "SELECT COUNT(*) FROM owner_operator_task_comments",
                    [],
                    |r| r.get(0),
                )?,
                db.db.query_row(
                    "SELECT COUNT(*) FROM management_commands WHERE operation LIKE 'task.%'",
                    [],
                    |r| r.get(0),
                )?,
            ))
        })
        .await
        .unwrap()
}
async fn close(f: Fixture, b: AdoptedManagementStore) {
    b.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn task_writer_original_lifecycle_and_authenticated_authors() {
    let (f, b, s, c) = setup().await;
    let mut first = create("create", "task_1");
    let Change::Create(v) = &mut first.change else {
        unreachable!()
    };
    v.description = Some(" Details ".into());
    v.assignee = Some(" Agent A ".into());
    v.labels = vec![" review ".into(), "review".into(), "".into(), "中文".into()];
    let result = s.apply_operator_task(&c, first.clone()).await.unwrap();
    assert_eq!(result.revision, 1);
    let d = detail(&s, &c, "task_1").await;
    assert!(d.write_supported);
    assert_eq!(d.revision, Some(1));
    let t = d.task.unwrap();
    assert_eq!(t.created_by, Some(f.profile.account.clone()));
    assert_eq!(t.assignee.as_deref(), Some("Agent A"));
    assert_eq!(t.labels, ["review", "中文"]);
    for (i, status) in ["accepted", "in_progress", "blocked", "in_progress", "done"]
        .into_iter()
        .enumerate()
    {
        let revision = i as u64 + 1;
        let change = if i == 0 {
            Change::Accept(version("task_1", revision))
        } else {
            Change::Transition(TransitionOperatorTask {
                id: "task_1".into(),
                expected_revision: revision,
                status: status.into(),
                waiting_reason: Some(" Need review ".into()),
                waiting_until: Some(" tomorrow ".into()),
            })
        };
        s.apply_operator_task(&c, command(&format!("transition{i}"), change))
            .await
            .unwrap();
        let t = detail(&s, &c, "task_1").await.task.unwrap();
        assert_eq!(t.status, status);
        assert!(t.started_at.is_some());
        if status == "blocked" {
            assert_eq!(t.waiting_reason.as_deref(), Some("Need review"));
        } else {
            assert!(t.waiting_reason.is_none() && t.waiting_until.is_none());
        }
    }
    let finished = detail(&s, &c, "task_1").await.task.unwrap();
    assert!(finished.completed_at.is_some());
    assert!(
        s.apply_operator_task(
            &c,
            command("accept_again", Change::Accept(version("task_1", 6)))
        )
        .await
        .is_err()
    );
    assert_eq!(s.apply_operator_task(&c, first).await.unwrap(), result);
    let comment = command(
        "comment",
        Change::Comment(CommentOperatorTask {
            id: "task_1".into(),
            expected_revision: 6,
            text: " Human comment ".into(),
        }),
    );
    let saved = s.apply_operator_task(&c, comment.clone()).await.unwrap();
    assert_eq!(s.apply_operator_task(&c, comment).await.unwrap(), saved);
    let d = detail(&s, &c, "task_1").await;
    assert_eq!(d.comment_count, 1);
    assert_eq!(d.task.unwrap().comments[0].author, f.profile.account);
    assert_eq!(rows(&f).await, (1, 1, 7));
    close(f, b).await;
}
#[tokio::test]
async fn task_writer_edit_clear_noop_parent_and_delete_semantics() {
    let (f, b, s, c) = setup().await;
    s.apply_operator_task(&c, create("create", "task_1"))
        .await
        .unwrap();
    let patch:EditOperatorTask=serde_json::from_value(json!({"id":"task_1","expectedRevision":1,"description":" Details ","assignee":"Agent A","parentId":"task_1","labels":["one","two"]})).unwrap();
    s.apply_operator_task(&c, command("edit", Change::Edit(patch)))
        .await
        .unwrap();
    let patch:EditOperatorTask=serde_json::from_value(json!({"id":"task_1","expectedRevision":2,"title":"   ","description":null,"assignee":null,"labels":null})).unwrap();
    s.apply_operator_task(&c, command("clear", Change::Edit(patch)))
        .await
        .unwrap();
    let d = detail(&s, &c, "task_1").await;
    assert_eq!(d.revision, Some(3));
    let t = d.task.unwrap();
    assert_eq!(t.title, "Original task");
    assert!(t.description.is_empty() && t.assignee.is_none() && t.labels.is_empty());
    assert_eq!(t.parent_id.as_deref(), Some("task_1"));
    let noop = command(
        "noop",
        Change::Edit(EditOperatorTask {
            id: "task_1".into(),
            expected_revision: 3,
            ..Default::default()
        }),
    );
    let n = s.apply_operator_task(&c, noop).await.unwrap();
    assert_eq!(n.revision, 3);
    let mut child = create("child", "task_child");
    if let Change::Create(v) = &mut child.change {
        v.parent_id = Some("task_1".into())
    }
    s.apply_operator_task(&c, child).await.unwrap();
    let delete = command("delete", Change::Delete(version("task_1", 3)));
    let receipt = s.apply_operator_task(&c, delete.clone()).await.unwrap();
    assert!(receipt.deleted);
    assert_eq!(s.apply_operator_task(&c, delete).await.unwrap(), receipt);
    assert_eq!(
        detail(&s, &c, "task_child")
            .await
            .task
            .unwrap()
            .parent_id
            .as_deref(),
        Some("task_1")
    );
    assert_eq!(rows(&f).await, (1, 0, 6));
    close(f, b).await;
}
#[tokio::test]
async fn task_writer_cas_receipt_collision_and_atomic_rollback() {
    let (f, b, s, c) = setup().await;
    s.apply_operator_task(&c, create("create", "task_1"))
        .await
        .unwrap();
    let other = writer(b.clone(), c.clone());
    let one = command(
        "one",
        Change::Comment(CommentOperatorTask {
            id: "task_1".into(),
            expected_revision: 1,
            text: "one".into(),
        }),
    );
    let two = command(
        "two",
        Change::Comment(CommentOperatorTask {
            id: "task_1".into(),
            expected_revision: 1,
            text: "two".into(),
        }),
    );
    let (a, z) = tokio::join!(
        s.apply_operator_task(&c, one.clone()),
        other.apply_operator_task(&c, two)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(z.is_ok()), 1);
    assert!(
        matches!(a, Err(Error::OperatorTaskRevisionChanged))
            || matches!(z, Err(Error::OperatorTaskRevisionChanged))
    );
    let mut collision = create("create", "task_2");
    assert!(matches!(
        s.apply_operator_task(&c, collision.clone()).await,
        Err(Error::Conflict)
    ));
    collision.command_id = "create2".into();
    s.apply_operator_task(&c, collision).await.unwrap();
    f.registry.0.store.call(1,|db|{db.db.execute_batch("CREATE TRIGGER fail_task_receipt BEFORE INSERT ON management_commands WHEN NEW.operation LIKE 'task.%' BEGIN SELECT RAISE(ABORT,'fixture rollback'); END;")?;Ok(())}).await.unwrap();
    let before = rows(&f).await;
    let failed = command(
        "rollback",
        Change::Comment(CommentOperatorTask {
            id: "task_2".into(),
            expected_revision: 1,
            text: "must rollback".into(),
        }),
    );
    assert!(s.apply_operator_task(&c, failed.clone()).await.is_err());
    assert_eq!(rows(&f).await, before);
    assert_eq!(detail(&s, &c, "task_2").await.revision, Some(1));
    f.registry
        .0
        .store
        .call(1, |db| {
            db.db.execute_batch("DROP TRIGGER fail_task_receipt")?;
            Ok(())
        })
        .await
        .unwrap();
    s.apply_operator_task(&c, failed).await.unwrap();
    close(f, b).await;
}
#[tokio::test]
async fn task_writer_no_permission_wrong_context_and_queued_retirement() {
    let (f, b, s, c) = setup().await;
    let read = Session::new_adopted(
        b.clone(),
        c.clone(),
        [Permission::ReadTasks],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    assert!(
        !read
            .operator_task_page(&c, Default::default())
            .await
            .unwrap()
            .write_supported
    );
    assert!(matches!(
        read.apply_operator_task(&c, create("denied", "task_1"))
            .await,
        Err(Error::LocalAuthority)
    ));
    let other = HostContext::new(
        "@other:test".into(),
        1,
        "a".repeat(64),
        1,
        "name_device".into(),
        1,
    )
    .unwrap();
    assert!(
        s.apply_operator_task(&other, create("other", "task_1"))
            .await
            .is_err()
    );
    let (entered, wait) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let store = f.registry.0.store.clone();
    let blocker = tokio::spawn(async move {
        store
            .call(1, move |_| {
                entered.send(()).unwrap();
                blocked.recv().unwrap();
                Ok(())
            })
            .await
    });
    wait.await.unwrap();
    let request = s.apply_operator_task(&c, create("retired", "task_1"));
    tokio::pin!(request);
    tokio::select! {biased;r=&mut request=>panic!("unexpected result {r:?}"),_=tokio::task::yield_now()=>{}}
    s.retire();
    release.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert!(request.await.is_err());
    assert_eq!(rows(&f).await, (0, 0, 0));
    close(f, b).await;
}
#[tokio::test]
async fn task_writer_postcommit_loss_original_journal_reopen_and_recovery() {
    let (mut f, b, s, c) = setup().await;
    let input = create("original_create", "task_original");
    let intent = Intent::new(input.change.operation(), input.command_id.clone(), &input).unwrap();
    s.prepare_intent(&c, intent.clone()).await.unwrap();
    let db = Arc::new(std::sync::Mutex::new(
        Connection::open_with_flags(
            f._root.path().join("owner/domain.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap(),
    ));
    let gate = db.clone();
    let profile = f
        .profile
        .clone()
        .with_native_gate(move || {
            gate.lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*)=0 FROM management_commands WHERE command_id='original_create'",
                    [],
                    |r| r.get::<_, bool>(0),
                )
                .unwrap()
        })
        .unwrap();
    let receipt =
        crate::management::operator_task_commands::command_receipt(&c, &f.profile.digest, &input)
            .unwrap();
    assert!(matches!(
        b.apply_operator_task(&profile, input.clone(), f.profile.account.clone(), receipt)
            .await,
        Err(Error::OutcomeUnknown)
    ));
    assert_eq!(rows(&f).await, (1, 0, 1));
    assert!(s.pending_intents(&c).await.unwrap()[0].reply.is_none());
    s.retire();
    drop(s);
    b.shutdown().await.unwrap();
    drop(b);
    drop(db);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._root.path().join("owner")).unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "name_transfer".into())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let b = transfer.adopted_management(journal).await.unwrap();
    let s = writer(b.clone(), c.clone());
    let original = s.pending_intents(&c).await.unwrap().remove(0);
    let recovered: Command = serde_json::from_value(original.input).unwrap();
    assert_eq!(recovered.task_id, "task_original");
    assert_eq!(recovered.command_id, "original_create");
    let result = s.apply_operator_task(&c, recovered).await.unwrap();
    s.record_intent_reply(
        &c,
        intent,
        IntentReply::Success(serde_json::to_value(&result).unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(rows(&f).await, (1, 0, 1));
    assert_eq!(result.revision, 1);
    close(f, b).await;
}
#[test]
fn task_writer_closed_payloads_and_patch_omission() {
    for field in [
        "author",
        "createdBy",
        "status",
        "commandId",
        "taskId",
        "profile",
        "path",
        "createdAt",
    ] {
        let mut v = json!({"title":"Task"});
        v[field] = json!("forged");
        assert!(
            serde_json::from_value::<CreateOperatorTask>(v).is_err(),
            "{field}"
        );
    }
    let p: EditOperatorTask =
        serde_json::from_value(json!({"id":"task","expectedRevision":1,"assignee":null})).unwrap();
    assert_eq!(p.title, TaskField::Unchanged);
    assert_eq!(p.assignee, TaskField::Set(None));
    assert!(serde_json::to_value(p).unwrap().get("title").is_none());
    assert!(
        serde_json::from_value::<CommentOperatorTask>(
            json!({"id":"task","expectedRevision":1,"text":"hi","author":"other"})
        )
        .is_err()
    );
    for status in ["accepted", "blocked", "done"] {
        let c = Change::Transition(TransitionOperatorTask {
            id: "task".into(),
            expected_revision: 1,
            status: status.into(),
            waiting_reason: None,
            waiting_until: None,
        });
        assert_eq!(c.validate().is_ok(), status != "blocked");
    }
}

#[tokio::test]
async fn task_writer_comment_bound_and_delete_comment_cascade() {
    let (f, b, s, c) = setup().await;
    s.apply_operator_task(&c, create("create", "task_1"))
        .await
        .unwrap();
    let profile = f.profile.digest.clone();
    f.registry.0.store.call(1,move|db| {
        for n in 0..99 { db.db.execute("INSERT INTO owner_operator_task_comments VALUES(?1,'task_1',?2,'Original author','Retained original comment',1)",params![profile,n])?; }
        Ok(())
    }).await.unwrap();
    let last = command(
        "last",
        Change::Comment(CommentOperatorTask {
            id: "task_1".into(),
            expected_revision: 1,
            text: "100th".into(),
        }),
    );
    assert_eq!(
        s.apply_operator_task(&c, last.clone())
            .await
            .unwrap()
            .comment_count,
        100
    );
    assert_eq!(
        s.apply_operator_task(&c, last).await.unwrap().comment_count,
        100
    );
    assert!(matches!(
        s.apply_operator_task(
            &c,
            command(
                "excess",
                Change::Comment(CommentOperatorTask {
                    id: "task_1".into(),
                    expected_revision: 2,
                    text: "101st".into()
                })
            )
        )
        .await,
        Err(Error::Capacity)
    ));
    assert_eq!(rows(&f).await, (1, 100, 2));
    s.apply_operator_task(&c, command("delete", Change::Delete(version("task_1", 2))))
        .await
        .unwrap();
    assert_eq!(rows(&f).await, (0, 0, 3));
    close(f, b).await;
}

#[tokio::test]
async fn task_writer_missing_proof_and_foreign_profile_never_write() {
    let f = fixture().await;
    let b = active(&f).await;
    let c = context(&f);
    let s = writer(b.clone(), c.clone());
    assert!(!s.operator_task_write_access(&c).await.unwrap());
    assert!(
        s.apply_operator_task(&c, create("unproven", "task_1"))
            .await
            .is_err()
    );
    assert_eq!(rows(&f).await, (0, 0, 0));
    close(f, b).await;
    let (f, b, s, c) = setup().await;
    let foreign = ProfileAuthority::new(
        "b".repeat(64),
        f.profile.account.clone(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap();
    let input = create("foreign", "task_1");
    let receipt =
        crate::management::operator_task_commands::command_receipt(&c, &foreign.digest, &input)
            .unwrap();
    assert!(
        b.apply_operator_task(&foreign, input, f.profile.account.clone(), receipt)
            .await
            .is_err()
    );
    assert_eq!(rows(&f).await, (0, 0, 0));
    let invalid = command(
        "invalid",
        Change::Transition(TransitionOperatorTask {
            id: "task_1".into(),
            expected_revision: 1,
            status: "blocked".into(),
            waiting_reason: Some("need review".into()),
            waiting_until: None,
        }),
    );
    assert!(s.apply_operator_task(&c, invalid).await.is_err());
    assert_eq!(rows(&f).await, (0, 0, 0));
    close(f, b).await;
}
