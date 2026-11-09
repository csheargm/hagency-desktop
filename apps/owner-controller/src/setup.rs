//! Explicit first-run enrollment, entirely outside the UI event thread.
use std::{io::{self, Read}, path::PathBuf, process::{ChildStdin, Command, Stdio}, sync::{Arc, Mutex, mpsc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

#[derive(Clone, Debug)]
pub enum Progress { SigningIn, FinishingCancellation, Enabling, Installed, Failed(&'static str), InstallFailed(&'static str) }
#[derive(Default)]
struct Cancellation { stopped: AtomicBool, stdin: Mutex<Option<ChildStdin>> }
pub struct Setup { events: mpsc::Receiver<Progress>, cancel: Arc<Cancellation> }
impl Drop for Setup {
    fn drop(&mut self) {
        self.cancel.stopped.store(true, Ordering::Release);
        // EOF also reaches the helper if the whole app exits or crashes. The
        // helper owns cancellation of browser enrollment, not a UI timer.
        if let Ok(mut pipe) = self.cancel.stdin.lock() { pipe.take(); }
    }
}

/// Mirrors the helper's `server_admission::normalize_origin`: HTTPS anywhere,
/// plain HTTP only for explicit loopback hosts (local development servers).
pub fn valid_server(server: &str) -> bool {
    server.len() <= 2048 && url::Url::parse(server).is_ok_and(|url| {
        let loopback = url.host_str().is_some_and(|host| host.eq_ignore_ascii_case("localhost")
            || host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()));
        (url.scheme() == "https" || url.scheme() == "http" && loopback) && url.host_str().is_some()
        && url.username().is_empty() && url.password().is_none()
        && url.query().is_none() && url.fragment().is_none()
    })
}
pub fn valid_owner(owner: &str) -> bool {
    owner.is_empty() || (owner.len() <= 512 && !owner.chars().any(char::is_whitespace)
        && owner.strip_prefix('@').and_then(|id| id.split_once(':'))
            .is_some_and(|(name, server)| !name.is_empty() && !server.is_empty()
                && valid_server(&format!("https://{server}")) && !server.contains('/')))
}

fn binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let packaged = exe.parent()?.parent()?.join("Helpers/hagency");
    packaged.is_file().then_some(packaged)
}
fn run(mut command: Command, timeout: Duration, cancel: &Cancellation, enrollment: bool, on_timeout: impl Fn()) -> io::Result<()> {
    if cancel.stopped.load(Ordering::Acquire) { return Err(io::ErrorKind::Interrupted.into()); }
    let mut child = command.stdin(if enrollment { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    let stderr = child.stderr.take();
    let (errors, error_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut captured = Vec::new();
        if let Some(mut pipe) = stderr {
            let mut buffer = [0u8; 1024];
            while let Ok(count) = pipe.read(&mut buffer) {
                if count == 0 { break; }
                let retain = count.min(16_384usize.saturating_sub(captured.len()));
                captured.extend_from_slice(&buffer[..retain]);
            }
        }
        // Only recognized codes cross into the UI. Raw helper output can
        // contain account data and is neither displayed nor logged here.
        let _ = errors.send(helper_failure(&String::from_utf8_lossy(&captured)));
    });
    if enrollment { *cancel.stdin.lock().unwrap_or_else(|e| e.into_inner()) = child.stdin.take(); }
    let start = Instant::now();
    let mut cancellation_started = None;
    let result = loop {
        if cancel.stopped.load(Ordering::Acquire) || start.elapsed() >= timeout {
            cancel.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
            if cancellation_started.is_none() && start.elapsed() >= timeout { on_timeout(); }
            cancellation_started.get_or_insert_with(Instant::now);
        }
        match child.try_wait() {
            Ok(Some(status)) => break if cancellation_started.is_some() { Err(if start.elapsed() >= timeout { io::ErrorKind::TimedOut } else { io::ErrorKind::Interrupted }.into()) } else if status.success() { Ok(()) } else { Err(io::Error::other(error_rx.recv_timeout(Duration::from_millis(200)).unwrap_or("setup_failed"))) },
            Err(error) => { if !enrollment { let _ = child.kill(); } let _ = child.wait(); break Err(error); },
            // Enrollment owns accepted native custody. EOF requests cancellation;
            // only that helper can determine when registration/drain is safe.
            Ok(None) if !enrollment && (start.elapsed() >= timeout || cancellation_started.is_some_and(|at| at.elapsed() >= Duration::from_secs(3))) => {
                let _ = child.kill(); let _ = child.wait(); break Err(if start.elapsed() >= timeout { io::ErrorKind::TimedOut } else { io::ErrorKind::Interrupted }.into());
            },
            Ok(None) => std::thread::sleep(Duration::from_millis(100)),
        }
    };
    cancel.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
    result
}
fn helper_failure(stderr: &str) -> &'static str {
    for code in ["daemon_other_owner_enrolled", "matrix_account_mismatch", "daemon_logout_pending",
        "daemon_owner_root_invalid", "daemon_handover_transition_refused", "unsupported_hagency_server",
        "daemon_enrollment_storage_unavailable", "daemon_session_storage_unavailable", "daemon_socket_unsafe",
        "matrix_authorization_unavailable", "matrix_authorization_required", "daemon_sign_in_cancelled",
        "daemon_enrollment_cancelled", "daemon_enrollment_detached", "daemon_already_running",
        "daemon_install_location_unsupported", "daemon_service_stop_pending"] {
        if stderr.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|word| word == code) { return code; }
    }
    if stderr.contains("already running") { return "daemon_already_running"; }
    "setup_failed"
}
fn failure_code(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::TimedOut => "setup_timed_out",
        io::ErrorKind::Interrupted => "daemon_enrollment_cancelled",
        _ => helper_failure(&error.to_string()),
    }
}
pub fn failure_message(code: &str, zh: bool) -> &'static str {
    match (code, zh) {
        ("daemon_other_owner_enrolled" | "matrix_account_mismatch", false) => "This service belongs to another account. Sign in with the account shown here, or sign out of the existing service first.",
        ("daemon_other_owner_enrolled" | "matrix_account_mismatch", true) => "后台服务属于另一个账号。请使用这里显示的账号登录，或先退出原有服务账号。",
        ("daemon_logout_pending", false) => "The previous sign-out is still pending. Reconnect to finish it before setting up hosting.",
        ("daemon_logout_pending", true) => "上次退出尚未完成。请恢复连接，完成退出后再设置托管。",
        ("setup_timed_out", false) => "Setup timed out. Check the current hosting status and server connection before retrying.",
        ("setup_timed_out", true) => "设置超时。请先检查当前托管状态和服务器连接，再重试。",
        ("daemon_sign_in_cancelled" | "daemon_enrollment_cancelled" | "daemon_enrollment_detached", false) => "Sign-in was cancelled or expired. Retry when you are ready to finish in your browser.",
        ("daemon_sign_in_cancelled" | "daemon_enrollment_cancelled" | "daemon_enrollment_detached", true) => "登录已取消或过期。准备好后请重试并在浏览器中完成登录。",
        ("daemon_already_running", false) => "Another setup is running. Finish or close that setup, then retry here.",
        ("daemon_already_running", true) => "另一个设置正在进行。请完成或关闭它后重试。",
        ("unsupported_hagency_server", false) => "This server does not support hosting setup. Check the server address and its Hagency version.",
        ("unsupported_hagency_server", true) => "此服务器不支持托管设置。请检查服务器地址和 Hagency 版本。",
        ("matrix_authorization_unavailable" | "matrix_authorization_required", false) => "Browser authorization is unavailable. Check your connection and allow the browser sign-in to finish, then retry.",
        ("matrix_authorization_unavailable" | "matrix_authorization_required", true) => "浏览器授权暂不可用。请检查连接并完成浏览器登录后重试。",
        ("daemon_owner_root_invalid" | "daemon_handover_transition_refused" | "daemon_enrollment_storage_unavailable" | "daemon_session_storage_unavailable" | "daemon_socket_unsafe", false) => "Local hosting state could not be opened safely. Check the app installation and its data-folder permissions, then retry.",
        ("daemon_owner_root_invalid" | "daemon_handover_transition_refused" | "daemon_enrollment_storage_unavailable" | "daemon_session_storage_unavailable" | "daemon_socket_unsafe", true) => "无法安全打开本地托管状态。请检查应用安装和数据目录权限后重试。",
        ("daemon_install_location_unsupported", false) => "Move Hagency Hosting to Applications, then retry starting the service.",
        ("daemon_install_location_unsupported", true) => "请将 Hagency Hosting 移到“应用程序”目录，然后重试启动服务。",
        ("daemon_service_stop_pending", false) => "The previous hosting service is still stopping. Wait for it to finish, then retry starting the service.",
        ("daemon_service_stop_pending", true) => "之前的托管服务仍在停止。请等待完成后重试启动服务。",
        (_, false) => "Hosting setup failed. Check the server, account and app installation, then retry.",
        (_, true) => "托管设置失败。请检查服务器、账号和应用安装后重试。",
    }
}
impl Setup {
    pub fn start(home: PathBuf, adopt_root: Option<PathBuf>, server: String, owner: Option<String>, install_only: bool, notify: impl Fn() + Send + 'static) -> Self {
        let (send, events) = mpsc::channel();
        let cancel = Arc::new(Cancellation::default());
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            let emit = |event| { let _ = send.send(event); notify(); };
            let Some(binary) = binary() else { return emit(Progress::Failed("helper_missing")); };
            if !install_only {
                emit(Progress::SigningIn);
                let mut enroll = Command::new(&binary);
                enroll.args(["daemon", "enroll", "--cancel-on-stdin-close", "--home"]).arg(&home).arg("--server").arg(server);
                if let Some(owner) = owner { enroll.arg("--owner").arg(owner); }
                if let Some(root) = adopt_root { enroll.arg("--adopt-root").arg(root); }
                if let Err(error) = run(enroll, Duration::from_secs(300), &worker_cancel, true, || emit(Progress::FinishingCancellation)) { return emit(Progress::Failed(failure_code(&error))); }
            }
            emit(Progress::Enabling);
            let mut enable = Command::new(binary);
            enable.args(["daemon", "install", "--home"]).arg(home);
            if let Err(error) = run(enable, Duration::from_secs(30), &worker_cancel, false, || {}) {
                let code = failure_code(&error);
                return emit(Progress::InstallFailed(if code == "setup_failed" { "service_start_failed" } else { code }));
            }
            // launchctl accepting a job is installation, not executor readiness.
            emit(Progress::Installed);
        });
        Self { events, cancel }
    }
    pub fn try_recv(&self) -> Option<Progress> { self.events.try_recv().ok() }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plain_http_servers_are_accepted_only_on_loopback() {
        for server in ["http://127.0.0.1:8088/", "http://localhost:8088", "http://[::1]:8088/", "https://hagency.example"] {
            assert!(valid_server(server), "{server}");
        }
        for server in ["http://hagency.example", "http://192.168.1.10:8088", "http://my-mac.local:8088", "https://user@hagency.example"] {
            assert!(!valid_server(server), "{server}");
        }
    }
    #[test]
    fn helper_errors_are_classified_without_exposing_raw_output() {
        assert_eq!(helper_failure("Error: daemon_other_owner_enrolled account=private"), "daemon_other_owner_enrolled");
        assert_eq!(helper_failure("token=private provider message"), "setup_failed");
        assert_eq!(helper_failure("daemon_install_location_unsupported: /private/account/path"), "daemon_install_location_unsupported");
        assert_eq!(helper_failure("daemon_service_stop_pending: details"), "daemon_service_stop_pending");
        assert_eq!(failure_code(&io::Error::from(io::ErrorKind::TimedOut)), "setup_timed_out");
        assert!(!failure_message("untrusted provider output", false).contains("untrusted"));
    }
    #[test]
    fn enrollment_timeout_waits_for_accepted_custody_after_eof() {
        let mut command = Command::new("/bin/sh");
        // Exit status 0 only after EOF and a drain longer than the former kill grace.
        command.args(["-c", "cat >/dev/null; sleep 3.2; exit 0"]);
        let start = Instant::now();
        let error = run(command, Duration::from_millis(50), &Cancellation::default(), true, || {}).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() >= Duration::from_millis(3200));
    }
}
