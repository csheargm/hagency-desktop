fn provider_fixture(subject: &str) -> VerifiedProviderAccount {
    VerifiedProviderAccount::new(
        "codex",
        "https://provider.test",
        subject,
        Instant::now() + Duration::from_secs(25),
    )
    .unwrap()
}

struct DelayedEnrollmentSource {
    source: Source,
    journal: PathBuf,
    expires: Arc<std::sync::Mutex<Option<Instant>>>,
    calls_after_save: Arc<std::sync::atomic::AtomicUsize>,
    first_check_live: Arc<AtomicBool>,
}
impl MatrixTokenSource for DelayedEnrollmentSource {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            let expires = *self.expires.lock().unwrap();
            if let Some(expires) = expires
                && self.journal.exists()
            {
                let count = self.calls_after_save.fetch_add(1, Ordering::AcqRel);
                if count == 0 {
                    self.first_check_live
                        .store(Instant::now() < expires, Ordering::Release);
                }
                if count == 1 {
                    tokio::time::sleep_until((expires + Duration::from_millis(20)).into()).await;
                }
            }
            self.source.access_token().await
        })
    }
}

#[tokio::test]
async fn native_owner_enrollment_expiry_during_final_sdk_await_does_not_return_proof() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let path = f.enrollment_path().await;
    let expires = Arc::new(std::sync::Mutex::new(None));
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let first_live = Arc::new(AtomicBool::new(false));
    let source = Arc::new(DelayedEnrollmentSource {
        source: Source(f.source.clone()),
        journal: path.clone(),
        expires: expires.clone(),
        calls_after_save: calls.clone(),
        first_check_live: first_live.clone(),
    });
    let mut f = f.restart_with_token_source(source).await;
    f.enable_factory().await;
    let result = f
        .service
        .verified_enrollment_with(
            &f.registry,
            "fixture_contribution".into(),
            move |_| async move {
                let until = Instant::now() + Duration::from_secs(2);
                *expires.lock().unwrap() = Some(until);
                Ok(VerifiedProviderAccount::new(
                    "codex",
                    "https://provider.test",
                    "synthetic-account",
                    until,
                )
                .unwrap())
            },
        )
        .await;
    assert!(
        first_live.load(Ordering::Acquire),
        "the first post-save SDK check must precede expiry"
    );
    assert!(
        calls.load(Ordering::Acquire) >= 2,
        "the delayed post-save SDK check must run; further source checks remain allowed"
    );
    assert!(
        result.is_err(),
        "a proof expired during the final SDK await must not be returned"
    );
    assert!(
        path.exists(),
        "the original native ID remains durable despite failed proof return"
    );
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}
impl Fixture {
    async fn enable_factory(&mut self) {
        self.service.manager.retire();
        let mut p = package(2, true);
        p.permissions
            .extend([Permission::ReadProviders, Permission::ConnectProviders]);
        self.service = OwnerGrantIntents::new(self.owner.management(p).await.unwrap());
    }
    async fn factory(&self) -> Result<VerifiedManagementEnrollment, NativeError> {
        self.service
            .verified_enrollment_with(
                &self.registry,
                "fixture_contribution".into(),
                |runtime| async move {
                    assert!(matches!(runtime, super::super::ManagementRuntime::Codex));
                    Ok(provider_fixture("synthetic-account"))
                },
            )
            .await
    }
    async fn enrollment_path(&self) -> PathBuf {
        let cookie = self.service.cookie().await.unwrap();
        self.service
            .io(&cookie, Permission::ConsentGrants)
            .await
            .unwrap()
            .path
            .with_file_name("management-enrollments-v1.json")
    }
    async fn binding(&self, proof: VerifiedManagementEnrollment) -> OwnerGrantBinding {
        self.registry
            .prepare_capacity_grant(
                proof,
                OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                    calendar: None,
                    command_id: "probe_command".into(),
                    grant_id: "probe_grant".into(),
                    coordinator_mxid: "@owner:test".into(),
                    delegation_expires_at_ms: now_ms().unwrap() + 60000,
                    allow_self_approval: false,
                    eligible_managers: vec!["@manager:test".into()],
                    allocated_tokens: 100,
                },
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap()
            .binding()
            .unwrap()
    }
}

#[tokio::test]
async fn native_owner_enrollment_persists_native_id_and_reuses_it_after_real_restart() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let first = f.binding(f.factory().await.unwrap()).await;
    assert_ne!(first.enrollment_id, "fixture_contribution");
    assert_eq!(first.enrollment_id.len(), 48);
    assert!(first.verified_resource_binding.is_some());
    let path = f.enrollment_path().await;
    let bytes = std::fs::read(&path).unwrap();
    assert!(!path.parent().unwrap().join("agent-local.sqlite").exists());
    let mut f = f.restart().await;
    f.enable_factory().await;
    let next = f.binding(f.factory().await.unwrap()).await;
    assert_eq!(first.enrollment_id, next.enrollment_id);
    assert_eq!(
        first.verified_resource_binding,
        next.verified_resource_binding
    );
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_requires_provider_grants_and_exact_quota_account() {
    let mut f = Fixture::new().await;
    assert!(f.factory().await.is_err());
    f.enable_factory().await;
    let result = f
        .service
        .verified_enrollment_with(&f.registry, "fixture_contribution".into(), |_| async {
            Ok(provider_fixture("another-quota-account"))
        })
        .await;
    assert!(result.is_err());
    assert!(!f.enrollment_path().await.exists());
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_rejects_malformed_foreign_and_unadmitted_central_state() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let original = f.admission.lock().unwrap().clone();
    for (field, value) in [
        ("ownerMxid", json!("@foreign:test")),
        ("executorDeviceId", json!("other")),
        ("executorGeneration", json!(2)),
        ("state", json!("revoked")),
        ("resourceRevision", json!("f".repeat(64))),
        ("runtime", json!("claude")),
    ] {
        let mut changed = original.clone();
        changed["admission"]["contribution"][field] = value;
        *f.admission.lock().unwrap() = changed;
        let refused = f.factory().await.err();
        assert!(refused.is_some(), "{field}");
        if field == "runtime" {
            // The central record names another tool than the local resource:
            // refused by name, before any provider is observed on its word.
            assert_eq!(refused.unwrap().code, "management_enrollment_changed");
        }
        assert!(!f.enrollment_path().await.exists());
    }
    let mut changed = original;
    changed["admission"]["request"]["state"] = json!("pending");
    *f.admission.lock().unwrap() = changed;
    assert!(f.factory().await.is_err());
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_rechecks_central_state_after_provider_observation() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let admission = f.admission.clone();
    let result = f
        .service
        .verified_enrollment_with(
            &f.registry,
            "fixture_contribution".into(),
            move |_| async move {
                admission.lock().unwrap()["admission"]["contribution"]["coordinatorMxid"] =
                    json!("@replacement:test");
                Ok(provider_fixture("synthetic-account"))
            },
        )
        .await;
    assert_eq!(result.err().unwrap().code, "management_enrollment_changed");
    assert!(!f.enrollment_path().await.exists());
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_refuses_admission_round_changed_during_observation() {
    // Identical resource/provider/coordinator fields do not mean that the
    // administrator's original admission is still the same decision.
    for change in ["request_id", "contribution_revision", "request_revision"] {
        let mut f = Fixture::new().await;
        f.enable_factory().await;
        let admission = f.admission.clone();
        let result = f.service.verified_enrollment_with(
            &f.registry,
            "fixture_contribution".into(),
            move |_| async move {
                let mut reply = admission.lock().unwrap();
                let current = &mut reply["admission"];
                match change {
                    "request_id" => {
                        current["contribution"]["requestId"] = json!("replacement_admission");
                        current["request"]["id"] = json!("replacement_admission");
                    }
                    "contribution_revision" => current["contribution"]["revision"] = json!(4),
                    "request_revision" => current["request"]["revision"] = json!(3),
                    _ => unreachable!(),
                }
                Ok(provider_fixture("synthetic-account"))
            },
        ).await;
        assert_eq!(result.err().expect(change).code, "management_enrollment_changed");
        assert!(!f.enrollment_path().await.exists(), "{change}");
        assert_eq!(f.count("owner_capacity_grants"), 0, "{change}");
        f.stop().await;
    }
}

#[tokio::test]
async fn native_owner_enrollment_sdk_retirement_during_observation_does_not_issue_identity() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let path = f.enrollment_path().await;
    let source = f.source.clone();
    let result = f
        .service
        .verified_enrollment_with(
            &f.registry,
            "fixture_contribution".into(),
            move |_| async move {
                source.store(false, Ordering::Release);
                Ok(provider_fixture("synthetic-account"))
            },
        )
        .await;
    assert!(result.is_err());
    assert!(!path.exists());
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_changed_canonical_account_after_review_is_denied_in_writer() {
    for change in ["provider", "access_revision"] {
        let mut f = Fixture::new().await;
        f.enable_factory().await;
        let proof = f.factory().await.unwrap();
        let review = f
            .service
            .prepare(&f.registry, proof, Fixture::choices())
            .await
            .unwrap();
        let db = rusqlite::Connection::open(f.root.path().join("registry/domain.sqlite3")).unwrap();
        if change == "provider" {
            db.execute(
                "UPDATE owner_resource_accounts SET provider_digest=?1",
                ["f".repeat(64)],
            )
            .unwrap();
        } else {
            db.execute("UPDATE owner_resource_grants SET revision=revision+1", [])
                .unwrap();
        }
        assert!(matches!(
            f.service.confirm(&f.registry, review).await.unwrap().state,
            OwnerGrantState::OutcomeUnknown
        ));
        assert_eq!(f.count("owner_capacity_grants"), 0);
        assert_eq!(f.count("owner_capacity_receipts"), 0);
        f.stop().await;
    }
}

#[tokio::test]
async fn native_owner_enrollment_lost_journal_cannot_replace_a_committed_enrollment() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let proof = f.factory().await.unwrap();
    let review = f
        .service
        .prepare(&f.registry, proof, Fixture::choices())
        .await
        .unwrap();
    f.service.confirm(&f.registry, review).await.unwrap();
    let path = f.enrollment_path().await;
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(f.factory().await.is_err());
    std::fs::remove_file(path.with_file_name("management-enrollments-v1.identity")).unwrap();
    assert_eq!(
        f.factory().await.err().unwrap().code,
        "management_enrollment_history_required"
    );
    assert!(!path.exists());
    assert!(!bytes.is_empty());
    assert_eq!(f.count("owner_capacity_grants"), 1);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_stale_original_binding_and_foreign_registry_cannot_rebind() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let proof = f.factory().await.unwrap();
    drop(proof);
    let path = f.enrollment_path().await;
    let bytes = std::fs::read(&path).unwrap();
    f.admission.lock().unwrap()["admission"]["contribution"]["coordinatorMxid"] =
        json!("@replacement:test");
    assert_eq!(
        f.factory().await.err().unwrap().code,
        "management_enrollment_changed"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    f.admission.lock().unwrap()["admission"]["contribution"]["coordinatorMxid"] =
        json!("@owner:test");
    let other =
        OwnerResourceRegistry::isolated_for_tests(&f.root.path().join("other-registry")).unwrap();
    let cookie = f.service.cookie().await.unwrap();
    let io = f
        .service
        .io(&cookie, Permission::ConsentGrants)
        .await
        .unwrap();
    let receipt = other
        .prepare_enrollment(
            io.profile.clone(),
            "other_registry_enrollment".into(),
            provider_fixture("synthetic-account"),
            "gpt-5.6-sol".into(),
            None,
            1000,
            Instant::now() + Duration::from_secs(20),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert_eq!(receipt.resource_id, f.resource);
    let result = f
        .service
        .verified_enrollment_with(&other, "fixture_contribution".into(), |_| async {
            Ok(provider_fixture("synthetic-account"))
        })
        .await;
    assert_eq!(result.err().unwrap().code, "management_enrollment_changed");
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    other.shutdown().await.unwrap();
    drop(io);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_original_review_cannot_cross_a_new_admission_after_restart() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let review = f.service.prepare(&f.registry, f.factory().await.unwrap(), Fixture::choices())
        .await.unwrap();
    let intent = review.summary().intent_id;
    drop(review);
    let path = f.enrollment_path().await;
    let original_enrollment = std::fs::read(&path).unwrap();
    let history = f.history().await;
    let original_history = std::fs::read(&history.path).unwrap();
    let original_admission = f.admission.lock().unwrap().clone();
    let mut f = f.restart().await;
    f.enable_factory().await;
    {
        let mut reply = f.admission.lock().unwrap();
        reply["admission"]["contribution"]["requestId"] = json!("readmitted_request");
        reply["admission"]["contribution"]["revision"] = json!(5);
        reply["admission"]["request"]["id"] = json!("readmitted_request");
    }
    assert_eq!(f.factory().await.err().expect("old enrollment must not adopt a new request").code,
        "management_enrollment_changed");
    assert_eq!(std::fs::read(&path).unwrap(), original_enrollment);
    assert_eq!(std::fs::read(&history.path).unwrap(), original_history);
    assert_eq!(f.count("owner_capacity_grants"), 0);
    let old = f.service.recover(&f.registry, intent.clone()).await.unwrap();
    assert_eq!(old.summary.intent_id, intent);
    assert!(matches!(old.state, OwnerGrantState::ReviewRequired));
    // Returning to the exact original decision still recovers its original ID.
    *f.admission.lock().unwrap() = original_admission;
    assert!(matches!(f.service.review_existing(&f.registry, f.factory().await.unwrap(), intent)
        .await.unwrap(), OwnerGrantReviewResult::Review(_)));
    assert_eq!(std::fs::read(path).unwrap(), original_enrollment);
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_legacy_initial_decision_keeps_bytes_but_cannot_claim_later_round() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let original = f.binding(f.factory().await.unwrap()).await;
    let path = f.enrollment_path().await;
    // Reconstruct the exact pre-round journal format, including its old digest.
    let mut legacy: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let entry = &mut legacy["entries"][0];
    entry["binding"].as_object_mut().unwrap().remove("admissionRequestId");
    entry["bindingDigest"] = json!(digest(&entry["binding"]).unwrap());
    let bytes = serde_json::to_vec(&legacy).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut f = f.restart().await;
    f.enable_factory().await;
    assert_eq!(f.binding(f.factory().await.unwrap()).await.enrollment_id, original.enrollment_id);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    {
        let mut reply = f.admission.lock().unwrap();
        reply["admission"]["contribution"]["revision"] = json!(5);
        reply["admission"]["contribution"]["requestId"] = json!("later_admission");
        reply["admission"]["request"]["id"] = json!("later_admission");
    }
    assert_eq!(f.factory().await.err().expect("legacy binding lacks later admission proof").code,
        "management_enrollment_history_required");
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_enrollment_rejects_invalid_persisted_admission_request_identity() {
    let mut f = Fixture::new().await;
    f.enable_factory().await;
    let _ = f.factory().await.unwrap();
    let path = f.enrollment_path().await;
    let mut journal: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let entry = &mut journal["entries"][0];
    entry["binding"]["admissionRequestId"] = json!("../another-request");
    entry["bindingDigest"] = json!(digest(&entry["binding"]).unwrap());
    let bytes = serde_json::to_vec(&journal).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(f.factory().await.is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}
