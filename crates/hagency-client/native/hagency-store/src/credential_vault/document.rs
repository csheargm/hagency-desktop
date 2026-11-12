//! Private, locked, compare-before-replace credential documents. A commit that
//! renamed successfully but failed parent sync is UNKNOWN, never rolled back.
//! Journal files contain references and digests, never legacy secret bytes.
use super::{Error, Scope, SecretRef, hex, random_id};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const MAX_JOURNAL: usize = 65536;

pub struct Document {
    directory: Dir,
    path: PathBuf,
    name: String,
    journal: String,
    _lock: File,
    limit: usize,
    #[cfg(test)]
    fail_parent_sync_after_rename: std::sync::atomic::AtomicBool,
    #[cfg(test)]
    fail_finish_after_rename: std::sync::atomic::AtomicBool,
}
pub struct Snapshot {
    identity: File,
    physical: hagency_platform::FileIdentity,
    bytes: Zeroizing<Vec<u8>>,
    digest: String,
}
impl Snapshot {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn physical_identity(&self) -> &hagency_platform::FileIdentity {
        &self.physical
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Journal {
    version: u32,
    pub scope: Scope,
    pub source_digest: Option<String>,
    pub source_identity: Option<hagency_platform::FileIdentity>,
    pub references: Vec<SecretRef>,
    candidate: String,
    payload_digest: Option<String>,
    journal_identity: Option<hagency_platform::FileIdentity>,
    #[serde(skip)]
    witness: Option<std::sync::Arc<File>>,
}
impl Journal {
    pub fn candidate_digest(&self) -> Option<&str> {
        self.payload_digest.as_deref()
    }
}
/// An encrypted candidate remains journal-owned on every failure. Drop does not
/// delete anything: cancellation is not evidence that a rename never happened.
pub struct Prepared {
    journal: Journal,
    candidate: std::sync::Arc<File>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Published {
    Durable,
    DurabilityUnknown,
}

impl Document {
    /// The parent must already be private. Locks serialize only this document;
    /// they do not authorize a stale account to publish after its epoch retires.
    pub fn open(path: &Path, limit: usize) -> Result<Self, Error> {
        if limit == 0 || limit > 16 * 1024 * 1024 {
            return Err(Error::Invalid);
        }
        let parent = path.parent().ok_or(Error::Invalid)?;
        crate::private::directory(parent).map_err(|_| Error::Storage)?;
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .filter(|n| !n.is_empty() && !n.starts_with('.'))
            .ok_or(Error::Invalid)?
            .to_owned();
        let directory = Dir::open_ambient_dir(parent, cap_std::ambient_authority())
            .map_err(|_| Error::Storage)?;
        crate::private::check_handle(
            &directory
                .try_clone()
                .map_err(|_| Error::Storage)?
                .into_std_file(),
        )
        .map_err(|_| Error::Storage)?;
        let lockname = format!(".{name}.vault-lock");
        let lock = match create(&directory, &lockname) {
            Ok(file) => file,
            Err(Error::Conflict) => open(&directory, &lockname)?,
            Err(error) => return Err(error),
        };
        lock.try_lock().map_err(|_| Error::Conflict)?;
        let document = Self {
            directory,
            path: path.into(),
            journal: format!(".{name}.vault-journal"),
            name,
            _lock: lock,
            limit,
            #[cfg(test)]
            fail_parent_sync_after_rename: std::sync::atomic::AtomicBool::new(false),
            #[cfg(test)]
            fail_finish_after_rename: std::sync::atomic::AtomicBool::new(false),
        };
        document.check_parent()?;
        Ok(document)
    }
    #[cfg(test)]
    pub(super) fn fail_next_finish_after_rename(&self) {
        self.fail_finish_after_rename
            .store(true, std::sync::atomic::Ordering::Release);
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    #[cfg(test)]
    pub(super) fn fail_next_parent_sync_after_rename(&self) {
        self.fail_parent_sync_after_rename
            .store(true, std::sync::atomic::Ordering::Release);
    }
    fn check_parent(&self) -> Result<(), Error> {
        let ambient = Dir::open_ambient_dir(
            self.path.parent().ok_or(Error::Invalid)?,
            cap_std::ambient_authority(),
        )
        .map_err(|_| Error::Conflict)?;
        if !hagency_platform::same_directory(
            &self
                .directory
                .try_clone()
                .map_err(|_| Error::Storage)?
                .into_std_file(),
            &ambient.into_std_file(),
        )
        .map_err(|_| Error::Storage)?
        {
            return Err(Error::Conflict);
        }
        crate::private::check_handle(
            &self
                .directory
                .try_clone()
                .map_err(|_| Error::Storage)?
                .into_std_file(),
        )
        .map_err(|_| Error::Storage)
    }
    pub fn read(&self) -> Result<Option<Snapshot>, Error> {
        self.check_parent()?;
        let file = match open(&self.directory, &self.name) {
            Err(Error::Missing) => return Ok(None),
            other => other?,
        };
        let bytes = read(&file, self.limit)?;
        Ok(Some(Snapshot {
            physical: hagency_platform::file_identity(&file).map_err(|_| Error::Storage)?,
            identity: file,
            digest: hex(&Sha256::digest(&bytes)),
            bytes,
        }))
    }
    fn read_pending(&self) -> Result<Option<Journal>, Error> {
        let file = match open(&self.directory, &self.journal) {
            Err(Error::Missing) => return Ok(None),
            other => other?,
        };
        let bytes = read(&file, MAX_JOURNAL)?;
        let mut journal: Journal = serde_json::from_slice(&bytes).map_err(|_| Error::Invalid)?;
        journal.scope.validate()?;
        if journal.version != 1
            || journal.journal_identity.is_none()
            || journal.source_digest.is_some() != journal.source_identity.is_some()
            || journal.references.is_empty()
            || journal.references.len() > 128
            || !valid_candidate(&journal.candidate, &self.name)
        {
            return Err(Error::Invalid);
        }
        if journal.journal_identity.as_ref()
            != Some(&hagency_platform::file_identity(&file).map_err(|_| Error::Storage)?)
        {
            return Err(Error::Conflict);
        }
        journal.witness = Some(std::sync::Arc::new(file));
        for reference in &journal.references {
            reference.check_owner(&journal.scope)?;
        }
        Ok(Some(journal))
    }
    pub fn pending(&self, scope: &Scope) -> Result<Option<Journal>, Error> {
        let journal = self.read_pending()?;
        if journal
            .as_ref()
            .is_some_and(|journal| &journal.scope != scope)
        {
            return Err(Error::Invalid);
        }
        Ok(journal)
    }
    /// Recovery may need to authenticate a newly issued grant's complete
    /// candidate before the previous canonical document has been replaced.
    /// This establishes installation ownership only; callers must validate
    /// every candidate identity/key reference and its authenticated payload.
    pub fn pending_for_installation(&self, installation: &str) -> Result<Option<Journal>, Error> {
        if !super::valid_id(installation) {
            return Err(Error::Invalid);
        }
        let journal = self.read_pending()?;
        if journal
            .as_ref()
            .is_some_and(|journal| journal.scope.installation != installation)
        {
            return Err(Error::Invalid);
        }
        Ok(journal)
    }
    /// Install intent BEFORE any OS key creation. Recovery reuses these exact
    /// references; it must not mint replacements for an existing encrypted DB.
    pub fn begin(
        &self,
        scope: &Scope,
        source: Option<&Snapshot>,
        refs: Vec<SecretRef>,
    ) -> Result<Journal, Error> {
        if let Some(journal) = self.pending(scope)? {
            if journal.source_digest.as_deref() != source.map(Snapshot::digest)
                || journal.source_identity.as_ref() != source.map(Snapshot::physical_identity)
                || journal.references != refs
            {
                return Err(Error::Conflict);
            }
            return Ok(journal);
        }
        if refs.is_empty() || refs.len() > 128 {
            return Err(Error::Invalid);
        }
        for reference in &refs {
            reference.check_owner(scope)?;
        }
        self.matches(source)?;
        let mut journal = Journal {
            version: 1,
            scope: scope.clone(),
            source_digest: source.map(|s| s.digest.clone()),
            source_identity: source.map(|s| s.physical.clone()),
            references: refs,
            candidate: format!(".{}.vault-{}.tmp", self.name, random_id()?),
            payload_digest: None,
            journal_identity: None,
            witness: None,
        };
        self.write_journal(&mut journal)?;
        Ok(journal)
    }
    pub fn prepare(&self, mut journal: Journal, encrypted: &[u8]) -> Result<Prepared, Error> {
        if encrypted.len() > self.limit {
            return Err(Error::Invalid);
        }
        // Reject stale/foreign intent instead of silently replacing its keys.
        let original = self.pending(&journal.scope)?.ok_or(Error::Conflict)?;
        if original.candidate != journal.candidate
            || original.references != journal.references
            || original.source_digest != journal.source_digest
            || original.source_identity != journal.source_identity
            || original.journal_identity != journal.journal_identity
            || !hagency_platform::same_file(
                original.witness.as_ref().ok_or(Error::Conflict)?,
                journal.witness.as_ref().ok_or(Error::Conflict)?,
            )
            .map_err(|_| Error::Storage)?
        {
            return Err(Error::Conflict);
        }
        match open(&self.directory, &journal.candidate) {
            Ok(file) => {
                let existing = read(&file, self.limit)?;
                // Never overwrite an existing candidate's journal hash before
                // verifying it; a failed retry must preserve its recovery proof.
                let digest = hex(&Sha256::digest(&existing));
                if original.payload_digest.as_deref() != Some(digest.as_str()) {
                    return Err(Error::Integrity);
                }
                if existing.as_slice() != encrypted {
                    return Err(Error::Conflict);
                }
                file.sync_all().map_err(|_| Error::Storage)?;
                return Ok(Prepared {
                    journal: original,
                    candidate: std::sync::Arc::new(file),
                });
            }
            Err(Error::Missing) => {}
            Err(error) => return Err(error),
        }
        journal.payload_digest = Some(hex(&Sha256::digest(encrypted)));
        self.write_journal(&mut journal)?;
        let mut file = create(&self.directory, &journal.candidate)?;
        file.write_all(encrypted).map_err(|_| Error::Storage)?;
        file.sync_all().map_err(|_| Error::Storage)?;
        crate::private::sync_directory(&self.directory).map_err(|_| Error::DurabilityUnknown)?;
        Ok(Prepared {
            journal,
            candidate: std::sync::Arc::new(file),
        })
    }
    /// Recover only a complete, digest-matching encrypted candidate. The caller
    /// MUST authenticate these bytes/context before publishing the returned
    /// prepared transaction. A partial candidate is retained for inspection.
    pub fn resume_candidate(
        &self,
        journal: &Journal,
    ) -> Result<(Prepared, Zeroizing<Vec<u8>>), Error> {
        let actual = self.pending(&journal.scope)?.ok_or(Error::Conflict)?;
        if actual.candidate != journal.candidate
            || actual.payload_digest != journal.payload_digest
            || actual.references != journal.references
            || actual.source_digest != journal.source_digest
            || actual.source_identity != journal.source_identity
            || actual.journal_identity != journal.journal_identity
            || !hagency_platform::same_file(
                actual.witness.as_ref().ok_or(Error::Conflict)?,
                journal.witness.as_ref().ok_or(Error::Conflict)?,
            )
            .map_err(|_| Error::Storage)?
        {
            return Err(Error::Conflict);
        }
        let file = open(&self.directory, &journal.candidate)?;
        let bytes = read(&file, self.limit)?;
        if journal.payload_digest.as_deref() != Some(hex(&Sha256::digest(&bytes)).as_str()) {
            return Err(Error::Integrity);
        }
        Ok((
            Prepared {
                journal: journal.clone(),
                candidate: std::sync::Arc::new(file),
            },
            bytes,
        ))
    }
    /// Caller holds its epoch/SDK publication fence throughout this synchronous
    /// call. Native credential calls and candidate encryption happen beforehand.
    pub fn publish(
        &self,
        prepared: Prepared,
        source: Option<&Snapshot>,
        current: impl Fn() -> bool,
    ) -> Result<Published, Error> {
        if !current() {
            return Err(Error::Retired);
        }
        if prepared.journal.source_digest.as_deref() != source.map(Snapshot::digest)
            || prepared.journal.source_identity.as_ref() != source.map(Snapshot::physical_identity)
        {
            return Err(Error::Conflict);
        }
        self.check_parent()?;
        self.verify_journal(&prepared.journal)?;
        self.matches(source)?;
        let file = open(&self.directory, &prepared.journal.candidate)?;
        if !hagency_platform::same_file(&file, &prepared.candidate).map_err(|_| Error::Storage)? {
            return Err(Error::Conflict);
        }
        let bytes = read(&file, self.limit)?;
        if prepared.journal.payload_digest.as_deref() != Some(hex(&Sha256::digest(&bytes)).as_str())
        {
            return Err(Error::Integrity);
        }
        // The compare and account fence remain valid through actual rename.
        if !current() {
            return Err(Error::Retired);
        }
        self.verify_journal(&prepared.journal)?;
        self.matches(source)?;
        let current_candidate = open(&self.directory, &prepared.journal.candidate)?;
        if !hagency_platform::same_file(&current_candidate, &prepared.candidate)
            .map_err(|_| Error::Storage)?
        {
            return Err(Error::Conflict);
        }
        self.directory
            .rename(&prepared.journal.candidate, &self.directory, &self.name)
            .map_err(|_| Error::Storage)?;
        #[cfg(test)]
        if self
            .fail_parent_sync_after_rename
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Ok(Published::DurabilityUnknown);
        }
        if crate::private::sync_directory(&self.directory).is_err() {
            return Ok(Published::DurabilityUnknown);
        }
        #[cfg(test)]
        if self
            .fail_finish_after_rename
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Ok(Published::DurabilityUnknown);
        }
        // The canonical rename already happened. A failed cleanup is an
        // unknown committed outcome, never a pre-publication conflict.
        if self.finish(&prepared.journal).is_err() {
            return Ok(Published::DurabilityUnknown);
        }
        Ok(Published::Durable)
    }
    /// After a crash, only a caller that has authenticated the published v2
    /// payload and resolved its exact refs may complete this journal. A digest
    /// match alone does not authenticate credentials or prove native presence.
    pub fn finish_authenticated(
        &self,
        journal: &Journal,
        authenticated_refs: &[SecretRef],
    ) -> Result<(), Error> {
        if journal.references != authenticated_refs {
            return Err(Error::Conflict);
        }
        let actual = self.read()?.ok_or(Error::Missing)?;
        if journal.payload_digest.as_deref() != Some(actual.digest()) {
            return Err(Error::Conflict);
        }
        crate::private::sync_directory(&self.directory).map_err(|_| Error::DurabilityUnknown)?;
        self.finish(journal)
    }
    /// Retire a preparation that never replaced its original source. The caller
    /// must first authenticate a durable revoke-only outcome owning these exact
    /// refs; native entries are retained. Any candidate is authenticated again
    /// through `verify_candidate` before its original private object is removed.
    /// A replacement source (including a rename with unknown parent durability)
    /// is a conflict and is never rolled back by this operation.
    pub fn discard_unpublished(
        &self,
        journal: &Journal,
        expected: Option<&Snapshot>,
        authenticated_refs: &[SecretRef],
        verify_candidate: impl FnOnce(&[u8]) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.check_parent()?;
        self.matches(expected)?;
        let actual = self.pending(&journal.scope)?.ok_or(Error::Conflict)?;
        if actual.references != authenticated_refs
            || actual.candidate != journal.candidate
            || actual.journal_identity != journal.journal_identity
            || !hagency_platform::same_file(
                actual.witness.as_ref().ok_or(Error::Conflict)?,
                journal.witness.as_ref().ok_or(Error::Conflict)?,
            )
            .map_err(|_| Error::Storage)?
            || actual.payload_digest != journal.payload_digest
            || actual.source_digest.as_deref() != expected.map(Snapshot::digest)
            || actual.source_identity.as_ref() != expected.map(Snapshot::physical_identity)
        {
            return Err(Error::Conflict);
        }
        match open(&self.directory, &actual.candidate) {
            Ok(file) => {
                let bytes = read(&file, self.limit)?;
                let digest = hex(&Sha256::digest(&bytes));
                if actual.payload_digest.as_deref() != Some(digest.as_str()) {
                    return Err(Error::DurabilityUnknown);
                }
                verify_candidate(&bytes)?;
                let current = open(&self.directory, &actual.candidate)?;
                if !hagency_platform::same_file(&file, &current).map_err(|_| Error::Storage)?
                    || read(&current, self.limit)?.as_slice() != bytes.as_slice()
                {
                    return Err(Error::Conflict);
                }
                self.matches(expected)?;
                // Both retained handles permit FILE_SHARE_DELETE; close after
                // checking identity so Windows can unlink the original name.
                self.directory
                    .remove_file(&actual.candidate)
                    .map_err(|_| Error::DurabilityUnknown)?;
            }
            Err(Error::Missing) if actual.payload_digest.is_none() => (),
            Err(Error::Missing) => return Err(Error::DurabilityUnknown),
            Err(error) => return Err(error),
        }
        self.matches(expected)?;
        self.finish(&actual)
    }
    fn verify_journal(&self, journal: &Journal) -> Result<(), Error> {
        let actual = self.pending(&journal.scope)?.ok_or(Error::Conflict)?;
        if actual.candidate != journal.candidate
            || actual.payload_digest != journal.payload_digest
            || actual.journal_identity != journal.journal_identity
            || !hagency_platform::same_file(
                actual.witness.as_ref().ok_or(Error::Conflict)?,
                journal.witness.as_ref().ok_or(Error::Conflict)?,
            )
            .map_err(|_| Error::Storage)?
        {
            return Err(Error::Conflict);
        }
        Ok(())
    }
    fn finish(&self, journal: &Journal) -> Result<(), Error> {
        self.verify_journal(journal)?;
        self.directory
            .remove_file(&self.journal)
            .map_err(|_| Error::DurabilityUnknown)?;
        crate::private::sync_directory(&self.directory).map_err(|_| Error::DurabilityUnknown)
    }
    pub fn remove_original(
        &self,
        expected: &Snapshot,
        current: impl Fn() -> bool,
    ) -> Result<Published, Error> {
        if !current() {
            return Err(Error::Retired);
        }
        self.check_parent()?;
        self.matches(Some(expected))?;
        if !current() {
            return Err(Error::Retired);
        }
        self.directory
            .remove_file(&self.name)
            .map_err(|_| Error::Storage)?;
        if crate::private::sync_directory(&self.directory).is_err() {
            return Ok(Published::DurabilityUnknown);
        }
        Ok(Published::Durable)
    }
    pub fn check_original(&self, expected: Option<&Snapshot>) -> Result<(), Error> {
        self.matches(expected)
    }
    fn matches(&self, expected: Option<&Snapshot>) -> Result<(), Error> {
        match (expected, self.read()?) {
            (None, None) => Ok(()),
            (Some(expected), Some(actual))
                if expected.digest == actual.digest
                    && hagency_platform::same_file(&expected.identity, &actual.identity)
                        .map_err(|_| Error::Storage)? =>
            {
                Ok(())
            }
            _ => Err(Error::Conflict),
        }
    }
    fn write_journal(&self, journal: &mut Journal) -> Result<(), Error> {
        let name = format!(".{}.vault-journal-{}.tmp", self.name, random_id()?);
        let mut file = create(&self.directory, &name)?;
        journal.journal_identity =
            Some(hagency_platform::file_identity(&file).map_err(|_| Error::Storage)?);
        let bytes = serde_json::to_vec(journal).map_err(|_| Error::Invalid)?;
        if bytes.len() > MAX_JOURNAL {
            return Err(Error::Invalid);
        }
        file.write_all(&bytes).map_err(|_| Error::Storage)?;
        file.sync_all().map_err(|_| Error::Storage)?;
        self.directory
            .rename(&name, &self.directory, &self.journal)
            .map_err(|_| Error::Storage)?;
        journal.witness = Some(std::sync::Arc::new(file));
        crate::private::sync_directory(&self.directory).map_err(|_| Error::DurabilityUnknown)
    }
}
fn valid_candidate(candidate: &str, name: &str) -> bool {
    candidate
        .strip_prefix(&format!(".{name}.vault-"))
        .and_then(|suffix| suffix.strip_suffix(".tmp"))
        .is_some_and(super::valid_id)
}

fn open(directory: &Dir, name: &str) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsSyncExt;
        options.nonblock(true);
    }
    let file = directory
        .open_with(name, &options)
        .map_err(classify)?
        .into_std();
    crate::private::check_handle(&file).map_err(|_| Error::Storage)?;
    Ok(file)
}
fn create(directory: &Dir, name: &str) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{WRITE_DAC, WRITE_OWNER},
        };
        options.access_mode(GENERIC_READ | GENERIC_WRITE | WRITE_DAC | WRITE_OWNER);
    }
    let file = directory
        .open_with(name, &options)
        .map_err(classify)?
        .into_std();
    // No bytes until native ownership and DACL validation succeeds. On failure
    // the empty create is retained as an unknown artifact, never blindly unlinked.
    crate::private::seal_created_file_handle(&file).map_err(|_| Error::Storage)?;
    Ok(file)
}
fn read(file: &File, max: usize) -> Result<Zeroizing<Vec<u8>>, Error> {
    if file.metadata().map_err(|_| Error::Storage)?.len() > max as u64 {
        return Err(Error::Invalid);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Storage)?;
    if bytes.len() > max {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}
fn classify(error: std::io::Error) -> Error {
    match error.kind() {
        std::io::ErrorKind::NotFound => Error::Missing,
        std::io::ErrorKind::AlreadyExists => Error::Conflict,
        _ => Error::Storage,
    }
}

/// One nonsecret random installation namespace per native data root. Existing
/// encrypted documents MUST pass allow_create=false; losing this marker is not
/// permission to silently adopt a new namespace or replace database secrets.
pub fn installation(root: &Path, allow_create: bool) -> Result<String, Error> {
    let key_directory = root.join("credentials-v1");
    if !allow_create && !key_directory.exists() {
        return Err(Error::Missing);
    }
    crate::private::directory(&key_directory).map_err(|_| Error::Storage)?;
    let retained = Dir::open_ambient_dir(&key_directory, cap_std::ambient_authority())
        .map_err(|_| Error::Storage)?;
    let file = match open(&retained, "installation") {
        Ok(file) => file,
        Err(Error::Missing) if allow_create => {
            let id = random_id()?;
            match create(&retained, "installation") {
                Ok(mut file) => {
                    file.write_all(id.as_bytes()).map_err(|_| Error::Storage)?;
                    file.sync_all().map_err(|_| Error::Storage)?;
                    crate::private::sync_directory(&retained)
                        .map_err(|_| Error::DurabilityUnknown)?;
                    return Ok(id);
                }
                Err(Error::Conflict) => open(&retained, "installation")?,
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    };
    let bytes = read(&file, 32)?;
    let id = std::str::from_utf8(&bytes).map_err(|_| Error::Invalid)?;
    if !super::valid_id(id) {
        return Err(Error::Invalid);
    }
    Ok(id.into())
}

/// A retained process lease for one authentication grant. A retired UI observer
/// must not own its lifetime: retain this with the original SDK/operation until
/// the original SDK has actually dropped. Separate grants use separate names.
pub struct GrantLease {
    _file: File,
    _directory: Dir,
}
impl GrantLease {
    pub fn acquire(directory: &Path, grant: &SecretRef) -> Result<Self, Error> {
        crate::private::directory(directory).map_err(|_| Error::Storage)?;
        let retained = Dir::open_ambient_dir(directory, cap_std::ambient_authority())
            .map_err(|_| Error::Storage)?;
        let digest = hex(&Sha256::digest(
            serde_json::to_vec(grant).map_err(|_| Error::Invalid)?,
        ));
        let name = format!("grant_{digest}.lease");
        let file = match create(&retained, &name) {
            Ok(file) => {
                file.sync_all().map_err(|_| Error::Storage)?;
                crate::private::sync_directory(&retained).map_err(|_| Error::DurabilityUnknown)?;
                file
            }
            Err(Error::Conflict) => open(&retained, &name)?,
            Err(error) => return Err(error),
        };
        if file.metadata().map_err(|_| Error::Storage)?.len() != 0 {
            return Err(Error::Invalid);
        }
        file.try_lock().map_err(|_| Error::Conflict)?;
        Ok(Self {
            _file: file,
            _directory: retained,
        })
    }
}
