//! Claude uses the same retained process custody and activity lease as Codex.
//! This is a trusted-host adapter: callers must still retain/recheck the owner
//! credential namespace and bind the signed resource policy before dispatch.
use super::{
    OwnedClaudeSession, SharedTextActivityControl, SharedTextProcessObservation,
    SharedTextProcessSource, StartError,
};
use crate::claude::{
    self,
    account::{AccountBinding, AccountObservation},
    session::{Error, Limits, Phase},
};
use hagency_platform::Launch;
use std::{path::Path, sync::Arc, time::Instant};
pub(super) mod isolation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedClaudePolicy {
    Text,
    LiveWeb,
    Standard { web: bool },
    Yolo { web: bool },
}
impl SharedClaudePolicy {
    pub fn from_options(web: bool, mode: Option<&str>) -> Result<Self, Error> {
        match mode {
            None | Some("none") => Ok(if web { Self::LiveWeb } else { Self::Text }),
            Some("standard") => Ok(Self::Standard { web }),
            Some("yolo") => Ok(Self::Yolo { web }),
            _ => Err(Error::Configuration),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "claude_text_v1",
            Self::LiveWeb => "claude_text_live_web_v1",
            Self::Standard { web: false } => "claude_tools_standard_v1",
            Self::Standard { web: true } => "claude_tools_standard_web_v1",
            Self::Yolo { web: false } => "claude_tools_yolo_v1",
            Self::Yolo { web: true } => "claude_tools_yolo_web_v1",
        }
    }
    pub fn tools(self) -> bool {
        !matches!(self, Self::Text | Self::LiveWeb)
    }
    pub fn web(self) -> bool {
        matches!(
            self,
            Self::LiveWeb | Self::Standard { web: true } | Self::Yolo { web: true }
        )
    }
    /// Native permissions always use the original stdio callback. Yolo is an
    /// explicit owner's host policy, not a room member's bypass CLI flag.
    pub fn arguments(self, model: &str, effort: &str) -> Result<Vec<String>, claude::Error> {
        let mut args = claude::interactive_arguments(model, effort)?;
        // Shared work never inherits a person's hooks, instructions, plugins,
        // settings or connectors. The authenticated host installs the exact
        // room's SDK MCP connection after initialization.
        args.extend(
            [
                "--safe-mode",
                "--setting-sources=",
                "--strict-mcp-config",
                "--mcp-config",
                "{\"mcpServers\":{}}",
                "--no-chrome",
            ]
            .map(str::to_owned),
        );
        if !self.tools() {
            args.extend(
                [
                    "--tools",
                    if self.web() { "WebSearch,WebFetch" } else { "" },
                ]
                .map(str::to_owned),
            );
        } else if !self.web() {
            args.extend(["--disallowedTools", "WebSearch,WebFetch"].map(str::to_owned));
        }
        Ok(args)
    }
    pub fn arguments_resuming(
        self,
        model: &str,
        effort: &str,
        resume: Option<&str>,
    ) -> Result<Vec<String>, claude::Error> {
        let mut args = self.arguments(model, effort)?;
        if let Some(id) = resume {
            if id.is_empty()
                || id.starts_with('-')
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            {
                return Err(claude::Error::Input);
            }
            args.extend(["--resume".into(), id.into()]);
        }
        Ok(args)
    }
    fn matches_resume(self, launch: &Launch, resume: Option<&str>) -> bool {
        let args: Option<Vec<&str>> = launch.arguments.iter().map(|v| v.to_str()).collect();
        let Some(args) = args else {
            return false;
        };
        let Some(model) = args.iter().find_map(|arg| arg.strip_prefix("--model=")) else {
            return false;
        };
        let Some(effort) = args
            .windows(2)
            .find_map(|p| (p[0] == "--effort").then_some(p[1]))
        else {
            return false;
        };
        self.arguments_resuming(model, effort, resume)
            .is_ok_and(|expected| expected == args)
            && [
                "CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS",
                "CLAUDE_CODE_BG_TASKS_REPORT_RUNNING",
            ]
            .iter()
            .all(|key| {
                launch
                    .environment
                    .get(std::ffi::OsStr::new(key))
                    .is_some_and(|v| v == "1")
            })
            && launch
                .environment
                .get(std::ffi::OsStr::new("CLAUDE_CONFIG_DIR"))
                .is_some_and(|v| Path::new(v).is_absolute())
    }
}

pub(super) struct State {
    pub(super) source: SharedTextProcessSource,
    policy: SharedClaudePolicy,
    account: Option<AccountObservation>,
    resume: Option<String>,
}

impl OwnedClaudeSession {
    /// Shared credentials and plaintext stay in one host-owned conversation
    /// scope. The OS sandbox covers the CLI and every native tool descendant.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_shared_isolated_resuming(
        guardian: &Path,
        mut launch: Launch,
        scope: &Path,
        credential: &crate::claude::account::SharedCredential,
        limits: Limits,
        policy: SharedClaudePolicy,
        resume: Option<&str>,
    ) -> Result<Self, StartError> {
        if !policy.matches_resume(&launch, resume) {
            return Err(StartError::Settings);
        }
        credential
            .inject(&mut launch)
            .map_err(|_| StartError::Settings)?;
        isolation::confine(&mut launch, scope)?;
        let mut session = Self::start_shared(guardian, &launch, limits, policy, resume)?;
        session
            .bind_shared_credential(credential.binding())
            .map_err(|_| StartError::Settings)?;
        Ok(session)
    }
    pub fn spawn_shared(
        guardian: &Path,
        launch: &Launch,
        limits: Limits,
        policy: SharedClaudePolicy,
    ) -> Result<Self, StartError> {
        Self::spawn_shared_resuming(guardian, launch, limits, policy, None)
    }
    pub fn spawn_shared_resuming(
        guardian: &Path,
        launch: &Launch,
        limits: Limits,
        policy: SharedClaudePolicy,
        resume: Option<&str>,
    ) -> Result<Self, StartError> {
        if !policy.matches_resume(launch, resume) {
            return Err(StartError::Settings);
        }
        Self::start_shared(guardian, launch, limits, policy, resume)
    }
    fn start_shared(
        guardian: &Path,
        launch: &Launch,
        limits: Limits,
        policy: SharedClaudePolicy,
        resume: Option<&str>,
    ) -> Result<Self, StartError> {
        let mut session = Self::spawn(guardian, launch, limits)?;
        session.shared = Some(State {
            source: SharedTextProcessSource::new(),
            policy,
            account: None,
            resume: resume.map(str::to_owned),
        });
        session
            .enable_native_completion()
            .map_err(|_| StartError::Settings)?;
        session
            .enable_continuous_session()
            .map_err(|_| StartError::Settings)?;
        if let Some(id) = resume {
            session
                .expect_shared_resume(id)
                .map_err(|_| StartError::Settings)?;
        }
        Ok(session)
    }
    pub fn shared_resume_id(&self) -> Result<Option<&str>, Error> {
        Ok(self.shared.as_ref().ok_or(Error::State)?.resume.as_deref())
    }
    pub fn shared_process_source(&self) -> Result<SharedTextProcessSource, Error> {
        Ok(self.shared.as_ref().ok_or(Error::State)?.source.clone())
    }
    pub fn shared_policy(&self) -> Result<SharedClaudePolicy, Error> {
        Ok(self.shared.as_ref().ok_or(Error::State)?.policy)
    }
    pub fn qualify_shared(
        &mut self,
        binding: &AccountBinding,
    ) -> Result<SharedTextProcessObservation, Error> {
        let account = self.qualify_account(binding)?;
        if !self.matches_account_observation(&account) {
            return Err(Error::Identity);
        }
        let state = self.shared.as_mut().ok_or(Error::State)?;
        let observed = state
            .source
            .qualify_native(
                account.provider_digest(),
                state.policy.name(),
                state.policy.web(),
                state.policy.tools(),
            )
            .map_err(|_| Error::Identity)?;
        state.account = Some(account);
        Ok(observed)
    }
    pub fn install_shared_activity(
        &mut self,
        epoch: u64,
        until: Instant,
        maximum: Instant,
    ) -> Result<(), Error> {
        let source = self.shared_process_source()?;
        source
            .install_activity(epoch, until, maximum)
            .map_err(|_| Error::State)?;
        self.attach_shared_activity(Arc::new(move || source.current_deadline().map(Into::into)))
    }
    pub fn shared_activity_control(&self) -> Result<SharedTextActivityControl, Error> {
        self.shared_process_source()?
            .activity_control()
            .map_err(|_| Error::State)
    }
    pub fn install_renewable_shared_activity(
        &mut self,
        epoch: u64,
        until: Instant,
    ) -> Result<(), Error> {
        let source = self.shared_process_source()?;
        source
            .install_renewable_activity(epoch, until)
            .map_err(|_| Error::State)?;
        self.attach_shared_activity(Arc::new(move || source.current_deadline().map(Into::into)))
    }
    /// The exact qualified process and actual prompt write remain connected to
    /// capture even after the short-lived startup account proof has expired.
    /// Startup-proof expiry must not interrupt an already admitted model turn.
    pub fn shared_call_matches(
        &self,
        source: &SharedTextProcessSource,
        input_digest: &str,
    ) -> bool {
        self.shared.as_ref().is_some_and(|state| {
            state.source.same(source)
                && state.source.is_live()
                && state.source.matches_input_digest(input_digest)
                && state
                    .account
                    .as_ref()
                    .is_some_and(|account| self.matches_account_observation(account))
                && matches!(self.phase(), Phase::Running | Phase::ResultObserved)
        })
    }
    pub fn stop_shared_observation(&mut self) -> Result<SharedTextProcessObservation, Error> {
        let source = self.shared_process_source()?;
        Ok(source.stopped(self.stop()))
    }
}
