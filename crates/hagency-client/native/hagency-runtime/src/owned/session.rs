use super::{Cleanup, StartError};
use crate::codex::{
    MAX_REQUEST_MS,
    session::{self, InterruptDisposition, Outcome, Phase, SessionDriver, Settings, Update},
    transport,
};
#[cfg(windows)]
use hagency_platform::WindowsPipe as Receiver;
#[cfg(windows)]
use hagency_platform::WindowsPipe as Sender;
use hagency_platform::{Launch, SupervisedProcess};
use std::{io, path::Path, time::Duration};
#[cfg(unix)]
use tokio::net::unix::pipe::{Receiver, Sender};

type Session = SessionDriver<Receiver, Sender, Receiver>;
const STOP_TIMEOUT: Duration = Duration::from_secs(3);

/// Spawning and stopping use the platform's bounded synchronous guardian API.
/// Call from a host execution worker with a Tokio IO runtime, not an HTTP handler.
/// Dropping an operation also performs bounded stop; it creates no detached task.
pub struct OwnedSession {
    session: Session,
    owner: SupervisedProcess,
    cleanup: Cleanup,
    pub(super) managed_source: super::managed::ManagedProcessSource,
    pub(super) text_source: super::shared_text::SharedTextProcessSource,
    pub(super) live_text: Option<crate::live_text::LiveText>,
    /// Diagnostics build only (ADR-182 pin): every stop verdict of this
    /// session reads `Unknown`, as a tree the guardian could not prove gone
    /// would, however many times the host asks. No production build has it.
    #[cfg(feature = "test-diagnostics")]
    unproven: bool,
}
impl OwnedSession {
    pub fn shared_workspace(&self) -> &Path {
        Path::new(self.session.settings().cwd())
    }
    pub(super) fn shared_settings_match(&self, mode: Option<&str>) -> bool {
        self.session.settings().shared_mode_matches(mode)
    }
    pub fn spawn(
        guardian: &Path,
        launch: &Launch,
        settings: Settings,
        limits: transport::Limits,
        response_timeout_ms: u64,
    ) -> Result<Self, StartError> {
        if Path::new(settings.cwd()) != launch.directory
            || limits.validate().is_err()
            || response_timeout_ms == 0
            || response_timeout_ms > MAX_REQUEST_MS
            || tokio::runtime::Handle::try_current().is_err()
        {
            return Err(StartError::Settings);
        }
        let (mut owner, pipes) =
            SupervisedProcess::spawn_piped(guardian, launch).map_err(|error| {
                if error.kind() == io::ErrorKind::Unsupported {
                    StartError::Unsupported
                } else {
                    StartError::Uncertain {
                        kind: error.kind(),
                        cleanup: Cleanup::Unknown { kind: error.kind() },
                    }
                }
            })?;
        let session = (|| {
            // These consume checked FIFO descriptors and make only the host
            // endpoint nonblocking. Work keeps ordinary blocking stdio. No
            // spawn_blocking wrappers or uncancellable pipe-reader tasks exist.
            #[cfg(unix)]
            let (stdin, stdout, stderr) = {
                let (stdin, stdout, stderr) = pipes.into_parts();
                (
                    Sender::from_owned_fd(stdin)?,
                    Receiver::from_owned_fd(stdout)?,
                    Receiver::from_owned_fd(stderr)?,
                )
            };
            #[cfg(windows)]
            let (stdin, stdout, stderr) = pipes.into_async_parts()?;
            Session::new(stdout, stdin, stderr, settings, limits, response_timeout_ms).map_err(
                |_| io::Error::new(io::ErrorKind::InvalidInput, "invalid runner session limits"),
            )
        })();
        match session {
            Ok(session) => {
                tracing::info!(pid = owner.id(), "owned runner spawned");
                Ok(Self {
                    session,
                    owner,
                    cleanup: Cleanup::Pending,
                    managed_source: super::managed::ManagedProcessSource::new(),
                    text_source: super::shared_text::SharedTextProcessSource::new(),
                    live_text: None,
                    #[cfg(feature = "test-diagnostics")]
                    unproven: false,
                })
            }
            Err(error) => {
                let cleanup = observe_stop(&mut owner);
                Err(StartError::Uncertain {
                    kind: error.kind(),
                    cleanup,
                })
            }
        }
    }
    pub fn id(&self) -> u32 {
        self.owner.id()
    }
    pub fn phase(&self) -> Phase {
        self.session.phase()
    }
    pub fn bind_dispatch_policy(&mut self, settings: &Settings) -> Result<(), session::Error> {
        self.managed_source.retire();
        self.text_source.retire();
        self.session.bind_dispatch_policy(settings)
    }
    pub fn bind_task_mcp(&mut self, helper: session::TaskMcp) -> Result<(), session::Error> {
        self.managed_source.retire();
        self.text_source.retire();
        self.session.bind_task_mcp(helper)
    }
    pub fn reserve_warm_idle(&mut self) -> Result<(), session::Error> {
        self.session.reserve_warm_idle()
    }
    pub fn enter_warm_idle(&mut self, until: tokio::time::Instant) -> Result<(), session::Error> {
        self.session.enter_warm_idle(until)
    }
    /// Physical owner observation only, on this original worker. It cannot
    /// establish effective settings/model readiness or any domain authority.
    pub fn qualify_ready_owner(&mut self, timeout: Duration) -> Result<(), session::Error> {
        if self.phase() != Phase::Ready {
            return Err(session::Error::State);
        }
        let operation = Operation::new(self)?;
        let result = match operation.runner.owner.observe_leader(timeout) {
            Ok(true) => Ok(()),
            Ok(false) | Err(_) => Err(session::Error::State),
        };
        operation.finish(result)
    }
    pub fn consume_warm_idle(
        &mut self,
        limits: transport::Limits,
        response_timeout_ms: u64,
        until: tokio::time::Instant,
    ) -> Result<(), session::Error> {
        self.session
            .consume_warm_idle(limits, response_timeout_ms, until)
    }
    pub fn observation_source(&self) -> Result<session::ObservationSource, session::Error> {
        self.session.observation_source()
    }
    /// Read-only account quota metadata this actual process received from
    /// its own `account/rateLimits/read` or `account/rateLimits/updated`.
    /// Account-wide; never per-turn usage and never authority.
    pub fn provider_rate_limits(&self) -> Option<&session::RateLimitSnapshot> {
        self.session.provider_rate_limits()
    }
    /// Digest of the provider account subject this process reported
    /// (`{"provider":"openai.chatgpt","subject":accountId}`); raw ID not kept.
    pub fn provider_account_digest(&self) -> Option<&str> {
        self.session.provider_account_digest()
    }
    /// The host's canonical resource model, checked by the actual upstream
    /// thread response before the running turn was accepted.
    pub fn running_model(&self) -> Option<&str> {
        self.session.running_model()
    }
    pub fn matches_observation_source(&self, source: &session::ObservationSource) -> bool {
        self.session.matches_observation_source(source)
    }
    pub fn protocol_outcome(&self) -> Option<&Outcome> {
        self.session.outcome()
    }
    pub fn cleanup(&self) -> Cleanup {
        self.cleanup
    }
    pub fn transport_termination(&self) -> Option<&transport::Termination> {
        self.session.transport_termination()
    }
    pub fn last_server_request(&self) -> Option<&'static str> {
        self.session.last_server_request()
    }
    /// The refused server request's own method name, bounded and printable.
    pub fn last_server_request_method(&self) -> Option<&str> {
        self.session.last_server_request_method()
    }
    pub fn refused_notification(&self) -> Option<&'static str> {
        self.session.refused_notification()
    }
    /// The provider's own reason for ending the turn, bounded at admission
    /// (board #110). Diagnostic only — TS surfaces the same words, and without
    /// them a usage-limit refusal reached the operator as a bare `protocol`
    /// "Result uncertain" with no reason at all.
    pub fn turn_failure(&self) -> Option<&str> {
        self.session.turn_failure()
    }
    /// The last `max` bytes of the provider's turn-failure reason, cut on a
    /// character boundary like `stderr_tail`, for the attempt's record
    /// (board #110). Evidence only: no verdict, retry or authority reads it.
    pub fn turn_failure_tail(&self, max: usize) -> String {
        let Some(text) = self.session.turn_failure() else {
            return String::new();
        };
        let start = text.ceil_char_boundary(text.len().saturating_sub(max));
        text[start..]
            .chars()
            .map(|c| {
                if c.is_control() && c != '\n' {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }
    /// Whether the connection still holds this prepared server request. False
    /// once `serverRequest/resolved` was parsed: the one-shot frame's transmit
    /// path is gone, so it must never be re-sent.
    pub fn prepared_admissible(&self, id: &crate::codex::RequestId) -> bool {
        self.session.prepared_admissible(id)
    }
    /// `(accepted, total)` bytes of the frame in the transport's write
    /// custody, or `None` when no frame is held. Read-only: the approval
    /// turn-end rule uses it to distinguish a transmitted (uncertain) frame
    /// from a never-transmitted one. No authority, no retry, no verdict
    /// inside the runtime.
    pub fn write_progress(&self) -> Option<(usize, usize)> {
        self.session.write_progress()
    }
    pub fn stderr_snapshot(&self) -> transport::StderrSnapshot {
        self.session.stderr_snapshot()
    }
    /// The leader's exit as the guardian reaped it, `code:N` or `signal:N`,
    /// decoded from the observed report's raw wait status. `None` before a
    /// report, when the guardian never reaped the leader, or on a platform
    /// that does not carry the status. Evidence for the attempt record only.
    pub fn exit_identity(&self) -> Option<String> {
        let Cleanup::Observed(report) = self.cleanup else {
            return None;
        };
        exit_identity(report.leader_status?)
    }
    /// The last `max` bytes of the retained runtime stderr (ADR-040's 16 KiB
    /// tail), cut on a character boundary, with control characters other than
    /// newline replaced. Private diagnostic text: no projection reads it.
    pub fn stderr_tail(&self, max: usize) -> String {
        let snapshot = self.session.stderr_snapshot();
        let text = String::from_utf8_lossy(&snapshot.tail);
        let start = text.ceil_char_boundary(text.len().saturating_sub(max));
        text[start..]
            .chars()
            .map(|c| {
                if c.is_control() && c != '\n' {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }
    /// The guardian's own stderr tail as the platform collected it (ADR-181).
    pub fn guardian_stderr_tail(&self) -> String {
        self.owner.guardian_stderr_tail()
    }

    /// Stop through the retained owner. A false whole_tree_stopped or an IO
    /// error stays explicit; neither result frees a domain lease or task.
    pub fn stop(&mut self) -> Cleanup {
        self.managed_source.retire();
        self.text_source.retire();
        self.session.close();
        if !matches!(self.cleanup, Cleanup::Observed(report) if report.scope.whole_tree_stopped) {
            self.cleanup = observe_stop(&mut self.owner);
        }
        #[cfg(feature = "test-diagnostics")]
        if self.unproven {
            self.cleanup = Cleanup::Unknown {
                kind: io::ErrorKind::TimedOut,
            };
        }
        self.cleanup
    }
    /// Diagnostics build only: from now on this session's stop verdict is
    /// `Unknown`, the one shape an offline fixture cannot produce soundly
    /// (a tree that outlives SIGKILL for the guardian's whole budget). The
    /// real stop still runs; only the verdict the host reads is pinned.
    #[cfg(feature = "test-diagnostics")]
    pub fn unprove_stop(&mut self) {
        self.unproven = true;
    }

    /// Runtime opt-in only; durable owner grants and finite launch admission
    /// remain the execution host's responsibility.
    pub fn enable_approval_control(
        &mut self,
        policy: session::ApprovalControlPolicy,
    ) -> Result<(), session::Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.enable_approval_control(policy);
        operation.finish(result)
    }
    pub fn approval_deadline(
        &self,
        id: &crate::codex::RequestId,
    ) -> Result<tokio::time::Instant, session::Error> {
        self.session.approval_deadline(id)
    }
    pub fn native_turn_context(&self, thread: &str, turn: &str) -> Result<session::NativeTurnContext, session::Error> {
        self.session.native_turn_context(thread, turn)
    }
    /// The host takes on owner-wait expiry for this session. Grants nothing.
    pub fn enable_owner_wait_expiry(&mut self) -> Result<(), session::Error> {
        self.session.enable_owner_wait_expiry()
    }
    /// Hand one unanswered callback to the host at its owner bound. Pure session
    /// state, deliberately not an `Operation`: see `SessionDriver::expire_approval`.
    pub fn expire_approval(&mut self, id: &crate::codex::RequestId) -> Result<(), session::Error> {
        self.session.expire_approval(id)
    }
    pub fn prepare_approval(
        &mut self,
        response: crate::codex::approval::ApprovalResponse,
    ) -> Result<session::PreparedApproval, session::Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.prepare_approval(response);
        operation.finish(result)
    }
    /// Borrow the existing pinned control future without transferring this
    /// session or its original process owner. A started future drop still stops.
    pub async fn next_observed_or_control<F: std::future::Future + ?Sized>(
        &mut self,
        control: std::pin::Pin<&mut F>,
    ) -> Result<session::ControlUpdate<F::Output>, session::Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .next_observed_or_control(control)
            .await;
        operation.finish(result)
    }
    pub async fn send_prepared_approval(
        &mut self,
        prepared: &mut session::PreparedApproval,
    ) -> Result<session::PreparedUpdate, session::Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .send_prepared_approval(prepared)
            .await;
        operation.finish(result)
    }
    pub async fn initialize(&mut self) -> Result<(), session::Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.initialize().await;
        operation.finish(result)
    }
    pub(super) async fn qualification_read(
        &mut self,
        read: session::QualificationRead,
    ) -> Result<serde_json::Value, session::Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.qualification_read(read).await;
        operation.finish(result)
    }
    pub async fn start_thread(&mut self) -> Result<String, session::Error> {
        self.managed_source.retire();
        self.text_source.retire();
        let operation = Operation::new(self)?;
        let result = operation.runner.session.start_thread().await;
        operation.finish(result)
    }
    pub async fn start_turn(&mut self, input: String) -> Result<String, session::Error> {
        self.managed_source.retire();
        self.text_source.retire();
        let operation = Operation::new(self)?;
        let result = operation.runner.session.start_turn(input).await;
        operation.finish(result)
    }
    /// One thread and one plain-text turn on a separately qualified text process.
    pub async fn start_shared_text_thread(&mut self) -> Result<String, session::Error> {
        self.text_source.begin_thread()?;
        self.managed_source.retire();
        let operation = Operation::new(self)?;
        let result = operation.runner.session.start_thread().await;
        operation.finish(result)
    }
    /// Bind the actual admitted Matrix room, never the reservation's initial
    /// room or a model-supplied path. A shared process still owns only one turn.
    pub async fn start_shared_text_thread_for_room(
        &mut self,
        room: &str,
    ) -> Result<String, session::Error> {
        self.text_source.begin_thread()?;
        self.managed_source.retire();
        let operation = Operation::new(self)?;
        let result = async {
            let directory = super::room_workspace::open(
                Path::new(operation.runner.session.settings().cwd()),
                room,
            )?;
            operation.runner.session.bind_shared_workspace(directory)?;
            operation.runner.session.start_thread().await
        }
        .await;
        operation.finish(result)
    }
    /// File access is limited to the qualified tools mode and the bound room workspace.
    pub fn enable_shared_room_files(&mut self) -> Result<(), session::Error> {
        if !self.text_source.has_live_activity() || !self.shared_tools_enabled() { return Err(session::Error::State); }
        self.session.enable_room_files()
    }
    pub async fn respond_shared_room_file(&mut self, call: &session::RoomFileCall, content: String) -> Result<(), session::Error> {
        if !self.text_source.has_live_activity() { return Err(session::Error::State); }
        let operation = Operation::new(self)?;
        let result = operation.runner.session.respond_room_file(call, content).await;
        operation.finish(result)
    }
    /// The host has bound a read-only history reader to this admitted room.
    pub fn enable_shared_room_history(&mut self) -> Result<(), session::Error> {
        if !self.text_source.has_live_activity() {
            return Err(session::Error::State);
        }
        self.session.enable_room_history()
    }
    pub(super) fn shared_room_history_enabled(&self) -> bool {
        self.session.settings().room_history_enabled()
    }
    pub async fn respond_shared_room_history(
        &mut self,
        call: &session::RoomHistoryCall,
        content: String,
    ) -> Result<(), session::Error> {
        if !self.text_source.has_live_activity() {
            return Err(session::Error::State);
        }
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .respond_room_history(call, content)
            .await;
        operation.finish(result)
    }
    /// Before the thread starts: the host will show this run's questions to
    /// its room, so Codex offers the model its question tool. The turn must
    /// still opt in with `enable_shared_user_input`.
    pub fn offer_shared_user_input_tool(&mut self) -> Result<(), session::Error> {
        if !self.text_source.has_live_activity() {
            return Err(session::Error::State);
        }
        self.session.offer_user_input_tool()
    }
    /// The host shows this turn's questions to its room and answers each one
    /// (Codex `request_user_input`). Without it every question is answered
    /// with no answers, so the model decides on its own.
    pub fn enable_shared_user_input(&mut self) -> Result<(), session::Error> {
        if !self.text_source.has_live_activity() {
            return Err(session::Error::State);
        }
        self.session.enable_user_input()
    }
    /// One answer to one question this turn asked. `false`: the answer is
    /// moot (Codex withdrew the question, or its turn is over) and nothing was
    /// written. A moot answer never stops the process.
    pub async fn respond_shared_user_input(
        &mut self,
        response: session::UserInputResponse,
    ) -> Result<bool, session::Error> {
        if !self.text_source.has_live_activity() {
            return Err(session::Error::State);
        }
        if self.session.user_input_moot(&response)? {
            return Ok(false);
        }
        let operation = Operation::new(self)?;
        let result = operation.runner.session.respond_user_input(response).await;
        operation.finish(result)
    }
    pub async fn start_shared_text_turn(
        &mut self,
        input: String,
    ) -> Result<String, session::Error> {
        if input.is_empty() || input.len() > 4096 {
            return Err(session::Error::Policy);
        }
        self.text_source.begin_turn(&input)?;
        self.managed_source.retire();
        let operation = Operation::new(self)?;
        let result = operation.runner.session.start_turn(input).await;
        operation.finish(result)
    }
    /// Separately versioned bounded input. Plain request limits and immutable
    /// process policy remain unchanged; the caller must own exact call custody.
    pub async fn start_shared_context_turn(
        &mut self,
        input: String,
        expected_digest: &str,
    ) -> Result<String, session::Error> {
        use sha2::{Digest, Sha256};
        if !cfg!(feature = "shared-context")
            || input.is_empty()
            || input.len() > 64 * 1024
            || format!("{:x}", Sha256::digest(input.as_bytes())) != expected_digest
        {
            return Err(session::Error::Policy);
        }
        self.text_source.begin_turn(&input)?;
        self.managed_source.retire();
        let operation = Operation::new(self)?;
        let result = operation.runner.session.start_turn(input).await;
        operation.finish(result)
    }
    pub(super) fn renew_shared_transport(
        &mut self,
        until: tokio::time::Instant,
    ) -> Result<(), session::Error> {
        self.session.renew_shared_activity(until)
    }
    pub(super) fn attach_shared_transport_activity(
        &mut self,
        clock: std::sync::Arc<dyn Fn() -> Option<tokio::time::Instant> + Send + Sync>,
    ) -> Result<(), session::Error> {
        self.session.attach_shared_activity(clock)
    }
    pub async fn interrupt(&mut self) -> Result<InterruptDisposition, session::Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.interrupt().await;
        operation.finish(result)
    }
    pub fn run_control_receipts(&mut self) -> Vec<session::RunReceipt> {
        self.session.run_control_receipts()
    }
    pub fn run_controls_hold_answers(&self) -> bool {
        self.session.run_controls_hold_answers()
    }
    pub fn run_stop_accepted(&self) -> bool {
        self.session.run_stop_accepted()
    }
    pub fn user_input_waiting(&self, request: &session::UserInputRequest) -> bool {
        self.session.user_input_waiting(request)
    }
    pub fn room_history_admissible(&self, call: &session::RoomHistoryCall) -> bool {
        self.session.room_history_admissible(call)
    }
    pub fn room_file_admissible(&self, call: &session::RoomFileCall) -> bool {
        self.session.room_file_admissible(call)
    }
    pub async fn submit_run_control(
        &mut self,
        id: String,
        action: session::RunAction,
    ) -> Result<session::RunOutcome, session::Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .submit_run_control(id, action)
            .await;
        operation.finish(result)
    }
    pub async fn next_update(&mut self) -> Result<Update, session::Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.next_update().await;
        operation.finish(result)
    }
    pub async fn next_observed_update(
        &mut self,
    ) -> Result<(Update, session::Observation), session::Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.next_observed_update().await;
        operation.finish(result)
    }
}
impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.stop();
    }
}
fn observe_stop(owner: &mut SupervisedProcess) -> Cleanup {
    tracing::info!(pid = owner.id(), "owned runner stop requested");
    match owner.stop(STOP_TIMEOUT) {
        Ok(report) => {
            tracing::info!(
                pid = owner.id(),
                cause = ?report.cause,
                refusal = ?report.refusal,
                guardian_exit = ?report.guardian_exit,
                whole_tree_stopped = report.scope.whole_tree_stopped,
                "owned runner stop reported"
            );
            Cleanup::Observed(report)
        }
        Err(error) => {
            tracing::warn!(pid = owner.id(), kind = ?error.kind(), "owned runner cleanup unknown");
            Cleanup::Unknown { kind: error.kind() }
        }
    }
}
#[cfg(unix)]
pub(super) fn exit_identity(status: i32) -> Option<String> {
    use std::os::unix::process::ExitStatusExt;
    let status = std::process::ExitStatus::from_raw(status);
    if let Some(code) = status.code() {
        Some(format!("code:{code}"))
    } else {
        status.signal().map(|signal| format!("signal:{signal}"))
    }
}
#[cfg(not(unix))]
pub(super) fn exit_identity(_status: i32) -> Option<String> {
    None
}
struct Operation<'a> {
    runner: &'a mut OwnedSession,
    finished: bool,
}
impl<'a> Operation<'a> {
    fn new(runner: &'a mut OwnedSession) -> Result<Self, session::Error> {
        if runner.cleanup != Cleanup::Pending {
            return Err(session::Error::State);
        }
        Ok(Self {
            runner,
            finished: false,
        })
    }
    fn finish<T>(mut self, result: Result<T, session::Error>) -> Result<T, session::Error> {
        if result.is_err()
            || (self.runner.phase() == Phase::Ended && !self.runner.text_source.has_live_activity())
        {
            self.runner.stop();
        }
        self.finished = true;
        result
    }
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.runner.stop();
        }
    }
}

pub(super) fn stderr_tail(tail: &[u8], max: usize) -> String {
    let text = String::from_utf8_lossy(tail);
    let start = text.ceil_char_boundary(text.len().saturating_sub(max));
    text[start..]
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect()
}
