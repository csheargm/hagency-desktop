//! A host stop request is separate from both tool approval and turn completion.
//! The host authenticates the controller before preparing it. A flushed write
//! is not an acknowledgement; an acknowledgement is not proof work has stopped.
use super::{ControlOutcome, Error, Message, Operation, Phase, SessionDriver, WriteProgress, io};
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

pub struct PreparedInterrupt {
    source: Arc<()>,
    id: String,
    sequence: u64,
    frame: io::PreparedFrame,
}
impl PreparedInterrupt {
    pub fn request_id(&self) -> &str {
        &self.id
    }
}

/// Correlated native acknowledgement only. Continue reading through the
/// original result, idle state and descendant completion before settling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InterruptReceipt {
    request_id: String,
    accepted: bool,
    still_queued: Option<Vec<String>>,
}
impl InterruptReceipt {
    pub fn request_id(&self) -> &str {
        &self.request_id
    }
    pub fn accepted(&self) -> bool {
        self.accepted
    }
    pub fn still_queued(&self) -> Option<&[String]> {
        self.still_queued.as_deref()
    }
}
pub enum InterruptUpdate {
    Message(Message),
    WriteAccepted(WriteProgress),
    /// The turn finished before this request was started; nothing was sent.
    Unavailable,
}

pub(super) struct Request {
    id: String,
    sequence: u64,
    deadline: Instant,
    started: bool,
    written: bool,
    receipt: Option<InterruptReceipt>,
    cancels: Vec<Cancel>,
}
/// One steer the stop withdraws, written in the same frame ahead of it.
struct Cancel {
    id: String,
    steer: String,
    cancelled: Option<bool>,
}
#[derive(Default)]
pub(super) struct State(Option<Request>);
impl State {
    pub(super) fn started_request_id(&self) -> Option<&str> {
        self.0.as_ref().filter(|r| r.started).map(|r| r.id.as_str())
    }
    pub(super) fn pending(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|r| r.started && r.receipt.is_none())
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.0
            .as_ref()
            .filter(|r| r.started && r.receipt.as_ref().is_none_or(|r| r.accepted))
            .map(|r| r.deadline)
    }
    pub(super) fn blocks_replies(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|r| r.started && r.receipt.as_ref().is_none_or(|r| r.accepted))
    }
    pub(super) fn quiescent(&self) -> bool {
        !self.pending()
            && self
                .receipt()
                .is_none_or(|r| r.still_queued.as_ref().is_none_or(Vec::is_empty))
            && self.0.as_ref().is_none_or(|r| {
                !r.started || r.cancels.iter().all(|c| c.cancelled.is_some())
            })
    }
    /// A request that was not natively refused, started or not.
    pub(super) fn outstanding(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|r| r.receipt.as_ref().is_none_or(|r| r.accepted))
    }
    fn receipt(&self) -> Option<&InterruptReceipt> {
        self.0.as_ref()?.receipt.as_ref()
    }
    /// This started request was accepted or has no native answer yet.
    pub(super) fn unrefused(&self, id: &str) -> bool {
        self.0.as_ref().is_some_and(|r| {
            r.id == id && r.started && r.receipt.as_ref().is_none_or(|r| r.accepted)
        })
    }
    /// Returns the steer whose withdrawal this response confirms, if any.
    pub(super) fn observe<R, W, E>(
        &mut self,
        id: &str,
        outcome: &ControlOutcome,
        wire: &io::Wire<R, W, E>,
    ) -> Result<Option<String>, Error> {
        let request = self
            .0
            .as_mut()
            .filter(|r| r.started)
            .ok_or(Error::Identity)?;
        if !request.written && !wire.prepared_bytes_sent(request.sequence) {
            return Err(Error::Identity);
        }
        if let Some(cancel) = request.cancels.iter_mut().find(|c| c.id == id) {
            if cancel.cancelled.is_some() {
                return Err(Error::Identity);
            }
            // False: the native queue no longer held it, so it had started.
            let cancelled = match outcome {
                ControlOutcome::Refused => false,
                ControlOutcome::Success(value) => {
                    value["cancelled"].as_bool().ok_or(Error::Malformed)?
                }
            };
            cancel.cancelled = Some(cancelled);
            return Ok(cancelled.then(|| cancel.steer.clone()));
        }
        if request.id != id || request.receipt.is_some() {
            return Err(Error::Identity);
        }
        let still_queued = match outcome {
            ControlOutcome::Refused => None,
            ControlOutcome::Success(value) => {
                if !value.is_object() {
                    return Err(Error::Malformed);
                }
                value
                    .get("still_queued")
                    .map(|value| {
                        let ids = value
                            .as_array()
                            .filter(|ids| ids.len() <= 1024)
                            .ok_or(Error::Malformed)?;
                        ids.iter()
                            .map(|id| {
                                id.as_str()
                                    .filter(|id| super::super::identifier(id))
                                    .map(str::to_owned)
                                    .ok_or(Error::Malformed)
                            })
                            .collect::<Result<Vec<_>, _>>()
                    })
                    .transpose()?
            }
        };
        request.receipt = Some(InterruptReceipt {
            request_id: id.into(),
            accepted: matches!(outcome, ControlOutcome::Success(_)),
            still_queued,
        });
        // An acknowledgement is not completion. Bound the final drain even
        // when the native peer accepts stop but never emits result and idle.
        if matches!(outcome, ControlOutcome::Success(_)) {
            request.deadline = Instant::now() + Duration::from_secs(10);
        }
        Ok(None)
    }
}
impl<R, W, E> SessionDriver<R, W, E> {
    /// One outstanding stop request. A native refusal permits a new request;
    /// an uncertain or accepted request is never replayed. It cannot be moved
    /// to another turn or interleaved into a partially written reply.
    pub fn prepare_interrupt(&mut self) -> Result<PreparedInterrupt, Error> {
        if self.phase != Phase::Running
            || self.awaiting_init
            || self.session_id.is_none()
            || self.completion.snapshot_requested()
            || self
                .interrupt
                .0
                .as_ref()
                .is_some_and(|r| r.receipt.as_ref().is_none_or(|r| r.accepted))
            || self.wire.write_progress().is_some()
        {
            return Err(Error::State);
        }
        let id = super::lifecycle::prompt_id();
        let sequence = self.control.next_sequence()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        // Stop means stop. An interrupt alone leaves a queued steer for the
        // native peer to run next, so the same write first withdraws each one.
        let mut bytes = Vec::new();
        let mut cancels = Vec::new();
        for steer in self.completion.steers.cancellable() {
            let cancel = super::lifecycle::prompt_id();
            bytes.extend(super::super::cancel_async_message(&cancel, steer)?);
            cancels.push(Cancel {
                id: cancel,
                steer: steer.into(),
                cancelled: None,
            });
        }
        bytes.extend(super::super::interrupt(&id)?);
        let frame = io::PreparedFrame::new(sequence, bytes, deadline);
        self.interrupt.0 = Some(Request {
            id: id.clone(),
            sequence,
            deadline,
            started: false,
            written: false,
            receipt: None,
            cancels,
        });
        Ok(PreparedInterrupt {
            source: self.source.clone(),
            id,
            sequence,
            frame,
        })
    }
    pub fn interrupt_receipt(&self) -> Option<&InterruptReceipt> {
        self.interrupt.receipt()
    }
    /// The native answer to the withdrawal the current stop wrote for this
    /// steer. None: no withdrawal was written for it, or it has no answer yet.
    pub fn steer_cancelled(&self, message_id: &str) -> Option<bool> {
        let request = self.interrupt.0.as_ref()?;
        request
            .cancels
            .iter()
            .find(|c| c.steer == message_id)?
            .cancelled
    }
    /// While true, hold host answer/tool futures and keep reading native
    /// events. Withdraw those futures once the request is accepted; an explicit
    /// refusal permits their original decisions to resume. Do not grant tools
    /// while the stop is pending or accepted.
    pub fn is_interrupting(&self) -> bool {
        self.interrupt.blocks_replies()
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// Preserve this exact frame across Message returns and process each event
    /// before resuming it. Cancelling the future closes the uncertain stream.
    pub async fn send_prepared_interrupt(
        &mut self,
        prepared: &mut PreparedInterrupt,
    ) -> Result<InterruptUpdate, Error> {
        if !Arc::ptr_eq(&self.source, &prepared.source) {
            return Err(Error::Identity);
        }
        let request = self
            .interrupt
            .0
            .as_ref()
            .filter(|r| r.id == prepared.id && r.sequence == prepared.sequence)
            .ok_or(Error::Identity)?;
        if request.written {
            return Err(Error::State);
        }
        if self.phase == Phase::ResultObserved && !request.started {
            return Ok(InterruptUpdate::Unavailable);
        }
        let expected = if self.phase == Phase::ResultObserved
            && self.wire.awaits_flush_only(&prepared.frame)
        {
            Phase::ResultObserved
        } else {
            Phase::Running
        };
        let operation = Operation::new(self, expected)?;
        let result = operation.driver.send_interrupt_inner(prepared).await;
        operation.finish(result)
    }
    async fn send_interrupt_inner(
        &mut self,
        prepared: &mut PreparedInterrupt,
    ) -> Result<InterruptUpdate, Error> {
        self.interrupt.0.as_mut().ok_or(Error::State)?.started = true;
        let until = self.callback_deadline()?;
        match self.wire.send_prepared(&mut prepared.frame, until).await? {
            io::Controlled::Message(received) => {
                self.observe(&received.message, received.at)?;
                Ok(InterruptUpdate::Message(received.message))
            }
            io::Controlled::Control(receipt) => {
                self.interrupt.0.as_mut().ok_or(Error::State)?.written = true;
                Ok(InterruptUpdate::WriteAccepted(receipt))
            }
        }
    }
}
