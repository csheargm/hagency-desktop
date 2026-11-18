//! The earlier (OS) store is asked about a secret at most once per run.
use super::DevelopmentFileStore;
use crate::credential_vault::{Error, Purpose, Scope, SecretRef, SecretStore, Vault};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use zeroize::Zeroizing;

/// An OS store stand-in that counts every read and holds at most one secret.
#[derive(Default)]
struct CountingEarlier {
    reads: AtomicUsize,
    held: Mutex<Option<Vec<u8>>>,
    failure: Mutex<Option<Error>>,
}
impl SecretStore for CountingEarlier {
    fn get(&self, _: &SecretRef) -> Result<Zeroizing<Vec<u8>>, Error> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = *self.failure.lock().unwrap() {
            return Err(error);
        }
        self.held.lock().unwrap().clone().map(Zeroizing::new).ok_or(Error::Missing)
    }
    fn put(&self, _: &SecretRef, _: &[u8]) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
    fn remove(&self, _: &SecretRef) -> Result<(), Error> {
        Err(Error::Unsupported)
    }
}

fn reference() -> SecretRef {
    let scope = Scope::new("0123456789abcdef0123456789abcdef", Purpose::Session, b"identity").unwrap();
    Vault::fresh_reference(&scope).unwrap()
}

fn fixture() -> (tempfile::TempDir, Arc<CountingEarlier>, DevelopmentFileStore) {
    let temporary = tempfile::tempdir().unwrap();
    let earlier = Arc::new(CountingEarlier::default());
    let store = DevelopmentFileStore::new(temporary.path().join("secrets"), earlier.clone());
    (temporary, earlier, store)
}

#[test]
fn a_missing_secret_is_asked_of_the_os_store_once_per_run() {
    let (_temporary, earlier, store) = fixture();
    let missing = reference();
    for _ in 0..50 {
        assert_eq!(store.get(&missing).unwrap_err(), Error::Missing);
    }
    assert_eq!(earlier.reads.load(Ordering::SeqCst), 1);
    // A second secret is its own question.
    assert_eq!(store.get(&reference()).unwrap_err(), Error::Missing);
    assert_eq!(earlier.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn a_migrated_secret_is_read_from_its_file_without_the_os_store() {
    let (_temporary, earlier, store) = fixture();
    let secret = reference();
    *earlier.held.lock().unwrap() = Some(b"earlier build".to_vec());
    for _ in 0..10 {
        assert_eq!(store.get(&secret).unwrap().as_slice(), b"earlier build");
    }
    assert_eq!(earlier.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn a_refused_os_read_is_not_remembered_as_missing() {
    let (_temporary, earlier, store) = fixture();
    let secret = reference();
    *earlier.held.lock().unwrap() = Some(b"earlier build".to_vec());
    *earlier.failure.lock().unwrap() = Some(Error::Denied);
    assert_eq!(store.get(&secret).unwrap_err(), Error::Denied);
    *earlier.failure.lock().unwrap() = None;
    assert_eq!(store.get(&secret).unwrap().as_slice(), b"earlier build");
    assert_eq!(earlier.reads.load(Ordering::SeqCst), 2);
}

#[test]
fn a_secret_saved_after_a_missing_answer_is_found_and_a_removed_one_stays_missing() {
    let (_temporary, earlier, store) = fixture();
    let secret = reference();
    assert_eq!(store.get(&secret).unwrap_err(), Error::Missing);
    store.put(&secret, b"saved now").unwrap();
    assert_eq!(store.get(&secret).unwrap().as_slice(), b"saved now");
    // The OS copy is not read back after a removal.
    *earlier.held.lock().unwrap() = Some(b"stale OS copy".to_vec());
    store.remove(&secret).unwrap();
    assert_eq!(store.get(&secret).unwrap_err(), Error::Missing);
    assert_eq!(earlier.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn a_files_only_store_never_has_an_earlier_store_to_ask() {
    let temporary = tempfile::tempdir().unwrap();
    let store = DevelopmentFileStore::files_only(temporary.path().join("tokens"));
    let secret = reference();
    assert_eq!(store.get(&secret).unwrap_err(), Error::Missing);
    store.put(&secret, b"token").unwrap();
    assert_eq!(store.get(&secret).unwrap().as_slice(), b"token");
}
