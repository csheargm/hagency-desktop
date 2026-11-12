//! The personal ledger as an earlier Hagency build left it.
//!
//! When that build set up pools on a computer it moved the owner's
//! local-Agent records into its resource database and locked this file as an
//! archive: application id "HAR1" and one identity row that can be neither
//! changed nor removed. This build runs local Agents beside pools again and
//! opens only its own ledger, so the archive kept every local Agent of that
//! owner from running, with no way out for the owner.
//!
//! Product decision of the owner (2026-11-10): delete it when the ledger is
//! first needed, without asking, and begin with an empty ledger. Usage and
//! session records of local Agents from before the pools are not carried
//! over. Only a file that carries that build's marker for this same owner and
//! profile is removed. Every other file that this build cannot open stays
//! where it is and stays an error.
use super::Error;
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};

/// "HAR1": the earlier build's stamp on an archived ledger.
const ARCHIVE_ID: i64 = 0x4841_5231;
/// The earlier build wrote at most this much into the identity row.
const LIMIT: usize = 4096;

/// One removal at a time in this process. The second caller looks again and
/// finds the new ledger, which is not an archive.
static REMOVAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// True only for a readable archive of exactly this owner and profile.
fn is_archive(path: &Path, owner: &str, profile: &str) -> bool {
    // The file itself must be a plain file. Folders above it may be links
    // (a temporary folder on macOS is), so SQLite's own no-link flag, which
    // covers the whole path, is not used.
    if !std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_file()) {
        return false;
    }
    let Ok(db) = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) else {
        return false;
    };
    if db
        .pragma_query_value(None, "application_id", |row| row.get::<_, i64>(0))
        .ok()
        != Some(ARCHIVE_ID)
    {
        return false;
    }
    let Ok(Some(raw)) = db.query_row(
        "SELECT CASE WHEN length(CAST(manifest AS BLOB))<=?1 THEN manifest END \
         FROM personal_archive_identity WHERE singleton=1",
        [LIMIT as i64],
        |row| row.get::<_, Option<String>>(0),
    ) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    manifest["version"] == 1
        && manifest["kind"] == "personal"
        && manifest["ownerMxid"] == owner
        && manifest["profileDigest"] == profile
}

fn sidecars(path: &Path) -> [PathBuf; 3] {
    ["-wal", "-shm", "-journal"].map(|suffix| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    })
}

/// Remove the earlier build's archive at `path`, if that is what the file is.
/// `Ok(true)` when it was removed and the caller starts an empty ledger.
pub(super) fn discard(path: &Path, owner: &str, profile: &str) -> Result<bool, Error> {
    if !is_archive(path, owner, profile) {
        return Ok(false);
    }
    let _one = REMOVAL.lock().map_err(|_| Error::Unavailable)?;
    if !is_archive(path, owner, profile) {
        return Ok(false);
    }
    // The database first: if this stops half way, what remains is no longer
    // an archive, and SQLite ignores stray side files of a new database.
    std::fs::remove_file(path).map_err(|_| Error::Unavailable)?;
    for side in sidecars(path) {
        match std::fs::remove_file(&side) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::Unavailable),
        }
    }
    tracing::warn!("Removed the local-Agent ledger archived by an earlier build; starting an empty one");
    Ok(true)
}

/// The file as the earlier build left it: a current ledger, then the
/// identity row with its two guards, then the archive stamp.
#[cfg(test)]
pub(super) fn archive(path: &Path, owner: &str, profile: &str) {
    drop(hagency_agent_local::Ledger::open_scoped(path, owner, profile).unwrap());
    let db = Connection::open(path).unwrap();
    db.execute_batch(
        "CREATE TABLE personal_archive_identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),manifest TEXT NOT NULL); \
         CREATE TRIGGER personal_archive_identity_retained BEFORE DELETE ON personal_archive_identity BEGIN SELECT RAISE(ABORT,'retain original archive identity'); END; \
         CREATE TRIGGER personal_archive_identity_immutable BEFORE UPDATE ON personal_archive_identity BEGIN SELECT RAISE(ABORT,'immutable archive identity'); END;",
    )
    .unwrap();
    let manifest = serde_json::json!({"version":1,"kind":"personal","migrationId":"m","registryId":"r",
        "profileDigest":profile,"ownerMxid":owner,"inventoryDigest":"i","sourceSchema":110});
    db.execute(
        "INSERT INTO personal_archive_identity VALUES(1,?1)",
        [manifest.to_string()],
    )
    .unwrap();
    db.pragma_update(None, "application_id", ARCHIVE_ID).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: &str = "@owner:test";

    fn profile() -> String {
        "a".repeat(64)
    }

    #[test]
    fn earlier_archive_of_this_profile_is_removed_and_an_empty_ledger_opens() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = temp.path().join("agent-local.sqlite");
        archive(&path, OWNER, &profile());
        // This is what kept local Agents from running.
        assert!(hagency_agent_local::Ledger::open_scoped(&path, OWNER, &profile()).is_err());
        // Empty side files, as SQLite leaves them after a clean close.
        for side in sidecars(&path) {
            std::fs::write(side, b"").unwrap();
        }
        assert!(discard(&path, OWNER, &profile()).unwrap());
        assert!(!path.exists());
        assert!(sidecars(&path).iter().all(|side| !side.exists()));
        drop(hagency_agent_local::Ledger::open_scoped(&path, OWNER, &profile()).unwrap());
        // The new ledger is not an archive: nothing is removed a second time.
        assert!(!discard(&path, OWNER, &profile()).unwrap());
        assert!(path.exists());
    }

    #[test]
    fn only_that_builds_archive_of_the_same_owner_and_profile_is_removed() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let other = "b".repeat(64);
        // A missing file, a current ledger, and a file that is not a database.
        let missing = temp.path().join("missing.sqlite");
        assert!(!discard(&missing, OWNER, &profile()).unwrap());
        let current = temp.path().join("current.sqlite");
        drop(hagency_agent_local::Ledger::open_scoped(&current, OWNER, &profile()).unwrap());
        assert!(!discard(&current, OWNER, &profile()).unwrap());
        let noise = temp.path().join("noise.sqlite");
        std::fs::write(&noise, b"not a database").unwrap();
        assert!(!discard(&noise, OWNER, &profile()).unwrap());
        // An archive of another profile or another owner.
        let foreign = temp.path().join("foreign.sqlite");
        archive(&foreign, OWNER, &other);
        assert!(!discard(&foreign, OWNER, &profile()).unwrap());
        assert!(!discard(&foreign, "@other:test", &other).unwrap());
        // The stamp without the earlier build's identity row, and a row of
        // another kind or a later format.
        let stamped = temp.path().join("stamped.sqlite");
        drop(hagency_agent_local::Ledger::open_scoped(&stamped, OWNER, &profile()).unwrap());
        Connection::open(&stamped)
            .unwrap()
            .pragma_update(None, "application_id", ARCHIVE_ID)
            .unwrap();
        assert!(!discard(&stamped, OWNER, &profile()).unwrap());
        for (key, value) in [
            ("kind", serde_json::json!("domain")),
            ("version", serde_json::json!(2)),
        ] {
            let odd = temp.path().join(format!("odd-{key}.sqlite"));
            archive(&odd, OWNER, &profile());
            let db = Connection::open(&odd).unwrap();
            let raw: String = db
                .query_row("SELECT manifest FROM personal_archive_identity", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let mut manifest: serde_json::Value = serde_json::from_str(&raw).unwrap();
            manifest[key] = value;
            db.execute_batch("DROP TRIGGER personal_archive_identity_immutable").unwrap();
            db.execute(
                "UPDATE personal_archive_identity SET manifest=?1",
                [manifest.to_string()],
            )
            .unwrap();
            drop(db);
            assert!(!discard(&odd, OWNER, &profile()).unwrap(), "{key}");
            assert!(odd.exists());
        }
        for kept in [&current, &noise, &foreign, &stamped] {
            assert!(kept.exists());
        }
    }
}
