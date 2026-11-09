use super::*;
use crate::console::native::reconciliation_sources::tests::Fixture;
use crate::console::native::{ManagementPackage};
use hagency_store::owner_resources::{OwnerResourceRegistry};
use std::time::{Duration, Instant};
fn package(instance: u64) -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadResources,
            Permission::PrepareSourceQuiescence,
            Permission::ReadResourceTemplates,
            Permission::ManageResourceTemplates,
            Permission::ReadIntents,
            Permission::AcknowledgeIntents,
        ],
    }
}
#[test]
fn native_template_original_ids_restart_and_no_approval_side_effect() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "console::native::resource_definitions::tests::native_template_process_child",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_TEMPLATE_FIXTURE", "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}
#[test]
fn native_template_cancellation_withdraws_put_off_change_and_recovers_applied_effect() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "console::native::resource_definitions::tests::native_template_cancellation_process_child",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_TEMPLATE_CANCEL_FIXTURE", "1")
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
async fn native_template_process_child() {
    if std::env::var("HAGENCY_TEMPLATE_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let f = Fixture::new().await;
    let registry=OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("canonical")).unwrap();
    let f=f.initialize_current(registry.clone()).await;
    let resource=f.current_resource(&registry).await;
    let manager = f.owner.management(package(2)).await.unwrap();

    let page = manager
        .resource_templates(resource.id(), None)
        .await
        .unwrap();
    let input = TemplateCreateInput {
        resource_id: resource.id(),
        expected_revision: page.revision,
        name: "native_helper".into(),
        role: page.roles[0].clone(),
        enabled: true,
    };
    let prepared = manager.prepare_template_create(input).await.unwrap();
    assert!(prepared.command_id.starts_with("templatecmd_"));
    assert!(prepared.definition_id.starts_with("rad_"));
    assert!(
        manager
            .resource_templates(resource.id(), None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let saved = manager.saved_commands().await.unwrap();
    assert_eq!(saved.len(), 1);
    let original: ResourceTemplateCommand = serde_json::from_value(saved[0].input.clone()).unwrap();
    let (context, session) = manager.authorized().await.unwrap();
    // Crash boundary: canonical definition and receipt commit, intent reply lost.
    let committed = session
        .apply_resource_template(&context, original)
        .await
        .unwrap();
    assert!(
        manager
            .review_template_command(prepared.command_id.clone())
            .await
            .unwrap()
            .result
            .is_none()
    );
    manager.retire();
    assert!(
        manager
            .apply_saved_template(prepared.command_id.clone())
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
            .apply_saved_template(prepared.command_id.clone())
            .await
            .unwrap(),
        committed
    );
    let detail = manager
        .resource_template(resource.id(), prepared.definition_id.clone())
        .await
        .unwrap();
    let edit = manager
        .prepare_template_update(TemplateUpdateInput {
            resource_id: resource.id(),
            id: prepared.definition_id.clone(),
            expected_revision: detail.revision,
            name: "renamed_helper".into(),
            role: detail.definition.role.clone(),
            enabled: false,
        })
        .await
        .unwrap();
    let edited = manager
        .apply_saved_template(edit.command_id.clone())
        .await
        .unwrap();
    assert!(!edited.deleted);
    assert_eq!(
        manager
            .apply_saved_template(prepared.command_id.clone())
            .await
            .unwrap(),
        committed
    );
    assert_eq!(
        manager
            .resource_template(resource.id(), prepared.definition_id.clone())
            .await
            .unwrap()
            .definition
            .name,
        "renamed_helper"
    );
    manager
        .acknowledge_command(prepared.command_id.clone())
        .await
        .unwrap();
    manager
        .acknowledge_command(edit.command_id.clone())
        .await
        .unwrap();
    let detail = manager
        .resource_template(resource.id(), prepared.definition_id.clone())
        .await
        .unwrap();
    let delete = manager
        .prepare_template_delete(TemplateDeleteInput {
            resource_id: resource.id(),
            id: prepared.definition_id.clone(),
            expected_revision: detail.revision,
        })
        .await
        .unwrap();
    assert_eq!(delete.change.name, "renamed_helper");
    assert!(!delete.change.enabled);
    let removed = manager
        .apply_saved_template(delete.command_id.clone())
        .await
        .unwrap();
    assert!(removed.deleted);
    assert_eq!(removed.definition_id, prepared.definition_id);
    assert_eq!(
        manager
            .apply_saved_template(delete.command_id.clone())
            .await
            .unwrap(),
        removed
    );
    assert!(
        manager
            .resource_templates(resource.id(), None)
            .await
            .unwrap()
            .items
            .is_empty()
    );
    let review = manager
        .review_template_command(delete.command_id)
        .await
        .unwrap();
    assert_eq!(review.result, Some(removed));
    let mut read_package = package(4);
    read_package
        .permissions
        .retain(|p| *p != Permission::ManageResourceTemplates);
    let read = f.owner.management(read_package).await.unwrap();
    assert!(
        !read
            .resource_templates(resource.id(), None)
            .await
            .unwrap()
            .can_create
    );
    let inspect = rusqlite::Connection::open_with_flags(
        f.temp.path().join("canonical/domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    for table in ["owner_managed_agents", "engagements", "effects"] {
        assert_eq!(
            inspect
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            0
        );
    }
    drop(inspect);
    read.retire();
    manager.retire();
}
#[tokio::test]
async fn native_template_cancellation_process_child() {
    if std::env::var("HAGENCY_TEMPLATE_CANCEL_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let f = Fixture::new().await;
    let registry=OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("canonical")).unwrap();
    let f=f.initialize_current(registry.clone()).await;
    let resource=f.current_resource(&registry).await;
    let manager = f.owner.management(package(2)).await.unwrap();
    template_cancellation(&manager, &resource.id()).await;
    manager.retire();
    f.owner.shutdown().await;
    registry.shutdown().await.unwrap();
}
/// A change put off with 「稍后」 can be withdrawn. Mirrors the saved web
/// catalogue cancellation: the canonical receipt decides a race with apply.
async fn template_cancellation(manager: &NativeManagement, resource: &str) {
    let page = manager
        .resource_templates(resource.into(), None)
        .await
        .unwrap();
    let role = page.roles[0].clone();
    let create = |name: &str, expected_revision: String| TemplateCreateInput {
        resource_id: resource.into(),
        expected_revision,
        name: name.into(),
        role: role.clone(),
        enabled: true,
    };
    let revision = || async {
        manager
            .resource_templates(resource.into(), None)
            .await
            .unwrap()
            .revision
    };
    let names = || async {
        manager
            .resource_templates(resource.into(), None)
            .await
            .unwrap()
            .items
            .into_iter()
            .map(|t| t.name)
            .collect::<Vec<_>>()
    };
    let saved = |id: String| async move {
        let rows = manager.saved_commands().await.unwrap();
        decode(rows.iter().find(|s| s.command_id == id).unwrap()).unwrap()
    };

    // Withdrawn before apply: no definition, and it can never be applied.
    let original = manager
        .prepare_template_create(create("withdrawn_helper", page.revision.clone()))
        .await
        .unwrap();
    let cancelled = manager
        .cancel_saved_template(original.command_id.clone())
        .await
        .unwrap();
    assert_eq!(cancelled.command_id, original.command_id);
    assert_eq!(
        cancelled.error.as_deref(),
        Some("management_change_cancelled")
    );
    assert!(cancelled.result.is_none());
    assert_eq!(
        manager
            .apply_saved_template(original.command_id.clone())
            .await
            .unwrap_err()
            .code,
        "management_change_cancelled"
    );
    assert_eq!(
        manager
            .cancel_saved_template(original.command_id.clone())
            .await
            .unwrap()
            .error,
        cancelled.error
    );
    assert!(names().await.is_empty());
    manager
        .acknowledge_command(original.command_id.clone())
        .await
        .unwrap();
    assert!(
        manager
            .saved_commands()
            .await
            .unwrap()
            .iter()
            .all(|s| s.command_id != original.command_id)
    );

    // Applied, journal reply lost: withdrawing recovers the applied effect.
    let applied = manager
        .prepare_template_create(create("kept_helper", revision().await))
        .await
        .unwrap();
    let command = saved(applied.command_id.clone()).await;
    let (context, session) = manager.authorized().await.unwrap();
    let effect = session
        .apply_resource_template(&context, command)
        .await
        .unwrap();
    assert!(
        manager
            .review_template_command(applied.command_id.clone())
            .await
            .unwrap()
            .result
            .is_none()
    );
    let recovered = manager
        .cancel_saved_template(applied.command_id.clone())
        .await
        .unwrap();
    assert_eq!(recovered.result, Some(effect.clone()));
    assert!(recovered.error.is_none());
    assert_eq!(
        manager
            .apply_saved_template(applied.command_id.clone())
            .await
            .unwrap(),
        effect
    );
    assert_eq!(
        manager
            .cancel_saved_template(applied.command_id.clone())
            .await
            .unwrap()
            .result,
        recovered.result
    );
    manager
        .acknowledge_command(applied.command_id)
        .await
        .unwrap();

    // Withdrawal committed, journal reply lost: apply stays refused.
    let lost = manager
        .prepare_template_create(create("lost_helper", revision().await))
        .await
        .unwrap();
    let command = saved(lost.command_id.clone()).await;
    assert!(
        session
            .cancel_resource_template(&context, command)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        manager
            .apply_saved_template(lost.command_id.clone())
            .await
            .unwrap_err()
            .code,
        "management_change_cancelled"
    );
    assert_eq!(
        manager
            .cancel_saved_template(lost.command_id)
            .await
            .unwrap()
            .error
            .as_deref(),
        Some("management_change_cancelled")
    );
    assert_eq!(names().await, ["kept_helper"]);
}
