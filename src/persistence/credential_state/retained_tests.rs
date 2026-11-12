use super::*;
use hagency_store::credential_vault::SecretStore;
use matrix_sdk_crypto::{OlmMachineBuilder, store::CryptoStore};
use matrix_sdk_base::store::StateStore;
use std::{collections::BTreeMap, sync::Mutex as StdMutex};

#[derive(Default)]
struct Fake {
    values: StdMutex<BTreeMap<Vec<u8>, Vec<u8>>>,
    puts: StdMutex<usize>,
    gets: StdMutex<usize>,
}
impl SecretStore for Fake {
    fn get(&self, r: &SecretRef) -> std::result::Result<Zeroizing<Vec<u8>>, Error> {
        *self.gets.lock().unwrap() += 1;
        self.values
            .lock()
            .unwrap()
            .get(&serde_json::to_vec(r).unwrap())
            .cloned()
            .map(Zeroizing::new)
            .ok_or(Error::Missing)
    }
    fn put(&self, r: &SecretRef, b: &[u8]) -> std::result::Result<(), Error> {
        *self.puts.lock().unwrap() += 1;
        self.values
            .lock()
            .unwrap()
            .insert(serde_json::to_vec(r).unwrap(), b.to_vec());
        Ok(())
    }
    fn remove(&self, r: &SecretRef) -> std::result::Result<(), Error> {
        self.values
            .lock()
            .unwrap()
            .remove(&serde_json::to_vec(r).unwrap());
        Ok(())
    }
}
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
fn fixture(root: &Path) -> (Arc<Service>, Arc<Fake>, FullSessionPersisted, PathBuf) {
    hagency_store::private::directory(root).unwrap();
    let fake = Arc::new(Fake::default());
    let service = Arc::new(Service::new(root.into(), Vault::new(fake.clone())).unwrap());
    let mut session = super::super::tests::session_fixture();
    session.client_session.passphrase = "ORIGINAL_DATABASE_KEY".repeat(2);
    session.client_session.db_path = "db_original".into();
    let account = crate::accounts::account_dir(root, &session.user_session.meta().user_id);
    hagency_store::private::directory(&account).unwrap();
    let state = account.join("persistent_state");
    hagency_store::private::directory(&state).unwrap();
    (service, fake, session, state.join("retained_db.json"))
}
fn seed_legacy(path: &Path, session: &FullSessionPersisted) {
    hagency_store::private::write_new(
        path,
        &serde_json::to_vec(&vec![session.client_session.clone()]).unwrap(),
    )
    .unwrap();
}
#[tokio::test]
async fn credential_migration_retained_original_ref_sqlite_and_state_survive_handoff() {
    const NAME: &str = "persistence::credential_state::retained::retained_tests::credential_migration_retained_original_ref_sqlite_and_state_survive_handoff";
    // This migration exercises the application's global pending-owner flush.
    // Give it one complete App lifetime rather than sharing intentionally dirty
    // OAuth fault fixtures from other independent App lifetimes in this harness.
    if std::env::var_os("HAGENCY_RETAINED_CLEANUP_CHILD").is_none() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("private-profile");
        hagency_store::private::create_directory_new(&root).unwrap();
        run_cleanup_child(NAME, &root, false);
        return;
    }
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("data");
    let (service, fake, session, path) = fixture(&root);
    let identity = identity(&session);
    let db = root.join(&identity.db_path);
    let crypto =
        matrix_sdk_sqlite::SqliteCryptoStore::open(&db, Some(&session.client_session.passphrase))
            .await
            .unwrap();
    let machine = OlmMachineBuilder::new(&identity.owner, identity.device.as_str().into())
        .with_crypto_store(crypto.clone())
        .build()
        .await
        .unwrap();
    let before = crypto
        .load_account()
        .await
        .unwrap()
        .unwrap()
        .identity_keys();
    let state =
        matrix_sdk_sqlite::SqliteStateStore::open(&db, Some(&session.client_session.passphrase))
            .await
            .unwrap();
    state
        .set_custom_value(b"retained-state", b"original-state-data".to_vec())
        .await
        .unwrap();
    drop(state);
    drop(machine);
    crypto.close().await.unwrap();
    drop(crypto);
    seed_legacy(&path, &session);
    service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    let retained: Retained = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let original_ref = retained.databases[0].database.reference.clone();
    let puts = *fake.puts.lock().unwrap();
    let selected = service.retained_database(&identity).unwrap().unwrap();
    assert_eq!(selected.reference, original_ref);
    assert!(selected.legacy_account.is_some());
    let secret = service
        .resolve_database(
            &selected,
            &identity,
            &service.existing_installation().unwrap(),
        )
        .unwrap();
    assert_eq!(secret.as_str(), session.client_session.passphrase);
    let original_crypto_bytes = std::fs::read(db.join("matrix-sdk-crypto.sqlite3")).unwrap();
    verify_retained_identity(
        service.clone(),
        selected.clone(),
        identity.clone(),
        secret.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(db.join("matrix-sdk-crypto.sqlite3")).unwrap(),
        original_crypto_bytes,
        "identity discovery must not initialize migrate or checkpoint original crypto data"
    );
    assert_eq!(
        *fake.puts.lock().unwrap(),
        puts,
        "handoff must not mint a replacement DB reference"
    );
    let reopened = matrix_sdk_sqlite::SqliteCryptoStore::open(&db, Some(&secret))
        .await
        .unwrap();
    assert_eq!(
        reopened
            .load_account()
            .await
            .unwrap()
            .unwrap()
            .identity_keys(),
        before
    );
    let state = matrix_sdk_sqlite::SqliteStateStore::open(&db, Some(&secret))
        .await
        .unwrap();
    assert_eq!(
        state
            .get_custom_value(b"retained-state")
            .await
            .unwrap()
            .unwrap(),
        b"original-state-data"
    );
    let mut forged = selected.clone();
    forged.legacy_account = None;
    assert!(
        service
            .resolve_database(
                &forged,
                &identity,
                &service.existing_installation().unwrap()
            )
            .is_err()
    );
    forged.legacy_account = Some(format!("account_{}", "0".repeat(64)));
    assert!(
        service
            .resolve_database(
                &forged,
                &identity,
                &service.existing_installation().unwrap()
            )
            .is_err()
    );
    let session_path = path.with_file_name("session");
    hagency_store::private::write_new(&session_path, &serde_json::to_vec(&session).unwrap())
        .unwrap();
    service
        .load_worker(&session_path, crate::account_session::epoch(), || true)
        .unwrap();
    let published: Stored = serde_json::from_slice(&std::fs::read(&session_path).unwrap()).unwrap();
    assert_eq!(published.database.reference, original_ref);
    assert!(published.database.legacy_account.is_some());
    let restored = service.read_original(&published).unwrap();
    assert_eq!(restored.secret.as_str(), secret.as_str());
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

#[tokio::test]
async fn credential_migration_retained_actual_sqlite_device_mismatch_and_missing_original_are_rejected()
 {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("data");
    let (service, fake, session, path) = fixture(&root);
    let identity = identity(&session);
    let db = root.join(&identity.db_path);
    let crypto =
        matrix_sdk_sqlite::SqliteCryptoStore::open(&db, Some(&session.client_session.passphrase))
            .await
            .unwrap();
    let machine = OlmMachineBuilder::new(&identity.owner, identity.device.as_str().into())
        .with_crypto_store(crypto.clone())
        .build()
        .await
        .unwrap();
    drop(machine);
    crypto.close().await.unwrap();
    drop(crypto);
    seed_legacy(&path, &session);
    service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    let selected = service.retained_database(&identity).unwrap().unwrap();
    let secret = service
        .resolve_database(
            &selected,
            &identity,
            &service.existing_installation().unwrap(),
        )
        .unwrap();
    let puts = *fake.puts.lock().unwrap();
    let mut other = identity.clone();
    other.device = "NEW_DEVICE".into();
    let error = verify_retained_identity(
        service.clone(),
        selected.clone(),
        other.clone(),
        secret.clone(),
    )
    .await
    .unwrap_err();
    assert!(error.downcast_ref::<RetainedDeviceChanged>().is_some());
    other = identity.clone();
    other.owner = matrix_sdk::ruma::owned_user_id!("@other:example.org");
    assert!(
        verify_retained_identity(
            service.clone(),
            selected.clone(),
            other.clone(),
            secret.clone()
        )
        .await
        .is_err()
    );
    std::fs::rename(
        db.join("matrix-sdk-crypto.sqlite3"),
        db.join("original-crypto.saved"),
    )
    .unwrap();
    assert!(
        verify_retained_identity(
            service.clone(),
            selected.clone(),
            identity.clone(),
            secret.clone()
        )
        .await
        .is_err()
    );
    assert!(!db.join("matrix-sdk-crypto.sqlite3").exists());
    assert!(service.validate_unreferenced_database(&identity).is_err());
    assert_eq!(*fake.puts.lock().unwrap(), puts);
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

async fn inventory(
    service: Arc<Service>,
    root: &Path,
    entries: Vec<anyhow::Result<PathBuf>>,
) -> anyhow::Result<std::collections::HashSet<PathBuf>> {
    let epoch = crate::account_session::epoch();
    crate::persistence::matrix_state::inventory_entries(
        root,
        futures_util::stream::iter(entries),
        move |path, retained| {
            let service = service.clone();
            async move {
                let original = service.clone();
                service
                    .worker
                    .call(move || {
                        if retained {
                            original
                                .retain_worker(&path, None, None, epoch)
                                .map_err(classify)
                        } else {
                            original
                                .inspect_database(&path)
                                .map(|s| vec![s.client_session.db_path])
                                .map_err(classify)
                        }
                    })
                    .await
                    .map_err(Into::into)
            }
        },
    )
    .await
}
#[tokio::test]
async fn credential_migration_retained_incomplete_inventory_refuses_actual_gc() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("data");
    let (service, fake, session, path) = fixture(&root);
    seed_legacy(&path, &session);
    service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    let valid = std::fs::read(&path).unwrap();
    let keys = fake.values.lock().unwrap().clone();
    let account = path.parent().unwrap().parent().unwrap().to_owned();
    let cache = temporary.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let orphan = root.join("db_orphan");
    let cached = cache.join("db_orphan");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::create_dir_all(&cached).unwrap();
    std::fs::write(orphan.join("keep"), b"data").unwrap();
    std::fs::write(cached.join("keep"), b"cache").unwrap();
    for fault in 0..3 {
        hagency_store::private::replace(&path, &valid).unwrap();
        *fake.values.lock().unwrap() = keys.clone();
        if fault == 0 {
            let mut unknown: serde_json::Value = serde_json::from_slice(&valid).unwrap();
            unknown["databases"][0]["identity"]["kind"] = "Unknown".into();
            hagency_store::private::replace(&path, &serde_json::to_vec(&unknown).unwrap()).unwrap();
        }
        if fault == 1 {
            fake.values.lock().unwrap().clear();
        }
        let mut entries = vec![Ok(account.clone())];
        if fault == 2 {
            entries.push(Err(anyhow!(
                "injected traversal IO error after a valid account"
            )));
        }
        let result = inventory(service.clone(), &root, entries).await;
        assert!(result.is_err());
        crate::persistence::matrix_state::cleanup_with_inventory(&root, &cache, async {
            result.ok()
        })
        .await;
        assert!(orphan.join("keep").exists());
        assert!(cached.join("keep").exists());
    }
    hagency_store::private::replace(&path, &valid).unwrap();
    *fake.values.lock().unwrap() = keys;
    let complete = inventory(service.clone(), &root, vec![Ok(account)])
        .await
        .unwrap();
    crate::persistence::matrix_state::cleanup_with_inventory(&root, &cache, async {
        Some(complete)
    })
    .await;
    assert!(!orphan.exists());
    assert!(!cached.exists());
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

#[tokio::test]
async fn credential_migration_retained_facade_matches_actual_device_without_reissuing_reference() {
    let _serial = SERIAL.lock().await;
    // Real SDK memory sessions. Any SDK initialization housekeeping stays on an
    // isolated loopback mock, never an external homeserver or real account.
    let server = wiremock::MockServer::start().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("data");
    let (service, fake, mut session, path) = fixture(&root);
    session.client_session.homeserver = format!("{}/", server.uri());
    let expected = identity(&session);
    let store = matrix_sdk_sqlite::SqliteCryptoStore::open(
        root.join(&expected.db_path),
        Some(&session.client_session.passphrase),
    )
    .await
    .unwrap();
    let machine = OlmMachineBuilder::new(&expected.owner, expected.device.as_str().into())
        .with_crypto_store(store.clone())
        .build()
        .await
        .unwrap();
    drop(machine);
    store.close().await.unwrap();
    drop(store);
    seed_legacy(&path, &session);
    service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    let original = std::fs::read(&path).unwrap();
    let reference = serde_json::from_slice::<Retained>(&original)
        .unwrap()
        .databases[0]
        .database
        .reference
        .clone();
    let puts = *fake.puts.lock().unwrap();
    for (device, matches) in [(expected.device.as_str(), true), ("NEW_DEVICE", false)] {
        let client = sdk_client(&server.uri(), expected.owner.clone(), device).await;
        let selected = retained_session_on(service.clone(), &client).await.unwrap();
        if matches {
            let selected = selected
                .expect("the exact original SDK device must recover its data-only material");
            assert_eq!(selected.db_path, session.client_session.db_path);
            assert_eq!(selected.homeserver, session.client_session.homeserver);
            assert_eq!(selected.passphrase, session.client_session.passphrase);
        } else {
            assert!(
                selected.is_none(),
                "a new device cannot adopt the old crypto account"
            );
        }
        assert_eq!(*fake.puts.lock().unwrap(), puts);
        assert_eq!(
            serde_json::from_slice::<Retained>(&std::fs::read(&path).unwrap())
                .unwrap()
                .databases[0]
                .database
                .reference,
            reference
        );
    }
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

#[tokio::test]
async fn credential_migration_retained_cancelled_observer_keeps_original_reader_owned() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("data");
    let (service, fake, session, path) = fixture(&root);
    let identity = identity(&session);
    let db = root.join(&identity.db_path);
    let crypto =
        matrix_sdk_sqlite::SqliteCryptoStore::open(&db, Some(&session.client_session.passphrase))
            .await
            .unwrap();
    let machine = OlmMachineBuilder::new(&identity.owner, identity.device.as_str().into())
        .with_crypto_store(crypto.clone())
        .build()
        .await
        .unwrap();
    drop(machine);
    crypto.close().await.unwrap();
    seed_legacy(&path, &session);
    service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    let database = service.retained_database(&identity).unwrap().unwrap();
    let secret = service
        .resolve_database(
            &database,
            &identity,
            &service.existing_installation().unwrap(),
        )
        .unwrap();
    let before = std::fs::read(db.join("matrix-sdk-crypto.sqlite3")).unwrap();
    let puts = *fake.puts.lock().unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
    let (finished_tx, finished_rx) = std::sync::mpsc::sync_channel(1);
    let original = service.clone();
    let observed = tokio::spawn(async move {
        let original_job = original.clone();
        original
            .worker
            .call(move || {
                let result =
                    verify_original(&original_job.root, &database, &identity, &secret, || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    });
                // This receipt is independent of the intentionally cancelled
                // observer and is sent only after the real SDK read returned.
                finished_tx.send(result.is_ok()).unwrap();
                Ok(())
            })
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), started_rx)
        .await
        .unwrap()
        .unwrap();
    observed.abort();
    assert!(observed.await.unwrap_err().is_cancelled());
    assert!(finished_rx.try_recv().is_err());
    assert!(!service.worker.shutdown(std::time::Duration::ZERO));
    release_tx.send(()).unwrap();
    assert!(
        finished_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap()
    );
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
    assert_eq!(
        std::fs::read(db.join("matrix-sdk-crypto.sqlite3")).unwrap(),
        before
    );
    assert_eq!(*fake.puts.lock().unwrap(), puts);
}

#[tokio::test]
async fn credential_migration_retained_old_absolute_path_preserves_relocated_database() {
    let _serial = SERIAL.lock().await;
    let server = wiremock::MockServer::start().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("new-sandbox");
    let (service, fake, mut session, path) = fixture(&root);
    session.client_session.homeserver = format!("{}/", server.uri());
    let old_absolute = temporary.path().join("retired-sandbox/db_original");
    session.client_session.db_path = old_absolute.clone();
    let expected = identity(&session);
    let actual = root.join("db_original");
    let crypto = matrix_sdk_sqlite::SqliteCryptoStore::open(
        &actual,
        Some(&session.client_session.passphrase),
    )
    .await
    .unwrap();
    let machine = OlmMachineBuilder::new(&expected.owner, expected.device.as_str().into())
        .with_crypto_store(crypto.clone())
        .build()
        .await
        .unwrap();
    drop(machine);
    crypto.close().await.unwrap();
    let before = std::fs::read(actual.join("matrix-sdk-crypto.sqlite3")).unwrap();
    seed_legacy(&path, &session);
    service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    let descriptor = serde_json::from_slice::<Retained>(&std::fs::read(&path).unwrap())
        .unwrap()
        .databases
        .remove(0)
        .database;
    assert_eq!(descriptor.db_path, old_absolute);
    assert!(descriptor.legacy_account.is_some());
    assert_eq!(resolve_database_path(&root, &old_absolute).unwrap(), actual);
    let puts = *fake.puts.lock().unwrap();
    let client = sdk_client(&server.uri(), expected.owner.clone(), &expected.device).await;
    let selected = retained_session_on(service.clone(), &client)
        .await
        .unwrap()
        .expect("same actual SDK identity must preserve relocated original material");
    assert_eq!(selected.db_path, old_absolute);
    assert_eq!(selected.passphrase, session.client_session.passphrase);
    assert_eq!(selected.homeserver, session.client_session.homeserver);
    assert!(
        !old_absolute.exists(),
        "discovery cannot create the obsolete path"
    );
    assert_eq!(
        std::fs::read(actual.join("matrix-sdk-crypto.sqlite3")).unwrap(),
        before
    );
    assert_eq!(*fake.puts.lock().unwrap(), puts);
    let after: Retained = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(after.databases[0].database, descriptor);
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

// Use a genuinely new private child as the actor's real application root.
// tempfile 3.x roots inherit default process permissions; do not chmod or
// weaken the production private-directory checker to accommodate that fixture.
fn run_cleanup_child(name: &str, root: &Path, writer: bool) {
    use std::io::{Read, Seek};
    let mut stdout = tempfile::tempfile().unwrap();
    let mut stderr = tempfile::tempfile().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture", "--test-threads=1"])
        .env("HAGENCY_RETAINED_CLEANUP_CHILD", "1")
        .env("HAGENCY_NATIVE_CREDENTIAL_IO", "deny")
        .env("HAGENCY_DESKTOP_DATA_DIR", root)
        .stdout(stdout.try_clone().unwrap())
        .stderr(stderr.try_clone().unwrap());
    if writer {
        command.env("HAGENCY_RETAINED_CLEANUP_WRITER", "1");
    } else {
        command.env_remove("HAGENCY_RETAINED_CLEANUP_WRITER");
    }
    let mut child = command.spawn().unwrap();
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
        !timed_out && status.success() && out.contains("1 passed"),
        "cleanup child writer={writer}, timeout={timed_out}, status={status}: {out}\n{err}"
    );
}

#[tokio::test]
async fn credential_migration_retained_startup_cleanup_preserves_cross_process_creation() {
    const NAME: &str = "persistence::credential_state::retained::retained_tests::credential_migration_retained_startup_cleanup_preserves_cross_process_creation";
    const WRITER: &str = "HAGENCY_RETAINED_CLEANUP_WRITER";
    assert_eq!(
        std::env::var("HAGENCY_NATIVE_CREDENTIAL_IO").unwrap(),
        "deny"
    );
    if std::env::var_os(WRITER).is_some() {
        // A real second process creates a current SQLite database only after
        // the other process has completed its earlier diagnostic inventory.
        let db = crate::app_data_dir().join("db_other_process");
        let crypto = matrix_sdk_sqlite::SqliteCryptoStore::open(
            &db,
            Some("isolated-other-process-database-key"),
        )
        .await
        .unwrap();
        let machine = OlmMachineBuilder::new(
            matrix_sdk::ruma::user_id!("@other:example.org"),
            "OTHER_DEVICE".into(),
        )
        .with_crypto_store(crypto.clone())
        .build()
        .await
        .unwrap();
        drop(machine);
        crypto.close().await.unwrap();
        std::fs::write(db.join("sentinel"), b"new process original data").unwrap();
        return;
    }
    if std::env::var_os("HAGENCY_RETAINED_CLEANUP_CHILD").is_none() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("private-profile");
        hagency_store::private::create_directory_new(&root).unwrap();
        run_cleanup_child(NAME, &root, false);
        return;
    }
    assert_eq!(
        std::env::var("HAGENCY_NATIVE_CREDENTIAL_IO").unwrap(),
        "deny"
    );
    let root = crate::app_data_dir();
    let (service, fake, session, path) = fixture(root);
    assert!(SERVICE.set(Ok(service.clone())).is_ok());
    let db = root.join(&session.client_session.db_path);
    let crypto =
        matrix_sdk_sqlite::SqliteCryptoStore::open(&db, Some(&session.client_session.passphrase))
            .await
            .unwrap();
    let expected = identity(&session);
    let machine = OlmMachineBuilder::new(&expected.owner, expected.device.as_str().into())
        .with_crypto_store(crypto.clone())
        .build()
        .await
        .unwrap();
    drop(machine);
    crypto.close().await.unwrap();
    std::fs::write(db.join("sentinel"), b"existing process original data").unwrap();
    seed_legacy(&path, &session);
    service
        .retain_worker(&path, None, None, crate::account_session::epoch())
        .unwrap();
    let snapshot = crate::persistence::matrix_state::collect_referenced_db_paths()
        .await
        .expect("the earlier native-key inventory must be complete, not an error fallback");
    assert!(snapshot.contains(&db));
    let new_db = root.join("db_other_process");
    assert!(!snapshot.contains(&new_db));
    assert!(!new_db.exists());

    run_cleanup_child(NAME, root, true);
    assert!(new_db.join("matrix-sdk-crypto.sqlite3").is_file());
    let original_bytes = std::fs::read(db.join("matrix-sdk-crypto.sqlite3")).unwrap();
    let new_bytes = std::fs::read(new_db.join("matrix-sdk-crypto.sqlite3")).unwrap();
    let cache_original = crate::cache_dir().join("db_original");
    let cache_new = crate::cache_dir().join("db_other_process");
    for cache in [&cache_original, &cache_new] {
        std::fs::create_dir_all(cache).unwrap();
        std::fs::write(cache.join("sentinel"), b"original cached data").unwrap();
    }
    let gets = *fake.gets.lock().unwrap();
    let puts = *fake.puts.lock().unwrap();
    // Invoke the exact production startup entry with its actual isolated root,
    // not the test-only pruning helper or an injected inventory callback.
    crate::persistence::matrix_state::cleanup_orphan_db_dirs().await;
    assert_eq!(
        std::fs::read(db.join("matrix-sdk-crypto.sqlite3")).unwrap(),
        original_bytes
    );
    assert_eq!(
        std::fs::read(new_db.join("matrix-sdk-crypto.sqlite3")).unwrap(),
        new_bytes
    );
    assert_eq!(
        std::fs::read(db.join("sentinel")).unwrap(),
        b"existing process original data"
    );
    assert_eq!(
        std::fs::read(new_db.join("sentinel")).unwrap(),
        b"new process original data"
    );
    assert!(cache_original.join("sentinel").exists());
    assert!(cache_new.join("sentinel").exists());
    assert_eq!(
        *fake.gets.lock().unwrap(),
        gets,
        "startup cleanup must not request native unlocks"
    );
    assert_eq!(*fake.puts.lock().unwrap(), puts);

    // Counterfactual negative control: all SDK connections and the second
    // process are closed. The unchanged old algorithm really misclassifies
    // the new database/cache using the earlier complete-but-stale snapshot.
    crate::persistence::matrix_state::cleanup_with_inventory(root, crate::cache_dir(), async {
        Some(snapshot)
    })
    .await;
    assert!(!new_db.exists());
    assert!(!cache_new.exists());
    assert_eq!(
        std::fs::read(db.join("matrix-sdk-crypto.sqlite3")).unwrap(),
        original_bytes
    );
    assert_eq!(
        std::fs::read(db.join("sentinel")).unwrap(),
        b"existing process original data"
    );
    assert!(cache_original.join("sentinel").exists());
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

async fn sdk_client(origin: &str, owner: OwnedUserId, device: &str) -> matrix_sdk::Client {
    let client = matrix_sdk::Client::builder()
        .homeserver_url(origin)
        .http_client(
            matrix_sdk::reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap(),
        )
        .build()
        .await
        .unwrap();
    client
        .restore_session(matrix_sdk::authentication::matrix::MatrixSession {
            meta: matrix_sdk::SessionMeta {
                user_id: owner,
                device_id: device.into(),
            },
            tokens: matrix_sdk::SessionTokens {
                access_token: "isolated-grant".into(),
                refresh_token: None,
            },
        })
        .await
        .unwrap();
    client
}

// A profile from before the credential move: a legacy session for the current
// database beside a legacy retained list that names an older database of the
// same account (left by an earlier sign-in). The first restore must migrate
// both and keep the session.
#[tokio::test]
async fn credential_migration_legacy_session_beside_legacy_retained_list_restores() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("profile");
    let (service, _fake, session, retained) = fixture(&root);
    let mut older = session.clone();
    older.client_session.db_path = "db_older".into();
    older.client_session.passphrase = "OLDER_DATABASE_KEY".repeat(2);
    seed_legacy(&retained, &older);
    hagency_store::private::directory(&root.join("db_older")).unwrap();
    hagency_store::private::directory(&root.join("db_original")).unwrap();
    let path = retained.with_file_name("session");
    hagency_store::private::write_new(&path, &serde_json::to_vec(&session).unwrap()).unwrap();
    let loaded = service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .map_err(|error| format!("{error:?}"));
    assert_eq!(
        loaded.map(|s| s.client_session.passphrase),
        Ok(session.client_session.passphrase.clone())
    );
    // The older database keeps its own key under the migrated list.
    let list: Retained = serde_json::from_slice(&std::fs::read(&retained).unwrap()).unwrap();
    assert_eq!(list.databases.len(), 1);
    assert_eq!(
        list.databases[0].database.db_path,
        older.client_session.db_path
    );
    let installation = service.existing_installation().unwrap();
    assert_eq!(
        service
            .vault
            .load(
                &list.databases[0].database.reference,
                &list.databases[0].identity.scope(&installation).unwrap()
            )
            .unwrap()
            .as_slice(),
        older.client_session.passphrase.as_bytes()
    );
    // A second start reads the published documents.
    let again = service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .map_err(|error| format!("{error:?}"));
    assert_eq!(
        again.map(|s| s.client_session.passphrase),
        Ok(session.client_session.passphrase.clone())
    );
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

// The same start when the legacy list names the database the session still
// uses: the session takes the list's reference and no second key is issued.
#[tokio::test]
async fn credential_migration_legacy_session_named_in_legacy_retained_list_restores() {
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("profile");
    let (service, _fake, session, retained) = fixture(&root);
    seed_legacy(&retained, &session);
    let db = root.join("db_original");
    hagency_store::private::directory(&db).unwrap();
    let path = retained.with_file_name("session");
    hagency_store::private::write_new(&path, &serde_json::to_vec(&session).unwrap()).unwrap();
    let loaded = service
        .load_worker(&path, crate::account_session::epoch(), || true)
        .map_err(|error| format!("{error:?}"));
    match loaded {
        Ok(loaded) => {
            assert_eq!(
                loaded.client_session.passphrase,
                session.client_session.passphrase
            );
            let list: Retained =
                serde_json::from_slice(&std::fs::read(&retained).unwrap()).unwrap();
            let published: Stored = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                published.database.reference,
                list.databases[0].database.reference
            );
        }
        Err(error) => panic!("{error}"),
    }
    assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
}

// A developer's locally built app keeps its secrets in private files under
// the profile. A later start (another build, to the OS another program) must
// restore the saved sign-in from those files without the OS store.
#[tokio::test]
async fn credential_development_files_restore_a_saved_sign_in_without_the_os_store() {
    use hagency_store::credential_vault::development::DevelopmentFileStore;
    let _serial = SERIAL.lock().await;
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("profile");
    let (unused, _, session, retained) = fixture(&root);
    assert!(unused.worker.shutdown(std::time::Duration::from_secs(2)));
    hagency_store::private::directory(&root.join("db_original")).unwrap();
    let path = retained.with_file_name("session");
    hagency_store::private::write_new(&path, &serde_json::to_vec(&session).unwrap()).unwrap();
    let files = |os: Arc<Fake>| {
        Vault::new(Arc::new(DevelopmentFileStore::new(
            root.join("credentials-v1/development-secrets"),
            os,
        )))
    };
    let os = Arc::new(Fake::default());
    let first = Arc::new(Service::new(root.clone(), files(os.clone())).unwrap());
    let loaded = first
        .load_worker(&path, crate::account_session::epoch(), || true)
        .map_err(|error| format!("{error:?}"));
    assert_eq!(
        loaded.map(|s| s.client_session.passphrase),
        Ok(session.client_session.passphrase.clone())
    );
    assert!(first.worker.shutdown(std::time::Duration::from_secs(2)));
    // The first process ends: what it held for this sign-in is released.
    drop(first);
    // Nothing went to the OS store, and the document on disk is the new form.
    assert_eq!(*os.puts.lock().unwrap(), 0);
    assert!(os.values.lock().unwrap().is_empty());
    let published: Stored = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(published.database.db_path, session.client_session.db_path);
    // The next start has an empty OS store and still restores.
    let second = Arc::new(Service::new(root.clone(), files(Arc::new(Fake::default()))).unwrap());
    let again = second
        .load_worker(&path, crate::account_session::epoch(), || true)
        .map_err(|error| format!("{error:?}"));
    assert_eq!(
        again.map(|s| s.client_session.passphrase),
        Ok(session.client_session.passphrase.clone())
    );
    assert!(second.worker.shutdown(std::time::Duration::from_secs(2)));
}
