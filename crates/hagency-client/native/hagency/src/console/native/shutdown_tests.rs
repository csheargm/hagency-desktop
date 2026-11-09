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

async fn projects(f: &Fixture, guarded: bool) -> Result<Value, NativeError> {
    if guarded {
        f.owner
            .execute_guarded(crate::native_owner::Command::Projects, Arc::new(|| true))
            .await
    } else {
        f.owner
            .execute(crate::native_owner::Command::Projects)
            .await
    }
}

struct CommandSource {
    current: Source,
    failed: Arc<AtomicBool>,
    code: &'static str,
}

impl MatrixTokenSource for CommandSource {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            if self.failed.load(Ordering::Acquire) {
                return Err(failure(
                    if self.code.ends_with("unavailable") {
                        503
                    } else {
                        401
                    },
                    self.code,
                ));
            }
            self.current.access_token().await
        })
    }
}

async fn command_source(f: &Fixture, code: &'static str) -> Arc<AtomicBool> {
    let failed = Arc::new(AtomicBool::new(false));
    let source: Arc<dyn MatrixTokenSource> = Arc::new(CommandSource {
        current: Source(f.current.clone()),
        failed: failed.clone(),
        code,
    });
    let cookie = f
        .owner
        .0
        .host
        .console
        .0
        .server_login
        .authorize_matrix(
            &f.owner.0.host.console,
            &f.owner.0.expected_origin,
            &f.owner.0.expected_owner,
            source.clone(),
        )
        .await
        .unwrap();
    *f.owner.0.matrix_source.lock().unwrap() = Some(source);
    *f.owner.0.cookie.lock().await = format!("hagency_console={cookie}");
    failed
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
    for guarded in [false, true] {
        let f = Fixture::new().await;
        let retained = f.owner.management(package(20)).await.unwrap();
        let path = f
            .owner
            .0
            .host
            .state_directory()
            .join("server-login-sdk-resume.json");
        let original = std::fs::read(&path).unwrap();
        f.current.store(false, Ordering::Release);
        assert_eq!(
            projects(&f, guarded).await.unwrap_err().code,
            "matrix_account_changed"
        );
        assert_eq!(f.state.lock().unwrap().project_reads, 0);
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(resume_state(&f), "active");
        assert!(f.owner.identity().await.is_err());
        assert!(retained.authorized().await.is_err());
        f.stop().await;
    }
}

#[tokio::test]
async fn native_owner_post_request_source_loss_retires_ordinary_and_guarded_commands() {
    for guarded in [false, true] {
        let f = Fixture::new().await;
        let failed = command_source(&f, "matrix_account_changed").await;
        let retained = f.owner.management(package(20)).await.unwrap();
        let path = f
            .owner
            .0
            .host
            .state_directory()
            .join("server-login-sdk-resume.json");
        let original = std::fs::read(&path).unwrap();
        f.state.lock().unwrap().fail_source_at_projects = Some(failed);
        assert_eq!(
            projects(&f, guarded).await.unwrap_err().code,
            "matrix_account_changed"
        );
        assert_eq!(f.state.lock().unwrap().project_reads, 1);
        assert_eq!(std::fs::read(path).unwrap(), original);
        assert_eq!(resume_state(&f), "active");
        assert!(f.owner.identity().await.is_err());
        assert!(retained.authorized().await.is_err());
        f.stop().await;
    }
}

#[tokio::test]
async fn native_owner_temporary_command_source_failure_preserves_active_work_and_recovery() {
    for guarded in [false, true] {
        for after_request in [false, true] {
            for code in [
                "matrix_authorization_unavailable",
                "hagency_server_unavailable",
            ] {
                let f = Fixture::new().await;
                let failed = command_source(&f, code).await;
                let retained = f.owner.management(package(20)).await.unwrap();
                let path = f
                    .owner
                    .0
                    .host
                    .state_directory()
                    .join("server-login-sdk-resume.json");
                let original = std::fs::read(&path).unwrap();
                // A mistaken suspension would retire this owner and wait for
                // the in-flight activity, instead of returning the transient error.
                let activity = f.owner.0.configuration_custody.enter().unwrap();
                if after_request {
                    f.state.lock().unwrap().fail_source_at_projects = Some(failed.clone());
                } else {
                    failed.store(true, Ordering::Release);
                }
                let error = tokio::time::timeout(Duration::from_secs(3), projects(&f, guarded))
                    .await
                    .expect("temporary source failure must not drain active work")
                    .unwrap_err();
                assert_eq!(error.status, 503);
                assert_eq!(error.code, code);
                assert_eq!(
                    f.state.lock().unwrap().project_reads,
                    usize::from(after_request)
                );
                assert!(!f.owner.0.configuration_stopped.load(Ordering::Acquire));
                assert_eq!(std::fs::read(&path).unwrap(), original);
                assert_eq!(resume_state(&f), "active");

                f.state.lock().unwrap().fail_source_at_projects = None;
                failed.store(false, Ordering::Release);
                assert!(f.owner.identity().await.is_ok());
                assert!(retained.authorized().await.is_ok());
                assert_eq!(projects(&f, guarded).await.unwrap()["projects"], json!([]));
                assert_eq!(
                    f.state.lock().unwrap().project_reads,
                    usize::from(after_request) + 1
                );
                assert_eq!(std::fs::read(&path).unwrap(), original);
                drop(activity);
                f.stop().await;
            }
        }
    }
}
