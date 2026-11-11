//! A token-free list of saved Matrix identities. Session files remain the source
//! of truth; identity validation prevents a legacy filename collision from
//! restoring another user's session.
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};
use matrix_sdk::ruma::{OwnedUserId, UserId};


#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedAccount {
    pub user_id: OwnedUserId,
    pub homeserver: String,
}
static ACCOUNTS: Mutex<Vec<SavedAccount>> = Mutex::new(Vec::new());

#[derive(Clone, Debug)]
pub enum AccountAction {
    Select(OwnedUserId),
    Add,
    Reauthenticate,
}

pub fn saved() -> Vec<SavedAccount> {
    ACCOUNTS.lock().unwrap().clone()
}
pub(crate) fn account_dir(root: &Path, user: &UserId) -> PathBuf {
    root.join(format!(
        "account_{}",
        blake3::hash(user.as_str().as_bytes()).to_hex()
    ))
}

pub(crate) fn scan(root: &Path) -> anyhow::Result<Vec<SavedAccount>> {
    let mut accounts = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(accounts),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let session_path = path.join("persistent_state/session");
        let bytes = match hagency_store::private::open(&session_path, false) {
            Ok(file) => {
                use std::io::Read;
                if file.metadata()?.len() > 1024 * 1024 { continue; }
                let mut bytes = zeroize::Zeroizing::new(Vec::new());
                file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 1024 * 1024 { continue; }
                bytes
            },
            Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let Ok(session) = crate::persistence::credential_state::public_identity(&bytes) else {
            continue;
        };
        let user = session.owner;
        let legacy = root.join(user.as_str().replace(':', "_").replace('@', ""));
        let canonical = account_dir(root, &user);
        // A manually misplaced session must never grant authority to its path.
        if path != canonical && path != legacy {
            continue;
        }
        if path == legacy {
            if canonical.exists() {
                continue;
            }
            std::fs::rename(&path, &canonical)?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&canonical, std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(
                canonical.join("persistent_state"),
                std::fs::Permissions::from_mode(0o700),
            )?;
            std::fs::set_permissions(
                canonical.join("persistent_state/session"),
                std::fs::Permissions::from_mode(0o600),
            )?;
        }
        if !accounts
            .iter()
            .any(|account: &SavedAccount| account.user_id == user)
        {
            accounts.push(SavedAccount {
                user_id: user,
                homeserver: session.homeserver,
            });
        }
    }
    accounts.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    Ok(accounts)
}
pub(crate) fn refresh() -> anyhow::Result<()> {
    *ACCOUNTS.lock().unwrap() = scan(crate::app_data_dir())?;
    Ok(())
}

pub(crate) async fn remove_session(root: &Path, user: &UserId) -> anyhow::Result<()> {
    let path = account_dir(root, user).join("persistent_state/session");
    crate::persistence::credential_state::remove_account(path, user.to_owned()).await
}

/// Preserve encrypted stores across logout or SSO reauthentication without
/// leaving any access/refresh tokens in the recovery metadata.
pub(crate) async fn retain_database(
    root: &Path,
    user: &UserId,
    session: crate::persistence::ClientSessionPersisted,
) -> anyhow::Result<()> {
    let path = account_dir(root, user).join("persistent_state/retained_db.json");
    crate::persistence::credential_state::retain(path, user.to_owned(), session).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("rinx-accounts-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn session(&self, user: &str, legacy: bool) -> PathBuf {
            let user: OwnedUserId = user.parse().unwrap();
            let dir = if legacy {
                self.0
                    .join(user.as_str().replace(':', "_").replace('@', ""))
            } else {
                account_dir(&self.0, &user)
            };
            let state = dir.join("persistent_state");
            hagency_store::private::directory(&dir).unwrap();
            hagency_store::private::directory(&state).unwrap();
            let value = json!({
                "client_session": {"homeserver":"http://localhost", "db_path":"db_fixture", "passphrase":"fixture-only"},
                "user_session": {"user_id":user, "device_id":"FIXTURE", "access_token":"fixture-only"}
            });
            hagency_store::private::write_new(&state.join("session"), &serde_json::to_vec(&value).unwrap()).unwrap();
            std::fs::write(state.join("latest_app_state.json"), "private fixture state").unwrap();
            dir
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn full_matrix_identity_has_collision_free_storage() {
        let root = Path::new("/fixture");
        let a: OwnedUserId = "@a:example.org:123".parse().unwrap();
        let b: OwnedUserId = "@a_example.org:123".parse().unwrap();
        assert_eq!(
            a.as_str().replace(':', "_").replace('@', ""),
            b.as_str().replace(':', "_").replace('@', "")
        );
        assert_ne!(account_dir(root, &a), account_dir(root, &b));
        let elsewhere: OwnedUserId = "@a_b:elsewhere.test".parse().unwrap();
        assert_ne!(account_dir(root, &a), account_dir(root, &elsewhere));
    }
    #[test]
    fn migration_preserves_sessions_and_app_state_and_is_idempotent() {
        let f = Fixture::new();
        let old = f.session("@alice:fixture.test", true);
        let before = std::fs::read(old.join("persistent_state/session")).unwrap();
        let first = scan(&f.0).unwrap();
        assert_eq!(first.len(), 1);
        let new = account_dir(&f.0, &first[0].user_id);
        assert!(!old.exists());
        assert_eq!(
            std::fs::read(new.join("persistent_state/session")).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read_to_string(new.join("persistent_state/latest_app_state.json")).unwrap(),
            "private fixture state"
        );
        assert_eq!(scan(&f.0).unwrap(), first);
    }
    #[test]
    fn misplaced_session_is_not_an_account() {
        let f = Fixture::new();
        let a = f.session("@alice:fixture.test", false);
        let b: OwnedUserId = "@bob:fixture.test".parse().unwrap();
        std::fs::rename(a, account_dir(&f.0, &b)).unwrap();
        assert!(scan(&f.0).unwrap().is_empty());
    }
    #[test]
    fn migration_does_not_overwrite_an_existing_canonical_profile() {
        let f = Fixture::new();
        let canonical = f.session("@alice:fixture.test", false);
        let legacy = f.session("@alice:fixture.test", true);
        std::fs::write(
            canonical.join("persistent_state/latest_app_state.json"),
            "canonical",
        )
        .unwrap();
        assert_eq!(scan(&f.0).unwrap().len(), 1);
        assert!(legacy.exists());
        assert_eq!(
            std::fs::read_to_string(canonical.join("persistent_state/latest_app_state.json"))
                .unwrap(),
            "canonical"
        );
    }
    #[tokio::test]
    async fn logout_removes_only_selected_tokens_and_preserves_other_accounts() {
        // This real credential worker uses the process-wide account epoch.
        // Other tests intentionally close their own App session; those are
        // different application lifetimes, not a logout of this fixture.
        const NAME: &str = "accounts::tests::logout_removes_only_selected_tokens_and_preserves_other_accounts";
        if crate::persistence::credential_state::tests::isolated_case(
            NAME,
            "HAGENCY_ACCOUNT_REMOVAL_CHILD",
        ) {
            return;
        }
        let f = Fixture::new();
        let a = f.session("@alice:fixture.test", false);
        let b = f.session("@bob:fixture.test", false);
        let before = std::fs::read(b.join("persistent_state/session")).unwrap();
        let credentials = crate::persistence::credential_state::tests::fake_service(&f.0);
        crate::persistence::credential_state::remove_account_on(credentials.clone(), a.join("persistent_state/session"), "@alice:fixture.test".parse::<OwnedUserId>().unwrap()).await.unwrap();
        assert!(crate::persistence::credential_state::test_shutdown(&credentials));
        assert!(!a.join("persistent_state/session").exists());
        let recovery =
            std::fs::read_to_string(a.join("persistent_state/retained_db.json")).unwrap();
        assert!(!recovery.contains("access_token"));
        assert!(!recovery.contains("refresh_token"));
        assert!(recovery.contains("db_fixture"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(a.join("persistent_state/retained_db.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        assert!(a.join("persistent_state/latest_app_state.json").exists());
        assert_eq!(
            std::fs::read(b.join("persistent_state/session")).unwrap(),
            before
        );
        assert_eq!(scan(&f.0).unwrap()[0].user_id.as_str(), "@bob:fixture.test");
    }
}
