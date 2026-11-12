// Copyright 2026 The Matrix.org Foundation C.I.C.
// SPDX-License-Identifier: Apache-2.0

//! Adapted from Client #22's SDK inspector for the official SDK pinned by Desktop.
//! Synchronous inspection; never opens the normal (initializing) store.
use std::{fs, path::Path};

use matrix_sdk_crypto::Account;
use matrix_sdk_store_encryption::StoreCipher;
use ruma::{DeviceId, OwnedDeviceId, OwnedUserId, UserId};
use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction};
use zeroize::Zeroizing;

// These formats are tied to the official SDK revision pinned in Cargo.toml.
// Unknown schema versions fail closed; this adapter never initializes a store.
const DATABASE_NAME: &str = "matrix-sdk-crypto.sqlite3";
const DATABASE_VERSION: u8 = 19;

/// Public identity of an existing persisted Olm account, without private keys.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExistingAccountIdentity {
    user_id: OwnedUserId,
    device_id: OwnedDeviceId,
}

impl ExistingAccountIdentity {
    /// The account's persisted Matrix user ID.
    pub fn user_id(&self) -> &UserId {
        &self.user_id
    }

    /// The account's persisted Matrix device ID.
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }
}

/// Failure to inspect an existing account. Errors never establish freshness.
/// Decoding errors deliberately exclude serialized account and cipher contents.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InspectAccountError {
    /// The original file could not be inspected.
    #[error("Cannot inspect original crypto database")]
    Io(#[source] std::io::Error),
    /// SQLite could not open or consistently read the original database.
    #[error("Cannot read original crypto database")]
    Sqlite(#[from] rusqlite::Error),
    /// Only the current schema is supported; inspection never migrates it.
    #[error("Unsupported crypto database schema")]
    UnsupportedSchema,
    /// The existing database has no account; this is not proof of freshness.
    #[error("The original crypto database has no account")]
    MissingAccount,
    /// The supplied secret cannot unlock the original cipher.
    #[error("Cannot unlock original crypto database cipher")]
    InvalidCipher,
    /// The original cipher requests an excessive PBKDF2 iteration count.
    #[error("Crypto database PBKDF2 cost {found} exceeds limit {max}")]
    KdfRoundLimit {
        /// Stored PBKDF2 iteration count.
        found: u32,
        /// Maximum accepted iteration count.
        max: u32,
    },
    /// The original account could not be authenticated or decoded.
    #[error("Invalid original crypto account")]
    InvalidAccount,
    /// A required original value has an unsupported type, size, or shape.
    #[error("Invalid original crypto database")]
    InvalidDatabase,
}

/// Inspect an existing account without initializing or migrating its store.
///
/// `path` is the existing store directory. Only the current crypto schema
/// is supported. Missing files, missing accounts, unsupported schemas, and
/// incorrect secrets are errors, never evidence that a database is fresh.
/// PBKDF2 imports above 1,000,000 iterations are rejected (standard SDK
/// exports use 200,000). Encoded values and decrypted account bytes are
/// size-limited, but the reused account decoder and SQLite native memory
/// are not claimed to have a complete allocation bound.
/// `passphrase` must use the original passphrase format (not a raw key or
/// the high-entropy key migration configuration).
///
/// The connection is opened read-only, without CREATE or URI/immutable
/// flags. A single SQLite read transaction includes the version, existing
/// cipher, and account, so committed WAL contents are respected. This does
/// not invoke store initialization, migrations, checkpoints, or account
/// writes. SQLite may coordinate readers through its WAL/SHM sidecars;
/// their bytes are not guaranteed unchanged. A read-only connection that
/// cannot access the necessary sidecars returns an error.
///
/// This function blocks, including passphrase derivation. Async callers
/// must run it in an owned blocking worker that retains the original job
/// and secret until completion even if its observer is cancelled. It does
/// not spawn or detach work itself. The caller remains responsible for
/// directory authority, original file identity, and later activation.
/// SQLite's NOFOLLOW flag is requested, and a final-component symlink is
/// rejected explicitly; platform/VFS support does not replace those caller
/// path and file-identity guards.
pub fn inspect_existing_account_identity(
    path: impl AsRef<Path>,
    passphrase: Option<&str>,
) -> Result<ExistingAccountIdentity, InspectAccountError> {
    inspect(path.as_ref(), passphrase, || {})
}

// This hook is a no-op in production; tests hold the real read snapshot while
// another connection commits an account/cipher replacement in WAL mode.
fn inspect(
    directory: &Path,
    passphrase: Option<&str>,
    after_version: impl FnOnce(),
) -> Result<ExistingAccountIdentity, InspectAccountError> {
    let path = directory.join(DATABASE_NAME);
    let metadata = fs::symlink_metadata(&path).map_err(InspectAccountError::Io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(InspectAccountError::InvalidDatabase);
    }
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let transaction = connection.transaction()?;
    let table: Option<String> = transaction
        .query_row(
            "SELECT type FROM sqlite_schema WHERE name = 'kv'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if table.as_deref() != Some("table") {
        return Err(InspectAccountError::UnsupportedSchema);
    }
    let version =
        value(&transaction, "version", 1)?.ok_or(InspectAccountError::UnsupportedSchema)?;
    if version.as_slice() != [DATABASE_VERSION] {
        return Err(InspectAccountError::UnsupportedSchema);
    }
    after_version();
    let encoded_cipher = value(&transaction, "cipher", 64 * 1024)?;
    let cipher = match (passphrase, encoded_cipher) {
        (Some(secret), Some(encoded)) => Some(import_cipher(secret, &encoded)?),
        (None, None) => None,
        _ => return Err(InspectAccountError::InvalidCipher),
    };
    let encoded = Zeroizing::new(
        value(&transaction, "account", 4 * 1024 * 1024 + 128)?
            .ok_or(InspectAccountError::MissingAccount)?,
    );
    let plaintext = match cipher {
        Some(cipher) => Zeroizing::new(
            cipher
                .decrypt_value_data(
                    rmp_serde::from_slice(&encoded)
                        .map_err(|_| InspectAccountError::InvalidAccount)?,
                )
                .map_err(|_| InspectAccountError::InvalidAccount)?,
        ),
        None => encoded,
    };
    if plaintext.len() > 4 * 1024 * 1024 {
        return Err(InspectAccountError::InvalidAccount);
    }
    let pickle =
        rmp_serde::from_slice(&plaintext).map_err(|_| InspectAccountError::InvalidAccount)?;
    let account = Account::from_pickle(pickle).map_err(|_| InspectAccountError::InvalidAccount)?;
    let identity = ExistingAccountIdentity {
        user_id: account.user_id().to_owned(),
        device_id: account.device_id().to_owned(),
    };
    transaction.commit()?;
    Ok(identity)
}

fn value(
    transaction: &Transaction<'_>,
    key: &str,
    limit: usize,
) -> Result<Option<Vec<u8>>, InspectAccountError> {
    // Preflight in this same read snapshot, before materializing the blob.
    let size: Option<i64> = transaction
        .query_row(
            "SELECT CASE WHEN typeof(value) = 'blob' THEN length(value) ELSE -1 END FROM kv WHERE key = ?",
            [key],
            |row| row.get(0),
        )
        .optional()?;
    let Some(size) = size else { return Ok(None) };
    if usize::try_from(size)
        .ok()
        .filter(|size| *size <= limit)
        .is_none()
    {
        return Err(InspectAccountError::InvalidDatabase);
    }
    Ok(Some(transaction.query_row(
        "SELECT value FROM kv WHERE key = ?",
        [key],
        |row| row.get(0),
    )?))
}

// Preflight the original KDF header before the official SDK derives a key.
// Ignore ciphertext during preflight; the SDK authenticates the original bytes.
#[derive(serde::Deserialize)]
struct CipherHeader {
    kdf_info: KdfInfo,
    #[allow(dead_code)]
    ciphertext_info: serde::de::IgnoredAny,
}
#[derive(serde::Deserialize)]
enum KdfInfo {
    Pbkdf2ToChaCha20Poly1305 { rounds: u32, kdf_salt: [u8; 32] },
}
fn import_cipher(secret: &str, encoded: &[u8]) -> Result<StoreCipher, InspectAccountError> {
    let header: CipherHeader = rmp_serde::from_slice(encoded)
        .or_else(|_| serde_json::from_slice(encoded))
        .map_err(|_| InspectAccountError::InvalidCipher)?;
    let KdfInfo::Pbkdf2ToChaCha20Poly1305 { rounds, kdf_salt } = header.kdf_info;
    let _ = kdf_salt;
    if rounds == 0 || rounds > 1_000_000 {
        return Err(InspectAccountError::KdfRoundLimit {
            found: rounds,
            max: 1_000_000,
        });
    }
    StoreCipher::import(secret, encoded).map_err(|_| InspectAccountError::InvalidCipher)
}

#[cfg(test)]
mod tests;
