//! Public recovery intent only. Secret keys are never serialized here.
use anyhow::{Context, Result, ensure};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use cap_std::fs::{Dir, OpenOptions};
use matrix_sdk::ruma::events::EventContentFromType;
use matrix_sdk::{Client, ruma::events::secret_storage::key::SecretStorageKeyEventContent};
use matrix_sdk_base::crypto::secret_storage::SecretStorageKey;
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::Path,
};
use zeroize::Zeroizing;

const LIMIT: u64 = 32 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Plan {
    pub version: u8,
    pub owner: String,
    pub origin: String,
    pub device: String,
    pub key_id: String,
    pub info: serde_json::Value,
    pub master: Option<String>,
    pub applying: bool,
    pub complete: bool,
}
impl Plan {
    pub fn new(client: &Client, key: &SecretStorageKey, master: Option<String>) -> Result<Self> {
        Ok(Self {
            version: 1,
            owner: client.user_id().context("Sign in first")?.to_string(),
            origin: client.homeserver().to_string(),
            device: client.device_id().context("No device")?.to_string(),
            key_id: key.key_id().to_owned(),
            info: serde_json::to_value(key.event_content())?,
            master,
            applying: false,
            complete: false,
        })
    }
    pub fn matches(&self, client: &Client) -> bool {
        self.version == 1
            && client.user_id().map(|u| u.as_str()) == Some(self.owner.as_str())
            && client.device_id().map(|d| d.as_str()) == Some(self.device.as_str())
            && client.homeserver().as_str() == self.origin
            && !self.key_id.is_empty()
            && self.key_id.len() <= 128
    }
    pub fn restore_key(&self, saved: &str) -> Result<SecretStorageKey> {
        let raw = serde_json::value::to_raw_value(&self.info)?;
        let content = SecretStorageKeyEventContent::from_parts(
            &format!("m.secret_storage.key.{}", self.key_id),
            &raw,
        )?;
        // SDK validates the key against the public IV/MAC before any upload.
        Ok(SecretStorageKey::from_account_data(saved, content)?)
    }
    fn name(&self) -> String {
        name(&self.origin, &self.owner, &self.device)
    }
    pub fn save(&self, root: &Path) -> Result<()> {
        let dir = directory(root)?;
        let data = serde_json::to_vec(self)?;
        ensure!(data.len() <= LIMIT as usize, "Recovery intent is too large");
        let temporary = format!("{}.tmp", uuid::Uuid::new_v4());
        let mut file = create_private(&dir, Path::new(&temporary))?;
        let result = (|| -> Result<()> {
            file.write_all(&data)?;
            file.sync_all()?;
            dir.rename(&temporary, &dir, self.name())?;
            Ok(())
        })();
        if result.is_err() {
            let _ = dir.remove_file(&temporary);
        }
        result
    }
    pub fn discard(&self, root: &Path) -> Result<()> {
        ensure!(
            !self.applying && !self.complete,
            "Published recovery cannot be discarded"
        );
        directory(root)?.remove_file(self.name())?;
        Ok(())
    }
    pub fn load(root: &Path, client: &Client) -> Result<Option<Self>> {
        let dir = directory(root)?;
        let filename = name(
            client.homeserver().as_str(),
            client.user_id().context("No user")?.as_str(),
            client.device_id().context("No device")?.as_str(),
        );
        let bytes = match read_bounded(&dir, Path::new(&filename), LIMIT) {
            Ok(b) => b,
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        };
        let plan: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            plan.matches(client),
            "Recovery intent belongs to another session"
        );
        Ok(Some(plan))
    }
}
fn name(origin: &str, owner: &str, device: &str) -> String {
    format!(
        "{}.json",
        blake3::hash(format!("{origin}\0{owner}\0{device}").as_bytes()).to_hex()
    )
}
fn directory(root: &Path) -> Result<Dir> {
    std::fs::create_dir_all(root)?;
    let root = Dir::open_ambient_dir(root, cap_std::ambient_authority())?;
    Ok(root)
}
fn create_private(dir: &Dir, name: &Path) -> Result<std::fs::File> {
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
        use windows::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE},
            Storage::FileSystem::{WRITE_DAC, WRITE_OWNER},
        };
        options.access_mode(GENERIC_READ.0 | GENERIC_WRITE.0 | WRITE_DAC.0 | WRITE_OWNER.0);
    }
    let file = dir.open_with(name, &options)?.into_std();
    hagency_store::private::seal_created_file_handle(&file)?;
    Ok(file)
}
fn read_bounded(dir: &Dir, name: &Path, limit: u64) -> Result<Zeroizing<Vec<u8>>> {
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = dir.open_with(name, &options)?.into_std();
    ensure!(file.metadata()?.is_file(), "Choose a regular key file");
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= limit, "Key file is too large");
    Ok(bytes)
}
/// User-selected destination; retain the parent capability through publication.
pub(super) fn save_key(path: &Path, key: &str) -> Result<()> {
    let parent = Dir::open_ambient_dir(
        path.parent().context("No parent")?,
        cap_std::ambient_authority(),
    )?;
    let mut file = create_private(
        &parent,
        Path::new(path.file_name().context("No file name")?),
    )?;
    file.write_all(key.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
pub(super) fn load_key(path: &Path) -> Result<Zeroizing<String>> {
    let parent = Dir::open_ambient_dir(
        path.parent().context("No parent")?,
        cap_std::ambient_authority(),
    )?;
    let bytes = read_bounded(
        &parent,
        Path::new(path.file_name().context("No file name")?),
        8192,
    )?;
    Ok(Zeroizing::new(
        std::str::from_utf8(&bytes)?.trim().to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_key_is_private_bounded_and_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("key.txt");
        save_key(&path, "synthetic-key").unwrap();
        assert_eq!(&*load_key(&path).unwrap(), "synthetic-key");
        assert!(save_key(&path, "replacement").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::write(&path, vec![b'a'; 8193]).unwrap();
        assert!(load_key(&path).is_err());
        #[cfg(unix)]
        {
            let link = root.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(load_key(&link).is_err());
        }
    }
    #[test]
    fn public_plan_can_reconstruct_only_the_original_saved_key() {
        let key = SecretStorageKey::new();
        let saved = Zeroizing::new(key.to_base58());
        let plan = Plan {
            version: 1,
            owner: "@alice:example.org".into(),
            origin: "https://example.org/".into(),
            device: "ONE".into(),
            key_id: key.key_id().into(),
            info: serde_json::to_value(key.event_content()).unwrap(),
            master: None,
            applying: false,
            complete: false,
        };
        let root = tempfile::tempdir().unwrap();
        plan.save(root.path()).unwrap();
        let bytes = std::fs::read_to_string(root.path().join(plan.name())).unwrap();
        assert!(!bytes.contains(saved.as_str()));
        assert!(!bytes.contains("secret_key"));
        assert_eq!(plan.restore_key(&saved).unwrap().to_base58(), *saved);
        assert!(
            plan.restore_key(&SecretStorageKey::new().to_base58())
                .is_err()
        );
        let mut value: serde_json::Value = serde_json::from_str(&bytes).unwrap();
        value["private_key"] = serde_json::json!("forbidden");
        assert!(serde_json::from_value::<Plan>(value).is_err());
        let mut started = plan.clone();
        started.applying = true;
        assert!(started.discard(root.path()).is_err());
        plan.discard(root.path()).unwrap();
        assert!(!root.path().join(plan.name()).exists());
        let mut corrupted = plan;
        corrupted.info["mac"] = serde_json::json!("AAAA");
        assert!(corrupted.restore_key(&saved).is_err());
    }
}
