use super::*;
use crate::console::lifecycle::{DrainFailure, RetainedTask, TaskCustody};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Mutex, Weak},
};
use tokio::sync::{oneshot, watch};
const MAX_PROCESSES: usize = 128;
#[derive(Clone, Copy)]
enum Phase {
    Pending,
    Issued(Option<u64>),
}
struct Entry {
    identity: Arc<()>,
    phase: Phase,
    assignment: String,
    digest: String,
    cancel: watch::Sender<bool>,
    task: RetainedTask,
}
#[derive(Default)]
struct State {
    closed: bool,
    entries: BTreeMap<String, Entry>,
    controls: Vec<RetainedTask>,
    stops: BTreeMap<String, u64>,
}
#[derive(Clone, Default)]
pub(in crate::console::native) struct Manager {
    pub(super) claude_workspaces: super::claude_workspace::Workspaces,
    state: Arc<Mutex<State>>,
    custody: TaskCustody,
    /// Owner telemetry fed by verified turn observations (never budget).
    telemetry: crate::console::native::telemetry::OwnerTelemetry,
}
/// Not constructible by a caller. Its identity pins one bounded pending slot;
/// stale cleanup can never install into another entry with the same command ID.
struct Admission {
    state: Weak<Mutex<State>>,
    id: String,
    identity: Arc<()>,
}
impl Admission {
    fn before_claim(&self) -> Result<(), NativeError> {
        let state = self.state.upgrade().ok_or_else(closed)?;
        let s = state.lock().map_err(|_| unknown())?;
        let e = s.entries.get(&self.id).ok_or_else(unknown)?;
        if s.closed || !Arc::ptr_eq(&e.identity, &self.identity) || *e.cancel.borrow() {
            return Err(closed());
        }
        Ok(())
    }
    fn issued(&self, revision: Option<u64>) -> Result<(), NativeError> {
        let state = self.state.upgrade().ok_or_else(unknown)?;
        let mut s = state.lock().map_err(|_| unknown())?;
        let e = s.entries.get(&self.id).ok_or_else(unknown)?;
        if !Arc::ptr_eq(&e.identity, &self.identity) {
            return Err(unknown());
        }
        if s.closed
            || *e.cancel.borrow()
            || s.stops
                .get(&e.assignment)
                .is_some_and(|stop| revision.is_none_or(|r| r <= *stop))
        {
            return Err(closed());
        }
        s.entries.get_mut(&self.id).ok_or_else(unknown)?.phase = Phase::Issued(revision);
        Ok(())
    }
}
fn closed() -> NativeError {
    failure(409, "shared_execution_closed")
}
fn unknown() -> NativeError {
    failure(409, "shared_execution_outcome_unknown")
}
impl Manager {
    pub(in crate::console::native) fn sweep_claude(&self, root: &std::path::Path) -> Result<(), NativeError> {
        self.claude_workspaces.sweep_existing(root)
    }
    pub(in crate::console::native) fn telemetry(
        &self,
    ) -> crate::console::native::telemetry::OwnerTelemetry {
        self.telemetry.clone()
    }
    pub fn close(&self) {
        self.claude_workspaces.close();
        self.custody.close();
        match self.state.lock() {
            Ok(mut s) => {
                s.closed = true;
                for e in s.entries.values() {
                    e.cancel.send_replace(true);
                }
            }
            Err(_) => self.custody.unknown(),
        }
    }
    pub async fn drain(&self) -> Result<(), DrainFailure> {
        self.close();
        self.custody.drain().await
    }
    /// Observation only: retained shared-execution tasks still running in
    /// this process. Not evidence that any assignment or model turn is live.
    pub(in crate::console::native) fn live(&self) -> Option<u64> {
        let s = self.state.lock().ok()?;
        Some(s.entries.values().filter(|e| !e.task.is_finished()).count() as u64)
    }
    pub(super) fn running(&self, id: &str, digest: &str) -> Result<bool, NativeError> {
        let s = self
            .state
            .lock()
            .map_err(|_| failure(409, "shared_execution_outcome_unknown"))?;
        if let Some(e) = s.entries.get(id) {
            if e.digest != digest {
                return Err(invalid());
            }
            if matches!(e.phase, Phase::Pending) && !e.task.is_finished() {
                // A reserved slot is not evidence of a running provider process.
                return Err(unknown());
            }
            return Ok(!e.task.is_finished());
        }
        Ok(false)
    }
    pub(super) fn stop_assignment(
        &self,
        assignment: &str,
        revision: u64,
    ) -> Result<(), NativeError> {
        // Stop pauses the Agent; a later Run continues it. Saved sessions and
        // work folders are therefore kept here. Only loss of a conversation's
        // authority removes that conversation's files (`Lease::retire`).
        let mut s = self
            .state
            .lock()
            .map_err(|_| failure(409, "shared_execution_outcome_unknown"))?;
        if !s.stops.contains_key(assignment) && s.stops.len() >= 128 {
            return Err(failure(409, "shared_execution_closed"));
        }
        let through = *s
            .stops
            .entry(assignment.into())
            .and_modify(|r| *r = (*r).max(revision))
            .or_insert(revision);
        for entry in s.entries.values().filter(|e| {
            e.assignment == assignment
                && matches!(e.phase, Phase::Issued(revision) if revision.is_none_or(|r| r <= through))
        }) {
            entry.cancel.send_replace(true);
        }
        Ok(())
    }
    /// Caller cancellation cannot cancel an accepted local Stop write or its
    /// exact original network attempt. Process cleanup is tracked independently.
    pub(super) fn retain_stop<T: Send + 'static>(
        &self,
        work: impl std::future::Future<Output = Result<T, NativeError>> + Send + 'static,
    ) -> Result<oneshot::Receiver<Result<T, NativeError>>, NativeError> {
        let activity = self
            .custody
            .enter()
            .map_err(|_| failure(409, "shared_execution_closed"))?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| failure(409, "shared_execution_outcome_unknown"))?;
        state.controls.retain(|t| !t.is_finished());
        if state.closed || state.controls.len() >= 16 {
            return Err(failure(409, "shared_execution_closed"));
        }
        let (tx, rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = tx.send(work.await);
        });
        state.controls.push(self.custody.track(task));
        drop(state);
        drop(activity);
        Ok(rx)
    }
    pub(super) fn request_stop(&self, id: &str, digest: &str) -> Result<bool, NativeError> {
        let s = self
            .state
            .lock()
            .map_err(|_| failure(409, "shared_execution_outcome_unknown"))?;
        let Some(entry) = s.entries.get(id) else {
            return Ok(false);
        };
        if entry.digest != digest {
            return Err(invalid());
        }
        if entry.task.is_finished() {
            return Ok(false);
        }
        entry.cancel.send_replace(true);
        Ok(true)
    }
    /// Reserve before the irreversible registry claim. Accepted admission and
    /// its result are retained independently of the caller's oneshot waiter.
    fn admit<F, W>(
        &self,
        id: String,
        digest: String,
        assignment: String,
        manual: bool,
        work: W,
    ) -> Result<oneshot::Receiver<Result<SharedExecutionProgress, NativeError>>, NativeError>
    where
        W: FnOnce(
            Admission,
            watch::Receiver<bool>,
            oneshot::Sender<Result<SharedExecutionProgress, NativeError>>,
        ) -> F,
        F: Future<Output = Result<(), DrainFailure>> + Send + 'static,
    {
        let activity = self.custody.enter().map_err(|_| closed())?;
        let mut state = self.state.lock().map_err(|_| unknown())?;
        // Digest conflicts on a retained key are checked even if cleanup is done.
        if state.entries.get(&id).is_some_and(|e| e.digest != digest) {
            return Err(invalid());
        }
        // TaskCustody independently retains/folds failures. Never discard an
        // unobserved finished handle or an explicit Unknown/error result here.
        state
            .entries
            .retain(|_, e| e.task.observed_result() != Some(Ok(())));
        if state.closed
            || (manual && state.stops.contains_key(&assignment))
            || state.entries.contains_key(&id)
            || state.entries.len() >= MAX_PROCESSES
        {
            return Err(closed());
        }
        let identity = Arc::new(());
        let admission = Admission {
            state: Arc::downgrade(&self.state),
            id: id.clone(),
            identity: identity.clone(),
        };
        let (cancel, rx) = watch::channel(false);
        let (tx, result) = oneshot::channel();
        // before_claim must acquire this mutex, so no claim can run until its
        // reservation has actually been installed. Close/drain retains this job.
        let task = self
            .custody
            .track_result(tokio::spawn(work(admission, rx, tx)));
        state.entries.insert(
            id,
            Entry {
                identity,
                phase: Phase::Pending,
                assignment,
                digest,
                cancel,
                task,
            },
        );
        drop(state);
        drop(activity);
        Ok(result)
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        &self,
        registry: OwnerResourceRegistry,
        proof: VerifiedSharedRuntimeDelivery,
        launch: launch::Launch,
        io: Io,
        delivery: SharedRuntimeDelivery,
        observed: u64,
        started: Instant,
        until: Instant,
    ) -> Result<oneshot::Receiver<Result<SharedExecutionProgress, NativeError>>, NativeError> {
        let telemetry = self.telemetry.clone();
        self.admit(
            delivery.command.command_id.clone(),
            delivery.command_digest.clone(),
            delivery.command.assignment_id.clone(),
            true,
            move |slot, cancel, sender| async move {
                if let Err(error) = slot.before_claim() {
                    let _ = sender.send(Err(error));
                    return Ok(()); // No claim was attempted.
                }
                let custody = match registry.claim_shared_runtime(proof).await {
                    Ok(SharedRuntimeClaim::Issued(custody)) => custody,
                    Ok(SharedRuntimeClaim::History(_)) => {
                        let _ =
                            sender.send(Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                                command_id: delivery.command.command_id,
                            }));
                        return Ok(()); // Existing history never yields new custody.
                    }
                    Err(error) => {
                        let _ = sender.send(Err(store(error)));
                        // The store's post-commit gate may lose authority. Do not
                        // infer no claim from an error or release unknown custody.
                        return Err(DrainFailure::Unknown);
                    }
                };
                if let Err(error) = slot.issued(None) {
                    let _ = sender.send(Err(error));
                    return Err(DrainFailure::Unknown);
                }
                run::run(
                    registry, *custody, launch, io, delivery, observed, started, until, cancel,
                    sender, telemetry,
                )
                .await
            },
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_attempt(
        &self,
        registry: OwnerResourceRegistry,
        proof: VerifiedSharedAttemptDelivery,
        launch: launch::Launch,
        io: Io,
        delivery: super::attempt_io::Delivery,
        observed: u64,
        started: Instant,
        until: Instant,
    ) -> Result<oneshot::Receiver<Result<SharedExecutionProgress, NativeError>>, NativeError> {
        let telemetry = self.telemetry.clone();
        self.admit(
            delivery.command.command_id.clone(),
            delivery.command_digest.clone(),
            delivery.command.assignment_id.clone(),
            false,
            move |slot, cancel, sender| async move {
                if let Err(error) = slot.before_claim() {
                    let _ = sender.send(Err(error));
                    return Ok(());
                }
                let custody = match registry.claim_shared_successor(proof).await {
                    Ok(SharedAttemptClaim::Issued(custody)) => custody,
                    Ok(SharedAttemptClaim::History(_)) => {
                        let _ =
                            sender.send(Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                                command_id: delivery.command.command_id,
                            }));
                        return Ok(());
                    }
                    Err(error) => {
                        let _ = sender.send(Err(store(error)));
                        return Err(DrainFailure::Unknown);
                    }
                };
                // Pending admission has no trusted control revision yet. Stop
                // records its fence without guessing; compare the actual sealed
                // custody now, so old Stop1 cannot cancel later Run3.
                if let Err(error) = slot.issued(custody.demand_control_revision()) {
                    let _ = sender.send(Err(error));
                    return Err(DrainFailure::Unknown);
                }
                super::attempt_run::run(
                    registry, *custody, launch, io, delivery, observed, started, until, cancel,
                    sender, telemetry,
                )
                .await
            },
        )
    }
    #[cfg(feature = "shared-budget-execution")]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_budget_attempt(
        &self,
        registry: OwnerResourceRegistry,
        proof: VerifiedSharedBudgetAttemptDelivery,
        launch: launch::Launch,
        io: Io,
        delivery: super::budget_io::Delivery,
        observed: u64,
        started: Instant,
        until: Instant,
    ) -> Result<oneshot::Receiver<Result<SharedExecutionProgress, NativeError>>, NativeError> {
        let telemetry = self.telemetry.clone();
        self.admit(
            delivery.command.command_id.clone(),
            delivery.command_digest.clone(),
            delivery.command.assignment_id.clone(),
            false,
            move |slot, cancel, sender| async move {
                if let Err(error) = slot.before_claim() {
                    let _ = sender.send(Err(error));
                    return Ok(());
                }
                let custody = match registry.claim_shared_budget_successor(proof).await {
                    Ok(SharedBudgetAttemptClaim::Issued(custody)) => custody,
                    Ok(SharedBudgetAttemptClaim::History(_)) => {
                        let _ =
                            sender.send(Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                                command_id: delivery.command.command_id,
                            }));
                        return Ok(());
                    }
                    Err(error) => {
                        let _ = sender.send(Err(store(error)));
                        return Err(DrainFailure::Unknown);
                    }
                };
                // Pending admission has no trusted control revision yet. Stop
                // records its fence without guessing; compare the actual sealed
                // custody now, so old Stop1 cannot cancel later Run3.
                if let Err(error) = slot.issued(custody.demand_control_revision()) {
                    let _ = sender.send(Err(error));
                    return Err(DrainFailure::Unknown);
                }
                super::budget_run::run(
                    registry, *custody, launch, io, delivery, observed, started, until, cancel,
                    sender, telemetry,
                )
                .await
            },
        )
    }
}
#[cfg(test)]
mod tests;
