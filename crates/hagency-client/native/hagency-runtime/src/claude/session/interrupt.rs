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
}
#[derive(Default)]
pub(super) struct State(Option<Request>);
impl State {
    pub(super) fn pending(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|r| r.started && r.receipt.is_none())
    }
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.0
            .as_ref()
            .filter(|r| r.started && r.receipt.is_none())
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
    }
    fn receipt(&self) -> Option<&InterruptReceipt> {
        self.0.as_ref()?.receipt.as_ref()
    }
    pub(super) fn observe<R, W, E>(
        &mut self,
        id: &str,
        outcome: &ControlOutcome,
        wire: &io::Wire<R, W, E>,
    ) -> Result<(), Error> {
        let request = self
            .0
            .as_mut()
            .filter(|r| r.id == id && r.started && r.receipt.is_none())
            .ok_or(Error::Identity)?;
        if !request.written && !wire.prepared_bytes_sent(request.sequence) {
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
        Ok(())
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
        let frame = io::PreparedFrame::new(sequence, super::super::interrupt(&id)?, deadline);
        self.interrupt.0 = Some(Request {
            id: id.clone(),
            sequence,
            deadline,
            started: false,
            written: false,
            receipt: None,
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
