//! A host message that joins the running turn. The host authenticates its
//! sender before preparing it. A flushed write is not an acknowledgement: the
//! native queue acknowledges the message by its own uuid, then either takes
//! it into the running cycle or runs it as a cycle of its own.
use super::{Error, Message, Operation, Phase, SessionDriver, WriteProgress, io};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

/// Per turn, not in flight: each steer may add a native result of its own,
/// and the usage projection admits a bounded number of results per call.
const MAX_STEERS: usize = 16;

pub struct PreparedSteer {
    source: Arc<()>,
    id: String,
    sequence: u64,
    frame: io::PreparedFrame,
}
impl PreparedSteer {
    pub fn message_id(&self) -> &str {
        &self.id
    }
}
pub enum SteerUpdate {
    Message(Message),
    WriteAccepted(WriteProgress),
    /// The turn finished or a stop was requested before this write was
    /// started; nothing was sent.
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerState {
    /// Prepared or written. The native queue has not reported it.
    Unacknowledged,
    Queued,
    Started,
    Completed,
    /// Withdrawn by a stop. A steer cancelled while queued was never read.
    Cancelled,
}

struct Steer {
    id: String,
    sequence: u64,
    deadline: Instant,
    started: bool,
    written: bool,
    state: SteerState,
    /// Native `started` was observed: the model may have read the message.
    ran: bool,
    /// One init or result has named this message; no other may.
    named: bool,
}
#[derive(Default)]
pub(super) struct State {
    steers: Vec<Steer>,
    /// The steers named by the init of the open steer cycle.
    cycle: Option<Vec<String>>,
    continues: bool,
}
impl State {
    pub(super) fn is_empty(&self) -> bool {
        self.steers.is_empty()
    }
    pub(super) fn cycle_open(&self) -> bool {
        self.cycle.is_some()
    }
    /// The result just admitted closed a steer cycle.
    pub(super) fn continues(&self) -> bool {
        self.continues
    }
    /// A steer whose write began belongs to this turn until the native queue
    /// reports it completed or cancelled, even one it reads only after idle.
    pub(super) fn settled(&self) -> bool {
        self.cycle.is_none()
            && self.steers.iter().all(|s| {
                !s.started || matches!(s.state, SteerState::Completed | SteerState::Cancelled)
            })
    }
    /// Only the acknowledgement is bounded. A queued steer waits on the model.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.steers
            .iter()
            .filter(|s| s.started && s.state == SteerState::Unacknowledged)
            .map(|s| s.deadline)
            .min()
    }
    /// What a stop must withdraw: written steers the native queue still holds.
    pub(super) fn cancellable(&self) -> impl Iterator<Item = &str> {
        self.steers
            .iter()
            .filter(|s| {
                s.started && matches!(s.state, SteerState::Unacknowledged | SteerState::Queued)
            })
            .map(|s| s.id.as_str())
    }
    /// The native queue reports a withdrawal itself before it answers the
    /// request, so a confirmed cancel changes nothing here.
    pub(super) fn cancelled(&self, id: &str) -> Result<(), Error> {
        self.steers
            .iter()
            .find(|s| s.id == id && s.state == SteerState::Cancelled && !s.ran)
            .map(|_| ())
            .ok_or(Error::Identity)
    }
    /// `stopped`: an interrupt of this turn was accepted. Stop means stop, so
    /// a steer that starts after it ends the session instead of running.
    pub(super) fn lifecycle<R, W, E>(
        &mut self,
        payload: &Value,
        wire: &io::Wire<R, W, E>,
        stopped: bool,
    ) -> Result<(), Error> {
        let Some(steer) = payload["command_uuid"]
            .as_str()
            .and_then(|id| self.steers.iter_mut().find(|s| s.id == id))
        else {
            return Ok(());
        };
        if !steer.written && !(steer.started && wire.prepared_bytes_sent(steer.sequence)) {
            return Err(Error::Identity);
        }
        steer.state = match (steer.state, payload["state"].as_str()) {
            (SteerState::Unacknowledged, Some("queued")) => SteerState::Queued,
            (SteerState::Queued, Some("started")) if !stopped => {
                steer.ran = true;
                SteerState::Started
            }
            (SteerState::Started, Some("completed")) => SteerState::Completed,
            (SteerState::Queued | SteerState::Started, Some("cancelled")) => SteerState::Cancelled,
            _ => return Err(Error::Identity),
        };
        Ok(())
    }
    /// A started steer that no running cycle took in runs as a cycle of its
    /// own, and steers that waited together share one. Its init names them.
    pub(super) fn begin_cycle(&mut self, payload: &Value) -> Result<(), Error> {
        let ids = payload["user_message_uuids"]
            .as_array()
            .filter(|ids| !ids.is_empty() && self.cycle.is_none())
            .ok_or(Error::Identity)?;
        let mut own = Vec::new();
        for id in ids {
            let steer = id
                .as_str()
                .and_then(|id| self.steers.iter_mut().find(|s| s.id == id))
                .filter(|s| s.state == SteerState::Started && !s.named)
                .ok_or(Error::Identity)?;
            steer.named = true;
            own.push(steer.id.clone());
        }
        if payload
            .get("user_message_uuid")
            .filter(|v| !v.is_null())
            .is_some_and(|id| !own.iter().any(|own| id == own))
        {
            return Err(Error::Identity);
        }
        self.cycle = Some(own);
        Ok(())
    }
    /// A result names the messages its cycle read: first the cycle's own (the
    /// prompt, or the steers its init named), then every steer the cycle took
    /// in while it ran. No message is named by two cycles.
    pub(super) fn result(&mut self, payload: &Value, prompt: &str) -> bool {
        self.continues = false;
        let single = payload
            .get("user_message_uuid")
            .filter(|v| !v.is_null());
        let own = self.cycle.as_deref();
        let Some(ids) = payload
            .get("user_message_uuids")
            .filter(|v| !v.is_null())
        else {
            // Without the list only the prompt's own cycle can be told apart.
            return own.is_none() && single.is_some_and(|id| id == prompt);
        };
        let Some(ids) = ids.as_array() else {
            return false;
        };
        let base = own.map_or(1, <[String]>::len);
        if ids.len() < base
            || !match own {
                Some(own) => {
                    ids.iter().zip(own).all(|(id, own)| id == own)
                        && single.is_none_or(|id| own.iter().any(|own| id == own))
                }
                None => ids[0] == prompt && single.is_none_or(|id| id == prompt),
            }
        {
            return false;
        }
        let mut taken = Vec::new();
        for id in &ids[base..] {
            let Some(index) = id
                .as_str()
                .and_then(|id| self.steers.iter().position(|s| s.id == id))
                .filter(|index| {
                    self.steers[*index].ran
                        && !self.steers[*index].named
                        && !taken.contains(index)
                })
            else {
                return false;
            };
            taken.push(index);
        }
        for index in taken {
            self.steers[index].named = true;
        }
        self.continues = self.cycle.take().is_some();
        true
    }
}
impl<R, W, E> SessionDriver<R, W, E> {
    /// Text for the running turn of a continuous session. It cannot be moved
    /// to another turn, follow a stop request or be interleaved into a
    /// partially written frame.
    pub fn prepare_steer(&mut self, text: &str) -> Result<PreparedSteer, Error> {
        if !self.continuous
            || self.phase != Phase::Running
            || self.awaiting_init
            || self.completion.snapshot_requested()
            || self.interrupt.outstanding()
            || self.wire.write_progress().is_some()
        {
            return Err(Error::State);
        }
        let session = self.session_id.as_deref().ok_or(Error::State)?;
        if self.completion.steers.steers.len() >= MAX_STEERS {
            return Err(Error::Capacity);
        }
        let id = super::lifecycle::prompt_id();
        let mut value: Value = serde_json::from_slice(&super::super::prompt(text, Some(session))?)
            .map_err(|_| Error::State)?;
        value["uuid"] = id.clone().into();
        // Never the sender's text first: see `STEER_LEAD`.
        value["message"]["content"] = format!("{}{text}", super::super::STEER_LEAD).into();
        let sequence = self.control.next_sequence()?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let frame = io::PreparedFrame::new(sequence, super::super::encode(value)?, deadline);
        self.completion.steers.steers.push(Steer {
            id: id.clone(),
            sequence,
            deadline,
            started: false,
            written: false,
            state: SteerState::Unacknowledged,
            ran: false,
            named: false,
        });
        Ok(PreparedSteer {
            source: self.source.clone(),
            id,
            sequence,
            frame,
        })
    }
    /// None for a message that is not a steer of this turn, including one
    /// whose write never started.
    pub fn steer_state(&self, message_id: &str) -> Option<SteerState> {
        self.completion
            .steers
            .steers
            .iter()
            .find(|s| s.id == message_id)
            .map(|s| s.state)
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// Preserve this exact frame across Message returns and process each event
    /// before resuming it. Cancelling the future closes the uncertain stream.
    pub async fn send_prepared_steer(
        &mut self,
        prepared: &mut PreparedSteer,
    ) -> Result<SteerUpdate, Error> {
        if !Arc::ptr_eq(&self.source, &prepared.source) {
            return Err(Error::Identity);
        }
        let steers = &mut self.completion.steers.steers;
        let index = steers
            .iter()
            .position(|s| s.id == prepared.id && s.sequence == prepared.sequence)
            .ok_or(Error::Identity)?;
        if steers[index].written {
            return Err(Error::State);
        }
        if !steers[index].started
            && (self.phase == Phase::ResultObserved || self.interrupt.outstanding())
        {
            steers.remove(index);
            return Ok(SteerUpdate::Unavailable);
        }
        // As for an approval: a frame whose bytes have all left only waits
        // for its flush, which the peer can outrun through to the turn's end.
        let expected = if self.phase == Phase::ResultObserved
            && self.wire.awaits_flush_only(&prepared.frame)
        {
            Phase::ResultObserved
        } else {
            Phase::Running
        };
        let operation = Operation::new(self, expected)?;
        let result = operation.driver.send_steer_inner(prepared, index).await;
        operation.finish(result)
    }
    async fn send_steer_inner(
        &mut self,
        prepared: &mut PreparedSteer,
        index: usize,
    ) -> Result<SteerUpdate, Error> {
        self.completion.steers.steers[index].started = true;
        let until = self.callback_deadline()?;
        match self.wire.send_prepared(&mut prepared.frame, until).await? {
            io::Controlled::Message(received) => {
                self.observe(&received.message, received.at)?;
                Ok(SteerUpdate::Message(received.message))
            }
            io::Controlled::Control(receipt) => {
                self.completion.steers.steers[index].written = true;
                Ok(SteerUpdate::WriteAccepted(receipt))
            }
        }
    }
}
