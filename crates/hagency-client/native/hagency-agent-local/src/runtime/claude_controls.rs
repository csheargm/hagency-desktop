//! One cooperative read path for native events, host decisions and stop.
//! Never race/cancel an OwnedClaudeSession read with an outer select: its
//! cancellation guard deliberately retires the native process.
use super::{Claude, Error, Result};
use crate::run_controls::{self, Action, Outcome, Pending, Stream};
use hagency_runtime::claude::{
    Message,
    session::{ControlUpdate, InterruptUpdate, Observation, PreparedInterrupt},
};
use std::{future::Future, pin::Pin};

pub(super) type Observed = (Message, Option<Observation>);
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
pub(super) struct Controls {
    stream: Option<Stream>,
    writing: Option<(Pending, PreparedInterrupt)>,
    waiting: Option<(Pending, String)>,
    accepted: bool,
    output: Option<hagency_runtime::live_text::LiveText>,
    message_id: String,
}
impl Controls {
    pub fn new(
        mut stream: Option<Stream>,
        output: Option<hagency_runtime::live_text::LiveText>,
    ) -> Self {
        if let Some(stream) = &mut stream {
            stream.activate();
        }
        Self {
            stream,
            writing: None,
            waiting: None,
            accepted: false,
            output,
            message_id: String::new(),
        }
    }
    pub fn observe_output(&mut self, message: &Message) {
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
        self.writing.is_none() && self.waiting.is_none()
    }
    fn receipt(&mut self, wire: &Claude) {
        let Some(receipt) = wire.interrupt_receipt() else {
            return;
        };
        if self
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
    }
    fn receive(&mut self, wire: &mut Claude, pending: Pending) {
        match pending.action {
            // Claude's busy user messages are next commands, not steering of
            // the current turn. The Matrix host queues/admit/settles those as
            // independent calls on the retained session.
            Action::Steer { .. } => pending.reply(Outcome::Unsupported),
            Action::Interrupt
                if self.accepted || self.waiting.is_some() || self.writing.is_some() =>
            {
                pending.reply(Outcome::InProgress)
            }
            Action::Interrupt => match wire.prepare_interrupt() {
                Ok(prepared) => self.writing = Some((pending, prepared)),
                Err(_) => pending.reply(Outcome::Unavailable),
            },
        }
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
            if let Some((pending, prepared)) = self.writing.as_mut() {
                pending.begin_write();
                match wire
                    .send_prepared_interrupt(prepared)
                    .await
                    .map_err(|_| Error::Protocol("Claude interrupt write"))?
                {
                    InterruptUpdate::Message(message) => {
                        self.observe_output(&message);
                        return Ok(Update::Message((message, wire.last_observation().cloned())));
                    }
                    InterruptUpdate::Unavailable => {
                        let (pending, _) = self.writing.take().expect("original interrupt");
                        pending.reply(Outcome::Unavailable);
                    }
                    InterruptUpdate::WriteAccepted(_) => {
                        let (pending, prepared) = self.writing.take().expect("original interrupt");
                        self.waiting = Some((pending, prepared.request_id().into()));
                        self.receipt(wire);
                    }
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
                    self.observe_output(&message);
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
