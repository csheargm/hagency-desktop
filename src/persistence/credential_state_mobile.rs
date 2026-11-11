//! Compile-time mobile compatibility only. This preserves the pre-existing v1
//! private-file format; desktop native failures can never select this module.
//! It does NOT remediate R05 on mobile or establish mobile keystore qualification.
use super::{ClientSessionPersisted, FullSessionPersisted};
#[cfg(any(target_os = "ios", target_os = "android"))]
use super::PersistedUserSession;
use anyhow::{Result, anyhow, ensure};
#[cfg(any(target_os = "ios", target_os = "android"))]
use matrix_sdk::{Client, ruma::OwnedUserId};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};
#[cfg(any(target_os = "ios", target_os = "android"))]
use std::path::PathBuf;
use zeroize::Zeroizing;
const MAX_BYTES: usize = 1024 * 1024;

#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) struct Identity {
    pub owner: OwnedUserId,
    pub device: String,
    pub homeserver: String,
    pub oauth_client_id: Option<String>,
    pub db_path: PathBuf,
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) struct ExistingDatabase {
    pub identity: Identity,
    pub client_session: ClientSessionPersisted,
}
struct Original {
    file: File,
    bytes: Zeroizing<Vec<u8>>,
    identity: hagency_platform::FileIdentity,
}
fn original(path: &Path) -> Result<Option<Original>> {
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    ensure!(
        file.metadata()?.len() <= MAX_BYTES as u64,
        "Mobile credential file exceeds the supported limit"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    (&file).take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_BYTES,
        "Mobile credential file exceeds the supported limit"
    );
    Ok(Some(Original {
        identity: hagency_platform::file_identity(&file)?,
        file,
        bytes,
    }))
}
fn reject_native_artifacts(path: &Path) -> Result<()> {
    let Some(parent) = path.parent() else {
        return Err(anyhow!("Credential directory missing"));
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("Credential filename invalid"))?;
    let prefix = format!(".{name}.vault-");
    match std::fs::read_dir(parent) {
        Ok(entries) => {
            for entry in entries {
                ensure!(
                    !entry?.file_name().to_string_lossy().starts_with(&prefix),
                    "This credential document uses a desktop native format or an incomplete migration"
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn read_session(path: &Path) -> Result<FullSessionPersisted> {
    reject_native_artifacts(path)?;
    let source = original(path)?.ok_or_else(|| anyhow!("Saved session missing"))?;
    // Strict legacy DTO rejects versioned/native envelopes and unknown fields.
    Ok(serde_json::from_slice(&source.bytes)?)
}
fn ensure_parent(path: &Path) -> Result<()> {
    let state = path
        .parent()
        .ok_or_else(|| anyhow!("Credential directory missing"))?;
    let account = state
        .parent()
        .ok_or_else(|| anyhow!("Account directory missing"))?;
    if let Some(root) = account.parent() {
        std::fs::create_dir_all(root)?;
    }
    hagency_store::private::directory(account)?;
    hagency_store::private::directory(state)?;
    Ok(())
}
fn matches(path: &Path, expected: Option<&Original>) -> Result<()> {
    match (expected, original(path)?) {
        (None, None) => Ok(()),
        (Some(expected), Some(actual))
            if expected.identity == actual.identity
                && expected.bytes.as_slice() == actual.bytes.as_slice()
                && hagency_platform::same_file(&expected.file, &actual.file)? =>
        {
            Ok(())
        }
        _ => Err(anyhow!("Credential file changed during publication")),
    }
}
fn replace_v1(path: &Path, encoded: &[u8], validate: impl Fn(&[u8]) -> Result<()>) -> Result<()> {
    ensure!(
        encoded.len() <= MAX_BYTES,
        "Mobile credential file exceeds the supported limit"
    );
    ensure_parent(path)?;
    reject_native_artifacts(path)?;
    let source = original(path)?;
    if let Some(source) = &source {
        validate(&source.bytes)?;
    }
    validate(encoded)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("Credential directory missing"))?;
    let temporary = parent.join(format!(
        ".mobile-v1-{}.tmp",
        hagency_store::credential_vault::random_id()?
    ));
    let mut file = hagency_store::private::open(&temporary, true)?;
    let identity = hagency_platform::file_identity(&file)?;
    let result = (|| -> Result<()> {
        file.write_all(encoded)?;
        file.sync_all()?;
        // Windows create-only private handles deny delete sharing. Release the
        // completed writer before renaming; keep the original target identity
        // and bytes checked immediately before publication.
        drop(file);
        // A desktop envelope imported while selection/IO was pending cannot be
        // silently replaced by this compile-time compatibility writer.
        reject_native_artifacts(path)?;
        matches(path, source.as_ref())?;
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        if original(&temporary)?.is_some_and(|actual| actual.identity == identity) {
            let _ = std::fs::remove_file(&temporary);
        }
    }
    result
}
fn validate_session(bytes: &[u8]) -> Result<()> {
    let _: FullSessionPersisted = serde_json::from_slice(bytes)?;
    Ok(())
}
fn validate_retained(bytes: &[u8]) -> Result<()> {
    let _: Vec<ClientSessionPersisted> = serde_json::from_slice(bytes)?;
    Ok(())
}
#[cfg(any(target_os = "ios", target_os = "android"))]
fn identity(session: &FullSessionPersisted) -> Identity {
    Identity {
        owner: session.user_session.meta().user_id.clone(),
        device: session.user_session.meta().device_id.to_string(),
        homeserver: session.client_session.homeserver.clone(),
        oauth_client_id: match &session.user_session {
            PersistedUserSession::OAuth {
                oauth_client_id, ..
            } => Some(oauth_client_id.clone()),
            _ => None,
        },
        db_path: session.client_session.db_path.clone(),
    }
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn load(path: PathBuf) -> Result<FullSessionPersisted> {
    tokio::task::spawn_blocking(move || read_session(&path)).await?
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) fn load_unlocked(path: &Path) -> Result<FullSessionPersisted> {
    read_session(path)
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) fn public_identity(bytes: &[u8]) -> Result<Identity> {
    ensure!(bytes.len() <= MAX_BYTES, "Credential file too large");
    Ok(identity(&serde_json::from_slice(bytes)?))
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn existing_database(path: PathBuf) -> Result<ExistingDatabase> {
    let session = load(path).await?;
    Ok(ExistingDatabase {
        identity: identity(&session),
        client_session: session.client_session,
    })
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn claim_restoration(_: &Client, _: &Path) -> Result<()> {
    Ok(())
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) fn retire() {}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) fn shutdown() -> bool {
    true
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) fn refresh_failed(_: &Client, _: u64) -> bool {
    false
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn retry_latest(_: &Client, _: u64) -> Result<()> {
    Ok(())
}

#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) fn install_cached(client: &Client, path: PathBuf) -> Result<()> {
    let epoch = crate::account_session::epoch();
    let read_path = path.clone();
    client.set_session_callbacks(
        Box::new(move |client| {
            let _guard = super::SESSION_IO
                .lock()
                .map_err(|_| std::io::Error::other("Session storage unavailable"))?;
            if crate::account_session::epoch() != epoch {
                return Err(std::io::Error::other("Account changed").into());
            }
            let stored = read_session(&read_path)?;
            if !super::matrix_state::same_stored_session(&client, &stored) {
                return Err(std::io::Error::other("Saved session identity changed").into());
            }
            Ok(match stored.user_session {
                PersistedUserSession::OAuth { user, .. } => user.tokens,
                PersistedUserSession::Matrix(value) => value.tokens,
            })
        }),
        Box::new(move |client| {
            let _guard = super::SESSION_IO
                .lock()
                .map_err(|_| std::io::Error::other("Session storage unavailable"))?;
            if crate::account_session::epoch() != epoch {
                return Err(std::io::Error::other("Account changed").into());
            }
            let mut stored = read_session(&path)?;
            if !super::matrix_state::same_stored_session(&client, &stored) {
                return Err(std::io::Error::other("Saved session identity changed").into());
            }
            stored.user_session = PersistedUserSession::from_sdk(
                client
                    .session()
                    .ok_or_else(|| anyhow!("SDK session missing"))?,
            )?;
            let bytes = Zeroizing::new(serde_json::to_vec(&stored)?);
            replace_v1(&path, &bytes, validate_session)?;
            Ok(())
        }),
    )?;
    Ok(())
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn save(
    client: &Client,
    session: FullSessionPersisted,
    epoch: u64,
    path: PathBuf,
) -> Result<()> {
    install_cached(client, path.clone())?;
    let _guard = super::SESSION_IO
        .lock()
        .map_err(|_| anyhow!("Session storage unavailable"))?;
    ensure!(crate::account_session::epoch() == epoch, "Account changed");
    let bytes = Zeroizing::new(serde_json::to_vec(&session)?);
    replace_v1(&path, &bytes, validate_session)
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn retained_paths(path: PathBuf) -> Result<Vec<PathBuf>> {
    tokio::task::spawn_blocking(move || {
        reject_native_artifacts(&path)?;
        let source = original(&path)?.ok_or_else(|| anyhow!("Recovery file missing"))?;
        let entries: Vec<ClientSessionPersisted> = serde_json::from_slice(&source.bytes)?;
        Ok(entries.into_iter().map(|entry| entry.db_path).collect())
    })
    .await?
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn retain(
    path: PathBuf,
    _: OwnedUserId,
    session: ClientSessionPersisted,
) -> Result<()> {
    let _guard = super::SESSION_IO
        .lock()
        .map_err(|_| anyhow!("Session storage unavailable"))?;
    reject_native_artifacts(&path)?;
    let mut entries: Vec<ClientSessionPersisted> = original(&path)?
        .map(|source| serde_json::from_slice(&source.bytes))
        .transpose()?
        .unwrap_or_default();
    if !entries.iter().any(|entry| entry.db_path == session.db_path) {
        entries.push(session);
    }
    let bytes = Zeroizing::new(serde_json::to_vec(&entries)?);
    replace_v1(&path, &bytes, validate_retained)
}
#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn remove_account(path: PathBuf, owner: OwnedUserId) -> Result<()> {
    reject_native_artifacts(&path)?;
    if original(&path)?.is_none() {
        return Ok(());
    }
    let session = read_session(&path)?;
    ensure!(
        session.user_session.meta().user_id == owner,
        "Saved account identity changed"
    );
    let source = original(&path)?.ok_or_else(|| anyhow!("Saved session missing"))?;
    retain(
        path.with_file_name("retained_db.json"),
        owner,
        session.client_session,
    )
    .await?;
    let _guard = super::SESSION_IO
        .lock()
        .map_err(|_| anyhow!("Session storage unavailable"))?;
    reject_native_artifacts(&path)?;
    matches(&path, Some(&source))?;
    validate_session(
        &original(&path)?
            .ok_or_else(|| anyhow!("Saved session missing"))?
            .bytes,
    )?;
    std::fs::remove_file(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_migration_mobile_v1_compatibility_never_downgrades_native_documents() {
        let root = tempfile::tempdir().unwrap();
        let account = root.path().join("account");
        hagency_store::private::directory(&account).unwrap();
        let state = account.join("persistent_state");
        hagency_store::private::directory(&state).unwrap();
        let path = state.join("session");
        let session: FullSessionPersisted = serde_json::from_value(serde_json::json!({"client_session":{"homeserver":"https://example.org","db_path":"db_original","passphrase":"DB_SECRET_MARKER"},"user_session":{"user_id":"@alice:example.org","device_id":"ORIGINAL","access_token":"ACCESS_SECRET_MARKER"}})).unwrap();
        let bytes = Zeroizing::new(serde_json::to_vec(&session).unwrap());
        replace_v1(&path, &bytes, validate_session).unwrap();
        assert_eq!(
            read_session(&path).unwrap().client_session.passphrase,
            "DB_SECRET_MARKER"
        );
        // Both first publication and rotation must close their temporary
        // writer before rename, including Windows handles without delete sharing.
        let mut refreshed = session.clone();
        if let super::super::PersistedUserSession::Matrix(user) = &mut refreshed.user_session {
            user.tokens.access_token = "REFRESHED_ACCESS_MARKER".into();
        } else {
            panic!("fixture must use a Matrix session");
        }
        let refreshed_bytes = Zeroizing::new(serde_json::to_vec(&refreshed).unwrap());
        replace_v1(&path, &refreshed_bytes, validate_session).unwrap();
        assert_eq!(
            serde_json::to_vec(&read_session(&path).unwrap()).unwrap(),
            *refreshed_bytes
        );
        assert!(std::fs::read_dir(&state).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".mobile-v1-")
        }));
        let native = br#"{"version":2,"authentication":{"ciphertext":"opaque"}}"#;
        hagency_store::private::replace(&path, native).unwrap();
        assert!(read_session(&path).is_err());
        assert!(replace_v1(&path, &bytes, validate_session).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), native);
        let retained = state.join("retained_db.json");
        let retained_bytes =
            Zeroizing::new(serde_json::to_vec(&vec![session.client_session]).unwrap());
        replace_v1(&retained, &retained_bytes, validate_retained).unwrap();
        hagency_store::private::replace(&retained, br#"{"version":2,"databases":[]}"#).unwrap();
        assert!(replace_v1(&retained, &retained_bytes, validate_retained).is_err());
        std::fs::remove_file(&path).unwrap();
        hagency_store::private::write_new(
            &state.join(".session.vault-journal"),
            b"incomplete native intent",
        )
        .unwrap();
        assert!(replace_v1(&path, &bytes, validate_session).is_err());
        assert!(!path.exists());
    }
}

#[cfg(any(target_os = "ios", target_os = "android"))]
pub(crate) async fn register_client(_: &Client, _: ClientSessionPersisted) -> Result<()> {
    Ok(())
}
