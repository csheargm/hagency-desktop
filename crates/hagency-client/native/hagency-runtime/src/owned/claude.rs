use super::{Cleanup, StartError};
use crate::claude::session::{
    ApprovalControlPolicy, ControlUpdate, PermissionDecision, PreparedApproval, PreparedUpdate,
};
use crate::claude::session::{InterruptReceipt, InterruptUpdate, PreparedInterrupt};
use crate::claude::session::{Observation, ObservationSource, PreparedRoomReply, RoomToolRequest};
use crate::claude::{
    Message,
    session::{Error, Limits, Phase, SessionDriver, StderrSnapshot, Termination, WriteProgress},
};
use hagency_platform::{Launch, SupervisedProcess};
#[cfg(windows)]
use hagency_platform::{WindowsPipe as Receiver, WindowsPipe as Sender};
use std::{io, path::Path, time::Duration};
#[cfg(unix)]
use tokio::net::unix::pipe::{Receiver, Sender};

/// Host-internal process custody, not a launch-policy or production admission
/// API. Launch is explicit and env-cleared by the platform. Effective runner
/// settings, sandbox, credentials and MCP remain the execution Host's duty.
pub struct OwnedClaudeSession {
    session: SessionDriver<Receiver, Sender, Receiver>,
    owner: SupervisedProcess,
    cleanup: Cleanup,
    pub(super) shared: Option<super::claude_shared::State>,
}
impl OwnedClaudeSession {
    /// Use on an IO-enabled execution worker: guardian startup/stop is bounded
    /// synchronous work. Never detach this owner into an HTTP request task.
    pub fn spawn(guardian: &Path, launch: &Launch, limits: Limits) -> Result<Self, StartError> {
        if limits.validate().is_err() || tokio::runtime::Handle::try_current().is_err() {
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
            SessionDriver::new(stdout, stdin, stderr, limits).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid Claude session limits")
            })
        })();
        match session {
            Ok(session) => Ok(Self {
                session,
                owner,
                cleanup: Cleanup::Pending,
                shared: None,
            }),
            Err(error) => Err(StartError::Uncertain {
                kind: error.kind(),
                cleanup: observe_stop(&mut owner),
            }),
        }
    }
    pub fn id(&self) -> u32 {
        self.owner.id()
    }
    pub fn phase(&self) -> Phase {
        self.session.phase()
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session.session_id()
    }
    pub fn expect_resume(&mut self, id: &str) -> Result<(), Error> {
        self.session.expect_resume(id)
    }
    /// Original pre-input refusal plus observed physical cleanup, never an EOF guess.
    pub fn missing_resume_id(&self) -> Option<&str> {
        if self.termination().is_some_and(|t| t.cause == Error::MissingResume)
            && matches!(self.cleanup, Cleanup::Observed(r) if r.scope.whole_tree_stopped) {
            self.session_id()
        } else { None }
    }
    pub(super) fn expect_shared_resume(&mut self, id: &str) -> Result<(), Error> {
        self.session.expect_resume(id)
    }
    /// Corroborate a host-authenticated account binding with this process's
    /// native initialization. The host must additionally retain and recheck
    /// the original selected credential namespace and launch policy. This
    /// receipt itself grants no task, capacity or provider-login authority.
    pub fn qualify_account(
        &self,
        binding: &crate::claude::account::AccountBinding,
    ) -> Result<crate::claude::account::AccountObservation, Error> {
        if self.cleanup != Cleanup::Pending {
            return Err(Error::Closed);
        }
        self.session.qualify_account(binding)
    }
    pub fn matches_account_observation(
        &self,
        receipt: &crate::claude::account::AccountObservation,
    ) -> bool {
        self.cleanup == Cleanup::Pending && self.session.matches_account_observation(receipt)
    }
    pub fn observation_source(&self) -> Result<ObservationSource, Error> {
        self.session.observation_source()
    }
    pub fn matches_observation_source(&self, source: &ObservationSource) -> bool {
        self.session.matches_observation_source(source)
    }
    pub fn last_observation(&self) -> Option<&Observation> {
        self.session.last_observation()
    }
    pub(super) fn attach_shared_activity(
        &mut self,
        activity: std::sync::Arc<dyn Fn() -> Option<tokio::time::Instant> + Send + Sync>,
    ) -> Result<(), Error> {
        self.session.attach_shared_activity(activity)
    }
    pub fn cleanup(&self) -> Cleanup {
        self.cleanup
    }
    pub fn termination(&self) -> Option<&Termination> {
        self.session.termination()
    }
    pub fn write_progress(&self) -> Option<WriteProgress> {
        self.session.write_progress()
    }
    pub fn stderr_snapshot(&self) -> StderrSnapshot {
        self.session.stderr_snapshot()
    }
    pub fn enable_native_completion(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.enable_native_completion();
        operation.finish(result)
    }
    pub fn supervise_turn(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.supervise_turn();
        operation.finish(result)
    }
    pub fn needs_usage_snapshot(&self) -> bool {
        self.session.needs_usage_snapshot()
    }
    pub fn enable_continuous_session(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.enable_continuous_session();
        operation.finish(result)
    }
    pub fn can_followup(&self) -> bool {
        self.session.can_followup()
    }
    pub fn begin_followup(&mut self, limits: Limits) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.begin_followup(limits);
        operation.finish(result)
    }
    pub async fn prepare_followup(&mut self, limits: Limits) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.prepare_followup(limits).await;
        operation.finish(result)
    }
    pub fn usage_snapshot_requested(&self) -> bool {
        self.session.usage_snapshot_requested()
    }
    pub async fn request_usage_snapshot(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.request_usage_snapshot().await;
        operation.finish(result)
    }
    pub fn enable_approval_control(&mut self, policy: ApprovalControlPolicy) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.enable_approval_control(policy);
        operation.finish(result)
    }
    pub fn approval_deadline(&self, id: &str) -> Result<tokio::time::Instant, Error> {
        self.session.approval_deadline(id)
    }
    pub fn set_question_wait(&mut self, wait_ms: u64) -> Result<(), Error> {
        self.session.set_question_wait(wait_ms)
    }
    pub fn question(&self, id: &str) -> Result<Vec<crate::questions::Question>, Error> {
        self.session.question(id)
    }
    pub fn prepare_question(
        &mut self,
        id: &str,
        answers: &crate::questions::Answers,
    ) -> Result<PreparedApproval, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.prepare_question(id, answers);
        operation.finish(result)
    }
    pub fn prepare_approval(
        &mut self,
        id: &str,
        decision: PermissionDecision,
    ) -> Result<PreparedApproval, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.prepare_approval(id, decision);
        operation.finish(result)
    }
    /// ResultObserved is not automatic stop: background activity may survive a
    /// Claude result. Only the retained owner can supply physical stop evidence.
    pub fn stop(&mut self) -> Cleanup {
        if let Some(shared) = &self.shared {
            shared.source.retire();
        }
        self.session.close();
        if !matches!(self.cleanup,Cleanup::Observed(report) if report.scope.whole_tree_stopped) {
            self.cleanup = observe_stop(&mut self.owner);
        }
        self.cleanup
    }
    pub async fn initialize(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.initialize().await;
        operation.finish(result)
    }
    pub async fn prompt(&mut self, text: &str) -> Result<WriteProgress, Error> {
        let operation = Operation::new(self)?;
        let result = async {
            if let Some(shared) = &operation.runner.shared {
                shared.source.begin_thread().map_err(|_| Error::State)?;
                shared.source.begin_turn(text).map_err(|_| Error::State)?;
            }
            operation.runner.session.prompt(text).await
        }
        .await;
        operation.finish(result)
    }
    pub async fn bind_task_mcp(&mut self, helper: crate::claude::TaskMcp) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.bind_task_mcp(helper).await;
        operation.finish(result)
    }
    pub async fn bind_room_tools(
        &mut self,
        tools: crate::room_tools::RoomTools,
    ) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.bind_room_tools(tools).await;
        operation.finish(result)
    }
    pub fn enable_room_tool_callbacks(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.enable_room_tool_callbacks();
        operation.finish(result)
    }
    pub fn room_tool_request(&self, id: &str) -> Result<RoomToolRequest, Error> {
        self.session.room_tool_request(id)
    }
    pub fn prepare_room_tool_reply(
        &mut self,
        request: &RoomToolRequest,
        text: &str,
        is_error: bool,
    ) -> Result<PreparedRoomReply, Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .prepare_room_tool_reply(request, text, is_error);
        operation.finish(result)
    }
    pub fn prepare_room_metadata_reply(
        &mut self,
        request: &RoomToolRequest,
    ) -> Result<PreparedRoomReply, Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .prepare_room_metadata_reply(request);
        operation.finish(result)
    }
    pub async fn send_prepared_room_reply(
        &mut self,
        prepared: &mut PreparedRoomReply,
    ) -> Result<PreparedUpdate, Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .send_prepared_room_reply(prepared)
            .await;
        operation.finish(result)
    }
    pub async fn next_message(&mut self) -> Result<Message, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.next_message().await;
        operation.finish(result)
    }
    pub fn prepare_interrupt(&mut self) -> Result<PreparedInterrupt, Error> {
        if self.cleanup != Cleanup::Pending {
            return Err(Error::Closed);
        }
        // A stale host action must not kill an unrelated current turn. This
        // synchronous preflight neither writes nor cancels a native operation.
        self.session.prepare_interrupt()
    }
    pub fn interrupt_receipt(&self) -> Option<&InterruptReceipt> {
        self.session.interrupt_receipt()
    }
    pub fn is_interrupting(&self) -> bool {
        self.session.is_interrupting()
    }
    pub async fn send_prepared_interrupt(
        &mut self,
        prepared: &mut PreparedInterrupt,
    ) -> Result<InterruptUpdate, Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .send_prepared_interrupt(prepared)
            .await;
        operation.finish_interrupt(result)
    }
    pub async fn next_or_control<F: std::future::Future + ?Sized>(
        &mut self,
        control: std::pin::Pin<&mut F>,
    ) -> Result<ControlUpdate<F::Output>, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.next_or_control(control).await;
        operation.finish(result)
    }
    pub async fn send_prepared_approval(
        &mut self,
        prepared: &mut PreparedApproval,
    ) -> Result<PreparedUpdate, Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .send_prepared_approval(prepared)
            .await;
        operation.finish(result)
    }
}
impl Drop for OwnedClaudeSession {
    fn drop(&mut self) {
        self.stop();
    }
}
fn observe_stop(owner: &mut SupervisedProcess) -> Cleanup {
    match owner.stop(Duration::from_secs(3)) {
        Ok(report) => Cleanup::Observed(report),
        Err(error) => Cleanup::Unknown { kind: error.kind() },
    }
}
struct Operation<'a> {
    runner: &'a mut OwnedClaudeSession,
    finished: bool,
}
impl<'a> Operation<'a> {
    fn new(runner: &'a mut OwnedClaudeSession) -> Result<Self, Error> {
        if runner.cleanup != Cleanup::Pending {
            return Err(Error::Closed);
        }
        Ok(Self {
            runner,
            finished: false,
        })
    }
    fn finish<T>(mut self, result: Result<T, Error>) -> Result<T, Error> {
        if result.is_err() {
            self.runner.stop();
        }
        self.finished = true;
        result
    }
    fn finish_interrupt<T>(mut self, result: Result<T, Error>) -> Result<T, Error> {
        // The driver closes on protocol/IO errors and uncertain writes, but
        // harmless rejected preparations leave its active stream untouched.
        if self.runner.session.phase() == Phase::Closed {
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
