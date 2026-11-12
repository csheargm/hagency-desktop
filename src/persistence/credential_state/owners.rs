//! Every SDK authentication owner reserves its snapshot capacity before it can
//! activate. A callback replaces that owner's cell; it never asks for capacity.
use super::*;
use matrix_sdk::Client;
use std::sync::{
    Weak,
    atomic::{AtomicU8, AtomicU64},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const CAPACITY: usize = 128;
pub(super) const PREPARING: u8 = 0;
pub(super) const PUBLISHED: u8 = 1;
pub(super) const REVOKING: u8 = 2;
pub(super) const ACTIVATING: u8 = 3;
static ADMISSION: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(CAPACITY)));
static REGISTRY: Mutex<Vec<Weak<Owner>>> = Mutex::new(Vec::new());
static PENDING: LazyLock<Mutex<Vec<Arc<Owner>>>> =
    LazyLock::new(|| Mutex::new(Vec::with_capacity(CAPACITY)));

#[derive(Clone)]
pub(super) struct Update {
    pub revision: u64,
    pub user: PersistedUserSession,
    pub sync_token: Option<String>,
    pub version: SlidingSyncVersion,
}
pub(super) struct Owner {
    _admission: OwnedSemaphorePermit,
    spare: Mutex<Option<OwnedSemaphorePermit>>,
    pub service: Arc<Service>,
    pub client: matrix_sdk::WeakClient,
    last_client: Mutex<matrix_sdk::WeakClient>,
    pub epoch: u64,
    pub metadata: ClientSessionPersisted,
    bound_metadata: std::sync::OnceLock<ClientSessionPersisted>,
    target: std::sync::OnceLock<revoke_transport::Target>,
    pub ram_progress: Mutex<revoke_transport::Progress>,
    pub keys: Arc<std::sync::OnceLock<Arc<Unlocked>>>,
    pub state: AtomicU8,
    pub preparing: AtomicBool,
    pub observing: AtomicBool,
    pub cleanup_pending: AtomicBool,
    pub fresh: std::sync::OnceLock<Arc<FreshDatabase>>,
    callback_life: std::sync::OnceLock<Weak<()>>,
    latest: Mutex<Option<Update>>,
    sequence: AtomicU64,
}
impl Drop for Owner {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.metadata.passphrase.zeroize();
        if let Some(metadata) = self.bound_metadata.get_mut() {
            metadata.passphrase.zeroize();
        }
    }
}

pub(super) fn reserve(
    service: Arc<Service>,
    client: &matrix_sdk::Client,
    metadata: ClientSessionPersisted,
) -> Result<Arc<Owner>> {
    if find(client)?.is_some() {
        return Err(Error::Conflict.into());
    }
    let admission = ADMISSION
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::Capacity)?;
    reserve_admitted(service, client, metadata, admission)
}
pub(super) fn reserve_admitted(
    service: Arc<Service>,
    client: &Client,
    metadata: ClientSessionPersisted,
    admission: OwnedSemaphorePermit,
) -> Result<Arc<Owner>> {
    let weak = client.downgrade();
    let owner = Arc::new(Owner {
        _admission: admission,
        spare: Mutex::new(None),
        service,
        client: weak.clone(),
        last_client: Mutex::new(weak),
        epoch: crate::account_session::epoch(),
        metadata,
        bound_metadata: std::sync::OnceLock::new(),
        target: std::sync::OnceLock::new(),
        ram_progress: Mutex::new(revoke_transport::Progress::default()),
        keys: Arc::new(std::sync::OnceLock::new()),
        state: AtomicU8::new(PREPARING),
        preparing: AtomicBool::new(false),
        observing: AtomicBool::new(false),
        cleanup_pending: AtomicBool::new(false),
        fresh: std::sync::OnceLock::new(),
        callback_life: std::sync::OnceLock::new(),
        latest: Mutex::new(None),
        sequence: AtomicU64::new(0),
    });
    let mut registry = REGISTRY.lock().map_err(|_| Error::Unavailable)?;
    registry.retain(|owner| owner.strong_count() > 0);
    registry.push(Arc::downgrade(&owner));
    Ok(owner)
}
pub(super) fn find(client: &Client) -> Result<Option<Arc<Owner>>> {
    let owners: Vec<_> = REGISTRY
        .lock()
        .map_err(|_| Error::Unavailable)?
        .iter()
        .filter_map(Weak::upgrade)
        .collect();
    // The pinned official SDK exposes no WeakClient::ptr_eq. http_client()
    // borrows the reqwest wrapper stored directly in ClientInner: clones share
    // that exact field, while separate SDK clients have separate fields even
    // when configured with clones of one reqwest client. No network IO occurs.
    Ok(owners.into_iter().find(|owner| {
        owner
            .client
            .get()
            .is_some_and(|original| std::ptr::eq(original.http_client(), client.http_client()))
    }))
}
pub(super) fn mark_pending(owner: &Arc<Owner>) -> Result<()> {
    let mut pending = PENDING.lock().map_err(|_| Error::Unavailable)?;
    if !pending.iter().any(|value| Arc::ptr_eq(value, owner)) {
        // Each element owns an admission permit; this bound was reserved before
        // activation. It is an invariant, never a late callback rejection path.
        assert!(pending.len() < CAPACITY, "owner admission invariant");
        pending.push(owner.clone());
    }
    Ok(())
}
pub(super) fn pending() -> Result<Vec<Arc<Owner>>> {
    Ok(PENDING.lock().map_err(|_| Error::Unavailable)?.clone())
}
pub(super) fn remove_if_clean(owner: &Arc<Owner>) -> Result<()> {
    // Keep the predicate locked through the registry removal. A capture writes
    // latest before taking PENDING, so it either prevents removal or reinserts
    // after it; a dirty owner can never lose its admitted strong custody.
    let latest = owner.latest.lock().map_err(|_| Error::Unavailable)?;
    if latest.is_some() || owner.cleanup_pending.load(Ordering::Acquire) {
        return Ok(());
    }
    #[cfg(test)]
    if let Some(window) = REMOVE_WINDOW.lock().unwrap().take() {
        window.entered.send(()).unwrap();
        window.release.recv().unwrap();
    }
    let removed = {
        let mut pending = PENDING.lock().map_err(|_| Error::Unavailable)?;
        let index = pending.iter().position(|value| Arc::ptr_eq(value, owner));
        index.map(|index| pending.swap_remove(index))
    };
    drop(latest);
    drop(removed);
    Ok(())
}
#[cfg(test)]
struct RemoveWindow {
    entered: std::sync::mpsc::SyncSender<()>,
    release: std::sync::mpsc::Receiver<()>,
}
#[cfg(test)]
static REMOVE_WINDOW: Mutex<Option<RemoveWindow>> = Mutex::new(None);
#[cfg(test)]
static CAPTURE_ENTERED: Mutex<Option<std::sync::mpsc::SyncSender<()>>> = Mutex::new(None);
#[cfg(test)]
static CAPTURE_READ_WINDOW: Mutex<Option<RemoveWindow>> = Mutex::new(None);

impl Owner {
    pub fn bind_metadata(&self, metadata: ClientSessionPersisted) -> Result<()> {
        ensure!(
            url::Url::parse(&metadata.homeserver)? == url::Url::parse(&self.metadata.homeserver)?,
            "credential_identity_changed"
        );
        if let Some(old) = self.bound_metadata.get() {
            ensure!(
                old.homeserver == metadata.homeserver
                    && old.db_path == metadata.db_path
                    && old.passphrase == metadata.passphrase,
                "credential_identity_changed"
            );
            return Ok(());
        }
        self.bound_metadata
            .set(metadata)
            .map_err(|_| Error::Conflict.into())
    }
    pub fn reserve_handoff(&self) -> Result<()> {
        let permit = ADMISSION
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Capacity)?;
        *self.spare.lock().map_err(|_| Error::Unavailable)? = Some(permit);
        Ok(())
    }
    pub fn has_handoff(&self) -> bool {
        self.spare
            .lock()
            .map(|value| value.is_some())
            .unwrap_or(false)
    }
    pub fn take_handoff(&self) -> Result<OwnedSemaphorePermit> {
        self.spare
            .lock()
            .map_err(|_| Error::Unavailable)?
            .take()
            .ok_or(Error::Capacity.into())
    }
    pub fn capture(self: &Arc<Self>, client: &Client) -> Result<u64> {
        #[cfg(test)]
        if let Some(entered) = CAPTURE_ENTERED.lock().unwrap().take() {
            entered.send(()).unwrap();
        }
        // Serialize the actual SDK read with replacement of the admitted cell.
        // Otherwise an older reader can overwrite a newer refresh callback's
        // snapshot and clear it before that callback acquires SESSION_IO.
        // SDK session() only clones under the short token mutex; the SDK drops
        // that mutex before invoking either synchronous session callback.
        let revision = {
            let mut latest = self.latest.lock().map_err(|_| Error::Unavailable)?;
            let user = PersistedUserSession::from_sdk(client.session().ok_or(Error::Invalid)?)?;
            ensure!(
                url::Url::parse(&self.metadata.homeserver)? == client.homeserver(),
                "credential_identity_changed"
            );
            let sync_token = self.keys.get().and_then(|keys| keys.sync_token.clone());
            let version = client.sliding_sync_version().into();
            let revision = self.sequence.fetch_add(1, Ordering::AcqRel) + 1;
            #[cfg(test)]
            {
                let window = { CAPTURE_READ_WINDOW.lock().unwrap().take() };
                if let Some(window) = window {
                    window.entered.send(()).unwrap();
                    window.release.recv().unwrap();
                }
            }
            *latest = Some(Update {
                revision,
                user,
                sync_token,
                version,
            });
            *self.last_client.lock().map_err(|_| Error::Unavailable)? = client.downgrade();
            revision
        };
        // No latest/token guard survives into the pending registry, filesystem,
        // native worker, or SESSION_IO publication boundary.
        mark_pending(self)?;
        Ok(revision)
    }
    pub fn update(&self) -> Result<Option<Update>> {
        Ok(self.latest.lock().map_err(|_| Error::Unavailable)?.clone())
    }
    pub fn dirty(&self) -> bool {
        self.latest
            .lock()
            .map(|value| value.is_some())
            .unwrap_or(true)
    }
    pub fn sdk(&self) -> Option<Client> {
        self.last_client
            .lock()
            .ok()
            .and_then(|weak| weak.get())
            .or_else(|| self.client.get())
    }
    pub fn snapshot(&self, update: Update) -> FullSessionPersisted {
        FullSessionPersisted {
            client_session: self
                .keys
                .get()
                .map(|keys| ClientSessionPersisted {
                    homeserver: keys.database.homeserver.clone(),
                    db_path: keys.database.db_path.clone(),
                    passphrase: keys.secret.to_string(),
                })
                .unwrap_or_else(|| self.bound_metadata.get().unwrap_or(&self.metadata).clone()),
            user_session: update.user,
            sync_token: update.sync_token,
            sliding_sync_version: update.version,
        }
    }
    pub fn clear(self: &Arc<Self>, revision: u64) -> Result<()> {
        {
            let mut latest = self.latest.lock().map_err(|_| Error::Unavailable)?;
            if latest
                .as_ref()
                .is_some_and(|value| value.revision == revision)
            {
                latest.take();
            }
        }
        remove_if_clean(self)
    }
    pub fn bind_callback_life(&self, life: &Arc<()>) -> Result<()> {
        self.callback_life
            .set(Arc::downgrade(life))
            .map_err(|_| Error::Conflict.into())
    }
    pub fn callbacks_alive(&self) -> bool {
        self.callback_life
            .get()
            .is_some_and(|life| life.upgrade().is_some())
    }
    pub fn require_cleanup(self: &Arc<Self>) -> Result<()> {
        let _latest = self.latest.lock().map_err(|_| Error::Unavailable)?;
        self.cleanup_pending.store(true, Ordering::Release);
        mark_pending(self)
    }
    pub fn target(&self) -> Option<revoke_transport::Target> {
        self.target.get().cloned()
    }
    pub fn set_target(&self, target: revoke_transport::Target) -> Result<()> {
        // A captured target is immutable for this logical SDK authentication owner.
        if self.target.get().is_none() {
            self.target.set(target).map_err(|_| Error::Conflict)?;
        }
        Ok(())
    }
    pub fn set_keys(&self, keys: Arc<Unlocked>) -> Result<()> {
        if let Some(target) = self.target.get() {
            let _ = keys.target.set(target.clone());
        }
        self.keys.set(keys).map_err(|_| Error::Conflict.into())
    }
    pub fn path(&self) -> Result<PathBuf> {
        let keys = self.keys.get().ok_or(Error::Unavailable)?;
        Ok(
            crate::accounts::account_dir(&self.service.root, &keys.identity.owner)
                .join("persistent_state/session"),
        )
    }
    /// Called only under SESSION_IO by the original callback or owned worker.
    pub fn flush(self: &Arc<Self>) -> Result<()> {
        if !self.dirty() {
            return Ok(());
        }
        // This fresh read happens after any native preparation wait. If the
        // original SDK has ended, its already admitted latest snapshot remains.
        if let Some(client) = self.sdk() {
            self.capture(&client)?;
        }
        let update = self.update()?.ok_or(Error::Invalid)?;
        let revision = update.revision;
        let keys = self.keys.get().ok_or(Error::Unavailable)?;
        let session = self.snapshot(update);
        ensure!(
            identity(&session) == keys.identity,
            "credential_identity_changed"
        );
        let path = self.path()?;
        let revoke =
            self.state.load(Ordering::Acquire) == REVOKING || keys.logout.load(Ordering::Acquire);
        let active = self.state.load(Ordering::Acquire) == PUBLISHED
            && self.epoch == crate::account_session::epoch()
            && self.service.owns_owner(&path, self);
        if revoke {
            recovery::write(keys, &session, recovery::Intent::Revoke)?;
        } else if !active {
            recovery::write(keys, &session, recovery::Intent::Keep)?;
        } else {
            persist_owner_snapshot(&path, keys, session, self)?;
        }
        self.clear(revision)
    }
}

pub(super) struct Preparing(pub Arc<Owner>);
impl Preparing {
    pub fn new(owner: &Arc<Owner>) -> Result<Self> {
        ensure!(
            !owner.preparing.swap(true, Ordering::AcqRel),
            "credential_preparation_already_running"
        );
        Ok(Self(owner.clone()))
    }
}
impl Drop for Preparing {
    fn drop(&mut self) {
        self.0.preparing.store(false, Ordering::Release);
    }
}

pub(super) fn live_grant(keys: &Unlocked) -> bool {
    let owners = match REGISTRY.lock() {
        Ok(registry) => registry
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>(),
        Err(_) => return true,
    };
    owners.iter().any(|owner| {
        owner
            .keys
            .get()
            .is_some_and(|value| value.key.reference() == keys.key.reference())
            && (owner.callbacks_alive()
                || owner.dirty()
                || owner.cleanup_pending.load(Ordering::Acquire)
                || owner.preparing.load(Ordering::Acquire))
    })
}

pub(super) struct Observation(pub Arc<Owner>);
impl Observation {
    pub fn new(owner: &Arc<Owner>) -> Self {
        owner.observing.store(true, Ordering::Release);
        Self(owner.clone())
    }
}
impl Drop for Observation {
    fn drop(&mut self) {
        self.0.observing.store(false, Ordering::Release);
        if self.0.state.load(Ordering::Acquire) == PREPARING {
            self.0.state.store(REVOKING, Ordering::Release);
            // Never start another native prompt for a cancelled preparation.
            // Ready RAM keys can retire through this original owned worker.
            if self.0.keys.get().is_some() {
                let _ = self.0.require_cleanup();
            } else {
                let _ = mark_pending(&self.0);
            }
            if self.0.keys.get().is_some() {
                let owner = self.0.clone();
                let _ = self
                    .0
                    .service
                    .worker
                    .call_sync(std::time::Duration::ZERO, move || {
                        let _guard = super::super::SESSION_IO
                            .lock()
                            .map_err(|_| Error::Unavailable)?;
                        recovery::retire_preparation(&owner).map_err(classify)
                    });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_migration_concurrent_clear_and_real_refresh_keep_dirty_owner_reserved() {
        const NAME: &str = "persistence::credential_state::owners::tests::credential_migration_concurrent_clear_and_real_refresh_keep_dirty_owner_reserved";
        if super::super::tests::isolated_case(NAME, "HAGENCY_PENDING_WINDOW_CHILD") {
            return;
        }
        use wiremock::{
            MockServer, Mock, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/_matrix/client/v3/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"concurrent-access","refresh_token":"concurrent-refresh"}))).expect(1).mount(&server).await;
        let root = tempfile::tempdir().unwrap();
        let service = super::super::tests::fake_service(root.path());
        let mut session = super::super::tests::session_fixture();
        session.client_session.homeserver = server.uri();
        let client = Client::builder()
            .homeserver_url(server.uri())
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
            .build()
            .await
            .unwrap();
        let owner = install_owner(service.clone(), &client, session.client_session, None).unwrap();
        client
            .restore_session(session.user_session.into_sdk())
            .await
            .unwrap();
        let revision = owner.capture(&client).unwrap();
        let (entered, waiting) = std::sync::mpsc::sync_channel(1);
        let (release, wait_release) = std::sync::mpsc::channel();
        struct Release(Option<std::sync::mpsc::Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }
        let release = Release(Some(release));
        *REMOVE_WINDOW.lock().unwrap() = Some(RemoveWindow {
            entered,
            release: wait_release,
        });
        let clearing = owner.clone();
        let clear = std::thread::spawn(move || clearing.clear(revision));
        waiting
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (capture_entered, capture_wait) = std::sync::mpsc::sync_channel(1);
        *CAPTURE_ENTERED.lock().unwrap() = Some(capture_entered);
        let original = client.clone();
        let refresh =
            tokio::spawn(async move { original.matrix_auth().refresh_access_token().await });
        // The deliberately blocked synchronous SDK callback can also hold a
        // runtime timer driver. The control thread uses an OS barrier instead.
        capture_wait
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        assert_eq!(client.access_token().as_deref(), Some("concurrent-access"));
        assert!(
            !refresh.is_finished(),
            "callback capture waits through the real pending-removal window"
        );
        assert!(
            owner.latest.try_lock().is_err(),
            "latest predicate stays locked through pending removal"
        );
        drop(release);
        clear.join().unwrap().unwrap();
        refresh.await.unwrap().unwrap();
        let actual = owner.update().unwrap().unwrap();
        assert!(
            matches!(&actual.user, PersistedUserSession::Matrix(value) if value.tokens.access_token == "concurrent-access" && value.tokens.refresh_token.as_deref() == Some("concurrent-refresh"))
        );
        assert!(
            pending()
                .unwrap()
                .iter()
                .any(|pending| Arc::ptr_eq(pending, &owner)),
            "dirty owner retains its original admitted strong custody"
        );
        owner.clear(actual.revision).unwrap();
        assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn credential_migration_old_capture_cannot_overwrite_real_rotated_pair() {
        const NAME: &str = "persistence::credential_state::owners::tests::credential_migration_old_capture_cannot_overwrite_real_rotated_pair";
        if super::super::tests::isolated_case(NAME, "HAGENCY_CAPTURE_ORDER_CHILD") {
            return;
        }
        use wiremock::{
            Mock, MockServer, ResponseTemplate,
            matchers::{method, path},
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token":"actual-new-access", "refresh_token":"actual-new-refresh"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("private-profile");
        hagency_store::private::create_directory_new(&root).unwrap();
        let service = super::super::tests::fake_service(&root);
        let mut session = super::super::tests::session_fixture();
        session.client_session.homeserver = server.uri();
        let account = crate::accounts::account_dir(&root, &session.user_session.meta().user_id);
        hagency_store::private::directory(&account).unwrap();
        let state = account.join("persistent_state");
        hagency_store::private::directory(&state).unwrap();
        let file = state.join("session");
        hagency_store::private::write_new(&file, &serde_json::to_vec(&session).unwrap()).unwrap();
        service
            .load_worker(&file, crate::account_session::epoch(), || true)
            .unwrap();
        let keys = service
            .cached(&file, crate::account_session::epoch())
            .unwrap();
        let client = Client::builder()
            .homeserver_url(server.uri())
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_13])
            .build()
            .await
            .unwrap();
        let owner = install_owner(
            service.clone(),
            &client,
            session.client_session.clone(),
            Some(keys),
        )
        .unwrap();
        owner.state.store(PUBLISHED, Ordering::Release);
        service.bind_owner(&file, &owner).unwrap();
        client
            .restore_session(session.user_session.into_sdk())
            .await
            .unwrap();
        owner.capture(&client).unwrap();
        let (entered, old_read) = std::sync::mpsc::sync_channel(1);
        let (release, resume) = std::sync::mpsc::channel();
        struct Release(Option<std::sync::mpsc::Sender<()>>);
        impl Drop for Release {
            fn drop(&mut self) {
                if let Some(release) = self.0.take() {
                    let _ = release.send(());
                }
            }
        }
        let release = Release(Some(release));
        *CAPTURE_READ_WINDOW.lock().unwrap() = Some(RemoveWindow {
            entered,
            release: resume,
        });
        let flushing = owner.clone();
        let flush = std::thread::spawn(move || {
            let _guard = super::super::super::SESSION_IO.lock().unwrap();
            flushing.flush()
        });
        old_read
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        let (entered, callback) = std::sync::mpsc::sync_channel(1);
        *CAPTURE_ENTERED.lock().unwrap() = Some(entered);
        let original = client.clone();
        let refresh =
            tokio::spawn(async move { original.matrix_auth().refresh_access_token().await });
        callback
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(client.access_token().as_deref(), Some("actual-new-access"));
        assert!(
            !refresh.is_finished(),
            "the original SDK callback is still owned"
        );
        assert!(
            owner.latest.try_lock().is_err(),
            "old SDK read and snapshot replacement must be one short critical section"
        );
        assert!(
            pending()
                .unwrap()
                .iter()
                .any(|value| Arc::ptr_eq(value, &owner)),
            "the admitted original snapshot slot remains retained"
        );
        drop(release);
        flush.join().unwrap().unwrap();
        refresh.await.unwrap().unwrap();
        let actual = service
            .load_worker(&file, crate::account_session::epoch(), || true)
            .unwrap();
        assert!(
            matches!(actual.user_session, PersistedUserSession::Matrix(value)
            if value.tokens.access_token == "actual-new-access"
                && value.tokens.refresh_token.as_deref() == Some("actual-new-refresh")),
            "both actual rotated tokens must reach the authenticated canonical document"
        );
        assert!(!owner.dirty());
        assert!(service.worker.shutdown(std::time::Duration::from_secs(2)));
    }
}

#[cfg(test)]
mod official_sdk_identity_tests {
    use super::*;
    #[tokio::test]
    async fn credential_owner_identity_distinguishes_preauth_clients_sharing_http_transport() {
        let transport = matrix_sdk::reqwest::Client::new();
        let a = Client::builder()
            .homeserver_url("http://localhost")
            .http_client(transport.clone())
            .build()
            .await
            .unwrap();
        let b = Client::builder()
            .homeserver_url("http://localhost")
            .http_client(transport)
            .build()
            .await
            .unwrap();
        assert!(a.session_meta().is_none() && b.session_meta().is_none());
        assert!(std::ptr::eq(a.http_client(), a.clone().http_client()));
        assert!(!std::ptr::eq(a.http_client(), b.http_client()));
        let weak = a.downgrade();
        assert!(std::ptr::eq(
            a.http_client(),
            weak.get().unwrap().http_client()
        ));
    }
}
