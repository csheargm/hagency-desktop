//! One disposable Octos session over a private `octos serve --stdio`
//! (ADR-193): the handshake, the dispatch's turn and every continuation turn
//! the kernel runs for its background work, until Octos reports the session
//! idle. No task, approval or cleanup authority.
mod control;
mod io;
mod observation;
mod questions;
use super::{Frame, Notification, Outcome, Payload, Usage};
pub use control::{
    ApprovalControlPolicy, ControlUpdate, PermissionDecision, PreparedApproval, PreparedUpdate,
};
pub use io::{Limits, StderrSnapshot, Termination, WriteProgress};
pub use observation::{
    CompletionReceipt, MAX_OBSERVATIONS, Observation, ObservationKind, ObservationSource, UsageCoverage, UsageEvidence,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

/// How long a session must stay quiet after its last terminal, with nothing
/// reported active, before it counts as idle. Octos reports a session's work
/// on a two-second tick, so a turn too short for one tick reports nothing;
/// two ticks and a margin.
pub const QUIET_MS: u64 = 5_000;
/// Notifications read while a request awaits its answer, kept for `next`.
const MAX_PENDING: usize = 256;
/// Turns one dispatch may run: its own and the kernel's continuations.
const MAX_TURNS: usize = 64;
/// Envelopes that move a reply or end a turn, remembered to drop redelivery.
const MAX_SEEN: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid Octos session limits")]
    Configuration,
    #[error("invalid Octos session state")]
    State,
    #[error("Octos session is closed")]
    Closed,
    #[error("Octos session operation was cancelled")]
    Cancelled,
    #[error("Octos session deadline exceeded")]
    Timeout,
    #[error("Octos session IO failed ({0})")]
    Io(&'static str),
    #[error("Octos session peer closed")]
    PeerEof,
    #[error("Octos session capacity exceeded")]
    Capacity,
    #[error("Octos session protocol failed: {0}")]
    Protocol(super::Error),
    #[error("Octos refused the request (code {0})")]
    Refused(i64),
    #[error("Octos session identity or answer mismatch")]
    Identity,
    #[error("Octos request is cancelled or already answered")]
    PermissionUnavailable,
    #[error("Octos session was closed by its host")]
    HostClosed,
}
impl From<super::Error> for Error {
    fn from(error: super::Error) -> Self {
        Self::Protocol(error)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    New,
    Ready,
    Open,
    Running,
    Idle,
    Closed,
}
/// The fixed permission profile of one dispatch (ADR-193 decision 3): never
/// `danger_full_access` and never approval policy `never`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permissions {
    /// `workspace_write`, network denied, approvals on request.
    WorkspaceWrite,
    /// `read_only`, network denied, approvals on request.
    ReadOnly,
}
/// What the dispatch's work came to when Octos went idle.
pub struct Idle {
    pub outcome: Outcome,
    pub error_code: Option<String>,
    /// The last turn's reply: its last persisted assistant text after its last
    /// tool start, as Octos's own client takes it.
    pub reply: Option<String>,
    /// The dispatch's turn and every continuation turn that ended.
    pub turns: u32,
}
/// What the host must act on while the session runs. Payloads may carry
/// private text and tool input; deliberately no Debug.
pub enum Event {
    /// A continuation turn the kernel started for the dispatch's work.
    TurnStarted {
        turn_id: String,
    },
    Approval {
        approval_id: String,
        turn_id: String,
        params: Value,
    },
    /// Octos settled an approval this host had not answered: cancelled,
    /// timed out or decided elsewhere.
    ApprovalSettled {
        approval_id: String,
    },
    Question {
        question_id: String,
        turn_id: String,
        questions: Vec<crate::questions::Question>,
    },
    QuestionCancelled {
        question_id: String,
    },
    /// Accepted steers the kernel returned without consuming, before terminal.
    SteerDropped {
        turn_id: String,
        inputs: Vec<String>,
    },
    ToolCall {
        params: Value,
    },
    ToolCancelled {
        call_id: String,
    },
    /// One of the turn's tools started or ended, for the activity notice.
    Tool {
        tool_call_id: String,
        name: Option<String>,
        ended: bool,
    },
    /// One turn ended: the dispatch's own or a continuation.
    TurnEnded {
        turn_id: String,
        outcome: Outcome,
    },
    Idle(Idle),
}
/// The most host tools one registration may carry (Octos's own bound).
pub const MAX_HOST_TOOLS: usize = 64;
/// How long Octos waits for one host tool's result before it gives up.
pub const HOST_TOOL_TIMEOUT_MS: u64 = 180_000;
/// One host tool as Octos registers it: `<app>.<tool>`, shown to the model as
/// `<app>_<tool>`.
pub struct HostTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// `read`, else `act`. Neither asks the owner in Octos.
    pub read_only: bool,
}
/// One host tool call Octos sent. Arguments may carry private text;
/// deliberately no Debug.
pub struct HostToolCall {
    pub call_id: String,
    pub name: String,
    pub args: Value,
}
/// Octos's rule for a host tool name: two to four dotted segments, each
/// `[a-z][a-z0-9_]{0,31}`, at most 64 bytes.
pub fn host_tool_name(name: &str) -> bool {
    let segments: Vec<&str> = name.split('.').collect();
    name.len() <= 64
        && (2..=4).contains(&segments.len())
        && segments.iter().all(|segment| {
            let bytes = segment.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 32
                && bytes[0].is_ascii_lowercase()
                && bytes
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        })
}
#[derive(Default)]
struct Turn {
    reply: Option<String>,
    reply_id: Option<String>,
    terminal: Option<(Outcome, Option<String>)>,
}

/// Private payloads deliberately have no Debug/Serialize projection. The caller
/// owns these streams; only OwnedOctosSession separately owns a process.
pub struct SessionDriver<R, W, E> {
    wire: io::Wire<R, W, E>,
    phase: Phase,
    next_id: u64,
    session_id: Option<String>,
    dispatch_turn: Option<String>,
    /// The dispatch's turn and each turn Octos announced on the session. A
    /// background child stream has its own identity and is not a turn.
    turns: BTreeMap<String, Turn>,
    last_ended: Option<String>,
    seen: BTreeSet<(String, u64)>,
    background_closed: BTreeSet<String>,
    /// Octos's last report of the session's whole job.
    active: bool,
    reported_idle: bool,
    /// When every known turn had ended; reset by any new turn.
    quiet_from: Option<Instant>,
    /// Notifications read while a request awaited its answer, with the time
    /// each was received.
    pending: VecDeque<(Notification, Instant)>,
    observations: observation::State,
    completion: Option<CompletionReceipt>,
    control: control::State,
    questions: BTreeMap<String, (String, Vec<crate::questions::Question>)>,
    seen_questions: BTreeSet<String>,
    live_text: Option<crate::live_text::LiveText>,
    delta_sequences: BTreeMap<String, u64>,
    active_only_steer: bool,
    host_scoped_profile: bool,
    host_scoped_workspace_io: bool,
    host_scoped_search: bool,
    host_scoped_vision: bool,
    host_patch_worker: bool,
    host_scoped_web: bool,
    host_calls: BTreeMap<String, String>,
    seen_host_calls: BTreeSet<String>,
    source: Arc<()>,
}
impl<R, W, E> SessionDriver<R, W, E> {
    pub fn new(stdout: R, stdin: W, stderr: E, limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            wire: io::Wire::new(stdout, stdin, stderr, limits)?,
            phase: Phase::New,
            next_id: 0,
            session_id: None,
            dispatch_turn: None,
            turns: BTreeMap::new(),
            last_ended: None,
            seen: BTreeSet::new(),
            background_closed: BTreeSet::new(),
            active: false,
            reported_idle: false,
            quiet_from: None,
            pending: VecDeque::new(),
            observations: observation::State::default(),
            completion: None,
            control: control::State::default(),
            questions: BTreeMap::new(),
            seen_questions: BTreeSet::new(),
            live_text: None,
            delta_sequences: BTreeMap::new(),
            active_only_steer: false,
            host_scoped_profile: false,
            host_scoped_workspace_io: false,
            host_scoped_search: false,
            host_scoped_vision: false,
            host_patch_worker: false,
            host_scoped_web: false,
            host_calls: BTreeMap::new(),
            seen_host_calls: BTreeSet::new(),
            source: Arc::new(()),
        })
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    /// Restrict every subsequent wire operation to a live, renewable lease.
    /// This is only a restriction; it does not grant process custody or pool
    /// authority. It must be installed before the turn is dispatched.
    pub fn restrict_activity(
        &mut self,
        activity: Arc<dyn Fn() -> Option<Instant> + Send + Sync>,
    ) -> Result<(), Error> {
        if !matches!(self.phase, Phase::New | Phase::Ready | Phase::Open) {
            return Err(Error::State);
        }
        self.wire.restrict_activity(activity)
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    pub fn dispatch_turn(&self) -> Option<&str> {
        self.dispatch_turn.as_deref()
    }
    pub fn supports_active_only_steer(&self) -> bool {
        self.active_only_steer
    }
    pub fn supports_host_scoped_profile(&self) -> bool {
        self.host_scoped_profile
    }
    pub fn supports_host_scoped_workspace_io(&self) -> bool { self.host_scoped_workspace_io }
    pub fn supports_host_scoped_search(&self) -> bool { self.host_scoped_search }
    pub fn supports_host_scoped_vision(&self) -> bool { self.host_scoped_vision }
    pub fn supports_host_patch_worker(&self) -> bool { self.host_patch_worker }
    pub fn supports_host_scoped_web(&self) -> bool { self.host_scoped_web }
    pub fn question_pending(&self, id: &str) -> bool {
        self.questions.get(id).is_some_and(|(turn, _)|
            self.turns.get(turn).is_some_and(|turn| turn.terminal.is_none()))
    }
    pub fn host_tool_pending(&self, id: &str) -> bool {
        self.host_calls.get(id).is_some_and(|turn|
            self.turns.get(turn).is_some_and(|turn| turn.terminal.is_none()))
    }
    pub fn termination(&self) -> Option<&Termination> {
        self.wire.termination()
    }
    pub fn write_progress(&self) -> Option<WriteProgress> {
        self.wire.write_progress()
    }
    pub fn stderr_snapshot(&self) -> StderrSnapshot {
        self.wire.stderr_snapshot()
    }
    pub fn close(&mut self) {
        self.fail(Error::HostClosed);
    }
    fn fail(&mut self, error: Error) {
        self.phase = Phase::Closed;
        self.observations.retire();
        self.pending.clear();
        self.control.clear();
        self.questions.clear();
        self.host_calls.clear();
        self.wire.close(error);
    }
    fn ours(&self, session: &str) -> bool {
        self.session_id.as_deref() == Some(session)
    }
    /// The dispatch's turn and every announced turn have ended, and Octos
    /// reports nothing still working.
    fn settled(&self) -> bool {
        self.dispatch_turn
            .as_ref()
            .and_then(|turn| self.turns.get(turn))
            .is_some_and(|turn| turn.terminal.is_some())
            && self.turns.values().all(|turn| turn.terminal.is_some())
            && !self.active
    }
    /// Settled, and either Octos said so or the session stayed quiet for
    /// longer than its report interval.
    fn idle_due(&self) -> bool {
        self.settled()
            && (self.reported_idle
                || self
                    .quiet_from
                    .is_some_and(|at| Instant::now() >= at + Duration::from_millis(QUIET_MS)))
    }
    /// A notification that starts work again, read while the totals were.
    fn restarts(&self, notification: &Notification) -> bool {
        match notification {
            Notification::TurnStarted { session_id, .. } => self.ours(session_id),
            Notification::Orchestration { session_id, active } => *active && self.ours(session_id),
            _ => false,
        }
    }
    fn first_sight(&mut self, thread: String, seq: u64) -> Result<bool, Error> {
        if self.seen.contains(&(thread.clone(), seq)) {
            return Ok(false);
        }
        if self.seen.len() >= MAX_SEEN {
            return Err(Error::Capacity);
        }
        self.seen.insert((thread, seq));
        Ok(true)
    }
    fn publish_reply(&self, turn: &Turn) -> Result<(), Error> {
        if let (Some(output), Some(id), Some(text)) = (&self.live_text, &turn.reply_id, &turn.reply) {
            output.message(id, text).map_err(|_| Error::Capacity)?;
        }
        Ok(())
    }
    /// The state one notification changes, and the event the host must see.
    fn apply(
        &mut self,
        notification: Notification,
        received: Instant,
    ) -> Result<Option<Event>, Error> {
        Ok(match notification {
            Notification::TurnStarted {
                session_id,
                turn_id,
            } if self.ours(&session_id) => {
                if self.turns.contains_key(&turn_id) {
                    // The dispatch's own turn, announced after `turn/start`.
                    return Ok(None);
                }
                if self.turns.len() >= MAX_TURNS {
                    return Err(Error::Capacity);
                }
                // A continuation's result supersedes the previous turn's
                // final reply, so preserve that earlier reply as its own row.
                if let Some(previous) = self.last_ended.as_ref().and_then(|id| self.turns.get(id)) {
                    self.publish_reply(previous)?;
                }
                self.turns.insert(turn_id.clone(), Turn::default());
                self.quiet_from = None;
                self.reported_idle = false;
                Some(Event::TurnStarted { turn_id })
            }
            Notification::Envelope(envelope)
                if self.ours(&envelope.session_id)
                    && matches!(&envelope.payload, Payload::BackgroundComplete { parent_turn_id, .. }
                        if self.turns.contains_key(parent_turn_id)) =>
            {
                if matches!(&envelope.payload, Payload::BackgroundComplete { parent_turn_id, .. }
                    if envelope.turn_id.as_str() == parent_turn_id.as_str() || self.turns.contains_key(&envelope.turn_id)) {
                    return Ok(None);
                }
                if self.background_closed.contains(&envelope.thread_id) {
                    return Ok(None);
                }
                if !self.first_sight(envelope.thread_id.clone(), envelope.seq)? {
                    return Ok(None);
                }
                let Payload::BackgroundComplete { message_id, text, .. } = envelope.payload else { unreachable!() };
                if let Some(output) = &self.live_text {
                    output.message(&format!("octos:message:{message_id}"), &text)
                        .map_err(|_| Error::Capacity)?;
                }
                if self.background_closed.len() >= MAX_SEEN { return Err(Error::Capacity); }
                self.background_closed.insert(envelope.thread_id);
                None
            }
            Notification::Envelope(envelope)
                if self.ours(&envelope.session_id)
                    && self.turns.contains_key(&envelope.turn_id) =>
            {
                if matches!(envelope.payload, Payload::Other) {
                    return Ok(None);
                }
                if matches!(envelope.payload, Payload::AssistantDelta { .. }) {
                    if self.live_text.is_none() { return Ok(None); }
                    if !self.delta_sequences.contains_key(&envelope.thread_id)
                        && self.delta_sequences.len() >= MAX_PENDING { return Err(Error::Capacity); }
                    if self.delta_sequences.get(&envelope.thread_id).is_some_and(|seq| *seq >= envelope.seq) {
                        return Ok(None);
                    }
                    self.delta_sequences.insert(envelope.thread_id, envelope.seq);
                } else if !self.first_sight(envelope.thread_id, envelope.seq)? {
                    return Ok(None);
                }
                let turn_id = envelope.turn_id;
                let turn = self.turns.get_mut(&turn_id).ok_or(Error::State)?;
                if turn.terminal.is_some() {
                    // A terminal is a hard barrier: nothing after it moves the
                    // turn, and a second terminal cannot overwrite the first.
                    return Ok(None);
                }
                match envelope.payload {
                    Payload::AssistantDelta { text, segment_id } => {
                        if let Some(output) = &self.live_text {
                            output.append(&format!("{turn_id}/{segment_id}"), &text);
                        }
                        None
                    }
                    Payload::AssistantPersisted { text, segment_id, message_id } => {
                        let id = message_id.map(|id| format!("octos:message:{id}"))
                            .unwrap_or_else(|| format!("octos:segment:{turn_id}:{}", segment_id.as_deref().unwrap_or("persisted")));
                        if turn.reply_id.as_deref().is_some_and(|previous| previous != id) {
                            if let (Some(output), Some(previous), Some(text)) = (&self.live_text, &turn.reply_id, &turn.reply) {
                                output.message(previous, text).map_err(|_| Error::Capacity)?;
                            }
                        } else if turn.reply_id.as_deref() == Some(&id) && turn.reply.as_ref() != Some(&text) {
                            return Err(Error::Identity);
                        }
                        turn.reply_id = Some(id);
                        turn.reply = Some(text.clone());
                        if let Some(output) = &self.live_text {
                            output.complete(&format!("{turn_id}/{}", segment_id.as_deref().unwrap_or("persisted")), &text);
                        }
                        None
                    }
                    Payload::ToolStart { tool_call_id, name } => {
                        if let (Some(output), Some(id), Some(text)) = (&self.live_text, &turn.reply_id, &turn.reply) {
                            output.message(id, text).map_err(|_| Error::Capacity)?;
                        }
                        turn.reply = None;
                        turn.reply_id = None;
                        tool_call_id.map(|tool_call_id| Event::Tool {
                            tool_call_id,
                            name,
                            ended: false,
                        })
                    }
                    Payload::ToolEnd { tool_call_id } => {
                        tool_call_id.map(|tool_call_id| Event::Tool {
                            tool_call_id,
                            name: None,
                            ended: true,
                        })
                    }
                    Payload::Terminal {
                        outcome,
                        error_code,
                        usage,
                    } => {
                        turn.terminal = Some((outcome, error_code));
                        self.questions.retain(|_, (id, _)| id != &turn_id);
                        self.host_calls.retain(|_, id| id != &turn_id);
                        self.observations.turn_ended(usage);
                        self.last_ended = Some(turn_id.clone());
                        if self.turns.values().all(|turn| turn.terminal.is_some()) {
                            self.quiet_from = Some(Instant::now());
                        }
                        Some(Event::TurnEnded { turn_id, outcome })
                    }
                    Payload::BackgroundComplete { .. } | Payload::Other => None,
                }
            }
            Notification::Orchestration { session_id, active } if self.ours(&session_id) => {
                self.active = active;
                self.reported_idle = !active;
                None
            }
            Notification::PlanUpdated { session_id, turn_id: Some(turn_id), plan }
                if self.ours(&session_id)
                    && self.turns.get(&turn_id).is_some_and(|turn| turn.terminal.is_none()) =>
            {
                if let Some(output) = &self.live_text && let Some(text) = render_plan(&plan) {
                    output.complete(&format!("{turn_id}/plan"), &text);
                }
                None
            }
            Notification::ApprovalRequested {
                session_id,
                approval_id,
                turn_id,
                params,
            } if self.ours(&session_id) => {
                self.control.admit(&approval_id, received, &self.wire)?;
                Some(Event::Approval {
                    approval_id,
                    turn_id,
                    params,
                })
            }
            Notification::ApprovalSettled {
                session_id,
                approval_id,
            } if self.ours(&session_id) => self
                .control
                .settled(&approval_id, self.wire.writing_prepared())
                .then_some(Event::ApprovalSettled { approval_id }),
            Notification::QuestionRequested { session_id, question_id, turn_id, params }
                if self.ours(&session_id)
                    && self.turns.get(&turn_id).is_some_and(|turn| turn.terminal.is_none()) =>
            {
                if self.seen_questions.contains(&question_id) {
                    return Ok(None);
                }
                if self.seen_questions.len() >= MAX_PENDING || self.questions.len() >= 8 {
                    return Err(Error::Capacity);
                }
                let questions = questions::parse(&params)?;
                self.seen_questions.insert(question_id.clone());
                self.questions.insert(question_id.clone(), (turn_id.clone(), questions.clone()));
                Some(Event::Question { question_id, turn_id, questions })
            }
            Notification::QuestionCancelled { session_id, question_id, turn_id }
                if self.ours(&session_id) =>
            {
                if self.questions.get(&question_id).is_some_and(|(id, _)| id == &turn_id) {
                    self.questions.remove(&question_id);
                    Some(Event::QuestionCancelled { question_id })
                } else {
                    None
                }
            }
            Notification::SteerDropped { session_id, turn_id, inputs }
                if self.ours(&session_id) && self.dispatch_turn.as_deref() == Some(&turn_id)
                    && self.turns.get(&turn_id).is_some_and(|turn| turn.terminal.is_none()) =>
            {
                Some(Event::SteerDropped { turn_id, inputs })
            }
            Notification::ToolCall { params }
                if params["session_id"]
                    .as_str()
                    .is_some_and(|session| self.ours(session))
                    && params["turn_id"].as_str().is_some_and(|id|
                        self.turns.get(id).is_some_and(|turn| turn.terminal.is_none())) =>
            {
                let call_id = params["call_id"].as_str().ok_or(Error::Identity)?;
                let turn_id = params["turn_id"].as_str().ok_or(Error::Identity)?;
                if self.seen_host_calls.contains(call_id) { return Ok(None); }
                if self.seen_host_calls.len() >= MAX_SEEN || self.host_calls.len() >= MAX_PENDING {
                    return Err(Error::Capacity);
                }
                self.seen_host_calls.insert(call_id.to_owned());
                self.host_calls.insert(call_id.to_owned(), turn_id.to_owned());
                Some(Event::ToolCall { params })
            }
            Notification::ToolCancelled { call_id } => {
                self.host_calls.remove(&call_id).map(|_| Event::ToolCancelled { call_id })
            }
            // Another session's or a child stream's frames, and every other
            // notification: OUP's additive rule.
            _ => None,
        })
    }
    fn idle(&mut self) -> Result<Idle, Error> {
        let last = self.last_ended.as_ref().ok_or(Error::State)?;
        let turn = self.turns.get(last).ok_or(Error::State)?;
        let (outcome, error_code) = turn.terminal.clone().ok_or(Error::State)?;
        let reply = turn.reply.clone();
        let turns = u32::try_from(self.turns.len()).map_err(|_| Error::Capacity)?;
        self.phase = Phase::Idle;
        Ok(Idle {
            outcome,
            error_code,
            reply,
            turns,
        })
    }
    fn observe(&mut self, event: Event) -> Result<Event, Error> {
        let session = self.session_id.clone().ok_or(Error::State)?;
        self.control.observed();
        self.observations.accept(&event, &session)?;
        if let Event::Idle(idle) = &event {
            let observed = self.observations.last().ok_or(Error::State)?;
            self.completion = Some(CompletionReceipt::new(observed.source().clone(), observed.sequence(),
                idle.outcome, idle.reply.as_deref().unwrap_or_default()));
        }
        Ok(event)
    }
    /// One frame read outside a request: Octos's answer to an approval this
    /// host sent, or a notification and the event it makes, observed.
    fn receive(&mut self, received: io::Received) -> Result<Option<Event>, Error> {
        match received.message {
            Frame::Response { id, .. } => {
                if !self.control.answered(&id) {
                    return Err(Error::Identity);
                }
                Ok(None)
            }
            Frame::Notification(notification) => match self.apply(notification, received.at)? {
                Some(event) => self.observe(event).map(Some),
                None => Ok(None),
            },
        }
    }
}
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// One request and its answer. Notifications that arrive first are kept,
    /// in order, for `next`.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, Error> {
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Capacity)?;
        let id = format!("hagency-{}", self.next_id);
        let until = self.wire.event_deadline();
        self.wire
            .send(super::request(&id, method, params)?, until)
            .await?;
        loop {
            match self.wire.next(until).await?.message {
                Frame::Response {
                    id: answered,
                    outcome,
                } => {
                    if answered == id {
                        return outcome.map_err(|error| Error::Refused(error.code));
                    }
                    // Octos's answer to an approval this host sent earlier.
                    if !self.control.answered(&answered) {
                        return Err(Error::Identity);
                    }
                }
                Frame::Notification(notification) => {
                    if self.pending.len() >= MAX_PENDING {
                        return Err(Error::Capacity);
                    }
                    self.pending.push_back((notification, Instant::now()));
                }
            }
        }
    }
    /// `client_hello` with Hagency's fixed features; the server must speak
    /// `octos-ui/v1alpha1`.
    pub async fn hello(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::New)?;
        let result = operation.driver.hello_inner().await;
        operation.finish(result)
    }
    async fn hello_inner(&mut self) -> Result<(), Error> {
        let answer = self
            .call(
                "client_hello",
                json!({"transport":"stdio","client":"hagency",
                    "supported_features":super::FEATURES}),
            )
            .await?;
        if answer["type"] != "server_hello"
            || answer["capabilities"]["version"]["protocol"] != super::PROTOCOL
        {
            return Err(Error::Protocol(super::Error::Unsupported));
        }
        self.active_only_steer = answer["capabilities"]["supported_features"]
            .as_array().is_some_and(|features| ["turn.steer_active_only.v1", "event.turn_steer_dropped.v1"]
                .iter().all(|wanted| features.iter().any(|feature| feature.as_str() == Some(*wanted))));
        self.host_scoped_profile = answer["capabilities"]["supported_features"]
            .as_array().is_some_and(|features| features.iter().any(|feature|
                feature.as_str() == Some("runtime.host_scoped_profile.v1")));
        self.host_scoped_workspace_io = answer["capabilities"]["supported_features"]
            .as_array().is_some_and(|features| features.iter().any(|feature|
                feature.as_str() == Some("runtime.host_scoped_workspace_io.v1")));
        self.host_scoped_search = answer["capabilities"]["supported_features"]
            .as_array().is_some_and(|features| features.iter().any(|feature|
                feature.as_str() == Some("runtime.host_scoped_search.v1")));
        self.host_scoped_vision = answer["capabilities"]["supported_features"]
            .as_array().is_some_and(|features| features.iter().any(|feature|
                feature.as_str() == Some("runtime.host_scoped_vision.v1")));
        self.host_patch_worker = answer["capabilities"]["supported_features"]
            .as_array().is_some_and(|features| features.iter().any(|feature|
                feature.as_str() == Some("runtime.host_patch_worker.v1")));
        self.host_scoped_web = answer["capabilities"]["supported_features"]
            .as_array().is_some_and(|features| features.iter().any(|feature|
                feature.as_str() == Some("runtime.host_scoped_web.v1")));
        self.phase = Phase::Ready;
        Ok(())
    }
    /// The permission profile, then a fresh session bound to `cwd`. Octos must
    /// bind exactly that workspace and profile.
    pub async fn open(
        &mut self,
        session_id: &str,
        profile_id: &str,
        cwd: &str,
        permissions: Permissions,
    ) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Ready)?;
        let result = operation
            .driver
            .open_inner(session_id, profile_id, cwd, permissions)
            .await;
        operation.finish(result)
    }
    async fn open_inner(
        &mut self,
        session_id: &str,
        profile_id: &str,
        cwd: &str,
        permissions: Permissions,
    ) -> Result<(), Error> {
        if !super::session_key(session_id, profile_id) || !cwd.starts_with('/') {
            return Err(Error::Protocol(super::Error::Input));
        }
        let mode = match permissions {
            Permissions::WorkspaceWrite => "workspace_write",
            Permissions::ReadOnly => "read_only",
        };
        // Before `session/open`, so the session's runtime is built with it.
        let applied = self
            .call(
                "permission/profile/set",
                json!({"session_id":session_id,
                    "update":{"mode":mode,"network":"deny","approval_policy":"on-request"}}),
            )
            .await?;
        if applied["current"]["mode"] != mode || applied["current"]["network"] != "deny" {
            return Err(Error::Identity);
        }
        let answer = self
            .call(
                "session/open",
                json!({"session_id":session_id,"profile_id":profile_id,"cwd":cwd}),
            )
            .await?;
        let opened = &answer["opened"];
        if opened["session_id"] != session_id
            || opened["workspace_root"] != cwd
            || opened
                .get("active_profile_id")
                .is_some_and(|active| active != profile_id)
        {
            return Err(Error::Identity);
        }
        self.session_id = Some(session_id.to_owned());
        self.phase = Phase::Open;
        Ok(())
    }
    /// The dispatch's one turn: accepted, not executed.
    pub async fn start_turn(&mut self, turn_id: &str, text: &str) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Open)?;
        let result = operation.driver.start_inner(turn_id, text).await;
        operation.finish(result)
    }
    async fn start_inner(&mut self, turn_id: &str, text: &str) -> Result<(), Error> {
        if !super::uuid(turn_id) || text.is_empty() || text.len() > super::MAX_TEXT_BYTES {
            return Err(Error::Protocol(super::Error::Input));
        }
        let session_id = self.session_id.clone().ok_or(Error::State)?;
        self.turns.insert(turn_id.to_owned(), Turn::default());
        self.dispatch_turn = Some(turn_id.to_owned());
        let answer = self
            .call(
                "turn/start",
                json!({"session_id":session_id,"turn_id":turn_id,
                    "input":[{"kind":"text","text":text}]}),
            )
            .await?;
        if answer["accepted"] != true {
            return Err(Error::Identity);
        }
        self.phase = Phase::Running;
        Ok(())
    }
    /// Enable presentation-only deltas before the turn starts.
    pub fn enable_live_text(&mut self, output: crate::live_text::LiveText) -> Result<(), Error> {
        if self.phase != Phase::Open {
            return Err(Error::State);
        }
        self.live_text = Some(output);
        Ok(())
    }
    /// Answer only a question admitted from this active session and turn.
    /// A stale question is not a successful delivery.
    pub async fn answer_question(
        &mut self,
        question_id: &str,
        answers: &crate::questions::Answers,
    ) -> Result<bool, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.answer_question_inner(question_id, answers).await;
        operation.finish(result)
    }
    async fn answer_question_inner(
        &mut self,
        question_id: &str,
        answers: &crate::questions::Answers,
    ) -> Result<bool, Error> {
        let Some((turn_id, questions)) = self.questions.get(question_id) else {
            return Ok(false);
        };
        if !self.turns.get(turn_id).is_some_and(|turn| turn.terminal.is_none()) {
            return Ok(false);
        }
        let answers = questions::response(questions, answers)?;
        let session_id = self.session_id.clone().ok_or(Error::State)?;
        let result = self.call("user_question/respond", json!({
            "session_id":session_id, "question_id":question_id, "answers":answers,
        })).await;
        self.questions.remove(question_id);
        match result {
            Ok(value) if value["question_id"] == question_id => Ok(value["accepted"] == true),
            Ok(_) => Err(Error::Identity),
            Err(Error::Refused(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }
    /// Submit a control only to the original active turn. In particular, never
    /// use OUP's idle `turn/steer` fallback, which would start another turn.
    pub async fn steer(&mut self, text: &str) -> Result<bool, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.control_inner(Some(text)).await;
        operation.finish(result)
    }
    pub async fn interrupt(&mut self) -> Result<bool, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.control_inner(None).await;
        operation.finish(result)
    }
    async fn control_inner(&mut self, text: Option<&str>) -> Result<bool, Error> {
        if text.is_some_and(|text| text.is_empty() || text.len() > super::MAX_TEXT_BYTES) {
            return Err(Error::Protocol(super::Error::Input));
        }
        // Older Octos servers start a new turn if steering races completion,
        // even when expected_turn_id is present. Never issue that request
        // unless the server negotiated the active-only precondition.
        if text.is_some() && !self.active_only_steer {
            return Ok(false);
        }
        let turn = self.dispatch_turn.clone().ok_or(Error::State)?;
        if !self.turns.get(&turn).is_some_and(|turn| turn.terminal.is_none()) {
            return Ok(false);
        }
        let session = self.session_id.clone().ok_or(Error::State)?;
        let (method, params) = match text {
            Some(text) => ("turn/steer", json!({"session_id":session,
                "expected_turn_id":turn,"require_active":true,
                "input":[{"kind":"text","text":text}]})),
            None => ("turn/interrupt", json!({"session_id":session,"turn_id":turn})),
        };
        match self.call(method, params).await {
            Ok(value) if text.is_some() => {
                Ok(value["steered"] == true && value["turn_id"] == turn)
            }
            Ok(value) => Ok(value["interrupted"] == true),
            Err(Error::Refused(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }
    /// Hagency's task tools on the open session, before its turn (ADR-193
    /// decision 5). They add to Octos's own tools: `generic_tools`, which
    /// would narrow them, is never sent. Each is `act` or `read` and never
    /// outward, so Octos runs it without an approval of its own; every call
    /// still meets the dispatch's capability in Hagency.
    pub async fn register_tools(&mut self, tools: &[HostTool]) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Open)?;
        let result = operation.driver.register_inner(tools, None).await;
        operation.finish(result)
    }
    /// A scoped text session grants only human questions and the gateway to
    /// these host-owned tools. Native shell, filesystem, network and inherited
    /// connectors are outside this tool surface. The executor still needs OS
    /// isolation and process custody. Verify Octos echoed the exact list.
    pub async fn register_shared_text_tools(&mut self, tools: &[HostTool]) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Open)?;
        let result = operation.driver.register_inner(tools, Some(&["ask_user_question", "discover_tools", "run_tool"])).await;
        operation.finish(result)
    }
    pub(crate) async fn register_shared_policy_tools(&mut self, tools: &[HostTool], generic: Vec<&str>) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Open)?;
        let result = operation.driver.register_inner(tools, Some(&generic)).await;
        operation.finish(result)
    }
    async fn register_inner(&mut self, tools: &[HostTool], restricted: Option<&[&str]>) -> Result<(), Error> {
        if (tools.is_empty() && restricted.is_none()) || tools.len() > MAX_HOST_TOOLS {
            return Err(Error::Protocol(super::Error::Input));
        }
        let session_id = self.session_id.clone().ok_or(Error::State)?;
        let mut declared = Vec::with_capacity(tools.len());
        for tool in tools {
            if !host_tool_name(&tool.name)
                || tool.description.trim().is_empty()
                || tool.description.len() > 2048
                || tool.input_schema["type"] != "object"
            {
                return Err(Error::Protocol(super::Error::Input));
            }
            declared.push(json!({"name":tool.name,"description":tool.description,
                "input_schema":tool.input_schema,
                "risk":if tool.read_only { "read" } else { "act" },
                "background":false,"outward":false}));
        }
        let mut params = json!({"session_id":session_id,"tools":declared,
            "call_timeout_ms":HOST_TOOL_TIMEOUT_MS});
        let generic = restricted.map(|names| json!(names));
        if let Some(generic) = &generic { params["generic_tools"] = generic.clone(); }
        let answer = self
            .call(
                "peer/tools/register",
                params,
            )
            .await?;
        // A tool Octos skipped (a kernel tool of the same name) is not offered.
        let registered = answer["tools"].as_array().ok_or(Error::Identity)?;
        if answer["session_id"] != session_id.as_str()
            || generic.as_ref().is_some_and(|generic| answer["generic_tools"] != *generic || answer["applies"] != "next_turn")
            || registered.len() != tools.len()
            || tools.iter().zip(registered).any(|(tool, answer)| {
                answer["name"] != tool.name.as_str()
                    || answer["model_name"] != tool.name.replace('.', "_").as_str()
            })
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
    /// One `peer/tool/call` for this session: its call ID, its declared
    /// (dotted) name and its arguments. Its other fields stay unread.
    pub fn host_tool_call(&self, params: &Value) -> Result<HostToolCall, Error> {
        let session = params["session_id"].as_str().ok_or(Error::Identity)?;
        let turn = params["turn_id"].as_str().ok_or(Error::Identity)?;
        let call_id = params["call_id"].as_str().ok_or(Error::Identity)?;
        let name = params["name"].as_str().ok_or(Error::Identity)?;
        if !self.ours(session)
            || !self.turns.get(turn).is_some_and(|turn| turn.terminal.is_none())
            || self.host_calls.get(call_id).is_none_or(|id| id != turn)
            || call_id.is_empty()
            || call_id.len() > 128
            || call_id.chars().any(char::is_control)
            || !host_tool_name(name)
        {
            return Err(Error::Identity);
        }
        let args = match params.get("args") {
            None | Some(Value::Null) => json!({}),
            Some(args) if args.is_object() => args.clone(),
            Some(_) => return Err(Error::Identity),
        };
        Ok(HostToolCall {
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            args,
        })
    }
    /// Answer one host tool call: `Ok(data)` is the tool's output, `Err`
    /// its failure message. `false` when Octos no longer waits for it (it
    /// timed out or was cancelled): Octos already told the model, and the
    /// session goes on.
    pub async fn host_tool_result(
        &mut self,
        call_id: &str,
        result: Result<Value, String>,
    ) -> Result<bool, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.result_inner(call_id, result).await;
        operation.finish(result)
    }
    async fn result_inner(
        &mut self,
        call_id: &str,
        result: Result<Value, String>,
    ) -> Result<bool, Error> {
        if !self.host_tool_pending(call_id) { return Ok(false); }
        let session_id = self.session_id.clone().ok_or(Error::State)?;
        let mut params = json!({"session_id":session_id,"call_id":call_id});
        match result {
            Ok(data) => {
                params["ok"] = true.into();
                params["data"] = data;
            }
            Err(message) => {
                let mut message = message;
                if message.len() > 4096 {
                    let mut end = 4096;
                    while !message.is_char_boundary(end) {
                        end -= 1;
                    }
                    message.truncate(end);
                }
                params["ok"] = false.into();
                params["error"] = json!({"kind":"tool_error","message":message});
            }
        }
        let result = self.call("peer/tool/result", params).await;
        self.host_calls.remove(call_id);
        match result {
            Ok(answer) if answer["call_id"] == call_id => Ok(answer["accepted"] == true),
            Ok(_) => Err(Error::Identity),
            Err(Error::Refused(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }
    /// The next event the host must see, ending with `Idle`. Every return
    /// carries exactly one observation (`last_observation`).
    pub async fn next(&mut self) -> Result<Event, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = match operation
            .driver
            .next_inner::<std::future::Pending<()>>(None)
            .await
        {
            Ok(ControlUpdate::Event(event)) => Ok(event),
            Ok(ControlUpdate::Control(())) => Err(Error::State),
            Err(error) => Err(error),
        };
        operation.finish(result)
    }
    /// The next event, or the host's own future when one is given and finishes
    /// first. The quiet wait, the owner bounds and the wire's deadlines bound
    /// every read.
    async fn next_inner<F: Future + ?Sized>(
        &mut self,
        mut control: Option<Pin<&mut F>>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        loop {
            if let Some((notification, received)) = self.pending.pop_front() {
                if let Some(event) = self.apply(notification, received)? {
                    return self.observe(event).map(ControlUpdate::Event);
                }
                continue;
            }
            if self.idle_due() {
                if let Some(event) = self.finish_idle().await? {
                    return self.observe(event).map(ControlUpdate::Event);
                }
                continue;
            }
            let wait = self.control.deadline(&self.wire)?;
            let quiet = self
                .quiet_from
                .filter(|_| self.settled())
                .map(|at| at + Duration::from_millis(QUIET_MS));
            let until = quiet.map_or(wait, |at| at.min(wait));
            let read = match control.as_mut() {
                Some(control) => match self.wire.next_or_control(control.as_mut(), until).await {
                    Ok(io::Controlled::Message(received)) => Ok(received),
                    Ok(io::Controlled::Control(output)) => {
                        return Ok(ControlUpdate::Control(output));
                    }
                    Err(error) => Err(error),
                },
                None => self.wire.next(until).await,
            };
            match read {
                Ok(received) => {
                    if let Some(event) = self.receive(received)? {
                        return Ok(ControlUpdate::Event(event));
                    }
                }
                // Only the quiet wait may end there; the wire's own event and
                // lifetime deadlines stay failures.
                Err(Error::Timeout)
                    if quiet.is_some_and(|at| at < wait && at < self.wire.lifetime())
                        && self.idle_due() => {}
                Err(error) => return Err(error),
            }
        }
    }
    /// The session's own totals close the usage record: they also cover its
    /// sub-agents. A refused or empty read keeps the turns' own sums. Work
    /// that began while the totals were read is not idle.
    async fn finish_idle(&mut self) -> Result<Option<Event>, Error> {
        let session_id = self.session_id.clone().ok_or(Error::State)?;
        let totals = match self
            .call("session/status/read", json!({"session_id":session_id}))
            .await
        {
            Ok(answer) => session_totals(&answer["usage"]),
            Err(Error::Refused(_)) => None,
            Err(error) => return Err(error),
        };
        if self.pending.iter().any(|(n, _)| self.restarts(n)) {
            return Ok(None);
        }
        self.observations.session_totals(totals);
        Ok(Some(Event::Idle(self.idle()?)))
    }
}
/// A plan is presentation only; malformed or oversized updates never change
/// execution authority or terminate an otherwise valid turn.
fn render_plan(plan: &Value) -> Option<String> {
    let items = plan.get("items")?.as_array()?;
    if items.len() > 64 { return None; }
    let mut lines = vec!["Plan / 计划".to_owned()];
    if items.is_empty() { lines.push("(no steps / 无步骤)".to_owned()); }
    for item in items {
        let title = item.get("title")?.as_str()?;
        if title.is_empty() || title.len() > 1024 || title.chars().any(|c| c.is_control() && c != '\t') { return None; }
        let mark = match item.get("status")?.as_str()? {
            "pending" => "○", "in_progress" => "◉", "completed" => "✓", _ => return None,
        };
        lines.push(format!("{mark} {title}"));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    #[test]
    fn plan_preview_is_bounded_and_handles_statuses() {
        let plan = json!({"items":[{"title":"Inspect","status":"completed"},
            {"title":"Build","status":"in_progress"},{"title":"Verify","status":"pending"}]});
        assert_eq!(render_plan(&plan).as_deref(), Some("Plan / 计划\n✓ Inspect\n◉ Build\n○ Verify"));
        assert!(render_plan(&json!({"items":[{"title":"unsafe\nline","status":"pending"}]})).is_none());
        assert!(render_plan(&json!({"items":vec![json!({"title":"x","status":"pending"}); 65]})).is_none());
        assert!(render_plan(&json!({"items":[{"title":"x","status":"unknown"}]})).is_none());
        assert_eq!(render_plan(&json!({"items":[]})).as_deref(), Some("Plan / 计划\n(no steps / 无步骤)"));
    }

    #[derive(Default)]
    struct Messages(std::sync::Mutex<Vec<(String, String)>>);
    impl crate::live_text::MessageSink for Messages {
        fn record(&self, id: &str, text: &str) -> Result<(), crate::live_text::MessageError> {
            self.0.lock().unwrap().push((id.into(), text.into()));
            Ok(())
        }
    }

    #[test]
    fn separate_octos_messages_preserve_scope_and_background_identity() {
        let messages = Arc::new(Messages::default());
        let mut session = SessionDriver::new(tokio::io::empty(), tokio::io::sink(), tokio::io::empty(), Limits::default()).unwrap();
        session.session_id = Some("coding:local:one".into());
        session.dispatch_turn = Some("parent".into());
        session.turns.insert("parent".into(), Turn::default());
        session.live_text = Some(crate::live_text::LiveText::with_messages(messages.clone()));
        let event = |stream: &str, turn: &str, seq, payload| Notification::Envelope(super::super::Envelope {
            session_id: "coding:local:one".into(), thread_id: stream.into(), turn_id: turn.into(), seq, payload,
        });
        let persisted = |id: &str, text: &str| Payload::AssistantPersisted {
            text: text.into(), segment_id: Some(id.into()), message_id: Some(id.into()),
        };
        session.apply(event("parent-stream", "parent", 1, persisted("one", "first")), Instant::now()).unwrap();
        session.apply(event("parent-stream", "parent", 2, persisted("two", "second")), Instant::now()).unwrap();
        session.apply(event("parent-stream", "parent", 3, Payload::ToolStart { tool_call_id: None, name: None }), Instant::now()).unwrap();
        session.apply(event("parent-stream", "parent", 4, persisted("final", "last answer")), Instant::now()).unwrap();
        let background = || Payload::BackgroundComplete { parent_turn_id: "parent".into(), task_id: "task".into(), message_id: "background".into(), text: "late report".into() };
        session.apply(event("child-stream", "child", 1, background()), Instant::now()).unwrap();
        session.apply(event("child-stream", "child", 1, background()), Instant::now()).unwrap();
        session.apply(event("forged-stream", "parent", 1, background()), Instant::now()).unwrap();
        session.apply(event("other-stream", "child-two", 1, Payload::BackgroundComplete {
            parent_turn_id: "foreign".into(), task_id: "task".into(), message_id: "forged".into(), text: "wrong room".into(),
        }), Instant::now()).unwrap();
        let rows = messages.0.lock().unwrap();
        assert_eq!(rows.iter().map(|(_, text)| text.as_str()).collect::<Vec<_>>(), ["first", "second", "late report"]);
        assert_eq!(session.turns["parent"].reply.as_deref(), Some("last answer"));
    }
}

/// `session/status/read` usage. An empty object (no recorded run, or no
/// ledger) is unknown, never zero.
fn session_totals(usage: &Value) -> Option<Usage> {
    let counter = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| *n <= 9_007_199_254_740_991)
    };
    Some(Usage {
        input: counter("input_tokens")?,
        output: counter("output_tokens")?,
        reasoning: 0,
        cache_read: counter("cached_input_tokens")?,
        cache_write: counter("cache_write_input_tokens")?,
    })
}
struct Operation<'a, R, W, E> {
    driver: &'a mut SessionDriver<R, W, E>,
    finished: bool,
}
impl<'a, R, W, E> Operation<'a, R, W, E> {
    fn new(driver: &'a mut SessionDriver<R, W, E>, expected: Phase) -> Result<Self, Error> {
        if driver.phase == Phase::Closed {
            return Err(Error::Closed);
        }
        if driver.phase != expected {
            driver.fail(Error::State);
            return Err(Error::State);
        }
        Ok(Self {
            driver,
            finished: false,
        })
    }
    fn finish<T>(mut self, result: Result<T, Error>) -> Result<T, Error> {
        if let Err(error) = result {
            self.driver.fail(error);
        }
        self.finished = true;
        result
    }
}
impl<R, W, E> Drop for Operation<'_, R, W, E> {
    fn drop(&mut self) {
        if !self.finished {
            self.driver.fail(Error::Cancelled);
        }
    }
}
