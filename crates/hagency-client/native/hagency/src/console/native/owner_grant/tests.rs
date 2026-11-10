use super::*;

#[tokio::test]
async fn owner_is_fixed_as_coordinator_before_a_capacity_review() {
    let f = Fixture::new().await;
    let mut choices = Fixture::choices();
    choices.coordinator_mxid = "@another:test".into();
    let error = f.service.prepare(&f.registry, f.enrollment().await, choices).await.err().unwrap();
    assert_eq!(error.code, "resource_owner_is_coordinator");
    assert!(f.history().await.data.records.is_empty());
    let review = f.review().await;
    assert_eq!(review.summary().coordinator_mxid, review.summary().owner_mxid);
}
use crate::native_owner::{ManagementPackage, MatrixAccessToken, MatrixTokenSource, NativeOwner};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
include!("enrollment_tests.rs");
include!("admission_guard_tests.rs");
include!("publication_tests.rs");
include!("publication_observation_tests.rs");
include!("succession_tests.rs");
include!("recurring_tests.rs");
#[cfg(feature = "resource-adoption")]
include!("current_profile_fixture.rs");
#[cfg(feature = "resource-adoption")]
include!("adoption_connection_tests.rs");
#[cfg(feature = "resource-adoption")]
include!("adoption_provenance_tests.rs");

struct Source(Arc<AtomicBool>);
impl MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            if !self.0.load(Ordering::Acquire) {
                return Err(failure(401, "matrix_account_changed"));
            }
            Ok(MatrixAccessToken {
                access_token: "fixture-sdk".into(),
                client_id: "sdk-client".into(),
            })
        })
    }
}
struct RecordRejectingSource {
    source: Source,
    journal: PathBuf,
}
impl MatrixTokenSource for RecordRejectingSource {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            if std::fs::read(&self.journal)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Journal>(&bytes).ok())
                .is_some_and(|journal| journal.records.iter().any(|r| r.state == State::Recorded))
            {
                return Err(failure(401, "matrix_account_changed"));
            }
            self.source.access_token().await
        })
    }
}
type AdmissionState = Arc<std::sync::Mutex<serde_json::Value>>;
#[path = "readmission_tests.rs"]
mod readmission;
async fn server() -> (
    String,
    tokio::task::JoinHandle<()>,
    AdmissionState,
    PublicationFixture,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let server_origin = origin.clone();
    let admission = Arc::new(std::sync::Mutex::new(json!(null)));
    let server_admission = admission.clone();
    let publications = Arc::new(std::sync::Mutex::new(PublicationFixtureState::default()));
    let server_publications = publications.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            let origin = server_origin.clone();
            let admission = server_admission.clone();
            let publications = server_publications.clone();
            tokio::spawn(async move {
                let mut raw = Vec::new();
                let mut chunk = [0u8; 4096];
                let split = loop {
                    let n = stream.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                    assert!(raw.len() < 32768);
                    if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8_lossy(&raw[..split]).to_string();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.split_once(':')
                            .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                            .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                while raw.len() < split + length {
                    let n = stream.read(&mut chunk).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    raw.extend_from_slice(&chunk[..n]);
                }
                let path = headers
                    .lines()
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap();
                let body: serde_json::Value =
                    serde_json::from_slice(&raw[split..split + length]).unwrap_or(json!(null));
                let at = now_ms().unwrap();
                let issuer = format!("{origin}auth/");
                let value = match path {
                    "/hagency/api/v1/discovery" => {
                        json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"],"homeserver":origin,"issuer":issuer})
                    }
                    "/hagency/api/v1/sessions/pasion" => {
                        json!({"token":"a".repeat(64),"userId":"uid","mxid":"@owner:test","validUntilMs":at+30000})
                    }
                    "/hagency/api/v1/identity" => {
                        json!({"userId":"uid","mxid":"@owner:test","issuer":issuer,"subject":"sub","clientId":"sdk-client","validUntilMs":at+30000})
                    }
                    "/hagency/api/v1/devices" => {
                        let mut s=publications.lock().unwrap();
                        s.device_posts += 1;
                        json!({"deviceId":s.device_override.as_deref().unwrap_or("device"),"token":"b".repeat(64),"generation":s.device_generation.unwrap_or(1),"validUntilMs":at+30000})
                    }
                    "/hagency/api/v1/devices/current" => {
                        let s=publications.lock().unwrap();
                        json!({"userId":"uid","mxid":"@owner:test","issuer":issuer,"subject":"sub","clientId":"sdk-client","deviceId":s.device_override.as_deref().unwrap_or("device"),"generation":s.device_generation.unwrap_or(1),"validUntilMs":at+30000})
                    }
                    "/hagency/api/v1/sessions/current/renew" => json!({"validUntilMs":at+30000}),
                    "/hagency/api/v1/management/session" => {
                        let state = publications.lock().unwrap();
                        let mut features = if state.allocations_disabled { vec![] }
                        else if state.admission_receipts && state.monthly_regrant {
                            vec!["admission-bound-grants-v2", "pool-monthly-regrant-v1"]
                        } else if state.admission_receipts {
                            vec!["admission-bound-grants-v2"]
                        } else { vec!["project-reservations-v1"] };
                        if state.calendar && !state.allocations_disabled { features.push("calendar-grants-v1"); }
                        if state.pool_renewal && !state.allocations_disabled { features.extend(["pool-grant-limits-v1", "pool-renewal-policy-v1"]); }
                        if state.succession && !state.allocations_disabled {
                            features.push("pool-grant-succession-v1");
                        }
                        json!({"features": features})
                    }
                    "/hagency/api/v1/management/contributions/fixture_contribution" => {
                        let mut state = publications.lock().unwrap();
                        state.contribution_reads += 1;
                        state.invalidate("contribution");
                        admission.lock().unwrap().clone()
                    }
                    #[cfg(feature = "resource-adoption")]
                    "/hagency/api/v1/management/contributions" | "/hagency/api/v1/management/contributions/fixture_contribution/reoffer" => {
                        let Some(value) = provenance_fixture_reply(&body, &publications) else { return; };
                        value
                    }
                    #[cfg(feature = "resource-adoption")]
                    path if path.ends_with("/fixture_contribution/resume") || path.ends_with("/fixture_contribution/verify") || path.contains("/management/commands/") => {
                        let Some(value)=connection_fixture_reply(path,&body,&admission,&publications) else {return};
                        value
                    }
                    path if path.starts_with("/hagency/api/v1/management/grants/") => {
                        let Some(result) = succession_fixture_reply(path, &body, &publications)
                        else {
                            return;
                        };
                        result
                    }
                    path if path.ends_with("/fixture_contribution/grants")
                        || path.contains("/allocation-commands/") =>
                    {
                        let Some(result) = publication_fixture_reply(path, &body, &publications)
                        else {
                            return;
                        };
                        result
                    }
                    "/hagency/api/v1/sessions/current" => {
                        json!({})
                    }
                    // A current server: the database a device names opens its number space.
                    "/hagency/api/v1/management/executor-ledger/declare" => {
                        json!({"ledgerId":body["ledgerId"],"epoch":1})
                    }
                    _ => panic!("unexpected route in native grant-intent fixture: {path}"),
                };
                let body = value.to_string();
                let response = format!(
                    "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    if value.get("code").is_some() {
                        409
                    } else {
                        200
                    },
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (origin, task, admission, publications)
}
fn package(instance: u64, consent: bool) -> ManagementPackage {
    let mut permissions = vec![Permission::ReadResources];
    if consent {
        permissions.push(Permission::ConsentGrants);
    }
    ManagementPackage {
        digest: "a".repeat(64),
        instance,
        account_epoch: 1,
        permissions,
    }
}
struct Fixture {
    root: tempfile::TempDir,
    origin: String,
    source: Arc<AtomicBool>,
    owner: NativeOwner,
    service: OwnerGrantIntents,
    registry: OwnerResourceRegistry,
    resource: String,
    revision: String,
    task: tokio::task::JoinHandle<()>,
    admission: AdmissionState,
    publications: PublicationFixture,
}
impl Fixture {
    async fn new() -> Self {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let (origin, task, admission, publications) = server().await;
        let source = Arc::new(AtomicBool::new(true));
        let owner = NativeOwner::open_with_matrix(
            &root.path().join("host"),
            &origin,
            "@owner:test",
            Arc::new(Source(source.clone())),
        )
        .await
        .unwrap();
        let service = OwnerGrantIntents::new(owner.management(package(1, true)).await.unwrap());
        let registry =
            OwnerResourceRegistry::isolated_for_tests(&root.path().join("registry")).unwrap();
        let cookie = service.cookie().await.unwrap();
        let io = service
            .io(&cookie, Permission::ConsentGrants)
            .await
            .unwrap();
        let resource = registry
            .prepare_enrollment(
                io.profile.clone(),
                "intent_resource".into(),
                VerifiedProviderAccount::new(
                    "codex",
                    "https://provider.test",
                    "synthetic-account",
                    Instant::now() + Duration::from_secs(30),
                )
                .unwrap(),
                "gpt-5.6-sol".into(),
                None,
                1000,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap()
            .approve()
            .await
            .unwrap()
            .resource_id;
        let revision = registry
            .resource(&io.profile, resource.clone())
            .await
            .unwrap()
            .revision;
        drop(io);
        let at = now_ms().unwrap();
        *admission.lock().unwrap() = json!({"admission":{"contribution":{"id":"fixture_contribution","ownerMxid":"@owner:test","executorDeviceId":"device","executorGeneration":1,"resourceId":resource,"resourceRevision":revision,"label":"Fixture","runtime":"codex","capabilities":["coding"],"coordinatorMxid":"@owner:test","state":"admitted","revision":2,"requestId":"admission_request","createdAtMs":at,"updatedAtMs":at,"verification":null},"request":{"id":"admission_request","kind":"resource_admission","contributionId":"fixture_contribution","projectId":null,"allocationId":null,"projectName":null,"resourceLabel":null,"requesterMxid":"@owner:test","approverMxid":"@admin:test","state":"approved","revision":2,"createdAtMs":at,"expiresAtMs":at+60000,"decidedAtMs":at,"canDecide":false}},"observedAtMs":at});
        Self {
            root,
            origin,
            source,
            owner,
            service,
            registry,
            resource,
            revision,
            task,
            admission,
            publications,
        }
    }
    fn choices() -> OwnerGrantChoices {
        OwnerGrantChoices { renewal_mode: None, per_request_tokens: None,
            calendar: None,
            coordinator_mxid: "@owner:test".into(),
            delegation_expires_at_ms: now_ms().unwrap() + 60000,
            allow_self_approval: false,
            eligible_managers: vec!["@manager:test".into()],
            allocated_tokens: 700,
        }
    }
    async fn enrollment(&self) -> VerifiedManagementEnrollment {
        let cookie = self.service.cookie().await.unwrap();
        let io = self
            .service
            .io(&cookie, Permission::ConsentGrants)
            .await
            .unwrap();
        VerifiedManagementEnrollment::new(
            io.profile.clone(),
            "fixture_enrollment".into(),
            "fixture_contribution".into(),
            self.resource.clone(),
            self.revision.clone(),
            "@owner:test".into(),
            "device".into(),
            1,
            io.deadline,
        )
        .unwrap()
    }
    async fn review(&self) -> OwnerGrantReview {
        self.service
            .prepare(&self.registry, self.enrollment().await, Self::choices())
            .await
            .unwrap()
    }
    async fn history(&self) -> History {
        let cookie = self.service.cookie().await.unwrap();
        let io = self
            .service
            .io(&cookie, Permission::ReadResources)
            .await
            .unwrap();
        History::load(&io.path, io.identity).unwrap()
    }
    fn count(&self, table: &str) -> u64 {
        let db =
            rusqlite::Connection::open(self.root.path().join("registry/domain.sqlite3")).unwrap();
        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }
    async fn restart(self) -> Self {
        self.restart_with_recorded_fence(None).await
    }
    async fn restart_with_recorded_fence(self, journal: Option<PathBuf>) -> Self {
        let token_source: Arc<dyn MatrixTokenSource> = match journal {
            Some(journal) => Arc::new(RecordRejectingSource {
                source: Source(self.source.clone()),
                journal,
            }),
            None => Arc::new(Source(self.source.clone())),
        };
        self.restart_with_token_source(token_source).await
    }
    async fn restart_with_token_source(self, token_source: Arc<dyn MatrixTokenSource>) -> Self {
        let Self {
            root,
            origin,
            source,
            owner,
            service,
            registry,
            resource,
            revision,
            task,
            admission,
            publications,
        } = self;
        service.manager.retire();
        drop(service);
        owner.suspend().await.unwrap();
        drop(owner);
        let owner = NativeOwner::open_with_matrix(
            &root.path().join("host"),
            &origin,
            "@owner:test",
            token_source,
        )
        .await
        .unwrap();
        let service = OwnerGrantIntents::new(owner.management(package(1, true)).await.unwrap());
        Self {
            root,
            origin,
            source,
            owner,
            service,
            registry,
            resource,
            revision,
            task,
            admission,
            publications,
        }
    }
    async fn stop(self) {
        self.registry.shutdown().await.unwrap();
        self.owner.retire();
        self.task.abort();
    }
}

#[tokio::test]
async fn native_owner_grant_intent_precedes_confirmation_and_decline_has_no_hold() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    let h = f.history().await;
    assert_eq!(h.data.records.len(), 1);
    assert_eq!(h.data.records[0].state, State::Review);
    assert_eq!(h.data.records[0].original, review.original);
    assert!(h.path.parent().unwrap().read_dir().unwrap().all(|p| {
        !p.unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".sqlite3")
    }));
    assert_eq!(f.count("owner_capacity_grants"), 0);
    let summary = serde_json::to_string(&review.summary()).unwrap();
    assert!(
        !summary.contains("subject")
            && !summary.contains("profileDigest")
            && !summary.contains("receipt")
    );
    let result = f.service.decline(review).await.unwrap();
    assert!(matches!(result.state, OwnerGrantState::Declined));
    assert!(matches!(
        f.service.recover(&f.registry, id).await.unwrap().state,
        OwnerGrantState::Declined
    ));
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_restart_recovers_original_ids_and_receipt() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    let original = review.original.clone();
    let before = f.history().await.data.records[0].original_digest.clone();
    let result = f.service.confirm(&f.registry, review).await.unwrap();
    assert!(matches!(result.state, OwnerGrantState::Recorded));
    let bytes = f.history().await.data.records[0].receipt.clone().unwrap();
    let f = f.restart().await;
    f.service.manager.retire();
    let read_only = OwnerGrantIntents::new(f.owner.management(package(2, false)).await.unwrap());
    let found = read_only.list_existing(&f.registry).await.unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].intent_id, id);
    let summary = serde_json::to_value(&found[0]).unwrap();
    assert!(summary.get("state").is_none());
    assert!(summary.get("receipt").is_none());
    assert!(summary.get("subject").is_none());
    let result = read_only.recover(&f.registry, id.clone()).await.unwrap();
    assert!(matches!(result.state, OwnerGrantState::Recorded));
    let cookie = read_only.cookie().await.unwrap();
    let io = read_only
        .io(&cookie, Permission::ReadResources)
        .await
        .unwrap();
    let h = History::load(&io.path, io.identity.clone()).unwrap();
    assert_eq!(h.data.records[0].original, original);
    assert_eq!(h.data.records[0].original_digest, before);
    assert_eq!(h.data.records[0].receipt.as_deref(), Some(bytes.as_str()));
    assert!(
        read_only
            .io(&cookie, Permission::ConsentGrants)
            .await
            .is_err()
    );
    assert_eq!(f.count("owner_capacity_grants"), 1);
    drop(io);
    drop(read_only);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_authority_change_after_journal_save_is_unknown() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    drop(review);
    let path = f.history().await.path;
    let f = f.restart_with_recorded_fence(Some(path.clone())).await;
    let OwnerGrantReviewResult::Review(review) = f
        .service
        .review_existing(&f.registry, f.enrollment().await, id.clone())
        .await
        .unwrap()
    else {
        panic!("unattempted review required")
    };
    assert_eq!(
        f.service
            .confirm(&f.registry, *review)
            .await
            .err()
            .unwrap()
            .code,
        "management_outcome_unknown"
    );
    assert_eq!(f.count("owner_capacity_grants"), 1);
    let journal: Journal = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(journal.records[0].state, State::Recorded);
    let bytes = journal.records[0].receipt.clone().unwrap();
    let f = f.restart().await;
    assert!(matches!(
        f.service.recover(&f.registry, id).await.unwrap().state,
        OwnerGrantState::Recorded
    ));
    assert_eq!(
        f.history().await.data.records[0].receipt.as_deref(),
        Some(bytes.as_str())
    );
    assert_eq!(f.count("owner_capacity_grants"), 1);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_commit_loss_recovers_canonical_result_and_missing_stays_unknown()
{
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    let mut h = f.history().await;
    h.data.records[0].state = State::Applying;
    h.save().unwrap();
    // Simulate reply loss at the exact boundary after the isolated real writer
    // commits but before the native journal can persist its result.
    let receipt = review.consent.approve().await.unwrap();
    let result = f.service.recover(&f.registry, id.clone()).await.unwrap();
    assert!(matches!(result.state, OwnerGrantState::Recorded));
    let h = f.history().await;
    assert_eq!(
        h.data.records[0].receipt.as_ref().unwrap(),
        &serde_json::to_string(&receipt).unwrap()
    );
    let db = rusqlite::Connection::open(f.root.path().join("registry/domain.sqlite3")).unwrap();
    // Cached journal JSON must not turn missing canonical owner-ledger state
    // into authorization or a newly minted hold.
    db.execute("DELETE FROM owner_capacity_grants", []).unwrap();
    let result = f.service.recover(&f.registry, id.clone()).await.unwrap();
    assert!(matches!(result.state, OwnerGrantState::OutcomeUnknown));
    let result = f
        .service
        .review_existing(&f.registry, f.enrollment().await, id)
        .await
        .unwrap();
    assert!(matches!(
        result,
        OwnerGrantReviewResult::Progress(progress) if matches!(progress.state, OwnerGrantState::OutcomeUnknown)
    ));
    assert_eq!(f.count("owner_capacity_grants"), 0);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_unknown_without_commit_never_creates_replacement() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    let mut h = f.history().await;
    h.data.records[0].state = State::Applying;
    h.save().unwrap();
    drop(review);
    assert!(matches!(
        f.service
            .recover(&f.registry, id.clone())
            .await
            .unwrap()
            .state,
        OwnerGrantState::OutcomeUnknown
    ));
    assert!(matches!(
        f.service
            .review_existing(&f.registry, f.enrollment().await, id)
            .await
            .unwrap(),
        OwnerGrantReviewResult::Progress(progress) if matches!(progress.state, OwnerGrantState::OutcomeUnknown)
    ));
    assert_eq!(
        f.service
            .prepare(&f.registry, f.enrollment().await, Fixture::choices())
            .await
            .err()
            .unwrap()
            .code,
        "owner_grant_original_intent_required"
    );
    assert_eq!(f.count("owner_capacity_grants"), 0);
    assert_eq!(f.history().await.data.records.len(), 1);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_re_review_preserves_ids_and_cannot_double_confirm() {
    let f = Fixture::new().await;
    let first = f.review().await;
    let id = first.summary().intent_id;
    let OwnerGrantReviewResult::Review(second) = f
        .service
        .review_existing(&f.registry, f.enrollment().await, id)
        .await
        .unwrap()
    else {
        panic!("review required")
    };
    assert_eq!(first.original, second.original);
    assert!(matches!(
        f.service.confirm(&f.registry, first).await.unwrap().state,
        OwnerGrantState::Recorded
    ));
    assert!(matches!(
        f.service.confirm(&f.registry, *second).await.unwrap().state,
        OwnerGrantState::Recorded
    ));
    assert_eq!(f.count("owner_capacity_grants"), 1);
    assert_eq!(f.count("owner_capacity_receipts"), 1);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_missing_history_and_changed_identity_cannot_approve() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let h = f.history().await;
    let bytes = std::fs::read(&h.path).unwrap();
    std::fs::remove_file(&h.path).unwrap();
    assert_eq!(
        f.service
            .confirm(&f.registry, review)
            .await
            .err()
            .unwrap()
            .code,
        "owner_grant_history_unavailable"
    );
    assert_eq!(f.count("owner_capacity_grants"), 0);
    assert!(
        f.service
            .prepare(&f.registry, f.enrollment().await, Fixture::choices())
            .await
            .is_err()
    );
    std::fs::write(&h.path, &bytes).unwrap();
    let mut identity = h.data.identity.clone();
    identity.installation = "another-installation".into();
    assert!(History::load(&h.path, identity).is_err());
    let mut identity = h.data.identity.clone();
    identity.subject = "another-subject".into();
    assert!(History::load(&h.path, identity).is_err());
    assert_eq!(std::fs::read(&h.path).unwrap(), bytes);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_missing_all_private_history_cannot_replace_original_hold() {
    let f = Fixture::new().await;
    let review = f.review().await;
    f.service.confirm(&f.registry, review).await.unwrap();
    let h = f.history().await;
    std::fs::remove_file(&h.path).unwrap();
    std::fs::remove_file(History::marker(&h.path)).unwrap();
    let mut choices = Fixture::choices();
    choices.allocated_tokens = 100; // Plenty of headroom, so budget alone cannot save us.
    assert_eq!(
        f.service
            .prepare(&f.registry, f.enrollment().await, choices)
            .await
            .err()
            .unwrap()
            .code,
        "owner_grant_original_intent_required"
    );
    assert!(!h.path.exists());
    assert!(!History::marker(&h.path).exists());
    assert_eq!(f.count("owner_capacity_grants"), 1);
    assert_eq!(f.count("owner_capacity_receipts"), 1);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_writer_guard_rechecks_history_after_review() {
    let f = Fixture::new().await;
    let mut choices = Fixture::choices();
    choices.allocated_tokens = 100;
    let review = f
        .service
        .prepare(&f.registry, f.enrollment().await, choices.clone())
        .await
        .unwrap();
    let original_id = review.summary().intent_id;
    let external = f
        .registry
        .prepare_capacity_grant(
            f.enrollment().await,
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: None,
                command_id: "concurrent_native_command".into(),
                grant_id: "concurrent_native_grant".into(),
                coordinator_mxid: choices.coordinator_mxid,
                delegation_expires_at_ms: choices.delegation_expires_at_ms,
                allow_self_approval: choices.allow_self_approval,
                eligible_managers: choices.eligible_managers,
                allocated_tokens: choices.allocated_tokens,
            },
            Instant::now() + Duration::from_secs(20),
        )
        .unwrap();
    external.approve().await.unwrap();
    assert!(matches!(
        f.service.confirm(&f.registry, review).await.unwrap().state,
        OwnerGrantState::OutcomeUnknown
    ));
    assert!(matches!(
        f.service
            .recover(&f.registry, original_id)
            .await
            .unwrap()
            .state,
        OwnerGrantState::OutcomeUnknown
    ));
    assert_eq!(f.count("owner_capacity_grants"), 1);
    assert_eq!(f.count("owner_capacity_receipts"), 1);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_expired_period_and_changed_proof_never_rebind() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    drop(review);
    let cookie = f.service.cookie().await.unwrap();
    let io = f
        .service
        .io(&cookie, Permission::ConsentGrants)
        .await
        .unwrap();
    for (revision, coordinator, generation) in [
        ("f".repeat(64), "@owner:test".to_owned(), 1),
        (f.revision.clone(), "@replacement:test".to_owned(), 1),
        (f.revision.clone(), "@owner:test".to_owned(), 2),
    ] {
        let proof = VerifiedManagementEnrollment::new(
            io.profile.clone(),
            "fixture_enrollment".into(),
            "fixture_contribution".into(),
            f.resource.clone(),
            revision,
            coordinator,
            "device".into(),
            generation,
            io.deadline,
        )
        .unwrap();
        assert!(
            f.service
                .review_existing(&f.registry, proof, id.clone())
                .await
                .is_err()
        );
    }
    let mut h = f.history().await;
    let original_ids = (
        h.data.records[0].original.selection.command_id.clone(),
        h.data.records[0].original.selection.grant_id.clone(),
    );
    // Explicit historical journal fixture, not a production clock override.
    h.data.records[0].original.period = Period {
        key: "2000-01".into(),
        start_ms: 946_684_800_000,
        end_ms: 949_363_200_000,
    };
    h.data.records[0].original_digest = digest(&h.data.records[0].original).unwrap();
    h.save().unwrap();
    let result = f
        .service
        .review_existing(&f.registry, f.enrollment().await, id)
        .await
        .unwrap();
    assert!(matches!(
        result,
        OwnerGrantReviewResult::Progress(progress) if matches!(progress.state, OwnerGrantState::ReviewExpired)
    ));
    let h = f.history().await;
    assert_eq!(
        (
            h.data.records[0].original.selection.command_id.clone(),
            h.data.records[0].original.selection.grant_id.clone()
        ),
        original_ids
    );
    assert_eq!(h.data.records[0].original.period.key, "2000-01");
    assert_eq!(f.count("owner_capacity_grants"), 0);
    drop(io);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_old_handle_stays_retired_and_current_source_is_checked() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    f.service.manager.retire();
    let fresh = OwnerGrantIntents::new(f.owner.management(package(2, true)).await.unwrap());
    assert_eq!(
        fresh.confirm(&f.registry, review).await.err().unwrap().code,
        "owner_grant_review_changed"
    );
    f.source.store(false, Ordering::Release);
    assert!(fresh.recover(&f.registry, id).await.is_err());
    assert_eq!(f.count("owner_capacity_grants"), 0);
    drop(fresh);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_package_retirement_fences_actual_queued_writer() {
    let f = Fixture::new().await;
    let cookie = f.service.cookie().await.unwrap();
    let io = f
        .service
        .io(&cookie, Permission::ConsentGrants)
        .await
        .unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate_entered = entered.clone();
    let profile = ProfileAuthority::new(
        profile_digest(&io.identity).unwrap(),
        "@owner:test".into(),
        io.deadline,
    )
    .unwrap()
    .with_native_gate(move || {
        if std::thread::current().name() == Some("hagency-domain") {
            gate_entered.notify_one();
        }
        true
    })
    .unwrap();
    let proof = VerifiedManagementEnrollment::new(
        profile,
        "fixture_enrollment".into(),
        "fixture_contribution".into(),
        f.resource.clone(),
        f.revision.clone(),
        "@owner:test".into(),
        "device".into(),
        1,
        io.deadline,
    )
    .unwrap();
    let review = f
        .service
        .prepare(&f.registry, proof, Fixture::choices())
        .await
        .unwrap();
    let id = review.summary().intent_id;
    let mut db = rusqlite::Connection::open(f.root.path().join("registry/domain.sqlite3")).unwrap();
    let lock = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let mut confirmation = Box::pin(f.service.confirm(&f.registry, review));
    tokio::select! {
        result=&mut confirmation=>panic!("writer was not queued: {}",result.err().map(|e|e.code).unwrap_or_default()),
        entered=tokio::time::timeout(Duration::from_secs(5),entered.notified())=>{entered.unwrap();},
    }
    // The production consent reached the real writer before BEGIN IMMEDIATE.
    f.service.manager.retire();
    lock.commit().unwrap();
    assert_eq!(
        confirmation.await.err().unwrap().code,
        "management_outcome_unknown"
    );
    assert_eq!(f.count("owner_capacity_grants"), 0);
    let fresh = OwnerGrantIntents::new(f.owner.management(package(2, false)).await.unwrap());
    assert!(matches!(
        fresh.recover(&f.registry, id).await.unwrap().state,
        OwnerGrantState::OutcomeUnknown
    ));
    drop(io);
    drop(fresh);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_postcommit_retirement_recovers_immutable_receipt() {
    let f = Fixture::new().await;
    let cookie = f.service.cookie().await.unwrap();
    let io = f
        .service
        .io(&cookie, Permission::ConsentGrants)
        .await
        .unwrap();
    let worker_seen = Arc::new(AtomicBool::new(false));
    let seen = worker_seen.clone();
    let retiring = f.service.manager.clone();
    let profile = ProfileAuthority::new(
        profile_digest(&io.identity).unwrap(),
        "@owner:test".into(),
        io.deadline,
    )
    .unwrap()
    .with_native_gate(move || {
        if std::thread::current().name() == Some("hagency-domain") {
            seen.store(true, Ordering::Release);
        } else if seen.load(Ordering::Acquire) {
            retiring.retire();
            return false;
        }
        true
    })
    .unwrap();
    let proof = VerifiedManagementEnrollment::new(
        profile,
        "fixture_enrollment".into(),
        "fixture_contribution".into(),
        f.resource.clone(),
        f.revision.clone(),
        "@owner:test".into(),
        "device".into(),
        1,
        io.deadline,
    )
    .unwrap();
    let review = f
        .service
        .prepare(&f.registry, proof, Fixture::choices())
        .await
        .unwrap();
    let id = review.summary().intent_id;
    assert_eq!(
        f.service
            .confirm(&f.registry, review)
            .await
            .err()
            .unwrap()
            .code,
        "management_outcome_unknown"
    );
    assert!(worker_seen.load(Ordering::Acquire));
    assert_eq!(f.count("owner_capacity_grants"), 1);
    let fresh = OwnerGrantIntents::new(f.owner.management(package(2, false)).await.unwrap());
    assert!(matches!(
        fresh.recover(&f.registry, id.clone()).await.unwrap().state,
        OwnerGrantState::Recorded
    ));
    let cookie = fresh.cookie().await.unwrap();
    let current = fresh.io(&cookie, Permission::ReadResources).await.unwrap();
    let before = std::fs::read(&current.path).unwrap();
    assert!(matches!(
        fresh.recover(&f.registry, id).await.unwrap().state,
        OwnerGrantState::Recorded
    ));
    assert_eq!(std::fs::read(&current.path).unwrap(), before);
    assert_eq!(f.count("owner_capacity_grants"), 1);
    drop(current);
    drop(io);
    drop(fresh);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_composed_gate_keeps_original_proof_restriction() {
    let f = Fixture::new().await;
    let cookie = f.service.cookie().await.unwrap();
    let io = f
        .service
        .io(&cookie, Permission::ConsentGrants)
        .await
        .unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let gate_live = live.clone();
    let profile = ProfileAuthority::new(
        profile_digest(&io.identity).unwrap(),
        "@owner:test".into(),
        io.deadline,
    )
    .unwrap()
    .with_native_gate(move || gate_live.load(Ordering::Acquire))
    .unwrap();
    let proof = VerifiedManagementEnrollment::new(
        profile,
        "fixture_enrollment".into(),
        "fixture_contribution".into(),
        f.resource.clone(),
        f.revision.clone(),
        "@owner:test".into(),
        "device".into(),
        1,
        io.deadline,
    )
    .unwrap();
    let review = f
        .service
        .prepare(&f.registry, proof, Fixture::choices())
        .await
        .unwrap();
    live.store(false, Ordering::Release);
    assert!(!f.service.manager.is_retired());
    assert!(f.service.confirm(&f.registry, review).await.is_err());
    assert_eq!(f.history().await.data.records[0].state, State::Review);
    assert_eq!(f.count("owner_capacity_grants"), 0);
    drop(io);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_cached_receipt_is_not_canonical_authority() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    f.service.confirm(&f.registry, review).await.unwrap();
    let mut h = f.history().await;
    let mut receipt: NativeGrantReceipt =
        serde_json::from_str(h.data.records[0].receipt.as_ref().unwrap()).unwrap();
    receipt.ledger_revision += 1;
    h.data.records[0].receipt = Some(serde_json::to_string(&receipt).unwrap());
    h.save().unwrap();
    let before = std::fs::read(&h.path).unwrap();
    assert_eq!(
        f.service.recover(&f.registry, id).await.err().unwrap().code,
        "owner_grant_history_unavailable"
    );
    assert_eq!(std::fs::read(&h.path).unwrap(), before);
    assert_eq!(f.count("owner_capacity_grants"), 1);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_intent_second_process_probe() {
    let Some(root) = std::env::var_os("HAGENCY_GRANT_INTENT_TEST_ROOT") else {
        return;
    };
    let origin = std::env::var("HAGENCY_GRANT_INTENT_TEST_ORIGIN").unwrap();
    let result = NativeOwner::open_with_matrix(
        std::path::Path::new(&root),
        &origin,
        "@owner:test",
        Arc::new(Source(Arc::new(AtomicBool::new(true)))),
    )
    .await;
    assert_eq!(result.err().unwrap().code, "native_owner_state_unavailable");
}

#[tokio::test]
async fn native_owner_grant_intent_second_process_cannot_acquire_journal_custody() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let h = f.history().await;
    let before = std::fs::read(&h.path).unwrap();
    let executable = std::env::current_exe().unwrap();
    let root = f.root.path().join("host");
    let origin = f.origin.clone();
    let result=tokio::task::spawn_blocking(move||std::process::Command::new(executable)
        .arg("--exact").arg("console::native::owner_grant::tests::native_owner_grant_intent_second_process_probe")
        .arg("--nocapture")
        .env("HAGENCY_GRANT_INTENT_TEST_ROOT",root)
        .env("HAGENCY_GRANT_INTENT_TEST_ORIGIN",origin)
        .output().unwrap()).await.unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(std::fs::read(&h.path).unwrap(), before);
    assert_eq!(f.count("owner_capacity_grants"), 0);
    drop(review);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_review_snapshot_changed_budget_requires_same_intent_review() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let original = review.summary();
    let context = review.context();
    assert_eq!(context.server_origin, f.origin);
    assert_eq!(context.resource_revision, f.revision);
    assert_eq!(context.resource_total_tokens, Some(1000));
    assert_eq!(context.remaining_tokens, Some(1000));
    assert_eq!(context.model, "gpt-5.6-sol");
    assert!(!review.expired());
    let cookie = f.service.cookie().await.unwrap();
    let io = f
        .service
        .io(&cookie, Permission::ConsentGrants)
        .await
        .unwrap();
    let other = VerifiedManagementEnrollment::new(
        io.profile.clone(),
        "other_enrollment".into(),
        "other_contribution".into(),
        f.resource.clone(),
        f.revision.clone(),
        "@owner:test".into(),
        "device".into(),
        1,
        io.deadline,
    )
    .unwrap();
    f.registry
        .prepare_capacity_grant(
            other,
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: None,
                command_id: "competing_review_budget".into(),
                grant_id: "competing_review_budget".into(),
                coordinator_mxid: "@owner:test".into(),
                delegation_expires_at_ms: now_ms().unwrap() + 60000,
                allow_self_approval: false,
                eligible_managers: vec!["@manager:test".into()],
                allocated_tokens: 200,
            },
            io.deadline,
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert_eq!(
        f.service
            .confirm(&f.registry, review)
            .await
            .err()
            .unwrap()
            .code,
        "owner_grant_review_changed"
    );
    assert_eq!(f.count("owner_capacity_grants"), 1);
    assert!(matches!(
        f.service
            .recover(&f.registry, original.intent_id.clone())
            .await
            .unwrap()
            .state,
        OwnerGrantState::ReviewRequired
    ));
    let OwnerGrantReviewResult::Review(refreshed) = f
        .service
        .review_existing(
            &f.registry,
            f.enrollment().await,
            original.intent_id.clone(),
        )
        .await
        .unwrap()
    else {
        panic!("review required")
    };
    assert_eq!(refreshed.summary().intent_id, original.intent_id);
    assert_eq!(refreshed.summary().period_key, original.period_key);
    assert_eq!(refreshed.context().remaining_tokens, Some(800));
    assert!(matches!(
        f.service
            .confirm(&f.registry, *refreshed)
            .await
            .unwrap()
            .state,
        OwnerGrantState::Recorded
    ));
    assert_eq!(f.count("owner_capacity_grants"), 2);
    drop(io);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_cancelled_review_keeps_original_for_restart_and_retirement() {
    let f = Fixture::new().await;
    let review = f.review().await;
    let id = review.summary().intent_id;
    drop(review); // Native Back/Cancel deliberately does not call decline.
    assert_eq!(f.count("owner_capacity_grants"), 0);
    let fresh = OwnerGrantIntents::new(f.owner.management(package(2, true)).await.unwrap());
    let OwnerGrantReviewResult::Review(review) = fresh
        .review_existing(&f.registry, f.enrollment().await, id.clone())
        .await
        .unwrap()
    else {
        panic!("original review")
    };
    assert_eq!(review.summary().intent_id, id);
    fresh.manager.retire();
    assert!(review.expired());
    assert!(fresh.confirm(&f.registry, *review).await.is_err());
    assert_eq!(f.count("owner_capacity_grants"), 0);
    drop(fresh);
    f.stop().await;
}

#[tokio::test]
async fn native_owner_grant_host_lease_revocation_fences_real_writer_without_retiring_manager() {
    let f = Fixture::new().await;
    let alive = Arc::new(AtomicBool::new(true));
    let entered = Arc::new(tokio::sync::Notify::new());
    let journal = f.history().await.path.clone();
    let gate_alive = alive.clone();
    let gate_entered = entered.clone();
    let access = OwnerGrantIntents::new(f.service.manager.clone())
        .bind_registry(f.registry.clone())
        .await
        .unwrap()
        .restrict_to_host(move || {
            if std::thread::current().name() == Some("hagency-domain")
                && std::fs::read(&journal)
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Journal>(&b).ok())
                    .is_some_and(|j| j.records.iter().any(|r| r.state == State::Applying))
            {
                gate_entered.notify_one();
            }
            gate_alive.load(Ordering::Acquire)
        })
        .unwrap();
    let review = access
        .service()
        .prepare(&f.registry, f.enrollment().await, Fixture::choices())
        .await
        .unwrap();
    let id = review.summary().intent_id;
    let mut db = rusqlite::Connection::open(f.root.path().join("registry/domain.sqlite3")).unwrap();
    let lock = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let mut confirmation = Box::pin(access.confirm(review));
    tokio::select! {
        result=&mut confirmation=>panic!("writer was not queued: {}",result.err().map(|e|e.code).unwrap_or_default()),
        entered=tokio::time::timeout(Duration::from_secs(5),entered.notified())=>{entered.unwrap();},
    }
    alive.store(false, Ordering::Release);
    assert!(
        !f.service.manager.is_retired(),
        "only the separate host lease was revoked"
    );
    lock.commit().unwrap();
    assert_eq!(
        confirmation.await.err().unwrap().code,
        "management_outcome_unknown"
    );
    assert_eq!(f.count("owner_capacity_grants"), 0);
    assert!(matches!(
        f.service.recover(&f.registry, id).await.unwrap().state,
        OwnerGrantState::OutcomeUnknown
    ));
    drop(access);
    f.stop().await;
}

#[cfg(all(unix,feature="resource-adoption"))]
include!("namespace_execution_tests.rs");
