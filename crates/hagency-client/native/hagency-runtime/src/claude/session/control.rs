//! Wire mechanics only. Original durable owner grants remain the Host's duty.
use super::{Error, Message, Operation, Phase, SessionDriver, WriteProgress, io};
use crate::deadline::Deadline;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::Arc,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

const MAX_CALLBACKS: usize = 16;
const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct ApprovalControlPolicy {
    pub owner_wait_ms: u64,
    pub response_reserve_ms: u64,
}
/// Host-selected wire behavior, not an owner verdict. No JSON constructor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
}
pub enum ControlUpdate<T> {
    Message(Message),
    Control(T),
}
pub enum PreparedUpdate {
    Message(Message),
    WriteAccepted(WriteProgress),
}
/// Exactly one original frame. Neither Clone, Deserialize nor arbitrary bytes.
pub struct PreparedApproval {
    source: Arc<()>,
    id: String,
    frame: io::PreparedFrame,
    response_deadline: Instant,
    sequence: u64,
}
impl PreparedApproval {
    pub fn request_id(&self) -> &str {
        &self.id
    }
    pub fn response_deadline(&self) -> Instant {
        self.response_deadline
    }
}
enum Stage {
    Waiting,
    Prepared(u64),
    Sent,
    Cancelled,
}
struct Callback {
    stage: Stage,
    tool: String,
    input: Option<Value>,
    owner: Option<Instant>,
    response: Option<Instant>,
}
#[derive(Default)]
pub(super) struct State {
    policy: Option<ApprovalControlPolicy>,
    question_wait_ms: Option<u64>,
    entries: BTreeMap<String, Callback>,
    seen: BTreeSet<String>,
    read_deadline: Option<Deadline>,
    sequence: u64,
}
impl State {
    pub(super) fn seen(&self, id: &str) -> bool {
        self.seen.contains(id)
    }
    pub(super) fn next_sequence(&mut self) -> Result<u64, Error> {
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        Ok(self.sequence)
    }
    pub(super) fn untouched(&self) -> bool {
        self.policy.is_none() && self.entries.is_empty() && self.seen.is_empty()
    }
    pub(super) fn quiescent(&self) -> bool {
        self.entries
            .values()
            .all(|entry| matches!(entry.stage, Stage::Sent | Stage::Cancelled))
    }
    pub fn clear(&mut self) {
        self.entries.clear();
        self.seen.clear();
    }
    pub fn observed(&mut self) {
        self.read_deadline = None;
    }
    pub fn admit<R, W, E>(
        &mut self,
        message: &Message,
        _received: Instant,
        _wire: &io::Wire<R, W, E>,
    ) -> Result<(), Error> {
        let Message::Permission {
            request_id,
            tool_name,
            input,
            ..
        } = message
        else {
            return Err(Error::State);
        };
        if self.seen.contains(request_id) {
            return Err(Error::Identity);
        }
        // Bound concurrent waits, not the number of tools in a long native
        // turn. Retain only IDs for replay detection once a callback is done.
        self.entries
            .retain(|_, callback| matches!(callback.stage, Stage::Waiting | Stage::Prepared(_)));
        if self.entries.len() >= MAX_CALLBACKS || self.seen.len() >= super::MAX_CALLBACK_IDS {
            return Err(Error::Capacity);
        }
        let input = if self.policy.is_some() {
            // Input is already a one-MiB/depth-bounded decoded JSON object.
            // This tighter limit bounds all16 retained originals and responses.
            if serde_json::to_vec(input)
                .map_err(|_| Error::Capacity)?
                .len()
                > MAX_INPUT_BYTES
            {
                return Err(Error::Capacity);
            }
            Some(input.clone())
        } else {
            None
        };
        self.seen.insert(request_id.clone());
        self.entries.insert(
            request_id.clone(),
            Callback {
                stage: Stage::Waiting,
                tool: tool_name.clone(),
                input,
                owner: None,
                response: None,
            },
        );
        Ok(())
    }
    pub fn cancel(&mut self, id: &str) -> Result<(), Error> {
        let callback = self.entries.get_mut(id).ok_or(Error::Identity)?;
        if matches!(callback.stage, Stage::Cancelled) {
            return Err(Error::Identity);
        }
        callback.stage = Stage::Cancelled;
        callback.input = None;
        Ok(())
    }
    pub fn deadline<R, W, E>(&mut self, wire: &io::Wire<R, W, E>) -> Result<Deadline, Error> {
        if self.policy.is_none() {
            return Ok(wire.activity_deadline());
        }
        let until = self
            .entries
            .values()
            .filter_map(|callback| match callback.stage {
                Stage::Waiting => callback.owner,
                Stage::Prepared(_) => callback.response,
                Stage::Sent | Stage::Cancelled => None,
            })
            .min()
            .map(Deadline::from)
            .unwrap_or_else(|| {
                *self
                    .read_deadline
                    .get_or_insert_with(|| wire.activity_deadline())
            });
        if until.expired() {
            return Err(Error::Timeout);
        }
        Ok(until)
    }
}
impl<R, W, E> SessionDriver<R, W, E> {
    /// The host's ordinary-question wait may differ from owner approval time.
    /// It does not extend the connection's admitted lifetime.
    pub fn set_question_wait(&mut self, wait_ms: u64) -> Result<(), Error> {
        if !self.control.entries.is_empty() || wait_ms == 0 {
            return Err(Error::State);
        }
        self.control.question_wait_ms = Some(wait_ms);
        Ok(())
    }
    /// Explicit host opt-in after original stream identity binding and before
    /// any callback is consumed. Does not authorize an allow or durable response.
    pub fn enable_approval_control(&mut self, policy: ApprovalControlPolicy) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.enable_inner(policy);
        operation.finish(result)
    }
    fn enable_inner(&mut self, policy: ApprovalControlPolicy) -> Result<(), Error> {
        if self.session_id.is_none()
            || self.control.policy.is_some()
            || !self.control.entries.is_empty()
        {
            return Err(Error::State);
        }
        self.wire.permission_deadlines(
            Instant::now(),
            policy.owner_wait_ms,
            policy.response_reserve_ms,
        )?;
        self.control.policy = Some(policy);
        Ok(())
    }
    /// Start the owner wait when this callback is presented. Concurrent native
    /// callbacks can queue behind it without spending an invisible deadline.
    /// Repeated reads never extend an already presented request.
    pub fn approval_deadline(&mut self, id: &str) -> Result<Instant, Error> {
        let policy = self.control.policy.ok_or(Error::State)?;
        let callback = self.control.entries.get_mut(id).ok_or(Error::Identity)?;
        if !matches!(callback.stage, Stage::Waiting | Stage::Prepared(_)) {
            return Err(Error::PermissionUnavailable);
        }
        if let Some(owner) = callback.owner {
            return Ok(owner);
        }
        let (owner, response) = self.wire.permission_deadlines(
            Instant::now(),
            if callback.tool == super::questions::TOOL {
                self.control
                    .question_wait_ms
                    .unwrap_or(policy.owner_wait_ms)
            } else {
                policy.owner_wait_ms
            },
            policy.response_reserve_ms,
        )?;
        callback.owner = Some(owner);
        callback.response = Some(response);
        Ok(owner)
    }
    /// Freeze once from the privately retained original input. The Host must
    /// hold this value before awaiting durable response-begin and must compare
    /// the exact original grant before sending. Encoding grants no authority.
    pub fn prepare_approval(
        &mut self,
        id: &str,
        decision: PermissionDecision,
    ) -> Result<PreparedApproval, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.prepare_inner(id, decision);
        operation.finish(result)
    }
    fn prepare_inner(
        &mut self,
        id: &str,
        decision: PermissionDecision,
    ) -> Result<PreparedApproval, Error> {
        self.prepare_response(id, |tool, input| match decision {
            PermissionDecision::Allow if tool == super::questions::TOOL => Err(Error::PermissionUnavailable),
            PermissionDecision::Allow => Ok(json!({"behavior":"allow","updatedInput":input})),
            PermissionDecision::Deny => Ok(json!({"behavior":"deny","message":"Permission denied by Hagency.","interrupt":true})),
        })
    }

    /// A question is data from the original native callback. A similarly
    /// shaped Bash/MCP input can never enter the question channel.
    pub fn question(&self, id: &str) -> Result<Vec<crate::questions::Question>, Error> {
        let callback = self.control.entries.get(id).ok_or(Error::Identity)?;
        if callback.tool != super::questions::TOOL || !matches!(callback.stage, Stage::Waiting) {
            return Err(Error::PermissionUnavailable);
        }
        super::questions::parse(callback.input.as_ref().ok_or(Error::State)?)
    }

    /// Answers replace only the original AskUserQuestion answers. This API
    /// cannot change a tool's input or grant any ordinary tool permission.
    pub fn prepare_question(
        &mut self,
        id: &str,
        answers: &crate::questions::Answers,
    ) -> Result<PreparedApproval, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.prepare_response(id, |tool, input| {
            if tool != super::questions::TOOL {
                return Err(Error::PermissionUnavailable);
            }
            super::questions::response(input, answers)
        });
        operation.finish(result)
    }

    fn prepare_response(
        &mut self,
        id: &str,
        response: impl FnOnce(&str, Value) -> Result<Value, Error>,
    ) -> Result<PreparedApproval, Error> {
        if self.interrupt.blocks_replies() {
            return Err(Error::PermissionUnavailable);
        }
        if self.control.policy.is_none() {
            return Err(Error::State);
        }
        self.approval_deadline(id)?;
        let until = self.callback_deadline()?;
        self.wire.check(until)?;
        let callback = self.control.entries.get_mut(id).ok_or(Error::Identity)?;
        if !matches!(callback.stage, Stage::Waiting) {
            return Err(Error::PermissionUnavailable);
        }
        if Instant::now() >= callback.owner.ok_or(Error::State)? {
            return Err(Error::Timeout);
        }
        let input = callback.input.take().ok_or(Error::State)?;
        let response = response(&callback.tool, input)?;
        let bytes = crate::claude::encode(json!({"type":"control_response","response":{
            "subtype":"success","request_id":id,"response":response}}))?;
        let response_deadline = callback.response.ok_or(Error::State)?;
        self.control.sequence = self
            .control
            .sequence
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let sequence = self.control.sequence;
        callback.stage = Stage::Prepared(sequence);
        Ok(PreparedApproval {
            source: self.source.clone(),
            id: id.into(),
            frame: io::PreparedFrame::new(sequence, bytes, response_deadline),
            response_deadline,
            sequence,
        })
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// Keep the SAME host future pinned across Message returns. Control returns
    /// its output once; never poll a completed future. No public read is raced
    /// or cancelled internally; selection happens at cancellation-safe leaf IO.
    pub async fn next_or_control<F: Future + ?Sized>(
        &mut self,
        control: Pin<&mut F>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.control_inner(control).await;
        operation.finish(result)
    }
    async fn control_inner<F: Future + ?Sized>(
        &mut self,
        mut control: Pin<&mut F>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        if self.control.policy.is_none() {
            return Err(Error::State);
        }
        let until = self.callback_deadline()?;
        for _ in 0..64 {
            match self.wire.next_or_control(control.as_mut(), until).await? {
                io::Controlled::Message(received) => {
                    if self.observe_prelude(&received.message)? {
                        continue;
                    }
                    self.observe(&received.message, received.at)?;
                    return Ok(ControlUpdate::Message(received.message));
                }
                io::Controlled::Control(output) => return Ok(ControlUpdate::Control(output)),
            }
        }
        Err(Error::Capacity)
    }
    /// Send only following positive original durable grant/begin/recheck. Each
    /// Message return suspends this exact frame: process it, recheck authority,
    /// then continue this same value. No ordinary read resumes its writer.
    pub async fn send_prepared_approval(
        &mut self,
        prepared: &mut PreparedApproval,
    ) -> Result<PreparedUpdate, Error> {
        // The peer can read the whole response and end its turn before this
        // host observes its own flush. A write whose bytes have all left only
        // waits for that flush, so it may complete after the Result was
        // observed; it transmits nothing new. Every other send still needs a
        // running turn.
        let expected = if self.phase == Phase::ResultObserved
            && self.wire.awaits_flush_only(&prepared.frame)
        {
            Phase::ResultObserved
        } else {
            Phase::Running
        };
        let operation = Operation::new(self, expected)?;
        let result = operation.driver.send_inner(prepared).await;
        operation.finish(result)
    }
    async fn send_inner(
        &mut self,
        prepared: &mut PreparedApproval,
    ) -> Result<PreparedUpdate, Error> {
        if self.interrupt.blocks_replies() {
            return Err(Error::PermissionUnavailable);
        }
        if !Arc::ptr_eq(&self.source, &prepared.source) {
            return Err(Error::Identity);
        }
        if self.control.policy.is_none() {
            return Err(Error::State);
        }
        let until = self.callback_deadline()?;
        self.wire.check(until)?;
        let callback = self
            .control
            .entries
            .get(&prepared.id)
            .ok_or(Error::Identity)?;
        if !matches!(callback.stage,Stage::Prepared(sequence) if sequence==prepared.sequence) {
            return Err(Error::PermissionUnavailable);
        }
        match self.wire.send_prepared(&mut prepared.frame, until).await? {
            io::Controlled::Message(received) => {
                self.observe(&received.message, received.at)?;
                Ok(PreparedUpdate::Message(received.message))
            }
            io::Controlled::Control(receipt) => {
                self.control
                    .entries
                    .get_mut(&prepared.id)
                    .ok_or(Error::Identity)?
                    .stage = Stage::Sent;
                Ok(PreparedUpdate::WriteAccepted(receipt))
            }
        }
    }
}
