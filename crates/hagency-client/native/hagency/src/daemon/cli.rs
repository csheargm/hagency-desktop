//! The owner daemon's command line, `<exe> daemon <subcommand> …` (ADR-192).
//!
//! Any binary that hosts the daemon dispatches it first in a synchronous
//! `main`, right after the guardian entry and before any UI or async runtime:
//!
//! ```no_run
//! if let Some(result) = hagency::native_owner::run_guardian_if_requested() {
//!     std::process::exit(if result.is_ok() { 0 } else { 1 });
//! }
//! if let Some(status) = hagency::daemon::cli::run_if_requested() {
//!     std::process::exit(status);
//! }
//! // … start the UI
//! ```
//!
//! `daemon install` writes a LaunchAgent that runs `current_exe() daemon run
//! --home <home>`, so it works for every executable that calls this entry.
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use super::host;
#[cfg(unix)]
use std::path::{Path, PathBuf};

/// The argument that selects the daemon command line.
pub const SUBCOMMAND: &str = "daemon";
/// A bounded, versioned display event; never an authorization receipt.
pub const ENROLLMENT_REGISTERING_LINE: &str = "HAGENCY_ENROLLMENT_NATIVE_REGISTRATION_V1";

#[cfg(unix)]
async fn observe_enrollment<T>(
    future: impl std::future::Future<Output = T>,
    control: std::sync::Arc<host::EnrollmentControl>,
    on_registration: impl Fn(),
) -> T {
    tokio::pin!(future);
    let mut timer = tokio::time::interval(std::time::Duration::from_millis(100));
    let mut reported = false;
    loop {
        tokio::select! {
            result = &mut future => return result,
            _ = timer.tick() => {
                if !reported && control.native_registration_started() {
                    reported = true;
                    on_registration();
                }
            }
        }
    }
}
/// Development builds only: an explicitly isolated installation, which the
/// LaunchAgent of such a build repeats before [`SUBCOMMAND`].
#[cfg(all(feature = "test-support", feature = "resource-adoption"))]
const ISOLATED_INSTALLATION: &str = "--hagency-isolated-installation-root";

/// Daemon subcommands. Every `--home` defaults to
/// [`crate::service::default_daemon_dir`].
#[cfg(unix)]
#[derive(clap::Subcommand, Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Run the daemon in the foreground (used by the LaunchAgent).
    Run {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Sign the daemon in through the browser with its own device and record
    /// the owner's one registered owner root. An active executor in that root
    /// is proved in place; otherwise a new executor device is registered.
    Enroll {
        #[arg(long)]
        home: Option<PathBuf>,
        #[arg(long)]
        server: String,
        #[arg(long)]
        owner: String,
        #[arg(long)]
        display_name: Option<String>,
        /// Cancel browser enrollment if its launching controller closes.
        #[arg(long, hide = true)]
        cancel_on_stdin_close: bool,
        #[arg(long)]
        no_open: bool,
        /// Register a new owner root first. Only for a computer where this
        /// account has no Desktop root; Desktop registers its own.
        #[arg(long)]
        register_root: bool,
    },
    /// Show the running daemon's owner and hosting snapshot.
    Status {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Stop admitting new work; running work finishes.
    Pause {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Resume admission, restarting hosting if it was stopped.
    Resume {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Drain and stop hosting; the daemon stays reachable.
    Stop {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Explicit owner sign-out: revoke native and SDK credentials. The owner
    /// root and its registration are kept.
    SignOut {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Register this executable's daemon as the user's LaunchAgent / systemd
    /// user service.
    Install {
        #[arg(long)]
        home: Option<PathBuf>,
    },
    /// Remove the daemon service; its home and enrollment are kept.
    Uninstall,
}

#[cfg(unix)]
#[derive(clap::Parser, Debug)]
#[command(about = "Hagency owner daemon")]
struct Invocation {
    #[cfg(all(feature = "test-support", feature = "resource-adoption"))]
    #[arg(
        long = "hagency-isolated-installation-root",
        global = true,
        hide = true
    )]
    isolated_installation_root: Option<PathBuf>,
    #[command(subcommand)]
    top: Top,
}

#[cfg(unix)]
#[derive(clap::Subcommand, Debug)]
enum Top {
    /// Per-user owner daemon: hosts the enrolled owner independently of any UI.
    Daemon {
        #[command(subcommand)]
        command: Command,
    },
}

/// Whether `args` (including the program name) ask for the daemon command line.
pub fn requested<S: AsRef<OsStr>>(args: &[S]) -> bool {
    let mut rest = args.iter().skip(1).map(AsRef::as_ref);
    let first = rest.next();
    #[cfg(all(feature = "test-support", feature = "resource-adoption"))]
    let first = match first {
        Some(flag) if flag == ISOLATED_INSTALLATION => rest.nth(1),
        Some(flag)
            if flag.to_str().is_some_and(|f| {
                f.starts_with(ISOLATED_INSTALLATION)
                    && f[ISOLATED_INSTALLATION.len()..].starts_with('=')
            }) =>
        {
            rest.next()
        }
        other => other,
    };
    first.is_some_and(|arg| arg == SUBCOMMAND)
}

/// `Some(exit status)` after running `daemon …` when the process was started
/// with it, `None` otherwise. Builds its own runtime: call it from a
/// synchronous `main`, before any UI or async runtime starts.
pub fn run_if_requested() -> Option<i32> {
    let args: Vec<OsString> = std::env::args_os().collect();
    if let Some(status) = crate::documents::worker_if_requested(&args) {
        return Some(status);
    }
    if !requested(&args) {
        return None;
    }
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(std::io::stderr)
        .try_init();
    #[cfg(unix)]
    raise_open_file_limit();
    Some(run_args(args))
}

/// Open files the daemon asks for. 10240 is macOS's OPEN_MAX, the most that
/// setrlimit accepts there.
pub(crate) const OPEN_FILES: u64 = 10240;

/// The daemon holds every pooled Agent's end-to-end encryption stores, each a
/// pool of SQLite connections. Under launchd's default limit of 256 open files
/// it ran out after about half an hour; every owner-root check then failed as
/// `source_root_changed` and no run was claimed. Desktop raises its own limit
/// when its window starts, which the daemon never reaches.
#[cfg(unix)]
fn raise_open_file_limit() {
    use rustix::process::{Resource, getrlimit, setrlimit};
    let mut limit = getrlimit(Resource::Nofile);
    let wanted = limit.maximum.map_or(OPEN_FILES, |max| max.min(OPEN_FILES));
    if limit.current.is_some_and(|current| current < wanted) {
        let raised = rustix::process::Rlimit { current: Some(wanted), maximum: limit.maximum };
        if setrlimit(Resource::Nofile, raised.clone()).is_ok() {
            limit = raised;
        }
    }
    tracing::info!(soft = ?limit.current, "Owner daemon open file limit");
}

#[cfg(unix)]
fn run_args(args: Vec<OsString>) -> i32 {
    use clap::Parser;
    let invocation = match Invocation::try_parse_from(args) {
        Ok(invocation) => invocation,
        Err(error) => {
            let _ = error.print();
            return error.exit_code();
        }
    };
    #[cfg(all(feature = "test-support", feature = "resource-adoption"))]
    if let Some(root) = &invocation.isolated_installation_root
        && let Err(code) = isolated_installation(root)
    {
        eprintln!("Error: {code}");
        return 1;
    }
    let runtime = match daemon_runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("Error: {error}");
            return 1;
        }
    };
    let Top::Daemon { command } = invocation.top;
    match runtime.block_on(run(command)) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Error: {error}");
            1
        }
    }
}

#[cfg(unix)]
fn daemon_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    // launchd does not inherit the Desktop launcher's RUST_MIN_STACK. Native
    // adoption and budget recovery futures need the same stack in this entry
    // point, including unoptimized builds and blocking initialization workers.
    tokio::runtime::Builder::new_multi_thread()
        .thread_stack_size(16 * 1024 * 1024)
        .enable_all()
        .build()
}

#[cfg(not(unix))]
fn run_args(_args: Vec<OsString>) -> i32 {
    eprintln!("Error: the Hagency owner daemon is not available on this platform");
    1
}

/// The same explicit installation the host may already have selected at
/// startup; a different one is refused.
#[cfg(all(unix, feature = "test-support", feature = "resource-adoption"))]
fn isolated_installation(root: &Path) -> Result<(), String> {
    if !root.is_absolute() {
        return Err("isolated installation root must be absolute".into());
    }
    let selected = crate::native_owner::resource_activation_fixture_root();
    if selected.is_some() && selected == root.canonicalize().ok() {
        return Ok(());
    }
    crate::native_owner::initialize_resource_activation_fixture(root).map_err(|error| error.code)
}

#[cfg(unix)]
fn absolute(path: PathBuf) -> Result<PathBuf, std::io::Error> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

#[cfg(unix)]
fn daemon_home(home: Option<PathBuf>) -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(absolute(match home {
        Some(path) => path,
        None => super::host::default_home()?,
    })?)
}

#[cfg(unix)]
async fn call(
    home: &Path,
    command: super::Command,
) -> Result<super::Reply, Box<dyn std::error::Error>> {
    use super::{DaemonClient, IpcError, Request, new_request_id};
    let mut client = DaemonClient::connect(home).await?;
    let hello = client
        .call(&Request::new(new_request_id(), None, super::Command::Hello))
        .await?
        .hello()
        .map_err(|e| e.message)?;
    let request = Request::new(new_request_id(), hello.owner_id, command);
    // An uncertain reply is retried once with the same request ID.
    match client.call(&request).await {
        Ok(reply) => Ok(reply),
        Err(IpcError::Uncertain) => Ok(DaemonClient::connect(home).await?.call(&request).await?),
        Err(e) => Err(e.into()),
    }
}

/// Run one daemon subcommand to completion.
#[cfg(unix)]
pub async fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    use super::{Command as C, host};
    match command {
        Command::Run { home } => {
            let home = daemon_home(home)?;
            let cancel = hagency_matrix::CancellationToken::new();
            let signal = cancel.clone();
            tokio::spawn(async move {
                if let Ok(mut termination) =
                    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                {
                    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=termination.recv()=>{}}
                } else {
                    let _ = tokio::signal::ctrl_c().await;
                }
                signal.cancel();
            });
            host::run(&home, cancel).await?;
        }
        Command::Enroll {
            home,
            server,
            owner,
            display_name,
            cancel_on_stdin_close,
            no_open,
            register_root,
        } => {
            let home = daemon_home(home)?;
            if register_root {
                crate::owner_roots::resolve(&server, &owner, None)
                    .map_err(|e| e.code())?;
            }
            let control = std::sync::Arc::new(host::EnrollmentControl::default());
            let enrollment = host::enroll_controlled(
                &home,
                &server,
                &owner,
                display_name,
                control.clone(),
                |url| async move {
                    println!("Sign in to enroll the Hagency owner daemon: {url}");
                    if !no_open {
                        crate::service::open_owner_link(&url);
                    }
                    Ok(())
                },
            );
            let enrollment = observe_enrollment(enrollment, control.clone(), || {
                use std::io::Write;
                println!("{ENROLLMENT_REGISTERING_LINE}");
                let _ = std::io::stdout().flush();
            });
            let report = if cancel_on_stdin_close {
                let (cancel, cancelled) = tokio::sync::oneshot::channel();
                // A detached OS reader must not keep the Tokio runtime alive
                // after successful enrollment while the controller owns stdin.
                std::thread::spawn(move || {
                    use std::io::Read;
                    let mut byte = [0];
                    let _ = std::io::stdin().read(&mut byte);
                    let _ = cancel.send(());
                });
                // Closing the controller cancels OAuth or detaches remote
                // enrollment. Once native registration is admitted, keep its
                // original future until cleanup is acknowledged.
                let cancel_control = control.clone();
                let watcher = tokio::spawn(async move {
                    let _ = cancelled.await;
                    cancel_control.cancel();
                });
                let report = enrollment.await;
                watcher.abort();
                report?
            } else {
                enrollment.await?
            };
            let executor = if report.existing_executor {
                "the existing executor"
            } else {
                "a new executor"
            };
            match report.pending {
                None => println!(
                    "Enrolled {} with {executor} (owner root {}). Start the daemon with `daemon run` or install it with `daemon install`.",
                    report.owner,
                    report.owner_root.display()
                ),
                Some(code) => println!(
                    "Enrolled {} but native registration is pending ({code}). The owner root may still be in use by another process.",
                    report.owner
                ),
            }
        }
        Command::Status { home } => {
            let home = daemon_home(home)?;
            match host::probe(&home).await {
                host::Presence::NotRunning => println!("Hagency owner daemon is not running."),
                host::Presence::Unsafe => {
                    return Err("the daemon socket is not private to this user".into());
                }
                host::Presence::Running(_) => {
                    let reply = call(
                        &home,
                        C::Snapshot {
                            period: None,
                            usage_filter: None,
                        },
                    )
                    .await?;
                    println!("{}", serde_json::to_string_pretty(&reply)?);
                }
            }
        }
        Command::Pause { home } => {
            let reply = call(&daemon_home(home)?, C::PauseHosting).await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
        }
        Command::Resume { home } => {
            let reply = call(&daemon_home(home)?, C::ResumeHosting).await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
        }
        Command::Stop { home } => {
            let reply = call(&daemon_home(home)?, C::StopHosting).await?;
            println!("{}", serde_json::to_string_pretty(&reply)?);
        }
        Command::SignOut { home } => {
            host::sign_out(&daemon_home(home)?).await?;
            println!("Owner signed out of the Hagency owner daemon. Its owner root is kept.");
        }
        Command::Install { home } => {
            println!("{}", crate::service::install_daemon(&daemon_home(home)?)?)
        }
        Command::Uninstall => println!("{}", crate::service::uninstall_daemon()?),
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[cfg(unix)]
    #[tokio::test]
    async fn enrollment_progress_is_reported_before_the_original_future_finishes() {
        use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
        let control = Arc::new(super::host::EnrollmentControl::default());
        control.accept_native().unwrap();
        let count = AtomicUsize::new(0);
        super::observe_enrollment(async {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            assert_eq!(count.load(Ordering::Acquire), 1);
        }, control, || { count.fetch_add(1, Ordering::AcqRel); }).await;
        let cancelled = Arc::new(super::host::EnrollmentControl::default());
        cancelled.cancel();
        super::observe_enrollment(async {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        }, cancelled, || { panic!("cancelled OAuth is not native registration"); }).await;
    }
    use super::*;
    use clap::Parser;

    #[test]
    fn daemon_workers_have_stack_without_launcher_environment() {
        const CHILD: &str = "HAGENCY_DAEMON_STACK_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            #[inline(never)]
            fn native_stack_work() {
                let mut frame = [0_u8; 3 * 1024 * 1024];
                let frame = std::hint::black_box(&mut frame);
                frame[0] = 17;
                frame[frame.len() - 1] = 29;
                assert_eq!(std::hint::black_box(frame[0] + frame[frame.len() - 1]), 46);
            }
            let runtime = daemon_runtime().unwrap();
            runtime.block_on(async {
                tokio::spawn(async { native_stack_work() }).await.unwrap();
                tokio::task::spawn_blocking(native_stack_work).await.unwrap();
            });
            return;
        }
        // CI normally exports RUST_MIN_STACK, which hid the launchd failure.
        // Isolate the environment change and a possible stack-overflow abort.
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "daemon::cli::tests::daemon_workers_have_stack_without_launcher_environment", "--nocapture"])
            .env(CHILD, "1")
            .env_remove("RUST_MIN_STACK")
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"), "stack probe must execute");
    }

    fn parse(args: &[&str]) -> Result<Command, clap::Error> {
        Invocation::try_parse_from(args).map(|i| {
            let Top::Daemon { command } = i.top;
            command
        })
    }

    #[test]
    fn only_a_daemon_invocation_is_dispatched() {
        for args in [
            &["rinx", "daemon", "run", "--home", "/tmp/h"][..],
            &[
                "/Applications/Rinx.app/Contents/MacOS/rinx",
                "daemon",
                "status",
            ],
            &["hagency", "daemon"],
        ] {
            assert!(requested(args), "{args:?}");
        }
        for args in [
            &["rinx"][..],
            &["rinx", "guardian"],
            &["rinx", "serve", "daemon"],
            &["rinx", "--daemon"],
            &[],
        ] {
            assert!(!requested(args), "{args:?}");
        }
        #[cfg(all(feature = "test-support", feature = "resource-adoption"))]
        {
            assert!(requested(&[
                "rinx",
                ISOLATED_INSTALLATION,
                "/tmp/isolated owner",
                "daemon",
                "run"
            ]));
            assert!(requested(&[
                "rinx",
                "--hagency-isolated-installation-root=/tmp/i",
                "daemon",
                "run"
            ]));
            assert!(!requested(&[
                "rinx",
                ISOLATED_INSTALLATION,
                "/tmp/i",
                "serve"
            ]));
        }
    }

    #[test]
    fn subcommands_parse_and_handover_commands_are_gone() {
        assert_eq!(
            parse(&["x", "daemon", "run", "--home", "/tmp/h"]).unwrap(),
            Command::Run {
                home: Some("/tmp/h".into())
            }
        );
        for args in [
            &["x", "daemon", "status"][..],
            &["x", "daemon", "pause"],
            &["x", "daemon", "resume"],
            &["x", "daemon", "stop"],
            &["x", "daemon", "sign-out"],
            &["x", "daemon", "install"],
            &["x", "daemon", "uninstall"],
            &[
                "x",
                "daemon",
                "enroll",
                "--server",
                "https://s.example/",
                "--owner",
                "@o:s.example",
            ],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        for args in [
            &["x", "daemon", "release"][..],
            &["x", "daemon", "enroll", "--server", "https://s.example/"],
            &["x", "daemon", "enroll", "--owner", "@o:s.example"],
            &[
                "x",
                "daemon",
                "enroll",
                "--server",
                "https://s.example/",
                "--owner",
                "@o:s.example",
                "--adopt-root",
                "/x",
            ],
            &["x", "daemon", "run", "--agent-driver", "x"],
            &["x", "daemon"],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn the_launch_agent_arguments_dispatch_to_daemon_run() {
        let exe = Path::new("/Applications/Hagency.app/Contents/MacOS/hagency-desktop");
        let home = Path::new("/Users/a/Library/Application Support/Hagency/owner-daemon");
        let args = crate::service::daemon_arguments(exe, home, None);
        assert!(requested(&args));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        assert_eq!(
            parse(&args).unwrap(),
            Command::Run {
                home: Some(home.into())
            }
        );
        #[cfg(all(feature = "test-support", feature = "resource-adoption"))]
        {
            let isolated =
                crate::service::daemon_arguments(exe, home, Some(Path::new("/tmp/isolated")));
            assert!(requested(&isolated));
            let invocation = Invocation::try_parse_from(&isolated).unwrap();
            assert_eq!(
                invocation.isolated_installation_root,
                Some(PathBuf::from("/tmp/isolated"))
            );
        }
    }

    #[test]
    fn dispatch_runs_the_command_and_reports_its_exit_status() {
        let temp = tempfile::Builder::new()
            .prefix("hc")
            .tempdir_in("/tmp")
            .unwrap();
        let home = temp.path().join("daemon").to_string_lossy().into_owned();
        // No daemon runs on this home: status answers without creating it.
        assert_eq!(
            run_args(
                ["x", "daemon", "status", "--home", &home]
                    .map(OsString::from)
                    .to_vec()
            ),
            0
        );
        assert!(!Path::new(&home).exists());
        // Sign-out needs an enrollment; the error is a failure status.
        assert_eq!(
            run_args(
                ["x", "daemon", "sign-out", "--home", &home]
                    .map(OsString::from)
                    .to_vec()
            ),
            1
        );
        // A usage error is clap's status.
        assert_eq!(
            run_args(["x", "daemon", "release"].map(OsString::from).to_vec()),
            2
        );
    }
}
