use super::*;

fn resume_state(f: &Fixture) -> String {
    let path = f
        .owner
        .0
        .host
        .state_directory()
        .join("server-login-sdk-resume.json");
    let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    value["state"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn native_owner_suspend_preserves_original_credentials_but_retires_capabilities() {
    let f = Fixture::new().await;
    let path = f
        .owner
        .0
        .host
        .state_directory()
        .join("server-login-sdk-resume.json");
    let original = std::fs::read(&path).unwrap();
    assert_eq!(resume_state(&f), "active");
    f.owner.suspend().await.unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(f.owner.identity().await.is_err());
    assert!(f.owner.management(package(20)).await.is_err());
    assert!(
        f.transport
            .poll_allocation(&f.registry, None)
            .await
            .is_err()
    );
    f.owner.shutdown_checked().await.unwrap();
    assert_eq!(resume_state(&f), "revoked");
    f.owner.suspend().await.unwrap();
    assert_eq!(
        resume_state(&f),
        "revoked",
        "suspend cannot undo explicit logout"
    );
    f.stop().await;
}

#[tokio::test]
async fn native_owner_cancelled_suspend_retains_drain_and_sync_logout_dominates() {
    let f = Fixture::new().await;
    let activity = f.owner.0.configuration_custody.enter().unwrap();
    let owner = f.owner.clone();
    let first = tokio::spawn(async move { owner.suspend().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !f.owner.0.configuration_stopped.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!first.is_finished());
    first.abort();
    let _ = first.await;
    f.owner.retire_and_revoke_resumption().unwrap();
    assert_eq!(
        resume_state(&f),
        "revoked",
        "durable refusal precedes next await"
    );
    let owner = f.owner.clone();
    let mut second = tokio::spawn(async move { owner.shutdown_checked().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(25), &mut second)
            .await
            .is_err()
    );
    drop(activity);
    tokio::time::timeout(Duration::from_secs(2), second)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    f.owner.suspend().await.unwrap();
    assert_eq!(resume_state(&f), "revoked");
    f.stop().await;
}

#[tokio::test]
async fn native_owner_unknown_drain_is_never_upgraded_to_success_by_retry() {
    let f = Fixture::new().await;
    f.owner.0.configuration_custody.unknown();
    for _ in 0..2 {
        assert_eq!(
            f.owner.suspend().await.unwrap_err().code,
            "native_configuration_drain_unknown"
        );
    }
    assert_eq!(resume_state(&f), "active");
    assert!(f.owner.identity().await.is_err());
    f.stop().await;
}

#[tokio::test]
async fn native_owner_checked_logout_reports_failed_durable_refusal() {
    let f = Fixture::new().await;
    let path = f
        .owner
        .0
        .host
        .state_directory()
        .join("server-login-sdk-resume.json");
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(
        f.owner.retire_and_revoke_resumption().unwrap_err().code,
        "native_logout_persistence_failed"
    );
    assert!(f.owner.identity().await.is_err());
    assert_eq!(
        f.owner.shutdown_checked().await.unwrap_err().code,
        "native_logout_persistence_failed"
    );
    f.stop().await;
}

#[tokio::test]
async fn native_owner_command_source_loss_suspends_without_withdrawing_saved_account() {
    let f = Fixture::new().await;
    let path = f
        .owner
        .0
        .host
        .state_directory()
        .join("server-login-sdk-resume.json");
    let original = std::fs::read(&path).unwrap();
    f.current.store(false, Ordering::Release);
    assert_eq!(
        f.owner
            .execute(crate::native_owner::Command::Projects)
            .await
            .unwrap_err()
            .code,
        "matrix_account_changed"
    );
    assert_eq!(std::fs::read(path).unwrap(), original);
    assert_eq!(resume_state(&f), "active");
    assert!(f.owner.identity().await.is_err());
    f.stop().await;
}
