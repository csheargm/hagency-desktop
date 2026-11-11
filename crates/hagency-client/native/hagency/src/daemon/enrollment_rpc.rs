//! Enrollment executes under the running daemon's existing home/SDK custody.
//! Controller closure only detaches from this workflow; it cannot drop an
//! accepted native registration future or unlock the daemon's Matrix store.
use super::{
    DaemonClient,
    host::{EnrollReport, HostError},
    new_request_id,
    protocol::{Command, Request},
};
use serde::{Deserialize, Serialize};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
pub enum Call {
    /// Enroll `owner` with its registered owner root (ADR-192).
    Start {
        origin: String,
        owner: String,
        display_name: Option<String>,
    },
    Status {
        id: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Status {
    pub id: String,
    pub authorization_url: Option<String>,
    pub result: Option<EnrollReport>,
    pub error: Option<String>,
}
/// Cancellation can end browser authorization, never an accepted native write.
/// Physical handles survive cancelled waiters until their drain is proven.
#[derive(Default)]
pub struct EnrollmentControl {
    phase: AtomicU8,
    cancelled: hagency_matrix::CancellationToken,
    registered: Mutex<Option<crate::native_owner::NativeOwner>>,
    custody: Mutex<Option<Arc<super::auth::SdkCustody>>>,
}
impl EnrollmentControl {
    /// Display-only observation: OAuth finished and native custody was accepted.
    /// It grants no authority and stays true while an accepted write drains.
    pub fn native_registration_started(&self) -> bool {
        self.phase.load(Ordering::Acquire) == 1
    }
    pub fn cancel(&self) {
        let _ = self
            .phase
            .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        self.cancelled.cancel();
    }
    pub async fn cancelled(&self) {
        self.cancelled.cancelled().await;
    }
    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }
    pub(super) fn accepting(&self) -> bool {
        self.phase.load(Ordering::Acquire) == 0 && !self.cancelled.is_cancelled()
    }
    pub(super) fn accept_native(&self) -> Result<(), &'static str> {
        self.phase
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| "daemon_enrollment_cancelled")
    }
    pub(super) fn set_custody(&self, custody: Arc<super::auth::SdkCustody>) {
        *self.custody.lock().unwrap_or_else(|p| p.into_inner()) = Some(custody);
    }
    pub(super) fn register(&self, owner: &crate::native_owner::NativeOwner) {
        *self.registered.lock().unwrap_or_else(|p| p.into_inner()) = Some(owner.clone());
    }
    fn retained(&self) -> bool {
        self.registered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }
    pub(super) async fn drain(&self, revoke: bool) -> Result<(), &'static str> {
        let owner = self
            .registered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(owner) = owner {
            if revoke {
                owner.shutdown_checked().await
            } else {
                owner.suspend().await
            }
            .map_err(|_| "native_owner_drain_required")?;
            self.registered
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .take();
        }
        if let Some(custody) = self
            .custody
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            custody.retire();
        }
        Ok(())
    }
}
pub(super) struct Workflow {
    pub status: Mutex<Status>,
    pub control: Arc<EnrollmentControl>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    done: AtomicBool,
    completed: tokio::sync::Notify,
}
struct Completed(Arc<Workflow>);
impl Drop for Completed {
    fn drop(&mut self) {
        let mut status = self.0.status.lock().unwrap_or_else(|p| p.into_inner());
        if status.result.is_none() && status.error.is_none() {
            status.error = Some("daemon_enrollment_outcome_unknown".into());
        }
        status.authorization_url = None;
        self.0.done.store(true, Ordering::Release);
        self.0.completed.notify_waiters();
    }
}
impl Workflow {
    pub fn spawn(self: &Arc<Self>, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let completed = Completed(self.clone());
        let task = tokio::spawn(async move {
            let _completed = completed;
            future.await;
        });
        *self.task.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
    }
    pub async fn drain(&self, revoke: bool) -> Result<(), &'static str> {
        self.control.cancel();
        loop {
            let notified = self.completed.notified();
            if self.done.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        self.control.drain(revoke).await
    }
}
#[derive(Default)]
pub(super) struct Flow {
    pub current: Option<(Call, Arc<Workflow>)>,
    closed: bool,
}
impl Flow {
    pub fn begin(&mut self, id: &str, call: &Call) -> Result<(Arc<Workflow>, bool), &'static str> {
        if self.closed {
            return Err("daemon_enrollment_closing");
        }
        if let Some((original, workflow)) = &self.current {
            let value = workflow
                .status
                .lock()
                .map_err(|_| "daemon_enrollment_unavailable")?;
            if value.id == id {
                return if original == call {
                    Ok((workflow.clone(), false))
                } else {
                    Err("request_id_reused")
                };
            }
            if !workflow.done.load(Ordering::Acquire) {
                return Err("daemon_enrollment_in_progress");
            }
            if workflow.control.retained() {
                return Err("native_owner_drain_required");
            }
        }
        let workflow = Arc::new(Workflow {
            status: Mutex::new(Status {
                id: id.into(),
                authorization_url: None,
                result: None,
                error: None,
            }),
            control: Arc::new(EnrollmentControl::default()),
            task: Mutex::new(None),
            done: AtomicBool::new(false),
            completed: tokio::sync::Notify::new(),
        });
        self.current = Some((call.clone(), workflow.clone()));
        Ok((workflow, true))
    }
    pub fn close(&mut self) -> Option<Arc<Workflow>> {
        self.closed = true;
        let active = self.current.as_ref().map(|(_, w)| w.clone());
        if let Some(w) = &active {
            w.control.cancel()
        }
        active
    }
    pub fn get(&self, id: &str) -> Result<Status, &'static str> {
        let (_, workflow) = self.current.as_ref().ok_or("daemon_enrollment_unknown")?;
        let status = workflow
            .status
            .lock()
            .map_err(|_| "daemon_enrollment_unavailable")?;
        if status.id != id {
            return Err("daemon_enrollment_unknown");
        }
        let mut status = status.clone();
        // The browser link is actionable only before native admission. Keeping
        // the wire shape unchanged also lets older clients keep polling.
        if workflow.control.native_registration_started() {
            status.authorization_url = None;
        }
        Ok(status)
    }
}
async fn request(home: &Path, id: &str, call: Call) -> Result<Status, HostError> {
    let mut client = DaemonClient::connect(home).await?;
    let reply = client
        .call(&Request::new(id, None, Command::Enrollment { call }))
        .await?;
    if reply.error.is_some() {
        return Err(HostError::Code("daemon_enrollment_unavailable"));
    }
    let result: Result<Status, String> = serde_json::from_value(
        reply
            .result
            .ok_or(HostError::Code("daemon_enrollment_unavailable"))?,
    )
    .map_err(|_| HostError::Code("daemon_enrollment_unavailable"))?;
    result.map_err(|_| HostError::Code("daemon_enrollment_unavailable"))
}
pub(super) async fn remote_enroll<F, Fut>(
    home: &Path,
    origin: &str,
    owner: &str,
    display_name: Option<String>,
    control: Arc<EnrollmentControl>,
    open: F,
) -> Result<EnrollReport, HostError>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), ()>>,
{
    let id = new_request_id();
    let mut open = Some(open);
    let mut status = request(
        home,
        &id,
        Call::Start {
            origin: origin.into(),
            owner: owner.into(),
            display_name,
        },
    )
    .await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(360);
    loop {
        let browser_pending = status.authorization_url.is_some();
        if let Some(url) = status.authorization_url.take() {
            if let Some(open) = open.take() {
                open(url)
                    .await
                    .map_err(|_| HostError::Code("daemon_enrollment_detached"))?;
            }
        }
        if !browser_pending && open.is_none() && status.result.is_none() && status.error.is_none() {
            let _ = control.accept_native();
        }
        if let Some(result) = status.result {
            return Ok(result);
        }
        if status.error.is_some() {
            return Err(HostError::Code("daemon_enrollment_failed"));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(HostError::Code("daemon_enrollment_pending"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        status = request(home, &new_request_id(), Call::Status { id: id.clone() }).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    fn call() -> Call {
        Call::Start {
            origin: "https://test/".into(),
            owner: "@owner:test".into(),
            display_name: None,
        }
    }
    #[test]
    fn browser_url_is_retired_after_native_admission_without_changing_rpc_shape() {
        let mut flow = Flow::default();
        let (work, _) = flow.begin("original", &call()).unwrap();
        work.status.lock().unwrap().authorization_url = Some("https://test/authorize".into());
        assert!(!work.control.native_registration_started());
        assert!(flow.get("original").unwrap().authorization_url.is_some());
        work.control.accept_native().unwrap();
        assert!(work.control.native_registration_started());
        assert!(flow.get("original").unwrap().authorization_url.is_none());
        // Losing the controller never rolls accepted native work back to OAuth.
        work.control.cancel();
        assert!(work.control.native_registration_started());
        let cancelled = EnrollmentControl::default();
        cancelled.cancel();
        assert!(!cancelled.native_registration_started());
    }
    #[tokio::test]
    async fn shutdown_cancels_authorization_before_any_native_registration_can_be_admitted() {
        let mut flow = Flow::default();
        let (work, start) = flow.begin("original", &call()).unwrap();
        assert!(start);
        let control = work.control.clone();
        flow.close();
        work.spawn(async move {
            assert!(!control.accepting());
            assert_eq!(control.accept_native(), Err("daemon_enrollment_cancelled"));
        });
        work.drain(false).await.unwrap();
        assert!(flow.begin("replacement", &call()).is_err());
    }
    #[tokio::test]
    async fn cancelled_shutdown_waiter_retains_original_accepted_task_until_completion() {
        let mut flow = Flow::default();
        let (work, _) = flow.begin("original", &call()).unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let writes = Arc::new(AtomicUsize::new(0));
        let count = writes.clone();
        let retained = work.clone();
        work.spawn(async move {
            retained.control.accept_native().unwrap();
            entered_tx.send(()).unwrap();
            release_rx.await.unwrap();
            count.fetch_add(1, Ordering::AcqRel);
            retained.status.lock().unwrap().result = Some(EnrollReport {
                owner: "@owner:test".into(),
                owner_root: "/original".into(),
                registered: true,
                pending: None,
                existing_executor: false,
            });
        });
        entered_rx.await.unwrap();
        flow.close();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), work.drain(false))
                .await
                .is_err()
        );
        assert_eq!(writes.load(Ordering::Acquire), 0);
        assert!(!work.done.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        work.drain(false).await.unwrap();
        assert_eq!(writes.load(Ordering::Acquire), 1);
        assert!(flow.get("original").unwrap().result.unwrap().registered);
    }
    #[tokio::test]
    async fn task_panic_reports_unknown_instead_of_losing_workflow_custody() {
        let mut flow = Flow::default();
        let (work, _) = flow.begin("original", &call()).unwrap();
        work.spawn(async { panic!("fixture task interruption") });
        work.drain(false).await.unwrap();
        assert_eq!(
            flow.get("original").unwrap().error.as_deref(),
            Some("daemon_enrollment_outcome_unknown")
        );
    }
}
