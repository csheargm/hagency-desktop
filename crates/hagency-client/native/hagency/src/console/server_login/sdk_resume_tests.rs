mod sdk_resume_tests {
    use super::*;

    fn source() -> Arc<Source> {
        Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn count(records: &Arc<std::sync::Mutex<Vec<(String, Value)>>>, suffix: &str) -> usize {
        records
            .lock()
            .unwrap()
            .iter()
            .filter(|(p, _)| p.ends_with(suffix))
            .count()
    }

    struct ExplicitSignIn(Arc<Source>, AtomicBool);
    impl MatrixTokenSource for ExplicitSignIn {
        fn permits_explicit_reenrollment(&self) -> bool {
            self.0.current.load(Ordering::Acquire) && self.1.load(Ordering::Acquire)
        }
        fn consume_explicit_reenrollment(&self) -> bool {
            self.0.current.load(Ordering::Acquire) && self.1.swap(false, Ordering::AcqRel)
        }
        fn access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            self.0.access_token()
        }
    }

    struct SingleSignIn {
        source: Arc<Source>,
        available: AtomicBool,
    }
    impl MatrixTokenSource for SingleSignIn {
        fn permits_explicit_reenrollment(&self) -> bool {
            self.source.current.load(Ordering::Acquire) && self.available.load(Ordering::Acquire)
        }
        fn consume_explicit_reenrollment(&self) -> bool {
            self.source.current.load(Ordering::Acquire)
                && self.available.swap(false, Ordering::AcqRel)
        }
        fn access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            self.source.access_token()
        }
    }

    #[tokio::test]
    async fn sdk_successful_authorization_consumes_signin_before_later_remote_revocation() {
        for restore in [false, true] {
            let mode = Arc::new(std::sync::atomic::AtomicU16::new(0));
            let (origin, records, task) =
                fixture_controlled("@owner:test", None, mode.clone()).await;
            let root =
                hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let state = root.path().join("state");
            if restore {
                let first = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
                    .await
                    .unwrap();
                first.suspend().await.unwrap();
                drop(first);
            }
            let sdk = Arc::new(SingleSignIn {
                source: source(),
                available: AtomicBool::new(true),
            });
            let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", sdk.clone())
                .await
                .unwrap();
            assert_eq!(count(&records, "/devices"), 1);
            let console = native.test_console();
            let login = &console.0.server_login;
            mode.store(9, Ordering::Release);
            assert_eq!(
                login
                    .authorize_matrix(console, &origin, "@owner:test", sdk.clone())
                    .await
                    .err(),
                Some("sign_in_required")
            );
            mode.store(0, Ordering::Release);
            assert_eq!(
                login
                    .authorize_matrix(console, &origin, "@owner:test", sdk.clone())
                    .await
                    .err(),
                Some("native_resume_revoked"),
                "an old explicit sign-in cannot replace a subsequently revoked native device"
            );
            assert_eq!(count(&records, "/devices"), 1);
            native.shutdown_checked().await.unwrap();
            task.abort();
        }
    }
    struct WithdrawnSignIn {
        source: Arc<Source>,
        checks: std::sync::atomic::AtomicUsize,
    }

    struct AdvertisedOnly(Arc<Source>);
    impl MatrixTokenSource for AdvertisedOnly {
        fn permits_explicit_reenrollment(&self) -> bool {
            true
        }
        fn access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            self.0.access_token()
        }
    }
    #[tokio::test]
    async fn sdk_explicit_reenrollment_requires_atomic_permission_before_device_post() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let first = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        first.shutdown_checked().await.unwrap();
        drop(first);
        let denied = NativeOwner::open_with_matrix(
            &state,
            &origin,
            "@owner:test",
            Arc::new(AdvertisedOnly(source())),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(denied.code, "matrix_account_changed");
        assert_eq!(
            count(&records, "/devices"),
            1,
            "a readable true flag alone cannot replace the device"
        );
        task.abort();
    }
    impl MatrixTokenSource for WithdrawnSignIn {
        fn permits_explicit_reenrollment(&self) -> bool {
            self.checks.fetch_add(1, Ordering::AcqRel) == 0
        }
        fn access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            self.source.access_token()
        }
    }

    #[tokio::test]
    async fn sdk_source_loss_during_renewal_closes_local_proof_without_revoking_saved_account() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let sdk = source();
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", sdk.clone())
            .await
            .unwrap();
        let login = &native.test_console().0.server_login;
        let old = login.authorized_device().await.unwrap();
        let original = std::fs::read(state.join("server-login-sdk-resume.json")).unwrap();
        for session in login.sessions.lock().await.values_mut() {
            session.checked = Instant::now() - Duration::from_secs(6);
        }
        // Reproduce the window before NativeOwner retirement observes the
        // trusted SDK service changing account. No host-ordering assumption.
        sdk.current.store(false, Ordering::Release);
        assert!(login.authorized_device().await.is_err());
        assert!(old.bearer().is_err());
        assert_eq!(
            std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
            original
        );
        assert!(login.revocations.lock().await.is_empty());
        assert_eq!(count(&records, "/sessions/current"), 0);
        native.suspend().await.unwrap();
        drop(native);
        let resumed = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        let current = resumed
            .test_console()
            .0
            .server_login
            .authorized_device()
            .await
            .unwrap();
        assert_eq!(current.device_id(), old.device_id());
        assert_eq!(current.generation(), old.generation());
        assert_eq!(count(&records, "/devices"), 1);
        resumed.shutdown_checked().await.unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn sdk_normal_restart_renews_same_original_device_without_registration() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        let old = native
            .test_console()
            .0
            .server_login
            .authorized_device()
            .await
            .unwrap();
        let original = std::fs::read(state.join("server-login-sdk-resume.json")).unwrap();
        native.suspend().await.unwrap();
        assert!(
            old.bearer().is_err(),
            "retired process capability cannot be restored"
        );
        drop(native);
        let resumed = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        let new = resumed
            .test_console()
            .0
            .server_login
            .authorized_device()
            .await
            .unwrap();
        assert_eq!(new.device_id(), old.device_id());
        assert_eq!(new.generation(), old.generation());
        assert_eq!(new.token, old.token);
        assert_eq!(
            std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
            original
        );
        assert_eq!(count(&records, "/devices"), 1);
        assert_eq!(count(&records, "/sessions/pasion"), 1);
        assert_eq!(count(&records, "/devices/current"), 1);
        assert!(count(&records, "/renew") >= 1);
        resumed.shutdown().await;
        task.abort();
    }

    #[tokio::test]
    async fn sdk_resume_unavailable_preserves_exact_credentials_and_never_rotates() {
        // 12: the device proof lies outside the local clock window. The clocks
        // disagree; the owner did not sign out.
        for mode_value in [2, 6, 7, 12] {
            let mode = Arc::new(std::sync::atomic::AtomicU16::new(0));
            let (origin, records, task) =
                fixture_controlled("@owner:test", None, mode.clone()).await;
            let root =
                hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let state = root.path().join("state");
            let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
                .await
                .unwrap();
            native.suspend().await.unwrap();
            drop(native);
            let original = std::fs::read(state.join("server-login-sdk-resume.json")).unwrap();
            mode.store(mode_value, Ordering::Release);
            let error = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
                .await
                .err()
                .unwrap();
            assert!(
                ServerLogin::preserves_sdk_resume(&error.code),
                "{}",
                error.code
            );
            assert_eq!(
                std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
                original
            );
            assert_eq!(count(&records, "/devices"), 1);
            assert_eq!(count(&records, "/sessions/pasion"), 1);
            mode.store(0, Ordering::Release);
            let resumed = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
                .await
                .unwrap();
            assert_eq!(
                resumed
                    .test_console()
                    .0
                    .server_login
                    .authorized_device()
                    .await
                    .unwrap()
                    .generation(),
                1
            );
            resumed.shutdown().await;
            task.abort();
        }
    }

    #[tokio::test]
    async fn sdk_resume_replaced_or_revoked_device_cannot_become_new_authority() {
        for mode_value in [5, 9] {
            let mode = Arc::new(std::sync::atomic::AtomicU16::new(0));
            let (origin, records, task) =
                fixture_controlled("@owner:test", None, mode.clone()).await;
            let root =
                hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let state = root.path().join("state");
            let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
                .await
                .unwrap();
            native.suspend().await.unwrap();
            drop(native);
            mode.store(mode_value, Ordering::Release);
            let error = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
                .await
                .err()
                .unwrap();
            assert!(matches!(
                error.code.as_str(),
                "matrix_account_mismatch" | "sign_in_required"
            ));
            assert_eq!(count(&records, "/devices"), 1);
            assert_eq!(count(&records, "/sessions/pasion"), 1);
            let value: Value = serde_json::from_slice(
                &std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(
                value["state"],
                if mode_value == 9 { "revoked" } else { "active" }
            );
            task.abort();
        }
    }

    #[tokio::test]
    async fn sdk_initial_registration_is_private_and_uncertain_registration_never_repeats() {
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(8));
        let (origin, records, task) = fixture_controlled("@owner:test", None, mode.clone()).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let result = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source()).await;
        assert_eq!(
            result.err().unwrap().code,
            "native_device_registration_unknown"
        );
        mode.store(0, Ordering::Release);
        let result = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source()).await;
        assert_eq!(
            result.err().unwrap().code,
            "native_device_registration_unknown"
        );
        assert_eq!(count(&records, "/devices"), 1);
        assert_eq!(count(&records, "/sessions/pasion"), 1);
        let bytes = std::fs::read(state.join("server-login-sdk-resume.json")).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("sdk-first"));
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["state"], "registering");
        task.abort();
    }

    #[tokio::test]
    async fn sdk_logout_persists_original_revocation_before_any_network() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        let login = &native.test_console().0.server_login;
        let old = login.authorized_device().await.unwrap();
        native.retire();
        login.revoke_sdk_resume().unwrap();
        assert!(old.bearer().is_err());
        let value: Value = serde_json::from_slice(
            &std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["state"], "revoked");
        assert_eq!(
            count(&records, "/sessions/current"),
            0,
            "tombstone precedes revoke RPC"
        );
        native.shutdown().await;
        drop(native);
        let next = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source()).await;
        assert_eq!(next.err().unwrap().code, "native_resume_revoked");
        assert_eq!(count(&records, "/devices"), 1);
        task.abort();
    }

    #[tokio::test]
    async fn sdk_only_explicit_new_signin_can_replace_a_durable_logout() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        native.shutdown_checked().await.unwrap();
        assert_eq!(
            count(&records, "/sessions/current"),
            1,
            "saved and live native session queue one original revoke"
        );
        drop(native);
        let restored = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .err()
            .unwrap();
        assert_eq!(restored.code, "native_resume_revoked");
        assert_eq!(count(&records, "/devices"), 1);
        let fresh = NativeOwner::open_with_matrix(
            &state,
            &origin,
            "@owner:test",
            Arc::new(ExplicitSignIn(source(), AtomicBool::new(true))),
        )
        .await
        .unwrap();
        assert_eq!(count(&records, "/devices"), 2);
        assert_eq!(count(&records, "/sessions/pasion"), 2);
        let history = records.lock().unwrap().clone();
        let last_register = history
            .iter()
            .rposition(|(p, _)| p.ends_with("/devices"))
            .unwrap();
        let last_revoke = history
            .iter()
            .rposition(|(p, _)| p.ends_with("/sessions/current"))
            .unwrap();
        assert!(
            last_revoke < last_register,
            "old remote native session is revoked before fresh registration"
        );
        fresh.shutdown_checked().await.unwrap();
        let before = count(&records, "/sessions/current");
        fresh.shutdown_checked().await.unwrap();
        assert_eq!(
            count(&records, "/sessions/current"),
            before + 1,
            "a later explicit cleanup may retry the saved original, but never queues it twice in one drain"
        );
        task.abort();
    }

    #[tokio::test]
    async fn sdk_logout_wins_a_late_original_device_restore_reply() {
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let gate = Arc::new(ProjectResponseGate::default());
        let (origin, records, task) =
            fixture_with_project_gate("@owner:test", None, mode.clone(), Some(gate.clone())).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        native.suspend().await.unwrap();
        drop(native);
        mode.store(10, Ordering::Release);
        let pending = Arc::new(std::sync::Mutex::new(None::<NativeOwner>));
        let saved = pending.clone();
        let next_state = state.clone();
        let next_origin = origin.clone();
        let open = tokio::spawn(async move {
            NativeOwner::open_with_matrix_guarded(
                &next_state,
                &next_origin,
                "@owner:test",
                source(),
                move |owner| {
                    *saved.lock().unwrap() = Some(owner.clone());
                    Ok(())
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
            .await
            .unwrap();
        let pending = pending.lock().unwrap().take().unwrap();
        pending.retire();
        pending
            .test_console()
            .0
            .server_login
            .revoke_sdk_resume()
            .unwrap();
        gate.release.notify_one();
        assert_eq!(
            open.await.unwrap().err().unwrap().code,
            "matrix_account_changed"
        );
        pending.shutdown_checked().await.unwrap();
        drop(pending);
        let record: Value = serde_json::from_slice(
            &std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(record["state"], "revoked");
        assert_eq!(count(&records, "/devices"), 1);
        task.abort();
    }

    #[tokio::test]
    async fn sdk_unopened_saved_account_logout_is_exact_locked_and_durable() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let absent = root.path().join("absent");
        assert!(
            !NativeOwner::revoke_saved_matrix_resumption(&absent, &origin, "@owner:test").unwrap()
        );
        assert!(!absent.exists());
        let state = root.path().join("state");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        let before = std::fs::read(state.join("server-login-sdk-resume.json")).unwrap();
        assert_eq!(
            NativeOwner::revoke_saved_matrix_resumption(&state, &origin, "@owner:test")
                .unwrap_err()
                .code,
            "native_owner_state_busy"
        );
        assert_eq!(
            std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
            before
        );
        native.suspend().await.unwrap();
        drop(native);
        assert_eq!(
            NativeOwner::revoke_saved_matrix_resumption(&state, &origin, "@other:test")
                .unwrap_err()
                .code,
            "matrix_account_mismatch"
        );
        assert_eq!(
            std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
            before
        );
        assert!(
            NativeOwner::revoke_saved_matrix_resumption(&state, &origin, "@owner:test").unwrap()
        );
        assert!(
            NativeOwner::revoke_saved_matrix_resumption(&state, &origin, "@owner:test").unwrap()
        );
        assert_eq!(
            count(&records, "/sessions/current"),
            0,
            "saved-account logout performs no network call"
        );
        let pending: Value = serde_json::from_slice(
            &std::fs::read(state.join("server-login-revocations.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(pending.as_array().unwrap().len(), 1);
        let error = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "native_resume_revoked");
        assert_eq!(count(&records, "/devices"), 1);
        task.abort();
    }

    #[tokio::test]
    async fn sdk_resume_record_cannot_be_copied_to_another_owner_root() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("original");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        native.suspend().await.unwrap();
        drop(native);
        let other = root.path().join("other");
        crate::owner_host::initialize(&other).unwrap();
        hagency_store::private::replace(
            &other.join("server-login-sdk-resume.json"),
            &std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
        )
        .unwrap();
        assert!(
            NativeOwner::revoke_saved_matrix_resumption(&other, &origin, "@owner:test").is_err()
        );
        assert!(
            NativeOwner::open_with_matrix(&other, &origin, "@owner:test", source())
                .await
                .is_err()
        );
        assert_eq!(count(&records, "/devices"), 1);
        let resumed = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        resumed.shutdown().await;
        task.abort();
    }

    #[tokio::test]
    async fn sdk_missing_saved_credential_never_silently_registers_a_known_owner() {
        let (origin, records, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        native.suspend().await.unwrap();
        drop(native);
        std::fs::remove_file(state.join("server-login-sdk-resume.json")).unwrap();
        let error = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "native_resume_missing");
        assert_eq!(count(&records, "/devices"), 1);
        let withdrawn = Arc::new(WithdrawnSignIn {
            source: source(),
            checks: std::sync::atomic::AtomicUsize::new(0),
        });
        let error = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", withdrawn)
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "matrix_account_changed");
        assert_eq!(
            count(&records, "/devices"),
            1,
            "fresh-login provenance is rechecked before the generation-changing POST"
        );
        let explicit = NativeOwner::open_with_matrix(
            &state,
            &origin,
            "@owner:test",
            Arc::new(ExplicitSignIn(source(), AtomicBool::new(true))),
        )
        .await
        .unwrap();
        assert_eq!(count(&records, "/devices"), 2);
        explicit.shutdown_checked().await.unwrap();
        task.abort();
    }

    #[tokio::test]
    async fn sdk_legacy_explicit_logout_command_tombstones_the_saved_credential() {
        let (origin, _, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let state = root.path().join("state");
        let native = NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
            .await
            .unwrap();
        native.execute(Command::Logout).await.unwrap();
        let value: Value = serde_json::from_slice(
            &std::fs::read(state.join("server-login-sdk-resume.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["state"], "revoked");
        native.suspend().await.unwrap();
        drop(native);
        assert_eq!(
            NativeOwner::open_with_matrix(&state, &origin, "@owner:test", source())
                .await
                .err()
                .unwrap()
                .code,
            "native_resume_revoked"
        );
        task.abort();
    }
}
