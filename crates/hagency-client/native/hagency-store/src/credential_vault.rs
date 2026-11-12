//! Scoped native credentials and authenticated documents. Native calls are
//! synchronous and may prompt or block: invoke them only from an owned worker.
//! SDK synchronous callbacks use an already unlocked `DocumentKey`, never OS IO.
//! There is deliberately no default or environment-selected mock backend.

pub mod document;
mod native;
#[cfg(test)]
mod tests;
pub mod worker;

use matrix_sdk_store_encryption::StoreCipher;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use zeroize::Zeroizing;

pub use native::NativeStore;

const VERSION: u32 = 1;
const MAX_NATIVE_BYTES: usize = 2048; // Below Windows' 2560-byte credential limit.
const MAX_SECRET_BYTES: usize = 512;

/// Deliberately stable, secret-free errors. Native error descriptions can carry
/// entry identifiers or platform diagnostics and never become UI/log text here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("credential_missing")]
    Missing,
    #[error("credential_access_unavailable")]
    Unavailable,
    #[error("credential_store_locked")]
    Locked,
    #[error("credential_access_denied")]
    Denied,
    #[error("credential_native_interaction_required")]
    InteractionRequired,
    #[error("credential_platform_unsupported")]
    Unsupported,
    #[error("credential_native_io_disabled_for_automation")]
    Disabled,
    #[error("credential_conflict")]
    Conflict,
    #[error("credential_schema_or_context_invalid")]
    Invalid,
    #[error("credential_integrity_failed")]
    Integrity,
    #[error("credential_randomness_unavailable")]
    Randomness,
    #[error("credential_storage_failed")]
    Storage,
    #[error("credential_commit_durability_unknown")]
    DurabilityUnknown,
    #[error("credential_operation_retired")]
    Retired,
    #[error("credential_session_revocation_pending")]
    SessionRetired,
    #[error("credential_original_sdk_still_completing")]
    OriginalBusy,
    #[error("credential_capacity_exhausted")]
    Capacity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Database,
    Session,
    AgentDatabase,
    Revocations,
    Readiness,
}
impl Purpose {
    fn label(self) -> &'static str {
        match self {
            Self::Database => "database",
            Self::Session => "session",
            Self::AgentDatabase => "agent_database",
            Self::Revocations => "revocations",
            Self::Readiness => "readiness",
        }
    }
}

/// Callers construct the complete canonical identity, never a display name.
/// Installation IDs are random per app-data root. Contexts include full MXID,
/// device, homeserver and OAuth client, or profile/Agent/execution-device IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    installation: String,
    purpose: Purpose,
    identity: String,
}
impl Scope {
    pub fn new(installation: &str, purpose: Purpose, identity: &[u8]) -> Result<Self, Error> {
        if !valid_id(installation) || identity.is_empty() || identity.len() > 65536 {
            return Err(Error::Invalid);
        }
        Ok(Self {
            installation: installation.into(),
            purpose,
            identity: hex(&Sha256::digest(identity)),
        })
    }
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }
    fn validate(&self) -> Result<(), Error> {
        if !valid_id(&self.installation) || !valid_digest(&self.identity) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SecretRef {
    version: u32,
    scope: Scope,
    id: String,
}
impl SecretRef {
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn validate(&self, expected: &Scope) -> Result<(), Error> {
        self.check(expected)
    }
    fn check(&self, expected: &Scope) -> Result<(), Error> {
        self.scope.validate()?;
        if self.version != VERSION || &self.scope != expected || !valid_id(&self.id) {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    fn check_owner(&self, expected: &Scope) -> Result<(), Error> {
        self.check(&self.scope)?;
        if self.scope.installation != expected.installation {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    fn entry_name(&self) -> Result<String, Error> {
        self.check(&self.scope)?;
        Ok(format!(
            "{}.{}.{}.{}",
            self.scope.installation,
            self.scope.purpose.label(),
            self.scope.identity,
            self.id
        ))
    }
}

/// Narrow binary OS adapter. Implementations must not expose enumeration or
/// choose another backend when the configured native store is unavailable.
/// A fresh random reference plus a caller-owned document lock serializes writes;
/// `put` itself is not claimed to be a cross-process compare-and-swap primitive.
pub trait SecretStore: Send + Sync {
    fn get(&self, reference: &SecretRef) -> Result<Zeroizing<Vec<u8>>, Error>;
    fn put(&self, reference: &SecretRef, bytes: &[u8]) -> Result<(), Error>;
    fn remove(&self, reference: &SecretRef) -> Result<(), Error>;
}

#[derive(Clone)]
pub struct Vault {
    store: Arc<dyn SecretStore>,
}
impl Vault {
    pub fn new(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }
    pub fn native() -> Self {
        Self::new(Arc::new(NativeStore::new()))
    }
    /// Non-authority, installation-local availability probe. This dedicated
    /// purpose can never unlock an account, database, Agent, or revoke record.
    /// Serialize callers through their retained private readiness-document lock.
    /// A fresh value forces both native write and readback even after a prior
    /// successful launch; no auth secret is overwritten or generated here.
    pub fn check_native_readiness(&self, installation: &str) -> Result<(), Error> {
        let scope = Scope::new(
            installation,
            Purpose::Readiness,
            b"native-credential-readiness-v1",
        )?;
        let reference = SecretRef {
            version: VERSION,
            scope,
            id: "00000000000000000000000000000001".into(),
        };
        let secret = DocumentKey::random_secret()?;
        let record = SecretRecord {
            version: VERSION,
            reference: reference.clone(),
            bytes: secret.to_vec(),
        };
        let encoded = Zeroizing::new(serde_json::to_vec(&record).map_err(|_| Error::Invalid)?);
        if encoded.len() > MAX_NATIVE_BYTES {
            return Err(Error::Invalid);
        }
        self.store.put(&reference, &encoded)?;
        if self.load(&reference, reference.scope())?.as_slice() != secret.as_slice() {
            return Err(Error::Integrity);
        }
        Ok(())
    }
    pub fn fresh_reference(scope: &Scope) -> Result<SecretRef, Error> {
        scope.validate()?;
        Ok(SecretRef {
            version: VERSION,
            scope: scope.clone(),
            id: random_id()?,
        })
    }
    /// Journal this fresh reference BEFORE writing it to native storage. If OS
    /// write or readback fails, retain the journal; the write may have happened.
    pub fn create_at(&self, reference: &SecretRef, secret: &[u8]) -> Result<(), Error> {
        reference.check(&reference.scope)?;
        if secret.is_empty() || secret.len() > MAX_SECRET_BYTES {
            return Err(Error::Invalid);
        }
        match self.store.get(reference) {
            Err(Error::Missing) => {}
            Ok(_) => return Err(Error::Conflict),
            Err(error) => return Err(error),
        }
        let record = SecretRecord {
            version: VERSION,
            reference: reference.clone(),
            bytes: secret.to_vec(),
        };
        let encoded = Zeroizing::new(serde_json::to_vec(&record).map_err(|_| Error::Invalid)?);
        if encoded.len() > MAX_NATIVE_BYTES {
            return Err(Error::Invalid);
        }
        self.store.put(reference, &encoded)?;
        let actual = self.load(reference, &reference.scope)?;
        if actual.as_slice() != secret {
            return Err(Error::Integrity);
        }
        Ok(())
    }
    pub fn load(
        &self,
        reference: &SecretRef,
        expected: &Scope,
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        reference.check(expected)?;
        let bytes = self.store.get(reference)?;
        if bytes.len() > MAX_NATIVE_BYTES {
            return Err(Error::Invalid);
        }
        let record: SecretRecord = serde_json::from_slice(&bytes).map_err(|_| Error::Integrity)?;
        if record.version != VERSION
            || record.reference != *reference
            || record.bytes.is_empty()
            || record.bytes.len() > MAX_SECRET_BYTES
        {
            return Err(Error::Integrity);
        }
        Ok(Zeroizing::new(record.bytes.clone()))
    }
    /// Only call after a complete document inventory proves this journal-owned
    /// reference unreferenced. Missing/unknown schemas cannot establish proof.
    pub fn remove_unreferenced(
        &self,
        reference: &SecretRef,
        proof: &Unreferenced,
    ) -> Result<(), Error> {
        if proof.reference != *reference {
            return Err(Error::Invalid);
        }
        match self.store.remove(reference) {
            Err(Error::Missing) => Ok(()),
            result => result,
        }
    }
    pub fn unlock(&self, reference: &SecretRef, expected: &Scope) -> Result<DocumentKey, Error> {
        let secret = self.load(reference, expected)?;
        if secret.len() != 32 {
            return Err(Error::Integrity);
        }
        Ok(DocumentKey {
            reference: reference.clone(),
            key: secret,
        })
    }
}

/// The owner can issue this only after checking all its known versioned files.
/// It is intentionally not inferred from NoEntry or a failed parse.
pub struct Unreferenced {
    reference: SecretRef,
}
impl Unreferenced {
    pub fn after_complete_inventory(
        reference: &SecretRef,
        all_refs: &[SecretRef],
    ) -> Result<Self, Error> {
        if all_refs.contains(reference) {
            return Err(Error::Conflict);
        }
        Ok(Self {
            reference: reference.clone(),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretRecord {
    version: u32,
    reference: SecretRef,
    bytes: Vec<u8>,
}
impl Drop for SecretRecord {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.bytes.zeroize();
    }
}

pub struct DocumentKey {
    reference: SecretRef,
    key: Zeroizing<Vec<u8>>,
}
impl std::fmt::Debug for DocumentKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocumentKey")
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}
impl DocumentKey {
    pub fn reference(&self) -> &SecretRef {
        &self.reference
    }
    pub fn random_secret() -> Result<Zeroizing<Vec<u8>>, Error> {
        let mut key = Zeroizing::new(vec![0; 32]);
        getrandom::fill(&mut key).map_err(|_| Error::Randomness)?;
        Ok(key)
    }
    pub fn seal<T: Serialize>(&self, value: &T) -> Result<Envelope, Error> {
        let cipher = StoreCipher::new().map_err(|_| Error::Integrity)?;
        let wrapped = cipher
            .export_with_key(&self.key)
            .map_err(|_| Error::Integrity)?;
        let ciphertext = cipher
            .encrypt_value(&Authenticated {
                version: VERSION,
                reference: self.reference.clone(),
                value,
            })
            .map_err(|_| Error::Integrity)?;
        Ok(Envelope {
            version: VERSION,
            reference: self.reference.clone(),
            wrapped,
            ciphertext,
        })
    }
    pub fn open<T: DeserializeOwned>(&self, envelope: &Envelope) -> Result<T, Error> {
        if envelope.version != VERSION || envelope.reference != self.reference {
            return Err(Error::Integrity);
        }
        let cipher = StoreCipher::import_with_key(&self.key, &envelope.wrapped)
            .map_err(|_| Error::Integrity)?;
        let authenticated: Authenticated<T> = cipher
            .decrypt_value(&envelope.ciphertext)
            .map_err(|_| Error::Integrity)?;
        if authenticated.version != VERSION || authenticated.reference != self.reference {
            return Err(Error::Integrity);
        }
        Ok(authenticated.value)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Authenticated<T> {
    version: u32,
    reference: SecretRef,
    value: T,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    version: u32,
    reference: SecretRef,
    wrapped: Vec<u8>,
    ciphertext: Vec<u8>,
}
impl Envelope {
    pub fn reference(&self) -> &SecretRef {
        &self.reference
    }
}

pub fn random_id() -> Result<String, Error> {
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| Error::Randomness)?;
    Ok(hex(&random))
}
fn valid_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
