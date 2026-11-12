// Copyright 2026 The Matrix.org Foundation C.I.C.
// SPDX-License-Identifier: Apache-2.0
use std::{fs, sync::mpsc};

use matrix_sdk_crypto::{
    Account,
    store::{CryptoStore, types::PendingChanges},
};
use ruma::{device_id, user_id};
use tempfile::TempDir;

use super::*;
use matrix_sdk_sqlite::SqliteCryptoStore;

async fn original(secret: Option<&str>) -> (TempDir, SqliteCryptoStore) {
    let directory = tempfile::tempdir().unwrap();
    let canonical = directory.path().canonicalize().unwrap();
    let store = SqliteCryptoStore::open(&canonical, secret).await.unwrap();
    let account =
        Account::with_device_id(user_id!("@original:example.org"), device_id!("ORIGINAL"));
    store
        .save_pending_changes(PendingChanges {
            account: Some(account),
        })
        .await
        .unwrap();
    (directory, store)
}

fn path(directory: &TempDir) -> std::path::PathBuf {
    directory.path().canonicalize().unwrap()
}

fn assert_original(identity: ExistingAccountIdentity) {
    assert_eq!(identity.user_id(), user_id!("@original:example.org"));
    assert_eq!(identity.device_id(), device_id!("ORIGINAL"));
}

#[tokio::test]
async fn inspect_existing_account_identity_reads_current_plain_and_encrypted_without_changes() {
    for secret in [None, Some("original-passphrase")] {
        let (directory, store) = original(secret).await;
        store.close().await.unwrap();
        let writer = Connection::open(path(&directory).join(DATABASE_NAME)).unwrap();
        writer
            .execute(
                "INSERT INTO kv (key, value) VALUES ('history-sentinel', ?)",
                [b"original opaque crypto history".as_slice()],
            )
            .unwrap();
        writer
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .unwrap();
        let rows = |connection: &Connection| -> Vec<(String, Vec<u8>)> {
            connection
                .prepare("SELECT key, value FROM kv ORDER BY key")
                .unwrap()
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let original_rows = rows(&writer);
        let original_schema: String = writer
            .query_row("SELECT group_concat(sql) FROM sqlite_schema", [], |row| {
                row.get(0)
            })
            .unwrap();
        let before = fs::read(path(&directory).join(DATABASE_NAME)).unwrap();
        assert_original(inspect_existing_account_identity(path(&directory), secret).unwrap());
        assert_eq!(
            fs::read(path(&directory).join(DATABASE_NAME)).unwrap(),
            before
        );
        assert_eq!(rows(&writer), original_rows);
        assert_eq!(
            writer
                .query_row("SELECT group_concat(sql) FROM sqlite_schema", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            original_schema
        );
        drop(writer);
        // Reopen through the original API and actually unpickle the same account.
        let original = SqliteCryptoStore::open(path(&directory), secret)
            .await
            .unwrap();
        let account = original.load_account().await.unwrap().unwrap();
        assert_eq!(account.device_id(), device_id!("ORIGINAL"));
        original.close().await.unwrap();
    }
}

#[test]
fn inspect_existing_account_identity_missing_does_not_create_anything() {
    let directory = tempfile::tempdir().unwrap();
    let missing = path(&directory).join("missing");
    assert!(matches!(
        inspect_existing_account_identity(&missing, None),
        Err(InspectAccountError::Io(_))
    ));
    assert!(!missing.exists());
    assert!(inspect_existing_account_identity(path(&directory), None).is_err());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn inspect_existing_account_identity_rejects_wrong_secret_missing_and_corrupt_values() {
    let (directory, store) = original(Some("original-passphrase")).await;
    store.close().await.unwrap();
    assert!(matches!(
        inspect_existing_account_identity(path(&directory), Some("wrong")),
        Err(InspectAccountError::InvalidCipher)
    ));
    assert!(matches!(
        inspect_existing_account_identity(path(&directory), None),
        Err(InspectAccountError::InvalidCipher)
    ));
    let connection = Connection::open(path(&directory).join(DATABASE_NAME)).unwrap();
    connection
        .execute("UPDATE kv SET value = x'00' WHERE key = 'account'", [])
        .unwrap();
    let before = fs::read(path(&directory).join(DATABASE_NAME)).unwrap();
    assert!(matches!(
        inspect_existing_account_identity(path(&directory), Some("original-passphrase")),
        Err(InspectAccountError::InvalidAccount)
    ));
    assert_eq!(
        fs::read(path(&directory).join(DATABASE_NAME)).unwrap(),
        before
    );
    connection
        .execute("DELETE FROM kv WHERE key = 'account'", [])
        .unwrap();
    assert!(matches!(
        inspect_existing_account_identity(path(&directory), Some("original-passphrase")),
        Err(InspectAccountError::MissingAccount)
    ));
    connection
        .execute("UPDATE kv SET value = x'00' WHERE key = 'cipher'", [])
        .unwrap();
    assert!(matches!(
        inspect_existing_account_identity(path(&directory), Some("original-passphrase")),
        Err(InspectAccountError::InvalidCipher)
    ));
    connection
        .execute("DELETE FROM kv WHERE key = 'cipher'", [])
        .unwrap();
    assert!(matches!(
        inspect_existing_account_identity(path(&directory), Some("original-passphrase")),
        Err(InspectAccountError::InvalidCipher)
    ));
}

#[tokio::test]
async fn inspect_existing_account_identity_refuses_schema_changes_without_migration() {
    let (directory, store) = original(None).await;
    store.close().await.unwrap();
    let connection = Connection::open(path(&directory).join(DATABASE_NAME)).unwrap();
    for version in [0_u8, DATABASE_VERSION - 1, DATABASE_VERSION + 1] {
        connection
            .execute(
                "UPDATE kv SET value = ? WHERE key = 'version'",
                [[version].as_slice()],
            )
            .unwrap();
        let before = fs::read(path(&directory).join(DATABASE_NAME)).unwrap();
        let schema: String = connection
            .query_row("SELECT group_concat(sql) FROM sqlite_schema", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(matches!(
            inspect_existing_account_identity(path(&directory), None),
            Err(InspectAccountError::UnsupportedSchema)
        ));
        assert_eq!(
            fs::read(path(&directory).join(DATABASE_NAME)).unwrap(),
            before
        );
        assert_eq!(
            connection
                .query_row("SELECT group_concat(sql) FROM sqlite_schema", [], |row| row
                    .get::<_, String>(0))
                .unwrap(),
            schema
        );
        assert_eq!(
            connection
                .query_row("SELECT value FROM kv WHERE key = 'version'", [], |row| row
                    .get::<_, Vec<
                    u8,
                >>(
                    0
                ))
                .unwrap(),
            [version]
        );
    }
}

#[tokio::test]
async fn inspect_existing_account_identity_reads_live_wal_and_one_consistent_snapshot() {
    let (directory, store) = original(Some("original-passphrase")).await;
    let writer = Connection::open(path(&directory).join(DATABASE_NAME)).unwrap();
    writer.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
    let replace = |writer: &Connection, device: &str| {
        let cipher = StoreCipher::new().unwrap();
        let account = Account::with_device_id(user_id!("@original:example.org"), device.into());
        let encoded = rmp_serde::to_vec_named(
            &cipher
                .encrypt_value_data(rmp_serde::to_vec_named(&account.pickle()).unwrap())
                .unwrap(),
        )
        .unwrap();
        writer.execute_batch("BEGIN IMMEDIATE").unwrap();
        writer
            .execute(
                "UPDATE kv SET value = ? WHERE key = 'cipher'",
                [cipher.export("original-passphrase").unwrap()],
            )
            .unwrap();
        writer
            .execute("UPDATE kv SET value = ? WHERE key = 'account'", [encoded])
            .unwrap();
        writer.execute_batch("COMMIT").unwrap();
    };
    replace(&writer, "WAL_LATEST");
    assert!(
        fs::metadata(path(&directory).join(format!("{DATABASE_NAME}-wal")))
            .unwrap()
            .len()
            > 0
    );
    let main_before = fs::read(path(&directory).join(DATABASE_NAME)).unwrap();
    let identity = inspect(&path(&directory), Some("original-passphrase"), || {
        replace(&writer, "NEXT_SNAPSHOT")
    })
    .unwrap();
    assert_eq!(identity.device_id(), device_id!("WAL_LATEST"));
    let next =
        inspect_existing_account_identity(path(&directory), Some("original-passphrase")).unwrap();
    assert_eq!(next.device_id(), device_id!("NEXT_SNAPSHOT"));
    assert_eq!(
        fs::read(path(&directory).join(DATABASE_NAME)).unwrap(),
        main_before
    );
    drop(writer);
    store.close().await.unwrap();
}

#[tokio::test]
async fn inspect_existing_account_identity_observer_cancellation_does_not_cancel_owned_original() {
    let (directory, store) = original(Some("original-passphrase")).await;
    store.close().await.unwrap();
    let (started_tx, started_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    let (observer_tx, observer_rx) = mpsc::sync_channel(1);
    let directory = path(&directory);
    let original = std::thread::spawn(move || {
        let secret = Zeroizing::new("original-passphrase".to_owned());
        let file = fs::File::open(directory.join(DATABASE_NAME)).unwrap();
        let result = inspect(&directory, Some(&secret), || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        assert!(file.metadata().unwrap().len() > 0);
        assert_original(result.unwrap());
        // The observer can disappear; the original still owns SQL + secret and
        // reaches this real completion before its thread can be joined.
        assert!(observer_tx.send(()).is_err());
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    drop(observer_rx);
    assert!(!original.is_finished());
    release_tx.send(()).unwrap();
    original.join().unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn inspect_existing_account_identity_refuses_final_symlink() {
    let (directory, store) = original(None).await;
    store.close().await.unwrap();
    let alias = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        path(&directory).join(DATABASE_NAME),
        alias.path().join(DATABASE_NAME),
    )
    .unwrap();
    assert!(matches!(
        inspect_existing_account_identity(path(&alias), None),
        Err(InspectAccountError::InvalidDatabase)
    ));
}

#[tokio::test]
async fn inspect_existing_account_identity_refuses_excessive_original_kdf_cost() {
    let (directory, store) = original(Some("original-passphrase")).await;
    store.close().await.unwrap();
    let connection = Connection::open(path(&directory).join(DATABASE_NAME)).unwrap();
    let encoded: Vec<u8> = connection
        .query_row("SELECT value FROM kv WHERE key = 'cipher'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut parsed: serde_json::Value = rmp_serde::from_slice(&encoded).unwrap();
    parsed["kdf_info"]["Pbkdf2ToChaCha20Poly1305"]["rounds"] = u32::MAX.into();
    for encoded in [
        rmp_serde::to_vec_named(&parsed).unwrap(),
        serde_json::to_vec(&parsed).unwrap(),
    ] {
        connection
            .execute("UPDATE kv SET value = ? WHERE key = 'cipher'", [encoded])
            .unwrap();
        assert!(matches!(
            inspect_existing_account_identity(path(&directory), Some("original-passphrase")),
            Err(InspectAccountError::KdfRoundLimit {
                found: u32::MAX,
                max: 1_000_000
            })
        ));
    }
}
