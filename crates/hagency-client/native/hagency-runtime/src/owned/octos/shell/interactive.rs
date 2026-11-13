//! Interactive commands remain in a separately supervised, confined process.
//! The trusted system `script` program allocates a PTY before entering the
//! shell sandbox; only its already-open terminal descriptors reach the child.
use super::{InteractiveShellResult, POISONED, SHELL_PATH, SYSTEM_PATH, SYSTEM_SDK, SharedOctosShell, ShellGuard, sandbox, system_toolchain_available};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{Arc, Mutex, atomic::{AtomicBool, AtomicU64, Ordering}},
    time::{Duration, Instant},
};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::unix::pipe::{Receiver, Sender}, sync::Notify};

const MAX_OUTPUT: usize = 64 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct State {
    bytes: Vec<u8>,
    truncated: bool,
    result: Option<Result<Option<i32>, &'static str>>,
    readers: usize,
}
struct Shared {
    state: Mutex<State>,
    notify: Notify,
}
pub(super) struct Session {
    input: tokio::sync::Mutex<Sender>,
    cancel: Arc<AtomicBool>,
    shared: Arc<Shared>,
}
pub(super) struct CancelOnDrop(Arc<Session>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) { self.0.cancel.store(true, Ordering::Release); }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn dropping_patch_guard_cancels_its_worker() {
        let (input, _output) = tokio::net::unix::pipe::pipe().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let session = Arc::new(Session {
            input: tokio::sync::Mutex::new(input), cancel: cancel.clone(),
            shared: Arc::new(Shared { state: Mutex::new(State::default()), notify: Notify::new() }),
        });
        drop(CancelOnDrop(session.clone()));
        assert!(cancel.load(Ordering::Acquire));
    }
}
impl Drop for Session {
    fn drop(&mut self) { self.cancel.store(true, Ordering::Release); }
}

fn resolve_workdir(shell: &SharedOctosShell, requested: Option<&str>) -> Result<std::path::PathBuf, &'static str> {
    let path = match requested {
        None | Some("") => shell.workspace.clone(),
        Some(value) if value.len() <= 1024 && !value.contains('\0') => {
            let path = Path::new(value);
            if path.is_absolute() { path.to_path_buf() } else { shell.workspace.join(path) }
        }
        _ => return Err("invalid_workdir"),
    };
    let path = path.canonicalize().map_err(|_| "invalid_workdir")?;
    if !path.starts_with(&shell.workspace) || !path.is_dir() { return Err("invalid_workdir"); }
    Ok(path)
}

async fn read_output(mut pipe: Receiver, shared: Arc<Shared>) {
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                let keep = n.min(MAX_OUTPUT.saturating_sub(state.bytes.len()));
                state.bytes.extend_from_slice(&buf[..keep]);
                state.truncated |= keep != n;
                drop(state);
                shared.notify.notify_one();
            }
        }
    }
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    state.readers = state.readers.saturating_sub(1);
    drop(state);
    shared.notify.notify_one();
}

async fn snapshot(id: &str, session: &Session, wait: Duration) -> Result<InteractiveShellResult, &'static str> {
    if wait > Duration::from_secs(30) { return Err("invalid_yield_time"); }
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let notified = session.shared.notify.notified();
        {
            let mut state = session.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.bytes.is_empty() || state.result.is_some() && state.readers == 0
                || tokio::time::Instant::now() >= deadline {
                let bytes = std::mem::take(&mut state.bytes);
                let truncated = std::mem::take(&mut state.truncated);
                let done = state.result.is_some() && state.readers == 0;
                let result = state.result;
                if done && let Some(Err(error)) = result { return Err(error); }
                return Ok(InteractiveShellResult {
                    session_id: (!done).then(|| id.to_owned()),
                    output: String::from_utf8_lossy(&bytes).into_owned(),
                    exit_code: if done { result.and_then(Result::ok).flatten() } else { None }, truncated,
                });
            }
        }
        let _ = tokio::time::timeout_at(deadline, notified).await;
    }
}

pub(super) async fn start(shell: &SharedOctosShell, command: &str, workdir: Option<&str>,
    tty: bool, yield_time: Duration, timeout: Duration, worker: Option<&Path>) -> Result<(InteractiveShellResult, Option<CancelOnDrop>), &'static str> {
    if command.is_empty() || command.len() > 16_000 || command.contains('\0')
        || timeout.is_zero() || timeout > Duration::from_secs(1800)
        || yield_time > Duration::from_secs(30) || !shell.active() {
        return Err("shell_request_refused");
    }
    let cwd = resolve_workdir(shell, workdir)?;
    let policy = sandbox(&shell.workspace, worker)?;
    let mut env = BTreeMap::new();
    env.insert("HOME".into(), shell.workspace.as_os_str().into());
    env.insert("TMPDIR".into(), shell.workspace.as_os_str().into());
    let system_tools = worker.is_none() && system_toolchain_available();
    env.insert("PATH".into(), if system_tools { SHELL_PATH } else { SYSTEM_PATH }.into());
    if system_tools && Path::new(SYSTEM_SDK).is_dir() { env.insert("SDKROOT".into(), SYSTEM_SDK.into()); }
    if tty { env.insert("TERM".into(), "xterm-256color".into()); }
    let args = if let Some(worker) = worker {
        if tty { return Err("invalid_patch_request"); }
        vec!["-p".into(), policy.into(), worker.as_os_str().into(), "__host-patch".into()]
    } else if tty {
        vec!["-q".into(), "/dev/null".into(), "/usr/bin/sandbox-exec".into(), "-p".into(),
            policy.into(), "/bin/sh".into(), "-c".into(), command.into()]
    } else {
        vec!["-p".into(), policy.into(), "/bin/sh".into(), "-c".into(), command.into()]
    };
    let launch = hagency_platform::Launch {
        executable: if tty { "/usr/bin/script".into() } else { "/usr/bin/sandbox-exec".into() },
        arguments: args, directory: cwd, environment: env, require_crash_containment: false,
    };
    let mut sessions = shell.sessions.lock().await;
    if sessions.len() >= 4 { return Err("shell_scope_busy_or_unknown"); }
    shell.claim_slot()?;
    let (owner, pipes) = match hagency_platform::SupervisedProcess::spawn_piped(&shell.guardian, &launch) {
        Ok(value) => value,
        Err(_) => { shell.pending.fetch_or(POISONED, Ordering::AcqRel); return Err("shell_start_unknown"); }
    };
    let owner = ShellGuard { owner, pending: shell.pending.clone(), settled: false };
    let (input, stdout, stderr) = pipes.into_parts();
    let input = Sender::from_owned_fd(input).map_err(|_| "shell_io_unavailable")?;
    let stdout = Receiver::from_owned_fd(stdout).map_err(|_| "shell_io_unavailable")?;
    let stderr = Receiver::from_owned_fd(stderr).map_err(|_| "shell_io_unavailable")?;
    let shared = Arc::new(Shared { state: Mutex::new(State { readers: 2, ..Default::default() }), notify: Notify::new() });
    let cancel = Arc::new(AtomicBool::new(false));
    let session = Arc::new(Session { input: tokio::sync::Mutex::new(input), cancel: cancel.clone(), shared: shared.clone() });
    // A patch must stop even if the host-tool future is dropped between
    // starting the process and waiting for its result.
    let cancel_guard = worker.is_some().then(|| CancelOnDrop(session.clone()));
    let id = format!("shell-{:016x}", NEXT.fetch_add(1, Ordering::Relaxed));
    sessions.insert(id.clone(), session.clone());
    drop(sessions);
    if worker.is_some() {
        // A cancelled host call has no reader left to remove its session.
        // Keep it visible to shutdown until the guardian reports termination,
        // then reclaim its slot even if the turn itself keeps running.
        let sessions = shell.sessions.clone();
        let watched = session.clone();
        let watched_id = id.clone();
        tokio::spawn(async move {
            for _ in 0..650 {
                let stopped = watched.shared.state.lock().unwrap_or_else(|e| e.into_inner()).result.is_some();
                if stopped && watched.cancel.load(Ordering::Acquire) {
                    let mut sessions = sessions.lock().await;
                    if sessions.get(&watched_id).is_some_and(|entry| Arc::ptr_eq(entry, &watched)) {
                        sessions.remove(&watched_id);
                    }
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        });
    }
    tokio::spawn(read_output(stdout, shared.clone()));
    tokio::spawn(read_output(stderr, shared.clone()));
    let source = shell.source.clone();
    let profile = shell.profile.clone();
    let workspace = shell.workspace.clone();
    tokio::task::spawn_blocking(move || {
        let mut owner = owner;
        let until = Instant::now() + timeout;
        let result = loop {
            if cancel.load(Ordering::Acquire) || Instant::now() >= until || !source.has_live_activity()
                || profile.check().is_err() || workspace.canonicalize().ok().as_deref() != Some(workspace.as_path()) {
                break owner.stop().and(Err("shell_interrupted"));
            }
            match owner.owner.wait(Duration::from_millis(100)) {
                Ok(Some(report)) if report.scope.whole_tree_stopped && report.scope.leader_exited => {
                    owner.proven();
                    break Ok(report.leader_status.and_then(|raw| ((raw & 0x7f) == 0).then_some((raw >> 8) & 0xff)));
                }
                Ok(Some(_)) | Err(_) => { let _ = owner.stop(); break Err("shell_stop_unknown"); }
                Ok(None) => {}
            }
        };
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.result = Some(result);
        drop(state);
        shared.notify.notify_one();
    });
    let result = match snapshot(&id, &session, yield_time).await {
        Ok(result) => result,
        Err(error) => { shell.sessions.lock().await.remove(&id); return Err(error); }
    };
    if result.session_id.is_none() { shell.sessions.lock().await.remove(&id); }
    Ok((result, cancel_guard))
}

pub(super) async fn write(shell: &SharedOctosShell, id: &str, input: &str, wait: Duration)
    -> Result<InteractiveShellResult, &'static str> {
    if id.is_empty() || id.len() > 64 || input.len() > 64 * 1024 || wait > Duration::from_secs(30) || !shell.active() {
        return Err("shell_request_refused");
    }
    let session = shell.sessions.lock().await.get(id).cloned().ok_or("shell_session_unavailable")?;
    if !input.is_empty() {
        let mut writer = session.input.lock().await;
        tokio::time::timeout(Duration::from_secs(5), writer.write_all(input.as_bytes())).await
            .map_err(|_| "shell_input_timeout")?.map_err(|_| "shell_input_unavailable")?;
    }
    let result = match snapshot(id, &session, wait).await {
        Ok(result) => result,
        Err(error) => { shell.sessions.lock().await.remove(id); return Err(error); }
    };
    if result.session_id.is_none() { shell.sessions.lock().await.remove(id); }
    Ok(result)
}

pub(super) async fn shutdown(shell: &SharedOctosShell) -> Result<(), &'static str> {
    let sessions = shell.sessions.lock().await.drain().map(|(_, s)| s).collect::<Vec<_>>();
    for session in &sessions { session.cancel.store(true, Ordering::Release); }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    for session in sessions {
        loop {
            let notified = session.shared.notify.notified();
            if session.shared.state.lock().unwrap_or_else(|e| e.into_inner()).result.is_some() { break; }
            tokio::time::timeout_at(deadline, notified).await.map_err(|_| "shell_stop_unknown")?;
        }
    }
    if shell.pending.load(Ordering::Acquire) == 0 { Ok(()) } else { Err("shell_stop_unknown") }
}
