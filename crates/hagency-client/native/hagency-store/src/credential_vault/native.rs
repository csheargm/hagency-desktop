use super::{Error, MAX_NATIVE_BYTES, SecretRef, SecretStore};
use std::sync::Mutex;
use zeroize::Zeroizing;

/// Explicit desktop native implementations. Construction performs no OS IO.
/// Calls can display native unlock/permission prompts; automated tests inject a
/// fake SecretStore and must never instantiate a live native qualification.
#[derive(Default)]
pub struct NativeStore {
    serial: Mutex<()>,
}
impl NativeStore {
    pub fn new() -> Self {
        Self::default()
    }
    #[cfg(any(target_os = "macos", target_os = "linux", windows))]
    fn entry(reference: &SecretRef) -> Result<keyring::Entry, Error> {
        let user = reference.entry_name()?;
        let service = "org.hagency.desktop.credentials.v1";
        #[cfg(target_os = "macos")]
        let builder = keyring::macos::default_credential_builder();
        #[cfg(target_os = "linux")]
        let builder = keyring::secret_service::default_credential_builder();
        #[cfg(windows)]
        let builder = keyring::windows::default_credential_builder();
        let credential = builder.build(None, service, &user).map_err(classify)?;
        Ok(keyring::Entry::new_with_credential(credential))
    }
}
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn classify(error: keyring::Error) -> Error {
    match &error {
        keyring::Error::PlatformFailure(source) | keyring::Error::NoStorageAccess(source) => {
            #[cfg(target_os = "macos")]
            if let Some(status) = source.downcast_ref::<security_framework::base::Error>() {
                match status.code() {
                    -128 | -25293 => return Error::Denied,
                    -25308 | -25315 => return Error::InteractionRequired,
                    _ => {}
                }
            }
            #[cfg(target_os = "linux")]
            if let Some(status) = source.downcast_ref::<secret_service::Error>() {
                match status {
                    secret_service::Error::Locked => return Error::Locked,
                    secret_service::Error::Prompt => return Error::InteractionRequired,
                    _ => {}
                }
            }
            #[cfg(windows)]
            if let Some(status) = source.downcast_ref::<keyring::windows::Error>()
                && matches!(status.0, 5 | 1223)
            {
                return Error::Denied;
            }
        }
        _ => {}
    }
    match error {
        keyring::Error::NoEntry => Error::Missing,
        keyring::Error::Ambiguous(_) => Error::Conflict,
        keyring::Error::TooLong(_, _) | keyring::Error::Invalid(_, _) => Error::Invalid,
        _ => Error::Unavailable,
    }
}
fn native_access() -> Result<(), Error> {
    if std::env::var_os("HAGENCY_NATIVE_CREDENTIAL_IO").as_deref()
        == Some(std::ffi::OsStr::new("deny"))
    {
        return Err(Error::Disabled);
    }
    Ok(())
}

impl SecretStore for NativeStore {
    fn get(&self, reference: &SecretRef) -> Result<Zeroizing<Vec<u8>>, Error> {
        native_access()?;
        let _guard = self.serial.lock().map_err(|_| Error::Unavailable)?;
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        {
            let secret = Zeroizing::new(Self::entry(reference)?.get_secret().map_err(classify)?);
            if secret.len() > MAX_NATIVE_BYTES {
                return Err(Error::Invalid);
            }
            Ok(secret)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let _ = reference;
            Err(Error::Unsupported)
        }
    }
    fn put(&self, reference: &SecretRef, bytes: &[u8]) -> Result<(), Error> {
        native_access()?;
        let _guard = self.serial.lock().map_err(|_| Error::Unavailable)?;
        if bytes.len() > MAX_NATIVE_BYTES {
            return Err(Error::Invalid);
        }
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        {
            Self::entry(reference)?.set_secret(bytes).map_err(classify)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let _ = reference;
            Err(Error::Unsupported)
        }
    }
    fn remove(&self, reference: &SecretRef) -> Result<(), Error> {
        native_access()?;
        let _guard = self.serial.lock().map_err(|_| Error::Unavailable)?;
        #[cfg(any(target_os = "macos", target_os = "linux", windows))]
        {
            Self::entry(reference)?
                .delete_credential()
                .map_err(classify)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
        {
            let _ = reference;
            Err(Error::Unsupported)
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn native_windows_denial_classification_preserves_other_failures() {
        for code in [5, 1223] {
            assert!(matches!(
                classify(keyring::Error::PlatformFailure(Box::new(
                    keyring::windows::Error(code)
                ))),
                Error::Denied
            ));
            assert!(matches!(
                classify(keyring::Error::NoStorageAccess(Box::new(
                    keyring::windows::Error(code)
                ))),
                Error::Denied
            ));
        }
        assert!(matches!(
            classify(keyring::Error::PlatformFailure(Box::new(
                keyring::windows::Error(1312)
            ))),
            Error::Unavailable
        ));
    }
}
