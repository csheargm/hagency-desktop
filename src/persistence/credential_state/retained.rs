//! Retained database-only references and complete GC inventory.
#[cfg(test)]
#[path = "retained_tests.rs"]
mod retained_tests;
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum RetainedIdentity {
    Session {
        identity: Identity,
    },
    LegacyDatabase {
        account_hash: String,
        homeserver: String,
        db_path: PathBuf,
    },
}
impl RetainedIdentity {
    pub(super) fn scope(&self, installation: &str) -> Result<Scope> {
        match self {
            Self::Session { identity } => database_scope(installation, identity),
            Self::LegacyDatabase { .. } => Ok(Scope::new(
                installation,
                Purpose::Database,
                &serde_json::to_vec(self)?,
            )?),
        }
    }
    fn validate(&self, root: &Path, account_hash: &str, database: &Database) -> Result<()> {
        match self {
            Self::Session { identity } => {
                ensure!(
                    crate::accounts::account_dir(root, &identity.owner)
                        .file_name()
                        .and_then(|n| n.to_str())
                        == Some(account_hash)
                        && identity.homeserver == database.homeserver
                        && identity.db_path == database.db_path
                        && database.legacy_account.is_none(),
                    "credential_retained_identity_invalid"
                );
            }
            Self::LegacyDatabase {
                account_hash: original,
                homeserver,
                db_path,
            } => {
                ensure!(
                    original == account_hash
                        && homeserver == &database.homeserver
                        && db_path == &database.db_path
                        && database
                            .legacy_account
                            .as_ref()
                            .is_none_or(|a| a == account_hash),
                    "credential_retained_identity_invalid"
                );
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RetainedEntry {
    pub(super) identity: RetainedIdentity,
    pub(super) database: Database,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Retained {
    pub(super) version: u32,
    pub(super) account_hash: String,
    pub(super) databases: Vec<RetainedEntry>,
}
impl Service {
    fn retained_entries(
        &self,
        bytes: &[u8],
        account_hash: &str,
        installation: &str,
    ) -> Result<Vec<RetainedEntry>> {
        let mut retained: Retained = serde_json::from_slice(bytes)?;
        ensure!(
            retained.version == VERSION
                && retained.account_hash == account_hash
                && retained.databases.len() <= 128,
            "credential_retained_schema_invalid"
        );
        let mut paths = std::collections::HashSet::new();
        for entry in &mut retained.databases {
            ensure!(
                paths.insert(entry.database.db_path.clone()),
                "credential_retained_database_ambiguous"
            );
            entry
                .identity
                .validate(&self.root, account_hash, &entry.database)?;
            let secret = self.vault.load(
                &entry.database.reference,
                &entry.identity.scope(installation)?,
            )?;
            ensure!(!secret.is_empty(), "credential_original_secret_missing");
            if let RetainedIdentity::LegacyDatabase { account_hash, .. } = &entry.identity {
                entry.database.legacy_account = Some(account_hash.clone());
            }
        }
        Ok(retained.databases)
    }
    fn original_entry(
        &self,
        old: &ClientSessionPersisted,
        hash: &str,
        session_held: bool,
    ) -> Result<Option<RetainedEntry>> {
        if let Some(cached) = self
            .cache
            .lock()
            .map_err(|_| Error::Storage)?
            .values()
            .find(|c| {
                account_hash(&self.root, &c.value.identity.owner).is_ok_and(|h| h == hash)
                    && c.value.database.db_path == old.db_path
                    && same_homeserver(&c.value.database.homeserver, &old.homeserver)
                        .unwrap_or(false)
                    && c.value.secret.as_str() == old.passphrase
            })
            .map(|c| c.value.clone())
        {
            let identity = if let Some(account_hash) = &cached.database.legacy_account {
                RetainedIdentity::LegacyDatabase {
                    account_hash: account_hash.clone(),
                    homeserver: cached.database.homeserver.clone(),
                    db_path: cached.database.db_path.clone(),
                }
            } else {
                RetainedIdentity::Session {
                    identity: cached.identity.clone(),
                }
            };
            return Ok(Some(RetainedEntry {
                identity,
                database: cached.database.clone(),
            }));
        }
        // The caller is migrating this account's session document and holds
        // its lock; that document has no published original to reuse, and a
        // second open of it here would be refused as a conflict.
        if session_held {
            return Ok(None);
        }
        let path = self.root.join(hash).join("persistent_state/session");
        match std::fs::symlink_metadata(&path) {
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        }
        let doc = Document::open(&path, MAX_SESSION)?;
        let source = doc.read()?.ok_or(Error::Missing)?;
        #[derive(Deserialize)]
        struct Version {
            version: Option<u32>,
        }
        let version: Version = serde_json::from_slice(source.bytes())?;
        if version.version.is_none() {
            return Ok(None);
        }
        ensure!(
            version.version == Some(VERSION),
            "credential_schema_unsupported"
        );
        let stored: Stored = serde_json::from_slice(source.bytes())?;
        self.check_account_path(&path, Some(&stored.identity.owner))?;
        if stored.database.db_path != old.db_path {
            return Ok(None);
        }
        ensure!(
            same_homeserver(&stored.database.homeserver, &old.homeserver)?,
            "credential_database_identity_invalid"
        );
        let original = self.resolve_database(
            &stored.database,
            &stored.identity,
            &self.existing_installation()?,
        )?;
        ensure!(
            original.as_str() == old.passphrase,
            "credential_original_secret_mismatch"
        );
        let identity = if let Some(account_hash) = &stored.database.legacy_account {
            RetainedIdentity::LegacyDatabase {
                account_hash: account_hash.clone(),
                homeserver: stored.database.homeserver.clone(),
                db_path: stored.database.db_path.clone(),
            }
        } else {
            RetainedIdentity::Session {
                identity: stored.identity,
            }
        };
        Ok(Some(RetainedEntry {
            identity,
            database: stored.database,
        }))
    }
    pub(super) fn retain_worker(
        &self,
        path: &Path,
        owner: Option<OwnedUserId>,
        incoming: Option<ClientSessionPersisted>,
        epoch: u64,
    ) -> Result<Vec<PathBuf>> {
        self.retain_worker_with(path, owner, incoming, epoch, false)
    }
    /// `session_held`: the caller holds this account's session document, which
    /// is absent or still in the legacy form.
    fn retain_worker_with(
        &self,
        path: &Path,
        owner: Option<OwnedUserId>,
        incoming: Option<ClientSessionPersisted>,
        epoch: u64,
        session_held: bool,
    ) -> Result<Vec<PathBuf>> {
        ensure!(
            crate::account_session::epoch() == epoch,
            "credential_operation_retired"
        );
        let hash = self.check_account_path(path, owner.as_ref())?;
        let doc = Document::open(path, MAX_SESSION)?;
        let source = doc.read()?;
        let is_legacy = source.as_ref().is_some_and(|s| {
            s.bytes().iter().copied().find(|b| !b.is_ascii_whitespace()) == Some(b'[')
        });
        let journal_path = path.with_file_name(".retained_db.json.vault-journal");
        let has_journal = match std::fs::symlink_metadata(journal_path) {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => return Err(e.into()),
        };
        if source.is_none() && incoming.is_none() && !has_journal {
            return Ok(Vec::new());
        }
        let installation = if (source.is_some() && !is_legacy) || has_journal {
            self.existing_installation()?
        } else {
            self.installation()?
        };
        let scope = Scope::new(
            &installation,
            Purpose::Database,
            &serde_json::to_vec(&("retained-db-v2", &hash))?,
        )?;
        let mut legacy: Vec<ClientSessionPersisted> = Vec::new();
        let mut entries = Vec::new();
        if let Some(source) = &source {
            if is_legacy {
                legacy = serde_json::from_slice(source.bytes())?;
            } else {
                entries = self.retained_entries(source.bytes(), &hash, &installation)?;
            }
        }
        let pending = doc.pending(&scope)?;
        if let Some(journal) = pending.as_ref().filter(|j| j.candidate_digest().is_some()) {
            if source
                .as_ref()
                .is_some_and(|s| journal.candidate_digest() == Some(s.digest()))
            {
                let refs = entries
                    .iter()
                    .map(|e| e.database.reference.clone())
                    .collect::<Vec<_>>();
                doc.finish_authenticated(journal, &refs)?;
            } else {
                let verified = doc.begin(&scope, source.as_ref(), journal.references.clone())?;
                let (prepared, bytes) = doc.resume_candidate(&verified)?;
                let recovered = self.retained_entries(&bytes, &hash, &installation)?;
                ensure!(
                    verified.references
                        == recovered
                            .iter()
                            .map(|e| e.database.reference.clone())
                            .collect::<Vec<_>>(),
                    "credential_migration_journal_invalid"
                );
                for entry in &entries {
                    ensure!(
                        recovered.contains(entry),
                        "credential_original_reference_changed"
                    );
                }
                for old in &legacy {
                    let entry = recovered
                        .iter()
                        .find(|e| e.database.db_path == old.db_path)
                        .ok_or(Error::Integrity)?;
                    ensure!(
                        same_homeserver(&entry.database.homeserver, &old.homeserver)?,
                        "credential_database_identity_invalid"
                    );
                    ensure!(
                        self.vault
                            .load(
                                &entry.database.reference,
                                &entry.identity.scope(&installation)?
                            )?
                            .as_slice()
                            == old.passphrase.as_bytes(),
                        "credential_original_secret_mismatch"
                    );
                }
                let _gate = crate::persistence::SESSION_IO
                    .lock()
                    .map_err(|_| Error::Storage)?;
                ensure!(
                    doc.publish(prepared, source.as_ref(), || crate::account_session::epoch(
                    ) == epoch)?
                        == Published::Durable,
                    "credential_commit_durability_unknown"
                );
                drop(_gate);
                drop(doc);
                return self.retain_worker_with(path, owner, incoming, epoch, session_held);
            }
        }
        if let Some(incoming) = incoming {
            if let Some(entry) = entries
                .iter()
                .find(|e| e.database.db_path == incoming.db_path)
            {
                ensure!(
                    same_homeserver(&entry.database.homeserver, &incoming.homeserver)?,
                    "credential_database_identity_invalid"
                );
                ensure!(
                    self.vault
                        .load(
                            &entry.database.reference,
                            &entry.identity.scope(&installation)?
                        )?
                        .as_slice()
                        == incoming.passphrase.as_bytes(),
                    "credential_original_secret_mismatch"
                );
            } else if let Some(old) = legacy.iter().find(|e| e.db_path == incoming.db_path) {
                ensure!(
                    same_homeserver(&old.homeserver, &incoming.homeserver)?
                        && old.passphrase == incoming.passphrase,
                    "credential_original_secret_mismatch"
                );
            } else {
                legacy.push(incoming);
            }
        }
        ensure!(
            legacy
                .len()
                .checked_add(entries.len())
                .is_some_and(|n| n <= 128),
            "credential_retained_capacity"
        );
        if legacy.is_empty() {
            return Ok(entries.into_iter().map(|e| e.database.db_path).collect());
        }
        let mut references = entries
            .iter()
            .map(|e| e.database.reference.clone())
            .collect::<Vec<_>>();
        let mut originals = Vec::new();
        for old in legacy {
            ensure!(
                !old.passphrase.is_empty(),
                "credential_original_secret_missing"
            );
            let url = url::Url::parse(&old.homeserver)?;
            ensure!(
                matches!(url.scheme(), "http" | "https")
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none(),
                "credential_retained_identity_invalid"
            );
            ensure!(
                !entries.iter().any(|e| e.database.db_path == old.db_path),
                "credential_retained_database_ambiguous"
            );
            let entry = if let Some(entry) = self.original_entry(&old, &hash, session_held)? {
                entry
            } else {
                let context = RetainedIdentity::LegacyDatabase {
                    account_hash: hash.clone(),
                    homeserver: old.homeserver.clone(),
                    db_path: old.db_path.clone(),
                };
                let key_scope = context.scope(&installation)?;
                let reference = if let Some(r) = pending
                    .as_ref()
                    .and_then(|j| j.references.iter().find(|r| r.scope() == &key_scope))
                {
                    r.clone()
                } else {
                    Vault::fresh_reference(&key_scope)?
                };
                originals.push((
                    reference.clone(),
                    key_scope,
                    Zeroizing::new(old.passphrase.clone()),
                ));
                RetainedEntry {
                    identity: context,
                    database: Database {
                        homeserver: old.homeserver.clone(),
                        db_path: old.db_path.clone(),
                        reference,
                        legacy_account: Some(hash.clone()),
                    },
                }
            };
            entry
                .identity
                .validate(&self.root, &hash, &entry.database)?;
            if !originals
                .iter()
                .any(|(r, _, _)| r == &entry.database.reference)
            {
                ensure!(
                    self.vault
                        .load(
                            &entry.database.reference,
                            &entry.identity.scope(&installation)?
                        )?
                        .as_slice()
                        == old.passphrase.as_bytes(),
                    "credential_original_secret_mismatch"
                );
            }
            references.push(entry.database.reference.clone());
            entries.push(entry);
        }
        let journal = doc.begin(&scope, source.as_ref(), references)?;
        for (reference, scope, secret) in originals {
            self.create_original(&reference, &scope, secret.as_bytes())?;
        }
        let result = entries.iter().map(|e| e.database.db_path.clone()).collect();
        let encoded = serde_json::to_vec(&Retained {
            version: VERSION,
            account_hash: hash,
            databases: entries,
        })?;
        let prepared = doc.prepare(journal, &encoded)?;
        let _gate = crate::persistence::SESSION_IO
            .lock()
            .map_err(|_| Error::Storage)?;
        ensure!(
            doc.publish(prepared, source.as_ref(), || crate::account_session::epoch(
            ) == epoch)?
                == Published::Durable,
            "credential_commit_durability_unknown"
        );
        Ok(result)
    }
}
pub(crate) async fn retain(
    path: PathBuf,
    owner: OwnedUserId,
    session: ClientSessionPersisted,
) -> Result<()> {
    let epoch = crate::account_session::epoch();
    let service = global()?.clone();
    let original = service.clone();
    service
        .worker
        .call(move || {
            original
                .retain_worker(&path, Some(owner), Some(session), epoch)
                .map(|_| ())
                .map_err(classify)
        })
        .await?;
    Ok(())
}
/// Complete native-key inventory; unknown or inaccessible entries refuse GC.
pub(crate) async fn retained_paths(path: PathBuf) -> Result<Vec<PathBuf>> {
    let epoch = crate::account_session::epoch();
    let service = global()?.clone();
    let original = service.clone();
    service
        .worker
        .call(move || {
            original
                .retain_worker(&path, None, None, epoch)
                .map_err(classify)
        })
        .await
        .map_err(Into::into)
}

fn account_hash(root: &Path, owner: &OwnedUserId) -> Result<String> {
    Ok(crate::accounts::account_dir(root, owner)
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or(Error::Invalid)?
        .to_owned())
}
fn same_homeserver(left: &str, right: &str) -> Result<bool> {
    Ok(url::Url::parse(left)? == url::Url::parse(right)?)
}
pub(super) fn resolve_database_path(root: &Path, stored: &Path) -> Result<PathBuf> {
    if !stored.is_absolute() {
        return Ok(root.join(stored));
    }
    match std::fs::symlink_metadata(stored) {
        Ok(_) => Ok(stored.to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(root.join(stored.file_name().ok_or(Error::Invalid)?))
        }
        Err(error) => Err(error.into()),
    }
}
impl Service {
    /// Resolve the independently derived original key domain. Legacy data-only
    /// references never acquire a full-device scope merely because a new grant
    /// has identified that device.
    pub(super) fn resolve_database(
        &self,
        database: &Database,
        identity: &Identity,
        installation: &str,
    ) -> Result<Zeroizing<String>> {
        ensure!(
            database.homeserver == identity.homeserver && database.db_path == identity.db_path,
            "credential_database_identity_invalid"
        );
        let scope = if let Some(legacy) = &database.legacy_account {
            ensure!(
                legacy == &account_hash(&self.root, &identity.owner)?,
                "credential_database_identity_invalid"
            );
            RetainedIdentity::LegacyDatabase {
                account_hash: legacy.clone(),
                homeserver: database.homeserver.clone(),
                db_path: database.db_path.clone(),
            }
            .scope(installation)?
        } else {
            database_scope(installation, identity)?
        };
        let bytes = self.vault.load(&database.reference, &scope)?;
        ensure!(!bytes.is_empty(), "credential_original_secret_missing");
        Ok(Zeroizing::new(
            String::from_utf8(bytes.to_vec()).map_err(|_| Error::Integrity)?,
        ))
    }
    /// Only an actual pre-creation receipt may bypass this guard in the caller.
    /// A new OAuth grant alone is not evidence that an existing DB is fresh.
    pub(super) fn validate_unreferenced_database(&self, identity: &Identity) -> Result<()> {
        let path = resolve_database_path(&self.root, &identity.db_path)?;
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "credential_original_secret_missing"
        );
        let mut entries = std::fs::read_dir(path)?;
        ensure!(
            entries.next().transpose()?.is_none(),
            "credential_original_secret_missing"
        );
        Ok(())
    }
    fn candidates(
        &self,
        owner: &OwnedUserId,
        homeserver: &str,
        epoch: u64,
    ) -> Result<Vec<(Database, Zeroizing<String>)>> {
        self.candidates_with(owner, homeserver, epoch, false)
    }
    fn candidates_with(
        &self,
        owner: &OwnedUserId,
        homeserver: &str,
        epoch: u64,
        session_held: bool,
    ) -> Result<Vec<(Database, Zeroizing<String>)>> {
        let path = crate::accounts::account_dir(&self.root, owner)
            .join("persistent_state/retained_db.json");
        match std::fs::symlink_metadata(&path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        }
        self.retain_worker_with(&path, Some(owner.clone()), None, epoch, session_held)?;
        let installation = self.existing_installation()?;
        let doc = Document::open(&path, MAX_SESSION)?;
        let source = doc.read()?.ok_or(Error::Missing)?;
        let hash = account_hash(&self.root, owner)?;
        let entries = self.retained_entries(source.bytes(), &hash, &installation)?;
        let mut result = Vec::new();
        for entry in entries {
            ensure!(
                crate::account_session::epoch() == epoch,
                "credential_operation_retired"
            );
            // Validate every entry/key before selecting one, so an unrelated
            // corrupt record is not hidden by a successful earlier match.
            let bytes = self.vault.load(
                &entry.database.reference,
                &entry.identity.scope(&installation)?,
            )?;
            let secret =
                Zeroizing::new(String::from_utf8(bytes.to_vec()).map_err(|_| Error::Integrity)?);
            ensure!(!secret.is_empty(), "credential_original_secret_missing");
            if same_homeserver(&entry.database.homeserver, homeserver)? {
                let mut database = entry.database;
                database.legacy_account = match entry.identity {
                    RetainedIdentity::LegacyDatabase { account_hash, .. } => Some(account_hash),
                    RetainedIdentity::Session { .. } => None,
                };
                result.push((database, secret));
            }
        }
        Ok(result)
    }
    /// Exact retained reference selection only. A missing key/schema is an
    /// error, never a miss that permits issuing a replacement database key.
    #[cfg(test)]
    pub(super) fn retained_database(&self, identity: &Identity) -> Result<Option<Database>> {
        self.retained_database_with(identity, false)
    }
    /// `session_held` as in `retain_worker_with`.
    pub(super) fn retained_database_with(
        &self,
        identity: &Identity,
        session_held: bool,
    ) -> Result<Option<Database>> {
        let mut result = None;
        for (database, _) in self.candidates_with(
            &identity.owner,
            &identity.homeserver,
            crate::account_session::epoch(),
            session_held,
        )? {
            if database.db_path != identity.db_path || database.homeserver != identity.homeserver {
                continue;
            }
            // Full scopes include the device; a Legacy scope remains data-only.
            self.resolve_database(&database, identity, &self.existing_installation()?)?;
            ensure!(result.is_none(), "credential_retained_database_ambiguous");
            result = Some(database);
        }
        Ok(result)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("credential_retained_device_changed")]
struct RetainedDeviceChanged;
/// Execute only on the existing credential worker, including callers already
/// inside its handoff job. This never initializes/migrates/checkpoints the DB.
pub(super) fn verify_retained_identity_on_worker(
    root: &Path,
    database: &Database,
    identity: &Identity,
    passphrase: &str,
) -> Result<()> {
    verify_original(root, database, identity, passphrase, || {})
}

// The hook is a no-op in production. The cancellation regression pauses while
// this actual original directory/file custody is held by the credential worker.
fn verify_original(
    root: &Path,
    database: &Database,
    identity: &Identity,
    passphrase: &str,
    after_original: impl FnOnce(),
) -> Result<()> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
    ensure!(
        database.homeserver == identity.homeserver && database.db_path == identity.db_path,
        "credential_database_identity_invalid"
    );
    let path = resolve_database_path(root, &database.db_path)?;
    let directory = std::fs::symlink_metadata(&path)?;
    ensure!(
        directory.is_dir() && !directory.file_type().is_symlink(),
        "credential_database_identity_invalid"
    );
    let original_directory =
        cap_std::fs::Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
    let directory_identity =
        hagency_platform::directory_identity(&original_directory.try_clone()?.into_std_file())?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let original = original_directory
        .open_with("matrix-sdk-crypto.sqlite3", &options)?
        .into_std();
    ensure!(
        original.metadata()?.is_file() && original.metadata()?.len() > 0,
        "credential_original_database_missing"
    );
    let original_identity = hagency_platform::file_identity(&original)?;
    // Resolve system aliases (for example macOS /var) only after capturing
    // the authorized original, then require that exact directory and file.
    let inspection_path = path.canonicalize()?;
    let inspection_directory =
        cap_std::fs::Dir::open_ambient_dir(&inspection_path, cap_std::ambient_authority())?;
    ensure!(
        hagency_platform::directory_identity(&inspection_directory.try_clone()?.into_std_file())?
            == directory_identity
            && hagency_platform::file_identity(
                &inspection_directory
                    .open_with("matrix-sdk-crypto.sqlite3", &options)?
                    .into_std()
            )? == original_identity,
        "credential_original_database_changed"
    );
    after_original();
    let account = super::database_inspect::inspect_existing_account_identity(
        &inspection_path,
        Some(passphrase),
    )?;
    let current_directory =
        cap_std::fs::Dir::open_ambient_dir(&path, cap_std::ambient_authority())?;
    ensure!(
        hagency_platform::directory_identity(&current_directory.try_clone()?.into_std_file())?
            == directory_identity,
        "credential_original_database_changed"
    );
    let current = current_directory
        .open_with("matrix-sdk-crypto.sqlite3", &options)?
        .into_std();
    ensure!(
        hagency_platform::file_identity(&current)? == original_identity,
        "credential_original_database_changed"
    );
    ensure!(
        account.user_id().as_str() == identity.owner.as_str(),
        "credential_retained_owner_changed"
    );
    if account.device_id().as_str() != identity.device {
        return Err(RetainedDeviceChanged.into());
    }
    Ok(())
}

/// Await only an observer of the existing worker. All original IO, file
/// handles, and secret custody stay in its admitted job until real completion.
pub(super) async fn verify_retained_identity(
    service: Arc<Service>,
    database: Database,
    identity: Identity,
    secret: Zeroizing<String>,
) -> Result<()> {
    let original = service.clone();
    service
        .worker
        .call(move || {
            // Keep the typed inner error (notably changed device) distinct
            // from worker admission/retirement errors.
            Ok(verify_retained_identity_on_worker(
                &original.root,
                &database,
                &identity,
                &secret,
            ))
        })
        .await?
}

pub(crate) async fn retained_session(
    client: &matrix_sdk::Client,
) -> Result<Option<ClientSessionPersisted>> {
    retained_session_on(global()?.clone(), client).await
}
async fn retained_session_on(
    service: Arc<Service>,
    client: &matrix_sdk::Client,
) -> Result<Option<ClientSessionPersisted>> {
    let epoch = crate::account_session::epoch();
    let owner = client.user_id().ok_or(Error::Invalid)?.to_owned();
    let device = client.device_id().ok_or(Error::Invalid)?.to_string();
    let homeserver = client.homeserver().to_string();
    let original = service.clone();
    let lookup_owner = owner.clone();
    let lookup_home = homeserver.clone();
    let candidates = service
        .worker
        .call(move || {
            if crate::account_session::epoch() != epoch {
                return Err(Error::Retired);
            }
            original
                .candidates(&lookup_owner, &lookup_home, epoch)
                .map_err(classify)
        })
        .await?;
    let mut result = None;
    for (database, secret) in candidates {
        ensure!(
            crate::account_session::epoch() == epoch,
            "credential_operation_retired"
        );
        let identity = Identity {
            owner: owner.clone(),
            device: device.clone(),
            homeserver: database.homeserver.clone(),
            oauth_client_id: None,
            db_path: database.db_path.clone(),
        };
        match verify_retained_identity(service.clone(), database.clone(), identity, secret.clone())
            .await
        {
            Ok(()) => (),
            Err(error) if error.downcast_ref::<RetainedDeviceChanged>().is_some() => continue,
            Err(error) => return Err(error),
        }
        ensure!(result.is_none(), "credential_retained_database_ambiguous");
        result = Some(ClientSessionPersisted {
            homeserver: database.homeserver,
            db_path: database.db_path,
            passphrase: secret.to_string(),
        });
    }
    ensure!(
        crate::account_session::epoch() == epoch
            && client.user_id() == Some(&owner)
            && client.device_id().is_some_and(|d| d.as_str() == device)
            && client.homeserver().as_str() == homeserver,
        "credential_operation_retired"
    );
    Ok(result)
}
