//! Current canonical profiles exercise management contracts without a source
//! database, migration plan, imported authority or legacy session factory.
use super::TestCurrentManagement;
use crate::{Error, EffectOutcome, OwnedFailure, management::*};
use crate::domain_worker::clock_fixtures as fixture;
use hagency_core::{replies::*, tasks::*};
use rusqlite::params;
use serde_json::json;
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};

const OWNER: &str = "@owner:example.test";
fn context() -> HostContext {
    HostContext::new(
        OWNER.into(),
        1,
        "b".repeat(64),
        1,
        "current_device".into(),
        1,
    )
    .unwrap()
}
fn session(store: &TestCurrentManagement) -> Session {
    Session::new_current(
        store.backing.clone(),
        context(),
        [
            Permission::ReadStoppedTasks,
            Permission::InspectStoppedTasks,
            Permission::ResolveStoppedTasks,
            Permission::ReadWorkspaces,
            Permission::ClearWorkspaceDirty,
            Permission::ReadTasks,
            Permission::ReadTaskBoard,
            Permission::CreateTasks,
            Permission::CommentTasks,
        ],
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
#[derive(Clone, Copy)]
enum State {
    Queued,
    Running,
    Stopped,
}
async fn seed(store: &TestCurrentManagement, state: State) -> StoppedQuery {
    store.call(8192, move |db| {
        db.register(&fixture::registration())?;
        let resource = fixture::resource("current", "current_seat", 1000);
        db.put_resource(&resource)?;
        db.db.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)", params!["a".repeat(64), resource.id()])?;
        let proof = fixture::proof(&fixture::request("current_request", "Worker", &resource, 100));
        let engagement = db.admit(&proof, 1000)?;
        db.approve("current_approve", &proof, 1000)?;
        let effect = db.claim_effect()?.ok_or(Error::State)?;
        db.observe_effect(&effect.id, effect.fence, &EffectOutcome::Applied { receipt: "current fixture provisioning".into() })?;
        db.observe_matrix_transport(&MatrixTransportObservation {
            engagement_id: engagement.id.clone(), registration_generation: 1, generation: 1,
            sender_mxid: "@worker:example.test".into(), device_id: "WORKER_DEVICE".into(),
        }, 1001)?;
        db.observe_matrix_room(&MatrixRoomObservation {
            engagement_id: engagement.id.clone(), registration_generation: 1, transport_generation: 1,
            room_id: "!direct:example.test".into(), generation: 1,
            privacy: RoomPrivacy::Direct { human_mxid: OWNER.into() },
            joined: BTreeSet::from(["@worker:example.test".into(), OWNER.into()]), invite_only: true, encrypted: true,
        }, 1002)?;
        db.resolve_verified_matrix_session(&SessionBinding {
            id: "session".into(), engagement_id: engagement.id.clone(), room_id: "!direct:example.test".into(), thread_root: None,
        }, 1003)?;
        db.create_canonical_task("task", "session", "Current interrupted work", 1004)?;
        db.register_workspace("workspace")?;
        db.enqueue_dispatch(&DispatchInput {
            id: "dispatch".into(), session_id: "session".into(), task_id: Some("task".into()),
            resources: vec![ResourceLease { id: "workspace".into(), exclusive: true }], payload: json!({"instruction":"Verify current task"}),
        })?;
        if !matches!(state, State::Queued) {
            let cap = db.claim_dispatch("current_runner", 1005, 60_000, 120_000, 1)?.ok_or(Error::State)?;
            let admitted = db.owned_dispatch_scope(&cap, 1006)?;
            let started = db.start_owned_dispatch(&cap, admitted.fingerprint(), 1006)?;
            if matches!(state, State::Stopped) {
                db.observe_owned_failure(&cap, OwnedFailure::Protocol, 1007)?;
                db.record_owned_stop_inspection(&cap, &started,
                    &json!({"profile":"stopped-content-inventory-v1","root":{},"entries":[]}), 1008)?;
            }
        }
        Ok(StoppedQuery { resource_id: resource.id(), engagement_id: engagement.id, dispatch_id: "dispatch".into(), entry_offset: 0 })
    }).await.unwrap()
}
fn review(row: &StoppedInspection) -> ReviewStopped {
    ReviewStopped {
        command_id: "review_current".into(),
        resource_id: row.attempt.resource_id.clone(),
        engagement_id: row.attempt.engagement_id.clone(),
        dispatch_id: row.attempt.dispatch_id.clone(),
        expected_revision: row.revision.clone(),
    }
}
fn decision(row: &StoppedReview, action: StoppedDecision) -> ResolveStopped {
    ResolveStopped {
        command_id: "resolve_current".into(),
        resource_id: row.inspection.attempt.resource_id.clone(),
        engagement_id: row.inspection.attempt.engagement_id.clone(),
        dispatch_id: row.inspection.attempt.dispatch_id.clone(),
        review_command_id: row.review_command_id.clone(),
        review_revision: row.inspection.revision.clone(),
        action,
        operator_note: "Inspected original workspace and effects".into(),
        continuation_instruction: (action == StoppedDecision::Continue)
            .then(|| "Complete remaining work".into()),
    }
}
async fn counts(store: &TestCurrentManagement) -> (u64, u64, u64) {
    store.call(256, |db| Ok(db.db.query_row(
        "SELECT (SELECT count(*) FROM outcome_resolutions),(SELECT count(*) FROM outcome_inspections WHERE consumed_at IS NOT NULL),(SELECT count(*) FROM management_commands WHERE operation='stopped.resolve')",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?)).await.unwrap()
}

#[tokio::test]
async fn current_management_three_stopped_decisions_are_atomic_and_restart_idempotent() {
    for action in [
        StoppedDecision::Continue,
        StoppedDecision::AcceptCompleted,
        StoppedDecision::KeepBlocked,
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("current");
        let store = TestCurrentManagement::open(&path, OWNER).await;
        let q = seed(&store, State::Stopped).await;
        let m = session(&store);
        let row = m.stopped_inspection(&context(), q.clone()).await.unwrap();
        assert!(row.resolvable && row.can_continue);
        let reviewed = m.review_stopped(&context(), review(&row)).await.unwrap();
        assert_eq!(
            reviewed,
            m.review_stopped(&context(), review(&row)).await.unwrap()
        );
        let input = decision(&reviewed, action);
        store.call(1024, |db| { db.db.execute_batch("CREATE TRIGGER current_fail_receipt BEFORE INSERT ON management_commands WHEN NEW.operation='stopped.resolve' BEGIN SELECT RAISE(ABORT,'fixture receipt failure'); END;")?; Ok(()) }).await.unwrap();
        assert!(m.resolve_stopped(&context(), input.clone()).await.is_err());
        assert_eq!(counts(&store).await, (0, 0, 0));
        assert!(
            m.stopped_inspection(&context(), q.clone())
                .await
                .unwrap()
                .resolvable
        );
        store
            .call(128, |db| {
                db.db.execute_batch("DROP TRIGGER current_fail_receipt")?;
                Ok(())
            })
            .await
            .unwrap();
        let saved = m.resolve_stopped(&context(), input.clone()).await.unwrap();
        assert_eq!(
            saved.task_status,
            match action {
                StoppedDecision::Continue => "in_progress",
                StoppedDecision::AcceptCompleted => "done",
                StoppedDecision::KeepBlocked => "blocked",
            }
        );
        assert_eq!(
            saved,
            m.resolve_stopped(&context(), input.clone()).await.unwrap()
        );
        assert_eq!(counts(&store).await, (1, 1, 1));
        assert_eq!(
            saved.replacement_id.is_some(),
            action == StoppedDecision::Continue
        );
        let mut changed = input.clone();
        changed.operator_note = "different".into();
        assert!(matches!(
            m.resolve_stopped(&context(), changed).await,
            Err(Error::Conflict)
        ));
        m.retire();
        store.shutdown().await.unwrap();
        drop(m);
        drop(store);
        let reopened = TestCurrentManagement::open(&path, OWNER).await;
        assert_eq!(
            saved,
            session(&reopened)
                .resolve_stopped(&context(), input)
                .await
                .unwrap()
        );
        assert_eq!(counts(&reopened).await, (1, 1, 1));
        reopened.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn current_management_expired_stopped_review_cannot_settle() {
    let tmp = tempfile::tempdir().unwrap();
    let store = TestCurrentManagement::open(&tmp.path().join("current"), OWNER).await;
    let q = seed(&store, State::Stopped).await;
    let m = session(&store);
    let row = m.stopped_inspection(&context(), q).await.unwrap();
    let reviewed = m.review_stopped(&context(), review(&row)).await.unwrap();
    store
        .call(128, |db| {
            db.db.execute(
                "UPDATE outcome_inspections SET created_at=1,expires_at=2",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        m.resolve_stopped(
            &context(),
            decision(&reviewed, StoppedDecision::KeepBlocked)
        )
        .await,
        Err(Error::StoppedNotResolvable)
    ));
    assert_eq!(counts(&store).await, (0, 0, 0));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn current_management_workspace_release_replays_after_restart_and_refuses_active_lease() {
    for running in [false, true] {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("current");
        let store = TestCurrentManagement::open(&path, OWNER).await;
        let q = seed(
            &store,
            if running {
                State::Running
            } else {
                State::Queued
            },
        )
        .await;
        store
            .call(128, |db| {
                db.db
                    .execute("UPDATE workspace_resources SET dirty=1", [])?;
                Ok(())
            })
            .await
            .unwrap();
        let m = session(&store);
        let query = WorkspaceQuery {
            resource_id: q.resource_id,
            workspace_id: "workspace".into(),
        };
        let row = m
            .resource_workspace(&context(), query.clone())
            .await
            .unwrap();
        let input = ClearWorkspaceDirty {
            command_id: "release_current".into(),
            resource_id: query.resource_id.clone(),
            workspace_id: query.workspace_id.clone(),
            expected_revision: row.revision.clone(),
        };
        if running {
            assert_eq!(
                row.release_block,
                Some(WorkspaceReleaseBlock::ActiveExecution)
            );
            assert!(m.clear_workspace_dirty(&context(), input).await.is_err());
            assert_eq!(m.resource_workspace(&context(), query).await.unwrap(), row);
            let receipts: u64 = store.call(128, |db| Ok(db.db.query_row("SELECT count(*) FROM management_commands WHERE operation='workspace.clear_dirty'", [], |r| r.get(0))?)).await.unwrap();
            assert_eq!(receipts, 0);
            store.shutdown().await.unwrap();
        } else {
            assert_eq!(row.release_block, None);
            let saved = m
                .clear_workspace_dirty(&context(), input.clone())
                .await
                .unwrap();
            assert!(saved.cleared);
            store
                .call(128, |db| {
                    db.db
                        .execute("UPDATE workspace_resources SET dirty=1", [])?;
                    Ok(())
                })
                .await
                .unwrap();
            m.retire();
            store.shutdown().await.unwrap();
            drop(m);
            drop(store);
            let reopened = TestCurrentManagement::open(&path, OWNER).await;
            let m = session(&reopened);
            assert_eq!(
                m.clear_workspace_dirty(&context(), input).await.unwrap(),
                saved
            );
            assert!(
                m.resource_workspace(&context(), query).await.unwrap().dirty,
                "original receipt must not clear a later dirty mark"
            );
            reopened.shutdown().await.unwrap();
        }
    }
}

fn bytes(path: &Path) -> (Vec<u8>, Option<Vec<u8>>) {
    let path = path.join("canonical");
    (
        std::fs::read(path.join("domain.sqlite3")).unwrap(),
        std::fs::read(path.join("domain.sqlite3-wal")).ok(),
    )
}
#[tokio::test]
async fn current_management_task_pages_board_and_escaped_detail_are_bounded_and_read_only() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("current");
    let store = TestCurrentManagement::open(&path, OWNER).await;
    let m = session(&store);
    let escaped = "line\n\"\\".repeat(500);
    for index in 0..18 {
        m.apply_operator_task(
            &context(),
            OperatorTaskCommand {
                command_id: format!("create_{index:02}"),
                task_id: format!("task_{index:02}"),
                change: OperatorTaskChange::Create(CreateOperatorTask {
                    title: format!("Task {index}"),
                    description: Some(escaped.clone()),
                    assignee: Some("Alpha".into()),
                    labels: vec!["review".into()],
                    priority: Some("p1".into()),
                    ..Default::default()
                }),
            },
        )
        .await
        .unwrap();
    }
    let mut revision = 1;
    for index in 0..5 {
        let saved = m
            .apply_operator_task(
                &context(),
                OperatorTaskCommand {
                    command_id: format!("comment_{index}"),
                    task_id: "task_17".into(),
                    change: OperatorTaskChange::Comment(CommentOperatorTask {
                        id: "task_17".into(),
                        expected_revision: revision,
                        text: format!("comment {index} \"\\\ncontent"),
                    }),
                },
            )
            .await
            .unwrap();
        revision = saved.revision;
    }
    let before = bytes(&path);
    let query = OperatorTaskQuery {
        assignee: Some("Alpha".into()),
        label: Some("review".into()),
        priority: Some("p1".into()),
        ..Default::default()
    };
    let first = m
        .operator_task_page(&context(), query.clone())
        .await
        .unwrap();
    assert_eq!(first.tasks.len(), 16);
    assert_eq!(first.next_offset, Some(16));
    let rest = m
        .operator_task_page(
            &context(),
            OperatorTaskQuery {
                offset: 16,
                ..query
            },
        )
        .await
        .unwrap();
    assert_eq!(rest.tasks.len(), 2);
    assert_eq!(rest.next_offset, None);
    let ids = first
        .tasks
        .iter()
        .chain(&rest.tasks)
        .map(|t| &t.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), 18);
    let mut offset = 0;
    let mut comments = Vec::new();
    loop {
        let detail = m
            .operator_task_detail(
                &context(),
                OperatorTaskDetailQuery {
                    id: "task_17".into(),
                    comment_offset: offset,
                },
            )
            .await
            .unwrap();
        let encoded = serde_json::to_vec(&detail).unwrap();
        assert!(encoded.len() < 120 * 1024);
        let roundtrip: OperatorTaskDetail = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            roundtrip.task.as_ref().unwrap().description.as_str(),
            escaped.as_str()
        );
        assert_eq!(detail.comment_count, 5);
        assert!(detail.task.as_ref().unwrap().comments.len() <= 2);
        comments.extend(detail.task.unwrap().comments.into_iter().map(|c| c.text));
        if let Some(next) = detail.next_comment_offset {
            offset = next;
        } else {
            break;
        }
    }
    assert_eq!(comments.len(), 5);
    let board = m
        .operator_board(&context(), OperatorBoardQuery::default())
        .await
        .unwrap();
    assert_eq!(board.task_totals["created"], 18);
    assert_eq!(board.task_totals.values().sum::<u64>(), 18);
    assert_eq!((board.project_count, board.agent_count), (0, 0));
    assert_eq!(bytes(&path), before);
    let oversized = store
        .call(128, |db| {
            db.db.execute(
                "UPDATE owner_operator_tasks SET description=?1 WHERE id='task_17'",
                ["\u{0001}".repeat(100_000)],
            )?;
            Ok(())
        })
        .await;
    assert!(
        matches!(oversized, Err(Error::Sqlite(_))),
        "current schema rejects an oversized retained description"
    );
    let retained = m
        .operator_task_detail(
            &context(),
            OperatorTaskDetailQuery {
                id: "task_17".into(),
                comment_offset: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(retained.task.unwrap().description, escaped);
    store.shutdown().await.unwrap();
}
