use super::*;
use crate::console::native::reconciliation_sources::tests::Fixture;
use crate::console::native::{ManagementPackage};
use hagency_store::{
    management::{CommentOperatorTask, CreateOperatorTask, OperatorTaskDetailQuery, Permission},
    owner_resources::{OwnerResourceRegistry},
};
use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
fn package(instance: u64) -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadResources,
            Permission::PrepareSourceQuiescence,
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
    }
}
#[test]
fn native_task_writer_original_ids_restart_and_current_authority() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "console::native::operator_task_commands::tests::native_task_writer_process_child",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_TASK_WRITER_FIXTURE", "1")
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
async fn native_task_writer_process_child() {
    if std::env::var("HAGENCY_TASK_WRITER_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let f = Fixture::new().await;
    let registry=OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("canonical")).unwrap();
    let f=f.initialize_current(registry.clone()).await;
    let manager = f.owner.management(package(2)).await.unwrap();
    let prepared = manager
        .prepare_operator_task(OperatorTaskChange::Create(CreateOperatorTask {
            title: "Native original".into(),
            ..Default::default()
        }))
        .await
        .unwrap();
    assert!(prepared.command_id.starts_with("taskcmd_") && prepared.task_id.starts_with("task_"));
    assert!(
        manager
            .operator_task_page(Default::default())
            .await
            .unwrap()
            .tasks
            .is_empty()
    );
    let saved = manager.saved_commands().await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].command_id, prepared.command_id);
    let original: OperatorTaskCommand = serde_json::from_value(saved[0].input.clone()).unwrap();
    // Real canonical commit, interrupted before the native intent reply is saved.
    let (context, session) = manager.authorized().await.unwrap();
    let committed = session
        .apply_operator_task(&context, original)
        .await
        .unwrap();
    assert!(
        manager
            .review_saved_operator_task(prepared.command_id.clone())
            .await
            .unwrap()
            .result
            .is_none()
    );
    manager.retire();
    assert!(
        manager
            .apply_saved_operator_task(prepared.command_id.clone())
            .await
            .is_err()
    );
    drop(session);
    drop(manager);
    let f = f
        .restart_current(registry.clone(), "current_fixture".into())
        .await;
    let manager = f.owner.management(package(3)).await.unwrap();
    assert_eq!(
        manager
            .apply_saved_operator_task(prepared.command_id.clone())
            .await
            .unwrap(),
        committed
    );
    assert_eq!(
        manager
            .operator_task_page(Default::default())
            .await
            .unwrap()
            .tasks
            .len(),
        1
    );
    let comment = manager
        .prepare_operator_task(OperatorTaskChange::Comment(CommentOperatorTask {
            id: prepared.task_id.clone(),
            expected_revision: 1,
            text: "Once only".into(),
        }))
        .await
        .unwrap();
    let first = manager
        .apply_saved_operator_task(comment.command_id.clone())
        .await
        .unwrap();
    assert_eq!(
        manager
            .apply_saved_operator_task(comment.command_id.clone())
            .await
            .unwrap(),
        first
    );
    let d = manager
        .operator_task_detail(OperatorTaskDetailQuery {
            id: prepared.task_id.clone(),
            comment_offset: 0,
        })
        .await
        .unwrap();
    assert_eq!(d.comment_count, 1);
    assert_eq!(d.task.unwrap().comments[0].author, "@owner:test");
    let mut read = package(4);
    read.permissions = vec![Permission::ReadTasks, Permission::ReadIntents];
    let readonly = f.owner.management(read).await.unwrap();
    assert!(
        readonly
            .apply_saved_operator_task(comment.command_id.clone())
            .await
            .is_err()
    );
    assert!(
        !readonly
            .operator_task_page(Default::default())
            .await
            .unwrap()
            .write_supported
    );
    f.live.store(false, Ordering::Release);
    assert!(
        manager
            .review_saved_operator_task(comment.command_id)
            .await
            .is_err()
    );
    assert!(
        manager
            .prepare_operator_task(OperatorTaskChange::Create(CreateOperatorTask {
                title: "Must not save".into(),
                ..Default::default()
            }))
            .await
            .is_err()
    );
    drop(readonly);
    drop(manager);
    f.stop().await;
    registry.shutdown().await.unwrap();
}
#[test]
fn native_task_writer_failure_classification_keeps_unknowns_pending() {
    for (status, code) in [
        (401, "management_access_required"),
        (409, "management_outcome_unknown"),
        (409, "management_revision_conflict"),
        (503, "management_unavailable"),
        (400, "unrelated_error"),
    ] {
        assert!(!known_failure(&failure(status, code)));
    }
    assert!(known_failure(&failure(
        409,
        "management_task_revision_changed"
    )));
}
