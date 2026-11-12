//! Deferred original refresh outcomes are isolated from canonical authority.
//! Only the same authentication grant can absorb Keep; revoke-only records
//! never become a login source. Original keys and process leases stay owned.
use super::*;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum Intent {
    Keep,
    Revoke,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Recovery {
    revision: u64,
    database: Database,
    target: Option<revoke_transport::Target>,
    progress: revoke_transport::Progress,
    version: u32,
    identity: Identity,
    grant: SecretRef,
    intent: Intent,
    user_session: PersistedUserSession,
    sync_token: Option<String>,
    sliding_sync_version: SlidingSyncVersion,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    version: u32,
    identity: Identity,
    grant: SecretRef,
    revocation_key: SecretRef,
    envelope: Envelope,
}

fn name(grant: &SecretRef, intent: Intent) -> Result<String> {
    let digest = blake3::hash(&serde_json::to_vec(grant)?).to_hex();
    Ok(format!(
        "{}_{digest}.json",
        if intent == Intent::Keep {
            "keep"
        } else {
            "revoke"
        }
    ))
}
pub(super) fn path(unlocked: &Unlocked, intent: Intent) -> Result<PathBuf> {
    Ok(unlocked
        .recovery_root
        .join(name(unlocked.key.reference(), intent)?))
}
fn key(unlocked: &Unlocked, intent: Intent) -> &DocumentKey {
    if intent == Intent::Keep {
        &unlocked.key
    } else {
        &unlocked.revocation
    }
}
fn authenticate(unlocked: &Unlocked, bytes: &[u8], intent: Intent) -> Result<Recovery> {
    let record: FileRecord = serde_json::from_slice(bytes)?;
    ensure!(
        record.version == VERSION
            && record.identity == unlocked.identity
            && record.grant == *unlocked.key.reference()
            && record.revocation_key == *unlocked.revocation.reference(),
        "credential_recovery_identity_invalid"
    );
    let value: Recovery = key(unlocked, intent).open(&record.envelope)?;
    ensure!(
        value.version == 1
            && value.database == unlocked.database
            && value.identity == unlocked.identity
            && value.grant == *unlocked.key.reference()
            && value.intent == intent
            && identity_from(
                &value.user_session,
                &unlocked.database.homeserver,
                &unlocked.database.db_path
            ) == unlocked.identity,
        "credential_recovery_integrity_failed"
    );
    Ok(value)
}
fn recovery_session(unlocked: &Unlocked, recovery: Recovery) -> FullSessionPersisted {
    FullSessionPersisted {
        client_session: ClientSessionPersisted {
            homeserver: unlocked.database.homeserver.clone(),
            db_path: unlocked.database.db_path.clone(),
            passphrase: unlocked.secret.to_string(),
        },
        user_session: recovery.user_session,
        sync_token: recovery.sync_token,
        sliding_sync_version: recovery.sliding_sync_version,
    }
}
/// Runs synchronously on the original SDK callback or owned credential worker.
/// Caller serializes against the epoch/owner retirement transition via SESSION_IO.
pub(super) fn write(
    unlocked: &Unlocked,
    session: &FullSessionPersisted,
    intent: Intent,
) -> Result<()> {
    ensure!(
        identity(session) == unlocked.identity,
        "credential_recovery_identity_invalid"
    );
    let path = path(unlocked, intent)?;
    let doc = Document::open(&path, MAX_SESSION)?;
    recover_original(&doc, unlocked, intent)?;
    let source = doc.read()?;
    let previous = if let Some(source) = &source {
        let previous = authenticate(unlocked, source.bytes(), intent)?;
        unlocked
            .revision
            .fetch_max(previous.revision, Ordering::AcqRel);
        Some(previous.progress)
    } else {
        None
    };
    let value = Recovery {
        database: unlocked.database.clone(),
        target: unlocked.target.get().cloned(),
        progress: previous.unwrap_or_default(),
        revision: unlocked.next_revision()?,
        version: 1,
        identity: unlocked.identity.clone(),
        grant: unlocked.key.reference().clone(),
        intent,
        user_session: session.user_session.clone(),
        sync_token: session.sync_token.clone(),
        sliding_sync_version: session.sliding_sync_version.clone(),
    };
    let key = key(unlocked, intent);
    let encoded = serde_json::to_vec(&FileRecord {
        version: VERSION,
        identity: unlocked.identity.clone(),
        grant: unlocked.key.reference().clone(),
        revocation_key: unlocked.revocation.reference().clone(),
        envelope: key.seal(&value)?,
    })?;
    let journal = doc.begin(
        key.reference().scope(),
        source.as_ref(),
        vec![key.reference().clone()],
    )?;
    let prepared = doc.prepare(journal, &encoded)?;
    ensure!(
        doc.publish(prepared, source.as_ref(), || true)? == Published::Durable,
        "credential_commit_durability_unknown"
    );
    Ok(())
}
fn recover_original(doc: &Document, unlocked: &Unlocked, intent: Intent) -> Result<()> {
    let key = key(unlocked, intent);
    let Some(journal) = doc.pending(key.reference().scope())? else {
        return Ok(());
    };
    let source = doc.read()?;
    if journal.candidate_digest() == source.as_ref().map(Snapshot::digest)
        && journal.candidate_digest().is_some()
    {
        authenticate(
            unlocked,
            source.as_ref().ok_or(Error::Missing)?.bytes(),
            intent,
        )?;
        doc.finish_authenticated(&journal, &[key.reference().clone()])?;
    } else if journal.candidate_digest().is_some() {
        let (prepared, bytes) = doc.resume_candidate(&journal)?;
        authenticate(unlocked, &bytes, intent)?;
        ensure!(
            doc.publish(prepared, source.as_ref(), || true)? == Published::Durable,
            "credential_commit_durability_unknown"
        );
    }
    Ok(())
}
/// This is a tombstone check, not a source of authentication credentials.
pub(super) fn revoke_pending(unlocked: &Unlocked) -> Result<bool> {
    let path = path(unlocked, Intent::Revoke)?;
    let doc = Document::open(&path, MAX_SESSION)?;
    recover_original(&doc, unlocked, Intent::Revoke)?;
    if doc
        .pending(unlocked.revocation.reference().scope())?
        .is_some()
    {
        return Err(Error::DurabilityUnknown.into());
    }
    let Some(source) = doc.read()? else {
        return Ok(false);
    };
    authenticate(unlocked, source.bytes(), Intent::Revoke)?;
    Ok(true)
}
/// Caller has acquired the original grant's process lease, so there is no live
/// original SDK that can consume another single-use refresh token concurrently.
pub(super) fn absorb_keep(
    doc: &Document,
    unlocked: &Unlocked,
    session: FullSessionPersisted,
    current: impl Fn() -> bool,
) -> Result<FullSessionPersisted> {
    if revoke_pending(unlocked)? {
        return Err(Error::SessionRetired.into());
    }
    let path = path(unlocked, Intent::Keep)?;
    let deferred = Document::open(&path, MAX_SESSION)?;
    recover_original(&deferred, unlocked, Intent::Keep)?;
    if deferred
        .pending(unlocked.key.reference().scope())?
        .is_some()
    {
        return Err(Error::DurabilityUnknown.into());
    }
    let Some(source) = deferred.read()? else {
        return Ok(session);
    };
    let recovery = authenticate(unlocked, source.bytes(), Intent::Keep)?;
    let original = doc.read()?.ok_or(Error::Missing)?;
    let stored: Stored = serde_json::from_slice(original.bytes())?;
    let canonical: Authentication = unlocked.key.open(&stored.authentication)?;
    unlocked.decode(stored)?;
    // An interrupted native preparation can leave a valid but older Keep.
    // Its authenticated sequence must never roll back a newer canonical pair.
    if recovery.revision <= canonical.revision {
        ensure!(
            deferred.remove_original(&source, &current)? == Published::Durable,
            "credential_commit_durability_unknown"
        );
        return Ok(session);
    }
    if sdk_lease_live(unlocked) {
        return Err(Error::OriginalBusy.into());
    }
    let revision = recovery.revision;
    let value = recovery_session(unlocked, recovery);
    unlocked.revision.fetch_max(revision, Ordering::AcqRel);
    let encoded = unlocked.encode_at(value.clone(), revision)?;
    let journal = doc.begin(
        unlocked.key.reference().scope(),
        Some(&original),
        vec![
            unlocked.key.reference().clone(),
            unlocked.database.reference.clone(),
            unlocked.revocation.reference().clone(),
        ],
    )?;
    let prepared = doc.prepare(journal, &encoded)?;
    ensure!(
        doc.publish(prepared, Some(&original), &current)? == Published::Durable,
        "credential_commit_durability_unknown"
    );
    ensure!(
        deferred.remove_original(&source, &current)? == Published::Durable,
        "credential_commit_durability_unknown"
    );
    Ok(value)
}

/// Only this authenticated revoke-only document reaches the network transport.
/// A public locator is never an endpoint, token source, or login authority.
struct RevocationRecord {
    path: PathBuf,
    source: Snapshot,
    outer: FileRecord,
    value: Recovery,
    key: DocumentKey,
}
fn standalone_session(value: &Recovery) -> FullSessionPersisted {
    FullSessionPersisted {
        client_session: ClientSessionPersisted {
            homeserver: value.identity.homeserver.clone(),
            db_path: value.identity.db_path.clone(),
            // This descriptor is never passed to an SDK/database constructor.
            passphrase: String::new(),
        },
        user_session: value.user_session.clone(),
        sync_token: value.sync_token.clone(),
        sliding_sync_version: value.sliding_sync_version.clone(),
    }
}
fn authenticate_standalone(
    service: &Service,
    path: &Path,
    bytes: &[u8],
) -> Result<(FileRecord, Recovery, DocumentKey)> {
    let outer: FileRecord = serde_json::from_slice(bytes)?;
    ensure!(
        outer.version == VERSION,
        "credential_recovery_schema_invalid"
    );
    let installation = service.existing_installation()?;
    let context = serde_json::to_vec(&outer.identity)?;
    let auth = Scope::new(&installation, Purpose::Session, &context)?;
    let scope = Scope::new(&installation, Purpose::Revocations, &context)?;
    outer.grant.validate(&auth)?;
    outer.revocation_key.validate(&scope)?;
    let session_path = crate::accounts::account_dir(&service.root, &outer.identity.owner)
        .join("persistent_state/session");
    service.check_account_path(&session_path, Some(&outer.identity.owner))?;
    ensure!(
        path == session_path
            .parent()
            .ok_or(Error::Invalid)?
            .join("credential-recovery-v1")
            .join(name(&outer.grant, Intent::Revoke)?),
        "credential_recovery_identity_invalid"
    );
    let key = service.vault.unlock(&outer.revocation_key, &scope)?;
    let value: Recovery = key.open(&outer.envelope)?;
    ensure!(
        value.version == 1
            && value.intent == Intent::Revoke
            && value.grant == outer.grant
            && value.identity == outer.identity
            && value.database.homeserver == value.identity.homeserver
            && value.database.db_path == value.identity.db_path
            && identity_from(
                &value.user_session,
                &value.identity.homeserver,
                &value.identity.db_path
            ) == value.identity,
        "credential_recovery_integrity_failed"
    );
    Ok((outer, value, key))
}
fn open_standalone(service: &Service, path: PathBuf) -> Result<RevocationRecord> {
    let doc = Document::open(&path, MAX_SESSION)?;
    if let Some(journal) = doc.pending_for_installation(&service.existing_installation()?)? {
        let source = doc.read()?;
        if journal.candidate_digest().is_some()
            && journal.candidate_digest() == source.as_ref().map(Snapshot::digest)
        {
            let (outer, _, _) = authenticate_standalone(
                service,
                &path,
                source.as_ref().ok_or(Error::Missing)?.bytes(),
            )?;
            doc.finish_authenticated(&journal, &[outer.revocation_key])?;
        } else if journal.candidate_digest().is_some() {
            let (prepared, bytes) = doc.resume_candidate(&journal)?;
            let (outer, _, _) = authenticate_standalone(service, &path, &bytes)?;
            ensure!(
                journal.references == vec![outer.revocation_key],
                "credential_recovery_integrity_failed"
            );
            ensure!(
                doc.publish(prepared, source.as_ref(), || true)? == Published::Durable,
                "credential_commit_durability_unknown"
            );
        } else {
            return Err(Error::DurabilityUnknown.into());
        }
    }
    let source = doc.read()?.ok_or(Error::Missing)?;
    let (outer, value, key) = authenticate_standalone(service, &path, source.bytes())?;
    // Release the actual per-document process lock before any network wait.
    drop(doc);
    Ok(RevocationRecord {
        path,
        source,
        outer,
        value,
        key,
    })
}
fn candidates(service: &Service) -> Result<Vec<PathBuf>> {
    let mut candidates = std::collections::BTreeSet::new();
    let entries = match std::fs::read_dir(&service.root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    for account in entries {
        let account = account?;
        if !account.file_type()?.is_dir() {
            continue;
        }
        let name = account.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("account_") || name.len() != 72 {
            continue;
        }
        let directory = account
            .path()
            .join("persistent_state/credential-recovery-v1");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        hagency_store::private::directory(&directory)?;
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                return Err(Error::Invalid.into());
            }
            let file = entry.file_name();
            let Some(file) = file.to_str() else {
                return Err(Error::Invalid.into());
            };
            let file = file.strip_prefix('.').unwrap_or(file);
            let canonical = file.strip_suffix(".vault-journal").unwrap_or(file);
            let Some(hash) = canonical
                .strip_prefix("revoke_")
                .and_then(|value| value.strip_suffix(".json"))
            else {
                continue;
            };
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(Error::Invalid.into());
            }
            candidates.insert(directory.join(canonical));
        }
    }
    Ok(candidates.into_iter().collect())
}
/// One record per idle turn, with a fixed transport deadline. The owning Worker
/// tracks this original IO through cancellation and shutdown; no lock is held
/// across the network and partial receipts compare against the original file.
pub(super) fn retry_one(
    service: &Service,
    runtime: &tokio::runtime::Handle,
) -> Result<Option<bool>> {
    let candidates = candidates(service)?;
    let selected = {
        let mut cursor = service
            .revoke_cursor
            .lock()
            .map_err(|_| Error::Unavailable)?;
        let selected = candidates
            .iter()
            .find(|path| cursor.as_ref().is_none_or(|previous| *path > previous))
            .or_else(|| candidates.first())
            .cloned();
        *cursor = selected.clone();
        selected
    };
    let Some(path) = selected else {
        return Ok(None);
    };
    retry_path(service, runtime, path).map(Some)
}
pub(super) fn retry_path(
    service: &Service,
    runtime: &tokio::runtime::Handle,
    path: PathBuf,
) -> Result<bool> {
    let mut record = open_standalone(service, path)?;
    let session = standalone_session(&record.value);
    let target = record.value.target.as_ref().ok_or(Error::Unavailable)?;
    target.validate_session(&session.user_session)?;
    ensure!(
        target.matches_homeserver(&record.value.identity.homeserver),
        "credential_recovery_integrity_failed"
    );
    if record.value.progress.confirmed_for(&session.user_session) {
        return Ok(true);
    }
    let progress = runtime.block_on(revoke_transport::revoke(
        target,
        &session.user_session,
        record.value.progress.clone(),
    ))?;
    let complete = progress.confirmed_for(&session.user_session);
    record.value.progress = progress;
    let doc = Document::open(&record.path, MAX_SESSION)?;
    doc.check_original(Some(&record.source))?;
    record.outer.envelope = record.key.seal(&record.value)?;
    let encoded = serde_json::to_vec(&record.outer)?;
    let journal = doc.begin(
        record.key.reference().scope(),
        Some(&record.source),
        vec![record.outer.revocation_key],
    )?;
    let prepared = doc.prepare(journal, &encoded)?;
    ensure!(
        doc.publish(prepared, Some(&record.source), || true)? == Published::Durable,
        "credential_commit_durability_unknown"
    );
    Ok(complete)
}

/// Only an authenticated document for this exact grant can participate in
/// retirement. A later grant's canonical file is never a token source for the
/// old owner and is never removed by this operation.
fn original_session(keys: &Unlocked, bytes: &[u8]) -> Result<Option<(u64, FullSessionPersisted)>> {
    #[derive(Deserialize)]
    struct Format {
        version: Option<u32>,
    }
    if serde_json::from_slice::<Format>(bytes)?.version.is_none() {
        // A legacy plaintext source can still exist before candidate rename.
        // It cannot participate as authenticated grant material. Still reject
        // unknown or malformed schemas rather than silently treating them as v1.
        let _: FullSessionPersisted = serde_json::from_slice(bytes)?;
        return Ok(None);
    }
    let stored: Stored = serde_json::from_slice(bytes)?;
    ensure!(stored.version == VERSION, "credential_schema_unsupported");
    if stored.authentication.reference() != keys.key.reference() {
        return Ok(None);
    }
    let authenticated: Authentication = keys.key.open(&stored.authentication)?;
    let revision = authenticated.revision;
    let session = keys.decode(stored)?;
    Ok(Some((revision, session)))
}

/// Retire a successfully prepared but unobserved grant without reopening any
/// native credential. A real SDK snapshot wins; otherwise select the latest
/// authenticated canonical, candidate, Keep or Revoke for the original grant.
pub(super) fn retire_preparation(owner: &Arc<owners::Owner>) -> Result<()> {
    let keys = owner.keys.get().ok_or(Error::Unavailable)?;
    owner.state.store(owners::REVOKING, Ordering::Release);
    keys.logout.store(true, Ordering::Release);
    if let Some(client) = owner.sdk() {
        owner.capture(&client)?;
    }
    let path = owner.path()?;
    let doc = Document::open(&path, MAX_SESSION)?;
    let source = doc.read()?;
    let mut latest = source
        .as_ref()
        .map(|source| original_session(keys, source.bytes()))
        .transpose()?
        .flatten()
        .map(|(revision, session)| (revision, session, false));
    let journal = doc.pending(keys.key.reference().scope())?;
    if let Some(journal) = journal.as_ref() {
        if journal.candidate_digest().is_some()
            && journal.candidate_digest() == source.as_ref().map(Snapshot::digest)
        {
            // original_session already authenticated the canonical bytes. A
            // different grant cannot finish this original grant's journal.
            ensure!(latest.is_some(), "credential_recovery_identity_invalid");
            doc.finish_authenticated(
                journal,
                &[
                    keys.key.reference().clone(),
                    keys.database.reference.clone(),
                    keys.revocation.reference().clone(),
                ],
            )?;
        } else if journal.candidate_digest().is_some() {
            let (_, bytes) = doc.resume_candidate(journal)?;
            let (revision, session) = original_session(keys, &bytes)?.ok_or(Error::Integrity)?;
            if latest
                .as_ref()
                .is_none_or(|(current, _, _)| revision > *current)
            {
                latest = Some((revision, session, false));
            }
        }
    }
    if owner.dirty() {
        owner.flush()?;
    } else {
        for intent in [Intent::Keep, Intent::Revoke] {
            let deferred = Document::open(&self::path(keys, intent)?, MAX_SESSION)?;
            recover_original(&deferred, keys, intent)?;
            if deferred
                .pending(key(keys, intent).reference().scope())?
                .is_some()
            {
                return Err(Error::DurabilityUnknown.into());
            }
            if let Some(snapshot) = deferred.read()? {
                let value = authenticate(keys, snapshot.bytes(), intent)?;
                let revision = value.revision;
                if latest
                    .as_ref()
                    .is_none_or(|(current, _, _)| revision > *current)
                {
                    latest = Some((
                        revision,
                        recovery_session(keys, value),
                        intent == Intent::Revoke,
                    ));
                }
                keys.revision.fetch_max(revision, Ordering::AcqRel);
            }
        }
        let (_, session, already_revoking) = latest.ok_or(Error::DurabilityUnknown)?;
        if !already_revoking {
            write(keys, &session, Intent::Revoke)?;
        }
    }
    ensure!(revoke_pending(keys)?, "credential_revocation_not_durable");
    if let Some(journal) = doc.pending(keys.key.reference().scope())? {
        doc.discard_unpublished(
            &journal,
            source.as_ref(),
            &[
                keys.key.reference().clone(),
                keys.database.reference.clone(),
                keys.revocation.reference().clone(),
            ],
            |bytes| {
                keys.decode(serde_json::from_slice(bytes).map_err(|_| Error::Invalid)?)
                    .map(|_| ())
                    .map_err(classify)
            },
        )?;
    }
    owner.cleanup_pending.store(false, Ordering::Release);
    owners::remove_if_clean(owner)?;
    Ok(())
}

/// Native preparation may fail after an SDK token already exists. Its admitted
/// RAM owner can revoke through the previously captured target without starting
/// another native prompt for the cancelled login. Unknown remote outcomes retain
/// the actual pair and its slot; this is not a claim of crash durability.
pub(super) fn retry_ram_one(
    service: &Service,
    runtime: &tokio::runtime::Handle,
) -> Result<Option<bool>> {
    let candidates: Vec<_> = owners::pending()?
        .into_iter()
        .filter(|owner| {
            std::ptr::eq(owner.service.as_ref(), service)
                && owner.state.load(Ordering::Acquire) == owners::REVOKING
                && !owner.observing.load(Ordering::Acquire)
                && !owner.preparing.load(Ordering::Acquire)
        })
        .collect();
    if candidates.is_empty() {
        return Ok(None);
    }
    let index = service.ram_cursor.fetch_add(1, Ordering::AcqRel) % candidates.len();
    let owner = &candidates[index];
    if let Some(keys) = owner.keys.get() {
        {
            let _guard = super::super::SESSION_IO
                .lock()
                .map_err(|_| Error::Unavailable)?;
            retire_preparation(owner)?;
        }
        return retry_path(service, runtime, path(keys, Intent::Revoke)?).map(Some);
    }
    let Some(update) = owner.update()? else {
        owners::remove_if_clean(owner)?;
        return Ok(Some(true));
    };
    let target = owner.target().ok_or(Error::Unavailable)?;
    let prior = owner
        .ram_progress
        .lock()
        .map_err(|_| Error::Unavailable)?
        .clone();
    let progress = runtime.block_on(revoke_transport::revoke(&target, &update.user, prior))?;
    let complete = progress.confirmed_for(&update.user);
    *owner.ram_progress.lock().map_err(|_| Error::Unavailable)? = progress;
    if complete {
        owner.clear(update.revision)?;
    }
    Ok(Some(complete))
}

#[cfg(test)]
pub(super) fn test_revoke_snapshot(
    service: &Service,
    path: PathBuf,
) -> Result<PersistedUserSession> {
    Ok(open_standalone(service, path)?.value.user_session)
}

/// SDK activation can durably save a fresher pair in Keep before it is allowed
/// to publish canonical authority. Reload must not resurrect the older pair.
pub(super) fn reload_keep(
    keys: &Unlocked,
    canonical_revision: u64,
    session: FullSessionPersisted,
) -> Result<FullSessionPersisted> {
    let doc = Document::open(&path(keys, Intent::Keep)?, MAX_SESSION)?;
    recover_original(&doc, keys, Intent::Keep)?;
    if doc.pending(keys.key.reference().scope())?.is_some() {
        return Err(Error::DurabilityUnknown.into());
    }
    let Some(source) = doc.read()? else {
        return Ok(session);
    };
    let value = authenticate(keys, source.bytes(), Intent::Keep)?;
    if value.revision <= canonical_revision {
        return Ok(session);
    }
    keys.revision.fetch_max(value.revision, Ordering::AcqRel);
    Ok(recovery_session(keys, value))
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_migration_retirement_uses_published_pair_after_real_sdk_drops() {
        const NAME: &str = "persistence::credential_state::recovery::retirement_tests::credential_migration_retirement_uses_published_pair_after_real_sdk_drops";
        if super::super::tests::isolated_case(NAME, "HAGENCY_CANONICAL_RETIRE_CHILD") {
            return;
        }
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token":"published-access-C", "refresh_token":"published-refresh-C"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("private-profile");
        hagency_store::private::create_directory_new(&root).unwrap();
        let service = super::super::tests::fake_service(&root);
        let mut old = super::super::tests::session_fixture();
        old.client_session.homeserver = server.uri();
        let account = crate::accounts::account_dir(&root, &old.user_session.meta().user_id);
        hagency_store::private::directory(&account).unwrap();
        let state = account.join("persistent_state");
        hagency_store::private::directory(&state).unwrap();
        let path = state.join("session");
        hagency_store::private::write_new(&path, &serde_json::to_vec(&old).unwrap()).unwrap();
        service
            .load_worker(&path, crate::account_session::epoch(), || true)
            .unwrap();
        let keys = service
            .cached(&path, crate::account_session::epoch())
            .unwrap();
        let client = matrix_sdk::Client::builder()
            .homeserver_url(server.uri())
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
            .build()
            .await
            .unwrap();
        let owner =
            install_owner(service.clone(), &client, old.client_session.clone(), None).unwrap();
        client
            .restore_session(old.user_session.clone().into_sdk())
            .await
            .unwrap();
        owner
            .set_target(revoke_transport::capture_target(&client).await.unwrap())
            .unwrap();
        owner.set_keys(keys.clone()).unwrap();
        owner.state.store(owners::PUBLISHED, Ordering::Release);
        service.bind_owner(&path, &owner).unwrap();
        // The original preparation Keep exists before the actual SDK rotation.
        write(&keys, &old, Intent::Keep).unwrap();
        client.matrix_auth().refresh_access_token().await.unwrap();
        assert!(!owner.dirty());
        let canonical = std::fs::read(&path).unwrap();
        {
            let document = Document::open(&path, MAX_SESSION).unwrap();
            assert!(
                document
                    .pending(keys.key.reference().scope())
                    .unwrap()
                    .is_none(),
                "successful publication has already removed its journal"
            );
            let current = keys
                .decode(serde_json::from_slice(&canonical).unwrap())
                .unwrap();
            assert!(
                matches!(current.user_session, PersistedUserSession::Matrix(value) if value.tokens.access_token == "published-access-C")
            );
        }
        let weak = client.downgrade();
        drop(client);
        assert!(weak.get().is_none());
        assert!(
            !owner.callbacks_alive(),
            "the actual SDK callback family has dropped"
        );
        assert!(owner.sdk().is_none());
        {
            let _guard = super::super::super::SESSION_IO.lock().unwrap();
            retire_preparation(&owner).unwrap();
        }
        let revoked =
            test_revoke_snapshot(&service, super::path(&keys, Intent::Revoke).unwrap()).unwrap();
        assert!(
            matches!(revoked, PersistedUserSession::Matrix(value)
            if value.tokens.access_token == "published-access-C" && value.tokens.refresh_token.as_deref() == Some("published-refresh-C")),
            "retirement must not replace the published pair C with the older Keep B"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            canonical,
            "retirement does not remove or rewrite canonical authority"
        );
        assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_migration_maintenance_alternates_pending_ram_and_durable_disk_receipt() {
        const NAME: &str = "persistence::credential_state::recovery::retirement_tests::credential_migration_maintenance_alternates_pending_ram_and_durable_disk_receipt";
        if super::super::tests::isolated_case(NAME, "HAGENCY_MAINTENANCE_FAIR_CHILD") {
            return;
        }
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path, header},
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/logout"))
            .and(header("authorization", "Bearer RAM_ACCESS"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/logout"))
            .and(header("authorization", "Bearer DISK_ACCESS"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("private-profile");
        hagency_store::private::create_directory_new(&root).unwrap();
        let service = super::super::tests::fake_service(&root);
        let mut disk = super::super::tests::session_fixture();
        disk.client_session.homeserver = server.uri();
        let PersistedUserSession::Matrix(user) = &mut disk.user_session else {
            unreachable!()
        };
        user.tokens.access_token = "DISK_ACCESS".into();
        user.tokens.refresh_token = Some("DISK_REFRESH".into());
        let account = crate::accounts::account_dir(&root, &user.meta.user_id);
        hagency_store::private::directory(&account).unwrap();
        let state = account.join("persistent_state");
        hagency_store::private::directory(&state).unwrap();
        let canonical = state.join("session");
        hagency_store::private::write_new(&canonical, &serde_json::to_vec(&disk).unwrap()).unwrap();
        service
            .load_worker(&canonical, crate::account_session::epoch(), || true)
            .unwrap();
        let keys = service
            .cached(&canonical, crate::account_session::epoch())
            .unwrap();
        let disk_client = matrix_sdk::Client::builder()
            .homeserver_url(server.uri())
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
            .build()
            .await
            .unwrap();
        disk_client
            .restore_session(disk.user_session.clone().into_sdk())
            .await
            .unwrap();
        assert!(
            keys.target
                .set(
                    revoke_transport::capture_target(&disk_client)
                        .await
                        .unwrap()
                )
                .is_ok()
        );
        write(&keys, &disk, Intent::Revoke).unwrap();
        let disk_path = super::path(&keys, Intent::Revoke).unwrap();
        drop(disk_client);
        let mut ram = disk.clone();
        let PersistedUserSession::Matrix(user) = &mut ram.user_session else {
            unreachable!()
        };
        user.meta.user_id = matrix_sdk::ruma::user_id!("@ram:example.org").into();
        user.tokens.access_token = "RAM_ACCESS".into();
        user.tokens.refresh_token = Some("RAM_REFRESH".into());
        let ram_client = matrix_sdk::Client::builder()
            .homeserver_url(server.uri())
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
            .build()
            .await
            .unwrap();
        let owner = install_owner(
            service.clone(),
            &ram_client,
            ram.client_session.clone(),
            None,
        )
        .unwrap();
        ram_client
            .restore_session(ram.user_session.clone().into_sdk())
            .await
            .unwrap();
        owner
            .set_target(revoke_transport::capture_target(&ram_client).await.unwrap())
            .unwrap();
        owner.capture(&ram_client).unwrap();
        owner.state.store(owners::REVOKING, Ordering::Release);
        assert!(owner.keys.get().is_none());
        let runtime = tokio::runtime::Handle::current();
        let original = service.clone();
        let handle = runtime.clone();
        assert!(
            !service
                .worker
                .call(move || original.maintenance_once(&handle).map_err(classify))
                .await
                .unwrap(),
            "RAM 503 retains an unconfirmed original pair"
        );
        assert!(owner.dirty());
        let original = service.clone();
        let handle = runtime.clone();
        assert!(
            service
                .worker
                .call(move || original.maintenance_once(&handle).map_err(classify))
                .await
                .unwrap(),
            "the next original turn must attempt the healthy on-disk record"
        );
        let record = open_standalone(&service, disk_path).unwrap();
        assert!(
            record.value.progress.confirmed_for(&disk.user_session),
            "actual disk HTTP success must have a durable authenticated receipt"
        );
        assert!(owner.dirty());
        assert!(
            owners::pending()
                .unwrap()
                .iter()
                .any(|value| Arc::ptr_eq(value, &owner)),
            "unconfirmed RAM still owns its finite snapshot slot"
        );
        assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
    }
}
