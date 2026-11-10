//! Durable control state and command receipts.
//!
//! SQLite commits hosting intent and the exact command reply in one transaction.
//! Receipts are indexed on disk and never evicted: a delayed retry after a daemon
//! restart cannot become a new operation merely because other commands ran.
use super::protocol::{DaemonError, Reply, Request, errors};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::{path::{Path, PathBuf}, sync::Mutex};

const VERSION: i64 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Intent { Hosting, Paused, Stopped }
impl Intent {
    fn key(self) -> &'static str { match self { Self::Hosting => "hosting", Self::Paused => "paused", Self::Stopped => "stopped" } }
    fn parse(value: &str) -> Result<Self, DaemonError> {
        match value { "hosting" => Ok(Self::Hosting), "paused" => Ok(Self::Paused), "stopped" => Ok(Self::Stopped), _ => Err(errors::STATE_UNAVAILABLE) }
    }
}
pub enum Lookup { New, Replay(Reply), Conflict, Unavailable }
struct State { db: Connection, revision: u64, intent: Intent }
pub struct ControlStore { path: PathBuf, state: Mutex<State> }
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64).unwrap_or(0)
}
fn storage<T>(result: rusqlite::Result<T>) -> Result<T, DaemonError> { result.map_err(|_| errors::STATE_UNAVAILABLE) }
impl ControlStore {
    /// An earlier build imported its JSON control file into `control.sqlite3`
    /// in the transaction that created the control row, and left the JSON in
    /// place. Once that row exists the JSON holds nothing this build needs, so
    /// it is removed at start (product decision: discard earlier-format
    /// hosting data without asking). Without the row the JSON may be the only
    /// record of a paused or stopped intent: it stays and `open` refuses it.
    pub fn discard_imported_json(path: &Path) -> bool {
        let json = path.with_extension("json");
        let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
        if path.extension() != Some(std::ffi::OsStr::new("sqlite3")) || !regular(path) || !regular(&json) {
            return false;
        }
        let imported = Connection::open_with_flags(path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .and_then(|db| db.query_row("SELECT COUNT(*) FROM control WHERE singleton=1", [], |r| r.get::<_, u64>(0)))
            .is_ok_and(|rows| rows == 1);
        imported && std::fs::remove_file(&json).is_ok()
    }
    pub fn open(path: &Path) -> Result<Self, DaemonError> {
        if path.extension() != Some(std::ffi::OsStr::new("sqlite3")) {
            return Err(errors::STATE_UNAVAILABLE);
        }
        // An obsolete file may contain a paused or stopped intent. Refuse it
        // before creating/opening SQLite; never reinterpret it as fresh hosting.
        match std::fs::symlink_metadata(path.with_extension("json")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(errors::STATE_UNAVAILABLE),
        }
        // Creates/checks a 0600, owner-only, single-link non-symlink entry.
        let (file, created) = match hagency_store::private::open(path, true) {
            Ok(file) => (file, true),
            Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists =>
                (hagency_store::private::open(path, false).map_err(|_| errors::STATE_UNAVAILABLE)?, false),
            Err(_) => return Err(errors::STATE_UNAVAILABLE),
        };
        file.sync_all().map_err(|_| errors::STATE_UNAVAILABLE)?;
        // macOS /var is a system symlink; SQLite NOFOLLOW applies to every
        // path component. Resolve it only after validating the actual file.
        let database = path.canonicalize().map_err(|_| errors::STATE_UNAVAILABLE)?;
        let mut db = storage(Connection::open_with_flags(&database,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW))?;
        storage(db.busy_timeout(std::time::Duration::from_secs(2)))?;
        let version: i64 = storage(db.query_row("PRAGMA user_version", [], |r| r.get(0)))?;
        if !created && (version != VERSION || !storage(super::sqlite::has_tables(&db, &[
            ("control", &["singleton", "revision", "intent"]),
            ("receipts", &["request_id", "fingerprint", "recorded_at_ms", "reply"]),
        ]))?) {
            return Err(errors::STATE_UNAVAILABLE);
        }
        // Validate saved intent before any write, including journal-mode changes.
        let (revision, intent) = if created {
            (0, Intent::Hosting)
        } else {
            let count: u64 = storage(db.query_row("SELECT COUNT(*) FROM control", [], |r| r.get(0)))?;
            if count != 1 { return Err(errors::STATE_UNAVAILABLE); }
            let (revision, intent): (u64, String) = storage(db.query_row(
                "SELECT revision,intent FROM control WHERE singleton=1", [], |r| Ok((r.get(0)?,r.get(1)?))))?;
            (revision, Intent::parse(&intent)?)
        };
        storage(db.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;"))?;
        let tx = storage(db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        if created {
            storage(tx.execute_batch("CREATE TABLE control (singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL CHECK(revision>=0), intent TEXT NOT NULL);
                CREATE TABLE receipts (request_id TEXT PRIMARY KEY, fingerprint TEXT NOT NULL, recorded_at_ms INTEGER NOT NULL, reply TEXT NOT NULL); PRAGMA user_version=1;"))?;
            storage(tx.execute("INSERT INTO control VALUES(1,?1,?2)", params![revision,intent.key()]))?;
        }
        let revision = revision.checked_add(1).filter(|v| *v <= i64::MAX as u64).ok_or(errors::STATE_UNAVAILABLE)?;
        storage(tx.execute("UPDATE control SET revision=?1 WHERE singleton=1", [revision]))?;
        storage(tx.commit())?;
        Ok(Self { path:database, state:Mutex::new(State {db,revision,intent}) })
    }
    fn lock(&self) -> std::sync::MutexGuard<'_, State> { self.state.lock().unwrap_or_else(|p|p.into_inner()) }
    fn available(&self) -> bool { hagency_store::private::open(&self.path, false).is_ok() }
    pub fn revision(&self) -> u64 { self.lock().revision }
    pub fn intent(&self) -> Intent { self.lock().intent }
    pub fn lookup(&self, request: &Request) -> Lookup {
        let state = self.lock();
        if !self.available() { return Lookup::Unavailable }
        match state.db.query_row("SELECT fingerprint,reply FROM receipts WHERE request_id=?1", [&request.request_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional() {
            Ok(None) => Lookup::New,
            Ok(Some((fingerprint,reply))) if fingerprint == request.fingerprint() => serde_json::from_str(&reply).map(Lookup::Replay).unwrap_or(Lookup::Unavailable),
            Ok(Some(_)) => Lookup::Conflict,
            Err(_) => Lookup::Unavailable,
        }
    }
    pub fn commit(&self, request: &Request, intent: Option<Intent>, reply: impl FnOnce(u64)->Reply) -> Result<Reply, DaemonError> {
        let mut state = self.lock();
        if !self.available() { return Err(errors::STATE_UNAVAILABLE) }
        let next_intent = intent.unwrap_or(state.intent);
        let revision = if next_intent != state.intent { state.revision.checked_add(1).filter(|v| *v <= i64::MAX as u64).ok_or(errors::STATE_UNAVAILABLE)? } else { state.revision };
        let tx = storage(state.db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate))?;
        let known: bool = storage(tx.query_row("SELECT EXISTS(SELECT 1 FROM receipts WHERE request_id=?1)", [&request.request_id], |r| r.get(0)))?;
        if known { return Err(errors::REQUEST_ID_REUSED) }
        let reply = reply(revision);
        let raw = serde_json::to_string(&reply).map_err(|_| errors::STATE_UNAVAILABLE)?;
        if raw.len() > super::protocol::MAX_FRAME { return Err(errors::STATE_UNAVAILABLE) }
        storage(tx.execute("INSERT INTO receipts VALUES(?1,?2,?3,?4)", params![request.request_id,request.fingerprint(),now_ms(),raw]))?;
        storage(tx.execute("UPDATE control SET revision=?1,intent=?2 WHERE singleton=1", params![revision,next_intent.key()]))?;
        storage(tx.commit())?;
        state.revision = revision;
        state.intent = next_intent;
        Ok(reply)
    }
    pub fn bump(&self) -> Result<u64, DaemonError> {
        let mut state = self.lock();
        if !self.available() { return Err(errors::STATE_UNAVAILABLE) }
        let revision = state.revision.checked_add(1).filter(|v| *v <= i64::MAX as u64).ok_or(errors::STATE_UNAVAILABLE)?;
        storage(state.db.execute("UPDATE control SET revision=?1 WHERE singleton=1", [revision]))?;
        state.revision = revision;
        Ok(revision)
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{Command, Reply};
    use super::*;

    pub(crate) fn private_dir() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        temp
    }

    fn request(id: &str, command: Command) -> Request {
        Request::new(id, Some("@o:s".into()), command)
    }

    #[test]
    fn intent_and_receipt_commit_together_and_survive_restart() {
        let temp = private_dir();
        let path = temp.path().join(super::super::CONTROL);
        let store = ControlStore::open(&path).unwrap();
        assert_eq!(store.revision(), 1);
        assert_eq!(store.intent(), Intent::Hosting);
        let pause = request("p1", Command::PauseHosting);
        let reply = store
            .commit(&pause, Some(Intent::Paused), |rev| {
                Reply::ok("p1", rev, serde_json::json!({"rev":rev}))
            })
            .unwrap();
        assert_eq!(reply.revision, 2);
        drop(store);
        let reopened = ControlStore::open(&path).unwrap();
        assert_eq!(reopened.intent(), Intent::Paused);
        // Each daemon start advances the revision once.
        assert_eq!(reopened.revision(), 3);
        match reopened.lookup(&pause) {
            Lookup::Replay(saved) => assert_eq!(saved, reply),
            _ => panic!("receipt lost across restart"),
        }
        let reused = request("p1", Command::StopHosting);
        assert!(matches!(reopened.lookup(&reused), Lookup::Conflict));
        assert_eq!(
            reopened
                .commit(&reused, Some(Intent::Stopped), |r| Reply::ok("p1", r, serde_json::json!({})))
                .unwrap_err(),
            errors::REQUEST_ID_REUSED
        );
        assert_eq!(reopened.intent(), Intent::Paused);
    }

    #[test]
    fn unchanged_intent_records_receipt_without_revision_change() {
        let temp = private_dir();
        let store = ControlStore::open(&temp.path().join(super::super::CONTROL)).unwrap();
        let before = store.revision();
        store
            .commit(&request("a", Command::ResumeHosting), Some(Intent::Hosting), |r| {
                Reply::ok("a", r, serde_json::json!({}))
            })
            .unwrap();
        assert_eq!(store.revision(), before);
        assert!(matches!(
            store.lookup(&request("a", Command::ResumeHosting)),
            Lookup::Replay(_)
        ));
    }

    #[test]
    fn command_receipts_remain_durable_after_many_commands_and_restart() {
        let temp = private_dir();
        let store = ControlStore::open(&temp.path().join(super::super::CONTROL)).unwrap();
        for i in 0..300 {
            let id = format!("r{i}");
            store
                .commit(&request(&id, Command::PauseHosting), None, |r| {
                    Reply::ok(&id, r, serde_json::json!({}))
                })
                .unwrap();
        }
        assert!(matches!(
            store.lookup(&request("r0", Command::PauseHosting)),
            Lookup::Replay(_)
        ));
        assert!(matches!(
            store.lookup(&request("r299", Command::PauseHosting)),
            Lookup::Replay(_)
        ));
        drop(store);
        let store = ControlStore::open(&temp.path().join(super::super::CONTROL)).unwrap();
        assert!(matches!(store.lookup(&request("r0", Command::PauseHosting)), Lookup::Replay(_)));
        assert!(matches!(store.lookup(&request("r0", Command::StopHosting)), Lookup::Conflict));
    }

    #[test]
    fn failed_persistence_applies_nothing() {
        let temp = private_dir();
        let dir = temp.path().join("gone");
        hagency_store::private::directory(&dir).unwrap();
        let store = ControlStore::open(&dir.join(super::super::CONTROL)).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let error = store
            .commit(&request("x", Command::StopHosting), Some(Intent::Stopped), |r| {
                Reply::ok("x", r, serde_json::json!({}))
            })
            .unwrap_err();
        assert_eq!(error, errors::STATE_UNAVAILABLE);
        assert_eq!(store.intent(), Intent::Hosting);
        assert!(matches!(
            store.lookup(&request("x", Command::StopHosting)),
            Lookup::Unavailable
        ));
    }

    #[test]
    fn corrupt_or_foreign_state_is_refused() {
        let temp = private_dir();
        let path = temp.path().join(super::super::CONTROL);
        hagency_store::private::replace(&path, b"{\"format\":\"other\"}").unwrap();
        assert!(ControlStore::open(&path).is_err());
    }

    #[test]
    fn old_json_intent_is_preserved_and_never_defaults_to_hosting() {
        for intent in ["hosting", "paused", "stopped", "future"] {
            let temp = private_dir();
            let path = temp.path().join(super::super::CONTROL);
            let old_path = temp.path().join("control.json");
            let raw = serde_json::to_vec(&serde_json::json!({
                "format":"hagency-owner-daemon-control", "version":1,
                "revision":17, "intent":intent, "receipts":[]
            })).unwrap();
            hagency_store::private::replace(&old_path, &raw).unwrap();
            assert!(matches!(ControlStore::open(&path), Err(errors::STATE_UNAVAILABLE)));
            assert!(matches!(ControlStore::open(&old_path), Err(errors::STATE_UNAVAILABLE)));
            assert_eq!(std::fs::read(&old_path).unwrap(), raw);
            assert!(!path.exists(), "unsupported intent created fresh hosting state");
        }
    }

    #[test]
    fn old_json_beside_current_sqlite_refuses_without_changing_either() {
        let temp = private_dir();
        let path = temp.path().join(super::super::CONTROL);
        drop(ControlStore::open(&path).unwrap());
        let before = std::fs::read(&path).unwrap();
        let old_path = temp.path().join("control.json");
        let raw = br#"{"format":"hagency-owner-daemon-control","version":1,"revision":4,"intent":"paused","receipts":[]}"#;
        hagency_store::private::replace(&old_path, raw).unwrap();
        assert!(matches!(ControlStore::open(&path), Err(errors::STATE_UNAVAILABLE)));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(&old_path).unwrap(), raw);
    }

    /// The earlier build that imported the JSON left it in place. At start it
    /// is removed once the SQLite store holds the control row, and the intent
    /// that store recorded is the one that continues.
    #[test]
    fn an_imported_old_json_is_removed_at_start_and_the_saved_intent_continues() {
        let temp = private_dir();
        let path = temp.path().join(super::super::CONTROL);
        let store = ControlStore::open(&path).unwrap();
        store.commit(&request("stopped", Command::StopHosting), Some(Intent::Stopped), |rev| {
            Reply::ok("stopped", rev, serde_json::json!({}))
        }).unwrap();
        drop(store);
        let old_path = temp.path().join("control.json");
        hagency_store::private::replace(&old_path,
            br#"{"format":"hagency-owner-daemon-control","version":1,"revision":4,"intent":"hosting","receipts":[]}"#).unwrap();
        assert!(matches!(ControlStore::open(&path), Err(errors::STATE_UNAVAILABLE)));
        assert!(ControlStore::discard_imported_json(&path));
        assert!(!old_path.exists());
        assert!(!ControlStore::discard_imported_json(&path), "nothing is left to remove");
        assert_eq!(ControlStore::open(&path).unwrap().intent(), Intent::Stopped);
    }

    /// Without a complete SQLite store the JSON may be the only record of a
    /// paused or stopped intent. It is never removed.
    #[test]
    fn an_old_json_that_was_never_imported_is_kept_and_still_refused() {
        let raw = br#"{"format":"hagency-owner-daemon-control","version":1,"revision":4,"intent":"paused","receipts":[]}"#;
        for sqlite in [None, Some(""), Some("DELETE FROM control")] {
            let temp = private_dir();
            let path = temp.path().join(super::super::CONTROL);
            match sqlite {
                None => {}
                Some("") => drop(hagency_store::private::open(&path, true).unwrap()),
                Some(change) => {
                    drop(ControlStore::open(&path).unwrap());
                    Connection::open(&path).unwrap().execute_batch(change).unwrap();
                }
            }
            let old_path = temp.path().join("control.json");
            hagency_store::private::replace(&old_path, raw).unwrap();
            assert!(!ControlStore::discard_imported_json(&path), "{sqlite:?}");
            assert_eq!(std::fs::read(&old_path).unwrap(), raw);
            assert!(matches!(ControlStore::open(&path), Err(errors::STATE_UNAVAILABLE)));
        }
        // No JSON at all: nothing to do, and the store is not created.
        let temp = private_dir();
        let path = temp.path().join(super::super::CONTROL);
        assert!(!ControlStore::discard_imported_json(&path));
        assert!(!path.exists());
    }

    #[test]
    fn incomplete_or_unknown_sqlite_is_never_repaired_as_fresh_hosting() {
        for change in [
            "PRAGMA user_version=0",
            "PRAGMA user_version=9",
            "DELETE FROM control",
            "UPDATE control SET intent='future'",
            "ALTER TABLE control ADD COLUMN handover TEXT",
            "DROP TABLE receipts",
        ] {
            let temp = private_dir();
            let path = temp.path().join(super::super::CONTROL);
            let store = ControlStore::open(&path).unwrap();
            store.commit(&request("paused", Command::PauseHosting), Some(Intent::Paused), |rev| {
                Reply::ok("paused", rev, serde_json::json!({}))
            }).unwrap();
            drop(store);
            let db = Connection::open(&path).unwrap();
            db.execute_batch(change).unwrap();
            drop(db);
            let before = std::fs::read(&path).unwrap();
            assert!(matches!(ControlStore::open(&path), Err(errors::STATE_UNAVAILABLE)), "{change}");
            assert_eq!(std::fs::read(&path).unwrap(), before, "{change}");
        }
        let temp = private_dir();
        let path = temp.path().join(super::super::CONTROL);
        hagency_store::private::write_new(&path, b"").unwrap();
        assert!(matches!(ControlStore::open(&path), Err(errors::STATE_UNAVAILABLE)));
        assert!(std::fs::read(&path).unwrap().is_empty());
    }
}
