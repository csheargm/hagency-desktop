//! Explicit native shared execution. The mini-app never receives an executor,
//! execution permission, provider handle or private work journal.
use super::*;
use ::hagency::native_owner::{
    CurrentProfileSelection, CurrentSharedExecutor, ManagedAgentProgress, ManagedMatrixProgress,
    ManagementPackage, SharedExecutionProgress,
};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Default)]
struct Control {
    retired: AtomicBool,
    wake: tokio::sync::Notify,
    executor: tokio::sync::Mutex<Option<Arc<CurrentSharedExecutor>>>,
}

#[derive(Default)]
pub(super) struct Worker {
    control: Arc<Control>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Worker {
    pub(super) fn retire(&self) {
        self.control.retired.store(true, Ordering::Release);
        self.control.wake.notify_waiters();
        self.control.wake.notify_one();
    }

    // Keep custody even when an account-transition waiter is cancelled. Native
    // owner shutdown separately drains every retained provider process.
    pub(super) async fn drain(&self) {
        let mut task = self.task.lock().await;
        if let Some(handle) = task.as_mut() {
            let _ = handle.await;
        }
        task.take();
        if let Some(executor) = self.control.executor.lock().await.take() {
            executor.retire();
        }
    }
}

impl Service {
    pub(super) async fn start_shared_worker(&self, owner: &NativeOwner) {
        if self.check().is_err() || self.0.shared.control.retired.load(Ordering::Acquire) {
            return;
        }
        let mut task = self.0.shared.task.lock().await;
        if task.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return;
        }
        if let Some(handle) = task.as_mut() {
            let _ = handle.await;
        }
        task.take();
        let Ok(Some(selection)) = self.load_current_initialization().await else {
            return;
        };
        if self.check().is_err() || self.0.shared.control.retired.load(Ordering::Acquire) {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let owner = owner.clone();
        let control = self.0.shared.control.clone();
        *task = Some(crate::sliding_sync::backend_runtime().spawn(async move {
            run(weak, owner, selection, control).await;
        }));
    }
}

async fn run(
    weak: Weak<Inner>,
    owner: NativeOwner,
    selection: CurrentProfileSelection,
    control: Arc<Control>,
) {
    // Keep every lane in the original retained task: retiring/draining still
    // waits for each in-flight receipt. Both ordinary/successor intake and
    // additive-budget work must wake independently of periodic maintenance.
    tokio::join!(
        async {
            #[cfg(any(feature = "shared-intake", feature = "shared-successors"))]
            run_intake(weak.clone(), owner.clone(), selection.clone(), control.clone()).await;
        },
        async {
            #[cfg(feature = "shared-budget-execution")]
            run_budget(weak.clone(), owner.clone(), selection.clone(), control.clone()).await;
        },
        run_maintenance(weak.clone(), owner.clone(), selection.clone(), control.clone()),
    );
}

#[cfg(any(feature = "shared-intake", feature = "shared-successors"))]
async fn run_intake(
    weak: Weak<Inner>,
    owner: NativeOwner,
    selection: CurrentProfileSelection,
    control: Arc<Control>,
) {
    let live = || {
        !control.retired.load(Ordering::Acquire)
            && weak.upgrade().is_some_and(|inner| Service(inner).check().is_ok())
    };
    #[cfg(feature = "shared-intake")]
    let mut intake = super::shared_intake::Polling::default();
    #[cfg(feature = "shared-successors")]
    let mut successors = super::shared_successors::Polling::default();
    let mut changes = None;
    while live() {
        let next = tokio::time::Instant::now() + Duration::from_secs(5);
        if let Ok(executor) = acquire_executor(&weak, &owner, &selection, &control).await {
            let changes = changes.get_or_insert_with(|| executor.work_changes());
            changes.borrow_and_update();
            // Retain Stop recovery before demand discovery and saved successor
            // recovery. Never cancel a pass when another message arrives.
            #[cfg(feature = "shared-intake")]
            intake.poll(executor.as_ref(), live).await;
            #[cfg(feature = "shared-successors")]
            successors.poll(executor.as_ref(), live).await;
        }
        if !live() { break; }
        wait_for_work(&control, changes.as_mut(), next).await;
    }
}

#[cfg(feature = "shared-budget-execution")]
async fn run_budget(
    weak: Weak<Inner>,
    owner: NativeOwner,
    selection: CurrentProfileSelection,
    control: Arc<Control>,
) {
    let live = || {
        !control.retired.load(Ordering::Acquire)
            && weak
                .upgrade()
                .is_some_and(|inner| Service(inner).check().is_ok())
    };
    let mut polling = super::shared_budget::Polling::default();
    let mut changes = None;
    while live() {
        // Matrix messages and room keys wake this lane immediately. Use five
        // seconds only as a fallback for server-side changes without a sync hint.
        let next = tokio::time::Instant::now() + Duration::from_secs(5);
        if let Ok(executor) = acquire_executor(&weak, &owner, &selection, &control).await {
            let changes = changes.get_or_insert_with(|| executor.work_changes());
            changes.borrow_and_update();
            // Original Stop recovery still precedes budget and demand work.
            // Never cancel a poll when another message arrives: it may already
            // have admitted a turn or be persisting its original receipt.
            polling.poll(executor.as_ref(), live).await;
        }
        if !live() {
            break;
        }
        wait_for_work(&control, changes.as_mut(), next).await;
    }
}

#[cfg(any(feature = "shared-budget-execution", feature = "shared-intake", feature = "shared-successors"))]
async fn wait_for_work(
    control: &Control,
    changes: Option<&mut tokio::sync::watch::Receiver<()>>,
    next: tokio::time::Instant,
) {
    if control.retired.load(Ordering::Acquire) {
        return;
    }
    let changed = async {
        if let Some(changes) = changes {
            if changes.changed().await.is_ok() {
                return;
            }
        }
        std::future::pending::<()>().await;
    };
    tokio::select! {
        _ = control.wake.notified() => {},
        _ = changed => {},
        _ = tokio::time::sleep_until(next) => {},
    }
}

async fn run_maintenance(
    weak: Weak<Inner>,
    owner: NativeOwner,
    selection: CurrentProfileSelection,
    control: Arc<Control>,
) {
    let gate_weak = weak.clone();
    let gate: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        gate_weak
            .upgrade()
            .is_some_and(|inner| Service(inner).check().is_ok())
    });
    let mut agent_cursor = None;
    let mut matrix_cursor = None;
    let mut runtime_cursor = None;
    let mut settlement_cursor = None;
    #[cfg(feature = "shared-intake")]
    let mut stop_cursor = None;
    loop {
        if control.retired.load(Ordering::Acquire) || !gate() {
            break;
        }
        let executor = acquire_executor(&weak, &owner, &selection, &control)
            .await
            .ok();
        // The original poll finishes before retirement drains its handle. The
        // native engine retains spawned work, receipt retries and stop evidence;
        // this worker must never restart a model turn after a missing response.
        if let Some(executor) = executor.as_ref() {
            // Original provisioning still reconciles saved Stop commands first.
            // Demand/successor recovery has its own cursors in the waking lane.
            #[cfg(feature = "shared-intake")]
            if let Ok(next) = executor.resume_next_demand_stop(stop_cursor.clone()).await {
                stop_cursor = next.map(|saved| saved.command_id);
            }
            if control.retired.load(Ordering::Acquire) || !gate() { break; }
            // Agent reserve/fence and Matrix readiness are prerequisites for
            // runtime delivery. Each uses its own original-command cursor.
            let result = executor.poll_agent(agent_cursor.clone()).await;
            advance_agent(&mut agent_cursor, result);
            if !control.retired.load(Ordering::Acquire) && gate() {
                let result = executor.poll_matrix(matrix_cursor.clone()).await;
                advance_matrix(&mut matrix_cursor, result);
            }
            if !control.retired.load(Ordering::Acquire) && gate() {
                let result = executor.poll(runtime_cursor.clone()).await;
                advance(&mut runtime_cursor, result);
            }
            // Observe only after the native fence/process reconciliation in this
            // pass. Each receipt belongs to its original settlement command;
            // closing the mini app does not interrupt this native worker.
            if !control.retired.load(Ordering::Acquire) && gate() {
                let result = executor.poll_settlement(settlement_cursor.clone()).await;
                advance_settlement(&mut settlement_cursor, result);
            }
        }
        if control.retired.load(Ordering::Acquire) || !gate() {
            break;
        }
        tokio::select! {
            _ = control.wake.notified() => {},
            _ = tokio::time::sleep(Duration::from_secs(5)) => {},
        }
    }
}

fn advance_settlement(
    cursor: &mut Option<String>,
    result: Result<::hagency::native_owner::AgentSettlementProgress, NativeError>,
) {
    use ::hagency::native_owner::AgentSettlementProgress;
    match result {
        Ok(
            AgentSettlementProgress::HistoricalReceiptMissing { command_id }
            | AgentSettlementProgress::Recorded { command_id, .. },
        ) => *cursor = Some(command_id),
        Ok(AgentSettlementProgress::Idle) | Err(_) => {}
    }
}

fn advance_agent(cursor: &mut Option<String>, result: Result<ManagedAgentProgress, NativeError>) {
    match result {
        Ok(
            ManagedAgentProgress::HistoricalReceiptMissing { command_id }
            | ManagedAgentProgress::Recorded { command_id, .. },
        ) => *cursor = Some(command_id),
        Ok(ManagedAgentProgress::Idle) | Err(_) => {}
    }
}

fn advance_matrix(cursor: &mut Option<String>, result: Result<ManagedMatrixProgress, NativeError>) {
    match result {
        Ok(
            ManagedMatrixProgress::HistoricalObservationMissing { command_id }
            | ManagedMatrixProgress::Observed { command_id, .. },
        ) => *cursor = Some(command_id),
        Ok(ManagedMatrixProgress::Idle) | Err(_) => {}
    }
}

pub(super) fn advance(
    cursor: &mut Option<String>,
    result: Result<SharedExecutionProgress, NativeError>,
) {
    match result {
        Ok(
            SharedExecutionProgress::Running { command_id }
            | SharedExecutionProgress::Stopping { command_id }
            | SharedExecutionProgress::HistoricalEvidenceMissing { command_id }
            | SharedExecutionProgress::Recorded { command_id, .. },
        ) => *cursor = Some(command_id),
        // The server rotates over original assignments, including pending
        // cleanup. An opaque cursor is neither completion nor execution proof.
        Ok(SharedExecutionProgress::Idle) | Err(_) => {}
    }
}

async fn acquire_executor(
    weak: &Weak<Inner>,
    owner: &NativeOwner,
    selection: &CurrentProfileSelection,
    control: &Arc<Control>,
) -> Result<Arc<CurrentSharedExecutor>, NativeError> {
    let mut saved = control.executor.lock().await;
    let inner = weak
        .upgrade()
        .ok_or_else(|| source_error("matrix_account_changed"))?;
    let service = Service(inner);
    service.check().map_err(|e| source_error(&e))?;
    if control.retired.load(Ordering::Acquire) {
        return Err(source_error("shared_execution_closed"));
    }
    if let Some(executor) = saved.as_ref() {
        return Ok(executor.clone());
    }
    let epoch = service
        .0
        .epoch
        .checked_add(1)
        .ok_or_else(|| source_error("matrix_account_changed"))?;
    let gate_weak = weak.clone();
    let gate_control = Arc::downgrade(control);
    let gate: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        gate_control
            .upgrade()
            .is_some_and(|c| !c.retired.load(Ordering::Acquire))
            && gate_weak
                .upgrade()
                .is_some_and(|inner| Service(inner).check().is_ok())
    });
    let package = ManagementPackage {
        digest: blake3::hash(b"hagency-desktop-native-shared-worker-v1")
            .to_hex()
            .to_string(),
        instance: 1,
        account_epoch: epoch,
        permissions: vec![],
    };
    let executor = Arc::new(selection.shared_executor(owner, package, gate).await?);
    if let Err(e) = service.check() {
        executor.retire();
        return Err(source_error(&e));
    }
    if control.retired.load(Ordering::Acquire) {
        executor.retire();
        return Err(source_error("shared_execution_closed"));
    }
    *saved = Some(executor.clone());
    Ok(executor)
}
#[cfg(feature = "shared-successors")]
impl Service {
    pub(super) async fn shared_successor_executor(
        &self,
    ) -> Result<Arc<CurrentSharedExecutor>, String> {
        self.check()?;
        self.verify_matrix(false).await?;
        let owner = self.native_owner().await?;
        let selection = self
            .load_current_initialization()
            .await?
            .ok_or("current_profile_required")?;
        let executor = acquire_executor(
            &Arc::downgrade(&self.0),
            &owner,
            &selection,
            &self.0.shared.control,
        )
        .await
        .map_err(|e| e.code)?;
        self.check()?;
        Ok(executor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "shared-intake", feature = "shared-budget-execution"))]
    #[tokio::test]
    async fn one_message_wakes_intake_and_budget_while_maintenance_receipt_stays_retained() {
        let worker = Worker::default();
        let control = worker.control.clone();
        let (changed, mut intake) = tokio::sync::watch::channel(());
        let mut budget = changed.subscribe();
        intake.borrow_and_update();
        budget.borrow_and_update();
        let (intake_done, intake_ready) = tokio::sync::oneshot::channel();
        let (budget_done, budget_ready) = tokio::sync::oneshot::channel();
        let (finish_maintenance, maintenance) = tokio::sync::oneshot::channel();
        *worker.task.lock().await = Some(tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            tokio::join!(
                async {
                    wait_for_work(&control, Some(&mut intake), deadline).await;
                    intake_done.send(()).unwrap();
                },
                async {
                    wait_for_work(&control, Some(&mut budget), deadline).await;
                    budget_done.send(()).unwrap();
                },
                async { maintenance.await.unwrap(); },
            );
        }));
        changed.send_replace(());
        tokio::time::timeout(Duration::from_secs(1), async {
            intake_ready.await.unwrap();
            budget_ready.await.unwrap();
        }).await.unwrap();
        assert!(!worker.task.lock().await.as_ref().unwrap().is_finished());
        worker.retire();
        assert!(tokio::time::timeout(Duration::from_millis(20), worker.drain()).await.is_err());
        finish_maintenance.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker.drain()).await.unwrap();
        assert!(worker.task.lock().await.is_none());
    }

    #[cfg(feature = "shared-budget-execution")]
    #[tokio::test]
    async fn room_key_arriving_during_poll_wakes_next_pass_without_losing_signal() {
        let control = Control::default();
        let (changed, mut updates) = tokio::sync::watch::channel(());
        updates.borrow_and_update();
        changed.send_replace(());
        tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_work(
                &control,
                Some(&mut updates),
                tokio::time::Instant::now() + Duration::from_secs(10),
            ),
        )
        .await
        .unwrap();
        assert!(!updates.has_changed().unwrap());
    }

    #[cfg(feature = "shared-budget-execution")]
    #[tokio::test]
    async fn slow_poll_and_retirement_do_not_add_another_sleep() {
        let control = Control::default();
        tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_work(
                &control,
                None,
                tokio::time::Instant::now() - Duration::from_secs(1),
            ),
        )
        .await
        .unwrap();
        control.retired.store(true, Ordering::Release);
        tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_work(
                &control,
                None,
                tokio::time::Instant::now() + Duration::from_secs(10),
            ),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn cancelled_drain_keeps_the_original_poll_task() {
        let worker = Worker::default();
        let release = Arc::new(tokio::sync::Notify::new());
        let finished = Arc::new(AtomicBool::new(false));
        let task_release = release.clone();
        let task_finished = finished.clone();
        *worker.task.lock().await = Some(tokio::spawn(async move {
            task_release.notified().await;
            task_finished.store(true, Ordering::Release);
        }));
        worker.retire();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), worker.drain())
                .await
                .is_err()
        );
        assert!(worker.task.lock().await.is_some());
        assert!(!finished.load(Ordering::Acquire));
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(1), worker.drain())
            .await
            .unwrap();
        assert!(finished.load(Ordering::Acquire));
        assert!(worker.task.lock().await.is_none());
    }

    #[test]
    fn circular_polling_advances_running_and_cleanup_without_inventing_receipts() {
        let mut cursor = None;
        for result in [
            SharedExecutionProgress::Running {
                command_id: "running".into(),
            },
            SharedExecutionProgress::Stopping {
                command_id: "cleanup".into(),
            },
            SharedExecutionProgress::HistoricalEvidenceMissing {
                command_id: "missing".into(),
            },
            SharedExecutionProgress::Recorded {
                command_id: "recorded".into(),
                observation_id: "original".into(),
                process_state: Some("stopped".into()),
            },
        ] {
            let expected = match &result {
                SharedExecutionProgress::Running { command_id }
                | SharedExecutionProgress::Stopping { command_id }
                | SharedExecutionProgress::HistoricalEvidenceMissing { command_id }
                | SharedExecutionProgress::Recorded { command_id, .. } => command_id.clone(),
                SharedExecutionProgress::Idle => unreachable!(),
            };
            advance(&mut cursor, Ok(result));
            assert_eq!(cursor.as_deref(), Some(expected.as_str()));
        }
        advance(&mut cursor, Ok(SharedExecutionProgress::Idle));
        advance(&mut cursor, Err(source_error("server_unavailable")));
        assert_eq!(cursor.as_deref(), Some("recorded"));
    }

    #[test]
    fn prerequisite_cursors_keep_exact_receipts_and_failed_readiness_observations() {
        let mut agent = Some("agent-original".into());
        let mut matrix = Some("matrix-original".into());
        advance_agent(&mut agent, Ok(ManagedAgentProgress::Idle));
        advance_agent(&mut agent, Err(source_error("server_unavailable")));
        advance_matrix(&mut matrix, Ok(ManagedMatrixProgress::Idle));
        advance_matrix(&mut matrix, Err(source_error("server_unavailable")));
        assert_eq!(agent.as_deref(), Some("agent-original"));
        assert_eq!(matrix.as_deref(), Some("matrix-original"));
        advance_agent(
            &mut agent,
            Ok(ManagedAgentProgress::Recorded {
                command_id: "agent-reserved".into(),
                ledger_revision: 4,
            }),
        );
        advance_matrix(
            &mut matrix,
            Ok(ManagedMatrixProgress::Observed {
                command_id: "matrix-not-ready".into(),
                eligible: false,
            }),
        );
        assert_eq!(agent.as_deref(), Some("agent-reserved"));
        assert_eq!(matrix.as_deref(), Some("matrix-not-ready"));
        advance_agent(
            &mut agent,
            Ok(ManagedAgentProgress::HistoricalReceiptMissing {
                command_id: "agent-history".into(),
            }),
        );
        advance_matrix(
            &mut matrix,
            Ok(ManagedMatrixProgress::HistoricalObservationMissing {
                command_id: "matrix-history".into(),
            }),
        );
        assert_eq!(agent.as_deref(), Some("agent-history"));
        assert_eq!(matrix.as_deref(), Some("matrix-history"));
    }
}
