use super::{Cleanup, StartError};
use crate::octos::session::{
    ApprovalControlPolicy, ControlUpdate, Error, Event, HostTool, HostToolCall, Limits,
    Observation, ObservationSource, PermissionDecision, Permissions, Phase, PreparedApproval,
    PreparedUpdate, SessionDriver, StderrSnapshot, Termination, WriteProgress,
};
use hagency_platform::{Launch, SupervisedProcess};
#[cfg(windows)]
use hagency_platform::{WindowsPipe as Receiver, WindowsPipe as Sender};
use std::{io, path::{Path, PathBuf}, time::Duration};
#[cfg(unix)]
use tokio::net::unix::pipe::{Receiver, Sender};
mod shared;
mod shell;
pub use shared::SharedOctosPolicy;
pub use shell::SharedOctosShell;

struct SharedScope {
    profile: crate::octos::profile_binding::ProfileBinding,
    profile_guard: std::sync::Arc<crate::octos::profile_binding::RunningProfileGuard>,
    workspace: PathBuf,
    policy: SharedOctosPolicy,
    guardian: PathBuf,
    executable: PathBuf,
    pending_shells: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    opened: bool,
    tools_restricted: bool,
    activity_restricted: bool,
    source: super::SharedTextProcessSource,
    #[cfg(target_os = "macos")]
    _runtime_alias: shared::RuntimeAlias,
}

/// Host-internal custody of one `octos serve --stdio` (ADR-193), not a
/// launch-policy or production admission API. Launch is explicit and
/// env-cleared by the platform. The profile, workspace and permission profile
/// remain the execution Host's duty.
pub struct OwnedOctosSession {
    session: SessionDriver<Receiver, Sender, Receiver>,
    owner: SupervisedProcess,
    cleanup: Cleanup,
    shared_scope: Option<SharedScope>,
}
impl OwnedOctosSession {
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
                io::Error::new(io::ErrorKind::InvalidInput, "invalid Octos session limits")
            })
        })();
        match session {
            Ok(session) => Ok(Self {
                session,
                owner,
                cleanup: Cleanup::Pending,
                shared_scope: None,
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
    /// Lease restriction only. Pool admission and OS process isolation are
    /// independently required before running a shared process.
    pub fn restrict_activity(
        &mut self,
        activity: std::sync::Arc<dyn Fn() -> Option<tokio::time::Instant> + Send + Sync>,
    ) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        // An arbitrary clock closure is useful for personal sessions, but is
        // not a pool authority. Shared sessions use the custody-bound method.
        let result = if operation.runner.shared_scope.is_none() {
            operation.runner.session.restrict_activity(activity)
        } else { Err(Error::State) };
        operation.finish(result)
    }
    pub fn session_id(&self) -> Option<&str> {
        self.session.session_id()
    }
    pub fn dispatch_turn(&self) -> Option<&str> {
        self.session.dispatch_turn()
    }
    pub fn supports_active_only_steer(&self) -> bool {
        self.session.supports_active_only_steer()
    }
    pub fn supports_host_scoped_profile(&self) -> bool {
        self.session.supports_host_scoped_profile()
    }
    pub fn supports_host_scoped_workspace_io(&self) -> bool { self.session.supports_host_scoped_workspace_io() }
    pub fn supports_host_patch_worker(&self) -> bool { self.session.supports_host_patch_worker() }
    pub fn question_pending(&self, id: &str) -> bool {
        self.session.question_pending(id)
    }
    pub fn host_tool_pending(&self, id: &str) -> bool {
        self.session.host_tool_pending(id)
    }
    pub async fn register_shared_text_tools(&mut self, tools: &[HostTool]) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = if operation.runner.shared_scope.as_ref().is_none_or(|scope| scope.policy == SharedOctosPolicy::Text) {
            operation.runner.session.register_shared_text_tools(tools).await
        } else { Err(Error::Identity) };
        if result.is_ok() {
            if let Some(scope) = &mut operation.runner.shared_scope { scope.tools_restricted = true; }
        }
        operation.finish(result)
    }
    /// The process policy, fixed at creation and qualified under custody,
    /// determines the exact native registry; callers cannot widen it.
    pub async fn register_shared_tools(&mut self, tools: &[HostTool]) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = if let Some(scope) = &operation.runner.shared_scope {
            let mut generic = scope.policy.generic_tools();
            if scope.policy.tools() && operation.runner.session.supports_host_scoped_vision() {
                generic.push("view_image");
            }
            operation.runner.session.register_shared_policy_tools(tools, generic).await
        } else { Err(Error::State) };
        if result.is_ok() {
            if let Some(scope) = &mut operation.runner.shared_scope { scope.tools_restricted = true; }
        }
        operation.finish(result)
    }
    pub fn shared_policy(&self) -> Result<SharedOctosPolicy, Error> {
        self.shared_scope.as_ref().map(|scope| scope.policy).ok_or(Error::State)
    }
    pub fn shared_shell(&self) -> Result<SharedOctosShell, Error> {
        let scope = self.shared_scope.as_ref().ok_or(Error::State)?;
        if !scope.policy.tools() || !scope.activity_restricted || !scope.source.has_live_activity()
            || scope.profile_guard.check().is_err() { return Err(Error::State); }
        Ok(SharedOctosShell::new(scope.guardian.clone(), scope.workspace.clone(), scope.source.clone(),
            scope.profile_guard.clone(), scope.pending_shells.clone(), scope.executable.clone(),
            self.session.supports_host_patch_worker()))
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
    pub fn completion_receipt(&self) -> Option<&crate::octos::session::CompletionReceipt> {
        self.session.completion_receipt()
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
    /// The leader's exit as the guardian reaped it, `code:N` or `signal:N`.
    /// Evidence for the attempt record only (ADR-181), as for Codex.
    pub fn exit_identity(&self) -> Option<String> {
        let Cleanup::Observed(report) = self.cleanup else {
            return None;
        };
        super::session::exit_identity(report.leader_status?)
    }
    /// The last `max` bytes of the retained stderr tail, control characters
    /// other than newline replaced. Private diagnostic text.
    pub fn stderr_tail(&self, max: usize) -> String {
        super::session::stderr_tail(&self.session.stderr_snapshot().tail, max)
    }
    /// The guardian's own stderr tail as the platform collected it (ADR-181).
    pub fn guardian_stderr_tail(&self) -> String {
        self.owner.guardian_stderr_tail()
    }
    pub fn enable_approval_control(&mut self, policy: ApprovalControlPolicy) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.enable_approval_control(policy);
        operation.finish(result)
    }
    pub fn approval_deadline(&self, id: &str) -> Result<tokio::time::Instant, Error> {
        self.session.approval_deadline(id)
    }
    pub fn enable_owner_wait_expiry(&mut self) -> Result<(), Error> {
        self.session.enable_owner_wait_expiry()
    }
    pub fn expire_approval(&mut self, id: &str) -> Result<(), Error> {
        self.session.expire_approval(id)
    }
    pub fn prepared_admissible(&self, prepared: &PreparedApproval) -> bool {
        self.session.prepared_admissible(prepared)
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
    /// Idle is not stop: closing stdin ends the connection, and only the
    /// retained owner can supply physical stop evidence for the whole tree.
    pub fn stop(&mut self) -> Cleanup {
        if let Some(scope) = &self.shared_scope { scope.source.retire(); }
        self.session.close();
        if !matches!(self.cleanup,Cleanup::Observed(report) if report.scope.whole_tree_stopped) {
            self.cleanup = observe_stop(&mut self.owner);
        }
        self.cleanup
    }
    pub async fn hello(&mut self) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.hello().await.and_then(|()| {
            if operation.runner.shared_scope.as_ref().is_some_and(|scope|
                !operation.runner.session.supports_host_scoped_profile()
                    || scope.policy.tools() && !operation.runner.session.supports_host_scoped_workspace_io()
                    || scope.policy.tools() && !operation.runner.session.supports_host_scoped_search()
                    || scope.policy.web() && !operation.runner.session.supports_host_scoped_web()) {
                Err(Error::Identity)
            } else { Ok(()) }
        });
        operation.finish(result)
    }
    pub async fn open(
        &mut self,
        session_id: &str,
        profile_id: &str,
        cwd: &str,
        permissions: Permissions,
    ) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let allowed = operation.runner.shared_scope.as_ref().is_none_or(|scope| {
            !scope.opened && scope.profile.profile_id() == profile_id
                && scope.workspace.to_str() == Some(cwd)
                && permissions == scope.policy.permissions() && scope.profile_guard.check().is_ok()
                && scope.activity_restricted
        });
        let result = if allowed {
            if let Some(scope) = &operation.runner.shared_scope { scope.source.begin_thread().map_err(|_| Error::State)?; }
            operation.runner.session.open(session_id, profile_id, cwd, permissions).await
        } else { Err(Error::Identity) };
        if result.is_ok() {
            if let Some(scope) = &mut operation.runner.shared_scope { scope.opened = true; }
        }
        operation.finish(result)
    }
    pub async fn start_turn(&mut self, turn_id: &str, text: &str) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let allowed = operation.runner.shared_scope.as_ref().is_none_or(|scope| {
            scope.opened && scope.tools_restricted && scope.activity_restricted && scope.profile_guard.check().is_ok()
        });
        let result = if allowed {
            if let Some(scope) = &operation.runner.shared_scope { scope.source.begin_turn(text).map_err(|_| Error::State)?; }
            operation.runner.session.start_turn(turn_id, text).await
        }
            else { Err(Error::Identity) };
        operation.finish(result)
    }
    pub fn enable_live_text(&mut self, output: crate::live_text::LiveText) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.enable_live_text(output);
        operation.finish(result)
    }
    pub async fn answer_question(
        &mut self,
        id: &str,
        answers: &crate::questions::Answers,
    ) -> Result<bool, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.answer_question(id, answers).await;
        operation.finish(result)
    }
    pub async fn steer(&mut self, text: &str) -> Result<bool, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.steer(text).await;
        operation.finish(result)
    }
    pub async fn interrupt(&mut self) -> Result<bool, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.interrupt().await;
        operation.finish(result)
    }
    pub async fn register_tools(&mut self, tools: &[HostTool]) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = if operation.runner.shared_scope.is_none() {
            operation.runner.session.register_tools(tools).await
        } else { Err(Error::Identity) };
        operation.finish(result)
    }
    pub fn host_tool_call(&self, params: &serde_json::Value) -> Result<HostToolCall, Error> {
        self.session.host_tool_call(params)
    }
    pub async fn host_tool_result(
        &mut self,
        call_id: &str,
        result: Result<serde_json::Value, String>,
    ) -> Result<bool, Error> {
        let operation = Operation::new(self)?;
        let result = operation
            .runner
            .session
            .host_tool_result(call_id, result)
            .await;
        operation.finish(result)
    }
    pub async fn next(&mut self) -> Result<Event, Error> {
        let operation = Operation::new(self)?;
        let result = operation.runner.session.next().await;
        operation.finish(result)
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
impl Drop for OwnedOctosSession {
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
    runner: &'a mut OwnedOctosSession,
    finished: bool,
}
impl<'a> Operation<'a> {
    fn new(runner: &'a mut OwnedOctosSession) -> Result<Self, Error> {
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
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.runner.stop();
        }
    }
}
