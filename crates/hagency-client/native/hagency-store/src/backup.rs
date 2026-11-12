//! Operator snapshot and restore of a native state directory.
//!
//! Parity source of truth: the retained product's only backup is a manual
//! `tar -czf … data/` (`docs/ROLLBACK.md:113-125`), taken with the services
//! stopped. That is UNSAFE for the native store and the parity audit says so
//! (`docs/parity/operator-cli-ops-2026-09-24.md:86`): the databases run in WAL
//! with checkpoints-on-close disabled (`database.rs:114-118`), so committed
//! frames legitimately live in the `-wal` and a plain file copy loses them.
//! Measured: a 50-row table lived entirely in the `-wal`; the main file was
//! 4096 bytes and a main-file-only copy had no table at all.
//!
//! This module captures each database with SQLite's own online backup
//! (`VACUUM INTO`), which reads a consistent snapshot while a writer holds the
//! database open, needs no stop, and writes a self-contained destination with
//! no `-wal`/`-shm` sidecars. It works for a live writer and for a crashed
//! one (a stale `-wal`/`-shm` left by a killed process).
//!
//! Everything else in the directory is copied byte-for-byte, except the
//! transient files that are not state: the SQLite sidecars (already captured
//! inside the destination database) and the `*.lock` ownership files.

use crate::{Error, private};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path, time::Duration};

/// The manifest's schema tag. A restore refuses any other value.
pub const KIND: &str = "hagency-native-backup-v1";
/// The manifest file name inside a snapshot directory.
pub const MANIFEST: &str = "manifest.json";
const DATABASE_SUFFIX: &str = ".sqlite3";
const TRANSIENT_SUFFIXES: [&str; 4] = ["-wal", "-shm", "-journal", ".lock"];
/// A path in a manifest is one relative, in-tree component chain. Anything
/// else could write outside the state directory on restore.
const MAX_PATH: usize = 4096;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    /// State-directory-relative path, `/`-separated.
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub kind: String,
    pub created_at_ms: u64,
    pub entries: Vec<Entry>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn transient(name: &str) -> bool {
    TRANSIENT_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// A manifest path is relative, non-empty, in-tree and bounded. Absolute
/// paths and any `..` component are refused rather than normalised.
fn validate_path(path: &str) -> Result<(), Error> {
    let invalid = || {
        Error::Invalid(hagency_core::InvalidInput(
            "backup manifest contains an unsafe path",
        ))
    };
    if path.is_empty() || path.len() > MAX_PATH || path.starts_with('/') || path.contains('\\') {
        return Err(invalid());
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(invalid());
        }
    }
    Ok(())
}

/// Capture one SQLite database with the online backup API. The source may be
/// held open by a live writer; the destination is refused if it already
/// exists (`VACUUM INTO` will not overwrite).
fn vacuum_into(source: &Path, destination: &Path) -> Result<(), Error> {
    // READ_WRITE, not READ_ONLY: reading a WAL database requires the sidecar
    // `-shm`, which a read-only connection cannot establish. The caller's
    // own store lock is a separate `*.lock` file and is not taken here, so
    // this runs while the service holds the directory.
    let connection =
        rusqlite::Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| Error::Schema)?;
    connection
        .busy_timeout(Duration::from_millis(5_000))
        .map_err(|_| Error::Schema)?;
    // The store's own posture (`database.rs:114-118`): a close is a bounded
    // resource release, NOT a checkpoint. Without this, closing this
    // connection would checkpoint and unlink the SOURCE's `-wal`/`-shm`,
    // mutating the very directory being snapshotted.
    connection
        .set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            true,
        )
        .map_err(|_| Error::Schema)?;
    let destination = destination.to_str().ok_or(Error::Private)?;
    connection
        .execute("VACUUM INTO ?1", [destination])
        .map_err(|_| Error::Schema)?;
    connection.close().map_err(|_| Error::Schema)?;
    Ok(())
}

fn entry_for(path: &str, bytes: &[u8]) -> Entry {
    Entry {
        path: path.to_owned(),
        bytes: bytes.len() as u64,
        sha256: hex(&Sha256::digest(bytes)),
    }
}

/// Walk `directory`, writing every durable file under `out` and recording it.
/// `relative` is the `/`-joined path of `directory` below the state root.
fn capture(
    directory: &Path,
    out: &Path,
    relative: &str,
    entries: &mut Vec<Entry>,
) -> Result<(), Error> {
    for entry in fs::read_dir(directory).map_err(|_| Error::Private)? {
        let entry = entry.map_err(|_| Error::Private)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if relative.is_empty() && name == MANIFEST {
            continue;
        }
        // Skip transient names BEFORE touching the filesystem. A `-wal`/`-shm`
        // may disappear between `read_dir` and the stat that would follow (a
        // concurrent SQLite close unlinks them), and a stat of a vanished
        // entry is not a state error — it is a file that is not state at all.
        if transient(&name) {
            continue;
        }
        let path = entry.path();
        let child = if relative.is_empty() {
            name.clone()
        } else {
            format!("{relative}/{name}")
        };
        let metadata = fs::symlink_metadata(&path).map_err(|_| Error::Private)?;
        // A snapshot is regular files and directories only. A symlink in the
        // state directory is refused rather than followed out of the tree
        // (the private gate already rejects a symlinked directory).
        if metadata.file_type().is_symlink() {
            return Err(Error::Private);
        }
        if metadata.is_dir() {
            let destination = out.join(&name);
            private::create_directory_new(&destination)?;
            capture(&path, &destination, &child, entries)?;
            continue;
        }
        validate_path(&child)?;
        let destination = out.join(&name);
        if name.ends_with(DATABASE_SUFFIX) {
            vacuum_into(&path, &destination)?;
        } else {
            let bytes = fs::read(&path).map_err(|_| Error::Private)?;
            private::write_new(&destination, &bytes)?;
        }
        let captured = fs::read(&destination).map_err(|_| Error::Private)?;
        entries.push(entry_for(&child, &captured));
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(())
}

/// Capture `state` into a new private directory `out`. Refuses an existing
/// `out` so a snapshot can never silently replace an earlier one.
pub fn snapshot(state: &Path, out: &Path, now: u64) -> Result<Manifest, Error> {
    #[cfg(test)]
    hagency_platform::legacy_writer::test_support::initialize_process_root()?;
    let _legacy_writer =
        hagency_platform::legacy_writer::LegacyWriterAdmission::acquire()?;

    private::directory(state)?;
    if out.exists() {
        return Err(Error::Invalid(hagency_core::InvalidInput(
            "backup destination already exists; refusing to overwrite it",
        )));
    }
    private::create_directory_new(out)?;
    let mut entries = Vec::new();
    capture(state, out, "", &mut entries)?;
    let manifest = Manifest {
        kind: KIND.to_owned(),
        created_at_ms: now,
        entries,
    };
    let bytes = serde_json::to_vec_pretty(&manifest).map_err(|_| Error::Schema)?;
    private::write_new(&out.join(MANIFEST), &bytes)?;
    Ok(manifest)
}

/// Read and check a snapshot's manifest, and confirm every recorded file is
/// present with the recorded length and digest. Verification is separate from
/// restore so an operator can prove a snapshot before trusting it.
pub fn verify(from: &Path) -> Result<Manifest, Error> {
    let raw = fs::read(from.join(MANIFEST)).map_err(|_| Error::Private)?;
    let manifest: Manifest = serde_json::from_slice(&raw).map_err(|_| Error::Schema)?;
    if manifest.kind != KIND {
        return Err(Error::Schema);
    }
    for entry in &manifest.entries {
        validate_path(&entry.path)?;
        let bytes = fs::read(from.join(&entry.path)).map_err(|_| Error::Schema)?;
        if bytes.len() as u64 != entry.bytes || hex(&Sha256::digest(&bytes)) != entry.sha256 {
            return Err(Error::Schema);
        }
    }
    Ok(manifest)
}

/// Restore `from` into an empty `state`. The empty-directory rule is `init`'s
/// (`main.rs:217`): existing state is never imported or overwritten.
pub fn restore(state: &Path, from: &Path) -> Result<Manifest, Error> {
    #[cfg(test)]
    hagency_platform::legacy_writer::test_support::initialize_process_root()?;
    let _legacy_writer =
        hagency_platform::legacy_writer::LegacyWriterAdmission::acquire()?;

    let manifest = verify(from)?;
    if state.exists() {
        let mut entries = fs::read_dir(state).map_err(|_| Error::Private)?;
        if entries.next().is_some() {
            return Err(Error::Invalid(hagency_core::InvalidInput(
                "restore requires an empty state directory; no existing data is overwritten",
            )));
        }
    }
    private::directory(state)?;
    for entry in &manifest.entries {
        let bytes = fs::read(from.join(&entry.path)).map_err(|_| Error::Schema)?;
        let destination = state.join(&entry.path);
        if let Some(parent) = destination.parent() {
            private::directory(parent)?;
        }
        private::write_new(&destination, &bytes)?;
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database(path: &Path, rows: u32) {
        // A live WAL writer with checkpoints disabled, exactly like the store.
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .unwrap();
        connection
            .execute("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)", [])
            .unwrap();
        for row in 0..rows {
            connection
                .execute("INSERT INTO t VALUES(?1,?2)", rusqlite::params![row, "x"])
                .unwrap();
        }
        // Deliberately still open: the snapshot must work on a live writer.
        std::mem::forget(connection);
    }

    /// The parity audit's claim — a naive copy is unsafe — is real, and the
    /// online backup is not: the frames live in the WAL and are only
    /// recovered by a reader that goes through SQLite.
    #[test]
    fn native_backup_captures_frames_a_plain_copy_loses() {
        let temporary = tempfile::tempdir().unwrap();
        let state = temporary.path().join("state");
        private::directory(&state).unwrap();
        database(&state.join("domain.sqlite3"), 50);
        private::write_new(&state.join("operator.token"), b"operator-token-value").unwrap();

        // A main-file-only copy loses every frame: the table is not there.
        let naive = temporary.path().join("naive.sqlite3");
        fs::copy(state.join("domain.sqlite3"), &naive).unwrap();
        let connection = rusqlite::Connection::open(&naive).unwrap();
        assert!(
            connection
                .query_row("SELECT COUNT(*) FROM t", [], |r| r.get::<_, u64>(0))
                .is_err(),
            "the naive copy unexpectedly kept the WAL frames"
        );
        drop(connection);

        let out = temporary.path().join("snapshot");
        let manifest = snapshot(&state, &out, 1_700_000_000_000).unwrap();
        assert_eq!(manifest.kind, KIND);
        assert_eq!(
            manifest
                .entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["domain.sqlite3", "operator.token"]
        );
        // The captured database is complete and self-contained.
        let connection = rusqlite::Connection::open(out.join("domain.sqlite3")).unwrap();
        assert_eq!(
            connection
                .query_row("SELECT COUNT(*) FROM t", [], |r| r.get::<_, u64>(0))
                .unwrap(),
            50
        );
        assert_eq!(
            connection
                .pragma_query_value(None, "integrity_check", |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert!(!out.join("domain.sqlite3-wal").exists());
    }
}
