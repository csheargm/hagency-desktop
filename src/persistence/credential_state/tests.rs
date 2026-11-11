use super::*;
use super::retained::{Retained, RetainedIdentity};
use hagency_store::credential_vault::SecretStore;
use std::collections::BTreeMap;

/// Isolate global epoch/admission tests, bound their original child lifetime,
/// and keep every filesystem side effect under a disposable profile.
pub(crate) fn isolated_case(name: &str, marker: &str) -> bool {
    if std::env::var_os(marker).as_deref() == Some(std::ffi::OsStr::new("1")) {
        return false;
    }
    use std::io::{Read, Seek};
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("private-profile");
    hagency_store::private::create_directory_new(&profile).unwrap();
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env(marker, "1")
        .env("HAGENCY_NATIVE_CREDENTIAL_IO", "deny")
        .env("HAGENCY_DESKTOP_DATA_DIR", &profile)
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            timed_out = true;
            child.kill().unwrap();
            break child.wait().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    stdout.rewind().unwrap();
    stderr.rewind().unwrap();
    let mut out = String::new();
    let mut err = String::new();
    stdout.read_to_string(&mut out).unwrap();
    stderr.read_to_string(&mut err).unwrap();
    assert!(
        !timed_out && status.success(),
        "isolated {name} timeout={timed_out}, status={status}: {out}\n{err}"
    );
    assert!(
        out.contains("1 passed"),
        "isolated selector did not run one case: {out}\n{err}"
    );
    true
}

#[derive(Default)]
pub(crate) struct FakeStore {
    entries: Mutex<BTreeMap<Vec<u8>, Vec<u8>>>,
    failure: Mutex<Option<Error>>,
}
impl SecretStore for FakeStore {
    fn get(&self, reference: &SecretRef) -> std::result::Result<Zeroizing<Vec<u8>>, Error> {
        if let Some(error) = *self.failure.lock().unwrap() {
            return Err(error);
        }
        self.entries
            .lock()
            .unwrap()
            .get(&serde_json::to_vec(reference).unwrap())
            .cloned()
            .map(Zeroizing::new)
            .ok_or(Error::Missing)
    }
    fn put(&self, reference: &SecretRef, bytes: &[u8]) -> std::result::Result<(), Error> {
        if let Some(error) = *self.failure.lock().unwrap() {
            return Err(error);
        }
        self.entries
            .lock()
            .unwrap()
            .insert(serde_json::to_vec(reference).unwrap(), bytes.to_vec());
        Ok(())
    }
    fn remove(&self, reference: &SecretRef) -> std::result::Result<(), Error> {
        self.entries
            .lock()
            .unwrap()
            .remove(&serde_json::to_vec(reference).unwrap());
        Ok(())
    }
}
pub(crate) fn fake_service(root: &Path) -> Arc<Service> {
    Arc::new(Service::new(root.into(), Vault::new(Arc::new(FakeStore::default()))).unwrap())
}
pub(crate) fn session_fixture() -> FullSessionPersisted {
    serde_json::from_value(serde_json::json!({
        "client_session":{"homeserver":"https://example.org","db_path":"db_original","passphrase":"DB_SECRET_MARKER"},
        "user_session":{"user_id":"@alice:example.org","device_id":"ORIGINAL","access_token":"ACCESS_SECRET_MARKER","refresh_token":"REFRESH_SECRET_MARKER"},
        "sync_token":"original_sync", "sliding_sync_version":"None"
    })).unwrap()
}
fn session_path(root: &Path, session: &FullSessionPersisted) -> PathBuf {
    let account = crate::accounts::account_dir(root, &session.user_session.meta().user_id);
    hagency_store::private::directory(&account).unwrap();
    let state = account.join("persistent_state");
    hagency_store::private::directory(&state).unwrap();
    state.join("session")
}
#[test]
fn credential_migration_session_preserves_exact_database_key_and_tokens() {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_session_preserves_exact_database_key_and_tokens";
    if isolated_case(NAME, "HAGENCY_SESSION_KEY_CHILD") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeStore::default());
    let service = Service::new(root.path().into(), Vault::new(fake.clone())).unwrap();
    let session = session_fixture();
    let path = session_path(root.path(), &session);
    hagency_store::private::write_new(&path, &serde_json::to_vec(&session).unwrap()).unwrap();
    let loaded = service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serde_json::to_value(&session).unwrap()
    );
    let bytes = std::fs::read(&path).unwrap();
    let encoded = String::from_utf8(bytes).unwrap();
    for marker in [
        "DB_SECRET_MARKER",
        "ACCESS_SECRET_MARKER",
        "REFRESH_SECRET_MARKER",
    ] {
        assert!(!encoded.contains(marker));
    }
    let reopened = service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .unwrap();
    assert_eq!(reopened.client_session.passphrase, "DB_SECRET_MARKER");
    fake.entries.lock().unwrap().clear();
    assert!(
        service
            .load_worker(&path, crate::account_session::epoch(), || true)
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        encoded,
        "NoEntry cannot rekey or rewrite an existing v2 database"
    );
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}
#[test]
fn credential_migration_locked_store_keeps_original_plaintext_only_until_verified_commit() {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_locked_store_keeps_original_plaintext_only_until_verified_commit";
    if isolated_case(NAME, "HAGENCY_LOCKED_STORE_CHILD") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeStore::default());
    let service = Service::new(root.path().into(), Vault::new(fake.clone())).unwrap();
    let session = session_fixture();
    let path = session_path(root.path(), &session);
    let old = serde_json::to_vec(&session).unwrap();
    hagency_store::private::write_new(&path, &old).unwrap();
    *fake.failure.lock().unwrap() = Some(Error::Locked);
    assert!(
        service
            .load_worker(&path, crate::account_session::epoch(), || true)
            .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), old);
    *fake.failure.lock().unwrap() = None;
    service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .unwrap();
    assert!(
        !std::fs::read_to_string(path)
            .unwrap()
            .contains("SECRET_MARKER")
    );
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}
#[test]
fn credential_migration_logout_legacy_retained_without_session_is_database_only() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeStore::default());
    let service = Service::new(root.path().into(), Vault::new(fake.clone())).unwrap();
    let session = session_fixture();
    let session_path = session_path(root.path(), &session);
    let path = session_path.with_file_name("retained_db.json");
    hagency_store::private::write_new(
        &path,
        &serde_json::to_vec(&vec![session.client_session]).unwrap(),
    )
    .unwrap();
    let paths = service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    assert_eq!(paths, [PathBuf::from("db_original")]);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("DB_SECRET_MARKER"));
    let retained: Retained = serde_json::from_slice(&bytes).unwrap();
    let entry = &retained.databases[0];
    assert!(matches!(
        entry.identity,
        RetainedIdentity::LegacyDatabase { .. }
    ));
    let installation = service.installation().unwrap();
    let scope = entry.identity.scope(&installation).unwrap();
    assert_eq!(
        service
            .vault
            .load(&entry.database.reference, &scope)
            .unwrap()
            .as_slice(),
        b"DB_SECRET_MARKER"
    );
    let auth_scope = Scope::new(
        &installation,
        Purpose::Session,
        &serde_json::to_vec(&retained.account_hash).unwrap(),
    )
    .unwrap();
    assert!(
        service
            .vault
            .unlock(&entry.database.reference, &auth_scope)
            .is_err()
    );
    let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    unknown["databases"][0]["identity"]["kind"] = "future_unknown".into();
    hagency_store::private::replace(&path, &serde_json::to_vec(&unknown).unwrap()).unwrap();
    assert!(
        service
            .retain_worker(&path, None, None, crate::account_session::epoch())
            .is_err()
    );
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

#[test]
fn credential_migration_older_keep_cannot_roll_back_newer_canonical_pair() {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_older_keep_cannot_roll_back_newer_canonical_pair";
    if isolated_case(NAME, "HAGENCY_OLDER_KEEP_CHILD") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let service = fake_service(root.path());
    let original = session_fixture();
    let path = session_path(root.path(), &original);
    hagency_store::private::write_new(&path, &serde_json::to_vec(&original).unwrap()).unwrap();
    service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .unwrap();
    let keys = service
        .cached(&path, crate::account_session::epoch())
        .unwrap();
    recovery::write(&keys, &original, recovery::Intent::Keep).unwrap();
    let mut newer = original.clone();
    if let PersistedUserSession::Matrix(session) = &mut newer.user_session {
        session.tokens.access_token = "LATEST_ACCESS".into();
        session.tokens.refresh_token = Some("LATEST_REFRESH".into());
    }
    let doc = Document::open(&path, MAX_SESSION).unwrap();
    let source = doc.read().unwrap().unwrap();
    let bytes = keys.encode(newer.clone()).unwrap();
    let journal = doc
        .begin(
            keys.key.reference().scope(),
            Some(&source),
            vec![
                keys.key.reference().clone(),
                keys.database.reference.clone(),
                keys.revocation.reference().clone(),
            ],
        )
        .unwrap();
    let prepared = doc.prepare(journal, &bytes).unwrap();
    assert_eq!(
        doc.publish(prepared, Some(&source), || true).unwrap(),
        Published::Durable
    );
    let reopened = keys
        .decode(serde_json::from_slice(doc.read().unwrap().unwrap().bytes()).unwrap())
        .unwrap();
    let actual = recovery::absorb_keep(&doc, &keys, reopened, || true).unwrap();
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(newer).unwrap()
    );
    assert!(
        !recovery::path(&keys, recovery::Intent::Keep)
            .unwrap()
            .exists()
    );
    assert!(test_shutdown(&service));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_migration_snapshot_admission_precedes_activation_and_keeps_full_capacity_refresh()
 {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_snapshot_admission_precedes_activation_and_keeps_full_capacity_refresh";
    if isolated_case(NAME, "HAGENCY_OWNER_CAPACITY_CHILD") {
        return;
    }
    use wiremock::{
        MockServer, Mock, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/_matrix/client/v3/refresh"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"access_token":"rotated-access", "refresh_token":"rotated-refresh"}),
        ))
        .expect(1)
        .mount(&server)
        .await;
    let root = tempfile::tempdir().unwrap();
    let service = fake_service(root.path());
    let mut clients = Vec::new();
    let mut original_owners = Vec::new();
    for index in 0..128 {
        let client = matrix_sdk::Client::builder()
            .homeserver_url(server.uri())
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
            .build()
            .await
            .unwrap();
        let mut session = session_fixture();
        session.client_session.homeserver = server.uri();
        if let PersistedUserSession::Matrix(value) = &mut session.user_session {
            value.meta.user_id = format!("@fixture{index}:localhost").try_into().unwrap();
            value.meta.device_id = format!("DEVICE{index}").into();
        }
        assert!(client.session().is_none());
        let owner = install_owner(
            service.clone(),
            &client,
            session.client_session.clone(),
            None,
        )
        .unwrap();
        client
            .restore_session(session.user_session.into_sdk())
            .await
            .unwrap();
        owner.capture(&client).unwrap();
        original_owners.push(owner);
        clients.push(client);
    }
    assert_eq!(owners::pending().unwrap().len(), 128);
    let rejected = matrix_sdk::Client::builder()
        .homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
        .build()
        .await
        .unwrap();
    let error = install_owner(
        service.clone(),
        &rejected,
        session_fixture().client_session,
        None,
    )
    .err()
    .unwrap();
    assert_eq!(error.downcast_ref::<Error>(), Some(&Error::Capacity));
    assert!(
        rejected.session().is_none(),
        "129th owner is rejected before SDK activation"
    );
    clients[0]
        .matrix_auth()
        .refresh_access_token()
        .await
        .unwrap();
    assert!(
        original_owners[0].keys.get().is_none(),
        "native keys are deliberately not ready"
    );
    let fresh = original_owners[0].update().unwrap().unwrap();
    assert!(
        matches!(fresh.user, PersistedUserSession::Matrix(value) if value.tokens.access_token == "rotated-access" && value.tokens.refresh_token.as_deref() == Some("rotated-refresh"))
    );
    assert_eq!(
        owners::pending().unwrap().len(),
        128,
        "same owner replaces its admitted cell at capacity"
    );
    for owner in &original_owners {
        owner
            .clear(owner.update().unwrap().unwrap().revision)
            .unwrap();
    }
    drop(original_owners);
    drop(clients);
    assert!(owners::pending().unwrap().is_empty());
    let admitted = install_owner(
        service.clone(),
        &rejected,
        ClientSessionPersisted {
            homeserver: server.uri(),
            ..session_fixture().client_session
        },
        None,
    )
    .unwrap();
    drop(admitted);
    drop(rejected);
    assert!(test_shutdown(&service));
}

#[tokio::test]
async fn credential_migration_locked_preflight_rejects_before_registration_or_token_requests() {
    use wiremock::MockServer;
    let server = MockServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(FakeStore::default());
    *fake.failure.lock().unwrap() = Some(Error::Locked);
    let service = Arc::new(Service::new(root.path().into(), Vault::new(fake)).unwrap());
    let client = matrix_sdk::Client::builder()
        .homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
        .build()
        .await
        .unwrap();
    let owner = install_owner(
        service.clone(),
        &client,
        ClientSessionPersisted {
            homeserver: server.uri(),
            ..session_fixture().client_session
        },
        None,
    )
    .unwrap();
    owner.reserve_handoff().unwrap();
    let error = preflight(&owner).await.unwrap_err();
    assert_eq!(error.downcast_ref::<Error>(), Some(&Error::Locked));
    assert!(client.session().is_none());
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "no registration or token request precedes native availability"
    );
    assert!(test_shutdown(&service));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_migration_cancelled_native_preparation_keeps_original_io_and_retires_ready_grant()
 {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_cancelled_native_preparation_keeps_original_io_and_retires_ready_grant";
    if isolated_case(NAME, "HAGENCY_CANCEL_PREPARE_CHILD") {
        return;
    }
    struct HeldStore {
        inner: FakeStore,
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
        inflight: Arc<AtomicBool>,
    }
    impl SecretStore for HeldStore {
        fn get(&self, reference: &SecretRef) -> std::result::Result<Zeroizing<Vec<u8>>, Error> {
            self.inner.get(reference)
        }
        fn put(&self, reference: &SecretRef, bytes: &[u8]) -> std::result::Result<(), Error> {
            if reference.scope().purpose() == Purpose::Database {
                if let Some(entered) = self.entered.lock().unwrap().take() {
                    self.inflight.store(true, Ordering::Release);
                    entered.send(()).unwrap();
                    self.release.lock().unwrap().recv().unwrap();
                    self.inflight.store(false, Ordering::Release);
                }
            }
            self.inner.put(reference, bytes)
        }
        fn remove(&self, reference: &SecretRef) -> std::result::Result<(), Error> {
            self.inner.remove(reference)
        }
    }
    let server = wiremock::MockServer::start().await;
    let (entered, native_entered) = tokio::sync::oneshot::channel();
    let (release, native_release) = std::sync::mpsc::channel();
    let inflight = Arc::new(AtomicBool::new(false));
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(HeldStore {
        inner: FakeStore::default(),
        entered: Mutex::new(Some(entered)),
        release: Mutex::new(native_release),
        inflight: inflight.clone(),
    });
    let service = Arc::new(Service::new(root.path().into(), Vault::new(store)).unwrap());
    let client = matrix_sdk::Client::builder()
        .homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
        .build()
        .await
        .unwrap();
    let mut session = session_fixture();
    session.client_session.homeserver = server.uri();
    client
        .restore_session(session.user_session.clone().into_sdk())
        .await
        .unwrap();
    let path = session_path(root.path(), &session);
    let original_service = service.clone();
    let original_client = client.clone();
    let original_path = path.clone();
    let observer = tokio::spawn(async move {
        test_save(original_service, &original_client, session, original_path).await
    });
    native_entered.await.unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    assert!(
        inflight.load(Ordering::Acquire),
        "observer abort did not end the actual native operation"
    );
    assert!(!path.exists(), "an unobserved grant must not be published");
    let mut following_job = Box::pin(service.worker.call(|| Ok(())));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(5), &mut following_job)
            .await
            .is_err(),
        "original worker cannot advance while native IO is held"
    );
    release.send(()).unwrap();
    following_job.await.unwrap();
    assert!(!inflight.load(Ordering::Acquire));
    assert!(!path.exists());
    assert!(
        !path.with_file_name(".session.vault-journal").exists(),
        "confirmed unpublished intent was safely retired"
    );
    let owner = owners::find(&client).unwrap().unwrap();
    let keys = owner.keys.get().unwrap();
    assert!(
        recovery::revoke_pending(keys).unwrap(),
        "actual ready pair has a durable revoke-only tombstone"
    );
    let record = test_revoke_snapshot(
        service.clone(),
        recovery::path(keys, recovery::Intent::Revoke).unwrap(),
    )
    .await
    .unwrap();
    assert!(
        matches!(record, PersistedUserSession::Matrix(value) if value.tokens.access_token == "ACCESS_SECRET_MARKER" && value.tokens.refresh_token.as_deref() == Some("REFRESH_SECRET_MARKER"))
    );
    assert!(test_shutdown(&service));
}

#[test]
fn credential_migration_activation_reload_uses_fresh_keep_without_publishing_authority() {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_activation_reload_uses_fresh_keep_without_publishing_authority";
    if isolated_case(NAME, "HAGENCY_ACTIVATION_RELOAD_CHILD") {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let service = fake_service(root.path());
    let original = session_fixture();
    let path = session_path(root.path(), &original);
    hagency_store::private::write_new(&path, &serde_json::to_vec(&original).unwrap()).unwrap();
    service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .unwrap();
    let canonical = std::fs::read(&path).unwrap();
    let keys = service
        .cached(&path, crate::account_session::epoch())
        .unwrap();
    let mut actual = original;
    if let PersistedUserSession::Matrix(session) = &mut actual.user_session {
        session.tokens.access_token = "activated-access".into();
        session.tokens.refresh_token = Some("activated-refresh".into());
    }
    recovery::write(&keys, &actual, recovery::Intent::Keep).unwrap();
    let reloaded = read_callback(&keys, &path).unwrap();
    assert_eq!(
        serde_json::to_value(reloaded).unwrap(),
        serde_json::to_value(actual).unwrap()
    );
    assert_eq!(std::fs::read(&path).unwrap(), canonical);
    assert!(test_shutdown(&service));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_migration_owned_sdk_handoff_keeps_original_lease_until_real_io_ends() {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_owned_sdk_handoff_keeps_original_lease_until_real_io_ends";
    if isolated_case(NAME, "HAGENCY_HANDOFF_CHILD") {
        return;
    }
    use wiremock::{
        MockServer, Mock, ResponseTemplate,
        matchers::{method, path},
    };
    let server = MockServer::start().await;
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, wait_release) = std::sync::mpsc::channel();
    let entered = Mutex::new(Some(entered));
    let wait_release = Mutex::new(wait_release);
    Mock::given(method("GET"))
        .and(path("/_matrix/client/versions"))
        .respond_with(move |_: &wiremock::Request| {
            entered.lock().unwrap().take().unwrap().send(()).unwrap();
            wait_release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"versions":["v1.15"],"unstable_features":{}}))
        })
        .expect(1)
        .mount(&server)
        .await;
    let service = fake_service(crate::app_data_dir());
    let mut session = session_fixture();
    session.client_session.homeserver = server.uri();
    session.client_session.db_path = "db_handoff".into();
    let client = matrix_sdk::Client::builder()
        .homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
        .build()
        .await
        .unwrap();
    let owner = install_owner(
        service.clone(),
        &client,
        session.client_session.clone(),
        None,
    )
    .unwrap();
    owner.reserve_handoff().unwrap();
    client
        .restore_session(session.user_session.clone().into_sdk())
        .await
        .unwrap();
    let path = session_path(crate::app_data_dir(), &session);
    test_save(service.clone(), &client, session, path.clone())
        .await
        .unwrap();
    let canonical = std::fs::read(&path).unwrap();
    let keys = owner.keys.get().unwrap();
    let grant = keys.key.reference().clone();
    let recovery_root = keys.recovery_root.clone();
    drop(owner);
    let original_weak = client.downgrade();
    let original = client.clone();
    fn discover(builder: matrix_sdk::ClientBuilder) -> matrix_sdk::ClientBuilder {
        builder
            .sliding_sync_version_builder(matrix_sdk::sliding_sync::VersionBuilder::DiscoverNative)
    }
    let observer =
        tokio::spawn(async move { handoff_client_with(&original, Some(discover)).await });
    waiting.await.unwrap();
    observer.abort();
    assert!(observer.await.unwrap_err().is_cancelled());
    drop(client);
    crate::account_session::close().await;
    service.cache.lock().unwrap().clear();
    assert!(
        original_weak.get().is_some(),
        "the actual held SDK builder retains its original candidate"
    );
    assert!(
        matches!(
            vault::document::GrantLease::acquire(&recovery_root, &grant),
            Err(Error::Conflict)
        ),
        "original IO still owns the grant lease"
    );
    assert!(
        !service.worker.shutdown(std::time::Duration::from_millis(5)),
        "unknown original SDK IO is not reported closed"
    );
    release.send(()).unwrap();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !service.worker.shutdown(std::time::Duration::ZERO) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "original SDK IO did not really finish"
        );
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    assert!(original_weak.get().is_none());
    assert!(
        vault::document::GrantLease::acquire(&recovery_root, &grant).is_ok(),
        "lease released only after actual original work ended"
    );
    assert_eq!(
        std::fs::read(&path).unwrap(),
        canonical,
        "old epoch cannot publish new canonical authority"
    );
    assert!(service.cache.lock().unwrap().is_empty());
    assert!(crate::sliding_sync::get_client().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn credential_migration_same_grant_restore_waits_for_real_shared_auth_context() {
    const NAME: &str = "persistence::credential_state::tests::credential_migration_same_grant_restore_waits_for_real_shared_auth_context";
    if isolated_case(NAME, "HAGENCY_SHARED_AUTH_CHILD") {
        return;
    }
    let server = wiremock::MockServer::start().await;
    let root = tempfile::tempdir().unwrap();
    let service = fake_service(root.path());
    let mut session = session_fixture();
    session.client_session.homeserver = server.uri();
    let client = matrix_sdk::Client::builder()
        .homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
        .build()
        .await
        .unwrap();
    client
        .restore_session(session.user_session.clone().into_sdk())
        .await
        .unwrap();
    let path = session_path(root.path(), &session);
    test_save(service.clone(), &client, session, path.clone())
        .await
        .unwrap();
    let owner = owners::find(&client).unwrap().unwrap();
    let original = client.downgrade();
    let notification = client
        .notification_client(matrix_sdk::cross_process_lock::CrossProcessLockConfig::SingleProcess)
        .await
        .unwrap();
    assert!(
        !std::ptr::eq(
            notification.http_client(),
            original.get().unwrap().http_client()
        ),
        "actual sibling has a distinct ClientInner"
    );
    drop(client);
    assert!(
        original.get().is_none(),
        "original weak identity can end before shared AuthCtx"
    );
    assert!(owner.callbacks_alive());
    let restored = matrix_sdk::Client::builder()
        .homeserver_url(server.uri())
        .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
        .build()
        .await
        .unwrap();
    let error = claim_restoration_on(service.clone(), &restored, path.clone())
        .await
        .unwrap_err();
    assert_eq!(error.downcast_ref::<Error>(), Some(&Error::OriginalBusy));
    assert!(
        restored.session().is_none(),
        "live shared grant is rejected before activation or RT use"
    );
    drop(notification);
    assert!(
        !owner.callbacks_alive(),
        "actual last SDK/AuthCtx drop ends callback custody"
    );
    claim_restoration_on(service.clone(), &restored, path)
        .await
        .unwrap();
    assert!(owners::find(&restored).unwrap().is_some());
    assert!(test_shutdown(&service));
}

#[test]
fn credential_profile_first_login_provisions_missing_parents_privately() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary
        .path()
        .join("new-organization/new-desktop-profile");
    let service = Service::new(root.clone(), Vault::new(Arc::new(FakeStore::default()))).unwrap();
    hagency_store::private::directory(&root).unwrap();
    hagency_store::private::directory(root.parent().unwrap()).unwrap();
    assert!(
        !root.join("credentials-v1").exists(),
        "constructing a service performs no native credential IO"
    );
    assert!(test_shutdown(&service));
}
