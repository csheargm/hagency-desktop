//! How the host runs one network git command of a trusted step: as the
//! Agent's own GitHub identity (its token, never the daemon's login), with a
//! time limit, and stopped at once when the turn is (Stop All drops the turn,
//! and with it the [`CancelOnDrop`] guard).
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The longest one host git command may run before it is killed.
pub const HOST_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// What a host step runs with.
pub struct HostCtx {
    /// The Agent's GitHub token, the only credential the host's git sees.
    pub token: Option<String>,
    /// Set when the turn is stopped: the running command is killed.
    pub cancel: Arc<AtomicBool>,
    /// Re-checked right before the push: the owner may have turned push
    /// trust off (or paused the watch) since the call was authorized.
    pub still_allowed: Box<dyn Fn() -> bool + Send + Sync>,
    pub timeout: Duration,
}

impl HostCtx {
    pub fn new(token: Option<String>, cancel: Arc<AtomicBool>, still_allowed: Box<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self { token, cancel, still_allowed, timeout: HOST_COMMAND_TIMEOUT }
    }
    pub(crate) fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Sets its flag when dropped: held by the turn's future, so dropping the
/// turn (Stop All) cancels the host step running off the async threads.
pub struct CancelOnDrop(pub Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Stdout of a successful command; stderr as the error. The child is killed
/// when `ctx` is cancelled or the command outlives `ctx.timeout`.
pub(crate) fn run(command: &mut Command, stdin: Option<&str>, ctx: &HostCtx) -> Result<String, String> {
    if ctx.cancelled() {
        return Err("the turn was stopped".into());
    }
    command.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Its own process group, so a stop kills git's helpers too
    // (git-remote-https, the credential helper), not just git.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(command, 0);
    let mut child = command.spawn().map_err(|e| format!("git: {e}"))?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A failed write shows up as the command's own failure below.
        let _ = pipe.write_all(input.as_bytes());
    }
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        let why = if ctx.cancelled() { Some("the turn was stopped") } else if started.elapsed() > ctx.timeout { Some("git took too long") } else { None };
        if let Some(why) = why {
            kill_group(child.id());
            // Already exited is fine: either way the command is over.
            let _ = child.kill();
            let _ = child.wait();
            return Err(why.into());
        }
        std::thread::sleep(POLL);
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    if status.success() { Ok(out.trim().to_owned()) } else { Err(err.trim().to_owned()) }
}

/// SIGKILL to every process of the group `pid` leads, sent a few times (a
/// process forked while the first signal was delivered can escape it).
/// Called BEFORE the leader is reaped: its zombie keeps the group id
/// reserved, so the signal can never reach a group that reused the id.
fn kill_group(pid: u32) {
    #[cfg(unix)]
    for _ in 0..GROUP_KILL_ROUNDS {
        // Best effort: a failed send means the group is already gone.
        let _ = Command::new("/bin/kill").args(["-KILL", "--", &format!("-{pid}")]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        std::thread::sleep(POLL);
    }
    #[cfg(not(unix))]
    let _ = pid;
}
const GROUP_KILL_ROUNDS: u32 = 4;

fn drain(pipe: Option<impl Read + Send + 'static>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(mut pipe) = pipe {
            // A broken pipe leaves what was read; the exit status decides.
            let _ = pipe.read_to_end(&mut text);
        }
        String::from_utf8_lossy(&text).into_owned()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ctx() -> HostCtx {
        HostCtx::new(None, Arc::default(), Box::new(|| true))
    }
    #[test]
    fn a_stopped_turn_kills_the_running_command() {
        let c = ctx();
        let flag = c.cancel.clone();
        let guard = CancelOnDrop(flag);
        let killer = std::thread::spawn(move || { std::thread::sleep(Duration::from_millis(200)); drop(guard); });
        let started = Instant::now();
        assert_eq!(run(Command::new("/bin/sleep").arg("30"), None, &c), Err("the turn was stopped".into()));
        assert!(started.elapsed() < Duration::from_secs(5));
        killer.join().unwrap();
        assert_eq!(run(&mut Command::new("true"), None, &c), Err("the turn was stopped".into()), "nothing starts after a stop");
    }
    #[test]
    fn a_command_past_its_time_limit_is_killed() {
        let mut c = ctx();
        c.timeout = Duration::from_millis(200);
        assert_eq!(run(Command::new("/bin/sleep").arg("30"), None, &c), Err("git took too long".into()));
        assert_eq!(run(Command::new("echo").arg("hi"), None, &ctx()), Ok("hi".into()));
        // A helper the command started dies with it (absolute paths: other
        // tests may narrow PATH).
        let marker = tempfile::tempdir().unwrap();
        let file = marker.path().join("late");
        let script = format!("(/bin/sleep 3; echo late > {}) & wait", file.display());
        assert!(run(Command::new("/bin/sh").args(["-c", &script]), None, &c).is_err());
        std::thread::sleep(Duration::from_millis(3500));
        assert!(!file.exists(), "the child's helper was killed too");
    }
}
