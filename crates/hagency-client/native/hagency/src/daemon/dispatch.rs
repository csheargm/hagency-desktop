//! Request dispatch: owner scope, idempotency, revision checks and the hosting
//! state machine. Execution lives behind [`Runtime`]; this layer never reports
//! a transition as complete before the runtime confirms it.
use super::{
    control::{ControlStore, Intent, Lookup, now_ms},
    protocol::{
        API_VERSION, Command, CommandOutcome, DaemonError, Hello, OutcomeState, Period, Reply,
        Request, errors,
    },
};
use hagency_metering::dashboard::{
    Activity, DASHBOARD_API_VERSION, DashboardSnapshot, HostingState, HostingStatus,
    OwnerIdentity, ProviderAccount, ResourceUsage, UsageSummary,
};
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerScope {
    pub user_id: String,
    pub server: String,
    pub display_name: String,
}

/// What the runtime has actually observed. `Running` requires authorized
/// workers; an enrolled owner alone is `Idle`/`Starting`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Starting,
    Running,
    Offline,
    Failed,
    Draining,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub phase: Phase,
    /// Static, user-presentable code or sentence. Never source/provider text.
    pub detail: Option<String>,
    /// `None` means unknown, which is different from zero.
    pub active_runs: Option<u64>,
    pub pending_approvals: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct SnapshotParts {
    pub inventory_available:bool,
    pub inventory_complete:bool,
    pub accounts: Vec<ProviderAccount>,
    pub resources: Vec<ResourceUsage>,
    pub usage: Option<UsageSummary>,
    pub usage_breakdown:Option<hagency_metering::dashboard::UsageBreakdown>,
    pub activities: Vec<Activity>,
}

/// The hosting engine controlled by the daemon. Implemented by
/// [`super::runtime::OwnerRuntime`] for real owners.
pub trait Runtime: Send + Sync + 'static {
    #[cfg(unix)]
    fn enrollment(&self,_id:String,_call:super::enrollment_rpc::Call)->BoxFuture<'_,Result<super::enrollment_rpc::Status,String>>{Box::pin(async{Err("daemon_enrollment_unavailable".into())})}
    fn bridge(&self, _id: String, _call: super::bridge::Call) -> BoxFuture<'_, Result<serde_json::Value, super::bridge::Failure>> {
        Box::pin(async { Err(crate::native_owner::NativeError { status:503, code:"daemon_bridge_unavailable".into() }.into()) })
    }
    fn generation(&self) -> u64 { 0 }
    fn owner(&self) -> Option<OwnerScope>;
    fn observe(&self) -> Observation;
    /// Close admission of new work and wait, bounded, until no admission
    /// operation is in flight. Returns whether that quiescence was reached.
    fn close_admission(&self) -> BoxFuture<'_, bool>;
    fn open_admission(&self);
    /// Begin or retry hosting. Returns at once; progress appears in `observe`.
    fn start(&self);
    /// Fence and drain hosting and release the owner root. Ordinary stop:
    /// preserves enrollment and never revokes owner credentials.
    fn stop(&self) -> BoxFuture<'_, Result<(), &'static str>>;
    fn snapshot_parts(&self, period: Option<Period>, filter:Option<hagency_metering::dashboard::UsageFilter>) -> BoxFuture<'_, SnapshotParts>;
    fn refresh_providers(&self) -> BoxFuture<'_, Result<(), DaemonError>>;
}

#[derive(Default)]
struct Drain {
    running: bool,
    failure: Option<&'static str>,
}

pub struct Controller<R: Runtime> {
    store: ControlStore,
    runtime: Arc<R>,
    mutation: tokio::sync::Mutex<()>,
    inflight: Mutex<HashMap<String, String>>,
    drain: Arc<Mutex<Drain>>,
    published: Mutex<Option<(HostingState, Option<String>)>>,
    closing: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Map durable intent plus runtime evidence to the shared hosting state.
pub fn effective(
    owner: bool,
    intent: Intent,
    observed: &Observation,
    draining: bool,
    drain_failure: Option<&'static str>,
) -> HostingStatus {
    let mut detail = observed.detail.clone();
    let state = if !owner {
        HostingState::Unconfigured
    } else if let Some(code) = drain_failure {
        detail = Some(code.into());
        HostingState::Failed
    } else {
        match (intent, observed.phase) {
            (Intent::Stopped, _) if draining => HostingState::Draining,
            (Intent::Stopped, Phase::Starting | Phase::Running | Phase::Draining) => {
                HostingState::Draining
            }
            (Intent::Stopped, _) => HostingState::Stopped,
            (_, Phase::Idle | Phase::Starting | Phase::Stopped) => HostingState::Starting,
            (_, Phase::Offline) => HostingState::Offline,
            (_, Phase::Failed) => HostingState::Failed,
            (_, Phase::Draining) => HostingState::Draining,
            (Intent::Paused, Phase::Running) => HostingState::Paused,
            (Intent::Hosting, Phase::Running) => HostingState::Running,
        }
    };
    HostingStatus {
        state,
        detail,
        // Unknown stays `None`; never reported as zero.
        active_runs: observed.active_runs,
        pending_approvals: observed.pending_approvals,
    }
}

impl<R: Runtime> Controller<R> {
    pub fn new(store: ControlStore, runtime: Arc<R>) -> Arc<Self> {
        Arc::new(Self {
            store,
            runtime,
            mutation: tokio::sync::Mutex::new(()),
            inflight: Mutex::new(HashMap::new()),
            drain: Arc::new(Mutex::new(Drain::default())),
            published: Mutex::new(None),
            closing: AtomicBool::new(false),
        })
    }

    pub fn runtime(&self) -> &Arc<R> {
        &self.runtime
    }

    pub fn revision(&self) -> u64 {
        self.store.revision()
    }

    /// Apply the saved intent after daemon start. A stopped intent stays
    /// stopped: the owner root is not opened until an explicit resume.
    pub async fn boot(&self) {
        match self.store.intent() {
            Intent::Hosting => {
                self.runtime.open_admission();
                self.runtime.start();
            }
            Intent::Paused => {
                self.runtime.close_admission().await;
                self.runtime.start();
            }
            Intent::Stopped => {}
        }
        self.observe_transition();
    }

    pub fn hosting_status(&self) -> HostingStatus {
        let drain = lock(&self.drain);
        effective(
            self.runtime.owner().is_some(),
            self.store.intent(),
            &self.runtime.observe(),
            drain.running,
            drain.failure,
        )
    }

    /// Advance the revision when the published state or its detail changed.
    /// Counter changes alone do not invalidate controller edits.
    pub fn observe_transition(&self) -> HostingStatus {
        let status = self.hosting_status();
        let key = (status.state, status.detail.clone());
        let mut published = lock(&self.published);
        if published.as_ref() != Some(&key) {
            if published.is_some() && self.store.bump().is_err() {
                tracing::warn!(
                    code = "daemon_revision_unavailable",
                    "Hosting transition revision not persisted"
                );
            }
            *published = Some(key);
        }
        status
    }

    pub async fn snapshot(&self, period: Option<Period>, filter:Option<hagency_metering::dashboard::UsageFilter>) -> DashboardSnapshot {
        let hosting = self.observe_transition();
        let Some(owner) = self.runtime.owner() else {
            let mut snapshot = DashboardSnapshot::unconfigured(now_ms());
            snapshot.revision = self.store.revision();
            snapshot.hosting = hosting;
            return snapshot;
        };
        let parts = self.runtime.snapshot_parts(period,filter).await;
        DashboardSnapshot {
            version: DASHBOARD_API_VERSION,
            revision: self.store.revision(),
            observed_at_ms: now_ms(),
            owner: Some(OwnerIdentity {
                user_id: owner.user_id,
                display_name: owner.display_name,
                server: owner.server,
            }),
            hosting,
            accounts: parts.accounts,
            resources: parts.resources,
            inventory_available:parts.inventory_available,
            inventory_complete:parts.inventory_complete,
            usage: parts.usage.unwrap_or_default(),
            usage_breakdown:parts.usage_breakdown,
            activities: parts.activities,
        }
    }

    fn reply_error(&self, request: &Request, error: DaemonError) -> Reply {
        Reply::err(&request.request_id, self.store.revision(), error)
    }

    pub async fn handle(self: &Arc<Self>, request: Request) -> Reply {
        if let Err(error) = request.validate() {
            return self.reply_error(&request, error);
        }
        #[cfg(unix)]
        if let Command::Enrollment{call}=&request.command {
            if self.closing.load(Ordering::Acquire) && matches!(call,super::enrollment_rpc::Call::Start{..}) {
                return self.reply_error(&request,errors::DAEMON_STOPPING);
            }
            let result=self.runtime.enrollment(request.request_id.clone(),call.clone()).await;
            return Reply::ok(&request.request_id,self.store.revision(),serde_json::to_value(result).unwrap_or_default());
        }
        let owner = self.runtime.owner();
        if let Command::Hello = request.command {
            let hello = Hello {
                api_version: API_VERSION,
                dashboard_version: DASHBOARD_API_VERSION,
                owner_id: owner.as_ref().map(|o| o.user_id.clone()),
                server: owner.as_ref().map(|o| o.server.clone()),
                hosting: self.hosting_status().state,
            };
            return Reply::ok(
                &request.request_id,
                self.store.revision(),
                serde_json::to_value(hello).unwrap_or_default(),
            );
        }
        match (&owner, request.owner_id.as_deref()) {
            (None, _) if request.command.is_mutation() => {
                return self.reply_error(&request, errors::OWNER_NOT_ADOPTED);
            }
            (None, Some(_)) => return self.reply_error(&request, errors::OWNER_NOT_ADOPTED),
            (Some(_), None) => return self.reply_error(&request, errors::OWNER_SCOPE_REQUIRED),
            (Some(o), Some(id)) if o.user_id != id => {
                return self.reply_error(&request, errors::OWNER_SCOPE_MISMATCH);
            }
            _ => {}
        }
        if let Command::Snapshot { period, usage_filter } = &request.command {
            let generation = self.runtime.generation();
            let snapshot = self.snapshot(period.clone(),usage_filter.clone()).await;
            if self.runtime.generation() != generation || self.runtime.owner() != owner || snapshot.owner.as_ref().map(|o| (&o.user_id, &o.server)) != owner.as_ref().map(|o| (&o.user_id, &o.server)) {
                return self.reply_error(&request, errors::OWNER_SCOPE_MISMATCH);
            }
            return Reply::ok(
                &request.request_id,
                snapshot.revision,
                serde_json::to_value(snapshot).unwrap_or_default(),
            );
        }
        if let Command::Bridge { call } = &request.command {
            if self.closing.load(Ordering::Acquire) { return self.reply_error(&request, errors::DAEMON_STOPPING); }
            let result = self.runtime.bridge(request.request_id.clone(), call.clone()).await
                .map_err(super::bridge::Failure::from);
            return Reply::ok(&request.request_id, self.store.revision(), serde_json::to_value(result).unwrap_or_default());
        }
        if self.closing.load(Ordering::Acquire) {
            return self.reply_error(&request, errors::DAEMON_STOPPING);
        }
        self.mutate(request).await
    }

    async fn mutate(self: &Arc<Self>, request: Request) -> Reply {
        let fingerprint = request.fingerprint();
        match self.store.lookup(&request) {
            Lookup::Replay(reply) => return reply,
            Lookup::Conflict => return self.reply_error(&request, errors::REQUEST_ID_REUSED),
            Lookup::Unavailable => return self.reply_error(&request, errors::STATE_UNAVAILABLE),
            Lookup::New => {}
        }
        {
            let mut inflight = lock(&self.inflight);
            match inflight.get(&request.request_id) {
                Some(f) if *f == fingerprint => {
                    return self.reply_error(&request, errors::IN_PROGRESS);
                }
                Some(_) => return self.reply_error(&request, errors::REQUEST_ID_REUSED),
                None => {}
            }
            inflight.insert(request.request_id.clone(), fingerprint);
        }
        let reply = match self.mutation.try_lock() {
            Ok(_guard) => self.mutate_locked(&request).await,
            Err(_) => self.reply_error(&request, errors::BUSY),
        };
        lock(&self.inflight).remove(&request.request_id);
        reply
    }

    async fn mutate_locked(self: &Arc<Self>, request: &Request) -> Reply {
        // A receipt may have been committed by a request that held the lock.
        match self.store.lookup(request) {
            Lookup::Replay(reply) => return reply,
            Lookup::Conflict => return self.reply_error(request, errors::REQUEST_ID_REUSED),
            Lookup::Unavailable => return self.reply_error(request, errors::STATE_UNAVAILABLE),
            Lookup::New => {}
        }
        if request
            .expected_revision
            .is_some_and(|r| r != self.store.revision())
        {
            return self.reply_error(request, errors::REVISION_CONFLICT);
        }
        let result = match request.command {
            Command::PauseHosting => self.pause(request).await,
            Command::ResumeHosting => self.resume(request),
            Command::StopHosting => self.stop(request).await,
            Command::RefreshProviders => self.refresh(request).await,
            Command::Hello | Command::Snapshot { .. } => Err(errors::INVALID_REQUEST),
            Command::Bridge { .. } => Err(errors::INVALID_REQUEST),
            #[cfg(unix)]
            Command::Enrollment{..} => Err(errors::INVALID_REQUEST),
        };
        self.observe_transition();
        result.unwrap_or_else(|error| self.reply_error(request, error))
    }

    fn outcome(
        &self,
        request: &Request,
        intent: Option<Intent>,
        state: OutcomeState,
        detail: Option<&str>,
    ) -> Result<Reply, DaemonError> {
        // The recorded hosting value is computed after the intent applies.
        let runtime = self.runtime.clone();
        let drain = self.drain.clone();
        let owner = runtime.owner().is_some();
        let resulting = intent.unwrap_or_else(|| self.store.intent());
        self.store.commit(request, intent, |revision| {
            let drain = lock(&drain);
            let hosting = effective(
                owner,
                resulting,
                &runtime.observe(),
                drain.running,
                drain.failure,
            )
            .state;
            Reply::ok(
                &request.request_id,
                revision,
                serde_json::to_value(CommandOutcome {
                    state,
                    hosting,
                    revision,
                    detail: detail.map(Into::into),
                })
                .unwrap_or_default(),
            )
        })
    }

    async fn pause(&self, request: &Request) -> Result<Reply, DaemonError> {
        match self.store.intent() {
            Intent::Stopped => Err(errors::HOSTING_STOPPED),
            Intent::Paused => {
                self.outcome(request, Some(Intent::Paused), OutcomeState::Unchanged, None)
            }
            Intent::Hosting => {
                let quiescent = self.runtime.close_admission().await;
                let (state, detail) = if quiescent {
                    (OutcomeState::Completed, None)
                } else {
                    (
                        OutcomeState::Accepted,
                        Some("Admission is closed; an admission already in flight may still finish."),
                    )
                };
                let reply = self.outcome(request, Some(Intent::Paused), state, detail);
                if reply.is_err() {
                    self.runtime.open_admission();
                }
                reply
            }
        }
    }

    fn resume(&self, request: &Request) -> Result<Reply, DaemonError> {
        if lock(&self.drain).running {
            return Err(errors::BUSY);
        }
        let observed = self.runtime.observe();
        match self.store.intent() {
            Intent::Hosting => {
                if matches!(observed.phase, Phase::Failed | Phase::Offline | Phase::Idle) {
                    let reply = self.outcome(
                        request,
                        Some(Intent::Hosting),
                        OutcomeState::Accepted,
                        Some("Retrying hosting startup."),
                    )?;
                    self.runtime.start();
                    Ok(reply)
                } else {
                    self.outcome(request, Some(Intent::Hosting), OutcomeState::Unchanged, None)
                }
            }
            Intent::Paused => {
                let state = if observed.phase == Phase::Running {
                    OutcomeState::Completed
                } else {
                    OutcomeState::Accepted
                };
                let reply = self.outcome(request, Some(Intent::Hosting), state, None)?;
                self.runtime.open_admission();
                if matches!(observed.phase, Phase::Failed | Phase::Offline | Phase::Idle) {
                    self.runtime.start();
                }
                Ok(reply)
            }
            Intent::Stopped => {
                lock(&self.drain).failure = None;
                let reply =
                    self.outcome(request, Some(Intent::Hosting), OutcomeState::Accepted, None)?;
                self.runtime.open_admission();
                self.runtime.start();
                Ok(reply)
            }
        }
    }

    async fn stop(self: &Arc<Self>, request: &Request) -> Result<Reply, DaemonError> {
        if self.store.intent() == Intent::Stopped && lock(&self.drain).failure.is_none() {
            return self.outcome(request, Some(Intent::Stopped), OutcomeState::Unchanged, None);
        }
        let previous = self.store.intent();
        // Fence admission before the durable commit; drain follows it.
        let quiescent = self.runtime.close_admission().await;
        {
            let mut drain = lock(&self.drain);
            drain.running = true;
            drain.failure = None;
        }
        let reply = self.outcome(
            request,
            Some(Intent::Stopped),
            OutcomeState::Accepted,
            Some(if quiescent {
                "Draining; stopped is reported only after the drain completes."
            } else {
                "Draining; an admission already in flight may still finish."
            }),
        );
        if reply.is_err() {
            lock(&self.drain).running = false;
            if previous == Intent::Hosting {
                self.runtime.open_admission();
            }
            return reply;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let result = this.runtime.stop().await;
            {
                let mut drain = lock(&this.drain);
                drain.running = false;
                drain.failure = result.err();
            }
            this.observe_transition();
        });
        reply
    }

    async fn refresh(&self, request: &Request) -> Result<Reply, DaemonError> {
        if self.store.intent() == Intent::Stopped {
            return Err(errors::HOSTING_STOPPED);
        }
        self.runtime.refresh_providers().await?;
        self.outcome(request, None, OutcomeState::Accepted, Some("Refreshing provider reports; each account keeps its observation time."))
    }

    /// Daemon process exit: refuse new mutations, then suspend hosting without
    /// changing the saved intent or revoking owner enrollment.
    pub fn begin_shutdown(&self) {
        self.closing.store(true,Ordering::Release);
    }
    pub async fn shutdown(&self) -> Result<(), &'static str> {
        self.begin_shutdown();
        let _guard = self.mutation.lock().await;
        self.runtime.close_admission().await;
        self.runtime.stop().await
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::protocol::Period;
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Deterministic in-process runtime. It records calls and reports exactly
    /// what each test configures; it never simulates provider execution.
    pub(crate) struct Probe {
        pub owner: Mutex<Option<OwnerScope>>,
        pub phase: Mutex<Phase>,
        pub admission_open: AtomicBool,
        pub quiescent: AtomicBool,
        pub starts: AtomicUsize,
        pub stops: AtomicUsize,
        pub stop_gate: tokio::sync::Semaphore,
        pub stop_result: Mutex<Result<(), &'static str>>,
        pub refresh: Mutex<Result<(), DaemonError>>,
    }
    impl Probe {
        pub(crate) fn new(owner: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                owner: Mutex::new(owner.map(|id| OwnerScope {
                    user_id: id.into(),
                    server: "https://s.example/".into(),
                    display_name: "Owner".into(),
                })),
                phase: Mutex::new(Phase::Idle),
                admission_open: AtomicBool::new(false),
                quiescent: AtomicBool::new(true),
                starts: AtomicUsize::new(0),
                stops: AtomicUsize::new(0),
                stop_gate: tokio::sync::Semaphore::new(1000),
                stop_result: Mutex::new(Ok(())),
                refresh: Mutex::new(Err(errors::PROVIDER_REFRESH_UNAVAILABLE)),
            })
        }
    }
    impl Runtime for Probe {
        fn owner(&self) -> Option<OwnerScope> {
            lock(&self.owner).clone()
        }
        fn observe(&self) -> Observation {
            Observation {
                phase: *lock(&self.phase),
                detail: None,
                active_runs: Some(0),
                pending_approvals: None,
            }
        }
        fn close_admission(&self) -> BoxFuture<'_, bool> {
            self.admission_open.store(false, Ordering::Release);
            Box::pin(async move { self.quiescent.load(Ordering::Acquire) })
        }
        fn open_admission(&self) {
            self.admission_open.store(true, Ordering::Release);
        }
        fn start(&self) {
            self.starts.fetch_add(1, Ordering::AcqRel);
            *lock(&self.phase) = Phase::Running;
        }
        fn stop(&self) -> BoxFuture<'_, Result<(), &'static str>> {
            Box::pin(async move {
                self.stops.fetch_add(1, Ordering::AcqRel);
                *lock(&self.phase) = Phase::Draining;
                let _permit = self.stop_gate.acquire().await.unwrap();
                let result = *lock(&self.stop_result);
                *lock(&self.phase) = if result.is_ok() {
                    Phase::Stopped
                } else {
                    Phase::Failed
                };
                result
            })
        }
        fn snapshot_parts(&self, _period: Option<Period>, filter:Option<hagency_metering::dashboard::UsageFilter>) -> BoxFuture<'_, SnapshotParts> {
            Box::pin(async { SnapshotParts::default() })
        }
        fn refresh_providers(&self) -> BoxFuture<'_, Result<(), DaemonError>> {
            Box::pin(async move { lock(&self.refresh).clone() })
        }
    }

    pub(crate) fn controller(
        dir: &std::path::Path,
        probe: Arc<Probe>,
    ) -> Arc<Controller<Probe>> {
        Controller::new(ControlStore::open(&dir.join(super::super::CONTROL)).unwrap(), probe)
    }

    fn private_dir() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        temp
    }

    fn req(id: &str, command: Command) -> Request {
        Request::new(id, Some("@o:s".into()), command)
    }

    fn outcome(reply: &Reply) -> CommandOutcome {
        reply.outcome().unwrap_or_else(|e| panic!("{e:?}"))
    }

    async fn settle<R: Runtime>(c: &Controller<R>, expected: HostingState) {
        for _ in 0..200 {
            if c.hosting_status().state == expected {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        panic!("state never became {expected:?}: {:?}", c.hosting_status());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graceful_shutdown_refuses_new_enrollment_but_keeps_status_readable() {
        let temp=private_dir();
        let c=controller(temp.path(),Probe::new(Some("@o:s")));
        c.begin_shutdown();
        let start=req("new-enroll",Command::Enrollment{call:super::super::enrollment_rpc::Call::Start {
            origin:"https://s.example/".into(),owner:"@o:s".into(),display_name:None,
        }});
        assert_eq!(c.handle(start).await.error.unwrap().code,"daemon_stopping");
        assert!(c.handle(req("status",Command::Snapshot{period:None,usage_filter:None})).await.snapshot().is_ok());
        assert_eq!(c.handle(req("pause",Command::PauseHosting)).await.error.unwrap().code,"daemon_stopping");
    }

    #[tokio::test]
    async fn owner_scope_is_required_and_exact() {
        let temp = private_dir();
        let c = controller(temp.path(), Probe::new(Some("@o:s")));
        let mut unscoped = req("a", Command::Snapshot { period: None, usage_filter:None });
        unscoped.owner_id = None;
        assert_eq!(c.handle(unscoped).await.error.unwrap().code, "owner_scope_required");
        let mut foreign = req("b", Command::PauseHosting);
        foreign.owner_id = Some("@other:s".into());
        assert_eq!(c.handle(foreign).await.error.unwrap().code, "owner_scope_mismatch");
        let snapshot = c.handle(req("c", Command::Snapshot { period: None, usage_filter:None })).await;
        let snapshot = snapshot.snapshot().unwrap();
        assert_eq!(snapshot.owner.unwrap().user_id, "@o:s");
        // Hello needs no scope and exposes only the adopted identity.
        let mut hello = req("d", Command::Hello);
        hello.owner_id = None;
        let hello = c.handle(hello).await.hello().unwrap();
        assert_eq!(hello.owner_id.as_deref(), Some("@o:s"));
        assert_eq!(hello.api_version, 1);
    }

    #[tokio::test]
    async fn unadopted_daemon_reports_unconfigured_and_refuses_mutation() {
        let temp = private_dir();
        let c = controller(temp.path(), Probe::new(None));
        let mut read = req("a", Command::Snapshot { period: None, usage_filter:None });
        read.owner_id = None;
        let snapshot = c.handle(read).await.snapshot().unwrap();
        assert_eq!(snapshot.hosting.state, HostingState::Unconfigured);
        assert!(snapshot.owner.is_none());
        assert!(snapshot.usage.counts.input.is_none());
        let mut pause = req("b", Command::PauseHosting);
        pause.owner_id = None;
        assert_eq!(c.handle(pause).await.error.unwrap().code, "owner_not_adopted");
    }

    #[tokio::test]
    async fn duplicate_request_replays_original_reply_and_reuse_is_refused() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        let first = c.handle(req("p", Command::PauseHosting)).await;
        assert_eq!(outcome(&first).state, OutcomeState::Completed);
        assert_eq!(outcome(&first).hosting, HostingState::Paused);
        // The retried request returns the original reply, even after more
        // changes have advanced the revision.
        c.handle(req("r", Command::ResumeHosting)).await;
        let again = c.handle(req("p", Command::PauseHosting)).await;
        assert_eq!(again, first);
        assert!(probe.admission_open.load(Ordering::Acquire));
        let reused = c.handle(req("p", Command::StopHosting)).await;
        assert_eq!(reused.error.unwrap().code, "request_id_reused");
        assert_eq!(probe.stops.load(Ordering::Acquire), 0);
        // Receipts survive daemon restart.
        drop(c);
        let c = controller(temp.path(), probe.clone());
        assert_eq!(c.handle(req("p", Command::PauseHosting)).await, first);
    }

    #[tokio::test]
    async fn stale_expected_revision_conflicts_without_effect() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        let current = c.revision();
        let mut stale = req("x", Command::PauseHosting);
        stale.expected_revision = Some(current - 1);
        assert_eq!(c.handle(stale).await.error.unwrap().code, "revision_conflict");
        assert!(probe.admission_open.load(Ordering::Acquire));
        let mut fresh = req("y", Command::PauseHosting);
        fresh.expected_revision = Some(current);
        assert_eq!(outcome(&c.handle(fresh).await).hosting, HostingState::Paused);
    }

    #[tokio::test]
    async fn pause_resume_stop_lifecycle_reports_only_confirmed_transitions() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        assert_eq!(c.hosting_status().state, HostingState::Running);
        assert!(probe.admission_open.load(Ordering::Acquire));

        // Pause without quiescence is accepted, never claimed complete.
        probe.quiescent.store(false, Ordering::Release);
        let paused = outcome(&c.handle(req("1", Command::PauseHosting)).await);
        assert_eq!(paused.state, OutcomeState::Accepted);
        assert!(paused.detail.is_some());
        assert!(!probe.admission_open.load(Ordering::Acquire));
        let unchanged = outcome(&c.handle(req("2", Command::PauseHosting)).await);
        assert_eq!(unchanged.state, OutcomeState::Unchanged);

        let resumed = outcome(&c.handle(req("3", Command::ResumeHosting)).await);
        assert_eq!((resumed.state, resumed.hosting), (OutcomeState::Completed, HostingState::Running));
        assert!(probe.admission_open.load(Ordering::Acquire));

        // Stop is accepted as draining and is stopped only once drained.
        let permits = probe.stop_gate.available_permits();
        probe.stop_gate.forget_permits(permits);
        let stopping = outcome(&c.handle(req("4", Command::StopHosting)).await);
        assert_eq!((stopping.state, stopping.hosting), (OutcomeState::Accepted, HostingState::Draining));
        assert!(!probe.admission_open.load(Ordering::Acquire));
        settle(&c, HostingState::Draining).await;
        assert_eq!(c.handle(req("5", Command::ResumeHosting)).await.error.unwrap().code, "busy");
        assert_eq!(c.handle(req("6", Command::PauseHosting)).await.error.unwrap().code, "hosting_stopped");
        probe.stop_gate.add_permits(1);
        settle(&c, HostingState::Stopped).await;
        let again = outcome(&c.handle(req("7", Command::StopHosting)).await);
        assert_eq!((again.state, again.hosting), (OutcomeState::Unchanged, HostingState::Stopped));
        assert_eq!(probe.stops.load(Ordering::Acquire), 1);

        // A stopped intent survives restart and does not reopen the owner.
        drop(c);
        let starts = probe.starts.load(Ordering::Acquire);
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        assert_eq!(probe.starts.load(Ordering::Acquire), starts);
        assert_eq!(c.hosting_status().state, HostingState::Stopped);
        let restarted = outcome(&c.handle(req("8", Command::ResumeHosting)).await);
        assert_eq!(restarted.state, OutcomeState::Accepted);
        assert_eq!(probe.starts.load(Ordering::Acquire), starts + 1);
        settle(&c, HostingState::Running).await;
    }

    #[tokio::test]
    async fn failed_drain_is_reported_as_failure_not_stopped() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        *lock(&probe.stop_result) = Err("management_drain_outcome_unknown");
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        c.handle(req("s", Command::StopHosting)).await;
        settle(&c, HostingState::Failed).await;
        assert_eq!(
            c.hosting_status().detail.as_deref(),
            Some("management_drain_outcome_unknown")
        );
        // Retrying stop is a new attempt, not an unchanged success.
        *lock(&probe.stop_result) = Ok(());
        let retry = outcome(&c.handle(req("s2", Command::StopHosting)).await);
        assert_eq!(retry.state, OutcomeState::Accepted);
        settle(&c, HostingState::Stopped).await;
    }

    #[tokio::test]
    async fn concurrent_same_id_is_in_progress_and_other_mutation_is_busy() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        let guard = c.mutation.lock().await;
        lock(&c.inflight).insert("held".into(), req("held", Command::PauseHosting).fingerprint());
        assert_eq!(c.handle(req("held", Command::PauseHosting)).await.error.unwrap().code, "in_progress");
        assert_eq!(c.handle(req("held", Command::StopHosting)).await.error.unwrap().code, "request_id_reused");
        assert_eq!(c.handle(req("other", Command::StopHosting)).await.error.unwrap().code, "busy");
        drop(guard);
        lock(&c.inflight).clear();
        // A busy refusal recorded nothing: the same ID may run later.
        assert_eq!(outcome(&c.handle(req("other", Command::StopHosting)).await).state, OutcomeState::Accepted);
    }

    #[tokio::test]
    async fn provider_refresh_failure_is_reported_not_faked() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        let reply = c.handle(req("r", Command::RefreshProviders)).await;
        assert_eq!(reply.error.unwrap().code, "provider_refresh_unavailable");
        *lock(&probe.refresh) = Ok(());
        let ok = outcome(&c.handle(req("r", Command::RefreshProviders)).await);
        assert_eq!(ok.state, OutcomeState::Accepted,"provider probes finish asynchronously; acceptance is not fresh data");
    }

    #[tokio::test]
    async fn unknown_counts_stay_unknown_and_transitions_advance_revision() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        let c = controller(temp.path(), probe.clone());
        let status = c.observe_transition();
        assert_eq!(status.state, HostingState::Starting);
        assert_eq!(status.active_runs, Some(0));
        assert_eq!(status.pending_approvals, None);
        let json = serde_json::to_value(&status).unwrap();
        assert!(json["pendingApprovals"].is_null());
        let before = c.revision();
        c.boot().await;
        assert_eq!(c.hosting_status().state, HostingState::Running);
        assert!(c.revision() > before);
    }

    #[tokio::test]
    async fn shutdown_refuses_new_mutations_and_preserves_intent() {
        let temp = private_dir();
        let probe = Probe::new(Some("@o:s"));
        let c = controller(temp.path(), probe.clone());
        c.boot().await;
        c.shutdown().await.unwrap();
        assert_eq!(c.handle(req("x", Command::PauseHosting)).await.error.unwrap().code, "daemon_stopping");
        assert_eq!(c.store.intent(), Intent::Hosting);
    }
}
