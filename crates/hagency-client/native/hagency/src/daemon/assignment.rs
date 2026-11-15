//! Who executes an owner on this computer (ADR-192). Only durable state
//! decides: socket liveness never grants custody, and on macOS a missing or
//! different enrollment means "set up hosting", never a local fallback.
use super::enrollment::Enrollment;
use crate::owner_roots::OwnerRoots;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Assignment {
    /// Execute in process at the owner's registered root. Only on platforms
    /// that do not ship the daemon yet; never on macOS.
    Local,
    /// The daemon in `home` hosts the owner's registered `root`. Attach to its
    /// control socket, whether or not the daemon is running right now.
    Daemon { home: PathBuf, root: PathBuf },
    /// macOS: hosting must be set up for this owner before anything executes.
    SetupRequired { reason: SetupReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupReason {
    /// The daemon has no completed enrollment (none, or sign-in unfinished).
    NotEnrolled,
    /// The daemon hosts another account. One OS user hosts one owner at a time.
    OtherOwner,
    /// The daemon's enrollment names a root that is not this owner's
    /// registered root (a journal from before ADR-192, or no registration).
    /// Enrolling again records the registered root.
    StaleRoot,
}

impl SetupReason {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotEnrolled => "daemon_setup_required",
            Self::OtherOwner => "daemon_other_owner_enrolled",
            Self::StaleRoot => "daemon_owner_root_unregistered",
        }
    }
}

/// Resolve symlink aliases through the nearest existing parent, without
/// creating an owner root merely to inspect its assignment.
pub(super) fn identity(path: &Path) -> Result<PathBuf, &'static str> {
    crate::owner_roots::real_path(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::InvalidInput => "daemon_owner_root_invalid",
        _ => "daemon_owner_root_unavailable",
    })
}

/// Who executes `owner` on `origin`, with the machine's owner-root registry.
/// Read-only: it never registers a root or changes the enrollment.
pub fn resolve(home: &Path, origin: &str, owner: &str) -> Result<Assignment, &'static str> {
    resolve_with(
        &OwnerRoots::system().map_err(|e| e.code())?,
        home,
        origin,
        owner,
    )
}

/// [`resolve`] with an explicit registry.
pub fn resolve_with(
    roots: &OwnerRoots,
    home: &Path,
    origin: &str,
    owner: &str,
) -> Result<Assignment, &'static str> {
    decide(roots, home, origin, owner, cfg!(target_os = "macos") && !demo_in_process())
}

/// DEMO ONLY (`demo` feature plus `HAGENCY_DEMO_IN_PROCESS_OWNER=1`): macOS executes the owner in
/// this process like the other platforms, so one-click Agents run without the background daemon and
/// its browser sign-in. A product build never reads the variable.
pub const DEMO_IN_PROCESS_ENV: &str = "HAGENCY_DEMO_IN_PROCESS_OWNER";
#[cfg(feature = "demo")]
fn demo_in_process() -> bool {
    std::env::var(DEMO_IN_PROCESS_ENV).is_ok_and(|v| v == "1")
}
#[cfg(not(feature = "demo"))]
fn demo_in_process() -> bool {
    false
}


fn decide(
    roots: &OwnerRoots,
    home: &Path,
    origin: &str,
    owner: &str,
    daemon_only: bool,
) -> Result<Assignment, &'static str> {
    let otherwise = |reason| {
        Ok(if daemon_only {
            Assignment::SetupRequired { reason }
        } else {
            Assignment::Local
        })
    };
    let Some(enrolled) = Enrollment::load(home).map_err(|e| e.code())? else {
        return otherwise(SetupReason::NotEnrolled);
    };
    if enrolled.owner != owner || !super::same_origin(&enrolled.origin, origin) {
        return otherwise(SetupReason::OtherOwner);
    }
    if !enrolled.hostable() {
        return otherwise(SetupReason::NotEnrolled);
    }
    let Some(registered) = roots.lookup(origin, owner).map_err(|e| e.code())? else {
        return otherwise(SetupReason::StaleRoot);
    };
    let root = identity(&enrolled.owner_root)?;
    if identity(&registered)? != root {
        return otherwise(SetupReason::StaleRoot);
    }
    Ok(Assignment::Daemon {
        home: home.into(),
        root,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    struct Fixture {
        temp: tempfile::TempDir,
        home: PathBuf,
        roots: OwnerRoots,
    }

    fn fixture() -> Fixture {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("daemon");
        hagency_store::private::directory(&home).unwrap();
        let roots = OwnerRoots::at(temp.path().join("resources")).unwrap();
        Fixture { temp, home, roots }
    }

    impl Fixture {
        fn decide(&self, origin: &str, owner: &str, daemon_only: bool) -> Assignment {
            decide(&self.roots, &self.home, origin, owner, daemon_only).unwrap()
        }
    }

    fn setup(reason: SetupReason) -> Assignment {
        Assignment::SetupRequired { reason }
    }

    #[test]
    fn macos_never_falls_back_to_local_and_never_reports_a_conflict() {
        let f = fixture();
        let desktop = f.temp.path().join("desktop-root");
        crate::owner_host::initialize(&desktop).unwrap();
        let registered = f
            .roots
            .resolve("https://s/", "@o:s", Some(&desktop))
            .unwrap();
        assert_eq!(
            f.decide("https://s/", "@o:s", true),
            setup(SetupReason::NotEnrolled)
        );
        Enrollment::prepare(&f.home, "https://s/", "@o:s", None, registered.clone()).unwrap();
        assert_eq!(
            f.decide("https://s/", "@o:s", true),
            setup(SetupReason::NotEnrolled)
        );
        // An enrollment from before ADR-192 names the daemon's own root.
        let legacy = f.home.join("owners").join("legacy");
        Enrollment::enroll(&f.home, "https://s/", "@o:s", None, &legacy).unwrap();
        assert_eq!(
            f.decide("https://s/", "@o:s", true),
            setup(SetupReason::StaleRoot)
        );
        assert_eq!(
            f.decide("https://s/", "@other:s", true),
            setup(SetupReason::OtherOwner)
        );
        assert_eq!(
            f.decide("https://t/", "@o:s", true),
            setup(SetupReason::OtherOwner)
        );
        // Enrollment of the registered root assigns the daemon, without a socket.
        Enrollment::enroll(&f.home, "https://s/", "@o:s", None, &registered).unwrap();
        for origin in ["https://s", "https://s/", "HTTPS://S/"] {
            assert_eq!(
                f.decide(origin, "@o:s", true),
                Assignment::Daemon {
                    home: f.home.clone(),
                    root: registered.clone()
                },
                "{origin}"
            );
        }
        assert!(!f.home.join("control.sock").exists());
        assert_eq!(
            f.decide("https://s/", "@other:s", true),
            setup(SetupReason::OtherOwner)
        );
        Enrollment::withdraw(&f.home).unwrap();
        assert_eq!(
            f.decide("https://s/", "@o:s", true),
            setup(SetupReason::NotEnrolled)
        );
    }

    #[test]
    fn an_owner_without_a_registration_needs_setup_even_when_enrolled() {
        let f = fixture();
        let root = f.temp.path().join("unregistered-root");
        Enrollment::enroll(&f.home, "https://s/", "@o:s", None, &root).unwrap();
        assert_eq!(
            f.decide("https://s/", "@o:s", true),
            setup(SetupReason::StaleRoot)
        );
        assert_eq!(f.decide("https://s/", "@o:s", false), Assignment::Local);
    }

    #[test]
    fn other_platforms_keep_local_execution_unless_the_daemon_hosts_the_registered_root() {
        let f = fixture();
        let registered = f.roots.resolve("https://s/", "@o:s", None).unwrap();
        assert_eq!(f.decide("https://s/", "@o:s", false), Assignment::Local);
        Enrollment::enroll(
            &f.home,
            "https://s/",
            "@other:s",
            None,
            &f.temp.path().join("other"),
        )
        .unwrap();
        assert_eq!(f.decide("https://s/", "@o:s", false), Assignment::Local);
        Enrollment::withdraw(&f.home).unwrap();
        Enrollment::enroll(&f.home, "https://s/", "@o:s", None, &registered).unwrap();
        assert!(matches!(
            f.decide("https://s/", "@o:s", false),
            Assignment::Daemon { .. }
        ));
    }

    #[test]
    fn an_aliased_enrollment_root_is_the_registered_root() {
        let f = fixture();
        let desktop = f.temp.path().join("desktop-root");
        crate::owner_host::initialize(&desktop).unwrap();
        let registered = f
            .roots
            .resolve("https://s/", "@o:s", Some(&desktop))
            .unwrap();
        let alias = f.temp.path().join("alias");
        std::os::unix::fs::symlink(&registered, &alias).unwrap();
        // The journal names the root through an alias; the identity is the same.
        Enrollment::enroll(&f.home, "https://s/", "@o:s", None, &alias).unwrap();
        assert_eq!(
            f.decide("https://s/", "@o:s", true),
            Assignment::Daemon {
                home: f.home.clone(),
                root: registered
            }
        );
    }

    #[test]
    fn the_running_platform_selects_setup_or_local() {
        let f = fixture();
        let expected = if cfg!(target_os = "macos") {
            setup(SetupReason::NotEnrolled)
        } else {
            Assignment::Local
        };
        assert_eq!(
            resolve_with(&f.roots, &f.home, "https://s/", "@o:s").unwrap(),
            expected
        );
    }
}

#[cfg(all(test, not(feature = "demo")))]
mod demo_tests {
    /// A product build never lets the environment move macOS execution into this process.
    #[test]
    fn product_build_ignores_the_in_process_override() {
        assert!(!super::demo_in_process());
    }
}
