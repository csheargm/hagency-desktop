use super::*;
use std::{collections::BTreeMap, sync::Mutex};

#[derive(Default)]
struct Fake {
    entries: Mutex<BTreeMap<String, Vec<u8>>>,
    failure: Mutex<Option<Error>>,
    writes: Mutex<Vec<usize>>,
}
impl SecretStore for Fake {
    fn get(&self, reference: &SecretRef) -> Result<Zeroizing<Vec<u8>>, Error> {
        if let Some(error) = *self.failure.lock().unwrap() {
            return Err(error);
        }
        self.entries
            .lock()
            .unwrap()
            .get(&reference.entry_name()?)
            .cloned()
            .map(Zeroizing::new)
            .ok_or(Error::Missing)
    }
    fn put(&self, reference: &SecretRef, bytes: &[u8]) -> Result<(), Error> {
        if let Some(error) = *self.failure.lock().unwrap() {
            return Err(error);
        }
        self.writes.lock().unwrap().push(bytes.len());
        self.entries
            .lock()
            .unwrap()
            .insert(reference.entry_name()?, bytes.to_vec());
        Ok(())
    }
    fn remove(&self, reference: &SecretRef) -> Result<(), Error> {
        self.entries
            .lock()
            .unwrap()
            .remove(&reference.entry_name()?)
            .map(|_| ())
            .ok_or(Error::Missing)
    }
}
fn fixture() -> (Arc<Fake>, Vault, Scope, SecretRef) {
    let store = Arc::new(Fake::default());
    let vault = Vault::new(store.clone());
    let scope = Scope::new(
        "0123456789abcdef0123456789abcdef",
        Purpose::Session,
        b"full owner/device/server/client identity",
    )
    .unwrap();
    let reference = Vault::fresh_reference(&scope).unwrap();
    (store, vault, scope, reference)
}
#[test]
fn credential_vault_large_auth_document_stays_out_of_native_store() {
    let (store, vault, scope, reference) = fixture();
    let secret = DocumentKey::random_secret().unwrap();
    vault.create_at(&reference, &secret).unwrap();
    let key = vault.unlock(&reference, &scope).unwrap();
    let value = format!("SECRET_MARKER{}", "very-long-jwt.".repeat(2000));
    let envelope = key.seal(&value).unwrap();
    let encoded = serde_json::to_vec(&envelope).unwrap();
    assert!(!encoded.windows(13).any(|w| w == b"SECRET_MARKER"));
    assert_eq!(key.open::<String>(&envelope).unwrap(), value);
    assert!(
        store
            .writes
            .lock()
            .unwrap()
            .iter()
            .all(|n| *n <= MAX_NATIVE_BYTES)
    );
    assert_eq!(
        vault.create_at(&reference, b"replacement"),
        Err(Error::Conflict)
    );
}
#[test]
fn credential_vault_context_swap_corruption_and_native_unavailability_fail_closed() {
    let (store, vault, scope, reference) = fixture();
    vault
        .create_at(&reference, &DocumentKey::random_secret().unwrap())
        .unwrap();
    let key = vault.unlock(&reference, &scope).unwrap();
    let mut envelope = key.seal(&"secret").unwrap();
    let other = Scope::new(
        "0123456789abcdef0123456789abcdef",
        Purpose::Database,
        b"full owner/device/server/client identity",
    )
    .unwrap();
    assert_eq!(vault.load(&reference, &other).unwrap_err(), Error::Invalid);
    envelope.ciphertext[0] ^= 1;
    assert!(key.open::<String>(&envelope).is_err());
    *store.failure.lock().unwrap() = Some(Error::Unavailable);
    assert_eq!(
        vault.load(&reference, &scope).unwrap_err(),
        Error::Unavailable
    );
    *store.failure.lock().unwrap() = None;
    store.entries.lock().unwrap().clear();
    assert_eq!(vault.load(&reference, &scope).unwrap_err(), Error::Missing);
}
#[test]
fn credential_vault_migration_journal_and_compare_before_publish() {
    use document::{Document, Published};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private");
    crate::private::directory(&root).unwrap();
    let path = root.join("session");
    crate::private::write_new(&path, b"legacy SECRET_MARKER").unwrap();
    let (_, vault, scope, reference) = fixture();
    let doc = Document::open(&path, 1024 * 1024).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc
        .begin(&scope, Some(&source), vec![reference.clone()])
        .unwrap();
    assert!(
        !std::fs::read(root.join(".session.vault-journal"))
            .unwrap()
            .windows(13)
            .any(|w| w == b"SECRET_MARKER")
    );
    vault
        .create_at(&reference, &DocumentKey::random_secret().unwrap())
        .unwrap();
    let key = vault.unlock(&reference, &scope).unwrap();
    let payload = serde_json::to_vec(&key.seal(&"SECRET_MARKER").unwrap()).unwrap();
    let prepared = doc.prepare(journal, &payload).unwrap();
    assert_eq!(
        doc.publish(prepared, Some(&source), || true).unwrap(),
        Published::Durable
    );
    assert_eq!(
        key.open::<String>(&serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap())
            .unwrap(),
        "SECRET_MARKER"
    );
    assert!(!root.join(".session.vault-journal").exists());
    assert!(
        Document::open(&path, 1024).is_err(),
        "actual file lock must exclude another owner"
    );
}
#[test]
fn credential_vault_stale_epoch_and_fresh_token_are_not_overwritten() {
    use document::Document;
    for fresh in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("private");
        crate::private::directory(&root).unwrap();
        let path = root.join("session");
        crate::private::write_new(&path, b"old token").unwrap();
        let (_, _, scope, reference) = fixture();
        let doc = Document::open(&path, 1024).unwrap();
        let source = doc.read().unwrap().unwrap();
        let journal = doc.begin(&scope, Some(&source), vec![reference]).unwrap();
        let prepared = doc.prepare(journal, b"encrypted candidate").unwrap();
        if fresh {
            crate::private::replace(&path, b"fresh token").unwrap();
        }
        let result = doc.publish(prepared, Some(&source), || fresh);
        assert_eq!(
            result,
            Err(if fresh {
                Error::Conflict
            } else {
                Error::Retired
            })
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            if fresh {
                b"fresh token".as_slice()
            } else {
                b"old token".as_slice()
            }
        );
        assert!(doc.pending(&scope).unwrap().is_some());
    }
}

#[tokio::test]
async fn credential_vault_worker_keeps_original_io_and_skips_cancelled_queue() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let worker = Arc::new(worker::Worker::new().unwrap());
    let (entered, observed) = tokio::sync::oneshot::channel();
    let (release, held) = std::sync::mpsc::channel();
    let original = worker.clone();
    let first = tokio::spawn(async move {
        original
            .call(move || {
                let _ = entered.send(());
                held.recv().unwrap();
                Ok(())
            })
            .await
    });
    observed.await.unwrap();
    first.abort();
    let _ = first.await;
    let invoked = Arc::new(AtomicBool::new(false));
    let flag = invoked.clone();
    let original = worker.clone();
    let queued = tokio::spawn(async move {
        original
            .call(move || {
                flag.store(true, Ordering::Release);
                Ok(())
            })
            .await
    });
    tokio::task::yield_now().await;
    queued.abort();
    let _ = queued.await;
    assert!(
        !worker.shutdown(std::time::Duration::from_millis(5)),
        "held original IO is unknown, not cancelled"
    );
    release.send(()).unwrap();
    assert!(worker.shutdown(std::time::Duration::from_secs(2)));
    assert!(!invoked.load(Ordering::Acquire));
}
#[test]
fn credential_vault_unknown_schema_symlink_and_original_replacement_are_refused() {
    use document::Document;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private");
    crate::private::directory(&root).unwrap();
    let path = root.join("session");
    crate::private::write_new(&path, b"original").unwrap();
    let (_, _, scope, reference) = fixture();
    let doc = Document::open(&path, 1024).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc.begin(&scope, Some(&source), vec![reference]).unwrap();
    let prepared = doc.prepare(journal, b"ciphertext").unwrap();
    std::fs::rename(&path, root.join("old")).unwrap();
    crate::private::write_new(&path, b"original").unwrap();
    assert_eq!(
        doc.publish(prepared, Some(&source), || true),
        Err(Error::Conflict),
        "same bytes with different original file ID must not publish"
    );
    assert_eq!(doc.remove_original(&source, || true), Err(Error::Conflict));
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
    drop(doc);
    let bytes = std::fs::read(root.join(".session.vault-journal")).unwrap();
    let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    unknown["version"] = 500.into();
    crate::private::replace(
        &root.join(".session.vault-journal"),
        &serde_json::to_vec(&unknown).unwrap(),
    )
    .unwrap();
    assert_eq!(
        Document::open(&path, 1024)
            .unwrap()
            .pending(&scope)
            .unwrap_err(),
        Error::Invalid
    );
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(root.join("old"), &path).unwrap();
        assert!(Document::open(&path, 1024).unwrap().read().is_err());
        assert_eq!(std::fs::read(root.join("old")).unwrap(), b"original");
    }
}
#[test]
fn credential_vault_create_readback_failure_retains_original_and_journal() {
    use document::Document;
    struct Unconfirmed {
        fake: Fake,
    }
    impl SecretStore for Unconfirmed {
        fn get(&self, reference: &SecretRef) -> Result<Zeroizing<Vec<u8>>, Error> {
            if self
                .fake
                .entries
                .lock()
                .unwrap()
                .contains_key(&reference.entry_name()?)
            {
                return Err(Error::Unavailable);
            }
            Err(Error::Missing)
        }
        fn put(&self, reference: &SecretRef, bytes: &[u8]) -> Result<(), Error> {
            self.fake.put(reference, bytes)
        }
        fn remove(&self, reference: &SecretRef) -> Result<(), Error> {
            self.fake.remove(reference)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private");
    crate::private::directory(&root).unwrap();
    let path = root.join("session");
    crate::private::write_new(&path, b"original database secret").unwrap();
    let (_, _, scope, reference) = fixture();
    let backend = Arc::new(Unconfirmed {
        fake: Fake::default(),
    });
    let vault = Vault::new(backend.clone());
    let doc = Document::open(&path, 1024).unwrap();
    let source = doc.read().unwrap().unwrap();
    doc.begin(&scope, Some(&source), vec![reference.clone()])
        .unwrap();
    assert_eq!(
        vault.create_at(&reference, b"original database secret"),
        Err(Error::Unavailable)
    );
    assert_eq!(
        backend.fake.entries.lock().unwrap().len(),
        1,
        "native write happened despite failed readback"
    );
    assert!(doc.pending(&scope).unwrap().is_some());
    assert_eq!(std::fs::read(&path).unwrap(), b"original database secret");
}

#[test]
fn credential_vault_parent_sync_unknown_reopens_authenticated_v2_without_plaintext_rollback() {
    use document::{Document, Published};
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private");
    crate::private::directory(&root).unwrap();
    let path = root.join("session");
    crate::private::write_new(&path, b"original plaintext").unwrap();
    let (_, vault, scope, reference) = fixture();
    vault
        .create_at(&reference, &DocumentKey::random_secret().unwrap())
        .unwrap();
    let key = vault.unlock(&reference, &scope).unwrap();
    let encoded = serde_json::to_vec(&key.seal(&"actual authenticated payload").unwrap()).unwrap();
    let doc = Document::open(&path, 65536).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc
        .begin(&scope, Some(&source), vec![reference.clone()])
        .unwrap();
    let prepared = doc.prepare(journal, &encoded).unwrap();
    doc.fail_next_parent_sync_after_rename();
    assert_eq!(
        doc.publish(prepared, Some(&source), || true).unwrap(),
        Published::DurabilityUnknown
    );
    assert_eq!(std::fs::read(&path).unwrap(), encoded);
    drop(doc);
    let reopened = Document::open(&path, 65536).unwrap();
    let published = reopened.read().unwrap().unwrap();
    assert_eq!(
        key.open::<String>(&serde_json::from_slice(published.bytes()).unwrap())
            .unwrap(),
        "actual authenticated payload"
    );
    let journal = reopened.pending(&scope).unwrap().unwrap();
    reopened
        .finish_authenticated(&journal, &[reference])
        .unwrap();
    assert!(reopened.pending(&scope).unwrap().is_none());
    assert_eq!(std::fs::read(&path).unwrap(), encoded);
}
#[test]
fn credential_vault_grant_leases_exclude_same_grant_but_allow_another() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private");
    crate::private::directory(&root).unwrap();
    let (_, _, scope, reference) = fixture();
    let other = Vault::fresh_reference(&scope).unwrap();
    let original = document::GrantLease::acquire(&root, &reference).unwrap();
    assert!(matches!(
        document::GrantLease::acquire(&root, &reference),
        Err(Error::Conflict)
    ));
    let independent = document::GrantLease::acquire(&root, &other).unwrap();
    drop(original);
    let restored = document::GrantLease::acquire(&root, &reference).unwrap();
    drop(restored);
    drop(independent);
}

#[test]
fn credential_vault_crash_recovery_rejects_same_bytes_with_a_new_source_file_id() {
    use document::Document;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private");
    crate::private::directory(&root).unwrap();
    let path = root.join("session");
    crate::private::write_new(&path, b"original same bytes").unwrap();
    let (_, _, scope, reference) = fixture();
    let doc = Document::open(&path, 1024).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc.begin(&scope, Some(&source), vec![reference]).unwrap();
    doc.prepare(journal, b"candidate encrypted data").unwrap();
    drop(source);
    drop(doc);
    std::fs::rename(&path, root.join("original-retained-object")).unwrap();
    crate::private::write_new(&path, b"original same bytes").unwrap();
    let reopened = Document::open(&path, 1024).unwrap();
    let new_source = reopened.read().unwrap().unwrap();
    let journal = reopened.pending(&scope).unwrap().unwrap();
    assert_eq!(journal.source_digest.as_deref(), Some(new_source.digest()));
    assert_ne!(
        journal.source_identity.as_ref(),
        Some(new_source.physical_identity())
    );
    let (prepared, _) = reopened.resume_candidate(&journal).unwrap();
    assert_eq!(
        reopened.publish(prepared, Some(&new_source), || true),
        Err(Error::Conflict)
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"original same bytes");
    assert!(reopened.pending(&scope).unwrap().is_some());
}
#[test]
fn credential_vault_retry_does_not_destroy_an_existing_candidate_recovery_proof() {
    use document::Document;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("private");
    crate::private::directory(&root).unwrap();
    let path = root.join("session");
    crate::private::write_new(&path, b"original").unwrap();
    let (_, _, scope, reference) = fixture();
    let doc = Document::open(&path, 1024).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc.begin(&scope, Some(&source), vec![reference]).unwrap();
    doc.prepare(journal, b"original candidate ciphertext")
        .unwrap();
    let original = doc.pending(&scope).unwrap().unwrap();
    assert!(matches!(
        doc.prepare(original.clone(), b"new ciphertext from a retry"),
        Err(Error::Conflict)
    ));
    let after = doc.pending(&scope).unwrap().unwrap();
    assert_eq!(after.candidate_digest(), original.candidate_digest());
    let (_, bytes) = doc.resume_candidate(&after).unwrap();
    assert_eq!(bytes.as_slice(), b"original candidate ciphertext");
}

#[test]
fn credential_vault_explicit_automation_deny_blocks_all_native_io_and_not_fake() {
    if std::env::var_os("HAGENCY_CREDENTIAL_GUARD_CHILD").as_deref()
        == Some(std::ffi::OsStr::new("1"))
    {
        let (_, fake, scope, reference) = fixture();
        let native = NativeStore::new();
        assert_eq!(native.get(&reference).unwrap_err(), Error::Disabled);
        assert_eq!(
            native.put(&reference, b"never sent to OS"),
            Err(Error::Disabled)
        );
        assert_eq!(native.remove(&reference), Err(Error::Disabled));
        fake.create_at(&reference, b"explicit fake backend")
            .unwrap();
        assert_eq!(
            fake.load(&reference, &scope).unwrap().as_slice(),
            b"explicit fake backend"
        );
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "credential_vault::tests::credential_vault_explicit_automation_deny_blocks_all_native_io_and_not_fake", "--test-threads=1"])
        .env("HAGENCY_NATIVE_CREDENTIAL_IO", "deny")
        .env("HAGENCY_CREDENTIAL_GUARD_CHILD", "1")
        .output().unwrap();
    assert!(
        output.status.success(),
        "deny interlock probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn credential_vault_readiness_forces_write_readback_in_a_non_authority_namespace() {
    let (store, vault, authority, _) = fixture();
    let installation = "0123456789abcdef0123456789abcdef";
    vault.check_native_readiness(installation).unwrap();
    let first = store.entries.lock().unwrap().clone();
    vault.check_native_readiness(installation).unwrap();
    let second = store.entries.lock().unwrap().clone();
    assert_eq!(first.len(), 1);
    assert_eq!(
        second.len(),
        1,
        "stable probe does not create unbounded OS entries"
    );
    assert_ne!(
        first, second,
        "a prior read-only success cannot replace a current write test"
    );
    let record: SecretRecord = serde_json::from_slice(second.values().next().unwrap()).unwrap();
    assert_eq!(record.reference.scope().purpose(), Purpose::Readiness);
    assert_eq!(record.bytes.len(), 32);
    assert_eq!(
        vault.load(&record.reference, &authority).unwrap_err(),
        Error::Invalid
    );
    *store.failure.lock().unwrap() = Some(Error::Locked);
    assert_eq!(
        vault.check_native_readiness(installation),
        Err(Error::Locked)
    );
    assert_eq!(*store.entries.lock().unwrap(), second);
}

#[test]
fn credential_vault_retired_preparation_discards_only_original_authenticated_objects() {
    use document::Document;
    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("private");
    crate::private::directory(&private).unwrap();
    let path = private.join("session");
    crate::private::write_new(&path, b"original plaintext stays unchanged").unwrap();
    let (_, vault, scope, reference) = fixture();
    vault
        .create_at(&reference, &DocumentKey::random_secret().unwrap())
        .unwrap();
    let key = vault.unlock(&reference, &scope).unwrap();
    let doc = Document::open(&path, 65536).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc
        .begin(&scope, Some(&source), vec![reference.clone()])
        .unwrap();
    let payload = serde_json::to_vec(&key.seal(&"authenticated candidate").unwrap()).unwrap();
    doc.prepare(journal, &payload).unwrap();
    let journal = doc.pending(&scope).unwrap().unwrap();
    doc.discard_unpublished(&journal, Some(&source), &[reference], |bytes| {
        let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?;
        if key.open::<String>(&envelope)? != "authenticated candidate" {
            return Err(Error::Integrity);
        }
        Ok(())
    })
    .unwrap();
    assert!(doc.pending(&scope).unwrap().is_none());
    assert_eq!(
        std::fs::read(&path).unwrap(),
        b"original plaintext stays unchanged"
    );
}

#[test]
fn credential_vault_same_bytes_journal_replacement_is_not_the_original_witness() {
    use document::Document;
    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("private");
    crate::private::directory(&private).unwrap();
    let path = private.join("session");
    crate::private::write_new(&path, b"original").unwrap();
    let (_, _, scope, reference) = fixture();
    let doc = Document::open(&path, 1024).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc
        .begin(&scope, Some(&source), vec![reference.clone()])
        .unwrap();
    let journal_path = private.join(".session.vault-journal");
    let bytes = std::fs::read(&journal_path).unwrap();
    std::fs::rename(&journal_path, private.join("original-journal-object")).unwrap();
    crate::private::write_new(&journal_path, &bytes).unwrap();
    assert!(matches!(doc.pending(&scope), Err(Error::Conflict)));
    assert_eq!(
        doc.discard_unpublished(&journal, Some(&source), &[reference], |_| Ok(())),
        Err(Error::Conflict)
    );
    assert_eq!(std::fs::read(&journal_path).unwrap(), bytes);
    assert_eq!(std::fs::read(&path).unwrap(), b"original");
}

#[test]
fn credential_vault_replaced_journal_is_rejected_before_canonical_rename() {
    use document::Document;
    for rewrite_self_identity in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        crate::private::directory(&private).unwrap();
        let path = private.join("session");
        crate::private::write_new(&path, b"original source").unwrap();
        let (_, _, scope, reference) = fixture();
        let doc = Document::open(&path, 1024).unwrap();
        let source = doc.read().unwrap().unwrap();
        let journal = doc.begin(&scope, Some(&source), vec![reference]).unwrap();
        let prepared = doc.prepare(journal, b"candidate ciphertext").unwrap();
        let original_journal = doc.pending(&scope).unwrap().unwrap();
        let journal_path = private.join(".session.vault-journal");
        let bytes = std::fs::read(&journal_path).unwrap();
        std::fs::rename(&journal_path, private.join("original-journal-object")).unwrap();
        let mut replacement = crate::private::open(&journal_path, true).unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        if rewrite_self_identity {
            value["journal_identity"] =
                serde_json::to_value(hagency_platform::file_identity(&replacement).unwrap())
                    .unwrap();
        }
        use std::io::Write;
        replacement
            .write_all(&serde_json::to_vec(&value).unwrap())
            .unwrap();
        replacement.sync_all().unwrap();
        assert!(matches!(
            doc.resume_candidate(&original_journal),
            Err(Error::Conflict)
        ));
        assert_eq!(
            doc.publish(prepared, Some(&source), || true),
            Err(Error::Conflict)
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"original source");
        doc.check_original(Some(&source)).unwrap();
    }
}

#[test]
fn credential_vault_finish_failure_after_rename_reports_committed_unknown() {
    use document::{Document, Published};
    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("private");
    crate::private::directory(&private).unwrap();
    let path = private.join("session");
    crate::private::write_new(&path, b"old source").unwrap();
    let (_, _, scope, reference) = fixture();
    let doc = Document::open(&path, 1024).unwrap();
    let source = doc.read().unwrap().unwrap();
    let journal = doc.begin(&scope, Some(&source), vec![reference]).unwrap();
    let prepared = doc.prepare(journal, b"new committed payload").unwrap();
    doc.fail_next_finish_after_rename();
    assert_eq!(
        doc.publish(prepared, Some(&source), || true).unwrap(),
        Published::DurabilityUnknown
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"new committed payload");
    assert!(doc.check_original(Some(&source)).is_err());
    assert!(doc.pending(&scope).unwrap().is_some());
}

/// Opt-in qualification creates and removes one random entry owned by this test.
/// Ordinary CI denies native IO and never runs this test.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires the logged-in user's real macOS Keychain"]
fn credential_vault_native_macos_roundtrip() {
    let store = Arc::new(NativeStore::new());
    let vault = Vault::new(store.clone());
    let scope = Scope::new(
        &super::random_id().unwrap(),
        Purpose::Readiness,
        b"r03-native-qualification",
    )
    .unwrap();
    let reference = Vault::fresh_reference(&scope).unwrap();
    // This is a random nonsecret locator, useful if a native prompt outlives the observer.
    eprintln!(
        "qualification_reference={}",
        serde_json::to_string(&reference).unwrap()
    );
    let secret = DocumentKey::random_secret().unwrap();
    let result = vault.create_at(&reference, &secret).and_then(|()| {
        if vault.load(&reference, &scope)?.as_slice() == secret.as_slice() {
            Ok(())
        } else {
            Err(Error::Integrity)
        }
    });
    let cleanup = store.remove(&reference);
    result.expect("native credential write/readback must succeed");
    cleanup.expect("remove the exact qualification-owned entry");
    assert!(matches!(store.get(&reference), Err(Error::Missing)));
}

fn development_fixture(
    directory: &std::path::Path,
) -> (Arc<Fake>, Vault, Vault, Scope, SecretRef) {
    let earlier = Arc::new(Fake::default());
    let files = Vault::new(Arc::new(development::DevelopmentFileStore::new(
        directory.join("development-secrets"),
        earlier.clone(),
    )));
    let scope = Scope::new(
        "0123456789abcdef0123456789abcdef",
        Purpose::Session,
        b"full owner/device/server/client identity",
    )
    .unwrap();
    let reference = Vault::fresh_reference(&scope).unwrap();
    (earlier.clone(), Vault::new(earlier), files, scope, reference)
}

#[test]
fn credential_vault_development_files_keep_new_secrets_out_of_the_os_store() {
    let temporary = tempfile::tempdir().unwrap();
    let (earlier, _, files, scope, reference) = development_fixture(temporary.path());
    files.create_at(&reference, b"kept in a private file").unwrap();
    assert_eq!(
        files.load(&reference, &scope).unwrap().as_slice(),
        b"kept in a private file"
    );
    assert!(earlier.entries.lock().unwrap().is_empty());
    assert!(earlier.writes.lock().unwrap().is_empty());
    let directory = temporary.path().join("development-secrets");
    let entries = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&entries[0]), 0o600);
        assert_eq!(mode(&directory), 0o700);
    }
    // The file name says nothing about the account or the purpose.
    let name = entries[0].file_name().unwrap().to_str().unwrap().to_owned();
    assert!(name.len() == 71 && name.ends_with(".secret"));
    assert!(name[..64].bytes().all(|byte| byte.is_ascii_hexdigit()));
    // A second secret for the same reference is refused, as in the OS store.
    assert_eq!(
        files.create_at(&reference, b"another value"),
        Err(Error::Conflict)
    );
    // Removal is of the file only.
    let store = development::DevelopmentFileStore::new(directory.clone(), earlier.clone());
    store.remove(&reference).unwrap();
    assert_eq!(store.remove(&reference), Err(Error::Missing));
    assert_eq!(files.load(&reference, &scope).unwrap_err(), Error::Missing);
}

#[test]
fn credential_vault_development_files_take_an_earlier_os_secret_once() {
    let temporary = tempfile::tempdir().unwrap();
    let (earlier, os, files, scope, reference) = development_fixture(temporary.path());
    os.create_at(&reference, b"written by an earlier build").unwrap();
    // A refused OS read is reported as it is and leaves no file behind.
    *earlier.failure.lock().unwrap() = Some(Error::Denied);
    assert_eq!(files.load(&reference, &scope).unwrap_err(), Error::Denied);
    assert!(!temporary.path().join("development-secrets").exists());
    // An allowed read copies the secret; later reads never ask the OS again.
    *earlier.failure.lock().unwrap() = None;
    assert_eq!(
        files.load(&reference, &scope).unwrap().as_slice(),
        b"written by an earlier build"
    );
    *earlier.failure.lock().unwrap() = Some(Error::Denied);
    assert_eq!(
        files.load(&reference, &scope).unwrap().as_slice(),
        b"written by an earlier build"
    );
    // The OS copy is left as it was.
    assert_eq!(earlier.entries.lock().unwrap().len(), 1);
}

#[test]
fn credential_vault_development_files_are_selected_only_by_their_own_word() {
    if std::env::var_os("HAGENCY_CREDENTIAL_GUARD_CHILD").as_deref()
        == Some(std::ffi::OsStr::new("development"))
    {
        // The word selects the files only in a build that has them.
        assert_eq!(development::selected(), development::COMPILED);
        return;
    }
    // This process: unset, or `deny` under the automation guard.
    assert!(!development::selected());
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "credential_vault::tests::credential_vault_development_files_are_selected_only_by_their_own_word", "--test-threads=1"])
        .env("HAGENCY_NATIVE_CREDENTIAL_IO", development::SELECTOR)
        .env("HAGENCY_CREDENTIAL_GUARD_CHILD", "development")
        .output().unwrap();
    assert!(
        output.status.success(),
        "selector probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
