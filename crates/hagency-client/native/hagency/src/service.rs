//! Per-user service registration for Desktop's headless owner daemon.
use std::{path::{Path, PathBuf}, time::{Duration, Instant}};

/// Owner daemon home (ADR 0012, ADR-192), one location for Desktop, the
/// command line and the LaunchAgent: `~/Library/Application Support/Hagency/owner-daemon`
/// on macOS, else `$XDG_STATE_HOME/hagency/owner-daemon` or
/// `~/.local/state/hagency/owner-daemon`. A non-empty
/// `HAGENCY_OWNER_DAEMON_HOME` overrides it for tests. Separate from the
/// owner-client state directory, whose entries are a closed set.
pub fn default_daemon_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("HAGENCY_OWNER_DAEMON_HOME").filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(daemon_dir(
        &home()?,
        std::env::var_os("XDG_STATE_HOME"),
        cfg!(target_os = "macos"),
    ))
}

fn daemon_dir(home: &Path, xdg_state: Option<std::ffi::OsString>, macos: bool) -> PathBuf {
    if macos {
        return home.join("Library/Application Support/Hagency/owner-daemon");
    }
    match xdg_state {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("hagency/owner-daemon"),
        _ => home.join(".local/state/hagency/owner-daemon"),
    }
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_owned())
}

pub fn open_owner_link(link: &str) {
    open_browser(link);
}
fn open_browser(link: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(link)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn launchctl(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| format!("launchctl: {e}"))
}

fn uid() -> String {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn systemctl(args: &[&str]) -> Result<(), String> {
    let out = std::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("systemctl --user: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "systemctl --user {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// LaunchAgent label of the owner daemon.
pub const DAEMON_LABEL: &str = "io.hagency.owner-daemon";

/// Per-user service definition for `<exe> daemon run`, where `<exe>` is any
/// binary that dispatches [`crate::daemon::cli::run_if_requested`]. KeepAlive
/// restarts it after a crash; a clean exit (owner stop of the whole service)
/// stays down.
pub fn daemon_launchd_plist(exe: &Path, home_dir: &Path, path: &str, log: &Path) -> String {
    daemon_launchd_plist_with_installation(exe, home_dir, path, log, None)
}

pub(crate) fn daemon_arguments(exe: &Path, home_dir: &Path, installation: Option<&Path>) -> Vec<String> {
    let mut args = vec![exe.to_string_lossy().into_owned()];
    if let Some(root) = installation {
        args.extend([
            "--hagency-isolated-installation-root".into(),
            root.to_string_lossy().into_owned(),
        ]);
    }
    args.extend([
        "daemon".into(),
        "run".into(),
        "--home".into(),
        home_dir.to_string_lossy().into_owned(),
    ]);
    args
}

// The daemon answers people who are waiting in a room and renews a 30 second
// lease for every run. As a `Background` job macOS scheduled it, and the
// coding tools it starts, below all other work on the computer: a busy
// computer left it without processor time and running Agents were ended as
// expired. `Standard` applies no such limit.
fn daemon_launchd_plist_with_installation(exe: &Path, home_dir: &Path, path: &str, log: &Path, installation: Option<&Path>) -> String {
    let args = daemon_arguments(exe, home_dir, installation);
    let args: String = args
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml(a)))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{DAEMON_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
{args}  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ProcessType</key>
  <string>Standard</string>
  <key>SoftResourceLimits</key>
  <dict>
    <key>NumberOfFiles</key>
    <integer>{open_files}</integer>
  </dict>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
        path = xml(path),
        log = xml(&log.to_string_lossy()),
        open_files = crate::daemon::cli::OPEN_FILES,
    )
}

pub fn daemon_systemd_unit(exe: &Path, home_dir: &Path, path: &str) -> String {
    daemon_systemd_unit_with_installation(exe, home_dir, path, None)
}

fn daemon_systemd_unit_with_installation(exe: &Path, home_dir: &Path, path: &str, installation: Option<&Path>) -> String {
    let quote = |p: &str| format!("\"{}\"", p.replace('\\', "\\\\").replace('"', "\\\""));
    let prefix = if let Some(root) = installation {
        format!("{} --hagency-isolated-installation-root {}", quote(&exe.to_string_lossy()), quote(&root.to_string_lossy()))
    } else {
        quote(&exe.to_string_lossy())
    };
    format!(
        "[Unit]\nDescription=Hagency owner daemon\nAfter=network-online.target\n\n[Service]\nExecStart={} daemon run --home {}\nEnvironment={}\nRestart=on-failure\nRestartSec=5\nLimitNOFILE={}\n\n[Install]\nWantedBy=default.target\n",
        prefix,
        quote(&home_dir.to_string_lossy()),
        quote(&format!("PATH={path}")),
        crate::daemon::cli::OPEN_FILES,
    )
}

/// Register and start the owner daemon for this user. Explicit owner action
/// only; nothing in the daemon installs itself.
fn stable_daemon_executable(exe: &Path, user_home: &Path) -> bool {
    [PathBuf::from("/Applications"), user_home.join("Applications"),
     PathBuf::from("/usr/local/bin"), PathBuf::from("/usr/bin"),
     PathBuf::from("/opt/homebrew/bin"), user_home.join(".local/bin")]
        .iter().any(|root| exe.starts_with(root))
}

pub fn install_daemon(home_dir: &Path) -> Result<String, String> {
    if !home_dir.is_absolute() {
        return Err("--home must be absolute".into());
    }
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("this binary's path: {e}"))?;
    if !stable_daemon_executable(&exe, &home()?) {
        return Err("daemon_install_location_unsupported: Move the app to /Applications or ~/Applications before enabling login hosting. For a command-line build, install it in ~/.local/bin or /usr/local/bin first; temporary, disk-image and development paths cannot be used for a login service.".into());
    }
    let path = std::env::var("PATH").unwrap_or_default();
    #[cfg(not(all(feature = "test-support", feature = "resource-adoption")))]
    let installation: Option<PathBuf> = None;
    #[cfg(all(feature = "test-support", feature = "resource-adoption"))]
    let installation = crate::native_owner::resource_activation_fixture_root();
    if cfg!(target_os = "macos") {
        let logs = home()?.join("Library/Logs/Hagency");
        std::fs::create_dir_all(&logs).map_err(|e| format!("{}: {e}", logs.display()))?;
        let plist = home()?.join(format!("Library/LaunchAgents/{DAEMON_LABEL}.plist"));
        if let Some(parent) = plist.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let domain = format!("gui/{}", uid());
        let label = format!("{domain}/{DAEMON_LABEL}");
        let _ = launchctl(&["bootout", &label]);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let registered=launchctl(&["print", &label])?.status.success();
            let drained=match crate::daemon::DaemonHome::open(home_dir) {
                Ok(held)=>{drop(held);true},
                Err(crate::daemon::HomeError::AlreadyRunning)=>false,
                Err(error)=>return Err(error.to_string()),
            };
            if !registered && drained { break; }
            if Instant::now() >= deadline {
                return Err("daemon_service_stop_pending: The previous hosting service is still stopping. Its saved installation was kept; retry after it has drained.".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        hagency_store::private::replace(&plist,
            daemon_launchd_plist_with_installation(&exe, home_dir, &path, &logs.join("owner-daemon.log"), installation.as_deref()).as_bytes())
            .map_err(|e| format!("{}: {e}", plist.display()))?;
        let out = launchctl(&["bootstrap", &domain, &plist.to_string_lossy()])?;
        if !out.status.success() {
            return Err(format!(
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    } else {
        let config = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => home()?.join(".config"),
        };
        let unit = config.join("systemd/user/hagency-owner-daemon.service");
        if let Some(parent) = unit.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        std::fs::write(&unit, daemon_systemd_unit_with_installation(&exe, home_dir, &path, installation.as_deref()))
            .map_err(|e| format!("{}: {e}", unit.display()))?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", "hagency-owner-daemon.service"])?;
        systemctl(&["restart", "hagency-owner-daemon.service"])?;
    }
    Ok(format!("Hagency owner daemon installed (home: {}).", home_dir.display()))
}

/// Remove the daemon service. Its home, enrollment and owner root are kept.
pub fn uninstall_daemon() -> Result<String, String> {
    if cfg!(target_os = "macos") {
        let plist = home()?.join(format!("Library/LaunchAgents/{DAEMON_LABEL}.plist"));
        let _ = launchctl(&["bootout", &format!("gui/{}/{DAEMON_LABEL}", uid())]);
        if plist.exists() {
            std::fs::remove_file(&plist).map_err(|e| format!("{}: {e}", plist.display()))?;
        }
    } else {
        let _ = systemctl(&["disable", "--now", "hagency-owner-daemon.service"]);
        let config = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir),
            _ => home()?.join(".config"),
        };
        let unit = config.join("systemd/user/hagency-owner-daemon.service");
        if unit.exists() {
            std::fs::remove_file(&unit).map_err(|e| format!("{}: {e}", unit.display()))?;
        }
        let _ = systemctl(&["daemon-reload"]);
    }
    Ok("Hagency owner daemon service removed; its home and enrollment were kept.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_login_install_requires_durable_executable_location() {
        let user=Path::new("/Users/owner");
        for path in ["/Applications/Hagency.app/Contents/MacOS/hagency-desktop","/Users/owner/Applications/Hagency.app/Contents/MacOS/hagency-desktop","/Users/owner/.local/bin/hagency"] { assert!(stable_daemon_executable(Path::new(path),user)); }
        for path in ["/Volumes/Hagency/Hagency.app/Contents/MacOS/hagency-desktop","/private/var/folders/x/AppTranslocation/Hagency.app/hagency","/Users/owner/dev/target/release/hagency"] { assert!(!stable_daemon_executable(Path::new(path),user)); }
    }
    #[test]
    fn daemon_service_runs_daemon_with_its_own_label_and_escaped_home() {
        let plist = daemon_launchd_plist(
            Path::new("/opt/h & co/hagency"),
            Path::new("/Users/a/Library/Application Support/Hagency/owner-daemon"),
            "/usr/bin:/bin",
            Path::new("/Users/a/Library/Logs/Hagency/owner-daemon.log"),
        );
        assert!(plist.contains(&format!("<string>{DAEMON_LABEL}</string>")));
        assert!(!plist.contains("<string>io.hagency</string>"));
        assert!(plist.contains("<string>/opt/h &amp; co/hagency</string>"));
        assert!(plist.contains("<string>daemon</string>\n    <string>run</string>"));
        assert!(plist.contains("<string>/Users/a/Library/Application Support/Hagency/owner-daemon</string>"));
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        // A Background job is starved on a busy computer and its runs expire.
        assert!(plist.contains("<key>ProcessType</key>\n  <string>Standard</string>"));
        assert!(!plist.contains("Background"));
        // launchd's default of 256 open files ran the daemon out within the hour.
        assert!(plist.contains("<key>NumberOfFiles</key>\n    <integer>10240</integer>"));
        let unit = daemon_systemd_unit(
            Path::new("/opt/hagency/hagency"),
            Path::new("/home/a/.local/state/hagency/owner-daemon"),
            "/usr/bin",
        );
        assert!(unit.contains(
            "ExecStart=\"/opt/hagency/hagency\" daemon run --home \"/home/a/.local/state/hagency/owner-daemon\""
        ));
        assert!(unit.contains("LimitNOFILE=10240\n"));
        let dir = default_daemon_dir().unwrap();
        assert!(dir.is_absolute());
    }

    #[test]
    fn the_daemon_home_is_one_canonical_location_per_user() {
        let user = Path::new("/Users/a");
        let canonical = Path::new("/Users/a/Library/Application Support/Hagency/owner-daemon");
        assert_eq!(daemon_dir(user, None, true), canonical);
        assert_eq!(daemon_dir(user, Some("/x/state".into()), true), canonical);
        let linux = Path::new("/home/a");
        let state = Path::new("/home/a/.local/state/hagency/owner-daemon");
        assert_eq!(
            daemon_dir(linux, Some("/x/state".into()), false),
            Path::new("/x/state/hagency/owner-daemon")
        );
        assert_eq!(daemon_dir(linux, Some("".into()), false), state);
        assert_eq!(daemon_dir(linux, None, false), state);
    }

    #[test]
    fn isolated_daemon_service_preserves_the_explicit_installation_on_restart() {
        let exe = Path::new("/Applications/Hagency.app/Contents/MacOS/hagency-desktop");
        let home = Path::new("/Users/a/Library/Application Support/Hagency/owner-daemon");
        let root = Path::new("/Users/a/isolated & original");
        let args = daemon_arguments(exe, home, Some(root));
        assert_eq!(&args[1..3], &["--hagency-isolated-installation-root", "/Users/a/isolated & original"]);
        assert_eq!(&args[3..], &["daemon", "run", "--home", home.to_str().unwrap()]);
        let plist = daemon_launchd_plist_with_installation(exe, home, "/usr/bin", Path::new("/tmp/daemon.log"), Some(root));
        assert!(plist.contains("<string>--hagency-isolated-installation-root</string>\n    <string>/Users/a/isolated &amp; original</string>"));
        let unit = daemon_systemd_unit_with_installation(exe, home, "/usr/bin", Some(root));
        assert!(unit.contains("--hagency-isolated-installation-root \"/Users/a/isolated & original\" daemon run"));
        assert!(!daemon_launchd_plist(exe, home, "/usr/bin", Path::new("/tmp/daemon.log")).contains("isolated-installation"));
        assert!(!daemon_systemd_unit(exe, home, "/usr/bin").contains("isolated-installation"));
    }
}
