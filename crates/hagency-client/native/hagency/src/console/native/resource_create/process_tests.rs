use super::*;
use crate::console::native::{
    ManagementPackage, initialize_resource_activation_fixture,
    reconciliation_sources::tests::Fixture,
};
use std::{sync::atomic::Ordering, time::Duration};

fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance: 1,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadResources,
            Permission::ConfigureResources,
            Permission::PrepareSourceQuiescence,
            Permission::ReadIntents,
        ],
    }
}
fn duplicate(input: &CreateResourceConfiguration) -> CreateResourceConfiguration {
    serde_json::from_value(serde_json::to_value(input).unwrap()).unwrap()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(30)
}

#[test]
fn native_additional_configuration_public_api_in_isolated_process() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "console::native::resource_create::process_tests::native_creation_child",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_CREATE_CONFIGURATION_PROCESS_TEST", "1")
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
async fn native_creation_child() {
    if std::env::var("HAGENCY_CREATE_CONFIGURATION_PROCESS_TEST").as_deref() != Ok("1") {
        return;
    }
    let installation = tempfile::tempdir().unwrap();
    initialize_resource_activation_fixture(&installation.path().join("installation")).unwrap();
    let f = Fixture::with_configured_installation().await;
    let (db, path, _) = f.domain();
    drop(db);
    let db = rusqlite::Connection::open(path.join("domain.sqlite3")).unwrap();
    db.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    db.execute("DELETE FROM resources", []).unwrap();
    drop(db);
    let review = f
        .owner
        .review_device_setup(package(), None)
        .await
        .unwrap();
    let selected = f
        .owner
        .confirm_device_setup(review, package(), || true)
        .await
        .unwrap();
    let f = f.restart_onboarding(selected.clone()).await;
    let manager = f.owner.management(package()).await.unwrap();
    let (context, session) = manager.authorized().await.unwrap();
    let (registry, profile) = session.local_resource_editor_registry(&context).unwrap();
    let proof = hagency_store::owner_resources::VerifiedProviderAccount::new(
        "codex",
        "fixture",
        "configuration-account",
        deadline(),
    )
    .unwrap();
    let source = registry
        .prepare_enrollment(
            profile,
            "original_enrollment".into(),
            proof,
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            700,
            deadline(),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    let detail = manager
        .resource_detail(hagency_store::management::ResourceDetailQuery {
            id: source.resource_id.clone(),
        })
        .await
        .unwrap();
    let request = CreateResourceConfiguration {
        execution_policy: Some(hagency_core::execution::ExecutionPolicy { yolo: true }),
        command_id: "native_create".into(),
        source_resource_id: source.resource_id,
        expected_revision: detail.resource.revision,
        profile: hagency_store::ProfileChange::Select {
            model: "gpt-5.6-sol".into(),
            reasoning: Some("high".into()),
        },
        ceiling: hagency_store::CeilingChange::Monthly {
            tokens: 450.try_into().unwrap(),
        },
    };
    assert!(matches!(
        manager
            .review_resource_configuration(&request)
            .await
            .unwrap(),
        ResourceConfigurationReview::Available(_)
    ));
    assert!(manager.saved_commands().await.unwrap().is_empty());
    assert_eq!(
        manager
            .resources(Default::default())
            .await
            .unwrap()
            .resources
            .len(),
        1
    );
    assert!(
        manager
            .create_resource_configuration(duplicate(&request), Instant::now())
            .await
            .is_err()
    );
    assert!(manager.saved_commands().await.unwrap().is_empty());
    let created = manager
        .create_resource_configuration(duplicate(&request), deadline())
        .await
        .unwrap();
    assert_ne!(created.resource_id, request.source_resource_id);
    assert!(manager.resource_detail(hagency_store::management::ResourceDetailQuery {
        id: created.resource_id.clone(),
    }).await.unwrap().execution_policy.yolo);
    assert!(matches!(
        manager
            .review_resource_configuration(&request)
            .await
            .unwrap(),
        ResourceConfigurationReview::Recorded(_)
    ));
    assert_eq!(
        manager
            .resources(Default::default())
            .await
            .unwrap()
            .resources
            .len(),
        2
    );
    assert_eq!(manager.saved_commands().await.unwrap().len(), 1);
    let mut stale = duplicate(&request);
    stale.command_id = "stale_source_request".into();
    stale.expected_revision = "b".repeat(64);
    assert_eq!(manager.create_resource_configuration(stale, deadline()).await.unwrap_err().code,
        "management_resource_source_changed");
    assert!(manager.saved_commands().await.unwrap().iter().any(|v| v.command_id=="stale_source_request"
        && matches!(&v.reply, Some(hagency_store::management::IntentReply::Failure {status:409,code}) if code=="management_resource_source_changed")));
    // Commit through the same canonical API after private custody, deliberately
    // lose only the private reply, then restart the actual NativeOwner.
    let mut lost = duplicate(&request);
    lost.command_id = "lost_native_reply".into();
    let intent = Intent::new(
        IntentOperation::CreateResourceConfiguration,
        lost.command_id.clone(),
        &lost,
    )
    .unwrap();
    session.prepare_intent(&context, intent).await.unwrap();
    let result = session
        .create_resource_configuration(&context, duplicate(&lost), deadline())
        .await
        .unwrap();
    assert!(
        manager
            .saved_commands()
            .await
            .unwrap()
            .iter()
            .any(|v| v.command_id == lost.command_id && v.reply.is_none())
    );
    drop(session);
    drop(manager);
    drop(registry);
    let f = f.restart_onboarding(selected.clone()).await;
    let manager = f.owner.management(package()).await.unwrap();
    let ResourceConfigurationReview::Recorded(review) =
        manager.review_resource_configuration(&lost).await.unwrap()
    else {
        panic!("original creation was not recovered")
    };
    assert_eq!(review.resource_id, result.resource_id);
    let recovered = manager
        .retry_saved_command(lost.command_id.clone())
        .await
        .unwrap();
    assert_eq!(recovered["operation"], "create_resource_configuration");
    assert_eq!(recovered["data"], serde_json::to_value(&result).unwrap());
    assert_eq!(
        manager
            .resources(Default::default())
            .await
            .unwrap()
            .resources
            .len(),
        3
    );
    assert!(
        manager
            .saved_commands()
            .await
            .unwrap()
            .iter()
            .any(|v| v.command_id == lost.command_id && v.reply.is_some())
    );
    f.live.store(false, Ordering::Release);
    assert!(
        manager
            .create_resource_configuration(duplicate(&request), deadline())
            .await
            .is_err()
    );
    drop(manager);
    let registry = selected.registry.clone();
    drop(selected);
    f.stop().await;
    registry.shutdown().await.unwrap();
}
