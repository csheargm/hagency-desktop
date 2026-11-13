//! Host-only process scope primitives. Not runner authorization or a sandbox.
use std::{collections::BTreeMap, ffi::OsString, io, path::PathBuf, time::Duration};

pub mod legacy_writer;
#[cfg(target_os = "macos")]
#[allow(unsafe_code)] // Narrow PDFKit ownership/selector boundary, used by the disposable parser.
pub mod pdf_text;
#[allow(unsafe_code)] // Narrow getpwuid_r boundary; never inherits HOME for ownership.
mod owner_home;
pub mod private_state;
pub use owner_home::{owner_home, codex_process_home};

mod child_identity;
pub use child_identity::{ChildIdentity, OwnedChildIdentity, SignalOutcome};
#[allow(unsafe_code)]
mod directory_identity;
pub use directory_identity::{DirectoryIdentity, directory_identity, same_directory};
#[allow(unsafe_code)]
mod file_identity;
pub use file_identity::{same_file, file_identity, FileIdentity};
mod supervisor;
#[cfg(unix)]
pub use supervisor::run_guardian;

/// The executable that implements this program's private helper roles (the
/// `guardian` of every provider process, the document reader): this program
/// itself.
///
/// A developer's run under the Makepad JIT runner is the one exception. There
/// the running executable is the runner, and a helper started through it is
/// another JIT instance: it cannot start in the guardian's sealed environment
/// (no HOME, an empty PATH, a 15 s limit; measured 2026-11-13: 39 s and a
/// failed start). A build with `development-native-helper` therefore takes
/// the helper from HAGENCY_NATIVE_HELPER, a native build of this same program.
/// Without that feature the variable is never read, so no distributed build
/// can be pointed at another helper.
pub fn helper_executable() -> std::io::Result<std::path::PathBuf> {
    #[cfg(feature = "development-native-helper")]
    if let Some(named) = std::env::var_os("HAGENCY_NATIVE_HELPER") {
        return named_helper(named.into());
    }
    std::env::current_exe()
}

/// A named helper is used as given or refused: never searched for on PATH,
/// never resolved against the working folder.
#[cfg(any(feature = "development-native-helper", test))]
fn named_helper(path: std::path::PathBuf) -> std::io::Result<std::path::PathBuf> {
    if !path.is_absolute() || !path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "HAGENCY_NATIVE_HELPER is not an absolute path to a file",
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod helper_tests {
    use super::named_helper;

    #[test]
    fn a_named_helper_is_an_absolute_path_to_an_existing_file() {
        let own = std::env::current_exe().unwrap();
        assert_eq!(named_helper(own.clone()).unwrap(), own);
        // A bare name would be looked up on PATH by the spawn; a folder or a
        // missing file would fail every Agent run with an unrelated error.
        for refused in ["rinx", "./rinx", "/", "/nonexistent/hagency/rinx"] {
            let error = named_helper(refused.into()).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput, "{refused}");
        }
    }
}
pub use supervisor::{
    LiveRow, StopCause, StopDetail, StopRefusal, SupervisedProcess, SupervisedReport,
};
mod stdio;
pub use stdio::StdioPipes;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod cgroup;
#[cfg(any(target_os = "linux", test))]
mod cgroup_checks;
#[cfg(target_os = "linux")]
pub use cgroup::CgroupRecovery;
#[cfg(windows)]
pub use windows::stdio::Pipe as WindowsPipe;

#[cfg(unix)]
mod unix;
#[cfg(unix)]
#[allow(unsafe_code)]
mod unix_spawn;
#[cfg(unix)]
use unix::Process;
#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;
#[cfg(windows)]
use windows::Process;

/// Explicit host configuration; no serde constructor and no environment inheritance.
pub struct Launch {
    pub executable: PathBuf,
    pub arguments: Vec<OsString>,
    pub directory: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub require_crash_containment: bool,
}
impl Launch {
    /// Checks bounded host configuration, not physical directory or sandbox custody.
    pub fn validate(&self) -> io::Result<()> {
        if !self.executable.is_absolute()
            || !self.directory.is_absolute()
            || !self.directory.is_dir()
            || self.arguments.len() > 256
            || self.environment.len() > 256
        {
            return Err(invalid());
        }
        let values = [self.executable.as_os_str(), self.directory.as_os_str()]
            .into_iter()
            .chain(self.arguments.iter().map(|v| v.as_os_str()))
            .chain(
                self.environment
                    .iter()
                    .flat_map(|(k, v)| [k.as_os_str(), v.as_os_str()]),
            );
        let mut length = 0usize;
        for value in values {
            let bytes = value.as_encoded_bytes();
            length = length.checked_add(bytes.len() + 1).ok_or_else(invalid)?;
            if bytes.contains(&0) || length > 64 * 1024 {
                return Err(invalid());
            }
        }
        for key in self.environment.keys() {
            let bytes = key.as_encoded_bytes();
            if bytes.is_empty()
                || !bytes
                    .iter()
                    .all(|v| v.is_ascii_alphanumeric() || *v == b'_')
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "invalid native launch configuration",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopReport {
    pub leader_exited: bool,
    /// Every requested cancellation signal was accepted (or had no live target).
    /// False retains a signal failure even if the leader was subsequently reaped.
    pub signals_accepted: bool,
    /// POSIX group-only cancellation always leaves this false. Detached children
    /// and owner-crash cleanup require the later guardian/identity adapter.
    pub whole_tree_stopped: bool,
}
pub struct OwnedProcess {
    inner: Process,
}
impl OwnedProcess {
    pub fn spawn(launch: &Launch) -> io::Result<Self> {
        launch.validate()?;
        Ok(Self {
            inner: Process::spawn(launch)?,
        })
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    pub(crate) fn spawn_piped(launch: &Launch, pipes: stdio::ChildPipes) -> io::Result<Self> {
        launch.validate()?;
        Ok(Self {
            inner: Process::spawn_piped(launch, pipes)?,
        })
    }
    #[cfg(windows)]
    pub(crate) fn spawn_piped(
        launch: &Launch,
        pipes: windows::stdio::ChildPipes,
    ) -> io::Result<Self> {
        launch.validate()?;
        Ok(Self {
            inner: Process::spawn_piped(launch, pipes)?,
        })
    }
    /// Informational only: ownership is the private child/job handle, never this PID.
    pub fn id(&self) -> u32 {
        self.inner.id()
    }
    /// Observe without reaping: the retained leader still anchors final signals.
    pub fn is_leader_running(&self) -> io::Result<bool> {
        self.inner.is_leader_running()
    }
    pub fn stop(&mut self, timeout: Duration) -> io::Result<StopReport> {
        if timeout.is_zero() || timeout > Duration::from_secs(5) {
            return Err(invalid());
        }
        self.inner.stop(timeout)
    }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        let _ = self.inner.stop(Duration::from_secs(2));
    }
}
