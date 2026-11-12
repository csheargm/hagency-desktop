//! Native allocation and shared-execution workers owned by the daemon.
//!
//! Ported from Hagency Desktop's `allocation_worker`, `shared_worker`,
//! `shared_intake`, `shared_budget` and `shared_successors`. The native engine
//! still owns every journal, receipt, fence and process; these loops only
//! drive the existing executor operations in their established order with
//! independent original-command cursors.
//!
//! Pausing closes admission. Steps that can begin a *new* model turn (new or
//! saved demand intake, budget demand intake, saved successor starts) are
//! skipped while admission is closed. Stop recovery, budget ledger deltas,
//! Agent reserve/fence, Matrix observation, runtime delivery and cleanup,
//! successor process polling and settlement continue, so running work is
//! fenced, finishes and settles normally.
use crate::native_owner::NativeError;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub type StepFuture<'a> = Pin<Box<dyn Future<Output = Result<Advance, NativeError>> + Send + 'a>>;

/// One step's result: the next cursor for its own journal and, for logging
/// only, a non-error outcome code worth reporting (never content or ids).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Advance {
    pub cursor: Option<String>,
    pub note: Option<String>,
}

impl From<Option<String>> for Advance {
    fn from(cursor: Option<String>) -> Self {
        Self { cursor, note: None }
    }
}

/// Status of a step whose last run succeeded without a note.
pub const STEP_OK: &str = "ok";
/// A shared step that found no local process evidence for central history.
pub const HISTORICAL_EVIDENCE_MISSING: &str = "historical_evidence_missing";

pub use crate::console::hosting::Admission;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Step {
    DemandStop,
    BudgetDelta,
    BudgetDemandRecover,
    BudgetProcess,
    BudgetDemand,
    /// Saved demands not yet finished locally; the only ones that can hold an
    /// Agent's FIFO. Visited separately so a restart releases them quickly.
    IntakeDemandUnfinished,
    IntakeDemandRecover,
    IntakeDemand,
    Agent,
    Matrix,
    Runtime,
    SuccessorResume,
    SuccessorPoll,
    Settlement,
}

impl Step {
    /// Steps that can begin a new model turn.
    pub fn admits_work(self) -> bool {
        matches!(
            self,
            Self::BudgetDemandRecover
                | Self::BudgetDemand
                | Self::IntakeDemandUnfinished
                | Self::IntakeDemandRecover
                | Self::IntakeDemand
                | Self::SuccessorResume
        )
    }
}

/// Desktop's order: stop recovery precedes every launch path; reserve/fence
/// and Matrix readiness precede runtime delivery; settlement observes last.
pub const ORDER: [Step; 14] = [
    Step::DemandStop,
    Step::BudgetDelta,
    Step::BudgetDemandRecover,
    Step::BudgetProcess,
    Step::BudgetDemand,
    Step::IntakeDemandUnfinished,
    Step::IntakeDemandRecover,
    Step::IntakeDemand,
    Step::Agent,
    Step::Matrix,
    Step::Runtime,
    Step::SuccessorResume,
    Step::SuccessorPoll,
    Step::Settlement,
];

/// One executor operation per step. `run` returns the next cursor for that
/// step's own journal; an error keeps the previous cursor.
pub trait PassIo: Send + Sync {
    fn enabled(&self, step: Step) -> bool;
    fn run(&self, step: Step, after: Option<String>) -> StepFuture<'_>;
}

/// Per-step cursors, and the last status code per step (`STEP_OK`, a note or
/// an error code), so a pass logs only when a step's status changes.
#[derive(Default, Debug)]
pub struct Cursors(HashMap<Step, Option<String>>, HashMap<Step, String>);

impl Cursors {
    pub fn get(&self, step: Step) -> Option<String> {
        self.0.get(&step).cloned().flatten()
    }
    pub fn status(&self, step: Step) -> Option<&str> {
        self.1.get(&step).map(String::as_str)
    }
}

/// Info-level and change-only: the step name and a static or error code, never
/// identifiers or content. Every failure still logs at debug below.
fn report_change(step: Step, previous: Option<&str>, status: &str) {
    let previous = previous.unwrap_or(STEP_OK);
    if previous == status {
        return;
    }
    if status == STEP_OK {
        tracing::info!(?step, previous = %previous, "Owner daemon executor step recovered");
    } else if status == HISTORICAL_EVIDENCE_MISSING {
        tracing::info!(?step, "Owner daemon shared step ended in HistoricalEvidenceMissing");
    } else {
        tracing::info!(?step, code = %status, "Owner daemon executor step status changed");
    }
}

/// Outcome of one pass, for health reporting only.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct PassReport {
    pub ran: usize,
    pub failed: usize,
    pub skipped_for_admission: usize,
    pub last_error: Option<String>,
}

pub async fn pass(
    io: &(impl PassIo + ?Sized),
    cursors: &mut Cursors,
    live: impl Fn() -> bool,
    admission: &Admission,
) -> PassReport {
    let mut report = PassReport::default();
    for step in ORDER {
        if !live() {
            return report;
        }
        if !io.enabled(step) {
            continue;
        }
        let guard = if step.admits_work() {
            match admission.enter() {
                Some(guard) => Some(guard),
                None => {
                    report.skipped_for_admission += 1;
                    continue;
                }
            }
        } else {
            None
        };
        let result = io.run(step, cursors.get(step)).await;
        drop(guard);
        report.ran += 1;
        // A retired host records nothing from a late reply.
        if !live() {
            return report;
        }
        let status = match &result {
            Ok(advance) => advance.note.clone().unwrap_or_else(|| STEP_OK.into()),
            Err(error) => error.code.clone(),
        };
        report_change(step, cursors.status(step), &status);
        cursors.1.insert(step, status);
        match result {
            Ok(advance) => {
                cursors.0.insert(step, advance.cursor);
            }
            Err(error) => {
                tracing::debug!(?step, status = error.status, code = %error.code, "Owner daemon executor step awaits recovery");
                report.failed += 1;
                report.last_error = Some(error.code);
            }
        }
    }
    report
}

/// Retire/wake control and retained task custody for one worker.
#[derive(Default)]
pub struct Control {
    pub retired: AtomicBool,
    pub wake: tokio::sync::Notify,
}

#[derive(Default)]
pub struct Worker {
    pub control: Arc<Control>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Worker {
    pub fn retire(&self) {
        self.control.retired.store(true, Ordering::Release);
        self.control.wake.notify_one();
    }
    pub fn retired(&self) -> bool {
        self.control.retired.load(Ordering::Acquire)
    }
    pub fn wake(&self) {
        self.control.wake.notify_one();
    }
    pub async fn install(&self, task: tokio::task::JoinHandle<()>) {
        *self.task.lock().await = Some(task);
    }
    /// A cancelled waiter leaves the original handle for the next drain;
    /// in-flight receipt transactions finish before the root is released.
    pub async fn drain(&self) {
        let mut task = self.task.lock().await;
        if let Some(handle) = task.as_mut() {
            let _ = handle.await;
        }
        task.take();
    }
}

pub async fn idle(control: &Control, period: Duration) {
    tokio::select! {
        _ = control.wake.notified() => {},
        _ = tokio::time::sleep(period) => {},
    }
}

#[cfg(feature = "resource-adoption")]
pub mod native {
    //! Real executor bindings. Compiled with the adopted-resource features
    //! that Hagency Desktop already uses for pool hosting.
    use super::*;
    use crate::native_owner::{
        CurrentAllocationExecutor, CurrentProfileSelection, CurrentSharedExecutor,
        AgentSettlementProgress, AllocationProgress, ManagedAgentProgress, ManagedMatrixProgress,
        ManagementPackage, NativeOwner, SharedDemandProgress, SharedExecutionProgress,
    };

    fn progress(after: Option<String>, p: SharedExecutionProgress) -> Option<String> {
        match p {
            SharedExecutionProgress::Running { command_id }
            | SharedExecutionProgress::Stopping { command_id }
            | SharedExecutionProgress::HistoricalEvidenceMissing { command_id }
            | SharedExecutionProgress::Recorded { command_id, .. } => Some(command_id),
            SharedExecutionProgress::Idle => after,
        }
    }

    /// Logging note for a shared outcome; never the command identifier.
    fn shared_note(p: &SharedExecutionProgress) -> Option<String> {
        matches!(p, SharedExecutionProgress::HistoricalEvidenceMissing { .. })
            .then(|| HISTORICAL_EVIDENCE_MISSING.into())
    }

    /// A demand step advances its cursor even when the selected demand's own
    /// operation fails. Report that operation's code instead of dropping it.
    fn demand_note(outcome: &Result<SharedDemandProgress, NativeError>) -> Option<String> {
        match outcome {
            Err(error) => Some(error.code.clone()),
            Ok(SharedDemandProgress::Execution { progress, .. }) => shared_note(progress),
            Ok(_) => None,
        }
    }

    impl PassIo for CurrentSharedExecutor {
        fn enabled(&self, step: Step) -> bool {
            match step {
                Step::DemandStop
                | Step::IntakeDemandUnfinished
                | Step::IntakeDemandRecover
                | Step::IntakeDemand => {
                    cfg!(feature = "shared-intake")
                }
                Step::BudgetDelta
                | Step::BudgetDemandRecover
                | Step::BudgetProcess
                | Step::BudgetDemand => cfg!(feature = "shared-budget-execution"),
                Step::SuccessorResume | Step::SuccessorPoll => cfg!(feature = "shared-successors"),
                Step::Agent | Step::Matrix | Step::Runtime | Step::Settlement => true,
            }
        }
        fn run(&self, step: Step, after: Option<String>) -> StepFuture<'_> {
            Box::pin(async move {
                let mut note = None;
                let cursor = match step {
                    Step::DemandStop => self
                        .resume_next_demand_stop(after)
                        .await
                        .map(|s| s.map(|v| v.command_id)),
                    Step::IntakeDemandUnfinished => {
                        self.resume_next_unfinished_demand(after).await.map(|s| {
                            s.map(|v| {
                                note = demand_note(&v.outcome);
                                v.demand_id
                            })
                        })
                    }
                    Step::IntakeDemandRecover => {
                        self.resume_next_demand(after).await.map(|s| {
                            s.map(|v| {
                                note = demand_note(&v.outcome);
                                v.demand_id
                            })
                        })
                    }
                    Step::IntakeDemand => self.poll_demand(after).await.map(|s| {
                        note = demand_note(&s.outcome);
                        s.next_after
                    }),
                    #[cfg(feature = "shared-budget-execution")]
                    Step::BudgetDelta => self.poll_budget(after).await.map(|s| {
                        use crate::native_owner::AgentBudgetProgress as B;
                        match s {
                            B::Idle => None,
                            B::HistoricalReceiptMissing { command_id }
                            | B::Recorded { command_id, .. } => Some(command_id),
                        }
                    }),
                    #[cfg(feature = "shared-budget-execution")]
                    Step::BudgetDemandRecover => {
                        self.resume_next_budget_demand(after).await.map(|s| {
                            s.map(|v| {
                                note = demand_note(&v.outcome);
                                v.demand_id
                            })
                        })
                    }
                    #[cfg(feature = "shared-budget-execution")]
                    Step::BudgetProcess => {
                        self.poll_budget_successor(after.clone()).await.map(|p| {
                            note = shared_note(&p);
                            match p {
                                SharedExecutionProgress::Idle => None,
                                p => progress(None, p),
                            }
                        })
                    }
                    #[cfg(feature = "shared-budget-execution")]
                    Step::BudgetDemand => self.poll_budget_demand(after).await.map(|s| {
                        note = demand_note(&s.outcome);
                        s.next_after
                    }),
                    #[cfg(not(feature = "shared-budget-execution"))]
                    Step::BudgetDelta
                    | Step::BudgetDemandRecover
                    | Step::BudgetProcess
                    | Step::BudgetDemand => Ok(after),
                    Step::Agent => self.poll_agent(after.clone()).await.map(|p| match p {
                        ManagedAgentProgress::HistoricalReceiptMissing { command_id }
                        | ManagedAgentProgress::Recorded { command_id, .. } => Some(command_id),
                        ManagedAgentProgress::Idle => after,
                    }),
                    Step::Matrix => self.poll_matrix(after.clone()).await.map(|p| match p {
                        ManagedMatrixProgress::HistoricalObservationMissing { command_id }
                        | ManagedMatrixProgress::Observed { command_id, .. } => Some(command_id),
                        ManagedMatrixProgress::Idle => after,
                    }),
                    Step::Runtime => self.poll(after.clone()).await.map(|p| {
                        note = shared_note(&p);
                        progress(after, p)
                    }),
                    Step::SuccessorResume => match self.resume_next_successor(after).await {
                        Ok(Some(result)) => Ok(Some(result.intent_id)),
                        Ok(None) => Ok(None),
                        Err(e) => Err(e),
                    },
                    Step::SuccessorPoll => self.poll_successor(after.clone()).await.map(|p| {
                        note = shared_note(&p);
                        progress(after, p)
                    }),
                    Step::Settlement => {
                        self.poll_settlement(after.clone()).await.map(|p| match p {
                            AgentSettlementProgress::HistoricalReceiptMissing { command_id }
                            | AgentSettlementProgress::Recorded { command_id, .. } => {
                                Some(command_id)
                            }
                            AgentSettlementProgress::Idle => after,
                        })
                    }
                };
                cursor.map(|cursor| Advance { cursor, note })
            })
        }
    }

    pub type Gate = Arc<dyn Fn() -> bool + Send + Sync>;

    fn package(digest: &[u8], epoch: u64) -> ManagementPackage {
        use sha2::{Digest, Sha256};
        ManagementPackage {
            digest: format!("{:x}", Sha256::digest(digest)),
            instance: 1,
            account_epoch: epoch,
            // Each sealed factory installs exactly its own native permission.
            permissions: vec![],
        }
    }

    /// Shared-execution loop. Mirrors Desktop's `shared_worker::run`.
    #[allow(clippy::too_many_arguments)]
    pub async fn run_shared(
        owner: NativeOwner,
        selection: CurrentProfileSelection,
        control: Arc<Control>,
        gate: Gate,
        admission: Arc<Admission>,
        epoch: u64,
        health: Arc<std::sync::Mutex<Option<String>>>,
        executor_slot: Arc<tokio::sync::Mutex<Option<Arc<CurrentSharedExecutor>>>>,
    ) {
        let live = || !control.retired.load(Ordering::Acquire) && gate();
        let mut cursors = Cursors::default();
        let mut grant_access: Option<crate::native_owner::OwnerGrantAccess> = None;
        let mut next_renewal = std::time::Instant::now();
        let mut renewal_error = None;
        while live() {
            if std::time::Instant::now() >= next_renewal {
                if let Some(_admission) = admission.enter() {
                    if grant_access.as_ref().is_none_or(|a| a.retired()) {
                        let control = Arc::downgrade(&control);
                        let gate = gate.clone();
                        grant_access = match selection.grant_access(&owner, package(b"hagency-owner-daemon-calendar-worker-v1", epoch)).await {
                            Ok(access) => access.restrict_to_host(move || control.upgrade().is_some_and(|c| !c.retired.load(Ordering::Acquire)) && gate()).ok(),
                            Err(error) => { renewal_error = Some(error.code); None }
                        };
                    }
                    if let Some(access) = &grant_access {
                        renewal_error = match access.renew_recurring().await {
                            Ok(_) => None,
                            Err(error) => {
                                tracing::debug!(code=%error.code,"Recurring capacity awaits authorization or settlement");
                                Some(error.code)
                            }
                        };
                    }
                }
                next_renewal = std::time::Instant::now() + Duration::from_secs(30);
            }
            let executor = {
                let mut slot = executor_slot.lock().await;
                if slot.is_none() && live() {
                    let worker_gate: Gate = {
                        let control = Arc::downgrade(&control);
                        let gate = gate.clone();
                        Arc::new(move || {
                            control
                                .upgrade()
                                .is_some_and(|c| !c.retired.load(Ordering::Acquire))
                                && gate()
                        })
                    };
                    match selection
                        .shared_executor(
                            &owner,
                            package(b"hagency-owner-daemon-shared-worker-v1", epoch),
                            worker_gate,
                        )
                        .await
                    {
                        Ok(executor) if live() => *slot = Some(Arc::new(executor)),
                        Ok(executor) => executor.retire(),
                        Err(error) => {
                            *health.lock().unwrap_or_else(|p| p.into_inner()) = Some(error.code);
                        }
                    }
                }
                slot.clone()
            };
            if let Some(executor) = executor.as_ref() {
                let report = pass(executor.as_ref(), &mut cursors, live, &admission).await;
                *health.lock().unwrap_or_else(|p| p.into_inner()) = report.last_error.or_else(|| renewal_error.clone());
            }
            if !live() {
                break;
            }
            idle(&control, Duration::from_secs(5)).await;
        }
        if let Some(access) = grant_access { access.retire(); }
        if let Some(executor) = executor_slot.lock().await.take() {
            executor.retire();
        }
    }

    /// Allocation loop. Mirrors Desktop's `allocation_worker::run`. Inventory
    /// and receipts continue while admission is paused.
    pub async fn run_allocation(
        owner: NativeOwner,
        selection: CurrentProfileSelection,
        control: Arc<Control>,
        gate: Gate,
        epoch: u64,
        health: Arc<std::sync::Mutex<Option<String>>>,
    ) {
        let live = || !control.retired.load(Ordering::Acquire) && gate();
        let mut allocation: Option<String> = None;
        let mut settlement: Option<String> = None;
        let mut executor: Option<CurrentAllocationExecutor> = None;
        let advance = |cursor: &mut Option<String>, r: Result<AllocationProgress, NativeError>| {
            match r {
                Ok(
                    AllocationProgress::Recorded { command_id, .. }
                    | AllocationProgress::HistoricalReceiptMissing { command_id },
                ) => *cursor = Some(command_id),
                Ok(AllocationProgress::Idle) => {}
                Err(_) => {}
            }
        };
        while live() {
            if executor.is_none() {
                match selection
                    .allocation_executor(
                        &owner,
                        package(b"hagency-owner-daemon-allocation-worker-v1", epoch),
                        gate.clone(),
                    )
                    .await
                {
                    Ok(e) => executor = Some(e),
                    Err(error) => {
                        *health.lock().unwrap_or_else(|p| p.into_inner()) = Some(error.code);
                    }
                }
            }
            if let Some(executor) = executor.as_ref() {
                let result = executor.poll_allocation(allocation.clone()).await;
                let failed = result.as_ref().err().map(|e| e.code.clone());
                advance(&mut allocation, result);
                if live() {
                    let result = executor.poll_parent_settlement(settlement.clone()).await;
                    advance(&mut settlement, result);
                }
                *health.lock().unwrap_or_else(|p| p.into_inner()) = failed;
            }
            if !live() {
                break;
            }
            idle(&control, Duration::from_secs(5)).await;
        }
        if let Some(executor) = executor {
            executor.retire();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Fixture {
        live: AtomicBool,
        calls: Mutex<Vec<(Step, Option<String>)>>,
        fail: Option<Step>,
        note: Option<Step>,
        retire_at: Option<Step>,
        hold: Option<(Step, Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    }
    impl Fixture {
        fn new() -> Self {
            Self {
                live: AtomicBool::new(true),
                calls: Mutex::new(Vec::new()),
                fail: None,
                note: None,
                retire_at: None,
                hold: None,
            }
        }
        fn steps(&self) -> Vec<Step> {
            self.calls.lock().unwrap().iter().map(|(s, _)| *s).collect()
        }
    }
    impl PassIo for Fixture {
        fn enabled(&self, _step: Step) -> bool {
            true
        }
        fn run(&self, step: Step, after: Option<String>) -> StepFuture<'_> {
            Box::pin(async move {
                self.calls.lock().unwrap().push((step, after));
                if let Some((s, entered, release)) = &self.hold
                    && *s == step
                {
                    entered.notify_one();
                    release.notified().await;
                }
                if self.retire_at == Some(step) {
                    self.live.store(false, Ordering::Release);
                }
                if self.fail == Some(step) {
                    return Err(NativeError {
                        status: 503,
                        code: "server_unavailable".into(),
                    });
                }
                Ok(Advance {
                    cursor: Some(format!("{step:?}-cursor")),
                    note: (self.note == Some(step)).then(|| HISTORICAL_EVIDENCE_MISSING.into()),
                })
            })
        }
    }

    #[tokio::test]
    async fn open_admission_runs_every_step_in_desktop_order_with_own_cursors() {
        let io = Fixture::new();
        let admission = Admission::default();
        admission.open();
        let mut cursors = Cursors::default();
        let live = || io.live.load(Ordering::Acquire);
        pass(&io, &mut cursors, live, &admission).await;
        assert_eq!(io.steps(), ORDER.to_vec());
        pass(&io, &mut cursors, live, &admission).await;
        let calls = io.calls.lock().unwrap();
        for (step, after) in &calls[ORDER.len()..] {
            assert_eq!(after.as_deref(), Some(format!("{step:?}-cursor").as_str()));
        }
    }

    #[tokio::test]
    async fn paused_admission_skips_only_new_work_and_keeps_recovery_and_settlement() {
        let io = Fixture::new();
        let admission = Admission::default();
        let mut cursors = Cursors::default();
        let report = pass(&io, &mut cursors, || true, &admission).await;
        let expected: Vec<Step> = ORDER.into_iter().filter(|s| !s.admits_work()).collect();
        assert_eq!(io.steps(), expected);
        assert_eq!(report.skipped_for_admission, 6);
        for step in [Step::DemandStop, Step::Runtime, Step::Settlement, Step::Agent, Step::SuccessorPoll] {
            assert!(io.steps().contains(&step));
        }
        for step in [Step::IntakeDemand, Step::IntakeDemandUnfinished, Step::IntakeDemandRecover, Step::BudgetDemand, Step::SuccessorResume] {
            assert!(!io.steps().contains(&step));
            assert!(cursors.get(step).is_none());
        }
    }

    #[tokio::test]
    async fn failure_keeps_previous_cursor_and_retirement_records_nothing_late() {
        let mut io = Fixture::new();
        io.fail = Some(Step::Runtime);
        let admission = Admission::default();
        admission.open();
        let mut cursors = Cursors::default();
        cursors.0.insert(Step::Runtime, Some("original".into()));
        let report = pass(&io, &mut cursors, || true, &admission).await;
        assert_eq!(cursors.get(Step::Runtime).as_deref(), Some("original"));
        assert_eq!(report.last_error.as_deref(), Some("server_unavailable"));
        for (index, step) in ORDER.into_iter().enumerate() {
            let mut io = Fixture::new();
            io.retire_at = Some(step);
            let mut cursors = Cursors::default();
            pass(&io, &mut cursors, || io.live.load(Ordering::Acquire), &admission).await;
            assert_eq!(io.steps().len(), index + 1, "{step:?}");
            assert!(cursors.get(step).is_none(), "{step:?}");
        }
    }

    #[tokio::test]
    async fn step_status_tracks_errors_and_notes_and_returns_to_ok() {
        let admission = Admission::default();
        admission.open();
        let mut cursors = Cursors::default();
        let mut io = Fixture::new();
        io.fail = Some(Step::IntakeDemand);
        io.note = Some(Step::SuccessorPoll);
        let report = pass(&io, &mut cursors, || true, &admission).await;
        assert_eq!(report.failed, 1);
        assert_eq!(cursors.status(Step::IntakeDemand), Some("server_unavailable"));
        // A note is not a failure: the cursor still advances.
        assert_eq!(cursors.status(Step::SuccessorPoll), Some(HISTORICAL_EVIDENCE_MISSING));
        assert_eq!(cursors.get(Step::SuccessorPoll).as_deref(), Some("SuccessorPoll-cursor"));
        assert_eq!(cursors.status(Step::Runtime), Some(STEP_OK));
        let io = Fixture::new();
        let report = pass(&io, &mut cursors, || true, &admission).await;
        assert_eq!(report.failed, 0);
        for step in ORDER {
            assert_eq!(cursors.status(step), Some(STEP_OK), "{step:?}");
        }
    }

    #[tokio::test]
    async fn closing_admission_waits_for_in_flight_admission_without_cancelling_it() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut io = Fixture::new();
        io.hold = Some((Step::IntakeDemand, entered.clone(), release.clone()));
        let io = Arc::new(io);
        let admission = Arc::new(Admission::default());
        admission.open();
        let task = {
            let (io, admission) = (io.clone(), admission.clone());
            tokio::spawn(async move {
                let mut cursors = Cursors::default();
                pass(io.as_ref(), &mut cursors, || true, &admission).await;
                cursors
            })
        };
        entered.notified().await;
        // In flight: a bounded close reports that quiescence was not reached.
        assert!(!admission.close(Duration::from_millis(30)).await);
        assert!(!admission.is_open());
        release.notify_one();
        let cursors = task.await.unwrap();
        // The in-flight operation completed and kept its cursor; later
        // admission steps in the same pass were skipped.
        assert_eq!(cursors.get(Step::IntakeDemand).as_deref(), Some("IntakeDemand-cursor"));
        assert!(!io.steps().contains(&Step::SuccessorResume));
        assert!(admission.close(Duration::from_millis(30)).await);
        assert!(admission.enter().is_none());
        admission.open();
        assert!(admission.enter().is_some());
    }

    #[tokio::test]
    async fn cancelled_drain_retains_the_original_task() {
        let worker = Worker::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(AtomicBool::new(false));
        let (r, f) = (release.clone(), finished.clone());
        worker
            .install(tokio::spawn(async move {
                r.notified().await;
                f.store(true, Ordering::Release);
            }))
            .await;
        worker.retire();
        assert!(tokio::time::timeout(Duration::from_millis(20), worker.drain()).await.is_err());
        assert!(!finished.load(Ordering::Acquire));
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), worker.drain()).await.unwrap();
        assert!(finished.load(Ordering::Acquire));
    }
}
