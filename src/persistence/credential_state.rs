//! Native secrets are opened only by the owned credential worker. Disk readers
//! can inspect a public identity, but authority always requires authenticated
//! payloads and the exact original OS database secret.
mod recovery;
mod database_inspect;
mod revoke_transport;
mod owners;
mod retained;
pub(crate) use retained::{retain, retained_paths, retained_session};

use super::{ClientSessionPersisted, FullSessionPersisted, PersistedUserSession, SlidingSyncVersion};
use anyhow::{Result, anyhow, ensure};
use hagency_store::credential_vault::{
    self as vault, DocumentKey, Envelope, Error, Purpose, Scope, SecretRef, Vault,
    document::{Document, Snapshot, Published},
    worker::Worker,
};
use matrix_sdk::ruma::OwnedUserId;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use zeroize::Zeroizing;

const MAX_SESSION: usize = 1024 * 1024;
const VERSION: u32 = 2;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Database {
    pub homeserver: String,
    pub db_path: PathBuf,
    pub reference: SecretRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_account: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub owner: OwnedUserId,
    pub device: String,
    pub homeserver: String,
    pub oauth_client_id: Option<String>,
    pub db_path: PathBuf,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    version: u32,
    identity: Identity,
    database: Database,
    authentication: Envelope,
    revocation_key: SecretRef,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Authentication {
    database: Database,
    revocation_target: Option<revoke_transport::Target>,
    revision: u64,
    identity: Identity,
    user_session: PersistedUserSession,
    sync_token: Option<String>,
    sliding_sync_version: SlidingSyncVersion,
}

/// RAM custody belongs to each original SDK callback/worker; retiring the public
/// registry stops new use but does not pretend to destroy existing SDK clones.
pub(crate) struct Unlocked {
    identity: Identity,
    database: Database,
    secret: Zeroizing<String>,
    key: DocumentKey,
    revocation: DocumentKey,
    logout: AtomicBool,
    revision: std::sync::atomic::AtomicU64,
    target: std::sync::OnceLock<revoke_transport::Target>,
    recovery_root: PathBuf,
    _grant_lease: Arc<vault::document::GrantLease>,
    sync_token: Option<String>,
    sliding_sync_version: SlidingSyncVersion,
}
impl Unlocked {
    fn next_revision(&self) -> Result<u64> {
        self.revision
            .try_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map(|value| value + 1)
            .map_err(|_| Error::Capacity.into())
    }
    fn decode(&self, stored: Stored) -> Result<FullSessionPersisted> {
        ensure!(
            stored.version == VERSION
                && stored.identity == self.identity
                && stored.database == self.database
                && stored.revocation_key == *self.revocation.reference(),
            "credential_identity_changed"
        );
        let auth: Authentication = self.key.open(&stored.authentication)?;
        ensure!(
            auth.database == stored.database
                && auth.identity == self.identity
                && identity_from(
                    &auth.user_session,
                    &stored.database.homeserver,
                    &stored.database.db_path
                ) == self.identity,
            "credential_integrity_failed"
        );
        if let Some(target) = auth.revocation_target.as_ref() {
            target.validate_session(&auth.user_session)?;
            ensure!(
                target.matches_homeserver(&self.identity.homeserver),
                "credential_integrity_failed"
            );
        }
        self.revision.fetch_max(auth.revision, Ordering::AcqRel);
        Ok(FullSessionPersisted {
            client_session: ClientSessionPersisted {
                homeserver: self.database.homeserver.clone(),
                db_path: self.database.db_path.clone(),
                passphrase: self.secret.to_string(),
            },
            user_session: auth.user_session,
            sync_token: auth.sync_token,
            sliding_sync_version: auth.sliding_sync_version,
        })
    }
    fn encode(&self, session: FullSessionPersisted) -> Result<Vec<u8>> {
        self.encode_at(session, self.next_revision()?)
    }
    fn encode_at(&self, session: FullSessionPersisted, revision: u64) -> Result<Vec<u8>> {
        ensure!(
            identity(&session) == self.identity
                && session.client_session.passphrase == self.secret.as_str(),
            "credential_identity_changed"
        );
        let value = Authentication {
            database: self.database.clone(),
            revocation_target: self.target.get().cloned(),
            revision,
            identity: self.identity.clone(),
            user_session: session.user_session,
            sync_token: session.sync_token,
            sliding_sync_version: session.sliding_sync_version,
        };
        Ok(serde_json::to_vec(&Stored {
            version: VERSION,
            identity: self.identity.clone(),
            database: self.database.clone(),
            authentication: self.key.seal(&value)?,
            revocation_key: self.revocation.reference().clone(),
        })?)
    }
}
struct Original {
    key: DocumentKey,
    revocation: DocumentKey,
    secret: Zeroizing<String>,
    authenticated: Authentication,
}
pub(crate) struct ExistingDatabase {
    pub identity: Identity,
    pub client_session: ClientSessionPersisted,
}

struct Cached {
    epoch: u64,
    value: Arc<Unlocked>,
    owner: std::sync::Weak<owners::Owner>,
}
pub(crate) struct Service {
    root: PathBuf,
    vault: Vault,
    worker: Worker,
    cache: Mutex<HashMap<PathBuf, Cached>>,
    retired: Mutex<Vec<Cached>>,
    revoke_cursor: Mutex<Option<PathBuf>>,
    ram_cursor: std::sync::atomic::AtomicUsize,
    maintenance_disk_first: AtomicBool,
}
static SERVICE: std::sync::OnceLock<std::result::Result<Arc<Service>, Error>> =
    std::sync::OnceLock::new();
fn global() -> std::result::Result<&'static Arc<Service>, Error> {
    SERVICE
        .get_or_init(|| {
            Service::new(crate::app_data_dir().to_owned(), Vault::native())
                .map(Arc::new)
                .map_err(classify)
        })
        .as_ref()
        .map_err(|error| *error)
}
// First sign-in can precede any window-state write. Provision missing profile
// ancestors privately; existing profiles keep their original permissions, while
// every credential/account document still checks its own private directory.
fn provision_data_root(root: &Path) -> Result<()> {
    let mut missing = Vec::new();
    let mut current = root;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_dir() && !metadata.file_type().is_symlink(),
                    "credential_profile_directory_invalid"
                );
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current);
                current = current.parent().ok_or(Error::Invalid)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for path in missing.into_iter().rev() {
        hagency_store::private::directory(path)?;
    }
    Ok(())
}
impl Service {
    pub(crate) fn new(root: PathBuf, vault: Vault) -> Result<Self> {
        provision_data_root(&root)?;
        Ok(Self {
            root,
            vault,
            worker: Worker::new()?,
            cache: Mutex::new(HashMap::new()),
            retired: Mutex::new(Vec::new()),
            revoke_cursor: Mutex::new(None),
            ram_cursor: std::sync::atomic::AtomicUsize::new(0),
            maintenance_disk_first: AtomicBool::new(false),
        })
    }
    fn start_maintenance(self: &Arc<Self>) -> Result<()> {
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| Error::Unavailable)?;
        let weak = Arc::downgrade(self);
        self.worker.set_idle_operation(Arc::new(move || {
            let Some(service) = weak.upgrade() else {
                return;
            };
            let result = service.maintenance_once(&runtime);
            if let Err(error) = result {
                makepad_widgets::log!("Credential revocation remains pending: {}", classify(error));
            }
        }))?;
        Ok(())
    }
    /// Runs on the existing owned worker. Alternate sources even when the
    /// preferred operation errors; only an absent source falls through. A turn
    /// still attempts at most one record, so a stuck RAM grant cannot starve
    /// independently recoverable on-disk revocations.
    fn maintenance_once(&self, runtime: &tokio::runtime::Handle) -> Result<bool> {
        if self
            .maintenance_disk_first
            .fetch_xor(true, Ordering::AcqRel)
        {
            if let Some(complete) = recovery::retry_one(self, runtime)? {
                return Ok(complete);
            }
            Ok(recovery::retry_ram_one(self, runtime)?.unwrap_or(true))
        } else {
            if let Some(complete) = recovery::retry_ram_one(self, runtime)? {
                return Ok(complete);
            }
            Ok(recovery::retry_one(self, runtime)?.unwrap_or(true))
        }
    }
    fn installation(&self) -> Result<String> {
        Ok(vault::document::installation(&self.root, true)?)
    }
    fn existing_installation(&self) -> Result<String> {
        Ok(vault::document::installation(&self.root, false)?)
    }
    fn read_original(&self, stored: &Stored) -> Result<Original> {
        ensure!(stored.version == VERSION, "credential_schema_unsupported");
        let installation = self.existing_installation()?;
        let context = serde_json::to_vec(&stored.identity)?;
        let auth_scope = Scope::new(&installation, Purpose::Session, &context)?;
        let revocation_scope = Scope::new(&installation, Purpose::Revocations, &context)?;
        ensure!(
            stored.database.homeserver == stored.identity.homeserver
                && stored.database.db_path == stored.identity.db_path,
            "credential_identity_changed"
        );
        let key = self
            .vault
            .unlock(stored.authentication.reference(), &auth_scope)?;
        let revocation = self
            .vault
            .unlock(&stored.revocation_key, &revocation_scope)?;
        let secret = self.resolve_database(&stored.database, &stored.identity, &installation)?;
        let authenticated: Authentication = key.open(&stored.authentication)?;
        ensure!(
            authenticated.database == stored.database
                && authenticated.identity == stored.identity
                && identity_from(
                    &authenticated.user_session,
                    &stored.database.homeserver,
                    &stored.database.db_path
                ) == stored.identity,
            "credential_integrity_failed"
        );
        if let Some(target) = authenticated.revocation_target.as_ref() {
            target.validate_session(&authenticated.user_session)?;
            ensure!(
                target.matches_homeserver(&stored.identity.homeserver),
                "credential_integrity_failed"
            );
        }
        Ok(Original {
            key,
            revocation,
            secret,
            authenticated,
        })
    }
    fn unlock(&self, stored: &Stored, path: &Path) -> Result<Arc<Unlocked>> {
        let original = self.read_original(stored)?;
        let recovery_root = path
            .parent()
            .ok_or(Error::Invalid)?
            .join("credential-recovery-v1");
        hagency_store::private::directory(&recovery_root)?;
        let cached = self
            .cache
            .lock()
            .map_err(|_| anyhow!("credential_cache_unavailable"))?
            .get(path)
            .filter(|old| {
                old.epoch == crate::account_session::epoch()
                    && old.value.key.reference() == stored.authentication.reference()
            })
            .map(|old| old.value.clone());
        if let Some(cached) = cached {
            ensure!(
                cached.identity == stored.identity
                    && cached.database == stored.database
                    && cached.secret.as_str() == original.secret.as_str()
                    && cached.revocation.reference() == &stored.revocation_key,
                "credential_identity_changed"
            );
            cached
                .revision
                .fetch_max(original.authenticated.revision, Ordering::AcqRel);
            return Ok(cached);
        }
        let lease = Arc::new(vault::document::GrantLease::acquire(
            &recovery_root,
            stored.authentication.reference(),
        )?);
        Ok(Arc::new(Unlocked {
            identity: stored.identity.clone(),
            database: stored.database.clone(),
            secret: original.secret,
            key: original.key,
            revocation: original.revocation,
            logout: AtomicBool::new(false),
            revision: std::sync::atomic::AtomicU64::new(original.authenticated.revision),
            target: target_cell(original.authenticated.revocation_target),
            recovery_root,
            _grant_lease: lease,
            sync_token: original.authenticated.sync_token,
            sliding_sync_version: original.authenticated.sliding_sync_version,
        }))
    }
    fn remember(&self, path: &Path, epoch: u64, value: Arc<Unlocked>) -> Result<()> {
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow!("credential_cache_unavailable"))?;
        let retired_paths: Vec<_> = cache
            .iter()
            .filter(|(_, value)| value.epoch != epoch)
            .map(|(path, _)| path.clone())
            .collect();
        let mut retired = Vec::new();
        for path in retired_paths {
            if let Some(old) = cache.remove(&path) {
                retired.push(old);
            }
        }
        ensure!(
            cache.contains_key(path) || cache.len() < 128,
            "credential_cache_capacity"
        );
        let owner = cache
            .get(path)
            .filter(|old| old.epoch == epoch && old.value.key.reference() == value.key.reference())
            .map(|old| old.owner.clone())
            .unwrap_or_default();
        if let Some(old) = cache.insert(
            path.into(),
            Cached {
                epoch,
                value,
                owner,
            },
        ) {
            retired.push(old);
        }
        drop(cache);
        drop(retired); // File/lease destruction stays outside metadata mutexes.
        Ok(())
    }
    fn bind_owner(&self, path: &Path, owner: &Arc<owners::Owner>) -> Result<()> {
        let mut cache = self.cache.lock().map_err(|_| Error::Unavailable)?;
        let cached = cache.get_mut(path).ok_or(Error::Missing)?;
        ensure!(
            cached.epoch == owner.epoch
                && owner
                    .keys
                    .get()
                    .is_some_and(|keys| keys.key.reference() == cached.value.key.reference()),
            "credential_identity_changed"
        );
        cached.owner = Arc::downgrade(owner);
        Ok(())
    }
    fn owns_owner(&self, path: &Path, owner: &Arc<owners::Owner>) -> bool {
        let original = self.cache.lock().ok().and_then(|cache| {
            cache
                .get(path)
                .filter(|cached| cached.epoch == owner.epoch)
                .and_then(|cached| cached.owner.upgrade())
        });
        original.is_some_and(|original| Arc::ptr_eq(&original, owner))
    }
    fn cached(&self, path: &Path, epoch: u64) -> Result<Arc<Unlocked>> {
        let cache = self
            .cache
            .lock()
            .map_err(|_| anyhow!("credential_cache_unavailable"))?;
        cache
            .get(path)
            .filter(|v| v.epoch == epoch)
            .map(|v| v.value.clone())
            .ok_or_else(|| anyhow!("credential_unlock_required"))
    }
    fn prepare(
        &self,
        path: &Path,
        session: FullSessionPersisted,
        fresh: Option<&FreshDatabase>,
    ) -> Result<Ready> {
        self.cleanup_retired();
        let doc = Document::open(path, MAX_SESSION)?;
        let source = doc.read()?;
        self.prepare_document(doc, source, session, fresh)
    }
    fn prepare_document(
        &self,
        doc: Document,
        source: Option<Snapshot>,
        session: FullSessionPersisted,
        fresh: Option<&FreshDatabase>,
    ) -> Result<Ready> {
        let context = identity(&session);
        self.check_account_path(doc.path(), Some(&context.owner))?;
        let existing_version = source.as_ref().is_some_and(|snapshot| {
            serde_json::from_slice::<serde_json::Value>(snapshot.bytes())
                .is_ok_and(|value| value.get("version").is_some())
        });
        let installation = if existing_version {
            self.existing_installation()?
        } else {
            self.installation()?
        };
        let bytes = serde_json::to_vec(&context)?;
        let scope = Scope::new(&installation, Purpose::Session, &bytes)?;
        let db_scope = database_scope(&installation, &context)?;
        let revocation_scope = Scope::new(&installation, Purpose::Revocations, &bytes)?;
        // Recovery reuses journal-owned references. NoEntry is permission to
        // create only while an authenticated legacy source still supplies the
        // exact original secret; a published v2 DB never takes this path.
        let pending = doc.pending(&scope)?;
        let existing = if let Some(snapshot) = source.as_ref() {
            let value: serde_json::Value = serde_json::from_slice(snapshot.bytes())?;
            if value.get("version").is_some() {
                Some(serde_json::from_value::<Stored>(value)?)
            } else {
                None
            }
        } else {
            None
        };
        let selected_database = if let Some(existing) = existing.as_ref() {
            // Authentication and DB provenance must both be verified before a
            // fresh grant can reuse an existing database reference.
            let original = self.read_original(existing)?;
            if existing.identity.owner == context.owner
                && existing.identity.device == context.device
                && existing.database.homeserver == context.homeserver
                && existing.database.db_path == context.db_path
            {
                ensure!(
                    original.secret.as_str() == session.client_session.passphrase,
                    "credential_original_secret_mismatch"
                );
                Some(existing.database.clone())
            } else {
                None
            }
        } else if let Some(fresh) = fresh {
            fresh.validate(&self.root, &session.client_session)?;
            None
        } else {
            // `doc` is held here and has no published version (`existing` is
            // none). A legacy retained list is migrated inside this lookup and
            // must not open the same session document again.
            let session_held = doc.path().file_name() == Some(std::ffi::OsStr::new("session"));
            self.retained_database_with(&context, session_held)?
        };
        if selected_database.is_none() && fresh.is_none() && source.is_none() {
            self.validate_unreferenced_database(&context)?;
        }
        if let Some(database) = selected_database.as_ref() {
            let secret = self.resolve_database(database, &context, &installation)?;
            ensure!(
                secret.as_str() == session.client_session.passphrase,
                "credential_original_secret_mismatch"
            );
        }
        let refs = if let Some(journal) = pending {
            journal.references
        } else if let Some(database) = selected_database.as_ref() {
            vec![
                Vault::fresh_reference(&scope)?,
                database.reference.clone(),
                Vault::fresh_reference(&revocation_scope)?,
            ]
        } else if let Some(existing) = existing {
            let original = self.read_original(&existing)?;
            // A missing key in an existing v2 document is never repaired by
            // generating a replacement, even when sign-in just succeeded.
            if database_scope(&installation, &existing.identity)? == db_scope {
                ensure!(
                    original.secret.as_str() == session.client_session.passphrase,
                    "credential_original_secret_mismatch"
                );
                vec![
                    Vault::fresh_reference(&scope)?,
                    existing.database.reference,
                    Vault::fresh_reference(&revocation_scope)?,
                ]
            } else {
                vec![
                    Vault::fresh_reference(&scope)?,
                    Vault::fresh_reference(&db_scope)?,
                    Vault::fresh_reference(&revocation_scope)?,
                ]
            }
        } else {
            vec![
                Vault::fresh_reference(&scope)?,
                Vault::fresh_reference(&db_scope)?,
                Vault::fresh_reference(&revocation_scope)?,
            ]
        };
        ensure!(refs.len() == 3, "credential_migration_journal_invalid");
        let journal = doc.begin(&scope, source.as_ref(), refs.clone())?;
        let auth_ref = refs
            .iter()
            .find(|r| r.scope().purpose() == Purpose::Session)
            .ok_or(Error::Invalid)?;
        let db_ref = refs
            .iter()
            .find(|r| r.scope().purpose() == Purpose::Database)
            .ok_or(Error::Invalid)?;
        if let Some(database) = selected_database.as_ref() {
            ensure!(
                &database.reference == db_ref,
                "credential_migration_journal_invalid"
            );
        } else {
            self.create_original(
                db_ref,
                &db_scope,
                session.client_session.passphrase.as_bytes(),
            )?;
        }
        let revocation_ref = refs
            .iter()
            .find(|r| r.scope().purpose() == Purpose::Revocations)
            .ok_or(Error::Invalid)?;
        let revocation = match self.vault.unlock(revocation_ref, &revocation_scope) {
            Ok(key) => key,
            Err(Error::Missing) => {
                self.vault
                    .create_at(revocation_ref, &DocumentKey::random_secret()?)?;
                self.vault.unlock(revocation_ref, &revocation_scope)?
            }
            Err(error) => return Err(error.into()),
        };
        let recovery_root = doc
            .path()
            .parent()
            .ok_or(Error::Invalid)?
            .join("credential-recovery-v1");
        hagency_store::private::directory(&recovery_root)?;
        let lease = Arc::new(vault::document::GrantLease::acquire(
            &recovery_root,
            auth_ref,
        )?);
        let key = match self.vault.unlock(auth_ref, &scope) {
            Ok(key) => key,
            Err(Error::Missing) => {
                self.vault
                    .create_at(auth_ref, &DocumentKey::random_secret()?)?;
                self.vault.unlock(auth_ref, &scope)?
            }
            Err(error) => return Err(error.into()),
        };
        let unlocked = Arc::new(Unlocked {
            identity: context,
            database: selected_database.unwrap_or(Database {
                homeserver: session.client_session.homeserver.clone(),
                db_path: session.client_session.db_path.clone(),
                reference: db_ref.clone(),
                legacy_account: None,
            }),
            secret: Zeroizing::new(session.client_session.passphrase.clone()),
            key,
            revocation,
            logout: AtomicBool::new(false),
            revision: std::sync::atomic::AtomicU64::new(0),
            target: std::sync::OnceLock::new(),
            recovery_root,
            _grant_lease: lease,
            sync_token: session.sync_token.clone(),
            sliding_sync_version: session.sliding_sync_version.clone(),
        });
        Ok(Ready {
            doc,
            source,
            journal,
            unlocked,
            initial_session: session,
        })
    }
    fn create_original(&self, reference: &SecretRef, scope: &Scope, secret: &[u8]) -> Result<()> {
        match self.vault.load(reference, scope) {
            Ok(original) => ensure!(
                original.as_slice() == secret,
                "credential_original_secret_mismatch"
            ),
            Err(Error::Missing) => self.vault.create_at(reference, secret)?,
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
    fn recover_session_journal(
        &self,
        doc: &Document,
        path: &Path,
        current: impl Fn() -> bool,
    ) -> Result<Option<Arc<Unlocked>>> {
        let installation = match self.existing_installation() {
            Ok(id) => id,
            Err(error) if error.downcast_ref::<Error>() == Some(&Error::Missing) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let Some(journal) = doc.pending_for_installation(&installation)? else {
            return Ok(None);
        };
        if journal.candidate_digest().is_none() {
            return Ok(None);
        }
        let source = doc.read()?;
        if source.as_ref().map(Snapshot::digest) == journal.candidate_digest() {
            let stored: Stored =
                serde_json::from_slice(source.as_ref().ok_or(Error::Missing)?.bytes())?;
            self.check_account_path(path, Some(&stored.identity.owner))?;
            let unlocked = self.unlock(&stored, path)?;
            unlocked.decode(stored)?;
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Storage)?;
            if !current() {
                return Err(Error::Retired.into());
            }
            doc.finish_authenticated(
                &journal,
                &[
                    unlocked.key.reference().clone(),
                    unlocked.database.reference.clone(),
                    unlocked.revocation.reference().clone(),
                ],
            )?;
            return Ok(Some(unlocked));
        }
        let (prepared, bytes) = doc.resume_candidate(&journal)?;
        let stored: Stored = serde_json::from_slice(&bytes)?;
        self.check_account_path(path, Some(&stored.identity.owner))?;
        let unlocked = self.unlock(&stored, path)?;
        unlocked.decode(stored)?;
        ensure!(
            journal.references
                == [
                    unlocked.key.reference().clone(),
                    unlocked.database.reference.clone(),
                    unlocked.revocation.reference().clone()
                ],
            "credential_migration_journal_invalid"
        );
        let _guard = super::SESSION_IO.lock().map_err(|_| Error::Storage)?;
        if doc.publish(prepared, source.as_ref(), &current)? != Published::Durable {
            return Err(Error::DurabilityUnknown.into());
        }
        Ok(Some(unlocked))
    }
    fn load_worker(
        &self,
        path: &Path,
        epoch: u64,
        current: impl Fn() -> bool,
    ) -> Result<FullSessionPersisted> {
        self.cleanup_retired();
        {
            let _guard = super::SESSION_IO
                .lock()
                .map_err(|_| anyhow!("credential_storage_unavailable"))?;
            flush_retired_snapshots(false)?;
        }
        ensure!(current(), "credential_operation_retired");
        let doc = Document::open(path, MAX_SESSION)?;
        let recovered = self.recover_session_journal(&doc, path, &current)?;
        let source = doc.read()?.ok_or(Error::Missing)?;
        let value: serde_json::Value = serde_json::from_slice(source.bytes())?;
        if value.get("version").is_some() {
            let stored: Stored = serde_json::from_value(value)?;
            self.check_account_path(path, Some(&stored.identity.owner))?;
            let unlocked = if let Some(unlocked) = recovered {
                unlocked
            } else {
                self.unlock(&stored, path)?
            };
            let mut result = unlocked.decode(stored)?;
            if let Some(journal) = doc.pending(unlocked.key.reference().scope())? {
                doc.finish_authenticated(
                    &journal,
                    &[
                        unlocked.key.reference().clone(),
                        unlocked.database.reference.clone(),
                        unlocked.revocation.reference().clone(),
                    ],
                )?;
            }
            {
                let _guard = super::SESSION_IO
                    .lock()
                    .map_err(|_| anyhow!("credential_storage_unavailable"))?;
                ensure!(current(), "credential_operation_retired");
                result = recovery::absorb_keep(&doc, &unlocked, result, &current)?;
            }
            self.remember(path, epoch, unlocked)?;
            return Ok(result);
        }
        let session: FullSessionPersisted = serde_json::from_value(value)?;
        let ready = self.prepare_document(doc, Some(source), session, None)?;
        let _guard = super::SESSION_IO
            .lock()
            .map_err(|_| anyhow!("credential_storage_unavailable"))?;
        ensure!(current(), "credential_operation_retired");
        let unlocked = ready.unlocked.clone();
        let encoded = ready.unlocked.encode(ready.initial_session)?;
        let prepared = ready.doc.prepare(ready.journal, &encoded)?;
        let published = ready
            .doc
            .publish(prepared, ready.source.as_ref(), &current)?;
        self.remember(path, epoch, unlocked.clone())?;
        ensure!(
            published == Published::Durable,
            "credential_commit_durability_unknown"
        );
        let stored: Stored =
            serde_json::from_slice(ready.doc.read()?.ok_or(Error::Missing)?.bytes())?;
        unlocked.decode(stored)
    }
}
struct Ready {
    doc: Document,
    source: Option<Snapshot>,
    journal: vault::document::Journal,
    unlocked: Arc<Unlocked>,
    initial_session: FullSessionPersisted,
}
fn identity(session: &FullSessionPersisted) -> Identity {
    identity_from(
        &session.user_session,
        &session.client_session.homeserver,
        &session.client_session.db_path,
    )
}
fn identity_from(user: &PersistedUserSession, homeserver: &str, db_path: &Path) -> Identity {
    Identity {
        owner: user.meta().user_id.clone(),
        device: user.meta().device_id.to_string(),
        homeserver: homeserver.into(),
        oauth_client_id: match user {
            PersistedUserSession::OAuth {
                oauth_client_id, ..
            } => Some(oauth_client_id.clone()),
            _ => None,
        },
        db_path: db_path.into(),
    }
}

pub(crate) async fn load(path: PathBuf) -> Result<FullSessionPersisted> {
    load_on(global()?.clone(), path).await
}
pub(crate) fn public_identity(bytes: &[u8]) -> Result<Identity> {
    ensure!(bytes.len() <= MAX_SESSION, "credential_document_too_large");
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    if value.get("version").is_some() {
        let stored: Stored = serde_json::from_value(value)?;
        ensure!(stored.version == VERSION, "credential_schema_unsupported");
        Ok(stored.identity)
    } else {
        Ok(identity(&serde_json::from_value(value)?))
    }
}
fn target_cell(
    target: Option<revoke_transport::Target>,
) -> std::sync::OnceLock<revoke_transport::Target> {
    let cell = std::sync::OnceLock::new();
    if let Some(target) = target {
        let _ = cell.set(target);
    }
    cell
}
fn classify(error: anyhow::Error) -> Error {
    error
        .downcast_ref::<Error>()
        .copied()
        .unwrap_or(Error::Storage)
}

pub(crate) fn retire() {
    if let Some(Ok(service)) = SERVICE.get() {
        if let Ok(mut cache) = service.cache.lock() {
            if let Ok(mut retired) = service.retired.lock() {
                retired.extend(std::mem::take(&mut *cache).into_values());
            }
        }
    }
}
pub(crate) fn shutdown() -> bool {
    retire();
    let Some(Ok(service)) = SERVICE.get() else {
        return SERVICE.get().is_none();
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let original = service.clone();
    let flushed = service
        .worker
        .call_sync(std::time::Duration::from_secs(5), move || {
            original.cleanup_retired();
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Storage)?;
            flush_retired_snapshots(true).map_err(classify)
        })
        .is_ok();
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    let joined = service.worker.shutdown(remaining);
    flushed && joined
}

pub(crate) async fn save(
    client: &matrix_sdk::Client,
    session: FullSessionPersisted,
    epoch: u64,
    path: PathBuf,
) -> Result<()> {
    save_with(global()?.clone(), client, session, epoch, path).await
}
async fn save_with(
    service: Arc<Service>,
    client: &matrix_sdk::Client,
    session: FullSessionPersisted,
    epoch: u64,
    path: PathBuf,
) -> Result<()> {
    let owner = match owners::find(client)? {
        Some(owner) => {
            if owner.state.load(Ordering::Acquire) != owners::PREPARING {
                return Err(Error::Conflict.into());
            }
            owner
        }
        None => install_owner(
            service.clone(),
            client,
            session.client_session.clone(),
            None,
        )?,
    };
    owner.bind_metadata(session.client_session.clone())?;
    owner.capture(client)?;
    let preparation = owners::Preparing::new(&owner)?;
    let _observation = owners::Observation::new(&owner);
    let target = revoke_transport::capture_target(client).await?;
    owner.set_target(target)?;
    let original = service.clone();
    let target = path.clone();
    let prepared_owner = owner.clone();
    let ready = service
        .worker
        .call(move || {
            let _preparation = preparation;
            if crate::account_session::epoch() != epoch {
                return Err(Error::Retired);
            }
            let ready = original
                .prepare(
                    &target,
                    session,
                    prepared_owner.fresh.get().map(Arc::as_ref),
                )
                .map_err(classify)?;
            prepared_owner
                .set_keys(ready.unlocked.clone())
                .map_err(classify)?;
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
            let retired = crate::account_session::epoch() != epoch
                || !prepared_owner.observing.load(Ordering::Acquire);
            if retired {
                prepared_owner
                    .state
                    .store(owners::REVOKING, Ordering::Release);
            }
            prepared_owner.flush().map_err(classify)?;
            if retired {
                discard_retired_preparation(&ready, &prepared_owner).map_err(classify)?;
                return Err(Error::Retired);
            }
            Ok(ready)
        })
        .await?;
    let original = service.clone();
    let original_client = client.clone();
    let accepted_owner = owner.clone();
    service
        .worker
        .call(move || {
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Storage)?;
            if crate::account_session::epoch() != epoch
                || !accepted_owner.observing.load(Ordering::Acquire)
            {
                accepted_owner
                    .state
                    .store(owners::REVOKING, Ordering::Release);
                accepted_owner.flush().map_err(classify)?;
                discard_retired_preparation(&ready, &accepted_owner).map_err(classify)?;
                return Err(Error::Retired);
            }
            let revision = accepted_owner.capture(&original_client).map_err(classify)?;
            let latest = snapshot_sdk(&original_client, &ready.unlocked).map_err(classify)?;
            let encoded = ready.unlocked.encode(latest).map_err(classify)?;
            let prepared = ready.doc.prepare(ready.journal, &encoded)?;
            let published = ready.doc.publish(prepared, ready.source.as_ref(), || {
                crate::account_session::epoch() == epoch
                    && accepted_owner.observing.load(Ordering::Acquire)
            })?;
            original
                .remember(&path, epoch, ready.unlocked)
                .map_err(classify)?;
            original
                .bind_owner(&path, &accepted_owner)
                .map_err(classify)?;
            accepted_owner
                .state
                .store(owners::PUBLISHED, Ordering::Release);
            if published != Published::Durable {
                return Err(Error::DurabilityUnknown);
            }
            accepted_owner.clear(revision).map_err(classify)?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn discard_retired_preparation(ready: &Ready, owner: &Arc<owners::Owner>) -> Result<()> {
    ensure!(
        recovery::revoke_pending(&ready.unlocked)?,
        "credential_revocation_not_durable"
    );
    let refs = vec![
        ready.unlocked.key.reference().clone(),
        ready.unlocked.database.reference.clone(),
        ready.unlocked.revocation.reference().clone(),
    ];
    ready
        .doc
        .discard_unpublished(&ready.journal, ready.source.as_ref(), &refs, |bytes| {
            ready
                .unlocked
                .decode(serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?)
                .map(|_| ())
                .map_err(classify)
        })?;
    owner.cleanup_pending.store(false, Ordering::Release);
    owners::remove_if_clean(owner)?;
    Ok(())
}
fn install_owner(
    service: Arc<Service>,
    client: &matrix_sdk::Client,
    metadata: ClientSessionPersisted,
    keys: Option<Arc<Unlocked>>,
) -> Result<Arc<owners::Owner>> {
    let owner = owners::reserve(service, client, metadata)?;
    install_callbacks(client, owner, keys)
}
fn install_callbacks(
    client: &matrix_sdk::Client,
    owner: Arc<owners::Owner>,
    keys: Option<Arc<Unlocked>>,
) -> Result<Arc<owners::Owner>> {
    if let Some(keys) = keys {
        owner.set_keys(keys)?;
    }
    let life = Arc::new(());
    owner.bind_callback_life(&life)?;
    let read_life = life.clone();
    let read_owner = owner.clone();
    let write_owner = owner.clone();
    client.set_session_callbacks(
        Box::new(move |client| {
            let _original_family = &read_life;
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
            let keys = read_owner.keys.get().ok_or(Error::Unavailable)?;
            let path = read_owner.path()?;
            if read_owner.epoch != crate::account_session::epoch()
                || keys.logout.load(Ordering::Acquire)
                || !(read_owner.state.load(Ordering::Acquire) == owners::ACTIVATING
                    || read_owner.state.load(Ordering::Acquire) == owners::PUBLISHED
                        && read_owner.service.owns_owner(&path, &read_owner))
            {
                return Err(Error::Retired.into());
            }
            if let Some(update) = read_owner.update()? {
                return Ok(match update.user {
                    PersistedUserSession::OAuth { user, .. } => user.tokens,
                    PersistedUserSession::Matrix(value) => value.tokens,
                });
            }
            let stored = read_callback(keys, &path)?;
            if !super::matrix_state::same_stored_session(&client, &stored) {
                return Err(Error::Integrity.into());
            }
            Ok(match stored.user_session {
                PersistedUserSession::OAuth { user, .. } => user.tokens,
                PersistedUserSession::Matrix(value) => value.tokens,
            })
        }),
        Box::new(move |client| {
            let _original_family = &life;
            // Capture before looking for keys or waiting on slow publication IO.
            // This cell's finite capacity was reserved before SDK activation.
            write_owner.capture(&client)?;
            if write_owner.keys.get().is_none() {
                return Err(Error::Unavailable.into());
            }
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
            write_owner.flush().map_err(|_| Error::Storage)?;
            Ok(())
        }),
    )?;
    Ok(owner)
}
pub(crate) struct FreshDatabase {
    path: PathBuf,
    directory: std::fs::File,
}
impl FreshDatabase {
    fn validate(&self, root: &Path, metadata: &ClientSessionPersisted) -> Result<()> {
        use cap_fs_ext::DirExt;
        let actual = if metadata.db_path.is_absolute() {
            metadata.db_path.clone()
        } else {
            root.join(&metadata.db_path)
        };
        ensure!(actual == self.path, "credential_database_receipt_mismatch");
        let parent = cap_std::fs::Dir::open_ambient_dir(
            actual.parent().ok_or(Error::Invalid)?,
            cap_std::ambient_authority(),
        )?;
        let current = parent
            .open_dir_nofollow(actual.file_name().ok_or(Error::Invalid)?)?
            .into_std_file();
        ensure!(
            hagency_platform::same_directory(&self.directory, &current)?,
            "credential_database_receipt_mismatch"
        );
        Ok(())
    }
}
pub(crate) async fn create_fresh_database(path: PathBuf) -> Result<FreshDatabase> {
    global()?
        .worker
        .call(move || {
            use cap_fs_ext::DirExt;
            hagency_store::private::create_directory_new(&path).map_err(|_| Error::Storage)?;
            let parent = cap_std::fs::Dir::open_ambient_dir(
                path.parent().ok_or(Error::Invalid)?,
                cap_std::ambient_authority(),
            )
            .map_err(|_| Error::Storage)?;
            let directory = parent
                .open_dir_nofollow(path.file_name().ok_or(Error::Invalid)?)
                .map_err(|_| Error::Storage)?
                .into_std_file();
            Ok(FreshDatabase { path, directory })
        })
        .await
        .map_err(Into::into)
}
async fn preflight(owner: &Arc<owners::Owner>) -> Result<()> {
    let service = owner.service.clone();
    let original = owner.clone();
    service
        .worker
        .call(move || {
            if original.epoch != crate::account_session::epoch() {
                return Err(Error::Retired);
            }
            let installation = original.service.installation().map_err(classify)?;
            let _lock = Document::open(
                &original
                    .service
                    .root
                    .join("credentials-v1/native-readiness"),
                4096,
            )?;
            original
                .service
                .vault
                .check_native_readiness(&installation)?;
            if original.epoch != crate::account_session::epoch() {
                return Err(Error::Retired);
            }
            Ok(())
        })
        .await?;
    service.start_maintenance()
}
pub(crate) async fn register_fresh_client(
    client: &matrix_sdk::Client,
    metadata: ClientSessionPersisted,
    fresh: FreshDatabase,
) -> Result<()> {
    let owner = install_owner(global()?.clone(), client, metadata, None)?;
    // The original directory exists before the SDK may create any database.
    owner
        .fresh
        .set(Arc::new(fresh))
        .map_err(|_| Error::Conflict)?;
    preflight(&owner).await
}
pub(crate) async fn register_client(
    client: &matrix_sdk::Client,
    metadata: ClientSessionPersisted,
) -> Result<()> {
    let owner = install_owner(global()?.clone(), client, metadata, None)?;
    preflight(&owner).await
}
/// This client is memory-only and has no automatic refresh. Reserve both the
/// candidate and final durable SDK owner before any PKCE token can be issued.
pub(crate) async fn register_preauth(
    client: &matrix_sdk::Client,
    metadata: ClientSessionPersisted,
) -> Result<()> {
    let owner = install_owner(global()?.clone(), client, metadata, None)?;
    owner.reserve_handoff()?;
    preflight(&owner).await
}
pub(crate) fn needs_handoff(client: &matrix_sdk::Client) -> Result<bool> {
    Ok(owners::find(client)?.is_some_and(|owner| owner.has_handoff()))
}
struct SdkObservation(Arc<AtomicBool>);
impl Drop for SdkObservation {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

pub(crate) async fn handoff_client(candidate: &matrix_sdk::Client) -> Result<matrix_sdk::Client> {
    handoff_client_with(candidate, None).await
}
async fn handoff_client_with(
    candidate: &matrix_sdk::Client,
    configure: Option<fn(matrix_sdk::ClientBuilder) -> matrix_sdk::ClientBuilder>,
) -> Result<matrix_sdk::Client> {
    let owner = owners::find(candidate)?.ok_or(Error::Missing)?;
    ensure!(
        owner.epoch == crate::account_session::epoch()
            && owner.state.load(Ordering::Acquire) == owners::PUBLISHED,
        "credential_operation_retired"
    );
    let keys = owner.keys.get().ok_or(Error::Missing)?.clone();
    let permit = owner.take_handoff()?;
    let observed = Arc::new(AtomicBool::new(true));
    let _observation = SdkObservation(observed.clone());
    let service = owner.service.clone();
    let original = candidate.clone();
    let runtime = tokio::runtime::Handle::current();
    service
        .worker
        .call(move || {
            // Original candidate, its grant lease, secret and reservation remain in
            // this original worker until build+activation actually finish. A dropped
            // oneshot observer never drops an in-progress SDK/SQLite future.
            if owner.epoch != crate::account_session::epoch() || !observed.load(Ordering::Acquire) {
                return Err(Error::Retired);
            }
            runtime.block_on(async move {
                let actual =
                    retained::resolve_database_path(&owner.service.root, &keys.database.db_path)
                        .map_err(classify)?;
                match std::fs::symlink_metadata(&actual) {
                    Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {
                        retained::verify_retained_identity_on_worker(
                            &owner.service.root,
                            &keys.database,
                            &keys.identity,
                            &keys.secret,
                        )
                        .map_err(classify)?;
                    }
                    Ok(_) => return Err(Error::Invalid),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        hagency_store::private::create_directory_new(&actual)
                            .map_err(|_| Error::Storage)?
                    }
                    Err(_) => return Err(Error::Storage),
                }
                let builder = crate::sliding_sync::base_client_builder(&actual, &keys.secret)
                    .homeserver_url(&keys.database.homeserver);
                let builder = if let Some(configure) = configure {
                    configure(builder)
                } else {
                    builder
                };
                let client = builder.build().await.map_err(|_| Error::Storage)?;
                if owner.epoch != crate::account_session::epoch()
                    || !observed.load(Ordering::Acquire)
                {
                    let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
                    owner.state.store(owners::PREPARING, Ordering::Release);
                    owner.capture(&original).map_err(classify)?;
                    owner.flush().map_err(classify)?;
                    return Err(Error::Retired);
                }
                let metadata = ClientSessionPersisted {
                    homeserver: keys.database.homeserver.clone(),
                    db_path: keys.database.db_path.clone(),
                    passphrase: keys.secret.to_string(),
                };
                let successor =
                    owners::reserve_admitted(owner.service.clone(), &client, metadata, permit)
                        .map_err(classify)?;
                install_callbacks(&client, successor.clone(), Some(keys.clone()))
                    .map_err(classify)?;
                successor.state.store(owners::ACTIVATING, Ordering::Release);
                // Never hold SESSION_IO while the SDK can invoke its sync callbacks.
                let session = original.session().ok_or(Error::Invalid)?;
                client
                    .restore_session(session)
                    .await
                    .map_err(|_| Error::Storage)?;
                let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
                successor.capture(&client).map_err(classify)?;
                if owner.epoch != crate::account_session::epoch()
                    || !observed.load(Ordering::Acquire)
                {
                    successor.state.store(owners::PREPARING, Ordering::Release);
                    successor.flush().map_err(classify)?; // original grant Keep only
                    return Err(Error::Retired);
                }
                owner
                    .service
                    .bind_owner(&owner.path().map_err(classify)?, &successor)
                    .map_err(classify)?;
                successor.state.store(owners::PUBLISHED, Ordering::Release);
                if let Err(error) = successor.flush() {
                    successor.state.store(owners::REVOKING, Ordering::Release);
                    keys.logout.store(true, Ordering::Release);
                    let _ = successor.flush(); // failure keeps the actual admitted snapshot
                    return Err(classify(error));
                }
                Ok(client)
            })
        })
        .await
        .map_err(Into::into)
}
pub(crate) fn install_cached(client: &matrix_sdk::Client, path: PathBuf) -> Result<()> {
    let service = global()?.clone();
    let epoch = crate::account_session::epoch();
    let keys = service.cached(&path, epoch)?;
    let metadata = ClientSessionPersisted {
        homeserver: keys.database.homeserver.clone(),
        db_path: keys.database.db_path.clone(),
        passphrase: keys.secret.to_string(),
    };
    let owner = if let Some(owner) = owners::find(client)? {
        owner
    } else {
        install_owner(service.clone(), client, metadata, Some(keys))?
    };
    owner.state.store(owners::PUBLISHED, Ordering::Release);
    service.bind_owner(&path, &owner)?;
    Ok(())
}
fn read_callback(unlocked: &Unlocked, path: &Path) -> Result<FullSessionPersisted> {
    let doc = Document::open(path, MAX_SESSION)?;
    let source = doc.read()?.ok_or(Error::Missing)?;
    let stored: Stored = serde_json::from_slice(source.bytes())?;
    let authenticated: Authentication = unlocked.key.open(&stored.authentication)?;
    let session = unlocked.decode(stored)?;
    recovery::reload_keep(unlocked, authenticated.revision, session)
}
fn persist_owner_snapshot(
    path: &Path,
    unlocked: &Unlocked,
    session: FullSessionPersisted,
    owner: &Arc<owners::Owner>,
) -> Result<()> {
    let doc = Document::open(path, MAX_SESSION)?;
    if let Some(journal) = doc.pending(unlocked.key.reference().scope())? {
        let source = doc.read()?;
        if source.as_ref().map(Snapshot::digest) == journal.candidate_digest()
            && journal.candidate_digest().is_some()
        {
            unlocked.decode(serde_json::from_slice(
                source.as_ref().ok_or(Error::Missing)?.bytes(),
            )?)?;
            doc.finish_authenticated(
                &journal,
                &[
                    unlocked.key.reference().clone(),
                    unlocked.database.reference.clone(),
                    unlocked.revocation.reference().clone(),
                ],
            )?;
        } else if journal.candidate_digest().is_some() {
            match doc.resume_candidate(&journal) {
                Ok((prepared, candidate)) => {
                    unlocked.decode(serde_json::from_slice(&candidate)?)?;
                    if doc.publish(prepared, source.as_ref(), || {
                        owner.epoch == crate::account_session::epoch()
                            && owner.service.owns_owner(path, owner)
                    })? != Published::Durable
                    {
                        return Err(Error::DurabilityUnknown.into());
                    }
                }
                Err(Error::Missing) => {} // Original latest SDK snapshot below is still owned.
                Err(error) => return Err(error.into()),
            }
        }
    }
    let source = doc.read()?.ok_or(Error::Missing)?;
    unlocked.decode(serde_json::from_slice(source.bytes())?)?;
    let encoded = unlocked.encode(session)?;
    let refs = vec![
        unlocked.key.reference().clone(),
        unlocked.database.reference.clone(),
        unlocked.revocation.reference().clone(),
    ];
    let journal = doc.begin(unlocked.key.reference().scope(), Some(&source), refs)?;
    let prepared = doc.prepare(journal, &encoded)?;
    if doc.publish(prepared, Some(&source), || {
        owner.epoch == crate::account_session::epoch() && owner.service.owns_owner(path, owner)
    })? != Published::Durable
    {
        return Err(Error::DurabilityUnknown.into());
    }
    Ok(())
}
pub(crate) fn has_owner(client: &matrix_sdk::Client) -> bool {
    owners::find(client).ok().flatten().is_some()
}
/// A failed/unobserved candidate can never become canonical authority again.
/// Native preparation already in progress keeps its original ownership; this
/// path only uses ready RAM keys or the admitted raw revoke-only snapshot.
pub(crate) async fn retire_candidate(client: &matrix_sdk::Client) -> Result<()> {
    let owner = owners::find(client)?.ok_or(Error::Missing)?;
    owner.capture(client)?;
    owner.state.store(owners::REVOKING, Ordering::Release);
    if let Some(keys) = owner.keys.get() {
        keys.logout.store(true, Ordering::Release);
    }
    if owner.target().is_none() {
        let target = revoke_transport::capture_target(client).await?;
        owner.set_target(target)?;
    }
    owner.state.store(owners::REVOKING, Ordering::Release);
    let service = owner.service.clone();
    let original = service.clone();
    let runtime = tokio::runtime::Handle::current();
    service
        .worker
        .call(move || {
            if owner.keys.get().is_some() {
                {
                    let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
                    recovery::retire_preparation(&owner).map_err(classify)?;
                }
                recovery::retry_path(
                    &original,
                    &runtime,
                    recovery::path(
                        owner.keys.get().ok_or(Error::Unavailable)?,
                        recovery::Intent::Revoke,
                    )
                    .map_err(classify)?,
                )
                .map_err(classify)?;
            } else {
                recovery::retry_ram_one(&original, &runtime).map_err(classify)?;
            }
            Ok(())
        })
        .await?;
    Ok(())
}
/// Actual original SDK owner and token pair are committed to a revoke-only
/// tombstone before remote revocation or local authority deletion can begin.
pub(crate) async fn begin_logout(client: &matrix_sdk::Client) -> Result<PathBuf> {
    let owner = owners::find(client)?.ok_or(Error::Missing)?;
    let epoch = crate::account_session::epoch();
    ensure!(owner.epoch == epoch, "credential_operation_retired");
    owner.capture(client)?;
    let keys = owner.keys.get().ok_or(Error::Unavailable)?.clone();
    if keys.target.get().is_none() {
        let target = revoke_transport::capture_target(client).await?;
        owner.set_target(target.clone())?;
        let _ = keys.target.set(target);
    }
    let service = owner.service.clone();
    let original = client.clone();
    let pending = service
        .worker
        .call(move || {
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
            let path = owner.path().map_err(classify)?;
            if owner.epoch != crate::account_session::epoch()
                || !owner.service.owns_owner(&path, &owner)
                || !crate::matrix_context::is_current(&original)
            {
                return Err(Error::Retired);
            }
            owner.state.store(owners::REVOKING, Ordering::Release);
            keys.logout.store(true, Ordering::Release);
            // A callback concurrent with logout shares this lock. Any later issued
            // pair will write the same original grant's revoke-only record.
            owner.capture(&original).map_err(classify)?;
            owner.flush().map_err(classify)?;
            recovery::path(&keys, recovery::Intent::Revoke).map_err(classify)
        })
        .await?;
    service.start_maintenance()?;
    Ok(pending)
}
pub(crate) async fn retry_revocation(path: PathBuf) -> Result<bool> {
    let service = global()?.clone();
    let runtime = tokio::runtime::Handle::current();
    let original = service.clone();
    service
        .worker
        .call(move || recovery::retry_path(&original, &runtime, path).map_err(classify))
        .await
        .map_err(Into::into)
}
pub(crate) async fn capture_restored_target(client: &matrix_sdk::Client) -> Result<()> {
    let owner = owners::find(client)?.ok_or(Error::Missing)?;
    let target = revoke_transport::capture_target(client).await?;
    owner.set_target(target.clone())?;
    let keys = owner.keys.get().ok_or(Error::Missing)?;
    let _ = keys.target.set(target);
    owner.capture(client)?;
    let service = owner.service.clone();
    service
        .worker
        .call(move || {
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
            owner.flush().map_err(classify)
        })
        .await?;
    service.start_maintenance()
}
pub(crate) fn refresh_failed(client: &matrix_sdk::Client, epoch: u64) -> bool {
    owners::find(client).ok().flatten().is_some_and(|owner| {
        owner.epoch == epoch
            && owner.state.load(Ordering::Acquire) == owners::PUBLISHED
            && owner.dirty()
    })
}
pub(crate) async fn retry_latest(client: &matrix_sdk::Client, epoch: u64) -> Result<()> {
    let original = client.downgrade();
    global()?
        .worker
        .call(move || {
            if crate::account_session::epoch() != epoch
                || original
                    .get()
                    .is_none_or(|client| !crate::matrix_context::is_current(&client))
            {
                return Err(Error::Retired);
            }
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
            for owner in owners::pending().map_err(classify)? {
                if owner.keys.get().is_some() {
                    owner.flush().map_err(classify)?;
                }
            }
            Ok(())
        })
        .await?;
    Ok(())
}

impl Service {
    fn check_account_path(&self, path: &Path, owner: Option<&OwnedUserId>) -> Result<String> {
        let state = path.parent().ok_or(Error::Invalid)?;
        let account = state.parent().ok_or(Error::Invalid)?;
        let account_hash = account
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(Error::Invalid)?;
        ensure!(
            state.file_name().and_then(|n| n.to_str()) == Some("persistent_state")
                && account.parent() == Some(self.root.as_path())
                && account_hash.starts_with("account_")
                && account_hash.len() == 72
                && account_hash[8..].bytes().all(|b| b.is_ascii_hexdigit()),
            "credential_account_path_invalid"
        );
        if let Some(owner) = owner {
            ensure!(
                crate::accounts::account_dir(&self.root, owner) == account,
                "credential_account_path_invalid"
            );
        }
        hagency_store::private::directory(account)?;
        hagency_store::private::directory(state)?;
        Ok(account_hash.into())
    }
    fn cleanup_retired(&self) {
        let retired = self
            .retired
            .lock()
            .map(|mut values| std::mem::take(&mut *values))
            .unwrap_or_default();
        drop(retired);
    }
}

async fn remove_authentication_with(
    service: Arc<Service>,
    path: PathBuf,
    expected: Snapshot,
) -> Result<()> {
    let epoch = crate::account_session::epoch();
    service
        .worker
        .call(move || {
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Storage)?;
            if crate::account_session::epoch() != epoch {
                return Err(Error::Retired);
            }
            let doc = Document::open(&path, MAX_SESSION)?;
            doc.check_original(Some(&expected))?;
            // Original retained DB refs were committed before removing authority.
            // Native key GC is separate and never inferred from a missing session.
            if doc.remove_original(&expected, || crate::account_session::epoch() == epoch)?
                != Published::Durable
            {
                return Err(Error::DurabilityUnknown);
            }
            Ok(())
        })
        .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
pub(super) async fn load_on(service: Arc<Service>, path: PathBuf) -> Result<FullSessionPersisted> {
    let original = service.clone();
    let epoch = crate::account_session::epoch();
    service
        .worker
        .call(move || {
            original
                .load_worker(&path, epoch, || crate::account_session::epoch() == epoch)
                .map_err(classify)
        })
        .await
        .map_err(Into::into)
}
#[cfg(test)]
pub(crate) async fn test_save(
    service: Arc<Service>,
    client: &matrix_sdk::Client,
    session: FullSessionPersisted,
    path: PathBuf,
) -> Result<()> {
    save_with(
        service,
        client,
        session,
        crate::account_session::epoch(),
        path,
    )
    .await
}

pub(crate) async fn remove_account(path: PathBuf, owner: OwnedUserId) -> Result<()> {
    remove_account_on(global()?.clone(), path, owner).await
}
pub(crate) async fn remove_account_on(
    service: Arc<Service>,
    path: PathBuf,
    owner: OwnedUserId,
) -> Result<()> {
    let inspected = service.clone();
    let inspect_path = path.clone();
    let epoch = crate::account_session::epoch();
    let (database, source) = service
        .worker
        .call(move || {
            if crate::account_session::epoch() != epoch {
                return Err(Error::Retired);
            }
            inspected
                .inspect_database_source(&inspect_path)
                .map_err(classify)
        })
        .await?;
    ensure!(
        database.identity.owner == owner,
        "credential_identity_changed"
    );
    let target = path.with_file_name("retained_db.json");
    let original = service.clone();
    let epoch = crate::account_session::epoch();
    service
        .worker
        .call(move || {
            original
                .retain_worker(&target, Some(owner), Some(database.client_session), epoch)
                .map(|_| ())
                .map_err(classify)
        })
        .await?;
    remove_authentication_with(service, path, source).await
}
#[cfg(test)]
pub(crate) async fn test_load(
    service: Arc<Service>,
    path: PathBuf,
) -> Result<FullSessionPersisted> {
    load_on(service, path).await
}
#[cfg(test)]
pub(crate) fn test_shutdown(service: &Service) -> bool {
    service.worker.shutdown(std::time::Duration::from_secs(2))
}

fn database_scope(installation: &str, identity: &Identity) -> Result<Scope> {
    // Database roots outlive OAuth registrations and authentication grants.
    Ok(Scope::new(
        installation,
        Purpose::Database,
        &serde_json::to_vec(&(
            "matrix-database-v1",
            &identity.owner,
            &identity.device,
            &identity.homeserver,
            &identity.db_path,
        ))?,
    )?)
}

fn snapshot_sdk(client: &matrix_sdk::Client, unlocked: &Unlocked) -> Result<FullSessionPersisted> {
    let user_session = PersistedUserSession::from_sdk(
        client
            .session()
            .ok_or_else(|| anyhow!("credential_sdk_session_missing"))?,
    )?;
    ensure!(
        identity_from(
            &user_session,
            &unlocked.database.homeserver,
            &unlocked.database.db_path
        ) == unlocked.identity,
        "credential_identity_changed"
    );
    Ok(FullSessionPersisted {
        client_session: ClientSessionPersisted {
            homeserver: unlocked.database.homeserver.clone(),
            db_path: unlocked.database.db_path.clone(),
            passphrase: unlocked.secret.to_string(),
        },
        user_session,
        sync_token: unlocked.sync_token.clone(),
        sliding_sync_version: unlocked.sliding_sync_version.clone(),
    })
}
fn sdk_lease_live(unlocked: &Unlocked) -> bool {
    owners::live_grant(unlocked)
}

/// Attach the process lease before SDK activation can spawn original work.
/// A restore-for-reauthentication client also retains it through this registry,
/// even though it reserves its once-only callbacks for the newly issued grant.
pub(crate) async fn claim_restoration(client: &matrix_sdk::Client, path: &Path) -> Result<()> {
    claim_restoration_on(global()?.clone(), client, path.to_owned()).await
}
async fn claim_restoration_on(
    service: Arc<Service>,
    client: &matrix_sdk::Client,
    path: PathBuf,
) -> Result<()> {
    let original = service.clone();
    let client = client.clone();
    let epoch = crate::account_session::epoch();
    service
        .worker
        .call(move || {
            let _guard = super::SESSION_IO.lock().map_err(|_| Error::Unavailable)?;
            if crate::account_session::epoch() != epoch || client.session().is_some() {
                return Err(Error::Retired);
            }
            let keys = original.cached(&path, epoch).map_err(classify)?;
            if keys.logout.load(Ordering::Acquire) || owners::live_grant(&keys) {
                return Err(Error::OriginalBusy);
            }
            let metadata = ClientSessionPersisted {
                homeserver: keys.database.homeserver.clone(),
                db_path: keys.database.db_path.clone(),
                passphrase: keys.secret.to_string(),
            };
            let owner =
                install_owner(original.clone(), &client, metadata, Some(keys)).map_err(classify)?;
            owner.state.store(owners::PUBLISHED, Ordering::Release);
            original.bind_owner(&path, &owner).map_err(classify)?;
            Ok(())
        })
        .await?;
    Ok(())
}

fn flush_retired_snapshots(force_all: bool) -> Result<()> {
    for owner in owners::pending()? {
        if force_all
            || owner.epoch != crate::account_session::epoch()
            || owner.state.load(Ordering::Acquire) == owners::REVOKING
            || !owner.callbacks_alive()
        {
            if owner.keys.get().is_none() {
                return Err(Error::DurabilityUnknown.into());
            }
            owner.flush()?;
        }
    }
    Ok(())
}

impl Service {
    fn inspect_database(&self, path: &Path) -> Result<ExistingDatabase> {
        self.inspect_database_source(path)
            .map(|(database, _)| database)
    }
    fn inspect_database_source(&self, path: &Path) -> Result<(ExistingDatabase, Snapshot)> {
        self.cleanup_retired();
        let doc = Document::open(path, MAX_SESSION)?;
        let source = doc.read()?.ok_or(Error::Missing)?;
        let value: serde_json::Value = serde_json::from_slice(source.bytes())?;
        if value.get("version").is_some() {
            let stored: Stored = serde_json::from_value(value)?;
            self.check_account_path(path, Some(&stored.identity.owner))?;
            let original = self.read_original(&stored)?;
            Ok((
                ExistingDatabase {
                    identity: stored.identity,
                    client_session: ClientSessionPersisted {
                        homeserver: stored.database.homeserver,
                        db_path: stored.database.db_path,
                        passphrase: original.secret.to_string(),
                    },
                },
                source,
            ))
        } else {
            let session: FullSessionPersisted = serde_json::from_value(value)?;
            let identity = identity(&session);
            self.check_account_path(path, Some(&identity.owner))?;
            Ok((
                ExistingDatabase {
                    identity,
                    client_session: session.client_session,
                },
                source,
            ))
        }
    }
}
pub(crate) async fn existing_database(path: PathBuf) -> Result<ExistingDatabase> {
    let service = global()?.clone();
    let original = service.clone();
    let epoch = crate::account_session::epoch();
    service
        .worker
        .call(move || {
            if crate::account_session::epoch() != epoch {
                return Err(Error::Retired);
            }
            original.inspect_database(&path).map_err(classify)
        })
        .await
        .map_err(Into::into)
}

#[cfg(test)]
pub(crate) async fn test_revoke_snapshot(
    service: Arc<Service>,
    path: PathBuf,
) -> Result<PersistedUserSession> {
    let original = service.clone();
    service
        .worker
        .call(move || recovery::test_revoke_snapshot(&original, path).map_err(classify))
        .await
        .map_err(Into::into)
}
#[cfg(test)]
pub(crate) async fn test_retry_revocation(service: Arc<Service>, path: PathBuf) -> Result<bool> {
    let original = service.clone();
    let runtime = tokio::runtime::Handle::current();
    service
        .worker
        .call(move || recovery::retry_path(&original, &runtime, path).map_err(classify))
        .await
        .map_err(Into::into)
}
