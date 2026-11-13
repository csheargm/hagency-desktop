//! Owner hosting on this computer (ADR-192).
//!
//! Each (server, owner) has one owner root per computer, and Desktop is the
//! one that registers its canonical path on first setup. On macOS only the
//! per-user owner daemon opens that root. The daemon runs from this binary
//! (`<exe> daemon …`, dispatched in `main` before the UI); Desktop sets it
//! up, starts and removes its service, and pauses it at quit when the owner
//! chose not to keep the resource pool running. Other platforms keep
//! executing in this process until they ship the daemon.
//!
//! Everything here reads files, runs helper processes or talks to the daemon
//! socket: call it from a worker thread, never from the UI thread. The page
//! state in [`view`] is the exception; it only reads memory.
use std::path::{Path, PathBuf};

pub use ::hagency::daemon::assignment::SetupReason;

/// The argument that selects this binary's daemon command line.
pub const SUBCOMMAND: &str = ::hagency::daemon::cli::SUBCOMMAND;

/// Where this computer keeps the owner-root registry and the daemon home.
#[derive(Clone, Debug)]
pub struct Locations {
    #[cfg(unix)]
    roots: ::hagency::owner_roots::OwnerRoots,
    #[cfg(unix)]
    daemon_home: PathBuf,
}

impl Locations {
    /// The OS user's registry (`~/.hagency-owner-resources`, or a test-support
    /// build's isolated installation) and the one daemon home of every host.
    pub fn system() -> Result<Self, String> {
        #[cfg(unix)]
        return Ok(Self {
            roots: ::hagency::owner_roots::OwnerRoots::system().map_err(|e| e.code().to_owned())?,
            daemon_home: ::hagency::service::default_daemon_dir().map_err(|_| "daemon_home_unavailable".to_owned())?,
        });
        #[cfg(not(unix))]
        Ok(Self {})
    }

    /// A registry and a daemon home inside one private test directory.
    #[cfg(test)]
    pub(crate) fn at(base: &Path) -> Self {
        #[cfg(unix)]
        return Self {
            roots: ::hagency::owner_roots::OwnerRoots::at(base.join("resources")).unwrap(),
            daemon_home: base.join("daemon"),
        };
        #[cfg(not(unix))]
        {
            let _ = base;
            Self {}
        }
    }

    #[cfg(unix)]
    pub fn daemon_home(&self) -> &Path {
        &self.daemon_home
    }
}

#[cfg(unix)]
fn root_code(error: ::hagency::owner_roots::RootError) -> String {
    error.code().to_owned()
}

/// Platforms without the per-user registry use one stable profile-local root.
#[cfg(any(not(unix), test))]
fn profile_owner_root(data_dir: &Path, origin: &str, owner: &str) -> Result<PathBuf, String> {
    let identity = serde_json::to_vec(&(origin, owner)).map_err(|_| "invalid_account_identity")?;
    Ok(data_dir.join("native-owner").join(blake3::hash(&identity).to_hex().as_str()))
}

/// Resolve the one registered owner root; new owners use the canonical path.
pub fn select_root(locations: &Locations, data_dir: &Path, origin: &str, owner: &str) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        let _ = data_dir;
        locations.roots.resolve(origin, owner, None).map_err(root_code)
    }
    #[cfg(not(unix))]
    {
        let _ = locations;
        profile_owner_root(data_dir, origin, owner)
    }
}

/// Signing in does not claim an execution root on a daemon-hosted platform.
/// Setup registers the canonical root; existing registered identities stay pinned.
pub fn sign_in_root(locations: &Locations, data_dir: &Path, origin: &str, owner: &str) -> Result<PathBuf, String> {
    sign_in_root_with(locations, data_dir, origin, owner, cfg!(target_os = "macos"))
}
fn sign_in_root_with(locations: &Locations, data_dir: &Path, origin: &str, owner: &str, delegated: bool) -> Result<PathBuf, String> {
    #[cfg(unix)]
    if delegated {
        return Ok(locations.roots.lookup(origin, owner).map_err(root_code)?
            .unwrap_or_else(||locations.roots.base().join(::hagency::owner_roots::OWNERS)
                .join(::hagency::owner_roots::key(origin, owner))));
    }
    let _ = delegated;
    select_root(locations, data_dir, origin, owner)
}

/// Who executes the owner right now, decided from durable state only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Execution {
    /// In this process at the registered root. Never on macOS.
    InProcess,
    /// The per-user daemon in `home` hosts the owner's registered root.
    Daemon { home: PathBuf },
    /// macOS: hosting must be set up first. Nothing runs for this owner: no
    /// in-process owner and no pool workers.
    Setup(SetupReason),
}

pub fn execution(locations: &Locations, origin: &str, owner: &str) -> Result<Execution, String> {
    #[cfg(unix)]
    {
        use ::hagency::daemon::assignment::{Assignment, resolve_with};
        Ok(match resolve_with(&locations.roots, &locations.daemon_home, origin, owner).map_err(str::to_owned)? {
            Assignment::Local => Execution::InProcess,
            Assignment::Daemon { home, .. } => Execution::Daemon { home },
            Assignment::SetupRequired { reason } => Execution::Setup(reason),
        })
    }
    // No daemon and no registry ship here yet.
    #[cfg(not(unix))]
    {
        let _ = (locations, origin, owner);
        Ok(Execution::InProcess)
    }
}

// ----- the installed service ------------------------------------------------

/// The per-user owner-daemon service as installed on this computer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Unknown,
    NotInstalled,
    /// The LaunchAgent runs this executable.
    ThisApp,
    /// The LaunchAgent runs another program, such as the helper of the former
    /// separate Hagency Hosting app. Installing again points it here.
    OtherProgram,
}

/// The program a LaunchAgent property list starts: the first string of its
/// `ProgramArguments`, unescaped.
#[cfg(any(target_os = "macos", test))]
fn plist_program(plist: &str) -> Option<String> {
    let after = &plist[plist.find("<key>ProgramArguments</key>")?..];
    let array = &after[after.find("<array>")? + "<array>".len()..after.find("</array>")?];
    let start = array.find("<string>")? + "<string>".len();
    let end = start + array[start..].find("</string>")?;
    Some(array[start..end].replace("&quot;", "\"").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&"))
}

#[cfg(any(target_os = "macos", test))]
fn service_at(plist: &Path, exe: &Path) -> Service {
    match std::fs::read_to_string(plist) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Service::NotInstalled,
        Err(_) => Service::Unknown,
        Ok(text) => match plist_program(&text) {
            None => Service::Unknown,
            Some(program) => {
                let program = Path::new(&program);
                if program == exe || program.canonicalize().is_ok_and(|p| p == exe) {
                    Service::ThisApp
                } else {
                    Service::OtherProgram
                }
            }
        },
    }
}

/// The installed owner-daemon LaunchAgent. Only macOS reports one.
pub fn installed_service() -> Service {
    #[cfg(target_os = "macos")]
    {
        let (Some(home), Ok(exe)) = (
            std::env::var_os("HOME").filter(|h| !h.is_empty()),
            std::env::current_exe().and_then(|p| p.canonicalize()),
        ) else {
            return Service::Unknown;
        };
        let plist = PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{}.plist", ::hagency::service::DAEMON_LABEL));
        service_at(&plist, &exe)
    }
    #[cfg(not(target_os = "macos"))]
    Service::Unknown
}

// ----- helper processes -----------------------------------------------------

#[cfg(unix)]
mod helper {
    use std::{
        ffi::OsStr,
        io::Read,
        path::Path,
        process::{ChildStdin, Command, Stdio},
        sync::{Mutex, atomic::{AtomicBool, Ordering}, mpsc},
        time::{Duration, Instant},
    };

    /// Stops a running job. Closing the enrollment helper's stdin is its
    /// cancellation request (`--cancel-on-stdin-close`).
    #[derive(Default)]
    pub struct Cancellation {
        stopped: AtomicBool,
        stdin: Mutex<Option<ChildStdin>>,
    }

    impl Cancellation {
        pub fn cancel(&self) {
            self.stopped.store(true, Ordering::Release);
            self.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        pub fn cancelled(&self) -> bool {
            self.stopped.load(Ordering::Acquire)
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum Failure {
        /// The owner cancelled; the helper finished its cancellation.
        Cancelled,
        TimedOut,
        /// A recognized static code, or `setup_failed`.
        Code(&'static str),
        /// The helper could not be started.
        Unavailable,
    }

    /// This executable's daemon command line, carrying the isolated
    /// installation that a test-support build selected at startup.
    pub fn command(exe: &Path, args: &[&OsStr]) -> Command {
        let mut command = Command::new(exe);
        #[cfg(feature = "onboarding-test-support")]
        if let Some(root) = ::hagency::native_owner::resource_activation_fixture_root() {
            command.arg("--hagency-isolated-installation-root").arg(root);
        }
        command.arg(super::SUBCOMMAND).args(args);
        command
    }

    /// Only recognized codes cross into the UI. Raw helper output can contain
    /// account data and is neither displayed nor logged.
    pub fn failure_code(output: &str) -> &'static str {
        for code in owner_dashboard::i18n::SETUP_FAILURE_CODES {
            if output.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|word| word == *code) {
                return code;
            }
        }
        if output.contains("another owner daemon is using this home") {
            return "daemon_already_running";
        }
        "setup_failed"
    }

    fn capture(pipe: Option<impl Read + Send + 'static>, registration: Option<mpsc::SyncSender<()>>) -> mpsc::Receiver<String> {
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let mut captured = Vec::new();
            if let Some(mut pipe) = pipe {
                let mut buffer = [0u8; 1024];
                let marker = ::hagency::daemon::cli::ENROLLMENT_REGISTERING_LINE.as_bytes();
                let mut line = Vec::with_capacity(marker.len());
                let mut overflow = false;
                while let Ok(count) = pipe.read(&mut buffer) {
                    if count == 0 { break; }
                    let retain = count.min(16_384usize.saturating_sub(captured.len()));
                    captured.extend_from_slice(&buffer[..retain]);
                    for byte in &buffer[..count] {
                        if *byte == b'\n' {
                            if !overflow && line == marker {
                                if let Some(send) = &registration { let _ = send.try_send(()); }
                            }
                            line.clear(); overflow = false;
                        } else if line.len() < marker.len() {
                            line.push(*byte);
                        } else { overflow = true; }
                    }
                }
            }
            let _ = send.send(String::from_utf8_lossy(&captured).into_owned());
        });
        receive
    }

    /// Run one helper to completion and return its standard output. The
    /// enrollment helper owns accepted native custody: after cancellation or
    /// its timeout it receives EOF and is waited for, never killed. Other
    /// helpers are killed at their timeout or three seconds after cancellation.
    pub fn run(command: Command, timeout: Duration, cancel: &Cancellation, enrollment: bool, on_stop: impl Fn()) -> Result<String, Failure> {
        run_observed(command, timeout, cancel, enrollment, on_stop, || {})
    }

    pub fn run_observed(mut command: Command, timeout: Duration, cancel: &Cancellation, enrollment: bool, on_stop: impl Fn(), on_registration: impl Fn()) -> Result<String, Failure> {
        if cancel.cancelled() { return Err(Failure::Cancelled); }
        let mut child = command.stdin(if enrollment { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|_| Failure::Unavailable)?;
        let (phase_send, phase_receive) = mpsc::sync_channel(1);
        let stdout = capture(child.stdout.take(), enrollment.then_some(phase_send));
        let stderr = capture(child.stderr.take(), None);
        let mut registration_reported = false;
        if enrollment { *cancel.stdin.lock().unwrap_or_else(|e| e.into_inner()) = child.stdin.take(); }
        let start = Instant::now();
        let mut stopping: Option<Instant> = None;
        let result = loop {
            if !registration_reported && phase_receive.try_recv().is_ok() {
                registration_reported = true;
                if stopping.is_none() { on_registration(); }
            }
            if stopping.is_none() && (cancel.cancelled() || start.elapsed() >= timeout) {
                cancel.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
                stopping = Some(Instant::now());
                on_stop();
            }
            let stopped = || if cancel.cancelled() { Failure::Cancelled } else { Failure::TimedOut };
            match child.try_wait() {
                Ok(Some(status)) => break if stopping.is_some() {
                    Err(stopped())
                } else if status.success() {
                    Ok(stdout.recv_timeout(Duration::from_millis(500)).unwrap_or_default())
                } else {
                    Err(Failure::Code(failure_code(&stderr.recv_timeout(Duration::from_millis(500)).unwrap_or_default())))
                },
                Err(_) => {
                    if !enrollment { let _ = child.kill(); }
                    let _ = child.wait();
                    break Err(Failure::Unavailable);
                }
                Ok(None) if !enrollment && stopping.is_some_and(|at| at.elapsed() >= Duration::from_secs(3)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(stopped());
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
            }
        };
        cancel.stdin.lock().unwrap_or_else(|e| e.into_inner()).take();
        result
    }

    /// The enrollment helper reports a native registration it could not
    /// finish, for example while another process holds the owner root. The
    /// service retries it when it opens the root.
    pub fn pending_registration(stdout: &str) -> Option<&'static str> {
        const MARK: &str = "native registration is pending (";
        let rest = &stdout[stdout.find(MARK)? + MARK.len()..];
        Some(match &rest[..rest.find(')')?] {
            "owner_root_busy_or_invalid" => "owner_root_busy",
            _ => "registration_pending",
        })
    }
}

// ----- jobs: setup, service start and removal -------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Account {
    pub origin: String,
    pub owner: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobKind {
    /// Register the root, sign a stale enrollment out, enroll through the
    /// browser, install the service and wait for it to report hosting.
    Setup,
    /// Install and start the service of an enrolled owner, then confirm.
    StartService,
    /// Remove the service. The daemon home and enrollment are kept.
    Uninstall,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Preparing,
    SigningIn,
    Registering,
    Cancelling,
    Enabling,
    Confirming,
    Removing,
    /// Finished: the service reports this owner, or was removed.
    Done,
    /// Finished: the service was installed but has not reported hosting.
    Unconfirmed,
    /// Finished: the owner cancelled the browser sign-in.
    Cancelled,
    Failed(&'static str),
}

impl Progress {
    pub fn finished(&self) -> bool {
        matches!(self, Self::Done | Self::Unconfirmed | Self::Cancelled | Self::Failed(_))
    }
}

/// Codes from this module's own steps are static; anything else is generic.
#[cfg(unix)]
fn static_code(code: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "owner_root_not_registered", "owner_root_invalid", "owner_root_record_invalid",
        "owner_root_registry_unavailable", "owner_root_registry_unsupported", "owner_identity_invalid",
        "daemon_home_unavailable", "daemon_enrollment_storage_unavailable", "daemon_owner_root_invalid",
        "daemon_owner_root_unavailable", "invalid_account_identity",
    ];
    KNOWN.iter().copied().find(|known| *known == code).unwrap_or("setup_failed")
}

/// Which setup steps run, decided on the registered root: `(sign out the
/// stale enrollment, enroll)`. Registration alone can hand an enrolled root
/// that holds the account's executor to the daemon; then neither runs.
#[cfg(unix)]
fn setup_steps(before: &Execution, after: &Execution) -> Result<(bool, bool), &'static str> {
    match (before, after) {
        (_, Execution::Setup(SetupReason::OtherOwner)) => Err("daemon_other_owner_enrolled"),
        (Execution::Setup(_), Execution::Daemon { .. }) => Ok((false, false)),
        (_, Execution::Setup(SetupReason::StaleRoot)) => Ok((true, true)),
        _ => Ok((false, true)),
    }
}

/// Wait until no daemon serves `home` and its lock is free.
#[cfg(unix)]
fn released(home: &Path, timeout: std::time::Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if daemon::hello(home, std::time::Duration::from_secs(2)).is_none()
            && !matches!(::hagency::daemon::DaemonHome::open(home), Err(::hagency::daemon::HomeError::AlreadyRunning))
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    false
}

/// Wait until the service reports this owner in a settled hosting state.
#[cfg(unix)]
fn confirm(home: &Path, owner: &str, timeout: std::time::Duration, cancel: &helper::Cancellation) -> Progress {
    use owner_dashboard::dto::HostingState;
    let start = std::time::Instant::now();
    while start.elapsed() < timeout && !cancel.cancelled() {
        if let Some((_, hello)) = daemon::hello(home, std::time::Duration::from_secs(5))
            && hello.owner_id.as_deref() == Some(owner)
            && matches!(hello.hosting, HostingState::Running | HostingState::Paused | HostingState::Stopped)
        {
            return Progress::Done;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Progress::Unconfirmed
}

#[cfg(unix)]
fn run_job(kind: JobKind, account: &Account, data_dir: &Path, cancel: &helper::Cancellation, report: &dyn Fn(Progress)) -> Progress {
    use helper::{Failure, command, run};
    use std::{ffi::OsStr, time::Duration};
    let failed = |failure: Failure| match failure {
        Failure::Cancelled => Progress::Cancelled,
        Failure::TimedOut => Progress::Failed("setup_timed_out"),
        Failure::Code(code) => Progress::Failed(code),
        Failure::Unavailable => Progress::Failed("helper_unavailable"),
    };
    let Ok(exe) = std::env::current_exe() else { return Progress::Failed("helper_unavailable") };
    let locations = match Locations::system() {
        Ok(locations) => locations,
        Err(code) => return Progress::Failed(static_code(&code)),
    };
    let home = locations.daemon_home().as_os_str();
    let (origin, owner) = (account.origin.as_str(), account.owner.as_str());
    if kind == JobKind::Uninstall {
        return match run(command(&exe, &[OsStr::new("uninstall")]), Duration::from_secs(30), cancel, false, || {}) {
            Ok(_) => Progress::Done,
            Err(Failure::Code("setup_failed")) => Progress::Failed("service_remove_failed"),
            Err(failure) => failed(failure),
        };
    }
    if kind == JobKind::Setup && installed_service() == Service::OtherProgram {
        // A service of the former Hosting app would otherwise enroll and sign
        // out with its own code over the socket. Remove it and wait until its
        // daemon has released the home; the install below replaces it.
        match run(command(&exe, &[OsStr::new("uninstall")]), Duration::from_secs(30), &helper::Cancellation::default(), false, || {}) {
            Ok(_) => {}
            Err(Failure::Code("setup_failed")) => return Progress::Failed("service_remove_failed"),
            Err(failure) => return failed(failure),
        }
        if !released(locations.daemon_home(), Duration::from_secs(20)) {
            return Progress::Failed("daemon_service_stop_pending");
        }
    }
    let mut registration_pending = None;
    if kind == JobKind::Setup {
        let before = match execution(&locations, origin, owner) {
            Ok(Execution::Setup(SetupReason::OtherOwner)) => return Progress::Failed("daemon_other_owner_enrolled"),
            Ok(before) => before,
            Err(code) => return Progress::Failed(static_code(&code)),
        };
        // Only Desktop registers the root; the daemon only looks it up.
        if let Err(code) = select_root(&locations, data_dir, origin, owner) {
            return Progress::Failed(static_code(&code));
        }
        let after = match execution(&locations, origin, owner) {
            Ok(after) => after,
            Err(code) => return Progress::Failed(static_code(&code)),
        };
        let (stale, enroll) = match setup_steps(&before, &after) {
            Ok(steps) => steps,
            Err(code) => return Progress::Failed(code),
        };
        if stale {
            // The enrollment names another root: revoke its executor first, so
            // enrolling the registered root never leaves a second executor.
            let sign_out = command(&exe, &[OsStr::new("sign-out"), OsStr::new("--home"), home]);
            if let Err(failure) = run(sign_out, Duration::from_secs(60), &helper::Cancellation::default(), false, || {}) {
                return failed(failure);
            }
        }
        if enroll {
            report(Progress::SigningIn);
            let enrollment = command(&exe, &[
                OsStr::new("enroll"), OsStr::new("--home"), home, OsStr::new("--server"), OsStr::new(origin),
                OsStr::new("--owner"), OsStr::new(owner), OsStr::new("--cancel-on-stdin-close"),
            ]);
            match helper::run_observed(enrollment, Duration::from_secs(300), cancel, true,
                || report(Progress::Cancelling), || report(Progress::Registering)) {
                Ok(stdout) => if let Some(code) = helper::pending_registration(&stdout) {
                    registration_pending = Some(code);
                    makepad_widgets::warning!("Hosting enrollment finished with a pending registration: {code}");
                },
                Err(failure) => return failed(failure),
            }
        }
    }
    report(Progress::Enabling);
    let install = command(&exe, &[OsStr::new("install"), OsStr::new("--home"), home]);
    match run(install, Duration::from_secs(30), &helper::Cancellation::default(), false, || {}) {
        Ok(_) => {}
        Err(Failure::Code("setup_failed")) => return Progress::Failed("service_start_failed"),
        Err(failure) => return failed(failure),
    }
    report(Progress::Confirming);
    let result = confirm(locations.daemon_home(), owner, Duration::from_secs(45), cancel);
    match (result, registration_pending) {
        (Progress::Unconfirmed, Some(code)) => Progress::Failed(code),
        (result, _) => result,
    }
}

// ----- the daemon socket: pause at quit, resume at start --------------------

#[cfg(unix)]
mod daemon {
    #[cfg(target_os = "macos")]
    use owner_dashboard::dto::HostingState;
    use owner_dashboard::transport::{
        API_VERSION, Command, DaemonConnection, DaemonEndpoint, Hello, READ_TIMEOUT, Request, new_request_id,
    };
    #[cfg(target_os = "macos")]
    use owner_dashboard::transport::{OutcomeState, TransportError};
    use std::{path::Path, time::Duration};

    pub fn hello(home: &Path, timeout: Duration) -> Option<(DaemonConnection, Hello)> {
        let mut connection = DaemonConnection::connect(&DaemonEndpoint::new(home)).ok()?;
        let request = Request { version: API_VERSION, request_id: new_request_id("odq"), owner_id: None, expected_revision: None, command: Command::Hello };
        let hello = connection.call(&request, timeout.min(READ_TIMEOUT)).ok()?.hello().ok()?;
        Some((connection, hello))
    }

    #[cfg(target_os = "macos")]
    fn request(owner: &str, command: Command) -> Request {
        Request { version: API_VERSION, request_id: new_request_id("odq"), owner_id: Some(owner.into()), expected_revision: None, command }
    }

    /// Pause hosting of `owner` before Desktop exits. True when this call
    /// paused it (or may have), so the next start resumes it.
    #[cfg(target_os = "macos")]
    pub fn pause_for_quit(home: &Path, owner: &str, timeout: Duration) -> bool {
        let Some((mut connection, hello)) = hello(home, Duration::from_secs(3)) else { return false };
        if hello.owner_id.as_deref() != Some(owner) || !matches!(hello.hosting, HostingState::Running | HostingState::Starting) {
            return false;
        }
        match connection.call(&request(owner, Command::PauseHosting), timeout) {
            Ok(reply) => reply.outcome().is_ok_and(|outcome| outcome.state != OutcomeState::Unchanged),
            Err(TransportError::Uncertain) => true,
            Err(_) => false,
        }
    }

    /// Resume hosting that Desktop paused when it quit. `false` asks to retry
    /// later: the service is not reachable or busy yet.
    #[cfg(target_os = "macos")]
    pub fn resume_after_start(home: &Path, owner: &str) -> bool {
        let Some((mut connection, hello)) = hello(home, READ_TIMEOUT) else { return false };
        if hello.owner_id.as_deref() != Some(owner) {
            // Another owner, or none: nothing of this owner's is paused.
            return true;
        }
        match hello.hosting {
            // Hosting already runs; a stop is never undone here.
            HostingState::Running | HostingState::Stopped | HostingState::Unconfigured => return true,
            HostingState::Draining => return false,
            _ => {}
        }
        match connection.call(&request(owner, Command::ResumeHosting), Duration::from_secs(15)) {
            Ok(reply) => match reply.outcome() {
                Ok(_) => true,
                Err(error) => !matches!(error.code.as_str(), "busy" | "in_progress" | "outcome_unknown" | "daemon_stopping"),
            },
            Err(_) => false,
        }
    }
}

/// Pause hosting of `owner` before Desktop exits; see the daemon module.
/// Blocking, bounded by `timeout`. True when Desktop paused it.
pub fn pause_for_quit(owner: &str, timeout: std::time::Duration) -> bool {
    #[cfg(target_os = "macos")]
    if let Ok(home) = ::hagency::service::default_daemon_dir() {
        return daemon::pause_for_quit(&home, owner, timeout);
    }
    let _ = (owner, timeout);
    false
}

/// Resume hosting that Desktop paused at its last quit. Blocking. False asks
/// the caller to retry later.
pub fn resume_after_start(owner: &str) -> bool {
    #[cfg(target_os = "macos")]
    if let Ok(home) = ::hagency::service::default_daemon_dir() {
        return daemon::resume_after_start(&home, owner);
    }
    let _ = owner;
    true
}

// ----- page state shared by every Hosting page ------------------------------

/// What the Hosting page shows for an account.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// `None` until the first read for this account completes.
    pub execution: Option<Result<Execution, String>>,
    pub service: Option<Service>,
    /// The running job, or the last finished one, for this account.
    pub job: Option<(JobKind, Progress)>,
    /// Changes whenever anything above changes.
    pub generation: u64,
}

#[derive(Default)]
struct Shared {
    account: Option<Account>,
    view: View,
    reading: bool,
    read_at: Option<std::time::Instant>,
    /// One job at a time, whichever account started it.
    #[cfg(unix)]
    running: bool,
    #[cfg(unix)]
    cancel: Option<std::sync::Arc<helper::Cancellation>>,
}

static SHARED: std::sync::LazyLock<std::sync::Mutex<Shared>> = std::sync::LazyLock::new(Default::default);

fn shared() -> std::sync::MutexGuard<'static, Shared> {
    SHARED.lock().unwrap_or_else(|p| p.into_inner())
}

/// Status reads while a page is visible.
const READ_EVERY: std::time::Duration = std::time::Duration::from_secs(2);

/// The page state for `account`, scheduling a background status read when
/// the last one is older than two seconds. Reads only memory.
pub fn view(account: &Account) -> View {
    let mut state = shared();
    if state.account.as_ref() != Some(account) {
        // Another account: its setup can never complete for this one.
        #[cfg(unix)]
        if let Some(cancel) = state.cancel.take() {
            cancel.cancel();
        }
        state.account = Some(account.clone());
        let generation = state.view.generation + 1;
        state.view = View { generation, ..View::default() };
        state.read_at = None;
    }
    if !state.reading && state.read_at.is_none_or(|at| at.elapsed() >= READ_EVERY) {
        state.reading = true;
        let account = account.clone();
        let spawned = std::thread::Builder::new().name("owner-hosting-status".into()).spawn(move || {
            let execution = Locations::system().and_then(|l| execution(&l, &account.origin, &account.owner));
            let service = installed_service();
            let mut state = shared();
            state.reading = false;
            state.read_at = Some(std::time::Instant::now());
            if state.account.as_ref() == Some(&account)
                && (state.view.execution.as_ref() != Some(&execution) || state.view.service != Some(service))
            {
                state.view.execution = Some(execution);
                state.view.service = Some(service);
                state.view.generation += 1;
                drop(state);
                makepad_widgets::SignalToUI::set_ui_signal();
            }
        });
        if spawned.is_err() {
            state.reading = false;
        }
    }
    state.view.clone()
}

/// Start a job for `account`. Refused while another job runs.
pub fn start(kind: JobKind, account: &Account) -> bool {
    #[cfg(unix)]
    {
        let mut state = shared();
        if state.running || state.account.as_ref() != Some(account) {
            return false;
        }
        let cancel = std::sync::Arc::new(helper::Cancellation::default());
        state.running = true;
        state.cancel = Some(cancel.clone());
        let first = match kind {
            JobKind::Setup => Progress::Preparing,
            JobKind::StartService => Progress::Enabling,
            JobKind::Uninstall => Progress::Removing,
        };
        state.view.job = Some((kind, first));
        state.view.generation += 1;
        drop(state);
        let account = account.clone();
        let data_dir = crate::app_data_dir().to_owned();
        let spawned = std::thread::Builder::new().name("owner-hosting-job".into()).spawn(move || {
            let publish = |progress: Progress| {
                let mut state = shared();
                if state.account.as_ref() == Some(&account) {
                    state.view.job = Some((kind, progress.clone()));
                    state.view.generation += 1;
                    // The next read shows the new enrollment or service.
                    if progress.finished() { state.read_at = None; }
                }
                drop(state);
                makepad_widgets::SignalToUI::set_ui_signal();
            };
            let outcome = run_job(kind, &account, &data_dir, &cancel, &publish);
            {
                let mut state = shared();
                state.running = false;
                if state.cancel.as_ref().is_some_and(|c| std::sync::Arc::ptr_eq(c, &cancel)) {
                    state.cancel = None;
                }
            }
            publish(outcome);
        });
        if spawned.is_err() {
            let mut state = shared();
            state.running = false;
            state.cancel = None;
            state.view.job = Some((kind, Progress::Failed("helper_unavailable")));
            state.view.generation += 1;
        }
        true
    }
    #[cfg(not(unix))]
    {
        let _ = (kind, account);
        false
    }
}

/// Cancel the running setup's browser sign-in.
pub fn cancel() {
    #[cfg(unix)]
    if let Some(cancel) = shared().cancel.clone() {
        cancel.cancel();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use ::hagency::daemon::enrollment::Enrollment;
    use ::hagency::native_owner::NativeOwner;
    use std::os::unix::fs::PermissionsExt;

    const ORIGIN: &str = "https://hagency.example.test/";
    const OWNER: &str = "@owner:hagency.example.test";
    const OTHER: &str = "@other:hagency.example.test";

    /// A private temporary directory: tests never touch the user's own
    /// registry, daemon home or Desktop data.
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "hagency-hosting-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            private(&path);
            Self(path.canonicalize().unwrap())
        }
        fn locations(&self) -> Locations {
            let locations = Locations::at(&self.0);
            private(locations.daemon_home());
            locations
        }
        fn data_dir(&self) -> PathBuf {
            self.0.join("desktop-data")
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn private(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// A root an older daemon enrolled under its home, optionally holding an
    /// executor resume record.
    fn enrolled_daemon_root(locations: &Locations, record: Option<bool>) -> PathBuf {
        let root = ::hagency::owner_host::initialize(&locations.daemon_home().join("owners").join("legacy")).unwrap();
        if let Some(active) = record {
            NativeOwner::write_test_resume_record(&root, ORIGIN, OWNER, "daemon-client", active);
        }
        Enrollment::enroll(locations.daemon_home(), ORIGIN, OWNER, None, &root).unwrap();
        root
    }

    fn canonical(locations: &Locations) -> PathBuf {
        locations.roots.base()
            .join(::hagency::owner_roots::OWNERS)
            .join(::hagency::owner_roots::key(ORIGIN, OWNER))
    }

    fn daemon(locations: &Locations) -> Execution {
        Execution::Daemon { home: locations.daemon_home().to_owned() }
    }

    fn obsolete_root(data: &Path, origin: &str, owner: &str) -> Result<PathBuf, String> {
        let identity=serde_json::to_vec(&(origin,owner)).unwrap();
        Ok(data.join("native-owner").join(blake3::hash(&identity).to_hex().as_str()))
    }

    #[test]
    fn profile_local_owner_root_keeps_its_derivation_and_creates_no_second_root() {
        let temp = Temp::new();
        let data = temp.data_dir();
        let expected = obsolete_root(&data, ORIGIN, OWNER).unwrap();
        private(&expected);
        std::fs::write(expected.join("identity-marker"), b"same identity").unwrap();
        assert_eq!(profile_owner_root(&data, ORIGIN, OWNER).unwrap(), expected);
        assert_eq!(std::fs::read(expected.join("identity-marker")).unwrap(), b"same identity");
        assert_eq!(std::fs::read_dir(data.join("native-owner")).unwrap().count(), 1);
    }

    #[test]
    fn unregistered_old_roots_are_not_adopted_implicitly() {
        let temp = Temp::new();
        let locations = temp.locations();
        let legacy = ::hagency::owner_host::initialize(&obsolete_root(&temp.data_dir(), ORIGIN, OWNER).unwrap()).unwrap();
        // An unregistered daemon root does not establish the account's identity.
        enrolled_daemon_root(&locations, Some(true));
        let root = select_root(&locations, &temp.data_dir(), ORIGIN, OWNER).unwrap();
        assert_ne!(root, legacy);
        assert!(legacy.exists());
        assert_eq!(root, canonical(&locations));
        assert_eq!(locations.roots.lookup(ORIGIN, OWNER).unwrap(), Some(root.clone()));
        // Another Desktop profile of the same account finds the same root.
        assert_eq!(select_root(&locations, &temp.0.join("other-profile"), ORIGIN, OWNER).unwrap(), root);
        assert!(!canonical(&locations).exists());
    }

    #[test]
    fn unregistered_daemon_roots_do_not_implicitly_claim_the_owner() {
        for active in [true, false] {
            let temp = Temp::new();
            let locations = temp.locations();
            let daemon_root = enrolled_daemon_root(&locations, Some(active));
            assert_ne!(select_root(&locations, &temp.data_dir(), ORIGIN, OWNER).unwrap(), daemon_root, "active={active}");
            assert_eq!(locations.roots.lookup(ORIGIN, OWNER).unwrap(), Some(canonical(&locations)));
        }
    }

    #[test]
    fn without_an_existing_root_the_canonical_root_is_registered_and_nothing_is_created() {
        let temp = Temp::new();
        let locations = temp.locations();
        // A daemon root without an executor record is not the account's root.
        enrolled_daemon_root(&locations, None);
        let root = select_root(&locations, &temp.data_dir(), ORIGIN, OWNER).unwrap();
        assert_eq!(root, canonical(&locations));
        assert!(!root.exists(), "registration names the root; the daemon creates it");
        // A Desktop root created later never replaces the registration.
        ::hagency::owner_host::initialize(&obsolete_root(&temp.data_dir(), ORIGIN, OWNER).unwrap()).unwrap();
        assert_eq!(select_root(&locations, &temp.data_dir(), ORIGIN, OWNER).unwrap(), root);
        // Another owner's enrollment is never this owner's root.
        let other = Temp::new();
        let locations = other.locations();
        let daemon_root = enrolled_daemon_root(&locations, Some(true));
        assert_ne!(select_root(&locations, &other.data_dir(), ORIGIN, OTHER).unwrap(), daemon_root);
    }

    #[test]
    fn macos_sign_in_uses_only_registered_identity_and_never_claims_an_old_root() {
        let temp = Temp::new();
        let locations = temp.locations();
        let canonical_path = canonical(&locations);
        // A profile without the account's root claims nothing at sign-in.
        assert_eq!(sign_in_root_with(&locations, &temp.data_dir(), ORIGIN, OWNER, true).unwrap(), canonical_path);
        assert_eq!(locations.roots.lookup(ORIGIN, OWNER).unwrap(), None);
        // Setup registers the canonical root; later sign-ins pin it.
        let registered = select_root(&locations, &temp.data_dir(), ORIGIN, OWNER).unwrap();
        assert_eq!(registered, canonical(&locations));
        assert_eq!(sign_in_root_with(&locations, &temp.data_dir(), ORIGIN, OWNER, true).unwrap(), registered);
        // An old root remains unclaimed until explicitly registered. Once
        // registered, sign-in preserves the existing daemon identity.
        let other = Temp::new();
        let locations = other.locations();
        let own = ::hagency::owner_host::initialize(&obsolete_root(&other.data_dir(), ORIGIN, OWNER).unwrap()).unwrap();
        Enrollment::enroll(locations.daemon_home(), ORIGIN, OWNER, None, &own).unwrap();
        assert_eq!(sign_in_root_with(&locations, &other.data_dir(), ORIGIN, OWNER, true).unwrap(), canonical(&locations));
        assert_eq!(locations.roots.lookup(ORIGIN, OWNER).unwrap(), None);
        locations.roots.resolve(ORIGIN, OWNER, Some(&own)).unwrap();
        assert_eq!(sign_in_root_with(&locations, &other.data_dir(), ORIGIN, OWNER, true).unwrap(), own);
        assert_eq!(locations.roots.lookup(ORIGIN, OWNER).unwrap(), Some(own));
        assert_eq!(execution(&locations, ORIGIN, OWNER).unwrap(), daemon(&locations));
        // Where Desktop executes in process, a sign-in needs a root at once.
        let third = Temp::new();
        let locations = third.locations();
        assert_eq!(sign_in_root_with(&locations, &third.data_dir(), ORIGIN, OWNER, false).unwrap(), canonical(&locations));
    }

    #[test]
    fn macos_requires_setup_and_never_runs_the_owner_in_this_process() {
        let temp = Temp::new();
        let locations = temp.locations();
        let setup = |reason| if cfg!(target_os = "macos") { Execution::Setup(reason) } else { Execution::InProcess };
        // Nothing enrolled, before and after registration.
        assert_eq!(execution(&locations, ORIGIN, OWNER).unwrap(), setup(SetupReason::NotEnrolled));
        ::hagency::owner_host::initialize(&obsolete_root(&temp.data_dir(), ORIGIN, OWNER).unwrap()).unwrap();
        let registered = select_root(&locations, &temp.data_dir(), ORIGIN, OWNER).unwrap();
        assert_eq!(execution(&locations, ORIGIN, OWNER).unwrap(), setup(SetupReason::NotEnrolled));
        // A daemon enrollment from before ADR-192 names another root: stale.
        enrolled_daemon_root(&locations, Some(true));
        assert_eq!(execution(&locations, ORIGIN, OWNER).unwrap(), setup(SetupReason::StaleRoot));
        // The daemon hosts another account: one Mac user hosts one owner.
        assert_eq!(execution(&locations, ORIGIN, OTHER).unwrap(), setup(SetupReason::OtherOwner));
        // Enrolling the registered root hands execution to the daemon.
        Enrollment::enroll(locations.daemon_home(), ORIGIN, OWNER, None, &registered).unwrap();
        assert_eq!(execution(&locations, ORIGIN, OWNER).unwrap(), daemon(&locations));
        assert_eq!(execution(&locations, ORIGIN.trim_end_matches('/'), OWNER).unwrap(), daemon(&locations));
    }

    #[test]
    fn setup_signs_out_only_a_stale_enrollment_and_skips_what_registration_settled() {
        let home = || Execution::Daemon { home: "/h".into() };
        let setup = Execution::Setup;
        assert_eq!(setup_steps(&setup(SetupReason::NotEnrolled), &setup(SetupReason::NotEnrolled)), Ok((false, true)));
        assert_eq!(setup_steps(&setup(SetupReason::StaleRoot), &setup(SetupReason::StaleRoot)), Ok((true, true)));
        // Registering the enrolled root resolved it: keep its executor.
        assert_eq!(setup_steps(&setup(SetupReason::StaleRoot), &home()), Ok((false, false)));
        // Already hosted: a renewed sign-in enrolls again without a sign-out.
        assert_eq!(setup_steps(&home(), &home()), Ok((false, true)));
        assert_eq!(setup_steps(&setup(SetupReason::NotEnrolled), &setup(SetupReason::OtherOwner)), Err("daemon_other_owner_enrolled"));
    }

    #[test]
    fn the_service_check_names_this_app_another_program_or_none() {
        let temp = Temp::new();
        let exe = temp.0.join("Hagency Desktop.app/Contents/MacOS/rinx");
        let plist = temp.0.join("io.hagency.owner-daemon.plist");
        assert_eq!(service_at(&plist, &exe), Service::NotInstalled);
        let write = |program: &Path| std::fs::write(&plist, ::hagency::service::daemon_launchd_plist(
            program, Path::new("/home/a/owner-daemon"), "/usr/bin", Path::new("/tmp/owner-daemon.log"))).unwrap();
        write(&exe);
        assert_eq!(service_at(&plist, &exe), Service::ThisApp);
        write(Path::new("/Applications/Hagency Hosting.app/Contents/Helpers/hagency"));
        assert_eq!(service_at(&plist, &exe), Service::OtherProgram);
        assert_eq!(plist_program("<key>ProgramArguments</key><array><string>/a &amp; b</string></array>").as_deref(), Some("/a & b"));
        std::fs::write(&plist, "not a property list").unwrap();
        assert_eq!(service_at(&plist, &exe), Service::Unknown);
    }

    #[test]
    fn helper_failures_are_classified_without_exposing_raw_output() {
        use helper::{failure_code, pending_registration};
        assert_eq!(failure_code("Error: daemon_other_owner_enrolled account=private"), "daemon_other_owner_enrolled");
        assert_eq!(failure_code("Error: owner_root_not_registered"), "owner_root_not_registered");
        assert_eq!(failure_code("daemon_install_location_unsupported: /private/account/path"), "daemon_install_location_unsupported");
        assert_eq!(failure_code("Error: another owner daemon is using this home"), "daemon_already_running");
        assert_eq!(failure_code("token=private provider message"), "setup_failed");
        assert_eq!(pending_registration("Enrolled @o:s but native registration is pending (owner_root_busy_or_invalid). The"), Some("owner_root_busy"));
        assert_eq!(pending_registration("Enrolled @o:s but native registration is pending (server_unavailable)."), Some("registration_pending"));
        assert_eq!(pending_registration("Enrolled @o:s with the existing executor"), None);
        assert_eq!(static_code("owner_root_record_invalid"), "owner_root_record_invalid");
        assert_eq!(static_code("/private/path"), "setup_failed");
    }

    #[test]
    fn enrollment_progress_is_streamed_bounded_and_contains_no_helper_text() {
        use helper::{Cancellation, run_observed};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;
        let count = AtomicUsize::new(0);
        let directory = Temp::new();
        let acknowledgement = directory.0.join("registration-observed");
        let mut command = std::process::Command::new("python3");
        command.env("HAGENCY_PROGRESS_ACK", &acknowledgement).args(["-c", r#"
import os, sys, time
from pathlib import Path
sys.stdout.write('https://test/authorize?secret=private\nHAGENCY_ENROLLMENT_NATIVE_')
sys.stdout.flush()
time.sleep(0.02)
sys.stdout.write('REGISTRATION_V1\n')
sys.stdout.flush()
deadline = time.monotonic() + 5
while not Path(os.environ['HAGENCY_PROGRESS_ACK']).exists():
    if time.monotonic() > deadline: sys.exit(1)
    time.sleep(0.01)
"#]);
        let output = run_observed(command, Duration::from_secs(10), &Cancellation::default(), true, || {}, || {
            // The helper cannot exit successfully until this callback runs.
            std::fs::write(&acknowledgement, b"observed").unwrap();
            count.fetch_add(1, Ordering::AcqRel);
        }).unwrap();
        assert!(output.contains("secret=private"));
        assert_eq!(count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn enrollment_cancellation_waits_for_the_helper_and_other_helpers_are_bounded() {
        use helper::{Cancellation, Failure, run};
        use std::time::{Duration, Instant};
        let shell = |script: &str| {
            let mut command = std::process::Command::new("/bin/sh");
            command.args(["-c", script]);
            command
        };
        // EOF reaches the enrollment helper; its own drain decides when it ends.
        let start = Instant::now();
        let error = run(shell("cat >/dev/null; sleep 1.2; exit 0"), Duration::from_millis(50), &Cancellation::default(), true, || {}).unwrap_err();
        assert_eq!(error, Failure::TimedOut);
        assert!(start.elapsed() >= Duration::from_millis(1200));
        let cancel = Cancellation::default();
        cancel.cancel();
        assert_eq!(run(shell("exit 0"), Duration::from_secs(5), &cancel, true, || {}), Err(Failure::Cancelled));
        assert_eq!(run(shell("echo ok"), Duration::from_secs(5), &Cancellation::default(), false, || {}).unwrap().trim(), "ok");
        assert_eq!(
            run(shell("echo 'Error: owner_root_invalid' >&2; exit 1"), Duration::from_secs(5), &Cancellation::default(), false, || {}),
            Err(Failure::Code("owner_root_invalid"))
        );
        let start = Instant::now();
        assert_eq!(run(shell("sleep 30"), Duration::from_millis(200), &Cancellation::default(), false, || {}), Err(Failure::TimedOut));
        assert!(start.elapsed() < Duration::from_secs(10));
    }
}
