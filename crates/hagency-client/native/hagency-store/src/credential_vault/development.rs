//! Developer-only secret store for locally built test apps.
//!
//! A local build has no stable code identity. The OS credential store ties
//! each item to the exact program that created it, so every rebuild is asked
//! for the login password again, item by item, and a dismissed prompt leaves
//! the saved sign-in unreadable. With `HAGENCY_NATIVE_CREDENTIAL_IO` set to
//! `development-files` the secrets are kept in private files inside the app
//! data directory instead. Only file permissions protect them, as they
//! protected saved sign-ins before credentials moved to the OS store. This is
//! for a developer's own test profiles; a distributed build must never set it.
use super::{Error, MAX_NATIVE_BYTES, SecretRef, SecretStore};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use zeroize::Zeroizing;

/// The value of `HAGENCY_NATIVE_CREDENTIAL_IO` that selects this store.
pub const SELECTOR: &str = "development-files";

/// Whether this build has the file store at all. A distributed build is made
/// without the `development-credential-files` feature: the selector then
/// selects nothing and saved sign-ins stay in the OS store.
pub const COMPILED: bool = cfg!(feature = "development-credential-files");

pub fn selected() -> bool {
    COMPILED
        && std::env::var_os("HAGENCY_NATIVE_CREDENTIAL_IO").as_deref()
            == Some(std::ffi::OsStr::new(SELECTOR))
}

pub struct DevelopmentFileStore {
    directory: PathBuf,
    /// Where a secret is looked up once when no file holds it yet: the OS
    /// store an earlier build of this profile wrote it to.
    earlier: Arc<dyn SecretStore>,
    serial: Mutex<()>,
}
impl DevelopmentFileStore {
    pub fn new(directory: PathBuf, earlier: Arc<dyn SecretStore>) -> Self {
        Self {
            directory,
            earlier,
            serial: Mutex::new(()),
        }
    }
    fn path(&self, reference: &SecretRef) -> Result<PathBuf, Error> {
        let digest = Sha256::digest(reference.entry_name()?.as_bytes());
        Ok(self.directory.join(format!("{digest:x}.secret")))
    }
    fn read(path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, Error> {
        match std::fs::symlink_metadata(path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(Error::Storage),
        }
        // `open` refuses a file that is not private to this user.
        let file = crate::private::open(path, false).map_err(|_| Error::Storage)?;
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(MAX_NATIVE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::Storage)?;
        if bytes.len() > MAX_NATIVE_BYTES {
            return Err(Error::Invalid);
        }
        Ok(Some(bytes))
    }
    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), Error> {
        if bytes.len() > MAX_NATIVE_BYTES {
            return Err(Error::Invalid);
        }
        crate::private::directory(&self.directory).map_err(|_| Error::Storage)?;
        crate::private::replace(path, bytes).map_err(|_| Error::Storage)
    }
}
impl SecretStore for DevelopmentFileStore {
    fn get(&self, reference: &SecretRef) -> Result<Zeroizing<Vec<u8>>, Error> {
        let _guard = self.serial.lock().map_err(|_| Error::Unavailable)?;
        let path = self.path(reference)?;
        if let Some(bytes) = Self::read(&path)? {
            return Ok(bytes);
        }
        // A profile that an earlier build moved to the OS store: read the
        // secret there once (the OS may ask) and keep it here from now on.
        // A refusal is reported as it is and nothing is written.
        let bytes = self.earlier.get(reference)?;
        self.write(&path, &bytes)?;
        Ok(bytes)
    }
    fn put(&self, reference: &SecretRef, bytes: &[u8]) -> Result<(), Error> {
        let _guard = self.serial.lock().map_err(|_| Error::Unavailable)?;
        self.write(&self.path(reference)?, bytes)
    }
    fn remove(&self, reference: &SecretRef) -> Result<(), Error> {
        let _guard = self.serial.lock().map_err(|_| Error::Unavailable)?;
        // The copy in the OS store, if any, is left alone: removing it could
        // ask for the login password again, which is what this store avoids.
        match std::fs::remove_file(self.path(reference)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(Error::Missing),
            Err(_) => Err(Error::Storage),
        }
    }
}
