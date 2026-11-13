//! Shared work keeps its original activity lease and usage sink. Matrix IO
//! runs beside the native reader, so a slow card edit cannot stall inference.
use super::*;
use crate::{
    agent_run_controls::{Action as MatrixAction, Card, State},
    console::owned_runtime::run_controls::CardHost,
};
use hagency_runtime::{
    codex::session::{RunAction, RunOutcome},
    owned::OwnedSession,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use tokio::sync::{mpsc, oneshot};

struct Request {
    id: String,
    action: Option<RunAction>,
    reply: Option<oneshot::Sender<RunOutcome>>,
    begun: bool,
    stop: bool,
}
impl Request {
    fn finish(mut self, value: RunOutcome) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(value);
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.send(if self.begun {
                RunOutcome::Unknown
            } else {
                RunOutcome::Unavailable
            });
        }
    }
}
pub(super) struct Controls {
    card: Arc<CardHost>,
    pending: BTreeMap<String, Request>,
    send: Option<(mpsc::Sender<Request>, mpsc::Sender<Request>)>,
    stop: mpsc::Receiver<Request>,
    steer: mpsc::Receiver<Request>,
    task: Option<tokio::task::JoinHandle<()>>,
    active: bool,
}
impl Controls {
    pub fn new(work: &SharedWorkItem, call: &str) -> Result<Self, NativeError> {
        let card = Card::for_shared(work, call).map_err(|_| invalid())?;
        let (stop_tx, stop) = mpsc::channel(1);
        let (steer_tx, steer) = mpsc::channel(16);
        Ok(Self {
            card: Arc::new(CardHost::new(card)),
            pending: BTreeMap::new(),
            send: Some((stop_tx, steer_tx)),
            stop,
            steer,
            task: None,
            active: false,
        })
    }
    pub fn activate(&mut self, process: &mut OwnedSession) {
        self.active = true;
        process.set_shared_live_text(self.card.live_text());
    }
    fn receipts(&mut self, process: &mut OwnedSession) {
        for receipt in process.run_control_receipts() {
            if let Some(request) = self.pending.remove(&receipt.command_id) {
                request.finish(receipt.outcome);
            }
        }
    }
    pub async fn poll(
        &mut self,
        io: &Io,
        command: &SharedRuntimeCommand,
        process: &mut OwnedSession,
    ) -> Result<(), NativeError> {
        if !self.active {
            return Ok(());
        }
        if let Some((stop, steer)) = self.send.take() {
            let io = io.clone();
            let command = command.clone();
            let card = self.card.clone();
            self.task = Some(tokio::spawn(async move {
                serve(io, command, card, stop, steer).await;
            }));
        }
        self.receipts(process);
        // The separate stop slot cannot be exhausted by pending steering.
        while let Some(mut request) = self
            .stop
            .try_recv()
            .ok()
            .or_else(|| self.steer.try_recv().ok())
        {
            if request
                .reply
                .as_ref()
                .is_none_or(oneshot::Sender::is_closed)
            {
                continue;
            }
            request.begun = true;
            let action = request.action.take().ok_or_else(invalid)?;
            let outcome = process
                .submit_run_control(request.id.clone(), action)
                .await
                .map_err(|_| failure(409, "shared_run_control_unknown"))?;
            if outcome == RunOutcome::Pending {
                self.pending.insert(request.id.clone(), request);
            } else {
                request.finish(outcome);
            }
            // Let the original reader make progress between native writes.
            break;
        }
        Ok(())
    }
    pub async fn finish(
        &mut self,
        process: &mut OwnedSession,
        result: &Result<SharedWorkReceipt, NativeError>,
    ) {
        self.receipts(process);
        // A Stop that reached the native process and got no answer.
        let stop_unanswered = self.pending.values().any(|request| request.stop);
        // Close intake before draining. Queued, unwritten requests settle as
        // unavailable; a native write without a receipt remains unknown.
        self.stop.close();
        self.steer.close();
        while let Ok(request) = self.stop.try_recv() {
            drop(request);
        }
        while let Ok(request) = self.steer.try_recv() {
            drop(request);
        }
        self.pending.clear();
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
        let state = match result {
            Ok(_)
                if matches!(
                    process.protocol_outcome(),
                    Some(hagency_runtime::codex::session::Outcome::Interrupted)
                ) =>
            {
                State::Interrupted
            }
            Ok(receipt) if receipt.state == "completed" => State::Completed,
            Ok(receipt) if receipt.state == "failed" => State::Failed,
            // The run ended after a Stop that Codex never answered. Its
            // process is not used again, so for the room it is stopped.
            Err(_) if stop_unanswered => State::Interrupted,
            _ => State::Unknown,
        };
        self.card.finish(state);
    }
}
impl Drop for Controls {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.card.finish(State::Unknown);
    }
}

async fn serve(
    io: Io,
    command: SharedRuntimeCommand,
    card: Arc<CardHost>,
    stop_tx: mpsc::Sender<Request>,
    steer_tx: mpsc::Sender<Request>,
) {
    // Optional presentation must not delay admission, native start or reads.
    // Resolve the original assignment's device only in this background task.
    // A device that is not ready yet is asked for again while the run lasts,
    // so a long run still gets its Stop. A new grant is a Matrix login and the
    // homeserver limits those, so the pauses grow.
    let Some(device) = crate::console::owned_runtime::run_controls::open_while_running(
        Duration::from_secs(10),
        || {
            let (io, command) = (&io, &command);
            async move {
                io.check_running(command).await?;
                let bound = io.bound().await?;
                let device = super::e2ee::device(io, &bound, &command.assignment_id).await?;
                io.check_running(command).await?;
                Ok::<_, NativeError>(device)
            }
        },
        || stop_tx.is_closed() || steer_tx.is_closed(),
    )
    .await
    else {
        return;
    };
    if card.open(device).is_err() {
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        if stop_tx.is_closed() || steer_tx.is_closed() {
            return;
        }
        card.drain_receipts(false);
        let _ = card.publish().await;
        for (received, action) in card.actions().await.unwrap_or_default() {
            // Current server identity, assignment and command; native submission
            // additionally checks the same process's unexpired activity lease.
            if io.check_running(&command).await.is_err() {
                let _ = card.receipt(&received.event_id, &received.sender, "unavailable", false);
                continue;
            }
            let (action, stop) = match action {
                MatrixAction::Steer { text } => (RunAction::Steer { text }, false),
                MatrixAction::Interrupt {} => (RunAction::Interrupt, true),
                MatrixAction::Queue { .. } => {
                    let _ =
                        card.receipt(&received.event_id, &received.sender, "unsupported", false);
                    continue;
                }
            };
            let Ok(encoded) = serde_json::to_vec(&(&received.card_event_id, &received.event_id))
            else {
                let _ = card.receipt(&received.event_id, &received.sender, "unavailable", stop);
                continue;
            };
            let id = format!("matrix_{:x}", Sha256::digest(encoded));
            let (reply, receipt) = oneshot::channel();
            let request = Request {
                id,
                action: Some(action),
                reply: Some(reply),
                begun: false,
                stop,
            };
            match (if stop { &stop_tx } else { &steer_tx }).try_send(request) {
                Ok(()) => card.track(received.event_id, received.sender, stop, receipt, name),
                Err(error) => {
                    let outcome = if matches!(error, mpsc::error::TrySendError::Full(_)) {
                        "busy"
                    } else {
                        "unavailable"
                    };
                    let _ = card.receipt(&received.event_id, &received.sender, outcome, stop);
                }
            }
        }
    }
}
fn name(outcome: RunOutcome) -> &'static str {
    match outcome {
        RunOutcome::Pending | RunOutcome::InProgress => "in_progress",
        RunOutcome::Accepted => "accepted",
        RunOutcome::Rejected => "rejected",
        RunOutcome::Unknown => "unknown",
        RunOutcome::Unavailable => "unavailable",
        RunOutcome::Busy => "busy",
    }
}
