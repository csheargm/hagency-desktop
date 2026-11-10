//! One cooperative read path for native events, host decisions and stop.
//! Never race/cancel an OwnedClaudeSession read with an outer select: its
//! cancellation guard deliberately retires the native process.
use super::{Claude, Error, Result};
use crate::run_controls::{self, Action, Outcome, Pending, Stream};
use hagency_runtime::claude::{
    Message,
    session::{
        ControlUpdate, InterruptUpdate, Observation, PreparedInterrupt, PreparedSteer, SteerState,
        SteerUpdate, join_answer,
    },
};
use std::{future::Future, pin::Pin};

pub(super) type Observed = (Message, Option<Observation>);

pub(super) fn remember_tool(ids: &mut std::collections::BTreeSet<String>, id: &str) -> Result<()> {
    // Preserve the previous memory bound for diagnostic identities while
    // allowing a text stream to outlive the former total-event ceiling.
    if !ids.contains(id) && ids.len() >= 16_384 {
        return Err(Error::Protocol("Claude tool diagnostic capacity"));
    }
    ids.insert(id.to_owned());
    Ok(())
}

/// The native driver has already consumed lifecycle and usage observations,
/// and Controls has already published text deltas. Only callbacks, final
/// results and tool-count diagnostics need a later host dispatch. Retaining
/// every text frame here makes an unrelated long stream exhaust a waiting
/// approval's queue.
pub(super) fn defer(
    buffered: &mut std::collections::VecDeque<Observed>,
    message: Message,
    observation: Option<Observation>,
) -> Result<()> {
    // The CLI can withdraw a callback that still waits here behind another
    // host decision. The driver has already retired that identity, so it can
    // no longer be presented or answered: drop only that queued request and
    // keep the turn. A withdrawal of the request being answered is seen by
    // its own wait, and every other callback stays queued in order.
    if let Message::ControlCancel { request_id } = &message {
        buffered.retain(|(queued, _)| {
            !matches!(
                queued,
                Message::Permission { request_id: id, .. } | Message::Mcp { request_id: id, .. }
                    if id == request_id
            )
        });
        return Ok(());
    }
    let needed = match &message {
        Message::Permission { .. } | Message::Mcp { .. } => true,
        Message::Event { kind, payload, .. } => {
            *kind == hagency_runtime::claude::EventKind::Result
                || payload["message"]["content"]
                    .as_array()
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block["type"] == "tool_use" || block["type"] == "tool_result"
                        })
                    })
        }
        _ => false,
    };
    if needed {
        if buffered.len() >= 256 {
            return Err(Error::Unknown);
        }
        buffered.push_back((message, observation));
    }
    Ok(())
}
/// The last original result of this turn exactly as it was read, so that a
/// later failure of the native stream cannot discard a clean final answer.
pub(super) struct Answer {
    pub text: String,
    /// A successful result that the native peer did not abort.
    pub clean: bool,
}
// Keep native events inline on this hot read path. Boxing every text delta
// would add an allocation solely to shrink the occasional empty control arm.
#[allow(clippy::large_enum_variant)]
pub(super) enum Update<T> {
    Message(Observed),
    Ready(T),
    Interrupted,
    Idle,
}
enum Input<T> {
    Host(T),
    Action(Pending),
}
/// The one control frame in flight. Each is resumed across native messages
/// until its flush; no other host write may start inside it.
enum Writing {
    Stop(Pending, PreparedInterrupt),
    Steer(Pending, PreparedSteer),
}
pub(super) struct Controls<'a, C: super::claude_turn::Capture> {
    capture: &'a mut C,
    stream: Option<Stream>,
    writing: Option<Writing>,
    waiting: Option<(Pending, String)>,
    /// Written steers the native queue has not answered for, by message id.
    steers: Vec<(Pending, String)>,
    accepted: bool,
    output: Option<hagency_runtime::live_text::LiveText>,
    message_id: String,
    answer: Option<Answer>,
}
impl<'a, C: super::claude_turn::Capture> Controls<'a, C> {
    pub fn new(
        mut stream: Option<Stream>,
        output: Option<hagency_runtime::live_text::LiveText>,
        capture: &'a mut C,
    ) -> Self {
        if let Some(stream) = &mut stream {
            stream.activate();
        }
        Self {
            capture,
            stream,
            writing: None,
            waiting: None,
            steers: Vec::new(),
            accepted: false,
            output,
            message_id: String::new(),
            answer: None,
        }
    }
    pub fn take_answer(&mut self) -> Option<Answer> {
        self.answer.take()
    }
    fn observe_answer(&mut self, wire: &Claude, message: &Message) {
        let Message::Event {
            kind: hagency_runtime::claude::EventKind::Result,
            payload,
            ..
        } = message
        else {
            return;
        };
        // The accounting query's own result is never the answer.
        let Some(observation) = wire
            .last_observation()
            .filter(|observation| !observation.is_usage_snapshot())
        else {
            return;
        };
        // A steer that ran as a cycle of its own continues the answer read so
        // far, as the driver's receipt does. Any other result replaces it.
        let mut text = match self.answer.take() {
            Some(answer) if observation.continues_answer() => answer.text,
            _ => String::new(),
        };
        join_answer(&mut text, payload["result"].as_str().unwrap_or(""));
        self.answer = Some(Answer {
            text,
            clean: payload["is_error"] == false
                && payload["terminal_reason"] != "aborted_streaming",
        });
    }
    pub async fn observe_message(&mut self, wire: &Claude, message: &Message) -> Result<()> {
        self.observe_answer(wire, message);
        if let Some(observation) = wire.last_observation() {
            self.capture.observe(observation).await?;
        }
        self.observe_output(message);
        Ok(())
    }
    pub async fn complete(&mut self, wire: &Claude, text: &str) -> Result<()> {
        self.capture.complete(wire, text).await
    }
    pub async fn complete_unaccounted(
        &mut self,
        receipt: &hagency_runtime::claude::session::CompletionReceipt,
        text: &str,
    ) -> Result<()> {
        self.capture.complete_unaccounted(receipt, text).await
    }
    fn observe_output(&mut self, message: &Message) {
        let Some(output) = &self.output else {
            return;
        };
        let Message::Event { kind, payload, .. } = message else {
            return;
        };
        if !payload["parent_tool_use_id"].is_null() {
            return;
        }
        if *kind == hagency_runtime::claude::EventKind::Stream {
            let event = &payload["event"];
            if event["type"] == "message_start"
                && let Some(id) = event["message"]["id"].as_str()
            {
                self.message_id = id.into();
            }
            if !self.message_id.is_empty()
                && event["type"] == "content_block_delta"
                && event["delta"]["type"] == "text_delta"
                && let Some(delta) = event["delta"]["text"].as_str()
            {
                output.append(&self.message_id, delta);
            }
        } else if *kind == hagency_runtime::claude::EventKind::Assistant
            && let (Some(id), Some(blocks)) = (
                payload["message"]["id"].as_str(),
                payload["message"]["content"].as_array(),
            )
        {
            // The assistant snapshot replaces its streamed deltas, avoiding
            // a second copy of the same model text in the live card.
            let completed = hagency_runtime::live_text::LiveText::default();
            for block in blocks {
                if block["type"] == "text"
                    && let Some(text) = block["text"].as_str()
                {
                    completed.append(id, text);
                }
            }
            if let Some((_, text)) = completed.since(0) {
                output.complete(id, &text);
            }
        }
    }
    pub fn accepted(&self) -> bool {
        self.accepted
    }
    pub fn settled(&self) -> bool {
        self.writing.is_none() && self.waiting.is_none() && self.steers.is_empty()
    }
    fn receipt(&mut self, wire: &Claude) {
        if let Some(receipt) = wire.interrupt_receipt()
            && self
                .waiting
                .as_ref()
                .is_some_and(|(_, id)| id == receipt.request_id())
        {
            let (pending, _) = self.waiting.take().expect("original interrupt");
            self.accepted = receipt.accepted();
            pending.reply(if self.accepted {
                Outcome::Accepted
            } else {
                Outcome::Rejected
            });
        }
        // The native queue acknowledges a steer; its flushed write does not.
        // A stop in progress withdraws a queued steer, so its answer waits
        // for that: withdrawn text was not taken and goes back to its sender.
        let stopping = self.accepted
            || self.waiting.is_some()
            || matches!(self.writing, Some(Writing::Stop(..)));
        let mut owed = Vec::new();
        for (pending, id) in self.steers.drain(..) {
            match wire.steer_state(&id) {
                Some(SteerState::Unacknowledged) => owed.push((pending, id)),
                Some(SteerState::Queued) if stopping => owed.push((pending, id)),
                Some(SteerState::Queued | SteerState::Started | SteerState::Completed) => {
                    pending.reply(Outcome::Accepted)
                }
                Some(SteerState::Cancelled) | None => pending.reply(Outcome::Unavailable),
            }
        }
        self.steers = owed;
    }
    fn receive(&mut self, wire: &mut Claude, pending: Pending) {
        let stopping = self.accepted || self.waiting.is_some() || self.writing.is_some();
        match &pending.action {
            // As for Codex: nothing joins a turn whose stop was requested,
            // and a turn that no longer runs takes no input.
            Action::Steer { .. } if stopping => pending.reply(Outcome::Unavailable),
            Action::Steer { text } => match wire.prepare_steer(text) {
                Ok(prepared) => self.writing = Some(Writing::Steer(pending, prepared)),
                Err(hagency_runtime::claude::session::Error::Capacity) => {
                    pending.reply(Outcome::Busy)
                }
                Err(_) => pending.reply(Outcome::Unavailable),
            },
            Action::Interrupt if stopping => pending.reply(Outcome::InProgress),
            Action::Interrupt => match wire.prepare_interrupt() {
                Ok(prepared) => self.writing = Some(Writing::Stop(pending, prepared)),
                Err(_) => pending.reply(Outcome::Unavailable),
            },
        }
    }
    /// Continue the control frame in flight. A native message read meanwhile
    /// is returned for the caller to handle before the same frame resumes.
    async fn write(&mut self, wire: &mut Claude) -> Result<Option<Observed>> {
        let message = match self.writing.as_mut() {
            Some(Writing::Stop(pending, prepared)) => {
                pending.begin_write();
                match wire
                    .send_prepared_interrupt(prepared)
                    .await
                    .map_err(|_| Error::Protocol("Claude interrupt write"))?
                {
                    InterruptUpdate::Message(message) => message,
                    InterruptUpdate::Unavailable => {
                        if let Some(Writing::Stop(pending, _)) = self.writing.take() {
                            pending.reply(Outcome::Unavailable);
                        }
                        return Ok(None);
                    }
                    InterruptUpdate::WriteAccepted(_) => {
                        if let Some(Writing::Stop(pending, prepared)) = self.writing.take() {
                            self.waiting = Some((pending, prepared.request_id().into()));
                        }
                        self.receipt(wire);
                        return Ok(None);
                    }
                }
            }
            Some(Writing::Steer(pending, prepared)) => {
                pending.begin_write();
                match wire
                    .send_prepared_steer(prepared)
                    .await
                    .map_err(|_| Error::Protocol("Claude steer write"))?
                {
                    SteerUpdate::Message(message) => message,
                    SteerUpdate::Unavailable => {
                        if let Some(Writing::Steer(pending, _)) = self.writing.take() {
                            pending.reply(Outcome::Unavailable);
                        }
                        return Ok(None);
                    }
                    SteerUpdate::WriteAccepted(_) => {
                        if let Some(Writing::Steer(pending, prepared)) = self.writing.take() {
                            self.steers.push((pending, prepared.message_id().into()));
                        }
                        self.receipt(wire);
                        return Ok(None);
                    }
                }
            }
            None => return Ok(None),
        };
        self.observe_message(wire, &message).await?;
        Ok(Some((message, wire.last_observation().cloned())))
    }
    /// A control write that a native message suspended must finish before
    /// the caller starts a host write that no cooperative read precedes.
    pub async fn finish_write(
        &mut self,
        wire: &mut Claude,
        buffered: &mut std::collections::VecDeque<Observed>,
    ) -> Result<()> {
        while self.writing.is_some() {
            if let Some((message, observation)) = self.write(wire).await? {
                defer(buffered, message, observation)?;
            }
        }
        Ok(())
    }
    pub async fn next<T, F: Future<Output = T> + ?Sized>(
        &mut self,
        wire: &mut Claude,
        mut host: Pin<&mut F>,
        withdraw_on_stop: bool,
    ) -> Result<Update<T>> {
        loop {
            self.receipt(wire);
            if self.accepted && withdraw_on_stop {
                return Ok(Update::Interrupted);
            }
            if self.settled()
                && wire.phase() == hagency_runtime::claude::session::Phase::ResultObserved
            {
                return Ok(Update::Idle);
            }
            if self.writing.is_some() {
                if let Some(observed) = self.write(wire).await? {
                    return Ok(Update::Message(observed));
                }
                continue;
            }
            // Keep the caller's original pinned decision across every native
            // message and interrupt acknowledgement. A refusal resumes it;
            // only accepted stop makes its owner withdraw it.
            let hold = self.waiting.is_some() || self.accepted;
            let update = {
                let next = async {
                    tokio::select! { biased;
                        pending = run_controls::next(&mut self.stream) => Input::Action(pending),
                        answer = host.as_mut(), if !hold => Input::Host(answer),
                    }
                };
                tokio::pin!(next);
                wire.next_or_control(next.as_mut())
                    .await
                    .map_err(|_| Error::Protocol("Claude controlled event"))?
            };
            match update {
                ControlUpdate::Message(message) => {
                    self.observe_message(wire, &message).await?;
                    self.receipt(wire);
                    return Ok(Update::Message((message, wire.last_observation().cloned())));
                }
                ControlUpdate::Control(Input::Host(answer)) => return Ok(Update::Ready(answer)),
                ControlUpdate::Control(Input::Action(pending)) => self.receive(wire, pending),
            }
        }
    }
    pub async fn message(&mut self, wire: &mut Claude) -> Result<Option<Observed>> {
        let idle = std::future::pending::<()>();
        tokio::pin!(idle);
        match self.next(wire, idle.as_mut(), false).await? {
            Update::Message(observed) => Ok(Some(observed)),
            Update::Idle => Ok(None),
            _ => unreachable!("idle host future cannot answer"),
        }
    }
}
