//! One native conversation stream. Task, approval and cleanup authority belong to the host.
mod control;
mod interrupt;
mod io;
mod lifecycle;
mod observation;
mod questions;
mod room_mcp;
mod task_mcp;
mod usage;
mod missing_resume;
use super::{ControlOutcome, EventKind, Message};
pub use control::{
    ApprovalControlPolicy, ControlUpdate, PermissionDecision, PreparedApproval, PreparedUpdate,
};
pub use interrupt::{InterruptReceipt, InterruptUpdate, PreparedInterrupt};
pub use io::{Limits, StderrSnapshot, Termination, WriteProgress};
pub use observation::{Observation, ObservationKind, ObservationSource};
pub use room_mcp::{PreparedRoomReply, RoomToolRequest};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
pub use usage::{UsageCounts, UsageCoverage, UsageDiagnostics, UsageEvidence};

const INITIALIZE_ID: &str = "hagency-claude-initialize-1";
// These IDs are retained for replay detection. Stream observations retain only
// their latest receipt and must not share this cumulative storage bound.
const MAX_CALLBACK_IDS: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid Claude question or answer data")]
    Malformed,
    #[error("invalid Claude session limits")]
    Configuration,
    #[error("invalid Claude session state")]
    State,
    #[error("Claude session is closed")]
    Closed,
    #[error("Claude session operation was cancelled")]
    Cancelled,
    #[error("Claude session deadline exceeded")]
    Timeout,
    #[error("Claude session IO failed ({0})")]
    Io(&'static str),
    #[error("Claude session peer closed")]
    PeerEof,
    #[error("Claude session capacity exceeded")]
    Capacity,
    #[error("Claude session protocol failed: {0}")]
    Protocol(super::Error),
    #[error("Claude native resume context is missing before input")]
    MissingResume,
    #[error("Claude initialization was refused")]
    Refused,
    #[error("Claude session identity or control response mismatch")]
    Identity,
    #[error("Claude permission request is cancelled or already answered")]
    PermissionUnavailable,
    #[error("Claude scoped task helper was not connected with its exact tool inventory")]
    TaskWriterStartup,
    #[error("Claude room tools were not connected with their exact inventory")]
    RoomToolStartup,
    #[error("Claude room tool request is cancelled or already answered")]
    RoomToolUnavailable,
    #[error("Claude session was closed by its host")]
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
    Initializing,
    Ready,
    Running,
    ResultObserved,
    Closed,
}

/// Private payloads deliberately have no Debug/Serialize projection. The caller
/// owns these streams; only OwnedClaudeSession separately owns a process.
pub struct SessionDriver<R, W, E> {
    wire: io::Wire<R, W, E>,
    phase: Phase,
    session_id: Option<String>,
    prelude_session_id: Option<String>,
    completion: lifecycle::Completion,
    continuous: bool,
    awaiting_init: bool,
    prompt_id: Option<String>,
    control: control::State,
    interrupt: interrupt::State,
    source: Arc<()>,
    account: Option<super::account::Metadata>,
    observations: observation::State,
    task_mcp: Option<super::TaskMcp>,
    task_mcp_attempted: bool,
    room_mcp: Option<room_mcp::State>,
}
impl<R, W, E> SessionDriver<R, W, E> {
    pub(crate) fn attach_shared_activity(
        &mut self,
        activity: Arc<dyn Fn() -> Option<tokio::time::Instant> + Send + Sync>,
    ) -> Result<(), Error> {
        if self.phase != Phase::Ready {
            return Err(Error::State);
        }
        self.wire.attach_activity(activity)
    }
    pub(crate) fn qualify_account(
        &self,
        binding: &super::account::AccountBinding,
    ) -> Result<super::account::AccountObservation, Error> {
        binding.check()?;
        if self.phase != Phase::Ready || !self.account.as_ref().is_some_and(|a| a.matches(binding))
        {
            return Err(Error::Identity);
        }
        Ok(super::account::AccountObservation {
            source: self.source.clone(),
            binding: binding.clone(),
        })
    }
    pub(crate) fn matches_account_observation(
        &self,
        receipt: &super::account::AccountObservation,
    ) -> bool {
        self.phase != Phase::Closed
            && Arc::ptr_eq(&self.source, &receipt.source)
            && self
                .account
                .as_ref()
                .is_some_and(|a| a.matches(&receipt.binding))
    }
    pub fn new(stdout: R, stdin: W, stderr: E, limits: Limits) -> Result<Self, Error> {
        Ok(Self {
            wire: io::Wire::new(stdout, stdin, stderr, limits)?,
            phase: Phase::New,
            session_id: None,
            prelude_session_id: None,
            completion: Default::default(),
            continuous: false,
            awaiting_init: false,
            prompt_id: None,
            control: control::State::default(),
            interrupt: Default::default(),
            source: Arc::new(()),
            account: None,
            observations: Default::default(),
            task_mcp: None,
            task_mcp_attempted: false,
            room_mcp: None,
        })
    }
    pub fn phase(&self) -> Phase {
        self.phase
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
    /// Pin the CLI's explicit resume argument before initialization or input.
    /// The original system/init must report this same native identity.
    pub(crate) fn expect_resume(&mut self, id: &str) -> Result<(), Error> {
        if self.phase != Phase::New || self.session_id.is_some()
            || id.is_empty() || id.starts_with('-') || id.len() > 128
            || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(Error::Identity);
        }
        self.session_id = Some(id.to_owned());
        Ok(())
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
        self.control.clear();
        self.room_mcp = None;
        self.wire.close(error);
    }
    fn observe(&mut self, message: &Message, received: tokio::time::Instant) -> Result<(), Error> {
        match message {
            Message::Event {
                session_id,
                kind,
                payload,
            } => {
                let init = *kind == EventKind::System && payload["subtype"] == "init";
                match &self.session_id {
                    bound if init && self.awaiting_init => {
                        if bound.as_ref().is_some_and(|id| id != session_id)
                            || self
                                .prompt_id
                                .as_ref()
                                .is_some_and(|id| !lifecycle::correlated(payload, id))
                        {
                            return Err(Error::Identity);
                        }
                        if self
                            .prelude_session_id
                            .as_ref()
                            .is_some_and(|id| id != session_id)
                        {
                            return Err(Error::Identity);
                        }
                        self.session_id = Some(session_id.clone());
                        self.awaiting_init = false;
                    }
                    Some(bound) if bound == session_id && !init && !self.awaiting_init => {}
                    _ => return Err(Error::Identity),
                }
                if *kind == EventKind::Result
                    && self
                        .prompt_id
                        .as_ref()
                        .is_some_and(|id| !lifecycle::correlated(payload, id))
                {
                    return Err(Error::Identity);
                }
                self.completion.observe(*kind, payload)?;
            }
            Message::Permission { request_id, .. } => {
                if self.session_id.is_none() || self.awaiting_init
                    || self.room_mcp.as_ref().is_some_and(|s| s.seen(request_id))
                {
                    return Err(Error::Identity);
                }
                self.control.admit(message, received, &self.wire)?;
            }
            Message::Mcp { .. } => self.admit_room_request(message, received)?,
            Message::ControlCancel { request_id } => {
                if self
                    .room_mcp
                    .as_ref()
                    .is_some_and(|state| state.seen(request_id))
                {
                    self.room_mcp
                        .as_mut()
                        .ok_or(Error::State)?
                        .cancel(request_id)?;
                } else {
                    self.control.cancel(request_id)?;
                }
            }
            Message::ControlResponse {
                request_id,
                outcome,
            } => {
                self.interrupt.observe(request_id, outcome, &self.wire)?;
            }
        }
        if self.completion.finished() && !self.interrupt.pending() {
            self.phase = Phase::ResultObserved;
        }
        self.control.observed();
        self.observations
            .accept(message, self.session_id.as_deref().ok_or(Error::Identity)?)?;
        Ok(())
    }
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
impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    pub async fn initialize(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::New)?;
        operation.driver.phase = Phase::Initializing;
        let result = operation.driver.initialize_inner().await;
        operation.finish(result)
    }
    async fn initialize_inner(&mut self) -> Result<(), Error> {
        // One response deadline covers the write and read, not one per event.
        let until = self.wire.event_deadline();
        self.wire
            .send(super::initialize(INITIALIZE_ID)?, until)
            .await?;
        match self.wire.next(until).await?.message {
            Message::ControlResponse {
                request_id,
                outcome,
            } if request_id == INITIALIZE_ID => match outcome {
                ControlOutcome::Success(value) => {
                    // Missing identity metadata does not prevent personal
                    // execution. A shared account qualification requires it.
                    self.account = super::account::Metadata::read(&value["account"]);
                    self.phase = Phase::Ready;
                    Ok(())
                }
                ControlOutcome::Refused => Err(Error::Refused),
            },
            Message::Event { session_id, kind: EventKind::Result, payload }
                if missing_resume::exact(self.session_id.as_deref(), &session_id, &payload) =>
                Err(Error::MissingResume),
            _ => Err(Error::Identity),
        }
    }
    /// Flushed stdin only: neither execution acknowledgement nor a task receipt.
    pub async fn prompt(&mut self, text: &str) -> Result<WriteProgress, Error> {
        let operation = Operation::new(self, Phase::Ready)?;
        let result = operation.driver.prompt_inner(text).await;
        operation.finish(result)
    }
    async fn prompt_inner(&mut self, text: &str) -> Result<WriteProgress, Error> {
        if self.wire.has_buffered_input() {
            return Err(Error::Identity);
        }
        if text.is_empty() || text.len() > super::MAX_TEXT_BYTES {
            return Err(Error::Protocol(super::Error::Input));
        }
        let guided = self
            .task_mcp
            .as_ref()
            .map(|helper| format!("{}\n\nAssigned task input:\n{}", helper.guidance(), text));
        let bytes = super::prompt(
            guided.as_deref().unwrap_or(text),
            self.session_id.as_deref(),
        )?;
        let bytes = if self.continuous {
            let id = lifecycle::prompt_id();
            let mut value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| Error::State)?;
            value["uuid"] = id.clone().into();
            self.prompt_id = Some(id);
            super::encode(value)?
        } else {
            bytes
        };
        self.awaiting_init = true;
        self.phase = Phase::Running;
        self.wire.send(bytes, self.wire.event_deadline()).await
    }
    /// Observations alone authorize no reply. The execution host binds them to
    /// the admitted call and handles each original private approval request.
    pub async fn next_message(&mut self) -> Result<Message, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.next_inner().await;
        operation.finish(result)
    }
    async fn next_inner(&mut self) -> Result<Message, Error> {
        let deadline = self.callback_deadline()?;
        // Current Claude emits command/state metadata before system/init. It
        // carries no usage or approval authority: bind its ID to the eventual
        // init, retain the same deadline, and keep init as observation one.
        for _ in 0..64 {
            let received = self.wire.next(deadline).await?;
            if self.observe_prelude(&received.message)? {
                continue;
            }
            self.observe(&received.message, received.at)?;
            return Ok(received.message);
        }
        Err(Error::Capacity)
    }
    // Ordinary reads and cooperative control reads must admit exactly the
    // same pre-init metadata, including the native /usage query after child
    // work finishes. Metadata never becomes usage or callback authority.
    fn observe_prelude(&mut self, message: &Message) -> Result<bool, Error> {
        if self.awaiting_init
            && let Message::Event {
                session_id,
                kind,
                payload,
            } = message
            && (*kind == EventKind::CommandLifecycle
                || (*kind == EventKind::System && payload["subtype"] == "session_state_changed"))
        {
            if self
                .prelude_session_id
                .as_ref()
                .is_some_and(|id| id != session_id)
                || self.session_id.as_ref().is_some_and(|id| id != session_id)
            {
                return Err(Error::Identity);
            }
            self.prelude_session_id = Some(session_id.clone());
            return Ok(true);
        }
        Ok(false)
    }
}
