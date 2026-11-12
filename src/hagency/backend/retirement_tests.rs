use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[cfg(all(unix, feature = "resource-adoption"))]
#[path = "daemon_handoff_tests.rs"]
mod daemon_handoff_tests;

// The real desktop registration/retirement code and real native owner are used,
// without touching the process-wide SDK account, CURRENT or any saved profile.
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "hagency-retirement-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn current_profile_process(name: &str) -> Option<Directory> {
    const CASE: &str = "HAGENCY_RETIREMENT_CURRENT_PROFILE_CASE";
    if std::env::var(CASE).as_deref() != Ok(name) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([name, "--exact", "--nocapture"])
            .env(CASE, name).output().unwrap();
        assert!(output.status.success(), "{} {}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        return None;
    }
    // Select both the canonical registry and writer authority before opening
    // any owner. This child can never fall back to the installed registry.
    let installation = Directory::new();
    ::hagency::native_owner::initialize_resource_activation_fixture(
        &installation.0.join("installation"),
    ).unwrap();
    Some(installation)
}
struct RevokeBarrier {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
struct Fixture {
    _directory: Directory,
    service: Service,
    requests: Arc<AtomicUsize>,
    authorizations: Arc<AtomicUsize>,
    revocations: Arc<AtomicUsize>,
    revoke_barrier: Arc<Mutex<Option<Arc<RevokeBarrier>>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new() -> Self {
        if ::hagency::native_owner::resource_activation_fixture_root().is_none() {
            ::hagency::test_support::initialize_resource_authority().unwrap();
        }
        let directory = Directory::new();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let service = Service(Arc::new(Inner {
            encrypted_approvals: None,
            origin: origin.clone(),
            owner: "@owner:test".into(),
            epoch: 1,
            client_revision: 0,
            explicit_reenrollment: std::sync::atomic::AtomicBool::new(false),
            // A private registry and daemon home: never the user's own.
            locations: crate::hagency::hosting::Locations::at(&directory.0),
            state: Ok(directory.0.join("owner")),
            native: tokio::sync::Mutex::new(None),
            remote: Mutex::new(None),
            native_retirement: Mutex::new(None),
            #[cfg(feature = "resource-adoption")]
            current_initialization: Mutex::new(None),
            #[cfg(feature = "resource-adoption")]
            setup_pending: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "resource-adoption")]
            activation: Default::default(),
            #[cfg(feature = "resource-adoption")]
            allocations: Default::default(),
            #[cfg(feature = "shared-execution")]
            shared: Default::default(),
            retired: std::sync::atomic::AtomicBool::new(false),
            logout_requested: std::sync::atomic::AtomicBool::new(false),
            matrix_checked: tokio::sync::Mutex::new(None),
        }));
        let requests = Arc::new(AtomicUsize::new(0));
        let authorizations = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let auth = authorizations.clone();
        let revocations = Arc::new(AtomicUsize::new(0));
        let revoked = revocations.clone();
        let revoke_barrier = Arc::new(Mutex::new(None::<Arc<RevokeBarrier>>));
        let revocation_wait = revoke_barrier.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let count = count.clone();
                let auth = auth.clone();
                let revoked = revoked.clone();
                let revocation_wait = revocation_wait.clone();
                let origin = origin.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut part = [0_u8; 4096];
                    let split = loop {
                        let n = stream.read(&mut part).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&part[..n]);
                        assert!(raw.len() < 32768);
                        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&raw[..split]).to_string();
                    let len = head
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    while raw.len() < split + len {
                        let n = stream.read(&mut part).await.unwrap();
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&part[..n]);
                        assert!(raw.len() < 32768);
                    }
                    let path = head
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap();
                    count.fetch_add(1, Ordering::AcqRel);
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64;
                    let value = match path {
                        "/hagency/api/v1/discovery" => {
                            json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"],"homeserver":origin,"issuer":format!("{origin}auth/")})
                        }
                        "/hagency/api/v1/sessions/pasion" => {
                            auth.fetch_add(1, Ordering::AcqRel);
                            json!({"token":"a".repeat(64),"userId":"uid","mxid":"@owner:test","validUntilMs":now+30000})
                        }
                        "/hagency/api/v1/identity" => {
                            json!({"userId":"uid","mxid":"@owner:test","issuer":format!("{origin}auth/"),"subject":"subject","clientId":"sdk-client","validUntilMs":now+30000})
                        }
                        "/hagency/api/v1/devices" => {
                            json!({"deviceId":"fixture-device","token":"b".repeat(64),"generation":1,"validUntilMs":now+30000})
                        }
                        "/hagency/api/v1/devices/current" => {
                            json!({"userId":"uid","mxid":"@owner:test","subject":"subject","clientId":"sdk-client","issuer":format!("{origin}auth/"),"deviceId":"fixture-device","generation":1,"validUntilMs":now+30000})
                        }
                        "/hagency/api/v1/sessions/current/renew" => {
                            json!({"validUntilMs":now+30000})
                        }
                        "/hagency/api/v1/sessions/current" => {
                            if head.starts_with("DELETE ") {
                                revoked.fetch_add(1, Ordering::AcqRel);
                                let barrier = revocation_wait.lock().unwrap().take();
                                if let Some(barrier) = barrier {
                                    barrier.entered.notify_one();
                                    barrier.release.notified().await;
                                }
                            }
                            json!({})
                        }
                        _ => panic!("unexpected native initialization route"),
                    };
                    let body = value.to_string();
                    let _=stream.write_all(format!("HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await;
                });
            }
        });
        Self {
            _directory: directory,
            service,
            requests,
            authorizations,
            revocations,
            revoke_barrier,
            task,
        }
    }
    async fn open(&self, source: Arc<dyn MatrixTokenSource>) -> Result<NativeOwner, NativeError> {
        let service = &self.service;
        NativeOwner::open_with_matrix_guarded(
            service.root().unwrap(),
            &service.0.origin,
            &service.0.owner,
            source,
            |owner| {
                service
                    .register_native_owner(owner)
                    .map_err(|code| source_error(&code))
            },
        )
        .await
    }
    async fn open_current_profile(&self) -> NativeOwner {
        use ::hagency::native_owner::{ManagementPackage, ManagementPermission};
        let source = Source::normal(&self.service);
        let owner = self.open(source.clone()).await.unwrap();
        let package = || ManagementPackage {
            digest: "f".repeat(64), instance: 1, account_epoch: 1,
            permissions: vec![ManagementPermission::ReadResources, ManagementPermission::PrepareSourceQuiescence],
        };
        // Explicit setup of this isolated profile creates no provider or quota.
        let review = owner.review_device_setup(package(), None).await.unwrap();
        let selection = owner.confirm_device_setup(review, package(), || true).await.unwrap();
        owner.suspend().await.unwrap();
        self.service.0.native_retirement.lock().unwrap().take();
        drop(owner);
        let service = &self.service;
        selection.open_with_matrix_guarded(
            service.root().unwrap(), &service.0.origin, &service.0.owner, source,
            |owner| service.register_native_owner(owner).map_err(|code| source_error(&code)),
        ).await.unwrap()
    }
    async fn stop(self) {
        self.service.shutdown().await;
        let owner = self.service.0.native_retirement.lock().unwrap().take();
        if let Some(owner) = owner {
            owner.shutdown().await;
        }
        self.task.abort();
    }
}
struct Source {
    service: Weak<Inner>,
    calls: AtomicUsize,
    entered: Option<Arc<tokio::sync::Notify>>,
    release: Option<Arc<tokio::sync::Notify>>,
    unavailable: bool,
}
impl Source {
    fn normal(service: &Service) -> Arc<Self> {
        Arc::new(Self {
            service: Arc::downgrade(&service.0),
            calls: AtomicUsize::new(0),
            entered: None,
            release: None,
            unavailable: false,
        })
    }
}
impl MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            if self.calls.fetch_add(1, Ordering::AcqRel) == 0 {
                if let Some(entered) = &self.entered {
                    entered.notify_one();
                }
                if let Some(release) = &self.release {
                    release.notified().await;
                }
            }
            if self
                .service
                .upgrade()
                .is_none_or(|s| s.retired.load(Ordering::Acquire))
            {
                return Err(source_error("matrix_account_changed"));
            }
            if self.unavailable {
                return Err(source_error("matrix_authorization_unavailable"));
            }
            Ok(MatrixAccessToken {
                access_token: "synthetic-sdk-token".into(),
                client_id: "sdk-client".into(),
            })
        })
    }
}

#[tokio::test]
async fn retired_desktop_cannot_reuse_explicit_signin_permission() {
    let f = Fixture::new().await;
    f.service
        .0
        .explicit_reenrollment
        .store(true, Ordering::Release);
    let source = SdkTokens(Arc::downgrade(&f.service.0));
    f.service.retire();
    assert!(!source.permits_explicit_reenrollment());
    assert!(!source.consume_explicit_reenrollment());
    assert_eq!(f.authorizations.load(Ordering::Acquire), 0);
    f.stop().await;
}

#[test]
fn dropped_desktop_cannot_authorize_native_reenrollment() {
    let source = SdkTokens(Weak::new());
    assert!(!source.permits_explicit_reenrollment());
    assert!(!source.consume_explicit_reenrollment());
}

#[tokio::test]
async fn desktop_retirement_before_initialization_rejects_without_sdk_or_http() {
    let f = Fixture::new().await;
    let source = Source::normal(&f.service);
    f.service.retire();
    assert_eq!(
        f.open(source.clone()).await.err().unwrap().code,
        "matrix_account_changed"
    );
    assert_eq!(source.calls.load(Ordering::Acquire), 0);
    assert_eq!(f.requests.load(Ordering::Acquire), 0);
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    f.stop().await;
}

#[tokio::test]
async fn desktop_retirement_during_initialization_does_not_wait_for_async_native_lock() {
    let f = Fixture::new().await;
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let source = Arc::new(Source {
        service: Arc::downgrade(&f.service.0),
        calls: AtomicUsize::new(0),
        entered: Some(entered.clone()),
        release: Some(release.clone()),
        unavailable: false,
    });
    let async_lock = f.service.0.native.lock().await;
    let mut opening = Box::pin(f.open(source));
    tokio::select! {biased; result=&mut opening=>panic!("initializer unexpectedly completed: {:?}",result.err()), _=entered.notified()=>{}}
    assert!(f.service.0.native_retirement.lock().unwrap().is_some());
    f.service.retire();
    release.notify_one();
    assert_eq!(opening.await.err().unwrap().code, "matrix_account_changed");
    assert_eq!(f.authorizations.load(Ordering::Acquire), 0);
    let retained = f
        .service
        .0
        .native_retirement
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert!(retained.identity().await.is_err());
    drop(async_lock);
    f.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn desktop_retirement_revokes_already_issued_native_management_before_cleanup() {
    let Some(_installation) = current_profile_process(
        "hagency::backend::retirement_tests::desktop_retirement_revokes_already_issued_native_management_before_cleanup",
    ) else { return; };
    let f = Fixture::new().await;
    let owner = f.open_current_profile().await;
    let manager = owner
        .management(::hagency::native_owner::ManagementPackage {
            digest: "c".repeat(64),
            instance: 1,
            account_epoch: 1,
            permissions: vec![::hagency::native_owner::ManagementPermission::ReadResources],
        })
        .await
        .unwrap();
    manager.resources(Default::default()).await.unwrap();
    let async_lock = f.service.0.native.lock().await;
    f.service.retire();
    assert!(owner.identity().await.is_err());
    assert!(manager.resources(Default::default()).await.is_err());
    assert!(f.service.register_native_owner(&owner).is_err());
    drop(async_lock);
    owner.shutdown().await;
    f.stop().await;
}

// The native resource ledger is not qualified on Windows in this release.
// Still exercise retirement there: failing to obtain a management lease must
// not leave a previously opened owner authorized or registered after logout.
#[cfg(not(unix))]
#[tokio::test]
async fn desktop_retirement_revokes_owner_when_native_management_is_unavailable() {
    let f = Fixture::new().await;
    let owner = f.open(Source::normal(&f.service)).await.unwrap();
    let error = owner
        .management(::hagency::native_owner::ManagementPackage {
            digest: "c".repeat(64),
            instance: 1,
            account_epoch: 1,
            permissions: vec![::hagency::native_owner::ManagementPermission::ReadResources],
        })
        .await
        .err()
        .expect("native resource management is unavailable on this platform");
    assert_eq!(error.code, "management_unavailable");
    let async_lock = f.service.0.native.lock().await;
    f.service.retire();
    assert!(owner.identity().await.is_err());
    assert!(f.service.register_native_owner(&owner).is_err());
    drop(async_lock);
    owner.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn desktop_failed_initialization_cannot_reuse_its_retired_owner_on_retry() {
    let f = Fixture::new().await;
    let source = Arc::new(Source {
        service: Arc::downgrade(&f.service.0),
        calls: AtomicUsize::new(0),
        entered: None,
        release: None,
        unavailable: true,
    });
    assert!(f.open(source).await.is_err());
    let failed = f
        .service
        .0
        .native_retirement
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert!(failed.identity().await.is_err());
    // The failed constructor has already shut down. Desktop must discard its
    // registration before a retry can reacquire the exclusive owner state.
    drop(failed);
    let native_lock = f.service.0.native.lock().await;
    f.service.cleanup_unpublished_native_owner().await.unwrap();
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    let good = f.open(Source::normal(&f.service)).await.unwrap();
    drop(native_lock);
    assert!(good.identity().await.is_ok());
    f.service.retire();
    assert!(good.identity().await.is_err());
    good.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn desktop_retirement_cleans_up_cancelled_registered_but_unpublished_owner() {
    let f = Fixture::new().await;
    let ready = Arc::new(tokio::sync::Notify::new());
    let signal = ready.clone();
    let mut initializing = Box::pin(async {
        let mut slot = f.service.0.native.lock().await;
        let owner = f.open(Source::normal(&f.service)).await.unwrap();
        // Same interval as runtime restoration: constructor has registered and
        // authorized the owner, but the async slot has not published it yet.
        signal.notify_one();
        std::future::pending::<()>().await;
        *slot = Some(owner);
    });
    tokio::select! {biased; _=&mut initializing=>panic!("initializer unexpectedly returned"), _=ready.notified()=>{}}
    drop(initializing);
    assert!(f.service.0.native.lock().await.is_none());
    let retained = f
        .service
        .0
        .native_retirement
        .lock()
        .unwrap()
        .clone()
        .unwrap();
    assert!(retained.identity().await.is_ok());
    assert_eq!(f.authorizations.load(Ordering::Acquire), 1);
    assert_eq!(f.revocations.load(Ordering::Acquire), 0);
    f.service.shutdown().await;
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    assert!(retained.identity().await.is_err());
    assert_eq!(f.revocations.load(Ordering::Acquire), 1);
    f.stop().await;
}

#[tokio::test]
async fn desktop_cancelled_initialization_can_retry_in_same_service() {
    let f = Fixture::new().await;
    let ready = Arc::new(tokio::sync::Notify::new());
    let signal = ready.clone();
    let mut initializing = Box::pin(async {
        let mut slot = f.service.0.native.lock().await;
        let owner = f.open(Source::normal(&f.service)).await.unwrap();
        signal.notify_one();
        std::future::pending::<()>().await;
        *slot = Some(owner);
    });
    tokio::select! {biased; _=&mut initializing=>panic!("initializer unexpectedly returned"), _=ready.notified()=>{}}
    drop(initializing);
    let native_lock = f.service.0.native.lock().await;
    assert!(native_lock.is_none());
    assert_eq!(f.authorizations.load(Ordering::Acquire), 1);
    f.service.cleanup_unpublished_native_owner().await.unwrap();
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    assert_eq!(f.revocations.load(Ordering::Acquire), 0);
    assert!(!f.service.0.retired.load(Ordering::Acquire));
    let good = f.open(Source::normal(&f.service)).await.unwrap();
    assert!(good.identity().await.is_ok());
    assert_eq!(f.authorizations.load(Ordering::Acquire), 1);
    drop(native_lock);
    good.shutdown().await;
    f.stop().await;
}

#[tokio::test]
async fn desktop_concurrent_shutdown_keeps_cleanup_custody_after_first_is_cancelled() {
    let f = Fixture::new().await;
    let owner = f.open(Source::normal(&f.service)).await.unwrap();
    *f.service.0.native.lock().await = Some(owner.clone());
    let barrier = Arc::new(RevokeBarrier {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    *f.revoke_barrier.lock().unwrap() = Some(barrier.clone());
    let mut first = Box::pin(f.service.shutdown());
    tokio::select! {biased; _=&mut first=>panic!("first shutdown returned before revoke"), _=barrier.entered.notified()=>{}}
    assert!(owner.identity().await.is_err());
    let mut second = Box::pin(f.service.shutdown());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "concurrent shutdown returned while the first still owned cleanup"
    );
    drop(first);
    barrier.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), second)
        .await
        .unwrap();
    assert!(f.service.0.native.lock().await.is_none());
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    assert!(
        f.revocations.load(Ordering::Acquire) >= 2,
        "canceled revocation must be retried from retained native custody"
    );
    let pending: serde_json::Value = serde_json::from_slice(
        &std::fs::read(f.service.root().unwrap().join("server-login-revocations.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(pending, json!([]));
    f.stop().await;
}

/// Synthetic owner for native host-cache tests; it uses the same real HTTP
/// authorization fixture but intentionally never enters Service's owner slots.
/// Shut down and drop all detached owners/managers before calling stop().
pub(crate) struct NativeFixture(Fixture);

// Cache eviction releases local ownership; it does not reverse an explicit
// logout. Model a separate successful SDK sign-in with a one-use permission.
struct FreshFixtureSignIn {
    source: Arc<Source>,
    available: std::sync::atomic::AtomicBool,
}
impl MatrixTokenSource for FreshFixtureSignIn {
    fn permits_explicit_reenrollment(&self) -> bool {
        self.source.service.upgrade().is_some_and(|service| {
            !service.retired.load(Ordering::Acquire) && self.available.load(Ordering::Acquire)
        })
    }
    fn consume_explicit_reenrollment(&self) -> bool {
        self.source.service.upgrade().is_some_and(|service| {
            !service.retired.load(Ordering::Acquire) && self.available.swap(false, Ordering::AcqRel)
        })
    }
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        self.source.access_token()
    }
}
impl NativeFixture {
    pub(crate) async fn new() -> Self {
        Self(Fixture::new().await)
    }
    pub(crate) async fn open_detached(&self) -> Result<NativeOwner, NativeError> {
        NativeOwner::open_with_matrix(
            self.0.service.root().unwrap(),
            &self.0.service.0.origin,
            &self.0.service.0.owner,
            Source::normal(&self.0.service),
        )
        .await
    }
    pub(crate) async fn open_after_explicit_signin(&self) -> Result<NativeOwner, NativeError> {
        let source = Arc::new(FreshFixtureSignIn {
            source: Source::normal(&self.0.service),
            available: std::sync::atomic::AtomicBool::new(true),
        });
        let owner = NativeOwner::open_with_matrix(
            self.0.service.root().unwrap(),
            &self.0.service.0.origin,
            &self.0.service.0.owner,
            source.clone(),
        )
        .await?;
        assert!(!source.permits_explicit_reenrollment());
        assert!(!source.consume_explicit_reenrollment());
        Ok(owner)
    }
    pub(crate) async fn stop(self) {
        self.0.stop().await;
    }
}

#[tokio::test]
async fn sdk_replacement_callback_retires_real_native_manager_before_slot_publication() {
    let Some(_installation) = current_profile_process(
        "hagency::backend::retirement_tests::sdk_replacement_callback_retires_real_native_manager_before_slot_publication",
    ) else { return; };
    let f = Fixture::new().await;
    let owner = f.open_current_profile().await;
    let manager = owner
        .management(::hagency::native_owner::ManagementPackage {
            digest: "e".repeat(64),
            instance: 1,
            account_epoch: 1,
            permissions: vec![::hagency::native_owner::ManagementPermission::ReadResources],
        })
        .await
        .unwrap();
    manager.resources(Default::default()).await.unwrap();
    let slot = crate::sliding_sync::client_slot::ClientSlot::new();
    slot.replace(Some("same SDK account"), || {});
    let async_lock = f.service.0.native.lock().await;
    let previous = slot.replace(Some("same SDK account"), || {
        assert_eq!(slot.revision(), 2);
        f.service.retire();
        assert!(f.service.0.retired.load(Ordering::Acquire));
    });
    assert_eq!(previous, Some("same SDK account"));
    assert!(owner.identity().await.is_err());
    assert!(manager.resources(Default::default()).await.is_err());
    drop(async_lock);
    owner.shutdown().await;
    f.stop().await;
}

#[cfg(feature = "resource-adoption")]
#[tokio::test]
async fn desktop_adoption_drain_retains_pending_transition_across_cancelled_logout() {
    let f = Fixture::new().await;
    let owner = f.open(Source::normal(&f.service)).await.unwrap();
    *f.service.0.native.lock().await = Some(owner.clone());
    f.service.0.setup_pending.store(true, Ordering::Release);
    let barrier = Arc::new(RevokeBarrier {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    *f.revoke_barrier.lock().unwrap() = Some(barrier.clone());
    // Explicit logout races with an adoption transition. It owns revocation;
    // ordinary source suspension below must never undo that durable intent.
    f.service.revoke_resumption().unwrap();
    let mut first = Box::pin(async {
        let mut slot = f.service.0.native.lock().await;
        f.service.drain_setup_owner(&mut slot).await
    });
    tokio::select! {biased; _=&mut first=>panic!("transition returned before revoke"), _=barrier.entered.notified()=>{}}
    assert!(owner.identity().await.is_err());
    assert!(f.service.0.setup_pending.load(Ordering::Acquire));
    assert!(f.service.0.native_retirement.lock().unwrap().is_some());
    drop(first);
    assert!(f.service.0.native.lock().await.is_some());
    barrier.release.notify_one();
    let mut slot = f.service.0.native.lock().await;
    // This fixture intentionally has no global SDK. Its final account check
    // fails, after the real retained revocation/drain has completed.
    assert!(f.service.drain_setup_owner(&mut slot).await.is_err());
    assert!(slot.is_none());
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    assert!(!f.service.0.setup_pending.load(Ordering::Acquire));
    assert!(f.service.0.retired.load(Ordering::Acquire));
    assert!(f.revocations.load(Ordering::Acquire) >= 2);
    drop(slot);
    f.stop().await;
}

#[cfg(feature = "resource-adoption")]
#[tokio::test]
async fn desktop_activation_waiter_cancellation_and_logout_keep_original_drain() {
    let f = Fixture::new().await;
    let owner = f.open(Source::normal(&f.service)).await.unwrap();
    *f.service.0.native.lock().await = Some(owner.clone());
    let barrier = Arc::new(RevokeBarrier {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    *f.revoke_barrier.lock().unwrap() = Some(barrier.clone());
    f.service.revoke_resumption().unwrap();
    let service = f.service.clone();
    // Enter the actual confirmed task's cleanup stage. Migration itself is
    // covered by the isolated multi-process source/admission fixture.
    *f.service.0.activation.lock().await =
        adoption::ActivationTask::Running(tokio::spawn(async move {
            let mut slot = service.0.native.lock().await;
            let owner = slot.as_ref().unwrap().clone();
            service
                .finish_activation(
                    &mut slot,
                    owner,
                    Err("current_profile_outcome_unknown".into()),
                )
                .await
        }));
    let mut ui_waiter = Box::pin(async {
        let mut task = f.service.0.activation.lock().await;
        Service::await_activation(&mut task).await
    });
    tokio::select! {
        biased;
        _ = &mut ui_waiter => panic!("activation completed before native revoke"),
        _ = barrier.entered.notified() => {}
    }
    drop(ui_waiter);
    assert!(matches!(
        *f.service.0.activation.lock().await,
        adoption::ActivationTask::Running(_)
    ));
    assert!(f.service.0.native_retirement.lock().unwrap().is_some());
    f.service.retire();
    assert!(owner.identity().await.is_err());
    let mut logout = Box::pin(f.service.shutdown());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut logout)
            .await
            .is_err(),
        "logout must wait for the retained original drain"
    );
    barrier.release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(3), logout)
        .await
        .unwrap();
    let mut task = f.service.0.activation.lock().await;
    assert_eq!(
        Service::await_activation(&mut task).await.err().unwrap(),
        "management_outcome_unknown"
    );
    assert_eq!(f.revocations.load(Ordering::Acquire), 1);
    assert!(f.service.0.native.lock().await.is_none());
    assert!(f.service.0.native_retirement.lock().unwrap().is_none());
    drop(task);
    let pending: serde_json::Value = serde_json::from_slice(
        &std::fs::read(f.service.root().unwrap().join("server-login-revocations.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(pending, json!([]));
    f.stop().await;
}
