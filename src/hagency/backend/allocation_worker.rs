//! Retained original-device allocation worker. Its authority comes from the
//! published owner grant and fresh SDK/device checks, never a mini-app lease.
use super::*;
use ::hagency::native_owner::{CurrentProfileSelection, AllocationProgress, ManagementPackage};
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Default)]
struct RenewalWarnings(Option<(String, Instant)>);

impl RenewalWarnings {
    fn should_log(&mut self, code: &str, now: Instant) -> bool {
        if self.0.as_ref().is_some_and(|(previous, at)| previous == code && now.duration_since(*at) < Duration::from_secs(600)) {
            return false;
        }
        self.0 = Some((code.into(), now));
        true
    }
}

#[derive(Default)]
struct Control {
    retired: AtomicBool,
    wake: tokio::sync::Notify,
}

#[derive(Default)]
pub(super) struct Worker {
    control: Arc<Control>,
    task: tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Worker {
    pub(super) fn retire(&self) {
        self.control.retired.store(true, Ordering::Release);
        self.control.wake.notify_one();
    }

    /// A cancelled waiter must leave the original task handle in this slot.
    /// In-flight receipt transactions finish before the native root is released.
    pub(super) async fn drain(&self) {
        let mut task = self.task.lock().await;
        if let Some(handle) = task.as_mut() {
            let _ = handle.await;
        }
        task.take();
    }
}

impl Service {
    pub(super) async fn start_allocation_worker(&self, owner: &NativeOwner) {
        // Inventory remains usable on a replacement device or when the server
        // has disabled execution. Those conditions cannot create authority.
        if self.check().is_err() || self.0.allocations.control.retired.load(Ordering::Acquire) {
            return;
        }
        let mut task = self.0.allocations.task.lock().await;
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
        if self.check().is_err() || self.0.allocations.control.retired.load(Ordering::Acquire) {
            return;
        }
        let weak = Arc::downgrade(&self.0);
        let owner = owner.clone();
        let control = self.0.allocations.control.clone();
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
    let gate_weak = weak.clone();
    let gate: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
        gate_weak
            .upgrade()
            .is_some_and(|inner| Service(inner).check().is_ok())
    });
    let mut allocation_cursor = None;
    let mut settlement_cursor = None;
    let mut executor = None;
    let mut last_renewal: Option<std::time::Instant> = None;
    let mut renewal_warnings = RenewalWarnings::default();
    loop {
        if control.retired.load(Ordering::Acquire) || !gate() {
            break;
        }
        let Some(inner) = weak.upgrade() else {
            break;
        };
        let Some(epoch) = inner.epoch.checked_add(1) else {
            break;
        };
        drop(inner);
        if executor.is_none() {
            let package = ManagementPackage {
                digest: blake3::hash(b"hagency-desktop-native-allocation-worker-v1")
                    .to_hex()
                    .to_string(),
                instance: 1,
                account_epoch: epoch,
                permissions: vec![], // sealed factory installs only native allocation permissions
            };
            executor = selection
                .allocation_executor(&owner, package, gate.clone())
                .await
                .ok();
        }
        // Only the native owner worker can extend standing V4 consent. A new
        // calendar period never comes from the script clock or a UI timer.
        if last_renewal.is_none_or(|last| last.elapsed() >= Duration::from_secs(30)) {
            last_renewal = Some(std::time::Instant::now());
            let package = ManagementPackage {
                digest: blake3::hash(b"hagency-desktop-native-recurring-grant-worker-v1").to_hex().to_string(),
                instance: 1, account_epoch: epoch, permissions: vec![],
            };
            let grant_gate = gate.clone();
            let result = match selection.grant_access(&owner, package).await
                .and_then(|access| access.restrict_to_host(move || grant_gate())) {
                Ok(access) => {
                    let result = access.renew_recurring().await;
                    access.retire();
                    result.map(|_| ())
                }
                Err(error) => Err(error),
            };
            match result {
                Err(error) if renewal_warnings.should_log(&error.code, Instant::now()) => {
                    makepad_widgets::warning!("Recurring capacity renewal pending: {}", error.code);
                }
                Ok(()) => renewal_warnings = RenewalWarnings::default(),
                Err(_) => {},
            }
        }
        // No operation future is cancelled by a UI timeout or the wake signal.
        // The transport owns bounded network leases and durable receipt recovery.
        if let Some(executor) = executor.as_ref() {
            let result = executor.poll_allocation(allocation_cursor.clone()).await;
            advance(&mut allocation_cursor, result);
            if !control.retired.load(Ordering::Acquire) && gate() {
                let result = executor
                    .poll_parent_settlement(settlement_cursor.clone())
                    .await;
                advance(&mut settlement_cursor, result);
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

#[cfg(test)]
mod renewal_warning_tests {
    use super::*;
    #[test]
    fn recurring_failure_logs_on_change_and_at_bounded_intervals() {
        let mut warnings = RenewalWarnings::default();
        let now = Instant::now();
        assert!(warnings.should_log("offline", now));
        assert!(!warnings.should_log("offline", now + Duration::from_secs(30)));
        assert!(warnings.should_log("offline", now + Duration::from_secs(600)));
        assert!(warnings.should_log("scope_changed", now + Duration::from_secs(601)));
        warnings = RenewalWarnings::default();
        assert!(warnings.should_log("scope_changed", now + Duration::from_secs(602)));
    }
}

fn advance(cursor: &mut Option<String>, result: Result<AllocationProgress, NativeError>) {
    match result {
        Ok(
            AllocationProgress::Recorded { command_id, .. }
            | AllocationProgress::HistoricalReceiptMissing { command_id },
        ) => *cursor = Some(command_id),
        // A transport error cannot fabricate a cursor or imply ledger success.
        Ok(AllocationProgress::Idle) | Err(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_drain_retains_original_worker_until_it_finishes() {
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
    fn cursor_uses_only_the_original_delivered_command() {
        let mut cursor = Some("original".into());
        advance(&mut cursor, Err(source_error("server_unavailable")));
        advance(&mut cursor, Ok(AllocationProgress::Idle));
        assert_eq!(cursor.as_deref(), Some("original"));
        advance(
            &mut cursor,
            Ok(AllocationProgress::HistoricalReceiptMissing {
                command_id: "historical".into(),
            }),
        );
        assert_eq!(cursor.as_deref(), Some("historical"));
        advance(
            &mut cursor,
            Ok(AllocationProgress::Recorded {
                command_id: "next".into(),
                ledger_revision: 12,
            }),
        );
        assert_eq!(cursor.as_deref(), Some("next"));
    }
}
