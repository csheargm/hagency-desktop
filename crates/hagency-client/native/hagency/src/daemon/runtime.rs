//! The daemon's hosting engine for one enrolled owner.
//!
//! Lifecycle ported from Hagency Desktop's `backend::Service`: register the
//! native owner in a synchronous retirement slot before authorization starts,
//! open the current profile through SDK authorization,
//! restore current personal runtime intents, then start the allocation and shared
//! workers. Stop retires and drains workers before suspending the native
//! owner, which releases the owner-root lock. Ordinary stop never revokes the
//! native resumption; only explicit sign-out does.
use super::{
    auth::{AuthError, AuthState, SdkCustody},
    control::now_ms,
    dispatch::{BoxFuture, Observation, OwnerScope, Phase, Runtime, SnapshotParts},
    enrollment::Enrollment,
    protocol::{DaemonError, Period, errors},
    workers::{Admission, Worker},
};
use crate::native_owner::{NativeError, NativeOwner, OwnerTelemetry};
use hagency_metering::dashboard::{collector::SnapshotOptions, period::UsagePeriod};
use hagency_metering::dashboard::{Activity, ActivityState, ResourceState, ResourceUsage, UNKNOWN_COUNTS};
use serde_json::Value;
use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const MAX_EVENTS: usize = 64;
const ADMISSION_BARRIER: Duration = Duration::from_secs(15);
const MONITOR_PERIOD: Duration = Duration::from_secs(15);
const READ_PERIOD: Duration = Duration::from_secs(60);
const MAX_PAGES: usize = 8;
const PROVIDER_PERIOD: Duration = Duration::from_secs(300);
/// Every external wait on the provider report path is bounded. The probe
/// bounds its own RPCs; these bound authorization, ledger reads and child
/// cleanup around it, so a stalled read reports itself as unavailable.
const PROVIDER_SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_PAGE_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_REPORT_TIMEOUT: Duration = Duration::from_secs(75);
/// Below the refresh period, so a stalled job never delays the next one.
const PROVIDER_REFRESH_BUDGET: Duration = Duration::from_secs(240);
/// Backstop for a job that cannot even observe its own budget.
const PROVIDER_REFRESH_STALL: Duration = Duration::from_secs(270);
/// Stop waits this long for a refresh before cancelling it. The probe's child
/// stays in provider task custody, which the owner drain still observes.
const PROVIDER_REFRESH_DRAIN: Duration = Duration::from_secs(10);
const DASHBOARD_READ_TIMEOUT: Duration = Duration::from_secs(120);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Codes that resolve without owner action. A root still locked by another
/// process appears as `native_owner_state_unavailable`.
pub(crate) fn retryable(code: &str) -> bool {
    code.ends_with("_unavailable")
        || matches!(code, "server_unavailable" | "source_root_changed" | "native_owner_drain_required")
}

#[derive(Default)]
struct Reads {
    inventory_available:bool,
    inventory_complete:bool,
    pending_approvals: Option<u64>,
    resources: Option<Vec<ResourceUsage>>,
}

impl Reads {
    fn refresh(&mut self,mut fresh:Self) {
        if fresh.resources.is_none() {fresh.resources=self.resources.clone();}
        *self=fresh;
    }
}

/// One in-flight provider report refresh. The handle stays here until the
/// task is observed finished, so a cancelled stop cannot detach it.
struct ProviderRefresh {
    task: tokio::task::JoinHandle<()>,
    started: tokio::time::Instant,
    stalled: bool,
}

impl ProviderRefresh {
    fn running(&self) -> bool {
        !self.task.is_finished()
    }
    /// True once, when a running job has outlived its own budget without
    /// reporting: its task is not making progress.
    fn newly_stalled(&mut self, now: tokio::time::Instant) -> bool {
        if self.stalled || !self.running() || now.duration_since(self.started) < PROVIDER_REFRESH_STALL {
            return false;
        }
        self.stalled = true;
        true
    }
}

/// Bounds for one refresh; tests use shorter ones.
#[derive(Clone, Copy)]
struct RefreshBounds {
    page: Duration,
    report: Duration,
    budget: Duration,
}

const REFRESH_BOUNDS: RefreshBounds = RefreshBounds {
    page: PROVIDER_PAGE_TIMEOUT,
    report: PROVIDER_REPORT_TIMEOUT,
    budget: PROVIDER_REFRESH_BUDGET,
};

#[derive(Debug, Default, PartialEq, Eq)]
struct ReportTally {
    observed: usize,
    failed: usize,
    complete: bool,
}

/// The two reads a provider refresh performs. Implemented by the native
/// manager; tests substitute stalled or failing readers.
trait ProviderReports: Send + Sync {
    type Resource: Send;
    fn page(&self, after: String) -> impl std::future::Future<Output = Result<(Vec<Self::Resource>, Option<String>), NativeError>> + Send;
    fn reportable(resource: &Self::Resource) -> bool;
    fn report(&self, resource: &Self::Resource) -> impl std::future::Future<Output = Result<(), NativeError>> + Send;
}

/// Owns the refresh's read-only management package and retires it on every
/// exit, including cancellation.
struct ProviderReader(Arc<crate::native_owner::NativeManagement>);

impl Drop for ProviderReader {
    fn drop(&mut self) {
        self.0.retire();
    }
}

impl ProviderReports for ProviderReader {
    type Resource = hagency_store::management::ResourceSummary;
    async fn page(&self, after: String) -> Result<(Vec<Self::Resource>, Option<String>), NativeError> {
        let page = self.0.resources(hagency_store::management::ResourceQuery { after, ..Default::default() }).await?;
        Ok((page.resources, page.next_after))
    }
    fn reportable(resource: &Self::Resource) -> bool {
        resource.framework == "codex"
    }
    async fn report(&self, resource: &Self::Resource) -> Result<(), NativeError> {
        self.0.refresh_resource_quota(resource).await
    }
}

/// Reads every page and reports each supported resource, each step bounded.
/// `None` means hosting was retired while reading: nothing is reported.
async fn read_provider_reports<P: ProviderReports>(reports: &P, retired: &AtomicBool, bounds: RefreshBounds) -> Option<ReportTally> {
    let mut tally = ReportTally::default();
    let mut after = String::new();
    for _ in 0..MAX_PAGES {
        if retired.load(Ordering::Acquire) {
            return None;
        }
        let (resources, next) = match tokio::time::timeout(bounds.page, reports.page(after)).await {
            Ok(Ok(page)) => page,
            Ok(Err(_)) => {
                tally.failed += 1;
                break;
            }
            Err(_) => {
                tracing::info!(code = "provider_inventory_timeout", "Provider report inventory unavailable");
                tally.failed += 1;
                break;
            }
        };
        for resource in resources {
            if retired.load(Ordering::Acquire) {
                return None;
            }
            if !P::reportable(&resource) {
                continue;
            }
            match tokio::time::timeout(bounds.report, reports.report(&resource)).await {
                Ok(Ok(())) => tally.observed += 1,
                Ok(Err(error)) => {
                    tally.failed += 1;
                    tracing::info!(code = %error.code, "Provider report unavailable");
                }
                Err(_) => {
                    tally.failed += 1;
                    tracing::info!(code = "provider_report_timeout", "Provider report unavailable");
                }
            }
        }
        match next {
            Some(next) => after = next,
            None => {
                tally.complete = true;
                break;
            }
        }
    }
    if !tally.complete {
        tally.failed += 1;
    }
    Some(tally)
}

/// The spawned refresh. It always ends in a finished activity unless hosting
/// was retired or replaced meanwhile.
async fn provider_refresh_job<P: ProviderReports>(
    reports: P,
    retired: Arc<AtomicBool>,
    inner: std::sync::Weak<Inner>,
    epoch: u64,
    bounds: RefreshBounds,
) {
    let outcome = tokio::time::timeout(bounds.budget, read_provider_reports(&reports, &retired, bounds)).await;
    drop(reports);
    let Some(inner) = inner.upgrade() else { return };
    if retired.load(Ordering::Acquire) || inner.generation.load(Ordering::Acquire) != epoch {
        return;
    }
    match outcome {
        Ok(None) => {}
        Ok(Some(tally)) => {
            tracing::info!(observed = tally.observed, failed = tally.failed, "Provider report refresh finished");
            if tally.observed > 0 {
                inner.event(
                    if tally.failed == 0 { ActivityState::Succeeded } else { ActivityState::Warning },
                    "Provider reports refreshed",
                    &format!("{} resource reports updated. {} unavailable; previous reports keep their observation times.", tally.observed, tally.failed),
                );
            } else {
                inner.event(ActivityState::Warning, "Provider reports unavailable", "No current supported provider report was available. Existing reports keep their observation times.");
            }
        }
        Err(_) => {
            tracing::warn!(code = "provider_refresh_timeout", "Provider report refresh did not finish within its budget");
            inner.event(ActivityState::Warning, "Provider reports unavailable", "The provider report refresh did not finish in time and was stopped. Existing reports keep their observation times.");
        }
    }
}

/// Waits for a task without detaching it: on timeout the handle stays usable.
async fn join_within(task: &mut tokio::task::JoinHandle<()>, within: Duration) -> bool {
    tokio::time::timeout(within, task).await.is_ok()
}

struct Hosted {
    custody: Arc<SdkCustody>,
    owner: NativeOwner,
    retired: Arc<AtomicBool>,
    adopted: bool,
    allocation: Arc<Worker>,
    shared: Arc<Worker>,
    #[cfg(feature="resource-adoption")]
    executor_slot:Arc<tokio::sync::Mutex<Option<Arc<crate::native_owner::CurrentSharedExecutor>>>>,
    monitor: tokio::task::JoinHandle<()>,
}

pub struct OwnerRuntime {
    inner: Arc<Inner>,
}

struct Inner {
    enrollment_flow:Mutex<super::enrollment_rpc::Flow>,
    logout:tokio::sync::Mutex<()>,
    bridge: super::bridge::Bridge,
    home: PathBuf,
    enrollment: Mutex<Option<Enrollment>>,
    admission: Arc<Admission>,
    phase: Mutex<(Phase, Option<String>)>,
    hosted: tokio::sync::Mutex<Option<Hosted>>,
    /// Native owner registered by the open guard but not yet published.
    registered: Mutex<Option<NativeOwner>>,
    opening_custody:Mutex<Option<Arc<SdkCustody>>>,
    generation: AtomicU64,
    stopping: AtomicBool,
    starting: Mutex<Option<tokio::task::JoinHandle<()>>>,
    events: Mutex<VecDeque<Activity>>,
    reads: Mutex<Reads>,
    telemetry: Mutex<Option<OwnerTelemetry>>,
    /// Held only briefly, never across provider or management IO: stop needs it.
    provider_refresh:tokio::sync::Mutex<Option<ProviderRefresh>>,
    health: Arc<Mutex<Option<String>>>,
}

enum OpenError {
    Retry(String),
    Terminal(String),
}

impl OwnerRuntime {
    /// `home` is the locked daemon home. Reads the enrollment journal; opens
    /// nothing until [`Runtime::start`].
    pub fn new(home: PathBuf) -> Result<Self, &'static str> {
        let enrollment = Enrollment::load(&home).map_err(|e| e.code())?;
        Ok(Self {
            inner: Arc::new(Inner {
                enrollment_flow:Mutex::new(super::enrollment_rpc::Flow::default()),
                logout:tokio::sync::Mutex::new(()),
                bridge: super::bridge::Bridge::open(&home).map_err(|_| "daemon_bridge_journal_unavailable")?,
                home: home.clone(),
                enrollment: Mutex::new(enrollment),
                admission: Arc::new(Admission::with_fence({ let home = home.clone(); move || super::revocation::load(&home).is_ok_and(|r| r.is_none()) })),
                phase: Mutex::new((Phase::Idle, None)),
                hosted: tokio::sync::Mutex::new(None),
                registered: Mutex::new(None),
                opening_custody:Mutex::new(None),
                generation: AtomicU64::new(0),
                stopping: AtomicBool::new(false),
                starting: Mutex::new(None),
                events: Mutex::new(VecDeque::new()),
                reads: Mutex::new(Reads::default()),
                telemetry: Mutex::new(None),
                provider_refresh:tokio::sync::Mutex::new(None),
                health: Arc::new(Mutex::new(None)),
            }),
        })
    }

    /// Explicit owner sign-out: durably revoke native resumption, fence and
    /// drain hosting, revoke the daemon SDK grant and withdraw enrollment.
    /// Preserves the existing revocation semantics of Desktop's logout.
    pub async fn process_revocation(&self)->Result<(), &'static str>{
        let Some(request)=super::revocation::load(&self.inner.home)? else{return Ok(())};
        if let Some(enrolled)=lock(&self.inner.enrollment).as_ref() {
            if enrolled.owner!=request.owner || !super::same_origin(&enrolled.origin,&request.origin) || enrolled.owner_root!=request.root {return Err("daemon_owner_scope_mismatch")}
        }
        self.sign_out().await.map_err(|code| {
            self.inner.set_phase(Phase::Failed, Some(code.into()));
            code
        })
    }
    /// Close enrollment admission and wait its original accepted task. A
    /// cancelled shutdown waiter cannot discard the retained native handle.
    pub async fn drain_enrollment(&self,revoke:bool)->Result<(), &'static str> {
        let workflow=lock(&self.inner.enrollment_flow).close();
        if let Some(workflow)=workflow {workflow.drain(revoke).await?;}
        Ok(())
    }
    pub async fn sign_out(&self) -> Result<(), &'static str> {
        let inner = &self.inner;
        let _logout=inner.logout.lock().await;
        // Enrollment may have reached its durable journal before publishing
        // the new runtime scope. Fence that scope before waiting any workflow.
        if let Some(enrolled)=Enrollment::load(&inner.home).map_err(|e|e.code())? {
            super::revocation::request(&inner.home,&enrolled.origin,&enrolled.owner)?;
        }
        self.drain_enrollment(true).await?;
        let enrollment=Enrollment::load(&inner.home).map_err(|e|e.code())?;
        *lock(&inner.enrollment)=enrollment.clone();
        let Some(enrollment)=enrollment else {
            *lock(&inner.enrollment_flow)=super::enrollment_rpc::Flow::default();
            return super::revocation::finish(&inner.home)
        };
        super::revocation::request(&inner.home,&enrollment.origin,&enrollment.owner)?;
        inner.bridge.revoke_all();
        inner.generation.fetch_add(1, Ordering::AcqRel);
        inner.stopping.store(true, Ordering::Release);

        let starting = lock(&inner.starting).take();
        if let Some(task) = starting {
            task.abort();
            let _ = task.await;
        }
        let mut hosted_slot=inner.hosted.lock().await;
        let registered = lock(&inner.registered).clone();
        // Keep original handles in custody slots until drain succeeds. IPC
        // connection cancellation must never discard an accepted owner drain.
        let custody = match (hosted_slot.as_ref(), registered) {
            (Some(hosted), _) => {
                let custody = hosted.custody.clone();
                inner.close(hosted, true).await?;
                hosted_slot.take();
                Some(custody)
            }
            (None, Some(owner)) => {
                owner.shutdown_checked().await.map_err(|error|static_code(&error.code))?;
                lock(&inner.registered).take();
                lock(&inner.opening_custody).take()
            }
            (None, None) => {
                NativeOwner::revoke_saved_matrix_resumption(&enrollment.owner_root, &enrollment.origin, &enrollment.owner)
                    .map_err(|_| "native_logout_persistence_failed")?;
                lock(&inner.opening_custody).take()
            }
        };
        drop(hosted_slot);
        let custody = match custody {
            Some(custody) => Some(custody),
            None => match SdkCustody::restore(&inner.home.join(super::MATRIX), &enrollment.origin, &enrollment.owner).await {
                Ok(custody) => Some(custody),
                Err(AuthError::NotEnrolled) => None,
                Err(error) => return Err(error.code()),
            },
        };
        if let Some(custody) = custody {
            custody.sign_out(&inner.home.join(super::MATRIX)).await.map_err(AuthError::code)?;
        }
        Enrollment::withdraw(&inner.home).map_err(|e| e.code())?;
        *lock(&inner.reads) = Reads::default();
        *lock(&inner.telemetry) = None;
        lock(&inner.events).clear();
        *lock(&inner.enrollment) = None;
        inner.stopping.store(false, Ordering::Release);
        inner.set_phase(Phase::Idle, None);
        inner.event(ActivityState::Succeeded, "Owner signed out", "Daemon enrollment withdrawn; owner credentials revoked.");
        super::revocation::finish(&inner.home)?;
        *lock(&inner.enrollment_flow)=super::enrollment_rpc::Flow::default();
        Ok(())
    }
}

impl Inner {
    fn set_phase(&self, phase: Phase, detail: Option<String>) {
        *lock(&self.phase) = (phase, detail);
    }

    fn event(&self, state: ActivityState, title: &str, detail: &str) {
        let mut events = lock(&self.events);
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let at = now_ms();
        events.push_back(Activity {
            id: format!("daemon-{at}-{}", SEQ.fetch_add(1, Ordering::Relaxed)),
            at_ms: at,
            title: title.into(),
            detail: detail.into(),
            state: state.as_str().into(),
        });
        while events.len() > MAX_EVENTS {
            events.pop_front();
        }
    }

    /// Starts one bounded refresh unless one is running. Neither `hosted` nor
    /// the refresh slot is held across management IO: stop, bridge calls and
    /// hosting start need `hosted`, and stop needs the slot.
    async fn refresh_provider_reports(self:&Arc<Self>)->Result<(),DaemonError> {
        let (owner,retired)={
            let hosted=self.hosted.lock().await;
            let current=hosted.as_ref().filter(|h|!h.retired.load(Ordering::Acquire))
                .ok_or(errors::PROVIDER_REFRESH_UNAVAILABLE)?;
            (current.owner.clone(),current.retired.clone())
        };
        if self.provider_refresh_busy().await {return Ok(());}
        let epoch=self.generation.load(Ordering::Acquire);
        let mut package=read_package(epoch);
        package.permissions.push(crate::native_owner::ManagementPermission::ReadProviders);
        let manager=match tokio::time::timeout(PROVIDER_SETUP_TIMEOUT,owner.management(package)).await {
            Ok(Ok(manager))=>ProviderReader(manager),
            Ok(Err(_))=>return Err(errors::PROVIDER_REFRESH_UNAVAILABLE),
            Err(_)=>{
                tracing::info!(code="provider_refresh_setup_timeout","Provider report refresh could not start");
                return Err(errors::PROVIDER_REFRESH_UNAVAILABLE);
            }
        };
        let mut slot=self.provider_refresh.lock().await;
        // Stop fences `retired` before it takes this slot, so a refresh is
        // never installed behind a stop that already drained the slot.
        if retired.load(Ordering::Acquire) || self.generation.load(Ordering::Acquire)!=epoch {
            return Err(errors::PROVIDER_REFRESH_UNAVAILABLE);
        }
        if slot.as_ref().is_some_and(ProviderRefresh::running) {return Ok(());}
        self.event(ActivityState::Running,"Refreshing provider reports","Reading current reports from the original coding accounts.");
        *slot=Some(ProviderRefresh {
            task: tokio::spawn(provider_refresh_job(manager,retired,Arc::downgrade(self),epoch,REFRESH_BOUNDS)),
            started: tokio::time::Instant::now(),
            stalled: false,
        });
        Ok(())
    }

    /// Whether a refresh is still running. A job that outlived its budget
    /// without finishing is reported once and cancelled; its probe child stays
    /// in provider task custody.
    async fn provider_refresh_busy(&self)->bool {
        let mut slot=self.provider_refresh.lock().await;
        let Some(refresh)=slot.as_mut() else {return false};
        if refresh.newly_stalled(tokio::time::Instant::now()) {
            refresh.task.abort();
            tracing::warn!(code="provider_refresh_stalled","Provider report refresh stopped making progress");
            self.event(ActivityState::Failed,"Provider reports unavailable","The provider report refresh stopped making progress and was cancelled. Existing reports keep their observation times.");
        }
        refresh.running()
    }

    /// Bounded wait for the in-flight refresh, then cancellation. The handle
    /// is released only after its task is observed finished.
    async fn settle_provider_refresh(&self)->Result<(), &'static str> {
        self.settle_provider_refresh_within(PROVIDER_REFRESH_DRAIN).await
    }

    async fn settle_provider_refresh_within(&self,within:Duration)->Result<(), &'static str> {
        let mut slot=self.provider_refresh.lock().await;
        if let Some(refresh)=slot.as_mut() && !join_within(&mut refresh.task,within).await {
            refresh.task.abort();
            if !join_within(&mut refresh.task,within).await {
                tracing::warn!(code="provider_refresh_drain_unknown","Provider report refresh did not stop");
                return Err("provider_refresh_drain_unknown");
            }
        }
        slot.take();
        Ok(())
    }

    async fn run_start(self: Arc<Self>) {
        let mut backoff = Duration::from_secs(5);
        loop {
            if self.stopping.load(Ordering::Acquire) {
                return;
            }
            match self.open_once().await {
                Ok(()) => return,
                Err(OpenError::Terminal(code)) => {
                    tracing::warn!(code = %code, "Owner hosting startup failed");
                    self.event(ActivityState::Failed, "Hosting could not start", &code);
                    self.set_phase(Phase::Failed, Some(code));
                    return;
                }
                Err(OpenError::Retry(code)) => {
                    tracing::info!(code = %code, "Owner hosting startup will retry");
                    self.set_phase(Phase::Offline, Some(code));
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(60));
                }
            }
        }
    }

    async fn open_once(self: &Arc<Self>) -> Result<(), OpenError> {
        if super::revocation::load(&self.home).map_err(|e|OpenError::Terminal(e.into()))?.is_some(){return Err(OpenError::Terminal("daemon_logout_pending".into()))}
        // Reload: the journal may have changed while stopped.
        let enrollment = Enrollment::load(&self.home).map_err(|e| OpenError::Terminal(e.code().into()))?;
        let previous = lock(&self.enrollment).clone();
        if previous.as_ref().map(|e| (&e.owner, &e.origin)) != enrollment.as_ref().map(|e| (&e.owner, &e.origin)) {
            self.generation.fetch_add(1, Ordering::AcqRel);
            *lock(&self.reads) = Reads::default();
            *lock(&self.telemetry) = None;
            lock(&self.events).clear();
        }
        *lock(&self.enrollment) = enrollment.clone();
        let Some(enrollment) = enrollment else {
            self.set_phase(Phase::Idle, None);
            return Ok(());
        };
        if !enrollment.hostable() {
            self.set_phase(Phase::Stopped, Some("daemon_enrollment_incomplete".into()));
            return Ok(());
        }
        let mut hosted = self.hosted.lock().await;
        if hosted.is_some() {
            if lock(&self.phase).0 == Phase::Failed {
                return Err(OpenError::Terminal("native_owner_drain_required".into()));
            }
            return Ok(());
        }
        if lock(&self.registered).is_some() {
            self.cleanup_registered().await;
            if lock(&self.registered).is_some() {return Err(OpenError::Terminal("native_owner_drain_required".into()));}
        }
        self.set_phase(Phase::Starting, None);
        let custody = SdkCustody::restore(&self.home.join(super::MATRIX), &enrollment.origin, &enrollment.owner)
            .await
            .map_err(|e| match e {
                AuthError::Unavailable => OpenError::Retry(e.code().into()),
                _ => OpenError::Terminal(e.code().into()),
            })?;
        custody.fence_on_logout(&self.home);
        *lock(&self.opening_custody)=Some(custody.clone());
        let epoch = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let retired = Arc::new(AtomicBool::new(false));
        let owner = match self.open_owner(&enrollment, &custody).await {
            Ok(opened) => opened,
            Err(error) => {
                self.cleanup_registered().await;
                return Err(if retryable(&error.code) {
                    OpenError::Retry(error.code)
                } else {
                    OpenError::Terminal(error.code)
                });
            }
        };
        let (owner, selection) = owner;
        owner.set_hosting_admission(self.admission.clone());
        let telemetry = owner.telemetry();
        let handle = telemetry.clone();
        let directory = telemetry_directory(&self.home, &enrollment);
        if !tokio::task::spawn_blocking(move || handle.attach_store(&directory)).await.is_ok_and(|r| r.is_ok()) {
            self.event(ActivityState::Warning, "Usage history unavailable", "telemetry_storage_unavailable");
        }
        *lock(&self.telemetry) = Some(telemetry);
        if self.stopping.load(Ordering::Acquire) {
            if owner.suspend().await.is_ok() {
                custody.retire();
                lock(&self.registered).take();
                lock(&self.opening_custody).take();
                return Err(OpenError::Terminal("daemon_stopping".into()));
            }
            return Err(OpenError::Terminal("native_owner_drain_required".into()));
        }
        let adopted = selection.is_some();
        if !adopted {
            // Saved explicit consent is not authority: every restore re-enters
            // the normal start controller with fresh gates.
            match owner.restore_runtime_intents().await {
                Ok(value) => {
                    let total = value["recoveries"].as_array().map_or(0, Vec::len);
                    tracing::info!(total, "Agent startup recovery evaluated");
                }
                Err(error) => tracing::warn!(code = %error.code, "Agent startup recovery unavailable"),
            }
        }
        let gate: Arc<dyn Fn() -> bool + Send + Sync> = {
            let custody = Arc::downgrade(&custody);
            let retired = retired.clone();
            Arc::new(move || {
                !retired.load(Ordering::Acquire) && custody.upgrade().is_some_and(|c| c.live())
            })
        };
        let allocation = Arc::new(Worker::default());
        let shared = Arc::new(Worker::default());
        #[cfg(feature="resource-adoption")]
        let executor_slot=Arc::new(tokio::sync::Mutex::new(None));
        #[cfg(feature = "resource-adoption")]
        if let Some(selection) = selection {
            use super::workers::native;
            allocation
                .install(tokio::spawn(native::run_allocation(
                    owner.clone(),
                    selection.clone(),
                    allocation.control.clone(),
                    gate.clone(),
                    epoch,
                    self.health.clone(),
                )))
                .await;
            #[cfg(feature = "shared-execution")]
            shared
                .install(tokio::spawn(native::run_shared(
                    owner.clone(),
                    selection,
                    shared.control.clone(),
                    gate.clone(),
                    self.admission.clone(),
                    epoch,
                    self.health.clone(),
                    executor_slot.clone(),
                )))
                .await;
        }
        #[cfg(not(feature = "resource-adoption"))]
        let _ = (selection, &gate);
        let monitor = tokio::spawn(monitor(Arc::downgrade(self), custody.clone(), owner.clone(), retired.clone(), epoch));
        *hosted = Some(Hosted {
            custody,
            owner,
            retired,
            adopted,
            allocation,
            shared,
            #[cfg(feature="resource-adoption")]
            executor_slot,
            monitor,
        });
        lock(&self.registered).take();
        lock(&self.opening_custody).take();
        drop(hosted);
        self.set_phase(
            Phase::Running,
            (!adopted).then(|| "No pooled resources are activated on this device.".into()),
        );
        self.event(ActivityState::Succeeded, "Hosting started", if adopted { "Pool workers started." } else { "Owner authorized." });
        Ok(())
    }

    #[cfg(feature = "resource-adoption")]
    async fn open_owner(
        &self,
        enrollment: &Enrollment,
        custody: &Arc<SdkCustody>,
    ) -> Result<(NativeOwner, Option<crate::native_owner::CurrentProfileSelection>), NativeError> {
        use crate::native_owner::CurrentProfileSelection;
        let root = &enrollment.owner_root;
        let selection = if root.exists() {
            CurrentProfileSelection::load_for_host(root, &enrollment.origin, &enrollment.owner).await?
        } else {
            None
        };
        let guard = |owner: &NativeOwner| self.register(owner);
        let owner = match &selection {
            Some(selection) => {
                selection
                    .open_with_matrix_guarded(root, &enrollment.origin, &enrollment.owner, custody.tokens(), guard)
                    .await?
            }
            None => {
                NativeOwner::open_with_matrix_guarded(root, &enrollment.origin, &enrollment.owner, custody.tokens(), guard)
                    .await?
            }
        };
        Ok((owner, selection))
    }

    #[cfg(not(feature = "resource-adoption"))]
    async fn open_owner(
        &self,
        enrollment: &Enrollment,
        custody: &Arc<SdkCustody>,
    ) -> Result<(NativeOwner, Option<()>), NativeError> {
        let guard = |owner: &NativeOwner| self.register(owner);
        let owner = NativeOwner::open_with_matrix_guarded(
            &enrollment.owner_root,
            &enrollment.origin,
            &enrollment.owner,
            custody.tokens(),
            guard,
        )
        .await?;
        Ok((owner, None))
    }

    /// Synchronous retirement slot, filled before SDK authorization starts.
    fn register(&self, owner: &NativeOwner) -> Result<(), NativeError> {
        let mut slot = lock(&self.registered);
        if self.stopping.load(Ordering::Acquire) {
            owner.retire();
            return Err(NativeError { status: 409, code: "daemon_stopping".into() });
        }
        *slot = Some(owner.clone());
        Ok(())
    }

    async fn cleanup_registered(&self) {
        let registered = lock(&self.registered).clone();
        if let Some(owner)=registered {
            if owner.suspend().await.is_err() {return;}
            lock(&self.registered).take();
        }
        if let Some(custody)=lock(&self.opening_custody).take() {custody.retire();}
    }

    /// Retire, drain workers, then release the native owner. Handles are kept
    /// on failure so a later stop retries the same drain.
    async fn close(&self, hosted: &Hosted, revoke: bool) -> Result<(), &'static str> {
        hosted.retired.store(true, Ordering::Release);
        hosted.allocation.retire();
        hosted.shared.retire();
        hosted.monitor.abort();
        // The read-only probe owns a real child. The wait is bounded and the
        // task is retained until observed finished, so a cancelled drain waiter
        // cannot detach process custody; an aborted probe's child stays in
        // provider task custody, which the owner drain below observes.
        self.settle_provider_refresh().await?;
        hosted.shared.drain().await;
        hosted.allocation.drain().await;
        let result = if revoke {
            hosted.owner.shutdown_checked().await
        } else {
            hosted.owner.suspend().await
        };
        result.map_err(|e| {
            tracing::warn!(code = drain_detail(&e.code), "Native owner drain did not complete");
            static_code(&e.code)
        })?;
        hosted.custody.retire();
        Ok(())
    }

    async fn stop(self: &Arc<Self>) -> Result<(), &'static str> {
        self.stopping.store(true, Ordering::Release);
        let starting = lock(&self.starting).take();
        if let Some(task) = starting {
            task.abort();
            let _ = task.await;
        }
        let mut hosted_slot=self.hosted.lock().await;
        let result = match hosted_slot.as_ref() {
            Some(hosted) => {
                self.set_phase(Phase::Draining, None);
                let result = self.close(hosted, false).await;
                if result.is_ok() {hosted_slot.take();}
                result
            }
            None => Ok(()),
        };
        drop(hosted_slot);
        // An aborted startup may have registered an owner before publishing.
        let registered = lock(&self.registered).clone();
        let result = match registered {
            Some(owner) => match owner.suspend().await {
                Ok(()) => {
                    lock(&self.registered).take();
                    if let Some(custody)=lock(&self.opening_custody).take() {custody.retire();}
                    result
                }
                Err(e) => Err(static_code(&e.code)),
            },
            None => result,
        };
        if lock(&self.registered).is_none() {
            if let Some(custody)=lock(&self.opening_custody).take() {custody.retire();}
        }
        match result {
            Ok(()) => {
                self.set_phase(Phase::Stopped, None);
                self.event(ActivityState::Succeeded, "Hosting stopped", "Workers drained and the owner root was released.");
            }
            Err(code) => {
                self.set_phase(Phase::Failed, Some(code.into()));
                self.event(ActivityState::Failed, "Hosting stop needs recovery", code);
            }
        }
        self.stopping.store(false, Ordering::Release);
        result
    }
}

/// Only static engine codes leave this module.
fn static_code(code: &str) -> &'static str {
    match code {
        "management_drain_outcome_unknown" => "management_drain_outcome_unknown",
        "management_drain_unavailable" => "management_drain_unavailable",
        "native_logout_persistence_failed" => "native_logout_persistence_failed",
        "shared_execution_outcome_unknown" => "shared_execution_outcome_unknown",
        "runtime_stop_unknown" => "runtime_stop_unknown",
        _ => "native_owner_drain_required",
    }
}

/// Which owner drain failed, for the log only. The returned code stays the
/// coarse `static_code`; this still never echoes an unknown string.
fn drain_detail(code: &str) -> &'static str {
    match code {
        "native_configuration_drain_unknown" => "native_configuration_drain_unknown",
        "native_local_authority_close_failed" => "native_local_authority_close_failed",
        "native_runtime_drain_unknown" => "native_runtime_drain_unknown",
        "native_provider_drain_unknown" => "native_provider_drain_unknown",
        "native_shared_execution_drain_unknown" => "native_shared_execution_drain_unknown",
        "native_managed_runtime_drain_unknown" => "native_managed_runtime_drain_unknown",
        "native_account_login_drain_unknown" => "native_account_login_drain_unknown",
        other => static_code(other),
    }
}

async fn monitor(
    inner: std::sync::Weak<Inner>,
    custody: Arc<SdkCustody>,
    owner: NativeOwner,
    retired: Arc<AtomicBool>,
    epoch: u64,
) {
    let mut last_read: Option<tokio::time::Instant> = None;
    let mut last_provider: Option<tokio::time::Instant> = None;
    let mut management = None;
    loop {
        tokio::time::sleep(MONITOR_PERIOD).await;
        let Some(inner) = inner.upgrade() else { return };
        if retired.load(Ordering::Acquire) || inner.generation.load(Ordering::Acquire) != epoch {
            return;
        }
        match custody.state() {
            AuthState::SignInRequired => {
                // Fence execution; keep native enrollment for an explicit
                // re-enrollment or sign-out decision by the owner.
                inner.event(ActivityState::Warning, "Owner authorization expired", "matrix_authorization_required");
                retired.store(true, Ordering::Release);
                // close() aborts this monitor: run it in its own task.
                tokio::spawn(async move {
                    let mut hosted_slot=inner.hosted.lock().await;
                    if inner.generation.load(Ordering::Acquire)!=epoch {return;}
                    if let Some(hosted) = hosted_slot.as_ref() {
                        let result = inner.close(hosted, false).await;
                        if result.is_ok() {hosted_slot.take();}
                        if inner.generation.load(Ordering::Acquire)==epoch {
                            inner.set_phase(
                                Phase::Failed,
                                Some(result.err().unwrap_or("matrix_authorization_required").into()),
                            );
                        }
                    }
                });
                return;
            }
            AuthState::Unavailable => {
                inner.set_phase(Phase::Offline, Some("matrix_authorization_unavailable".into()))
            }
            AuthState::Authorized => {
                if lock(&inner.phase).0 == Phase::Offline {
                    let adopted = inner.hosted.try_lock().ok().and_then(|h| h.as_ref().map(|h| h.adopted));
                    inner.set_phase(
                        Phase::Running,
                        (adopted == Some(false)).then(|| "No pooled resources are activated on this device.".into()),
                    );
                }
            }
            AuthState::Retired => return,
        }
        if last_provider.is_none_or(|t|t.elapsed()>=PROVIDER_PERIOD) {
            last_provider=Some(tokio::time::Instant::now());
            let _=inner.refresh_provider_reports().await;
        }
        if last_read.is_none_or(|t| t.elapsed() >= READ_PERIOD) {
            last_read = Some(tokio::time::Instant::now());
            if management.is_none() {
                management = match tokio::time::timeout(PROVIDER_SETUP_TIMEOUT, owner.management(read_package(epoch))).await {
                    Ok(result) => result.map_err(|e| tracing::info!(code = %e.code, "Dashboard reads unavailable")).ok(),
                    Err(_) => {
                        tracing::info!(code = "dashboard_read_setup_timeout", "Dashboard reads unavailable");
                        None
                    }
                };
            }
            if let Some(manager) = management.as_ref() {
                // A stalled read must not stop this monitor: it also drives
                // provider refreshes and authorization fencing.
                let reads = tokio::time::timeout(DASHBOARD_READ_TIMEOUT, read_dashboard(manager, custody.owner()))
                    .await
                    .unwrap_or_else(|_| {
                        tracing::info!(code = "dashboard_read_timeout", "Dashboard reads unavailable");
                        Reads::default()
                    });
                if retired.load(Ordering::Acquire) || inner.generation.load(Ordering::Acquire) != epoch { return; }
                lock(&inner.reads).refresh(reads);
            }
        }
    }
}

fn read_package(epoch: u64) -> crate::native_owner::ManagementPackage {
    use crate::native_owner::{ManagementPackage, ManagementPermission as P};
    use sha2::{Digest, Sha256};
    ManagementPackage {
        digest: format!("{:x}", Sha256::digest(b"hagency-owner-daemon-dashboard-read-v1")),
        instance: 1,
        account_epoch: epoch,
        permissions: vec![P::ReadResources, P::ReadCatalog, P::ReadAgentRequests, P::ReadAgentWork],
    }
}

async fn pages(
    manager: &crate::native_owner::NativeManagement,
    command: impl Fn(Option<String>) -> crate::native_owner::ManagementCommand,
) -> Option<Vec<Value>> {
    let mut items = Vec::new();
    let mut after = None;
    for _ in 0..MAX_PAGES {
        let page = manager.central(command(after.clone())).await.ok()?;
        items.extend(page["items"].as_array()?.iter().cloned());
        match page["nextAfter"].as_str() {
            Some(next) => after = Some(next.to_owned()),
            None => return Some(items),
        }
    }
    // More pages than the bound: the list is incomplete, so report unknown.
    None
}

async fn read_dashboard(manager: &crate::native_owner::NativeManagement, owner: &str) -> Reads {
    use crate::native_owner::ManagementCommand as C;
    let catalog = pages(manager, |after| C::Catalog { after, capability: None }).await;
    let requests = pages(manager, |after| C::PoolAgents { after, pool_id: None, needs_my_action: false }).await;
    let decisions = pages(manager, |after| C::PoolAgents { after, pool_id: None, needs_my_action: true }).await;
    let approvals = manager
        .central(C::SharedApprovals { room_id: None })
        .await
        .ok()
        .and_then(|v| v["approvals"].as_array().map(|rows| rows.iter().filter(|r| r["canDecide"] == true).count() as u64));
    let inventory=local_resources(manager, catalog.as_deref(), requests.as_deref(), owner).await;
    Reads {
        inventory_available:inventory.is_some(),
        inventory_complete:inventory.as_ref().is_some_and(|(_,complete)|*complete),
        pending_approvals: match (approvals, decisions.as_ref()) {
            (Some(a), Some(d)) => Some(a + d.len() as u64),
            _ => None,
        },
        resources: inventory.map(|(rows,_)|rows),
    }
}

fn telemetry_directory(home: &std::path::Path, enrollment: &Enrollment) -> PathBuf {
    use sha2::{Digest, Sha256};
    let identity = serde_json::to_vec(&(&enrollment.origin, &enrollment.owner)).unwrap_or_default();
    home.join("telemetry").join(format!("{:x}", Sha256::digest(identity)))
}

async fn local_resources(
    manager: &crate::native_owner::NativeManagement,
    catalog: Option<&[Value]>, requests: Option<&[Value]>, owner: &str,
) -> Option<(Vec<ResourceUsage>,bool)> {
    let mut rows = Vec::new();
    let mut after = String::new();
    for _ in 0..MAX_PAGES {
        let page = manager.resources(hagency_store::management::ResourceQuery { after, ..Default::default() }).await.ok()?;
        for resource in page.resources {
            let agent_count = catalog.zip(requests).map(|(catalog, requests)| {
                requests.iter().filter(|request| {
                    request["resourceOwnerMxid"] == owner && request["state"] == "admitted"
                        && catalog.iter().any(|contribution| contribution["ownerMxid"] == owner
                            && contribution["resourceId"] == resource.id && contribution["id"] == request["poolId"])
                }).count() as u64
            });
            rows.push(ResourceUsage {
                name: resource.local_name.map(|v| v.name).unwrap_or_else(|| resource.id.clone()),
                id: resource.id,
                account_id: None,
                provider: resource.framework,
                model: resource.model,
                agent_count,
                budget_tokens: resource.budget.pool.ceiling.map(u64::from),
                state: ResourceState::Unmeasured.as_str().into(),
                counts: UNKNOWN_COUNTS,
            });
        }
        match page.next_after { Some(next) => after = next, None => return Some((rows,true)) }
    }
    Some((rows,false))
}

/// Owner's own pools from the server catalog. Token counts stay unknown
/// here: measured consumption comes from the telemetry ledger, never from
/// a published grant or a plan name.
pub(crate) fn resources(pools: &[Value], requests: Option<&[Value]>, owner: &str) -> Vec<ResourceUsage> {
    pools
        .iter()
        .filter(|p| p["ownerMxid"] == owner)
        .filter_map(|p| {
            let id = p["id"].as_str()?.to_owned();
            let agent_count = requests.map(|rows| {
                rows.iter()
                    .filter(|r| r["poolId"] == id.as_str() && r["resourceOwnerMxid"] == owner && r["state"] == "admitted")
                    .count() as u64
            });
            Some(ResourceUsage {
                name: p["label"].as_str().unwrap_or(&id).to_owned(),
                account_id: None,
                provider: p["runtime"].as_str().unwrap_or("unknown").to_owned(),
                model: "unknown".into(),
                agent_count,
                budget_tokens: p["publishedTokens"].as_i64().and_then(|t| u64::try_from(t).ok()),
                // Usage is not attached to these rows yet: unmeasured.
                state: ResourceState::Unmeasured.as_str().into(),
                counts: UNKNOWN_COUNTS,
                id,
            })
        })
        .collect()
}

impl Runtime for OwnerRuntime {
    fn enrollment(&self,id:String,call:super::enrollment_rpc::Call)->BoxFuture<'_,Result<super::enrollment_rpc::Status,String>> {
        Box::pin(async move {
            use super::enrollment_rpc::Call;
            if let Call::Status{id}=call{return lock(&self.inner.enrollment_flow).get(&id).map_err(str::to_owned)}
            let Call::Start{origin,owner,display_name}=&call else{unreachable!()};
            if let Some(current)=lock(&self.inner.enrollment).as_ref()
                && (!super::same_origin(origin,&current.origin)||owner!=&current.owner) {return Err("daemon_owner_scope_mismatch".into())}
            let (workflow,start)=lock(&self.inner.enrollment_flow).begin(&id,&call).map_err(str::to_owned)?;
            if start {
                let (origin,owner,display_name)=(origin.clone(),owner.clone(),display_name.clone());
                let runtime=Self{inner:self.inner.clone()};let progress=workflow.clone();
                workflow.spawn(async move {
                    runtime.inner.bridge.revoke_all();
                    let mut stopped = false;
                    let result=async {
                        let roots=crate::owner_roots::OwnerRoots::system().map_err(|e|super::host::HostError::Code(e.code()))?;
                        runtime.stop().await.map_err(super::host::HostError::Code)?;
                        stopped = true;
                        let prompt=progress.clone();
                        super::host::enroll_locked_controlled(&runtime.inner.home,&roots,&origin,&owner,display_name,progress.control.clone(),move |url|async move{lock(&prompt.status).authorization_url=Some(url);Ok(())}).await
                    }.await;
                    match result {
                        Ok(report)=>{lock(&progress.status).result=Some(report);if !progress.control.is_cancelled(){runtime.start();}},
                        Err(error)=> {
                            let code = match error { super::host::HostError::Code(code) => code, _ => "daemon_enrollment_failed" };
                            lock(&progress.status).error=Some(code.into());
                            runtime.inner.set_phase(Phase::Failed, Some(code.into()));
                            if stopped && Enrollment::load(&runtime.inner.home).ok().flatten().is_some_and(|e| e.hostable()) && super::revocation::load(&runtime.inner.home).is_ok_and(|r| r.is_none()) {
                                runtime.start();
                            }
                        }
                    }
                });
            }
            let value=lock(&workflow.status).clone();Ok(value)
        })
    }

    fn bridge(&self, id:String, call:super::bridge::Call) -> BoxFuture<'_,Result<serde_json::Value,super::bridge::Failure>> {
        Box::pin(async move {
            let scope = self.owner().ok_or_else(||super::bridge::error(401,"daemon_enrollment_required"))?;
            let generation = self.generation();
            if let super::bridge::Call::SignOut { controller } = &call {
                self.inner.bridge.validate(controller,generation)?;
                self.inner.bridge.revoke_all();
                self.sign_out().await.map_err(|code|super::bridge::error(503,code))?;
                return Ok(serde_json::Value::Null);
            }
            let native = self.inner.hosted.lock().await.as_ref().filter(|hosted|!hosted.retired.load(Ordering::Acquire)).map(|hosted|hosted.owner.clone());
            #[cfg(feature="resource-adoption")]
            let executor_slot=self.inner.hosted.lock().await.as_ref().map(|h|h.executor_slot.clone());
            #[cfg(feature="resource-adoption")]
            let shared=match executor_slot {Some(slot)=>slot.lock().await.clone(),None=>None};
            let root=lock(&self.inner.enrollment).as_ref().ok_or_else(||super::bridge::error(401,"daemon_enrollment_required"))?.owner_root.clone();
            #[cfg(feature="resource-adoption")]
            let activate=matches!(call,super::bridge::Call::OwnerAccess{operation:super::bridge::OwnerOperation::ConfirmDeviceSetup,..});
            let result=self.inner.bridge.handle_recorded(&id,&scope.user_id,&scope.server,generation,native,&root,#[cfg(feature="resource-adoption")] shared,call).await;
            #[cfg(feature="resource-adoption")]
            if activate && result.is_ok() {
                self.inner.bridge.revoke_all();
                self.stop().await.map_err(|code|super::bridge::error(503,code))?;
                self.inner.stopping.store(false,Ordering::Release);
                self.inner.open_once().await.map_err(|error|match error {OpenError::Retry(code)|OpenError::Terminal(code)=>super::bridge::error(503,&code)})?;
            }
            result
        })
    }
    fn generation(&self) -> u64 { self.inner.generation.load(Ordering::Acquire) }
    fn owner(&self) -> Option<OwnerScope> {
        lock(&self.inner.enrollment).as_ref().map(|e| OwnerScope {
            user_id: e.owner.clone(),
            server: e.origin.clone(),
            display_name: e.display_name.clone().unwrap_or_else(|| e.owner.clone()),
        })
    }

    fn observe(&self) -> Observation {
        let (mut phase, mut detail) = lock(&self.inner.phase).clone();
        if phase == Phase::Running && !self.inner.admission.is_open() && !self.inner.admission.is_quiescent() {
            phase = Phase::Draining;
            detail = Some("Waiting for admitted requests to finish starting.".into());
        }
        let active_runs = Some(self.inner.admission.active_turns());
        if phase == Phase::Running
            && let Some(code) = lock(&self.inner.health).clone()
        {
            detail = Some(match detail {
                Some(d) => format!("{d} Last worker error: {code}."),
                None => format!("Last worker error: {code}."),
            });
        }
        Observation {
            phase,
            detail,
            active_runs,
            pending_approvals: lock(&self.inner.reads).pending_approvals,
        }
    }

    fn close_admission(&self) -> BoxFuture<'_, bool> {
        Box::pin(self.inner.admission.close(ADMISSION_BARRIER))
    }

    fn open_admission(&self) {
        self.inner.admission.open();
    }

    fn start(&self) {
        let mut starting = lock(&self.inner.starting);
        if starting.as_ref().is_some_and(|h| !h.is_finished()) {
            return;
        }
        let inner = self.inner.clone();
        *starting = Some(tokio::spawn(inner.run_start()));
    }

    fn stop(&self) -> BoxFuture<'_, Result<(), &'static str>> {
        let inner = self.inner.clone();
        Box::pin(async move { inner.stop().await })
    }

    fn snapshot_parts(&self, period: Option<Period>, filter:Option<hagency_metering::dashboard::UsageFilter>) -> BoxFuture<'_, SnapshotParts> {
        Box::pin(async move {
            let (mut resources,inventory_available,inventory_complete)={
                let reads=lock(&self.inner.reads);
                (reads.resources.clone().unwrap_or_default(),reads.inventory_available,reads.inventory_complete)
            };
            let telemetry = lock(&self.inner.telemetry).clone();
            let options = SnapshotOptions {
                period: period.unwrap_or(UsagePeriod::Day),
                utc_offset_minutes: 0,
            };
            let query_telemetry=telemetry.clone();
            let parts = if let Some(telemetry) = telemetry {
                tokio::task::spawn_blocking(move || {
                    let at=now_ms();
                    let parts=telemetry.parts(at, options)?;
                    let personal=telemetry.personal_resources(at, options)?;
                    Ok::<_, crate::native_owner::TelemetryError>((parts,personal))
                }).await.ok().and_then(Result::ok)
            } else { None };
            let (parts,personal)=parts.map(|(parts,personal)|(Some(parts),personal)).unwrap_or_default();
            resources.extend(personal);
            if let Some(parts) = &parts {
                for row in &mut resources {
                    if let Some(usage) = parts.resources.iter().find(|r| r.resource_id == row.id) {
                        row.account_id = usage.account_id.clone();
                        row.counts = usage.counts;
                        row.state = usage.state.as_str().into();
                    }
                }
            }
            let mut snapshot=SnapshotParts {
                inventory_available,
                inventory_complete,
                usage_breakdown:None,
                accounts: parts.as_ref().map(|p| p.accounts.clone()).unwrap_or_default(),
                resources,
                usage: parts.map(|p| p.usage),
                activities: lock(&self.inner.events).iter().rev().cloned().collect(),
            };
            if let Some(telemetry)=query_telemetry {
                snapshot=tokio::task::spawn_blocking(move||{
                    let _=super::snapshot_query::apply(&telemetry,&mut snapshot,now_ms(),options,filter);
                    snapshot
                }).await.unwrap_or_default();
            }
            snapshot
        })
    }

    fn refresh_providers(&self) -> BoxFuture<'_, Result<(), DaemonError>> {
        Box::pin(self.inner.refresh_provider_reports())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failed_inventory_read_keeps_last_rows_but_cannot_be_reported_as_confirmed_empty() {
        let mut reads=Reads::default();
        assert!(!reads.inventory_available);
        let row=ResourceUsage{id:"original".into(),name:"Original".into(),account_id:None,provider:"codex".into(),model:"model".into(),agent_count:None,budget_tokens:None,state:ResourceState::Unmeasured.as_str().into(),counts:UNKNOWN_COUNTS};
        reads.refresh(Reads{resources:Some(vec![row]),inventory_available:true,inventory_complete:false,..Default::default()});
        assert!(!reads.inventory_complete,"bounded partial pages are explicitly incomplete");
        reads.refresh(Reads::default());
        assert!(!reads.inventory_available);
        assert_eq!(reads.resources.as_ref().unwrap()[0].id,"original");
        reads.refresh(Reads{resources:Some(vec![]),inventory_available:true,inventory_complete:true,..Default::default()});
        assert!(reads.inventory_available && reads.inventory_complete);
        assert!(reads.resources.unwrap().is_empty(),"a successful empty inventory replaces old rows");
    }

    /// A provider reader whose page or report either answers, fails, or never
    /// returns (a stalled probe, child reap or authorization wait).
    struct Reader {
        page: Behavior,
        report: Behavior,
        reports: std::sync::atomic::AtomicUsize,
        retire_on_report: Option<Arc<AtomicBool>>,
    }
    #[derive(Clone, Copy)]
    enum Behavior { Ok, Fail, Stall }
    impl Reader {
        fn new(page: Behavior, report: Behavior) -> Self {
            Self { page, report, reports: Default::default(), retire_on_report: None }
        }
    }
    async fn behave(behavior: Behavior) -> Result<(), NativeError> {
        match behavior {
            Behavior::Ok => Ok(()),
            Behavior::Fail => Err(NativeError { status: 409, code: "provider_identity_unavailable".into() }),
            Behavior::Stall => std::future::pending().await,
        }
    }
    impl ProviderReports for Reader {
        type Resource = &'static str;
        async fn page(&self, _after: String) -> Result<(Vec<&'static str>, Option<String>), NativeError> {
            behave(self.page).await?;
            Ok((vec!["codex", "claude", "codex"], None))
        }
        fn reportable(resource: &&'static str) -> bool {
            *resource == "codex"
        }
        async fn report(&self, _resource: &&'static str) -> Result<(), NativeError> {
            self.reports.fetch_add(1, Ordering::SeqCst);
            if let Some(retired) = &self.retire_on_report {
                retired.store(true, Ordering::Release);
            }
            behave(self.report).await
        }
    }

    fn private_home() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        temp
    }

    fn last_event(runtime: &OwnerRuntime) -> Option<(String, String, String)> {
        lock(&runtime.inner.events).back().map(|a| (a.title.clone(), a.state.clone(), a.detail.clone()))
    }

    /// Production proportions, scaled down so the bounds fire in milliseconds.
    const FAST: RefreshBounds = RefreshBounds {
        page: Duration::from_millis(40),
        report: Duration::from_millis(100),
        budget: Duration::from_millis(1500),
    };

    #[tokio::test]
    async fn a_stalled_provider_report_finishes_as_unavailable_instead_of_running_forever() {
        let temp = private_home();
        let runtime = OwnerRuntime::new(temp.path().into()).unwrap();
        let epoch = runtime.inner.generation.load(Ordering::Acquire);
        // Incident shape: the probe, child reap or authorization for a
        // resource never returns. Before, the job never finished and every
        // later refresh returned early behind it, leaving "Running" forever.
        let started = tokio::time::Instant::now();
        let job = provider_refresh_job(Reader::new(Behavior::Ok, Behavior::Stall), Arc::new(AtomicBool::new(false)), Arc::downgrade(&runtime.inner), epoch, FAST);
        tokio::time::timeout(Duration::from_secs(10), job).await.expect("the refresh job must finish on its own");
        assert!(started.elapsed() < FAST.budget, "each stalled report is bounded on its own");
        let (title, state, detail) = last_event(&runtime).expect("a finished activity, not a perpetual Running");
        assert_eq!(title, "Provider reports unavailable");
        assert_eq!(state, ActivityState::Warning.as_str());
        assert!(detail.contains("Existing reports keep their observation times"));

        // A stalled inventory read is bounded the same way.
        let job = provider_refresh_job(Reader::new(Behavior::Stall, Behavior::Ok), Arc::new(AtomicBool::new(false)), Arc::downgrade(&runtime.inner), epoch, FAST);
        tokio::time::timeout(Duration::from_secs(10), job).await.expect("a stalled inventory read must not hang the job");
        assert_eq!(last_event(&runtime).unwrap().0, "Provider reports unavailable");

        // The overall budget covers work whose individual steps each answer.
        let slow = RefreshBounds { budget: Duration::from_millis(150), ..FAST };
        let job = provider_refresh_job(Reader::new(Behavior::Ok, Behavior::Stall), Arc::new(AtomicBool::new(false)), Arc::downgrade(&runtime.inner), epoch, slow);
        tokio::time::timeout(Duration::from_secs(10), job).await.expect("the budget must end the job");
        let (_, _, detail) = last_event(&runtime).unwrap();
        assert!(detail.contains("did not finish in time"), "{detail}");
    }

    #[tokio::test]
    async fn provider_report_tallies_are_bounded_per_resource_and_reported() {
        let retired = AtomicBool::new(false);
        let stalled = Reader::new(Behavior::Ok, Behavior::Stall);
        let tally = read_provider_reports(&stalled, &retired, FAST).await.unwrap();
        assert_eq!(tally, ReportTally { observed: 0, failed: 2, complete: true });
        assert_eq!(stalled.reports.load(Ordering::SeqCst), 2, "only supported resources are probed, each once");
        let ok = read_provider_reports(&Reader::new(Behavior::Ok, Behavior::Ok), &retired, FAST).await.unwrap();
        assert_eq!(ok, ReportTally { observed: 2, failed: 0, complete: true });
        let failing = read_provider_reports(&Reader::new(Behavior::Ok, Behavior::Fail), &retired, FAST).await.unwrap();
        assert_eq!(failing, ReportTally { observed: 0, failed: 2, complete: true });
        let inventory = read_provider_reports(&Reader::new(Behavior::Fail, Behavior::Ok), &retired, FAST).await.unwrap();
        assert!(!inventory.complete && inventory.observed == 0 && inventory.failed > 0);
    }

    #[tokio::test]
    async fn a_refresh_retired_while_reading_reports_nothing() {
        let temp = private_home();
        let runtime = OwnerRuntime::new(temp.path().into()).unwrap();
        let retired = Arc::new(AtomicBool::new(false));
        let mut reader = Reader::new(Behavior::Ok, Behavior::Ok);
        reader.retire_on_report = Some(retired.clone());
        assert_eq!(read_provider_reports(&reader, &retired, FAST).await, None);
        let mut reader = Reader::new(Behavior::Ok, Behavior::Ok);
        reader.retire_on_report = Some(retired.clone());
        provider_refresh_job(reader, retired, Arc::downgrade(&runtime.inner), 0, FAST).await;
        assert!(last_event(&runtime).is_none(), "a retired host records nothing from a late reply");
    }

    #[tokio::test]
    async fn a_running_refresh_is_reported_stalled_once_after_its_budget() {
        let mut refresh = ProviderRefresh {
            task: tokio::spawn(std::future::pending::<()>()),
            started: tokio::time::Instant::now(),
            stalled: false,
        };
        assert!(!refresh.newly_stalled(refresh.started + PROVIDER_REFRESH_BUDGET));
        assert!(refresh.newly_stalled(refresh.started + PROVIDER_REFRESH_STALL));
        assert!(!refresh.newly_stalled(refresh.started + PROVIDER_REFRESH_STALL * 2), "reported once");
        refresh.task.abort();
        assert!(PROVIDER_REFRESH_STALL < PROVIDER_PERIOD && PROVIDER_REFRESH_BUDGET < PROVIDER_REFRESH_STALL);
        assert!(PROVIDER_PAGE_TIMEOUT < PROVIDER_REFRESH_BUDGET && PROVIDER_REPORT_TIMEOUT < PROVIDER_REFRESH_BUDGET);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stop_waits_for_a_refresh_only_within_a_bound_and_retains_an_unfinished_task() {
        let temp = private_home();
        let runtime = OwnerRuntime::new(temp.path().into()).unwrap();
        let within = Duration::from_millis(50);
        // An async stall is cancelled after the bound and then released.
        *runtime.inner.provider_refresh.lock().await = Some(ProviderRefresh {
            task: tokio::spawn(std::future::pending::<()>()),
            started: tokio::time::Instant::now(),
            stalled: false,
        });
        assert_eq!(runtime.inner.settle_provider_refresh_within(within).await, Ok(()));
        assert!(runtime.inner.provider_refresh.lock().await.is_none());
        // A task that cannot observe cancellation keeps its handle, so a later
        // stop waits for the same task instead of detaching it.
        let (release, blocked) = std::sync::mpsc::channel::<()>();
        *runtime.inner.provider_refresh.lock().await = Some(ProviderRefresh {
            task: tokio::spawn(async move { let _ = blocked.recv(); }),
            started: tokio::time::Instant::now(),
            stalled: false,
        });
        assert_eq!(runtime.inner.settle_provider_refresh_within(within).await, Err("provider_refresh_drain_unknown"));
        assert!(runtime.inner.provider_refresh.lock().await.is_some());
        release.send(()).unwrap();
        assert_eq!(runtime.inner.settle_provider_refresh_within(Duration::from_secs(5)).await, Ok(()));
        assert!(runtime.inner.provider_refresh.lock().await.is_none());
    }

    #[test]
    fn drain_detail_names_only_known_drains() {
        assert_eq!(drain_detail("native_provider_drain_unknown"), "native_provider_drain_unknown");
        assert_eq!(drain_detail("native_shared_execution_drain_unknown"), "native_shared_execution_drain_unknown");
        assert_eq!(drain_detail("management_drain_unavailable"), "management_drain_unavailable");
        assert_eq!(drain_detail("/Users/x/.codex/auth.json secret"), "native_owner_drain_required");
        assert_eq!(static_code("native_provider_drain_unknown"), "native_owner_drain_required", "returned codes stay coarse");
    }

    #[test]
    fn retry_classification_waits_for_a_busy_root_but_not_for_authority() {
        assert!(retryable("native_owner_state_unavailable"));
        assert!(retryable("matrix_authorization_unavailable"));
        assert!(retryable("server_unavailable"));
        for code in ["matrix_authorization_required", "matrix_account_mismatch", "current_profile_scope_changed", "native_resume_revoked"] {
            assert!(!retryable(code), "{code}");
        }
        assert_eq!(static_code("/Users/x/.codex/auth.json secret"), "native_owner_drain_required");
    }

    #[test]
    fn resources_are_the_owners_pools_with_unknown_usage() {
        let pools = vec![
            json!({"id":"p1","label":"Terra","ownerMxid":"@o:s","runtime":"codex","publishedTokens":5000,"canRequest":true}),
            json!({"id":"p2","label":"Other","ownerMxid":"@x:s","runtime":"codex","canRequest":true}),
            json!({"id":"p3","ownerMxid":"@o:s","runtime":"codex","canRequest":false,"blockingReason":"grant_expired"}),
        ];
        let requests = vec![
            json!({"poolId":"p1","resourceOwnerMxid":"@o:s","state":"admitted"}),
            json!({"poolId":"p1","resourceOwnerMxid":"@o:s","state":"pending_approval"}),
            json!({"poolId":"p1","resourceOwnerMxid":"@o:s","state":"admitted"}),
        ];
        let rows = resources(&pools, Some(&requests), "@o:s");
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].name.as_str(), rows[0].agent_count, rows[0].budget_tokens), ("Terra", Some(2), Some(5000)));
        assert_eq!(rows[1].name, "p3");
        assert!(rows.iter().all(|r| r.state == "unmeasured"));
        assert!(rows.iter().all(|r| r.counts.input.is_none() && r.counts.output.is_none()));
    }

    #[tokio::test]
    async fn unenrolled_runtime_is_idle_and_hosts_nothing() {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let runtime = OwnerRuntime::new(temp.path().into()).unwrap();
        assert!(runtime.owner().is_none());
        runtime.start();
        for _ in 0..100 {
            if lock(&runtime.inner.starting).as_ref().is_some_and(|t| t.is_finished()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(runtime.observe().phase, Phase::Idle);
        assert_eq!(runtime.observe().active_runs, Some(0));
        assert!(runtime.stop().await.is_ok());
        assert_eq!(runtime.observe().phase, Phase::Stopped);
        assert_eq!(runtime.refresh_providers().await.unwrap_err(), errors::PROVIDER_REFRESH_UNAVAILABLE);
    }

    #[tokio::test]
    async fn an_unfinished_enrollment_is_never_hosted() {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let root = temp.path().join("owners").join("registered-root");
        Enrollment::prepare(
            temp.path(),
            "https://s.example/",
            "@o:s.example",
            None,
            root.clone(),
        )
        .unwrap();
        let runtime = OwnerRuntime::new(temp.path().into()).unwrap();
        runtime.start();
        for _ in 0..200 {
            if lock(&runtime.inner.starting)
                .as_ref()
                .is_some_and(|t| t.is_finished())
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let observed = runtime.observe();
        assert_eq!(observed.phase, Phase::Stopped);
        assert_eq!(
            observed.detail.as_deref(),
            Some("daemon_enrollment_incomplete")
        );
        assert!(!root.exists());
    }

    #[tokio::test]
    async fn enrolled_without_sdk_session_fails_closed_without_opening_the_root() {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        hagency_store::private::directory(&temp.path().join(super::super::MATRIX)).unwrap();
        let root = temp
            .path()
            .parent()
            .unwrap()
            .join(format!("never-created-{}", super::super::new_request_id()));
        let enrollment = Enrollment::enroll(
            temp.path(),
            "https://s.example/",
            "@o:s.example",
            None,
            &root,
        )
        .unwrap();
        let runtime = OwnerRuntime::new(temp.path().into()).unwrap();
        assert_eq!(runtime.owner().unwrap().user_id, "@o:s.example");
        runtime.start();
        for _ in 0..200 {
            if runtime.observe().phase == Phase::Failed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let observed = runtime.observe();
        assert_eq!(observed.phase, Phase::Failed);
        assert_eq!(observed.detail.as_deref(), Some("daemon_enrollment_required"));
        assert!(!enrollment.owner_root.exists(), "root must not be created without authorization");
    }
}
