//! Durable admission before effects. Pending after a crash is unknown, never
//! replayed as a second mutation. Native journals retain original command IDs.
use super::error;
#[cfg(any(unix, test))]
use super::Failure;
use crate::native_owner::NativeError;
use rusqlite::{Connection, params};
#[cfg(any(unix, test))]
use rusqlite::OptionalExtension;
#[cfg(any(unix, test))]
use serde_json::Value;
use std::{path::Path, sync::Mutex};
#[cfg(any(unix, test))]
use std::collections::{HashMap, HashSet};

pub struct Journal(
    Mutex<Connection>,
    #[cfg(any(unix, test))] Mutex<HashSet<String>>,
    #[cfg(any(unix, test))] Mutex<HashMap<String, Result<Value, Failure>>>,
);
const RETRY_WINDOW_MS: u64 = 24 * 60 * 60 * 1000;
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCommand {
    pub request_id: String,
    pub created_at_ms: u64,
    pub retry_until_ms: u64,
}

#[cfg(any(unix, test))]
pub enum Admission {
    New,
    Pending,
    RecoveryRequired,
    Result(Result<Value, Failure>),
}
impl Journal {
    pub fn open(path: &Path) -> Result<Self, NativeError> {
        let (file, created) = match hagency_store::private::open(path, true) {
            Ok(file) => (file, true),
            Err(hagency_store::Error::Io(io_error)) if io_error.kind() == std::io::ErrorKind::AlreadyExists => (
                hagency_store::private::open(path, false)
                    .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?,
                false,
            ),
            Err(_) => return Err(error(503, "daemon_bridge_journal_unavailable")),
        };
        drop(file);
        let path = path
            .canonicalize()
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let db = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        db.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let version: i64 = db.query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        if !created {
            let current = version == 0 && super::super::sqlite::has_tables(&db, &[
                ("calls", &["id", "fingerprint", "response", "recovery_version", "native_id"]),
                ("pending", &["fingerprint", "id", "created_at_ms", "owner", "retry_until_ms"]),
            ]).map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
            if !current {
                return Err(error(503, "daemon_bridge_journal_unavailable"));
            }
            let unsupported: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM calls WHERE recovery_version IS NOT 1)
                    OR EXISTS(SELECT 1 FROM pending WHERE owner IS NULL OR owner='' OR created_at_ms<=0 OR retry_until_ms<created_at_ms)",
                [], |row| row.get(0),
            ).map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
            if unsupported {
                return Err(error(503, "daemon_bridge_journal_unavailable"));
            }
        }
        // Only a file created by this call gets initialized. Existing files
        // must already carry the complete current recovery contract.
        db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;")
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        if created {
            let tx = db.unchecked_transaction().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
            tx.execute_batch("CREATE TABLE calls(id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, response TEXT, recovery_version INTEGER NOT NULL DEFAULT 0, native_id TEXT);
                CREATE TABLE pending(fingerprint TEXT PRIMARY KEY, id TEXT NOT NULL, created_at_ms INTEGER NOT NULL DEFAULT 0, owner TEXT NOT NULL DEFAULT '', retry_until_ms INTEGER NOT NULL DEFAULT 0);")
                .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
            tx.commit().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        }
        Ok(Self(
            Mutex::new(db),
            #[cfg(any(unix, test))] Mutex::new(HashSet::new()),
            #[cfg(any(unix, test))] Mutex::new(HashMap::new()),
        ))
    }
    #[cfg(any(unix, test))]
    pub fn admit(&self, id: &str, fingerprint: &str) -> Result<Admission, NativeError> {
        // If persistence failed after completion, retry only the receipt write.
        // The native operation must never be invoked a second time.
        let completed = self.2.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?.get(id).cloned();
        if let Some(result) = completed { self.finish(id, &result)?; }
        let mut db = self
            .0
            .lock()
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let existing: Option<(String, Option<String>, i64)> = tx
            .query_row(
                "SELECT fingerprint,response,recovery_version FROM calls WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let result = match existing {
            Some((_, _, version)) if version != 1 => return Err(error(503, "daemon_bridge_journal_unavailable")),
            Some((old, _, _)) if old != fingerprint => return Err(error(409, "request_id_reused")),
            Some((_, Some(response), _)) => {
                let result=serde_json::from_str(&response).map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?;
                if super::terminal_receipt(&result) {Admission::Result(result)} else {Admission::RecoveryRequired}
            },
            Some((_, None, _)) => {
                if self.1.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?.contains(id) { Admission::Pending }
                else { Admission::RecoveryRequired }
            },
            None => {
                tx.execute(
                    "INSERT INTO calls(id,fingerprint,recovery_version) VALUES(?1,?2,1)",
                    params![id, fingerprint],
                )
                .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
                Admission::New
            }
        };
        tx.commit()
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        if matches!(result, Admission::New) { self.1.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?.insert(id.into()); }
        Ok(result)
    }
    /// Take a command that needs recovery back into flight, for a command
    /// that may run again. False when it is already running: two retries of
    /// one request never run side by side.
    #[cfg(any(unix, test))]
    pub fn reenter(&self, id: &str) -> Result<bool, NativeError> {
        Ok(self.1.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?.insert(id.into()))
    }
    /// An admitted current command with no pin has not entered native execution.
    #[cfg(any(unix, test))]
    pub fn native_pin(&self, id: &str) -> Result<Option<String>, NativeError> {
        self.0.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?
            .query_row("SELECT native_id FROM calls WHERE id=?1 AND recovery_version=1", [id], |row| row.get(0))
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))
    }
    #[cfg(any(unix, test))]
    pub fn pin_native(&self, id: &str, native: &str) -> Result<(), NativeError> {
        let n = self.0.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?
            .execute("UPDATE calls SET native_id=?1 WHERE id=?2 AND recovery_version=1 AND (native_id IS NULL OR native_id=?1)", params![native,id])
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        if n != 1 { return Err(error(409, "daemon_command_recovery_required")); }
        Ok(())
    }
    #[cfg(any(unix, test))]
    pub fn finish(&self, id: &str, result: &Result<Value, Failure>) -> Result<(), NativeError> {
        self.2.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?.insert(id.into(), result.clone());
        let raw = serde_json::to_string(result)
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        {
            let mut db=self.0.lock().map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?;
            let tx=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?;
            let previous:Option<String>=tx.query_row("SELECT response FROM calls WHERE id=?1",[id],|r|r.get(0)).map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?;
            let terminal=previous.map(|raw|serde_json::from_str(&raw)).transpose().map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?.as_ref().is_some_and(super::terminal_receipt);
            if !terminal {tx.execute("UPDATE calls SET response=?1 WHERE id=?2",params![raw,id]).map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?;}
            tx.commit().map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?;
        }
        self.1.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?.remove(id);
        self.2.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?.remove(id);
        Ok(())
    }
    #[cfg(test)]
    pub fn pending(&self, fingerprint: &str) -> Result<String, NativeError> { self.pending_for("@owner:test", fingerprint) }
    pub fn pending_for(&self, owner: &str, fingerprint: &str) -> Result<String, NativeError> {
        if owner.is_empty() { return Err(error(503, "daemon_bridge_journal_unavailable")); }
        let db = self.0.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let id = super::super::new_request_id();
        let now = super::super::control::now_ms();
        db.execute("INSERT OR IGNORE INTO pending(fingerprint,id,created_at_ms,owner,retry_until_ms) VALUES(?1,?2,?3,?4,?5)", params![fingerprint,id,now,owner,now.saturating_add(RETRY_WINDOW_MS)])
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        let (id, until): (String,u64) = db.query_row("SELECT id,retry_until_ms FROM pending WHERE fingerprint=?1 AND owner=?2", params![fingerprint,owner], |r| Ok((r.get(0)?,r.get(1)?)))
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        if now > until { return Err(error(409, "daemon_command_receipt_expired")); }
        Ok(id)
    }
    pub fn pending_commands(&self, owner: &str) -> Result<Vec<PendingCommand>, NativeError> {
        let db = self.0.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        db.prepare("SELECT id,created_at_ms,retry_until_ms FROM pending WHERE owner=?1 ORDER BY created_at_ms,id LIMIT 256")
            .and_then(|mut s| s.query_map([owner], |r| { let created_at_ms: u64 = r.get(1)?; Ok(PendingCommand { request_id:r.get(0)?, created_at_ms, retry_until_ms:r.get(2)? }) })?.collect())
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))
    }
    /// Explicitly reopen recovery of this original command; never mint a new ID.
    pub fn resume_pending(&self, owner: &str, id: &str) -> Result<(), NativeError> {
        let n = self.0.lock().map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?
            .execute("UPDATE pending SET retry_until_ms=?1 WHERE owner=?2 AND id=?3", params![super::super::control::now_ms().saturating_add(RETRY_WINDOW_MS),owner,id])
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        if n != 1 { return Err(error(404, "daemon_command_not_found")); }
        Ok(())
    }
    /// Used only by explicit original-command recovery entrypoints, never by
    /// a new mutation with coincidentally identical arguments.
    pub fn resume_fingerprint(&self, owner:&str, fingerprint:&str) -> Result<(),NativeError> {
        let n=self.0.lock().map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?
            .execute("UPDATE pending SET retry_until_ms=?1 WHERE owner=?2 AND fingerprint=?3",params![super::super::control::now_ms().saturating_add(RETRY_WINDOW_MS),owner,fingerprint])
            .map_err(|_|error(503,"daemon_bridge_journal_unavailable"))?;
        if n!=1 { return Err(error(404,"daemon_command_not_found")); }
        Ok(())
    }
    pub fn clear(&self, fingerprint: &str, id: &str) -> Result<(), NativeError> {
        self.0
            .lock()
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?
            .execute(
                "DELETE FROM pending WHERE fingerprint=?1 AND id=?2",
                params![fingerprint, id],
            )
            .map_err(|_| error(503, "daemon_bridge_journal_unavailable"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_command_survives_reopen_and_is_not_executed_twice() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal.sqlite3");
        let journal = Journal::open(&path).unwrap();
        let id = journal.pending("owner-package-command").unwrap();
        assert!(matches!(
            journal.admit(&id, "fingerprint").unwrap(),
            Admission::New
        ));
        drop(journal);
        let journal = Journal::open(&path).unwrap();
        assert_eq!(journal.pending("owner-package-command").unwrap(), id);
        assert!(matches!(
            journal.admit(&id, "fingerprint").unwrap(),
            Admission::RecoveryRequired
        ));
        assert_eq!(
            journal.admit(&id, "other").err().unwrap().code,
            "request_id_reused"
        );
        journal
            .finish(&id, &Ok(serde_json::json!({"originalId":"native-123"})))
            .unwrap();
        drop(journal);
        let journal = Journal::open(&path).unwrap();
        let Admission::Result(Ok(value)) = journal.admit(&id, "fingerprint").unwrap() else {
            panic!("must replay receipt")
        };
        assert_eq!(value["originalId"], "native-123");
        journal.clear("owner-package-command", "wrong-id").unwrap();
        assert_eq!(journal.pending("owner-package-command").unwrap(), id);
        journal.clear("owner-package-command", &id).unwrap();
        assert_ne!(journal.pending("owner-package-command").unwrap(), id);
    }
    #[test]
    fn receipt_write_failure_retries_only_persistence_and_expiry_keeps_original_id() {
        let temp = tempfile::tempdir().unwrap();
        let journal = Journal::open(&temp.path().join("journal")).unwrap();
        let id = journal.pending_for("@owner:test", "fingerprint").unwrap();
        assert!(matches!(journal.admit(&id,"fingerprint").unwrap(),Admission::New));
        journal.0.lock().unwrap().execute_batch("PRAGMA query_only=ON").unwrap();
        assert!(journal.finish(&id,&Ok(serde_json::json!({"original":"native-1"}))).is_err());
        journal.0.lock().unwrap().execute_batch("PRAGMA query_only=OFF").unwrap();
        let Admission::Result(Ok(value))=journal.admit(&id,"fingerprint").unwrap() else {panic!("completed effects must replay")};
        assert_eq!(value["original"],"native-1");
        journal.0.lock().unwrap().execute("UPDATE pending SET retry_until_ms=1",[]).unwrap();
        assert_eq!(journal.pending_for("@owner:test","fingerprint").unwrap_err().code,"daemon_command_receipt_expired");
        assert_eq!(journal.pending_commands("@owner:test").unwrap()[0].request_id,id);
        assert!(journal.pending_commands("@other:test").unwrap().is_empty());
        assert!(journal.resume_pending("@other:test",&id).is_err());
        journal.resume_pending("@owner:test",&id).unwrap();
        assert_eq!(journal.pending_for("@owner:test","fingerprint").unwrap(),id);
    }
    #[test]
    fn native_pin_is_immutable_and_preserved_across_restart() {
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("journal");
        let journal=Journal::open(&path).unwrap();
        journal.admit("current","print").unwrap();
        assert_eq!(journal.native_pin("current").unwrap(),None);
        journal.pin_native("current","begin-original").unwrap();
        assert!(journal.pin_native("current","replacement").is_err());
        drop(journal);let journal=Journal::open(&path).unwrap();
        assert_eq!(journal.native_pin("current").unwrap(),Some("begin-original".into()));
        assert!(matches!(journal.admit("current","print").unwrap(),Admission::RecoveryRequired));
    }
    #[test]
    fn obsolete_schema_is_refused_without_upgrading_or_dropping_receipts() {
        for sql in [
            "CREATE TABLE calls(id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, response TEXT);
             CREATE TABLE pending(fingerprint TEXT PRIMARY KEY, id TEXT NOT NULL);
             INSERT INTO calls VALUES('original','scope',NULL);
             INSERT INTO pending VALUES('scope','original');",
            "CREATE TABLE calls(id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, response TEXT, recovery_version INTEGER NOT NULL DEFAULT 0, native_id TEXT);
             CREATE TABLE pending(fingerprint TEXT PRIMARY KEY, id TEXT NOT NULL, created_at_ms INTEGER NOT NULL DEFAULT 0, owner TEXT NOT NULL DEFAULT '');",
            "",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("journal");
            drop(hagency_store::private::open(&path, true).unwrap());
            let db = Connection::open(&path).unwrap();
            db.execute_batch(sql).unwrap();
            drop(db);
            let before = std::fs::read(&path).unwrap();
            assert_eq!(Journal::open(&path).err().unwrap().code, "daemon_bridge_journal_unavailable");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
    #[test]
    fn unsupported_recovery_rows_and_unowned_pending_are_never_adopted() {
        for sql in [
            "INSERT INTO calls(id,fingerprint,recovery_version) VALUES('original','scope',0)",
            "INSERT INTO calls(id,fingerprint,recovery_version) VALUES('original','scope',2)",
            "INSERT INTO pending(fingerprint,id,created_at_ms,owner,retry_until_ms) VALUES('scope','original',1,'',9999999999999)",
            "INSERT INTO pending(fingerprint,id,created_at_ms,owner,retry_until_ms) VALUES('scope','original',0,'@owner:test',9999999999999)",
            "PRAGMA user_version=9",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("journal");
            let journal = Journal::open(&path).unwrap();
            journal.0.lock().unwrap().execute_batch(sql).unwrap();
            if sql.contains("INSERT INTO calls") {
                assert!(journal.admit("original", "scope").is_err());
                assert!(journal.native_pin("original").is_err());
                assert!(journal.pin_native("original", "replacement").is_err());
            }
            if sql.contains("VALUES('scope','original',1,'',") {
                assert!(journal.pending_for("@owner:test", "scope").is_err());
                let owner: String = journal.0.lock().unwrap().query_row(
                    "SELECT owner FROM pending WHERE id='original'", [], |row| row.get(0)
                ).unwrap();
                assert!(owner.is_empty());
            }
            drop(journal);
            let before = std::fs::read(&path).unwrap();
            assert_eq!(Journal::open(&path).err().unwrap().code, "daemon_bridge_journal_unavailable");
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
    #[test]
    fn a_refused_command_that_may_run_again_reenters_once_and_keeps_its_last_answer() {
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("journal");let journal=Journal::open(&path).unwrap();
        let id=journal.pending_for("owner","start").unwrap();
        assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::New));
        journal.finish(&id,&Err(Failure::from(error(409,"runtime_profile_unavailable")))).unwrap();
        // Refused, not final: the same request is not a new one.
        assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::RecoveryRequired));
        assert!(journal.reenter(&id).unwrap());
        // While it runs, the same request waits instead of running beside it.
        assert!(!journal.reenter(&id).unwrap());
        assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::RecoveryRequired));
        // Refused again: still open for the next try.
        journal.finish(&id,&Err(Failure::from(error(409,"runtime_profile_unavailable")))).unwrap();
        assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::RecoveryRequired));
        assert!(journal.reenter(&id).unwrap());
        journal.finish(&id,&Ok(serde_json::json!({"started":true}))).unwrap();
        drop(journal);let journal=Journal::open(&path).unwrap();
        assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::Result(Ok(_))));
        // After a restart cut a run short, the admitted request can be taken up again.
        let cut=journal.pending_for("owner","stop").unwrap();
        assert!(matches!(journal.admit(&cut,"scope-2").unwrap(),Admission::New));
        drop(journal);let journal=Journal::open(&path).unwrap();
        assert!(matches!(journal.admit(&cut,"scope-2").unwrap(),Admission::RecoveryRequired));
        assert!(journal.reenter(&cut).unwrap());
    }
    #[test]
    fn recorded_native_unknown_is_reconciled_to_terminal_receipt_under_same_id() {
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("journal");let journal=Journal::open(&path).unwrap();
        let id=journal.pending_for("owner","resume").unwrap();journal.admit(&id,"scope").unwrap();journal.pin_native(&id,"original-begin").unwrap();
        journal.finish(&id,&Err(Failure::from(error(503,"management_outcome_unknown")))).unwrap();
        assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::RecoveryRequired));
        assert_eq!(journal.native_pin(&id).unwrap(),Some("original-begin".into()));
        journal.finish(&id,&Ok(serde_json::json!({"commandId":"original-begin"}))).unwrap();
        assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::Result(Ok(_))));
        // A late unknown reply cannot replace the proven original result.
        journal.finish(&id,&Err(Failure::from(error(503,"management_outcome_unknown")))).unwrap();
        drop(journal);let journal=Journal::open(&path).unwrap();assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::Result(Ok(_))));
    }
    #[cfg(unix)]
    #[test]
    fn journal_refuses_a_symlink_and_public_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("journal");
        std::fs::write(&path, b"").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Journal::open(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let alias = temp.path().join("alias");
        symlink(&path, &alias).unwrap();
        assert!(Journal::open(&alias).is_err());
    }
}
