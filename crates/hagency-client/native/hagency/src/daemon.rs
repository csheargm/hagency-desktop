//! Per-user Hagency owner daemon (ADR 0012, ADR-192).
//!
//! On macOS the daemon is the only process that opens an owner root and the
//! resource ledger. It hosts the owner's one registered root
//! ([`crate::owner_roots`]), authenticates through its own Matrix SDK OAuth
//! session, starts the native allocation and shared-execution workers, and
//! serves a versioned, bounded control API over a private Unix socket.
//! Desktop attaches and detaches; closing it never stops hosting. Any binary
//! can run the daemon through [`cli::run_if_requested`].
//!
//! Layout of a daemon home (directory 0700, files 0600):
//!
//! | Entry | Purpose |
//! | --- | --- |
//! | `daemon.lock` | exclusive lock: one daemon per home |
//! | `control.sock` | private control socket |
//! | `control.sqlite3` | durable hosting intent, revision and command receipts |
//! | `enrollment.json` | enrolled owner (origin, Matrix ID) and its registered owner-root path |
//! | `matrix/` | the daemon's own SDK session and crypto store |
//!
//! The owner root keeps its own exclusive `.owner-client.lock`, which the
//! native engine takes when the daemon opens the owner. A second daemon or an
//! in-process executor therefore cannot host the same root at the same time.
#[cfg(unix)]
pub mod auth;
pub mod control;
mod sqlite;
pub mod bridge;
pub mod cli;
pub mod dispatch;
pub mod enrollment;
#[cfg(unix)]
pub mod enrollment_rpc;
pub mod assignment;
pub mod revocation;
#[cfg(unix)]
pub mod host;
#[cfg(unix)]
pub mod ipc;
pub mod protocol;
#[cfg(unix)]
pub mod runtime;
#[cfg(unix)]
mod snapshot_query;
pub mod workers;
#[cfg(unix)]
pub use ipc::{DaemonClient, IpcError};
pub use protocol::{
    API_VERSION, Command, CommandOutcome, ErrorBody, Hello, OutcomeState, Period, Reply, Request,
};

use std::path::{Path, PathBuf};

pub const LOCK: &str = "daemon.lock";
pub const CONTROL: &str = "control.sqlite3";
pub const ENROLLMENT: &str = "enrollment.json";
pub const MATRIX: &str = "matrix";

/// Server origin equivalence, the same rule as the owner-root registry: a
/// terminal slash is not scope and scheme and host ignore case.
pub fn same_origin(a: &str, b: &str) -> bool {
    crate::owner_roots::normalize_origin(a) == crate::owner_roots::normalize_origin(b)
}

/// Read a private (owner-only, single-link, non-symlink) file of at most
/// `limit` bytes. `Ok(None)` when it does not exist.
pub(crate) fn read_private(path: &Path, limit: usize) -> Result<Option<Vec<u8>>, ()> {
    use std::io::Read;
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(hagency_store::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(_) => return Err(()),
    };
    if file.metadata().map_err(|_| ())?.len() > limit as u64 {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > limit {
        return Err(());
    }
    Ok(Some(bytes))
}

#[derive(Debug, thiserror::Error)]
pub enum HomeError {
    #[error("the daemon home must be an absolute private directory")]
    State,
    #[error("another owner daemon is using this home")]
    AlreadyRunning,
}

/// Exclusive custody of one daemon home for the life of the process.
pub struct DaemonHome {
    path: PathBuf,
    _lock: std::fs::File,
}

impl DaemonHome {
    pub fn open(path: &Path) -> Result<Self, HomeError> {
        if !path.is_absolute() {
            return Err(HomeError::State);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| HomeError::State)?;
        }
        hagency_store::private::directory(path).map_err(|_| HomeError::State)?;
        let path = path.canonicalize().map_err(|_| HomeError::State)?;
        let lock = hagency_store::private::open(&path.join(LOCK), true)
            .or_else(|_| hagency_store::private::open(&path.join(LOCK), false))
            .map_err(|_| HomeError::State)?;
        lock.try_lock().map_err(|_| HomeError::AlreadyRunning)?;
        hagency_store::private::directory(&path.join(MATRIX)).map_err(|_| HomeError::State)?;
        Ok(Self { path, _lock: lock })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn control(&self) -> PathBuf {
        self.path.join(CONTROL)
    }
    pub fn enrollment(&self) -> PathBuf {
        self.path.join(ENROLLMENT)
    }
    pub fn matrix(&self) -> PathBuf {
        self.path.join(MATRIX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_private_and_exclusive() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("daemon");
        let first = DaemonHome::open(&home).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(first.path()).unwrap().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(first.matrix()).unwrap().mode() & 0o777, 0o700);
        }
        assert!(matches!(DaemonHome::open(&home), Err(HomeError::AlreadyRunning)));
        drop(first);
        assert!(DaemonHome::open(&home).is_ok());
        assert!(matches!(
            DaemonHome::open(Path::new("relative")),
            Err(HomeError::State)
        ));
    }
}

/// A fresh request ID. Reuse it verbatim when retrying an uncertain command.
pub fn new_request_id() -> String {
    let mut bytes = [0u8; 16];
    if getrandom::fill(&mut bytes).is_err() {
        // Uniqueness also comes from time; the daemon still refuses reuse.
        bytes[..16].copy_from_slice(
            &std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
                .to_be_bytes(),
        );
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
