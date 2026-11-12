//! Handles app persistence by saving and restoring client session data to/from the filesystem.

use std::path::{Path, PathBuf};
use anyhow::{anyhow, bail};
use makepad_widgets::{log, Cx};
use matrix_sdk::{
    authentication::{
        matrix::MatrixSession,
        oauth::{OAuthSession, UserSession},
        AuthSession,
    },
    SessionMeta,
    ruma::{OwnedUserId, UserId},
    sliding_sync, Client,
};
use serde::{Deserialize, Serialize};

use crate::{app_data_dir, login::login_screen::LoginAction};

/// The data needed to re-build a client.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSessionPersisted {
    /// The URL of the homeserver of the user.
    pub homeserver: String,

    /// The database path. New sessions store this as a relative subfolder
    /// (joined with `app_data_dir()` at restore time); legacy sessions
    /// may have an absolute path. `restore_session` handles both.
    pub db_path: PathBuf,

    /// The passphrase of the database.
    pub passphrase: String,
}

impl std::fmt::Debug for ClientSessionPersisted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientSessionPersisted")
            .field("homeserver", &self.homeserver)
            .field("db_path", &self.db_path)
            .field("passphrase", &"<REDACTED>")
            .finish()
    }
}

/// Persist the SDK authentication API as well as its exact device identity.
/// OAuth refresh credentials have one owner: the SDK, never NativeOwner.
#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum PersistedUserSession {
    OAuth {
        oauth_client_id: String,
        #[serde(flatten)]
        user: UserSession,
    },
    Matrix(MatrixSession),
}
impl<'de> Deserialize<'de> for PersistedUserSession {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        if let Some(id) = value.get("oauth_client_id") {
            let id = id
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 255)
                .ok_or_else(|| serde::de::Error::custom("Invalid OAuth client identity"))?
                .to_owned();
            let user = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            Ok(Self::OAuth {
                oauth_client_id: id,
                user,
            })
        } else {
            serde_json::from_value(value)
                .map(Self::Matrix)
                .map_err(serde::de::Error::custom)
        }
    }
}

impl std::fmt::Debug for PersistedUserSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistedUserSession")
            .field("meta", self.meta())
            .finish_non_exhaustive()
    }
}
impl PersistedUserSession {
    pub fn meta(&self) -> &SessionMeta {
        match self {
            Self::OAuth { user, .. } => &user.meta,
            Self::Matrix(s) => &s.meta,
        }
    }
    pub fn from_sdk(session: AuthSession) -> anyhow::Result<Self> {
        Ok(match session {
            AuthSession::Matrix(s) => Self::Matrix(s),
            AuthSession::OAuth(s) => Self::OAuth {
                oauth_client_id: s.client_id.as_str().to_owned(),
                user: s.user,
            },
            _ => bail!("Unsupported SDK authentication API"),
        })
    }
    pub(crate) fn into_sdk(self) -> AuthSession {
        match self {
            Self::Matrix(s) => AuthSession::Matrix(s),
            Self::OAuth {
                oauth_client_id,
                user,
            } => AuthSession::OAuth(Box::new(OAuthSession {
                client_id: matrix_sdk::authentication::oauth::ClientId::new(oauth_client_id),
                user,
            })),
        }
    }
}

/// The full session to persist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FullSessionPersisted {
    /// The data to re-build the client.
    pub client_session: ClientSessionPersisted,

    /// The Matrix user session.
    pub user_session: PersistedUserSession,

    /// The latest sync token.
    ///
    /// It is only needed to persist it when using `Client::sync_once()` and we
    /// want to make our syncs faster by not receiving all the initial sync
    /// again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_token: Option<String>,

    /// The sliding sync version to use for this client session.
    ///
    /// This determines the sync protocol used by the Matrix client:
    /// - `Native`: Uses the server's native sliding sync implementation for efficient syncing
    /// - `None`: Falls back to standard Matrix sync (without sliding sync optimizations)
    ///
    /// The value is restored and applied to the client via `client.set_sliding_sync_version()`
    /// when rebuilding the session from persistent storage.
    #[serde(default)]
    pub sliding_sync_version: SlidingSyncVersion,
}

/// A serializable duplicate of [`sliding_sync::Version`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum SlidingSyncVersion {
    #[default]
    Native,
    None,
}
impl From<SlidingSyncVersion> for sliding_sync::Version {
    fn from(version: SlidingSyncVersion) -> Self {
        match version {
            SlidingSyncVersion::None => sliding_sync::Version::None,
            SlidingSyncVersion::Native => sliding_sync::Version::Native,
        }
    }
}
impl From<sliding_sync::Version> for SlidingSyncVersion {
    fn from(version: sliding_sync::Version) -> Self {
        match version {
            sliding_sync::Version::None => SlidingSyncVersion::None,
            sliding_sync::Version::Native => SlidingSyncVersion::Native,
        }
    }
}

/// Returns a collision-free directory keyed by the complete Matrix user ID.
/// Legacy directories are validated and migrated by `accounts::refresh`.
pub fn persistent_state_dir(user_id: &UserId) -> PathBuf {
    crate::accounts::account_dir(app_data_dir(), user_id).join("persistent_state")
}

/// Returns the path to the session file for the given user.
pub fn session_file_path(user_id: &UserId) -> PathBuf {
    persistent_state_dir(user_id).join("session")
}

const LATEST_USER_ID_FILE_NAME: &str = "latest_user_id.txt";

/// Returns the user ID of the most recently-logged in user session.
pub async fn most_recent_user_id() -> Option<OwnedUserId> {
    tokio::fs::read_to_string(app_data_dir().join(LATEST_USER_ID_FILE_NAME))
        .await
        .ok()?
        .trim()
        .try_into()
        .ok()
}

/// Resolves the path that `restore_session()` would actually open.
pub(crate) fn resolve_db_path(stored: PathBuf) -> PathBuf {
    if !stored.is_absolute() {
        return app_data_dir().join(stored);
    }
    if stored.exists() {
        return stored;
    }
    let Some(name) = stored.file_name() else {
        return stored;
    };
    // iOS sandbox UUID changes across reinstalls; the absolute path
    // baked into the session is now stale. Use the basename instead.
    app_data_dir().join(name)
}

/// Returns the set of `db` paths referenced by any saved session file.
///
/// Scan every saved user session directory, not only the most recent one.
///
/// Diagnostic inventory only; even a complete result never authorizes deletion.
/// This explicit diagnostic may read scoped native credentials. Startup cleanup
/// does not invoke it or request native unlocks.
/// Return `None` when any part of inventory cannot be completed.
pub async fn collect_referenced_db_paths() -> Option<std::collections::HashSet<PathBuf>> {
    let root = app_data_dir();
    let directory = match tokio::fs::read_dir(root).await {
        Ok(directory) => directory,
        Err(error) => {
            log!("Database inventory incomplete: {error}");
            return None;
        }
    };
    let entries = futures_util::stream::try_unfold(directory, |mut directory| async move {
        let entry = directory.next_entry().await?;
        Ok::<_, std::io::Error>(entry.map(|entry| (entry.path(), directory)))
    });
    use futures_util::TryStreamExt;
    let result = inventory_entries(
        root,
        entries.map_err(anyhow::Error::from),
        |path, retained| async move {
            if retained {
                super::credential_state::retained_paths(path).await
            } else {
                Ok(vec![
                    super::credential_state::existing_database(path)
                        .await?
                        .client_session
                        .db_path,
                ])
            }
        },
    )
    .await;
    match result {
        Ok(paths) => Some(paths),
        Err(error) => {
            log!("Database inventory incomplete; no directories can be pruned: {error}");
            None
        }
    }
}

/// A complete inventory is the only input accepted by GC. The production stream
/// retains real read_dir errors; isolated tests can inject an error after valid
/// entries without replacing the credential/file readers.
pub(super) async fn inventory_entries<S, F, Fut>(
    root: &Path,
    entries: S,
    mut references: F,
) -> anyhow::Result<std::collections::HashSet<PathBuf>>
where
    S: futures_util::Stream<Item = anyhow::Result<PathBuf>> + Send,
    F: FnMut(PathBuf, bool) -> Fut + Send,
    Fut: std::future::Future<Output = anyhow::Result<Vec<PathBuf>>> + Send,
{
    use futures_util::StreamExt;
    let mut paths = std::collections::HashSet::new();
    futures_util::pin_mut!(entries);
    while let Some(entry) = entries.next().await {
        let path = entry?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow!("Database inventory contains an unknown path"))?;
        let metadata = tokio::fs::symlink_metadata(&path).await?;
        if name.starts_with("db_") {
            continue;
        }
        anyhow::ensure!(
            !metadata.file_type().is_symlink(),
            "Database inventory contains a linked account path"
        );
        if !metadata.is_dir() {
            continue;
        }
        let state = path.join("persistent_state");
        match tokio::fs::symlink_metadata(&state).await {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
            Ok(metadata) => anyhow::ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "Database inventory contains an invalid state directory"
            ),
        }
        for (name, retained) in [("retained_db.json", true), ("session", false)] {
            let document = state.join(name);
            match tokio::fs::symlink_metadata(&document).await {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match tokio::fs::symlink_metadata(state.join(format!(".{name}.vault-journal")))
                        .await
                    {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                        Ok(_) => anyhow::bail!(
                            "Database inventory contains an unfinished credential transaction"
                        ),
                    }
                }
                Err(error) => return Err(error.into()),
                Ok(metadata) => anyhow::ensure!(
                    metadata.is_file() && !metadata.file_type().is_symlink(),
                    "Database inventory contains an invalid credential file"
                ),
            }
            for stored in references(document, retained).await? {
                let resolved = if !stored.is_absolute() {
                    root.join(stored)
                } else {
                    match tokio::fs::symlink_metadata(&stored).await {
                        Ok(_) => stored,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => root.join(
                            stored
                                .file_name()
                                .ok_or_else(|| anyhow!("Invalid database path"))?,
                        ),
                        Err(error) => return Err(error.into()),
                    }
                };
                paths.insert(resolved);
            }
        }
    }
    Ok(paths)
}

/// Test-only preservation of the original pruning algorithm. A diagnostic
/// inventory snapshot cannot authorize production deletion across creators.
/// Deletes old database files that start with `"db_"` within the given `dir` and its subdirectories.
///
/// Only deletes database files that are inactive, i.e., where `is_active` returns false.
#[cfg(test)]
async fn prune_orphan_db_dirs(dir: &std::path::Path, is_active: impl Fn(&std::ffi::OsStr) -> bool) {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            log!(
                "prune_orphan_db_dirs: could not read {}: {e}",
                dir.display()
            );
            return;
        }
    };

    let mut deleted: usize = 0;
    let mut bytes_freed: u64 = 0;
    let mut kept: usize = 0;

    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !name_str.starts_with("db_") {
            continue;
        }
        if is_active(name) {
            kept += 1;
            continue;
        }
        let size = dir_size_bytes(&path).await.unwrap_or(0);
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => {
                deleted += 1;
                bytes_freed += size;
                // log!(
                //     "prune_orphan_db_dirs: deleted orphaned dir ({size} bytes): {}",
                //     path.display(),
                // );
            }
            Err(e) => {
                log!(
                    "prune_orphan_db_dirs: failed to delete {}: {e}",
                    path.display()
                );
            }
        }
    }

    if deleted > 0 || kept > 0 {
        log!(
            "prune_orphan_db_dirs ({}): deleted {deleted} orphan(s), freed {bytes_freed} bytes; kept {kept} active",
            dir.display()
        );
    }
}

/// Defer automatic database/cache deletion until inventory and every database
/// creator participate in one cross-process protocol. A complete diagnostic
/// snapshot alone does not prove that a newly created database is orphaned.
/// This startup entry performs no inventory/native credential IO or deletion.
pub async fn cleanup_orphan_db_dirs() {
    log!(
        "Automatic database cleanup deferred: cross-process inventory and database creation \
         are not serialized; no database or cache directories deleted"
    );
}

#[cfg(test)]
pub(super) async fn cleanup_with_inventory(
    data_dir: &Path,
    cache: &Path,
    inventory: impl std::future::Future<Output = Option<std::collections::HashSet<PathBuf>>>,
) {
    use std::collections::HashSet;
    use std::ffi::OsString;

    // If we couldn't read the data directory, we can't know which ones are active, so skip pruning.
    let Some(active) = inventory.await else {
        return;
    };
    let active_names: HashSet<OsString> = active
        .iter()
        .filter_map(|p| p.file_name().map(ToOwned::to_owned))
        .collect();

    prune_orphan_db_dirs(data_dir, |name| active.contains(&data_dir.join(name))).await;
    prune_orphan_db_dirs(cache, |name| active_names.contains(name)).await;
}

/// Recursive size sum, best-effort. Just for the test-only cleanup log line.
#[cfg(test)]
async fn dir_size_bytes(path: &std::path::Path) -> Option<u64> {
    let mut total = 0u64;
    let mut entries = tokio::fs::read_dir(path).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(md) = entry.metadata().await else {
            continue;
        };
        if md.is_file() {
            total = total.saturating_add(md.len());
        } else if md.is_dir() {
            // matrix-sdk-sqlite doesn't nest subdirectories, but be safe.
            if let Some(sub) = Box::pin(dir_size_bytes(&entry.path())).await {
                total = total.saturating_add(sub);
            }
        }
    }
    Some(total)
}

/// Save which user was the most recently logged in.
async fn save_latest_user_id(user_id: &UserId) -> anyhow::Result<()> {
    tokio::fs::write(
        app_data_dir().join(LATEST_USER_ID_FILE_NAME),
        user_id.as_str(),
    )
    .await?;
    Ok(())
}

/// Restores the given user's previous session from the filesystem.
///
/// If no User ID is specified, the ID of the most recently-logged in user
/// is retrieved from the filesystem.
pub async fn restore_session(
    user_id: Option<OwnedUserId>,
) -> anyhow::Result<(Client, Option<String>)> {
    restore_session_inner(user_id, true).await
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn restore_session_for_login(
    user_id: OwnedUserId,
) -> anyhow::Result<(Client, Option<String>)> {
    restore_session_inner(Some(user_id), false).await
}
async fn restore_session_inner(
    user_id: Option<OwnedUserId>,
    callbacks: bool,
) -> anyhow::Result<(Client, Option<String>)> {
    let user_id = if let Some(user_id) = user_id {
        Some(user_id)
    } else {
        most_recent_user_id().await
    };

    let Some(user_id) = user_id else {
        log!("Could not find previous latest User ID");
        bail!("Could not find previous latest User ID");
    };
    let session_file = session_file_path(&user_id);
    if !session_file.exists() {
        log!("Could not find previous session file for user {user_id}");
        bail!("Could not find previous session file");
    }
    let status_str = format!("Loading previous session file for {user_id}...");
    log!("{status_str}: '{}'", session_file.display());
    Cx::post_action(LoginAction::Status {
        title: "Restoring session".into(),
        status: status_str,
    });

    // The session was serialized as JSON in a file.
    let loaded_session = super::credential_state::load(session_file).await?;
    let FullSessionPersisted {
        client_session,
        user_session,
        sync_token,
        sliding_sync_version,
    } = loaded_session;

    if user_session.meta().user_id != user_id {
        bail!("Saved session belongs to another account");
    }

    let status_str = format!(
        "Loaded session file for:\n{user_id}\n\nTrying to connect to homeserver...\n{}",
        client_session.homeserver,
    );
    log!("{status_str}");
    Cx::post_action(LoginAction::Status {
        title: "Connecting to homeserver".into(),
        status: status_str,
    });
    let original_stored = client_session.db_path.clone();
    let db_path = resolve_db_path(client_session.db_path);
    if db_path != original_stored {
        log!(
            "Stored db_path '{}' relocated to '{}'",
            original_stored.display(),
            db_path.display(),
        );
    }
    log!(
        "Restoring session for {user_id} with db at: {} (stored as: {})",
        db_path.display(),
        original_stored.display(),
    );
    let client = crate::sliding_sync::base_client_builder(&db_path, &client_session.passphrase)
        .homeserver_url(client_session.homeserver)
        .build()
        .await?;
    crate::login::homeserver::login_methods(&client).await?;
    if !matches!(&user_session, PersistedUserSession::OAuth { .. }) {
        bail!("Sign in again through Pasion on your Hagency server.");
    }
    client.set_sliding_sync_version(sliding_sync_version.into());
    let status_str = format!(
        "Authenticating previous login session for {}...",
        user_session.meta().user_id
    );
    log!("{status_str}");
    Cx::post_action(LoginAction::Status {
        title: "Authenticating session".into(),
        status: status_str,
    });

    // Hold the original grant lease before activation can spawn SDK work.
    super::credential_state::claim_restoration(&client, &session_file_path(&user_id)).await?;
    // Restore the Matrix user session.
    client.restore_session(user_session.into_sdk()).await?;
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    super::credential_state::capture_restored_target(&client).await?;
    save_latest_user_id(&user_id).await?;
    if callbacks {
        install_session_callbacks(&client, session_file_path(&user_id))?;
    }

    Ok((client, sync_token))
}

/// Persist a logged-in client session to the filesystem for later use.
///
/// Note that we could also build the user session from the login response.
pub async fn save_session(
    client: &Client,
    client_session: ClientSessionPersisted,
) -> anyhow::Result<()> {
    let user_session = PersistedUserSession::from_sdk(
        client
            .session()
            .ok_or_else(|| anyhow!("A logged-in client should have a session"))?,
    )?;

    let user_id = user_session.meta().user_id.clone();
    let sliding_sync_version = client.sliding_sync_version().into();
    // Save that user's session.
    let session_file = session_file_path(&user_session.meta().user_id);
    if let Some(parent) = session_file.parent() {
        provision_session_parent(parent)?;
    }
    super::credential_state::save(
        client,
        FullSessionPersisted {
            client_session,
            user_session,
            sync_token: None,
            sliding_sync_version,
        },
        crate::account_session::epoch(),
        session_file.clone(),
    )
    .await?;
    save_latest_user_id(&user_id).await?;

    crate::accounts::refresh()?;
    log!("Session persisted to: {}", session_file.display());
    Ok(())
}

/// Remove the LATEST_USER_ID_FILE_NAME file if it exists
///
/// Returns:
/// - Ok(true) if file was found and deleted
/// - Ok(false) if file didn't exist
/// - Err if deletion failed
pub async fn delete_latest_user_id() -> anyhow::Result<bool> {
    let last_login_path = app_data_dir().join(LATEST_USER_ID_FILE_NAME);

    if last_login_path.exists() {
        tokio::fs::remove_file(&last_login_path)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to remove latest user file: {e}"))
            .map(|_| true)
    } else {
        Ok(false)
    }
}

/// Serializes synchronous SDK refresh persistence against account removal.
pub(crate) static SESSION_IO: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub(super) fn same_stored_session(client: &Client, stored: &FullSessionPersisted) -> bool {
    if client.session_meta() != Some(stored.user_session.meta())
        || client.homeserver().as_str().trim_end_matches('/')
            != stored.client_session.homeserver.trim_end_matches('/')
    {
        return false;
    }
    match (&stored.user_session, client.auth_api()) {
        (
            PersistedUserSession::OAuth {
                oauth_client_id, ..
            },
            Some(matrix_sdk::authentication::AuthApi::OAuth(api)),
        ) => api
            .client_id()
            .is_some_and(|id| id.as_str() == oauth_client_id),
        (PersistedUserSession::Matrix(_), Some(matrix_sdk::authentication::AuthApi::Matrix(_))) => {
            true
        }
        _ => false,
    }
}

pub(crate) fn install_session_callbacks(client: &Client, path: PathBuf) -> anyhow::Result<()> {
    super::credential_state::install_cached(client, path)
}

fn provision_session_parent(parent: &std::path::Path) -> anyhow::Result<()> {
    if let Some(account) = parent.parent() {
        if !account.exists() {
            hagency_store::private::create_directory_new(account)?;
        }
        hagency_store::private::directory(account)?;
    }
    hagency_store::private::directory(parent)?;
    Ok(())
}

/// Native key preparation runs on the owned credential worker; final publication
/// still holds the original epoch/SDK fence without an await inside that fence.
pub(crate) async fn save_oauth_session(
    client: &Client,
    client_session: ClientSessionPersisted,
    epoch: u64,
) -> anyhow::Result<()> {
    let user = client
        .user_id()
        .ok_or_else(|| anyhow!("Missing OAuth identity"))?;
    let path = session_file_path(user);
    if let Some(parent) = path.parent() {
        provision_session_parent(parent)?;
    }
    let user_session = PersistedUserSession::from_sdk(
        client
            .session()
            .ok_or_else(|| anyhow!("Missing OAuth session"))?,
    )?;
    if !matches!(user_session, PersistedUserSession::OAuth { .. }) {
        bail!("Missing OAuth session");
    }
    super::credential_state::save(
        client,
        FullSessionPersisted {
            client_session,
            user_session,
            sync_token: None,
            sliding_sync_version: client.sliding_sync_version().into(),
        },
        epoch,
        path,
    )
    .await?;
    // Credential commit succeeded. Convenience picker failure must not revoke it.
    let _ = save_latest_user_id(user).await;
    let _ = crate::accounts::refresh();
    Ok(())
}
