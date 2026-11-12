use super::*;
use crate::console::native::reconciliation_sources::tests::Fixture;
fn package() -> ManagementPackage {
    ManagementPackage {
        digest: "a".repeat(64),
        instance: 1,
        account_epoch: 1,
        permissions: vec![
            Permission::ReadResources,
            Permission::PrepareSourceQuiescence,
        ],
    }
}
#[tokio::test]
async fn current_setup_preview_cancel_and_expiry_create_no_profile_or_quota() {
    let f = Fixture::new().await;
    let context = f.owner.device_setup_context(package(), None).await.unwrap();
    assert!(context.initialization_id.is_none());
    let review = f.owner.review_device_setup(package(), None).await.unwrap();
    assert!(!review.expired());
    drop(review);
    let review = f.owner.review_device_setup(package(), None).await.unwrap();
    assert!(
        f.owner
            .confirm_device_setup(review, package(), || false)
            .await
            .is_err()
    );
    let mut review = f.owner.review_device_setup(package(), None).await.unwrap();
    review.expires = Instant::now() - Duration::from_secs(1);
    assert!(
        f.owner
            .confirm_device_setup(review, package(), || true)
            .await
            .is_err()
    );
    assert!(
        !f.owner
            .0
            .host
            .state_directory()
            .join("adopted-management-initializer-v1.json")
            .exists()
    );
    assert!(
        !f.owner
            .0
            .configuration_stopped
            .load(std::sync::atomic::Ordering::Acquire)
    );
    f.stop().await;
}
#[test]
fn current_setup_confirmation_restart_keeps_current_identity_and_command_receipts() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "console::native::device_setup::tests::current_setup_process_child",
            "--exact",
            "--nocapture",
        ])
        .env("HAGENCY_CURRENT_SETUP_PROCESS", "1")
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
async fn current_setup_process_child() {
    if std::env::var("HAGENCY_CURRENT_SETUP_PROCESS").as_deref() != Ok("1") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    initialize_resource_activation_fixture(&root.path().join("installation")).unwrap();
    let f = Fixture::with_configured_installation().await;
    let review = f.owner.review_device_setup(package(), None).await.unwrap();
    let selected = f
        .owner
        .confirm_device_setup(review, package(), || true)
        .await
        .unwrap();
    let id = selected.initialization_id.clone();
    let f = f.restart_onboarding(selected.clone()).await;
    assert_eq!(
        selected.device_setup_phase(&f.owner).await.unwrap(),
        "active"
    );
    assert!(
        f.owner
            .review_device_setup(package(), Some(selected.clone()))
            .await
            .is_err()
    );
    let mut p = package();
    p.permissions
        .extend([Permission::PrepareAccounts, Permission::ReadAccounts]);
    let manager = f.owner.management(p).await.unwrap();
    assert!(manager.account_enrollment_ready());
    assert!(
        manager
            .resources(Default::default())
            .await
            .unwrap()
            .resources
            .is_empty()
    );
    let account = manager
        .prepare_account(hagency_store::management::PrepareAccount {
            command_id: "current_account".into(),
        })
        .await
        .unwrap();
    manager.retire();
    drop(manager);
    let f = f.restart_onboarding(selected.clone()).await;
    let again = f
        .owner
        .current_profile_selection(selected.registry.clone())
        .await
        .unwrap();
    assert_eq!(again.initialization_id, id);
    let mut p = package();
    p.permissions
        .extend([Permission::PrepareAccounts, Permission::ReadAccounts]);
    let manager = f.owner.management(p).await.unwrap();
    assert_eq!(
        manager
            .prepare_account(hagency_store::management::PrepareAccount {
                command_id: "current_account".into()
            })
            .await
            .unwrap()
            .account_id,
        account.account_id
    );
    assert!(
        manager
            .resources(Default::default())
            .await
            .unwrap()
            .resources
            .is_empty()
    );
    manager.retire();
    drop(manager);
    let registry = selected.registry.clone();
    drop(again);
    drop(selected);
    f.stop().await;
    registry.shutdown().await.unwrap();
}
