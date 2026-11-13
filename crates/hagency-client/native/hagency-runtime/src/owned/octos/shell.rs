//! An independently supervised shell for a qualified shared Octos process.
//! macOS cannot apply another Seatbelt profile inside the Octos sandbox, so
//! the host starts the shell under its own stricter sandbox and tracks it.
use super::super::SharedTextProcessSource;
use crate::octos::profile_binding::RunningProfileGuard;
use std::{path::PathBuf, sync::{Arc, atomic::AtomicUsize}, time::Duration};
#[cfg(target_os = "macos")]
use std::{collections::BTreeMap, path::Path, sync::atomic::Ordering, time::Instant};

#[cfg(target_os = "macos")]
mod interactive;
#[cfg(target_os = "macos")]
const SHELL_PATH: &str = "/Library/Developer/CommandLineTools/usr/bin:/usr/bin:/bin:/usr/sbin:/sbin";
#[cfg(target_os = "macos")]
const SYSTEM_SDK: &str = "/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk";
#[cfg(target_os = "macos")]
const SYSTEM_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

#[cfg(target_os = "macos")]
fn system_path_is_trusted(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).is_ok_and(|m|
        m.is_dir() && m.uid() == 0 && m.mode() & 0o022 == 0
            && rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_err())
}

#[cfg(target_os = "macos")]
fn system_toolchain_available() -> bool {
    ["/Library", "/Library/Developer", "/Library/Developer/CommandLineTools"]
        .iter().all(|path| system_path_is_trusted(Path::new(path)))
}

#[cfg(all(test, target_os = "macos"))]
mod toolchain_tests {
    use super::system_path_is_trusted;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn writable_or_symlinked_toolchain_directories_are_not_trusted() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("toolchain");
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!system_path_is_trusted(&directory));
        let alias = temp.path().join("alias");
        symlink("/Library/Developer/CommandLineTools", &alias).unwrap();
        assert!(!system_path_is_trusted(&alias));
    }
}

#[derive(Clone)]
pub struct SharedOctosShell {
    #[cfg(target_os = "macos")]
    guardian: PathBuf,
    #[cfg(target_os = "macos")]
    workspace: PathBuf,
    #[cfg(target_os = "macos")]
    source: SharedTextProcessSource,
    #[cfg(target_os = "macos")]
    profile: Arc<RunningProfileGuard>,
    #[cfg(target_os = "macos")]
    pending: Arc<AtomicUsize>,
    #[cfg(target_os = "macos")]
    executable: PathBuf,
    #[cfg(target_os = "macos")]
    patch_supported: bool,
    #[cfg(target_os = "macos")]
    sessions: Arc<tokio::sync::Mutex<std::collections::HashMap<String, Arc<interactive::Session>>>>,
}

#[derive(serde::Serialize)]
pub struct ShellResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

/// A bounded incremental transcript of a supervised interactive command.
#[derive(serde::Serialize)]
pub struct InteractiveShellResult {
    pub session_id: Option<String>,
    pub output: String,
    pub exit_code: Option<i32>,
    pub truncated: bool,
}

impl SharedOctosShell {
    pub(super) fn new(guardian: PathBuf, workspace: PathBuf, source: SharedTextProcessSource,
        profile: Arc<RunningProfileGuard>, pending: Arc<AtomicUsize>, executable: PathBuf,
        patch_supported: bool) -> Self {
        #[cfg(target_os = "macos")]
        {
        Self { guardian, workspace, source, profile, pending, executable, patch_supported,
            sessions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (guardian, workspace, source, profile, pending, executable, patch_supported);
            Self {}
        }
    }
    #[cfg(target_os = "macos")]
    fn active(&self) -> bool {
        self.source.has_live_activity() && self.profile.check().is_ok()
            && self.workspace.canonicalize().ok().as_deref() == Some(self.workspace.as_path())
    }
    /// Never admits a call: the host must first verify the work and the
    /// owner's exact permission. This capability only confines its execution.
    #[cfg(target_os = "macos")]
    pub async fn run(&self, command: &str, timeout: Duration) -> Result<ShellResult, &'static str> {
        use tokio::io::AsyncReadExt;
        if command.is_empty() || command.len() > 16_000 || command.as_bytes().contains(&0)
            || timeout.is_zero() || timeout > Duration::from_secs(120) || !self.active() {
            return Err("shell_request_refused");
        }
        let policy = sandbox(&self.workspace, None)?;
        let mut environment = BTreeMap::new();
        environment.insert("HOME".into(), self.workspace.as_os_str().into());
        environment.insert("TMPDIR".into(), self.workspace.as_os_str().into());
        let system_tools = system_toolchain_available();
        environment.insert("PATH".into(), if system_tools { SHELL_PATH } else { SYSTEM_PATH }.into());
        if system_tools && Path::new(SYSTEM_SDK).is_dir() { environment.insert("SDKROOT".into(), SYSTEM_SDK.into()); }
        let launch = hagency_platform::Launch {
            executable: "/usr/bin/sandbox-exec".into(),
            arguments: vec!["-p".into(), policy.into(), "/bin/sh".into(), "-c".into(), command.into()],
            directory: self.workspace.clone(), environment, require_crash_containment: false,
        };
        self.claim_slot()?;
        // An uncertain spawn is never proof that no helper briefly ran.
        let (owner, pipes) = match hagency_platform::SupervisedProcess::spawn_piped(&self.guardian, &launch) {
            Ok(value) => value,
            Err(_) => {
                self.pending.fetch_or(POISONED, Ordering::AcqRel);
                return Err("shell_start_unknown");
            }
        };
        let mut owner = ShellGuard { owner, pending: self.pending.clone(), settled: false };
        let (stdin, stdout, stderr) = pipes.into_parts();
        drop(stdin);
        let stdout = tokio::net::unix::pipe::Receiver::from_owned_fd(stdout).map_err(|_| "shell_io_unavailable")?;
        let stderr = tokio::net::unix::pipe::Receiver::from_owned_fd(stderr).map_err(|_| "shell_io_unavailable")?;
        async fn drain(mut pipe: tokio::net::unix::pipe::Receiver) -> (Vec<u8>, bool) {
            let mut result = Vec::new();
            let mut truncated = false;
            let mut buf = [0u8; 4096];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let keep = n.min(32 * 1024usize - result.len());
                        result.extend_from_slice(&buf[..keep]);
                        truncated |= keep != n;
                    }
                }
            }
            (result, truncated)
        }
        let out = tokio::spawn(drain(stdout));
        let err = tokio::spawn(drain(stderr));
        let deadline = Instant::now() + timeout;
        let report = loop {
            if !self.active() || Instant::now() >= deadline {
                owner.stop()?;
                out.abort(); err.abort();
                return Err("shell_interrupted");
            }
            match owner.owner.wait(Duration::from_millis(100)) {
                Ok(Some(report)) => break report,
                Ok(None) => tokio::task::yield_now().await,
                Err(_) => { out.abort(); err.abort(); return Err("shell_stop_unknown"); }
            }
        };
        if !report.scope.whole_tree_stopped || !report.scope.leader_exited {
            out.abort(); err.abort();
            return Err("shell_stop_unknown");
        }
        owner.proven();
        let (stdout, out_truncated) = tokio::time::timeout(Duration::from_secs(2), out).await
            .map_err(|_| "shell_output_unavailable")?.map_err(|_| "shell_output_unavailable")?;
        let (stderr, err_truncated) = tokio::time::timeout(Duration::from_secs(2), err).await
            .map_err(|_| "shell_output_unavailable")?.map_err(|_| "shell_output_unavailable")?;
        let exit_code = report.leader_status.and_then(|raw| ((raw & 0x7f) == 0).then_some((raw >> 8) & 0xff));
        Ok(ShellResult { stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(), exit_code,
            truncated: out_truncated || err_truncated })
    }
    #[cfg(not(target_os = "macos"))]
    pub async fn run(&self, _command: &str, _timeout: Duration) -> Result<ShellResult, &'static str> {
        Err("shell_platform_unsupported")
    }

    #[cfg(target_os = "macos")]
    fn claim_slot(&self) -> Result<(), &'static str> {
        self.pending.fetch_update(Ordering::AcqRel, Ordering::Acquire,
            |count| (count < 4).then_some(count + 1))
            .map(|_| ()).map_err(|_| "shell_scope_busy_or_unknown")
    }

    /// The owner authorizes the command before the host invokes this method.
    /// Each session is tied to the same live activity and workspace as Octos.
    #[cfg(target_os = "macos")]
    pub async fn exec(&self, command: &str, workdir: Option<&str>, tty: bool,
        yield_time: Duration, timeout: Duration) -> Result<InteractiveShellResult, &'static str> {
        interactive::start(self, command, workdir, tty, yield_time, timeout, None).await.map(|(result, _)| result)
    }
    #[cfg(not(target_os = "macos"))]
    pub async fn exec(&self, _command: &str, _workdir: Option<&str>, _tty: bool,
        _yield_time: Duration, _timeout: Duration) -> Result<InteractiveShellResult, &'static str> {
        Err("shell_platform_unsupported")
    }
    #[cfg(target_os = "macos")]
    pub async fn write_stdin(&self, id: &str, input: &str, yield_time: Duration)
        -> Result<InteractiveShellResult, &'static str> {
        interactive::write(self, id, input, yield_time).await
    }
    #[cfg(not(target_os = "macos"))]
    pub async fn write_stdin(&self, _id: &str, _input: &str, _yield_time: Duration)
        -> Result<InteractiveShellResult, &'static str> { Err("shell_platform_unsupported") }
    #[cfg(target_os = "macos")]
    pub async fn shutdown(&self) -> Result<(), &'static str> { interactive::shutdown(self).await }
    #[cfg(not(target_os = "macos"))]
    pub async fn shutdown(&self) -> Result<(), &'static str> { Err("shell_platform_unsupported") }

    /// Apply a patch in a separately supervised workspace-only sandbox. The
    /// worker loads no Octos profile and has no network or credential access.
    #[cfg(target_os = "macos")]
    pub async fn apply_patch(&self, args: &serde_json::Value) -> Result<serde_json::Value, &'static str> {
        if !self.patch_supported || !self.active()
            || self.executable.canonicalize().ok().as_deref() != Some(self.executable.as_path()) {
            return Err("patch_worker_unavailable");
        }
        let mut input = serde_json::to_string(args).map_err(|_| "invalid_patch_request")?;
        input.push('\n');
        if input.len() > 64 * 1024 { return Err("patch_request_too_large"); }
        let (initial, _cancel) = interactive::start(self, "__host-patch", None, false, Duration::ZERO,
            Duration::from_secs(60), Some(&self.executable)).await?;
        let id = initial.session_id.ok_or("patch_worker_unavailable")?;
        let mut output = initial.output;
        if initial.truncated { return Err("patch_output_too_large"); }
        let mut result = interactive::write(self, &id, &input, Duration::from_secs(10)).await?;
        loop {
            if result.truncated || output.len() + result.output.len() > 64 * 1024 { return Err("patch_output_too_large"); }
            output.push_str(&result.output);
            if result.session_id.is_none() { break; }
            result = interactive::write(self, &id, "", Duration::from_secs(10)).await?;
        }
        if result.exit_code != Some(0) { return Err("patch_worker_failed"); }
        serde_json::from_str(&output).map_err(|_| "patch_output_invalid")
    }
    #[cfg(not(target_os = "macos"))]
    pub async fn apply_patch(&self, _args: &serde_json::Value) -> Result<serde_json::Value, &'static str> {
        Err("shell_platform_unsupported")
    }
}

#[cfg(target_os = "macos")]
const POISONED: usize = 1usize << (usize::BITS - 1);
#[cfg(target_os = "macos")]
struct ShellGuard { owner: hagency_platform::SupervisedProcess, pending: Arc<AtomicUsize>, settled: bool }
#[cfg(target_os = "macos")]
impl ShellGuard {
    fn proven(&mut self) { if !self.settled { self.pending.fetch_sub(1, Ordering::AcqRel); self.settled = true; } }
    fn stop(&mut self) -> Result<(), &'static str> {
        let report = self.owner.stop(Duration::from_secs(3)).map_err(|_| {
            self.pending.fetch_or(POISONED, Ordering::AcqRel);
            "shell_stop_unknown"
        })?;
        if !report.scope.whole_tree_stopped || !report.scope.leader_exited {
            self.pending.fetch_or(POISONED, Ordering::AcqRel);
            return Err("shell_stop_unknown");
        }
        self.proven();
        Ok(())
    }
}
#[cfg(target_os = "macos")]
impl Drop for ShellGuard { fn drop(&mut self) { if !self.settled { let _ = self.stop(); } } }

#[cfg(target_os = "macos")]
fn sandbox(workspace: &Path, worker: Option<&Path>) -> Result<String, &'static str> {
    if !workspace.is_absolute() || workspace.canonicalize().ok().as_deref() != Some(workspace) {
        return Err("shell_workspace_changed");
    }
    let scope = serde_json::to_string(workspace.to_str().ok_or("shell_workspace_changed")?)
        .map_err(|_| "shell_workspace_changed")?;
    // The system developer tools are root-owned and read-only inside this
    // sandbox. A profile-free patch worker does not need this access.
    let toolchain = if worker.is_none() && system_toolchain_available() {
        "(allow file-read* (subpath \"/Library/Developer/CommandLineTools\"))"
    } else { "" };
    let worker = match worker {
        Some(path) if path.is_absolute() && path.canonicalize().ok().as_deref() == Some(path) =>
            format!("(allow file-read* (literal {}))(deny file-read* (subpath \"/private/etc\"))(deny file-read* (subpath \"/dev\"))", serde_json::to_string(path.to_str().ok_or("patch_worker_unavailable")?)
                .map_err(|_| "patch_worker_unavailable")?),
        Some(_) => return Err("patch_worker_unavailable"),
        None => String::new(),
    };
    Ok(format!(r#"(version 1)
(deny default)
(allow process-exec)(allow process-fork)(allow process-info* (target self))
(allow sysctl-read)(allow mach-lookup)(deny mach-lookup (global-name-prefix "com.apple.security"))
(deny mach-priv-task-port)(allow signal (target self))(allow file-ioctl)
(allow file-read-metadata)
(allow file-read* (literal "/") (subpath "/System") (subpath "/usr") (subpath "/bin")
 (subpath "/sbin") (subpath "/dev") (subpath "/private/etc") (subpath "/private/var/db/timezone") (subpath {scope}))
{toolchain}
{worker}
(deny file-read* (subpath "/usr/local"))
(deny file-read* (regex #"^/dev/ttys[0-9]+$"))
(allow file-write* (subpath {scope}) (literal "/dev/null") (subpath "/dev/fd"))
(deny network*)
"#))
}
