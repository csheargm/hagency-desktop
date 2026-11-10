//! Process-level entry points: run the daemon, enroll an owner, probe a
//! running daemon and sign out. Used by the `daemon` command line and by
//! Hagency Desktop's hosting integration.
pub use super::enrollment_rpc::EnrollmentControl;
use super::{
    DaemonHome, HomeError,
    auth::{AuthError, SdkCustody},
    control::ControlStore,
    dispatch::{Controller, Runtime},
    enrollment::{Enrollment, EnrollmentError},
    ipc::{DaemonClient, IpcError, Server, new_request_id},
    protocol::{Command, Hello, Request},
    runtime::OwnerRuntime,
};
use crate::{native_owner::NativeOwner, owner_roots::OwnerRoots};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error(transparent)]
    Home(#[from] HomeError),
    #[error("{0}")]
    Code(&'static str),
    #[error(transparent)]
    Ipc(#[from] IpcError),
}

impl From<EnrollmentError> for HostError {
    fn from(e: EnrollmentError) -> Self {
        Self::Code(e.code())
    }
}
impl From<AuthError> for HostError {
    fn from(e: AuthError) -> Self {
        Self::Code(e.code())
    }
}

/// `~/Library/Application Support/Hagency/owner-daemon` on macOS, else
/// `$XDG_STATE_HOME/hagency/owner-daemon` or `~/.local/state/hagency/owner-daemon`.
/// See [`crate::service::default_daemon_dir`].
pub fn default_home() -> Result<PathBuf, HostError> {
    crate::service::default_daemon_dir().map_err(|_| HostError::Code("daemon_home_unavailable"))
}

/// Run until `shutdown` fires. Holds the daemon home lock for the whole
/// process; a second instance fails with `AlreadyRunning`. On shutdown the
/// hosting intent is preserved and the owner is suspended, not signed out.
pub async fn run(
    home: &Path,
    shutdown: hagency_matrix::CancellationToken,
) -> Result<(), HostError> {
    let home = DaemonHome::open(home)?;
    // Under the home lock and before either file is read: what an earlier
    // Hagency version left here is discarded, not migrated.
    if ControlStore::discard_imported_json(&home.control()) {
        tracing::warn!("Removed the control file an earlier Hagency version left behind");
    }
    if Enrollment::discard_earlier_format(home.path()) {
        tracing::warn!("Removed the enrollment an earlier Hagency version wrote; hosting must be set up again");
    }
    let store = ControlStore::open(&home.control()).map_err(|e| HostError::Code(e.code))?;
    let runtime = Arc::new(OwnerRuntime::new(home.path().into()).map_err(HostError::Code)?);
    let controller = Controller::new(store, runtime);
    let mut server = Server::start(home.path(), controller.clone())?;
    if super::revocation::load(home.path()).map_err(HostError::Code)?.is_none() {
        controller.boot().await;
    }
    tracing::info!("Hagency owner daemon is serving its private control socket");
    let observer = {
        let controller = controller.clone();
        tokio::spawn(async move {
            loop {
                let _ = controller.runtime().process_revocation().await;
                controller.observe_transition();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        })
    };
    shutdown.cancelled().await;
    observer.abort();
    controller.begin_shutdown();
    // Keep the private home lock, original task/native handles, and read-only
    // status socket while a graceful drain needs recovery. A failed drain is
    // never permission to release custody to another process.
    loop {
        let result=match controller.runtime().drain_enrollment(false).await {
            Ok(())=>controller.shutdown().await,
            Err(code)=>Err(code),
        };
        match result {
            Ok(())=>break,
            Err(code)=>{
                tracing::warn!(code,"Daemon shutdown is waiting for the original drain");
                controller.observe_transition();
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    server.stop().await;
    drop(home);
    Ok(())
}

/// Result of a probe. Desktop must not start its own executor for an owner
/// root while a daemon reports that owner.
#[derive(Debug)]
pub enum Presence {
    NotRunning,
    Running(Hello),
    /// The socket exists but is not private or the peer is another user.
    Unsafe,
}

pub async fn probe(home: &Path) -> Presence {
    let mut client = match DaemonClient::connect(home).await {
        Ok(client) => client,
        Err(IpcError::Unsafe | IpcError::PeerRejected) => return Presence::Unsafe,
        Err(_) => return Presence::NotRunning,
    };
    let request = Request::new(new_request_id(), None, Command::Hello);
    match client
        .call(&request)
        .await
        .ok()
        .and_then(|reply| reply.hello().ok())
    {
        Some(hello) => Presence::Running(hello),
        None => Presence::NotRunning,
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnrollReport {
    pub owner: String,
    pub owner_root: PathBuf,
    /// The owner root was opened and authorized during enrollment, which
    /// completes native device registration under the daemon's SDK client.
    pub registered: bool,
    /// Static code when registration must be retried later.
    pub pending: Option<String>,
    /// The root already held an active executor, which the daemon proves in
    /// place; otherwise the daemon registers a new executor device.
    #[serde(default)]
    pub existing_executor: bool,
}

/// What enrollment does with the owner's registered root, decided only from
/// the root's contents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Executor {
    /// An active executor resume record: sign in with the root's OAuth client
    /// and prove that device in place with the existing adoption checks.
    InPlace { client_id: String },
    /// No active executor (no root yet, no record, or a revoked one): sign in
    /// with the daemon's own OAuth client and register a new executor device.
    Fresh,
}

impl Executor {
    /// `origin` in the native form (see [`native_origin`]).
    pub fn inspect(root: &Path, origin: &str, owner: &str) -> Result<Self, &'static str> {
        match std::fs::symlink_metadata(root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::Fresh),
            Err(_) => return Err("owner_root_busy_or_invalid"),
            Ok(_) => {}
        }
        match NativeOwner::matrix_resume_client_id(root, origin, owner) {
            Ok(client_id) => Ok(Self::InPlace { client_id }),
            Err(e)
                if matches!(
                    e.code.as_str(),
                    "native_resume_missing" | "native_resume_revoked"
                ) =>
            {
                Ok(Self::Fresh)
            }
            // A record of another account, or one bound to another path.
            Err(e) if e.code == "matrix_account_mismatch" => Err("matrix_account_mismatch"),
            Err(_) => Err("native_resume_unavailable"),
        }
    }
    fn client_id(&self) -> Option<&str> {
        match self {
            Self::InPlace { client_id } => Some(client_id),
            Self::Fresh => None,
        }
    }
}

/// The URL form native owner roots record (`https://host/`). Root contents
/// such as the resume record compare origins exactly.
pub fn native_origin(origin: &str) -> Result<String, HostError> {
    crate::server_admission::normalize_origin(origin)
        .map(|url| url.to_string())
        .map_err(|_| AuthError::UnsupportedServer.into())
}

/// Explicit enrollment of `owner`. The daemon records the owner's registered
/// root ([`crate::owner_roots`]), never a different one, and signs in through
/// the browser with its own SDK device, reusing the root's OAuth client when
/// the root holds an active executor. It then opens the root in-process, so
/// the one-time sign-in permission is consumed by native device proof or
/// registration immediately. A running daemon on `home` performs the same
/// steps under its own custody.
pub async fn enroll<F, Fut>(
    home: &Path,
    origin: &str,
    owner: &str,
    display_name: Option<String>,
    open_url: F,
) -> Result<EnrollReport, HostError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), ()>>,
{
    enroll_controlled(
        home,
        origin,
        owner,
        display_name,
        Arc::new(EnrollmentControl::default()),
        open_url,
    )
    .await
}

pub async fn enroll_controlled<F, Fut>(
    home: &Path,
    origin: &str,
    owner: &str,
    display_name: Option<String>,
    control: Arc<EnrollmentControl>,
    open_url: F,
) -> Result<EnrollReport, HostError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), ()>>,
{
    match probe(home).await {
        Presence::Running(_) => {
            return tokio::select! {
                result=super::enrollment_rpc::remote_enroll(home,origin,owner,display_name,open_url)=>result,
                _=control.cancelled()=>Err(HostError::Code("daemon_enrollment_detached")),
            };
        }
        Presence::Unsafe => return Err(HostError::Code("daemon_socket_unsafe")),
        Presence::NotRunning => {}
    }
    let held = DaemonHome::open(home)?;
    let roots = OwnerRoots::system().map_err(|e| HostError::Code(e.code()))?;
    enroll_locked_controlled(
        held.path(),
        &roots,
        origin,
        owner,
        display_name,
        control,
        open_url,
    )
    .await
}

pub(super) async fn enroll_locked_controlled<F, Fut>(
    home: &Path,
    roots: &OwnerRoots,
    origin: &str,
    owner: &str,
    display_name: Option<String>,
    control: Arc<EnrollmentControl>,
    open_url: F,
) -> Result<EnrollReport, HostError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), ()>>,
{
    if !control.accepting() {
        return Err(HostError::Code("daemon_enrollment_cancelled"));
    }
    if super::revocation::load(home)
        .map_err(HostError::Code)?
        .is_some()
    {
        return Err(HostError::Code("daemon_logout_pending"));
    }
    let origin = native_origin(origin)?;
    if Enrollment::load(home)?.is_some_and(|existing| {
        existing.owner != owner || !super::same_origin(&existing.origin, &origin)
    }) {
        return Err(EnrollmentError::OtherOwner.into());
    }
    // Only Desktop (or an explicit `--register-root`) registers a root: it
    // knows the account's existing root, so the daemon can never claim the
    // account first with an empty one (ADR-192).
    let root = roots
        .lookup(&origin, owner)
        .map_err(|e| HostError::Code(e.code()))?
        .ok_or(HostError::Code(crate::owner_roots::RootError::NotRegistered.code()))?;
    let executor = Executor::inspect(&root, &origin, owner).map_err(HostError::Code)?;
    Enrollment::prepare(home, &origin, owner, display_name.clone(), root.clone())?;
    let directory = home.join(super::MATRIX);
    let custody = tokio::select! {
        biased;
        _=control.cancelled()=>return Err(HostError::Code("daemon_enrollment_cancelled")),
        result=SdkCustody::reauthenticate(&directory,&origin,Some(owner),executor.client_id(),open_url)=>result?,
    };
    custody.fence_on_logout(home);
    // Claim native custody before publishing a hostable enrollment. Browser
    // cancellation can never land an enrolled root between these steps.
    control.accept_native().map_err(HostError::Code)?;
    control.set_custody(custody.clone());
    let enrollment = Enrollment::enroll(home, &origin, owner, display_name, &root)?;
    if super::revocation::load(home)
        .map_err(HostError::Code)?
        .is_some()
    {
        custody.retire();
        return Err(HostError::Code("daemon_logout_pending"));
    }
    let existing_executor = matches!(executor, Executor::InPlace { .. });
    let opened = open_for_registration(&enrollment, &custody, control.clone()).await;
    let report = match opened {
        Ok(native) => {
            if super::revocation::load(home)
                .map_err(HostError::Code)?
                .is_some()
            {
                control.drain(true).await.map_err(HostError::Code)?;
                return Err(HostError::Code("daemon_logout_pending"));
            }
            let _ = native.identity().await;
            control.drain(false).await.map_err(HostError::Code)?;
            EnrollReport {
                owner: owner.into(),
                owner_root: enrollment.owner_root,
                registered: true,
                pending: None,
                existing_executor,
            }
        }
        Err(code) => EnrollReport {
            owner: owner.into(),
            owner_root: enrollment.owner_root,
            registered: false,
            pending: Some(code.into()),
            existing_executor,
        },
    };
    control.drain(false).await.map_err(HostError::Code)?;
    custody.retire();
    Ok(report)
}

async fn open_for_registration(
    enrollment: &Enrollment,
    custody: &Arc<SdkCustody>,
    control: Arc<EnrollmentControl>,
) -> Result<NativeOwner, &'static str> {
    let root = &enrollment.owner_root;
    #[cfg(feature = "resource-adoption")]
    {
        use crate::native_owner::CurrentProfileSelection;
        let selection = if root.exists() {
            CurrentProfileSelection::load_for_host(root, &enrollment.origin, &enrollment.owner)
                .await
                .map_err(|_| "current_profile_initializer_unavailable")?
        } else {
            None
        };
        if let Some(selection) = selection {
            return selection
                .open_with_matrix_guarded(
                    root,
                    &enrollment.origin,
                    &enrollment.owner,
                    custody.tokens(),
                    move |owner| {
                        control.register(owner);
                        Ok(())
                    },
                )
                .await
                .map_err(|e| registration_code(&e.code));
        }
    }
    NativeOwner::open_with_matrix_guarded(
        root,
        &enrollment.origin,
        &enrollment.owner,
        custody.tokens(),
        move |owner| {
            control.register(owner);
            Ok(())
        },
    )
    .await
    .map_err(|e| registration_code(&e.code))
}

fn registration_code(code: &str) -> &'static str {
    match code {
        "native_owner_state_unavailable" => "owner_root_busy_or_invalid",
        "matrix_account_mismatch" => "matrix_account_mismatch",
        "native_resume_missing" => "native_resume_missing",
        "native_resume_revoked" => "native_resume_revoked",
        "native_device_registration_unknown" => "native_device_registration_unknown",
        "current_profile_scope_changed" => "current_profile_scope_changed",
        "server_unavailable" | "matrix_authorization_unavailable" => "server_unavailable",
        _ => "native_registration_failed",
    }
}

/// Explicit owner sign-out. Durable native revocation precedes the drain; the
/// SDK grant is revoked last. The owner root and its registration remain, so
/// enrolling again finds the same root.
pub async fn sign_out(home: &Path) -> Result<(), HostError> {
    if let Presence::Running(_) = probe(home).await {
        let enrollment = Enrollment::load(home)?.ok_or(EnrollmentError::Missing)?;
        super::revocation::request(home, &enrollment.origin, &enrollment.owner)
            .map_err(HostError::Code)?;
        let client = super::bridge::ControllerClient::new(
            home.into(),
            enrollment.owner,
            enrollment.origin,
            1,
        );
        let result = client
            .sign_out()
            .await
            .map_err(|_| HostError::Code("daemon_logout_pending"));
        client.retire();
        return result;
    }
    let home = DaemonHome::open(home)?;
    let runtime = OwnerRuntime::new(home.path().into()).map_err(HostError::Code)?;
    if runtime.owner().is_none() {
        return Err(EnrollmentError::Missing.into());
    }
    runtime.sign_out().await.map_err(HostError::Code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn short_home() -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix("hh")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[tokio::test]
    async fn pending_remote_logout_still_serves_status_and_never_opens_hosting() {
        let temp = short_home();
        let home = temp.path().join("d");
        let root = temp.path().join("owner-root");
        hagency_store::private::directory(&home).unwrap();
        Enrollment::enroll(
            &home,
            "https://unavailable.invalid/",
            "@owner:unavailable.invalid",
            None,
            &root,
        )
        .unwrap();
        hagency_store::private::directory(&home.join(super::super::MATRIX)).unwrap();
        hagency_store::private::replace(
            &home.join(super::super::MATRIX).join("session.json"),
            b"{}",
        )
        .unwrap();
        super::super::revocation::request(
            &home,
            "https://unavailable.invalid",
            "@owner:unavailable.invalid",
        )
        .unwrap();
        let shutdown = hagency_matrix::CancellationToken::new();
        let task = {
            let home = home.clone();
            let shutdown = shutdown.clone();
            tokio::spawn(async move { run(&home, shutdown).await })
        };
        let hello = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Presence::Running(hello) = probe(&home).await {
                    break hello;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_ne!(
            hello.hosting,
            hagency_metering::dashboard::HostingState::Running
        );
        assert!(super::super::revocation::load(&home).unwrap().is_some());
        assert!(!root.exists());
        shutdown.cancel();
        task.await.unwrap().unwrap();
    }
    #[tokio::test]
    async fn run_serves_an_unenrolled_daemon_and_detaching_controllers_change_nothing() {
        let temp = short_home();
        let home = temp.path().join("d");
        let shutdown = hagency_matrix::CancellationToken::new();
        let task = {
            let (home, shutdown) = (home.clone(), shutdown.clone());
            tokio::spawn(async move { run(&home, shutdown).await })
        };
        let mut hello = None;
        for _ in 0..200 {
            if let Presence::Running(h) = probe(&home).await {
                hello = Some(h);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let hello = hello.expect("daemon never answered");
        assert_eq!(hello.owner_id, None);
        assert_eq!(
            hello.hosting,
            hagency_metering::dashboard::HostingState::Unconfigured
        );
        // A second daemon on the same home is refused while the first runs.
        assert!(matches!(
            run(&home, hagency_matrix::CancellationToken::new()).await,
            Err(HostError::Home(HomeError::AlreadyRunning))
        ));
        // Local enrollment and sign-out also require the home lock.
        assert!(matches!(
            DaemonHome::open(&home),
            Err(HomeError::AlreadyRunning)
        ));
        // Controllers attach and detach freely.
        for _ in 0..3 {
            let mut client = DaemonClient::connect(&home).await.unwrap();
            let reply = client
                .call(&Request::new(
                    new_request_id(),
                    None,
                    Command::Snapshot {
                        period: None,
                        usage_filter: None,
                    },
                ))
                .await
                .unwrap();
            assert_eq!(
                reply.snapshot().unwrap().hosting.state,
                hagency_metering::dashboard::HostingState::Unconfigured
            );
        }
        assert!(matches!(probe(&home).await, Presence::Running(_)));
        shutdown.cancel();
        task.await.unwrap().unwrap();
        assert!(matches!(probe(&home).await, Presence::NotRunning));
        assert!(!home.join(super::super::ipc::SOCKET).exists());
    }

    const OWNER: &str = "@owner:test";

    struct Enrolling {
        temp: tempfile::TempDir,
        roots: OwnerRoots,
        home: DaemonHome,
    }

    fn enrolling() -> Enrolling {
        let temp = short_home();
        let roots = OwnerRoots::at(temp.path().join("resources")).unwrap();
        let home = DaemonHome::open(&temp.path().join("d")).unwrap();
        Enrolling { temp, roots, home }
    }

    impl Enrolling {
        /// A Desktop root, optionally holding an executor resume record.
        fn desktop_root(&self, origin: &str, active: Option<bool>) -> PathBuf {
            let root =
                crate::owner_host::initialize(&self.temp.path().join("desktop-root")).unwrap();
            if let Some(active) = active {
                NativeOwner::write_test_resume_record(
                    &root,
                    origin,
                    OWNER,
                    "desktop-client",
                    active,
                );
            }
            root
        }
        /// Enroll until the browser would open; the owner closes it there.
        async fn until_browser(
            &self,
            origin: &str,
        ) -> (Result<EnrollReport, HostError>, Option<reqwest::Url>) {
            let opened = Arc::new(std::sync::Mutex::new(None));
            let seen = opened.clone();
            let result = enroll_locked_controlled(
                self.home.path(),
                &self.roots,
                origin,
                OWNER,
                None,
                Arc::new(EnrollmentControl::default()),
                move |url| async move {
                    *seen.lock().unwrap() = reqwest::Url::parse(&url).ok();
                    Err(())
                },
            )
            .await;
            let url = opened.lock().unwrap().take();
            (result, url)
        }
    }

    #[test]
    fn only_an_active_resume_record_is_proved_in_place() {
        let e = enrolling();
        let origin = "https://s.example/";
        let missing = e.temp.path().join("never-created");
        assert_eq!(
            Executor::inspect(&missing, origin, OWNER),
            Ok(Executor::Fresh)
        );
        let root = e.desktop_root(origin, None);
        assert_eq!(Executor::inspect(&root, origin, OWNER), Ok(Executor::Fresh));
        NativeOwner::write_test_resume_record(&root, origin, OWNER, "desktop-client", true);
        let in_place = Ok(Executor::InPlace {
            client_id: "desktop-client".into(),
        });
        assert_eq!(Executor::inspect(&root, origin, OWNER), in_place);
        // Enrollment passes the native origin form, whatever the caller typed.
        assert_eq!(
            Executor::inspect(&root, &native_origin("HTTPS://S.example").unwrap(), OWNER),
            in_place
        );
        assert_eq!(
            Executor::inspect(&root, origin, "@other:s.example"),
            Err("matrix_account_mismatch")
        );
        NativeOwner::write_test_resume_record(&root, origin, OWNER, "desktop-client", false);
        assert_eq!(Executor::inspect(&root, origin, OWNER), Ok(Executor::Fresh));
        // A record bound to another path is never proved here.
        let copy = e.temp.path().join("copied-root");
        crate::owner_host::initialize(&copy).unwrap();
        std::fs::copy(
            root.join("server-login-sdk-resume.json"),
            copy.join("server-login-sdk-resume.json"),
        )
        .unwrap();
        assert_eq!(
            Executor::inspect(&copy, origin, OWNER),
            Err("native_resume_unavailable")
        );
    }

    #[tokio::test]
    async fn enrollment_proves_an_active_root_in_place_with_its_own_oauth_client() {
        let fixture = super::super::auth::readiness_tests::Fixture::new(0).await;
        let e = enrolling();
        let desktop = e.desktop_root(&fixture.origin, Some(true));
        assert_eq!(
            e.roots
                .resolve(&fixture.origin, OWNER, Some(&desktop))
                .unwrap(),
            desktop
        );
        let (result, url) = e.until_browser(fixture.origin.trim_end_matches('/')).await;
        assert!(
            matches!(result, Err(HostError::Code("daemon_sign_in_cancelled"))),
            "{result:?}"
        );
        let url = url.expect("the browser step was reached");
        assert!(
            url.query_pairs()
                .any(|(k, v)| k == "client_id" && v == "desktop-client"),
            "{url}"
        );
        let prepared = Enrollment::load(e.home.path()).unwrap().unwrap();
        assert_eq!(
            (prepared.owner_root.clone(), prepared.hostable()),
            (desktop.clone(), false)
        );
        assert_eq!(
            prepared.origin, fixture.origin,
            "the journal keeps the native origin form"
        );
        assert_eq!(
            e.roots.lookup(&fixture.origin, OWNER).unwrap(),
            Some(desktop)
        );
    }

    #[tokio::test]
    async fn enrollment_requires_a_registered_root_and_never_creates_another() {
        let fixture = super::super::auth::readiness_tests::Fixture::new(0).await;
        let e = enrolling();
        // The daemon never claims an account first: without a registered root
        // it refuses before any sign-in and records nothing.
        let (unregistered, url) = e.until_browser(&fixture.origin).await;
        assert!(
            matches!(
                unregistered,
                Err(HostError::Code("owner_root_not_registered"))
            ),
            "{unregistered:?}"
        );
        assert!(url.is_none(), "{url:?}");
        assert!(e.roots.lookup(&fixture.origin, OWNER).unwrap().is_none());
        assert!(Enrollment::load(e.home.path()).unwrap().is_none());
        // Desktop, or `daemon enroll --register-root`, registers it first.
        e.roots.resolve(&fixture.origin, OWNER, None).unwrap();
        let (first, url) = e.until_browser(&fixture.origin).await;
        // A fresh executor signs in with the daemon's own OAuth client, which
        // needs dynamic client registration; the fixture server has none.
        assert!(
            matches!(
                first,
                Err(HostError::Code("matrix_authorization_unavailable"))
            ),
            "{first:?}"
        );
        assert!(
            url.as_ref()
                .is_none_or(|url| !url.query_pairs().any(|(k, _)| k == "client_id")),
            "{url:?}"
        );
        let registered = e
            .roots
            .lookup(&fixture.origin, OWNER)
            .unwrap()
            .expect("registered");
        let canonical = e
            .roots
            .base()
            .canonicalize()
            .unwrap()
            .join(crate::owner_roots::OWNERS);
        assert!(registered.starts_with(&canonical), "{registered:?}");
        assert!(
            !registered.exists(),
            "nothing is created before authorization"
        );
        assert_eq!(
            Enrollment::load(e.home.path()).unwrap().unwrap().owner_root,
            registered
        );
        // A Desktop root appearing later does not replace the registered root.
        let _desktop = e.desktop_root(&fixture.origin, Some(true));
        let (_, url) = e.until_browser(&fixture.origin).await;
        assert!(url.is_none_or(|url| {
            !url.query_pairs()
                .any(|(k, v)| k == "client_id" && v == "desktop-client")
        }));
        assert_eq!(
            Enrollment::load(e.home.path()).unwrap().unwrap().owner_root,
            registered
        );
        // Another owner is refused before any sign-in.
        Enrollment::withdraw(e.home.path()).unwrap();
        Enrollment::enroll(
            e.home.path(),
            &fixture.origin,
            "@other:test",
            None,
            &registered,
        )
        .unwrap();
        let (refused, url) = e.until_browser(&fixture.origin).await;
        assert!(
            matches!(refused, Err(HostError::Code("daemon_other_owner_enrolled"))),
            "{refused:?}"
        );
        assert!(url.is_none());
    }

    #[tokio::test]
    async fn sign_out_keeps_the_root_and_its_registration() {
        let e = enrolling();
        let origin = "https://unavailable.invalid/";
        let owner = "@owner:unavailable.invalid";
        let root = crate::owner_host::initialize(&e.temp.path().join("owner-root")).unwrap();
        assert_eq!(e.roots.resolve(origin, owner, Some(&root)).unwrap(), root);
        Enrollment::enroll(e.home.path(), origin, owner, None, &root).unwrap();
        let home = e.home.path().to_path_buf();
        drop(e.home);
        sign_out(&home).await.unwrap();
        assert!(Enrollment::load(&home).unwrap().is_none());
        assert!(super::super::revocation::load(&home).unwrap().is_none());
        assert!(root.is_dir());
        assert_eq!(e.roots.lookup(origin, owner).unwrap(), Some(root));
    }
}
