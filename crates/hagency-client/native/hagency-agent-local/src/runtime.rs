//! Desktop runtime bridge. Native sessions may span many independently admitted
//! calls. Accounts remain provider-owned; each call has its own ledger and approvals.
use crate::codex::{self, ApprovalQueue, BudgetMode, Completed, Error, Profile, Result, TurnStatus};
use crate::{Ledger, Reservation, Scope, ToolPolicy, ToolProposal, Usage};
use hagency_runtime::{claude, octos};
use serde_json::Value;
use std::{
    path::Path,
    time::{Duration, Instant},
};
mod claude_controls;
pub mod claude_turn;
pub mod octos_turn;
#[cfg(unix)]
mod octos_files;

type Claude = hagency_runtime::owned::OwnedClaudeSession;
type Octos = hagency_runtime::owned::OwnedOctosSession;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Codex,
    Claude,
    Octos,
}
impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Octos => "octos",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "claude" => Some(Self::Claude),
            "octos" => Some(Self::Octos),
            _ => None,
        }
    }
}
/// Strict metadata grammar, never a credential resolver or login proof.
pub fn managed_reference(reference: &str) -> Option<(Kind, Option<&str>)> {
    for (prefix, kind) in [
        ("claude-managed:shared-home:", Kind::Claude),
        ("octos-managed:shared-home:", Kind::Octos),
    ] {
        if let Some(rest) = reference.strip_prefix(prefix) {
            let (hash, profile) = rest
                .split_once(':')
                .map(|(h, p)| (h, Some(p)))
                .unwrap_or((rest, None));
            if hash.len() != 64
                || !hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return None;
            }
            return match (kind, profile) {
                (Kind::Claude, None) => Some((kind, None)),
                (Kind::Octos, Some(p)) if octos::profile_id(p) => Some((kind, Some(p))),
                _ => None,
            };
        }
    }
    None
}
pub fn local_reference(
    kind: Kind,
    home: &Path,
    profile: Option<(&str, &octos::ProfileModel)>,
) -> String {
    use sha2::{Digest, Sha256};
    let digest = if let Some((id, primary)) = profile {
        Sha256::digest(
            serde_json::to_vec(&(home.to_string_lossy(), id, &primary.family, &primary.model))
                .expect("public strings"),
        )
    } else {
        Sha256::digest(home.to_string_lossy().as_bytes())
    };
    let mut reference = format!("{}-managed:shared-home:{digest:x}", kind.name());
    if let Some((id, _)) = profile {
        reference.push(':');
        reference.push_str(id);
    }
    reference
}
pub fn kind(reference: &str) -> Kind {
    managed_reference(reference)
        .map(|v| v.0)
        .unwrap_or(Kind::Codex)
}
pub fn validate(profile: &Profile, reference: &str) -> Result<()> {
    if kind(reference) == Kind::Codex {
        return profile.validate();
    }
    for path in [
        &profile.executable,
        &profile.home,
        &profile.codex_home,
        &profile.cwd,
    ] {
        if !path.is_absolute() || path.canonicalize().ok().as_ref() != Some(path) {
            return Err(Error::Profile("canonical absolute paths required"));
        }
    }
    if !profile.executable.is_file()
        || !profile.home.is_dir()
        || !profile.codex_home.is_dir()
        || !profile.cwd.is_dir()
        || profile.codex_home.starts_with(&profile.cwd)
        || profile.home.starts_with(&profile.cwd)
        || !profile.shared_auth
    {
        return Err(Error::Profile("local provider directory required"));
    }
    crate::key(&profile.model)?;
    match kind(reference) {
        Kind::Claude
            if local_reference(Kind::Claude, &profile.codex_home, None) != reference
                || !matches!(
                    profile.effort.as_str(),
                    "low" | "medium" | "high" | "xhigh" | "max"
                ) =>
        {
            return Err(Error::Profile("Claude account or effort mismatch"));
        }
        Kind::Octos if profile.effort != "none" => {
            return Err(Error::Profile("Octos requires profile reasoning settings"));
        }
        _ => {}
    }
    if kind(reference) == Kind::Octos {
        let (_, Some(id)) =
            managed_reference(reference).ok_or(Error::Profile("Octos profile required"))?
        else {
            return Err(Error::Profile("Octos profile required"));
        };
        let file = profile
            .codex_home
            .join("profiles")
            .join(format!("{id}.json"));
        if std::fs::symlink_metadata(&file)?.file_type().is_symlink() {
            return Err(Error::Profile("Octos profile must be local"));
        }
        let mut bytes = Vec::new();
        use std::io::Read;
        std::fs::File::open(file)?
            .take(octos::MAX_PROFILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        let model =
            octos::profile_model(id, &bytes).ok_or(Error::Profile("Octos profile unavailable"))?;
        if model.model != profile.model
            || local_reference(Kind::Octos, &profile.codex_home, Some((id, &model))) != reference
        {
            return Err(Error::Profile("Octos profile changed"));
        }
    }
    Ok(())
}

pub struct Process {
    pub session: Session,
    /// Set when this start retired a saved native session, with the stable
    /// reason code. The host records it; it changes nothing else.
    pub discarded_session: Option<&'static str>,
}
/// The native CLI reported, before any input, that the saved context is gone.
pub const SESSION_DISCARDED_MISSING: &str = "native_context_missing";
/// The resumed start failed before any input `claude::resume_failures::LIMIT`
/// times in a row without that report, and a fresh process then initialized.
pub const SESSION_DISCARDED_RESUME_FAILED: &str = "native_resume_failed_repeatedly";
impl Process {
    pub async fn stop(&mut self) -> Result<()> {
        match &mut self.session {
            Session::Codex(wire) => require_stopped(wire.stop())?,
            Session::Claude { wire, .. } => require_stopped(wire.stop())?,
            Session::Octos { wire, .. } => require_stopped(wire.stop())?,
        }
        Ok(())
    }
}
fn require_stopped(cleanup: hagency_runtime::owned::Cleanup) -> Result<()> {
    if matches!(cleanup,hagency_runtime::owned::Cleanup::Observed(report) if report.scope.whole_tree_stopped)
    {
        Ok(())
    } else {
        Err(Error::Unknown)
    }
}
pub enum Session {
    Codex(Box<codex::Owned>),
    Claude {
        wire: Box<Claude>,
        profile: Profile,
        scope: Option<Scope>,
        context: String,
        reusable: bool,
    },
    Octos {
        wire: Box<Octos>,
        profile: Profile,
        id: String,
        scope: Option<Scope>,
        context: String,
        room: Option<hagency_runtime::room_tools::RoomTools>,
        files: Option<OctosHostFiles>,
    },
}

/// Owner-selected workspace capability, bound to this Octos process only.
pub struct OctosHostFiles {
    #[cfg(unix)]
    workspace: crate::room_files::Workspace,
    #[cfg(unix)]
    gate: std::sync::Arc<dyn codex::HostToolGate>,
}

pub async fn spawn(profile: &Profile, reference: &str) -> Result<Process> {
    spawn_inner(profile, reference, None, None).await
}
pub async fn spawn_with_guardian(
    profile: &Profile,
    reference: &str,
    guardian: Option<&Path>,
) -> Result<Process> {
    spawn_inner(profile, reference, None, guardian).await
}
fn context_profile(profile: &Profile, reference: &str) -> Result<String> {
    crate::hash(&(reference, &profile.model, &profile.effort, &profile.cwd)).map_err(Into::into)
}
pub async fn spawn_for_scope(
    profile: &Profile,
    reference: &str,
    ledger: &mut Ledger,
    scope: &Scope,
) -> Result<Process> {
    spawn_for_scope_with_guardian(profile, reference, ledger, scope, None).await
}
/// Explicit host custody executable; no credentials or runtime authority.
pub async fn spawn_for_scope_with_guardian(
    profile: &Profile,
    reference: &str,
    ledger: &mut Ledger,
    scope: &Scope,
    guardian: Option<&Path>,
) -> Result<Process> {
    use rusqlite::OptionalExtension;
    crate::verify(&ledger.db, scope)?;
    let context = scope.context_key(&ledger.owner)?;
    let profile_key = context_profile(profile, reference)?;
    let session: Option<String> = ledger
        .db
        .query_row(
            "SELECT session FROM external_sessions WHERE scope=? AND profile=?",
            (&context, &profile_key),
            |r| r.get(0),
        )
        .optional()
        .map_err(crate::Error::from)?;
    let mut process = spawn_inner(profile, reference, session.as_deref(), guardian).await?;
    if let (Some(saved), Session::Claude { wire, .. }) = (session.as_deref(), &mut process.session)
    {
        wire.enable_native_completion()
            .map_err(|_| Error::Protocol("Claude lifecycle"))?;
        wire.enable_continuous_session()
            .map_err(|_| Error::Protocol("Claude continuous session"))?;
        wire.expect_resume(saved)
            .map_err(|_| Error::Protocol("Claude resume identity"))?;
        // Failed resumed starts are counted per saved row, for this host
        // process only.
        let counted = format!("personal\n{context}\n{profile_key}\n{saved}");
        match wire.initialize().await {
            Ok(()) => claude::resume_failures::clear(&counted),
            // The installation, not the saved session: a fresh start meets the
            // same thing. Keep the row; this is never counted.
            Err(error) if error == claude::session::Error::UnsupportedVersion => {
                return Err(claude_resume_error(error));
            }
            // The native CLI's own report that this context does not exist is
            // the only single failure that retires the saved row.
            Err(claude::session::Error::MissingResume) => {
                if wire.pre_prompt_resume_id() != Some(saved) {
                    return Err(Error::Protocol("Claude initialize"));
                }
                retire_saved_session(ledger, scope, &context, &profile_key, saved)?;
                claude::resume_failures::clear(&counted);
                process = spawn_inner(profile, reference, None, guardian).await?;
                process.discarded_session = Some(SESSION_DISCARDED_MISSING);
            }
            // A timeout, a refusal, an exit or an unexpected frame before the
            // prompt says nothing about the saved context: keep the row and
            // return the failure, so that the next message resumes it. Only a
            // session that keeps failing this way, and whose failed process is
            // proven stopped before any prompt byte, is replaced below.
            Err(error) => {
                if claude::resume_failures::record(&counted) < claude::resume_failures::LIMIT
                    || wire.failed_before_prompt_resume_id() != Some(saved)
                {
                    return Err(claude_resume_error(error));
                }
                // The saved row is retired only once a fresh process has
                // initialized. If that fails too, the row and the count stay
                // and the next message tries again.
                let mut fresh = spawn_inner(profile, reference, None, guardian).await?;
                let retired = match fresh.session.initialize().await {
                    Ok(()) => retire_saved_session(ledger, scope, &context, &profile_key, saved),
                    Err(error) => Err(error),
                };
                if let Err(error) = retired {
                    let _ = fresh.stop().await;
                    return Err(error);
                }
                claude::resume_failures::clear(&counted);
                fresh.discarded_session = Some(SESSION_DISCARDED_RESUME_FAILED);
                process = fresh;
            }
        }
    }
    Ok(process)
}
/// Remove one saved context pointer. Preserve every original usage,
/// reservation and uncertain call; verify authority again after IO.
fn retire_saved_session(
    ledger: &mut Ledger,
    scope: &Scope,
    context: &str,
    profile_key: &str,
    saved: &str,
) -> Result<()> {
    crate::verify(&ledger.db, scope)?;
    let removed = ledger
        .db
        .execute(
            "DELETE FROM external_sessions WHERE scope=? AND profile=? AND session=?",
            (context, profile_key, saved),
        )
        .map_err(crate::Error::from)?;
    if removed != 1 {
        return Err(Error::Protocol("Claude context changed"));
    }
    Ok(())
}
async fn spawn_inner(
    profile: &Profile,
    reference: &str,
    resume: Option<&str>,
    guardian: Option<&Path>,
) -> Result<Process> {
    validate(profile, reference)?;
    let kind = kind(reference);
    if kind == Kind::Codex {
        let guardian = match guardian {
            Some(path) => path.to_path_buf(),
            None => hagency_platform::helper_executable()?,
        };
        return Ok(Process {
            session: Session::Codex(Box::new(codex::Owned::spawn(profile, &guardian)?)),
            discarded_session: None,
        });
    }
    let mut command = tokio::process::Command::new(&profile.executable);
    command
        .env_clear()
        .env("HOME", &profile.home)
        .current_dir(&profile.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    // Native local login and provider helpers require only this allowlist.
    for name in ["PATH", "USER", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    match kind {
        Kind::Claude => {
            command
                .env("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1")
                .env("CLAUDE_CODE_BG_TASKS_REPORT_RUNNING", "1");
            if let Some(session) = resume {
                crate::key(session)?;
                command.args(["--resume", session]);
            }
            if !claude_uses_default_config(&profile.home, &profile.codex_home) {
                command.env("CLAUDE_CONFIG_DIR", &profile.codex_home);
            }
            command.args(
                claude::interactive_arguments(&profile.model, &profile.effort)
                    .map_err(|_| Error::Profile("Claude model or effort"))?,
            );
        }
        Kind::Octos => {
            let digest = crate::hash(&(reference, &profile.cwd))?;
            let instance = profile
                .cwd
                .parent()
                .ok_or(Error::Profile("instance parent"))?
                .join(".hagency-octos-instances")
                .join(digest);
            std::fs::create_dir_all(&instance)?;
            if instance.canonicalize()? != instance {
                return Err(Error::Profile("Octos instance symlink"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&instance, std::fs::Permissions::from_mode(0o700))?;
            }
            let config = instance.join("config.json");
            // Refuse links and don't read/write any provider-owned config.
            if std::fs::symlink_metadata(&config).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(Error::Profile("Octos settings symlink"));
            }
            use std::io::{Read, Write};
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32);
            }
            let mut file = options.open(&config)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if file.metadata()?.nlink() != 1 {
                    return Err(Error::Profile("Octos settings hardlink"));
                }
            }
            let mut existing = Vec::new();
            (&mut file).take(8193).read_to_end(&mut existing)?;
            if existing.is_empty() {
                file.write_all(octos::CONFIG)?;
                file.sync_all()?;
            } else if existing != octos::CONFIG {
                return Err(Error::Profile("Octos instance settings changed"));
            }
            command.env("OCTOS_HOME", &profile.codex_home).args(
                octos::serve_arguments_scoped(
                    profile
                        .cwd
                        .to_str()
                        .ok_or(Error::Profile("workspace encoding"))?,
                    instance
                        .to_str()
                        .ok_or(Error::Profile("instance encoding"))?,
                    config.to_str().ok_or(Error::Profile("config encoding"))?,
                    managed_reference(reference).and_then(|v| v.1)
                        .ok_or(Error::Profile("Octos profile required"))?,
                )
                .map_err(|_| Error::Profile("Octos settings"))?,
            );
        }
        Kind::Codex => unreachable!(),
    }
    let guardian = match guardian {
        Some(path) => path.to_path_buf(),
        None => hagency_platform::helper_executable()?,
    };
    let launch = hagency_platform::Launch {
        executable: profile.executable.clone(),
        arguments: command
            .as_std()
            .get_args()
            .map(|v| v.to_os_string())
            .collect(),
        directory: profile.cwd.clone(),
        environment: command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| (key.to_os_string(), value.to_os_string()))
            })
            .collect(),
        require_crash_containment: false,
    };
    let session = match kind {
        Kind::Claude => Session::Claude {
            wire: Box::new(
                Claude::spawn(&guardian, &launch, claude::session::Limits::default())
                    .map_err(|_| Error::Profile("Claude process custody unavailable"))?,
            ),
            profile: profile.clone(),
            scope: None,
            context: context_profile(profile, reference)?,
            reusable: false,
        },
        Kind::Octos => Session::Octos {
            wire: Box::new(
                Octos::spawn(
                    &guardian,
                    &launch,
                    octos::session::Limits {
                        event_wait_ms: 1_200_000,
                        ..Default::default()
                    },
                )
                .map_err(|_| Error::Profile("Octos process custody unavailable"))?,
            ),
            profile: profile.clone(),
            id: managed_reference(reference)
                .and_then(|v| v.1)
                .unwrap()
                .into(),
            scope: None,
            context: context_profile(profile, reference)?,
            room: None,
            files: None,
        },
        Kind::Codex => unreachable!(),
    };
    Ok(Process {
        session,
        discarded_session: None,
    })
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}
struct Charge<'a> {
    ledger: &'a mut Ledger,
    scope: Scope,
    call: String,
    settled: bool,
}
impl Drop for Charge<'_> {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.ledger.mark_unknown(&self.scope, &self.call);
        }
    }
}
impl Session {
    pub async fn initialize(&mut self) -> Result<()> {
        match self {
            Self::Codex(session) => {
                session.initialize().await?;
            }
            Self::Claude { wire, .. } => {
                if wire.phase() == claude::session::Phase::Ready {
                    return Ok(());
                }
                wire.enable_native_completion()
                    .map_err(|_| Error::Protocol("Claude lifecycle"))?;
                wire.enable_continuous_session()
                    .map_err(|_| Error::Protocol("Claude continuous session"))?;
                wire.initialize().await.map_err(claude_initialize_error)?;
            }
            Self::Octos { wire, .. } => {
                wire.hello().await.map_err(|_| Error::Protocol("Octos hello"))?;
                if !wire.supports_host_scoped_profile() {
                    return Err(Error::Protocol("Octos scoped profile unsupported"));
                }
            }
        }
        Ok(())
    }
    pub async fn verify_host_environment(&mut self) -> Result<()> {
        if let Self::Codex(session) = self {
            session.verify_host_environment().await?;
        }
        Ok(())
    }
    pub async fn require_local_account(&mut self) -> Result<()> {
        match self {
            Self::Codex(session) => session.require_local_account().await?,
            Self::Claude { profile, .. } => {
                if !claude_account(&profile.executable, &profile.home, &profile.codex_home).await? {
                    return Err(Error::Profile("dedicated owner provider login required"));
                }
            }
            Self::Octos { .. } => {}
        }
        Ok(())
    }
    /// Bind once after initialize, before opening/running the native context.
    /// Every call's host authority is supplied separately by ApprovalQueue.
    pub async fn enable_room_tools(
        &mut self,
        tools: hagency_runtime::room_tools::RoomTools,
    ) -> Result<()> {
        match self {
            Self::Codex(session) => session.enable_room_tools(tools).await,
            Self::Claude { wire, .. } => wire
                .bind_room_tools(tools)
                .await
                .map_err(|_| Error::Protocol("Claude room tools startup")),
            Self::Octos { room, .. } if room.is_none() => {
                *room = Some(tools);
                Ok(())
            }
            Self::Octos { .. } => Err(Error::Profile("room tools already bound")),
        }
    }
    #[cfg(unix)]
    pub fn enable_host_files(
        &mut self,
        workspace: crate::room_files::Workspace,
        gate: std::sync::Arc<dyn codex::HostToolGate>,
    ) -> Result<()> {
        match self {
            Self::Codex(session) => session.enable_host_files(workspace, gate),
            Self::Octos { profile, scope, files, .. }
                if scope.is_none() && files.is_none()
                    && profile.cwd == workspace.canonical_directory() => {
                *files = Some(OctosHostFiles { workspace, gate });
                Ok(())
            }
            _ => Err(Error::Profile("host files not released for this runtime")),
        }
    }
    #[cfg(unix)]
    pub fn rebind_host_files(
        &mut self,
        workspace: crate::room_files::Workspace,
        gate: std::sync::Arc<dyn codex::HostToolGate>,
    ) -> Result<()> {
        match self {
            Self::Codex(session) => session.rebind_host_files(workspace, gate),
            _ => Err(Error::Profile("host files not released for this runtime")),
        }
    }
    pub fn park(&mut self) {
        if let Self::Codex(session) = self {
            session.park();
        }
    }
    pub fn can_continue(&self) -> bool {
        match self {
            Self::Codex(session) => session.can_continue(),
            Self::Claude { wire, reusable, .. } => *reusable && wire.can_followup(),
            Self::Octos { .. } => false,
        }
    }
    /// Revalidate durable scope and the retained native process before a new
    /// reservation. A failed probe has not transmitted this turn's model input.
    pub async fn continue_context(
        &mut self,
        ledger: &mut Ledger,
        scope: &Scope,
        profile: &Profile,
    ) -> Result<()> {
        if !self.can_continue() {
            return Err(Error::Protocol("native session cannot continue"));
        }
        match self {
            Self::Codex(session) => session.continue_context(ledger, scope, profile).await,
            Self::Claude {
                wire,
                profile: prior,
                scope: bound,
                context,
                reusable,
                ..
            } => {
                crate::verify(&ledger.db, scope)?;
                if bound.as_ref() != Some(scope) || prior != profile {
                    return Err(Error::Protocol("retained context scope or profile differs"));
                }
                let (saved, counters): (String, String) = ledger.db.query_row(
                    "SELECT session,counters FROM external_sessions WHERE scope=? AND profile=?",
                    (scope.context_key(&ledger.owner)?, context.as_str()), |row| Ok((row.get(0)?, row.get(1)?)),
                ).map_err(crate::Error::from)?;
                if wire.session_id() != Some(saved.as_str())
                    || counters.is_empty()
                    || counters == "null"
                {
                    return Err(Error::Protocol(
                        "retained native session or accounting baseline differs",
                    ));
                }
                *reusable = false;
                wire.prepare_followup(claude::session::Limits::default())
                    .await
                    .map_err(|_| Error::Protocol("retained Claude process unavailable"))
            }
            Self::Octos { .. } => Err(Error::Protocol("native session cannot continue")),
        }
    }
    pub async fn open_context(
        &mut self,
        ledger: &mut Ledger,
        scope: &Scope,
        profile: &Profile,
    ) -> Result<()> {
        match self {
            Self::Codex(session) => session.open_context(ledger, scope, profile).await,
            Self::Claude {
                scope: bound,
                profile: original,
                ..
            }
            | Self::Octos {
                scope: bound,
                profile: original,
                ..
            } => {
                crate::verify(&ledger.db, scope)?;
                if bound.is_some() || original != profile {
                    return Err(Error::Protocol("context already open or profile differs"));
                }
                *bound = Some(scope.clone());
                Ok(())
            }
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_started(
        &mut self,
        ledger: &mut Ledger,
        owner: &str,
        scope: &Scope,
        call: &str,
        dispatch: &str,
        input: &str,
        mode: BudgetMode,
        queue: &ApprovalQueue,
        started: Option<tokio::sync::oneshot::Sender<()>>,
    ) -> Result<Completed> {
        self.run_with_started_observed(
            ledger, owner, scope, call, dispatch, input, mode, queue, started, None,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn run_with_started_observed(
        &mut self,
        ledger: &mut Ledger,
        owner: &str,
        scope: &Scope,
        call: &str,
        dispatch: &str,
        input: &str,
        mode: BudgetMode,
        queue: &ApprovalQueue,
        started: Option<tokio::sync::oneshot::Sender<()>>,
        observer: Option<&(dyn Fn(&Usage) + Send + Sync)>,
    ) -> Result<Completed> {
        if let Self::Codex(session) = self {
            return session
                .run_with_started_observed(
                    ledger, owner, scope, call, dispatch, input, mode, queue, started, observer,
                )
                .await;
        }
        ledger.owner(owner)?;
        let bound = match self {
            Self::Claude { scope, .. } | Self::Octos { scope, .. } => scope.as_ref(),
            _ => unreachable!(),
        };
        if bound != Some(scope) {
            return Err(Error::Protocol("run scope differs from opened context"));
        }
        if input.is_empty() || input.len() > 65536 {
            return Err(Error::Profile("input size"));
        }
        let controls = queue
            .controls
            .as_ref()
            .map(|inbox| inbox.claim(scope, dispatch, call))
            .transpose()?;
        if mode.admit(ledger, scope, call, dispatch)? != Reservation::New {
            return Err(Error::Protocol("refusing provider replay"));
        }
        if let Self::Claude { reusable, .. } = self {
            *reusable = false;
        }
        let mut charge = Charge {
            ledger,
            scope: scope.clone(),
            call: call.into(),
            settled: false,
        };
        let began = Instant::now();
        let result = match self {
            Self::Claude {
                wire,
                profile,
                context,
                ..
            } => {
                run_claude(
                    wire,
                    profile,
                    charge.ledger,
                    owner,
                    scope,
                    call,
                    context,
                    dispatch,
                    input,
                    queue,
                    started,
                    controls,
                )
                .await
            }
            Self::Octos {
                wire, profile, id, room, files, ..
            } => {
                run_octos(
                    wire,
                    profile,
                    id,
                    charge.ledger,
                    owner,
                    scope,
                    call,
                    dispatch,
                    input,
                    queue,
                    started,
                    controls,
                    *room,
                    files.as_mut(),
                )
                .await
            }
            _ => unreachable!(),
        };
        let TurnResult {
            text,
            mut usage,
            status,
            requests,
            returns,
            cumulative,
            late_failure,
        } = result?;
        if late_failure.is_some() {
            // The native stream failed after this answer was read. The charge
            // stays unsettled, so dropping it marks the call unknown exactly
            // as when such a turn failed outright; only the answer is new.
            return Ok(Completed {
                text,
                usage,
                status,
                elapsed_seconds: began.elapsed().as_secs(),
                tool_requests: requests,
                tool_returns: returns,
                late_failure,
            });
        }
        let is_claude = matches!(self, Self::Claude { .. });
        let (context, session) = match self {
            Self::Claude { wire, context, .. } => {
                (context.as_str(), wire.session_id().ok_or(Error::Unknown)?)
            }
            Self::Octos { wire, context, .. } => {
                (context.as_str(), wire.session_id().ok_or(Error::Unknown)?)
            }
            _ => unreachable!(),
        };
        // Claude projects UUID-deduplicated native result deltas for this
        // admitted turn. Octos retains its distinct session counter contract.
        let counters = if usage.is_reported() {
            use rusqlite::OptionalExtension;
            let baseline: Option<(String, String)> = charge
                .ledger
                .db
                .query_row(
                    "SELECT session,counters FROM external_sessions WHERE scope=? AND profile=?",
                    (scope.context_key(owner)?, context),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(crate::Error::from)?;
            if baseline.as_ref().is_some_and(|(prior, _)| prior != session) {
                return Err(Error::Protocol("resumed native session identity differs"));
            }
            let baseline = if is_claude {
                None
            } else {
                baseline.map(|(_, counters)| counters)
            };
            let baseline_missing = baseline
                .as_deref()
                .is_some_and(|s| s.is_empty() || s == "null");
            let prior: Option<Usage> = baseline
                .filter(|s| !s.is_empty() && s != "null")
                .map(|s| serde_json::from_str(&s))
                .transpose()?;
            if cumulative {
                let absolute = serde_json::to_string(&usage)?;
                if baseline_missing {
                    usage = crate::unreported_usage();
                } else if let Some(prior) = prior {
                    // A backwards native counter makes the charge unknown,
                    // not the already completed answer unavailable.
                    usage = usage_delta(&usage, &prior).unwrap_or_else(crate::unreported_usage);
                }
                absolute
            } else {
                // Claude main-loop counters exclude subagents, so they cannot
                // establish a whole-session baseline for a future modelUsage.
                if is_claude || baseline_missing {
                    "null".into()
                } else {
                    let mut absolute = usage.clone();
                    if let Some(prior) = prior {
                        absolute.input = absolute
                            .input
                            .checked_add(prior.input)
                            .ok_or(Error::Unknown)?;
                        absolute.output = absolute
                            .output
                            .checked_add(prior.output)
                            .ok_or(Error::Unknown)?;
                        absolute.cached_input = absolute
                            .cached_input
                            .checked_add(prior.cached_input)
                            .ok_or(Error::Unknown)?;
                        absolute.cache_write_input =
                            match (absolute.cache_write_input, prior.cache_write_input) {
                                (Some(current), Some(prior)) => {
                                    Some(current.checked_add(prior).ok_or(Error::Unknown)?)
                                }
                                _ => None,
                            };
                    }
                    serde_json::to_string(&absolute)?
                }
            }
        } else {
            String::new()
        };
        if let Some(observer) = observer {
            observer(&usage);
        }
        charge
            .ledger
            .settle_external(scope, call, &usage, context, session, &counters)?;
        charge.settled = true;
        if status == TurnStatus::Failed {
            return Err(Error::Failed);
        }
        if let Self::Claude { reusable, .. } = self {
            *reusable = usage.is_reported() && cumulative;
        }
        Ok(Completed {
            text,
            usage,
            status,
            elapsed_seconds: began.elapsed().as_secs(),
            tool_requests: requests,
            tool_returns: returns,
            late_failure: None,
        })
    }
}

fn usage_delta(current: &Usage, prior: &Usage) -> Option<Usage> {
    let mut delta = current.clone();
    delta.input = current.input.checked_sub(prior.input)?;
    delta.output = current.output.checked_sub(prior.output)?;
    delta.cached_input = current.cached_input.checked_sub(prior.cached_input)?;
    delta.cache_write_input = match (current.cache_write_input, prior.cache_write_input) {
        (Some(current), Some(prior)) => Some(current.checked_sub(prior)?),
        _ => None,
    };
    Some(delta)
}

/// The protocol error of a Claude Code installation below the floor. The host
/// matches it to tell the owner to update the CLI.
pub const CLAUDE_VERSION_REQUIRED: &str = "Claude Code 2.1.322 or newer is required";
fn claude_initialize_error(error: claude::session::Error) -> Error {
    match error {
        claude::session::Error::UnsupportedVersion => Error::Protocol(CLAUDE_VERSION_REQUIRED),
        _ => Error::Protocol("Claude initialize"),
    }
}
/// A resumed launch that failed before input for any reason other than a
/// missing context. The saved session row is kept in every one of these cases.
fn claude_resume_error(error: claude::session::Error) -> Error {
    use claude::session::Error as Native;
    match error {
        Native::UnsupportedVersion => claude_initialize_error(error),
        Native::Timeout => Error::Protocol("Claude resume timed out before input"),
        Native::Refused => Error::Protocol("Claude resume was refused before input"),
        Native::PeerEof => Error::Protocol("Claude resume process ended before input"),
        _ => Error::Protocol("Claude resume failed before input"),
    }
}

#[allow(clippy::too_many_arguments)]
async fn permission(
    ledger: &mut Ledger,
    owner: &str,
    scope: &Scope,
    dispatch: &str,
    tool: String,
    arguments: Value,
    cwd: &Path,
    queue: &ApprovalQueue,
) -> bool {
    let Ok(versions) = ledger.policy_snapshot(scope) else {
        return false;
    };
    let proposal = ToolProposal {
        scope: scope.clone(),
        dispatch: dispatch.into(),
        tool,
        arguments,
        canonical_directory: cwd.to_string_lossy().into(),
        risk: "high".into(),
        policy_revision: versions.map(|v| v.revision),
        expires: now() + 280,
    };
    match ledger.tool_disposition(&proposal, now()) {
        Ok(ToolPolicy::AllowWithRules { .. }) => ledger.authorize_tool(&proposal, now()).is_ok(),
        Ok(ToolPolicy::AskOwner) => {
            let (pending, decision) = codex::pending_approval(proposal.clone());
            queue.offer(pending)
                && matches!(
                    tokio::time::timeout(Duration::from_secs(280), decision).await,
                    Ok(Ok(true))
                )
                && ledger
                    .approve_tool(owner, &proposal, now())
                    .and_then(|_| ledger.authorize_tool(&proposal, now()))
                    .is_ok()
        }
        _ => false,
    }
}
type TurnResult = claude_turn::Outcome;

/// Whether this input is a slash command that the Agent's owner sent to their
/// own local Claude Agent. The owner's CLI then runs it (`/compact`, `/cost`,
/// a command or skill from the owner's settings). A room member who is not
/// the owner cannot run commands of the owner's CLI: their text stays a
/// prompt.
pub fn owner_command(owner: &str, scope: &Scope, input: &str) -> bool {
    scope.requester == owner && input.starts_with('/')
}

struct PersonalClaudeHost<'a> {
    ledger: &'a mut Ledger,
    owner: &'a str,
    scope: &'a Scope,
    call: &'a str,
    context: &'a str,
    dispatch: &'a str,
    queue: &'a ApprovalQueue,
}
impl claude_turn::Host for PersonalClaudeHost<'_> {
    async fn start(&mut self, process: &mut Claude, input: &str) -> Result<()> {
        // The owner's own slash command runs as one. Anyone else's text, and
        // any text the host wrapped, is a prompt and can never be a command.
        let written = if owner_command(self.owner, self.scope, input) {
            process.owner_command(input).await
        } else {
            process.prompt(input).await
        };
        written
            .map(|_| ())
            .map_err(|_| Error::Protocol("Claude prompt"))
    }
    fn bind_session(&mut self, session: &str) -> Result<()> {
        self.ledger
            .bind_external_session(
                self.scope,
                self.call,
                self.context,
                session,
                &Usage {
                    input: 0,
                    output: 0,
                    cached_input: 0,
                    cache_write_input: Some(0),
                    reasoning_output: 0,
                    accounting_version: "claude-reported-v1".into(),
                },
            )
            .map_err(Into::into)
    }
    fn replace_session(&mut self, session: &str) -> Result<()> {
        self.ledger
            .replace_external_session(
                self.scope,
                self.call,
                self.context,
                session,
                &Usage {
                    input: 0,
                    output: 0,
                    cached_input: 0,
                    cache_write_input: Some(0),
                    reasoning_output: 0,
                    accounting_version: "claude-reported-v1".into(),
                },
            )
            .map_err(Into::into)
    }
    async fn permission(
        &mut self,
        _request_id: &str,
        tool: String,
        arguments: Value,
        directory: &Path,
    ) -> bool {
        permission(
            self.ledger,
            self.owner,
            self.scope,
            self.dispatch,
            tool,
            arguments,
            directory,
            self.queue,
        )
        .await
    }
}
// Personal accounting is retained by its original session ledger and native
// final model totals. Shared execution supplies its separate typed call sink.
struct PersonalClaudeCapture;
impl claude_turn::Capture for PersonalClaudeCapture {
    async fn bind(&mut self, _process: &Claude) -> Result<()> {
        Ok(())
    }
    async fn observe(&mut self, _observation: &claude::session::Observation) -> Result<()> {
        Ok(())
    }
    async fn complete(&mut self, _process: &Claude, _text: &str) -> Result<()> {
        Ok(())
    }
    async fn complete_unaccounted(
        &mut self,
        _receipt: &claude::session::CompletionReceipt,
        _text: &str,
    ) -> Result<()> {
        Ok(())
    }
}
#[allow(clippy::too_many_arguments)]
async fn run_claude(
    wire: &mut Claude,
    profile: &Profile,
    ledger: &mut Ledger,
    owner: &str,
    scope: &Scope,
    call: &str,
    context: &str,
    dispatch: &str,
    input: &str,
    queue: &ApprovalQueue,
    started: Option<tokio::sync::oneshot::Sender<()>>,
    stream: Option<crate::run_controls::Stream>,
) -> Result<TurnResult> {
    let mut host = PersonalClaudeHost {
        ledger,
        owner,
        scope,
        call,
        context,
        dispatch,
        queue,
    };
    claude_turn::run(
        wire,
        profile,
        scope,
        dispatch,
        input,
        queue,
        started,
        stream,
        &mut host,
        &mut PersonalClaudeCapture,
    )
    .await
}
enum OctosWait<T> {
    Event(octos::session::Event),
    Host(T),
    Control(crate::run_controls::Pending),
}

#[derive(Default)]
struct OctosSteers(Vec<(String, crate::run_controls::Pending)>);
impl OctosSteers {
    fn dropped(&mut self, inputs: Vec<String>) -> Result<()> {
        for text in inputs {
            // Octos drains accepted inputs in order and returns the remaining
            // suffix. For identical text, match the newest still-pending
            // submission so an earlier consumed action is not restored.
            let index = self.0.iter().rposition(|(pending, _)| pending == &text)
                .ok_or(Error::Protocol("unknown dropped Octos steer"))?;
            let (_, pending) = self.0.remove(index);
            pending.reply(crate::run_controls::Outcome::Unavailable);
        }
        Ok(())
    }
    fn consumed(&mut self) {
        for (_, pending) in self.0.drain(..) {
            pending.reply(crate::run_controls::Outcome::Accepted);
        }
    }
}

async fn octos_wait<F: std::future::Future + ?Sized>(
    wire: &mut Octos,
    host: std::pin::Pin<&mut F>,
    controls: &mut Option<crate::run_controls::Stream>,
) -> Result<OctosWait<F::Output>> {
    let selected = async {
        tokio::select! { biased;
            control = crate::run_controls::next(controls) => OctosWait::Control(control),
            output = host => OctosWait::Host(output),
        }
    };
    tokio::pin!(selected);
    match wire.next_or_control(selected.as_mut()).await
        .map_err(|_| Error::Protocol("Octos event wait"))? {
        octos::session::ControlUpdate::Event(event) => Ok(OctosWait::Event(event)),
        octos::session::ControlUpdate::Control(value) => Ok(value),
    }
}

/// Keep processing the original turn while a host tool waits on an owner or
/// a fresh server authorization. Dropping the wait withdraws its local request.
#[cfg(unix)]
async fn octos_wait_tool<F: std::future::Future + ?Sized>(
    wire: &mut Octos,
    mut host: std::pin::Pin<&mut F>,
    controls: &mut Option<crate::run_controls::Stream>,
    buffered: &mut std::collections::VecDeque<(octos::session::Event, Option<octos::session::Observation>)>,
    stopping: &mut bool,
    steers: &mut OctosSteers,
    call_id: &str,
) -> Result<Option<F::Output>> {
    loop {
        match octos_wait(wire, host.as_mut(), controls).await? {
            OctosWait::Host(value) => return Ok(wire.host_tool_pending(call_id).then_some(value)),
            OctosWait::Control(action) => {
                if octos_control(wire, action, steers).await? {
                    *stopping = true;
                    controls.take();
                    return Ok(None);
                }
            }
            OctosWait::Event(event) => {
                let ended = matches!(&event, octos::session::Event::TurnEnded { .. } | octos::session::Event::Idle(_))
                    || matches!(&event, octos::session::Event::ToolCancelled { call_id: id } if id == call_id);
                if buffered.len() >= 256 { return Err(Error::Unknown); }
                buffered.push_back((event, wire.last_observation().cloned()));
                if ended { return Ok(None); }
            }
        }
    }
}

/// Return true only after the original turn's interrupt was acknowledged.
async fn octos_control(wire: &mut Octos, mut pending: crate::run_controls::Pending, steers: &mut OctosSteers) -> Result<bool> {
    use crate::run_controls::{Action, Outcome};
    if pending.is_closed() {
        return Ok(false);
    }
    let interrupt = matches!(pending.action, Action::Interrupt);
    if !interrupt && !wire.supports_active_only_steer() {
        pending.reply(Outcome::Unsupported);
        return Ok(false);
    }
    if !interrupt && steers.0.len() >= 16 {
        pending.reply(Outcome::Busy);
        return Ok(false);
    }
    pending.begin_write();
    let accepted = match &pending.action {
        Action::Steer { text } => wire.steer(text).await,
        Action::Interrupt => wire.interrupt().await,
    }.map_err(|_| Error::Protocol("Octos control outcome unknown"))?;
    if accepted && !interrupt {
        let Action::Steer { text } = &pending.action else { unreachable!() };
        steers.0.push((text.clone(), pending));
    } else {
        pending.reply(if accepted { Outcome::Accepted } else { Outcome::Rejected });
    }
    Ok(interrupt && accepted)
}

fn octos_room_tools(tools: hagency_runtime::room_tools::RoomTools) -> Result<Vec<octos::session::HostTool>> {
    tools.definitions().into_iter().map(|definition| {
        let name = definition["name"].as_str().ok_or(Error::Protocol("room tool name"))?;
        let description = definition["description"].as_str().ok_or(Error::Protocol("room tool description"))?;
        Ok(octos::session::HostTool {
            name: format!("hagency.{name}"),
            description: description.into(),
            input_schema: definition["inputSchema"].clone(),
            read_only: matches!(name, "read_room_history" | "list_received_files" | "get_file_delivery"),
        })
    }).collect()
}

fn octos_progress_tool() -> octos::session::HostTool {
    octos::session::HostTool {
        name: "hagency.report_progress".into(),
        description: "Send a standalone progress message to the current conversation while continuing this task. Use for meaningful findings, milestones, blockers, or corrections. Do not include secrets or private reasoning.".into(),
        input_schema: serde_json::json!({"type":"object","properties":{
            "text":{"type":"string","minLength":1,"maxLength":16384}},
            "required":["text"],"additionalProperties":false}),
        read_only: false,
    }
}
fn octos_progress(
    call: &octos::session::HostToolCall, output: Option<&hagency_runtime::live_text::LiveText>,
) -> std::result::Result<Value, String> {
    let Some(output) = output.filter(|output| output.has_messages()) else {
        return Err("progress_delivery_unavailable".into());
    };
    let Some(args) = call.args.as_object() else { return Err("invalid_progress_message".into()); };
    let Some(text) = args.get("text").and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty() && text.len() <= 16 * 1024) else {
        return Err("invalid_progress_message".into());
    };
    if args.len() != 1 { return Err("invalid_progress_message".into()); }
    output.message(&format!("octos:tool:{}", call.call_id), text)
        .map_err(|_| "progress_delivery_unavailable".to_owned())?;
    Ok(serde_json::json!({"accepted":true,"delivery":"queued"}))
}

#[allow(clippy::too_many_arguments)]
async fn run_octos(
    wire: &mut Octos,
    profile: &Profile,
    id: &str,
    ledger: &mut Ledger,
    owner: &str,
    scope: &Scope,
    execution: &str,
    dispatch: &str,
    input: &str,
    queue: &ApprovalQueue,
    started: Option<tokio::sync::oneshot::Sender<()>>,
    mut controls: Option<crate::run_controls::Stream>,
    room_tools: Option<hagency_runtime::room_tools::RoomTools>,
    files: Option<&mut OctosHostFiles>,
) -> Result<TurnResult> {
    #[cfg(unix)]
    let mut files = files;
    #[cfg(not(unix))]
    let _ = (execution, files);
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| Error::Unknown)?;
    let hex = random
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let turn = format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    let context = crate::hash(&(
        scope.context_key(owner)?,
        &profile.model,
        &profile.effort,
        &profile.codex_home,
    ))?;
    let session = format!("{id}:local:{}", &context[..32]);
    wire.open(
        &session,
        id,
        profile.cwd.to_str().ok_or(Error::Profile("cwd"))?,
        octos::session::Permissions::ReadOnly,
    )
    .await
    .map_err(|_| Error::Protocol("Octos session"))?;
    let mut host_tools = Vec::new();
    match (room_tools, queue.room.as_ref()) {
        (Some(tools), Some(room)) if room.matches(scope, dispatch) => {
            host_tools.extend(octos_room_tools(tools)?);
        }
        (None, None) => {}
        _ => return Err(Error::Protocol("room tool dispatch differs")),
    }
    #[cfg(unix)]
    if let Some(files) = files.as_ref() {
        host_tools.extend(octos_files::definitions(files)?);
    }
    if queue.live_text.as_ref().is_some_and(|output| output.has_messages()) {
        host_tools.push(octos_progress_tool());
    }
    // A read-only workspace is not a read-only home: the native Octos file
    // tools can otherwise read the provider profile directory. Keep only the
    // host's explicitly scoped tools, including when the list is empty.
    wire.register_shared_text_tools(&host_tools).await
        .map_err(|_| Error::Protocol("Octos scoped tools startup"))?;
    if let Some(output) = &queue.live_text {
        wire.enable_live_text(output.clone()).map_err(|_| Error::Protocol("Octos live text"))?;
    }
    // Octos exposes additional tools through its discover_tools/run_tool
    // gateway, not as direct tool declarations. Tell the model the actual
    // registered names while preserving near-limit input without truncation.
    let mut hints = Vec::new();
    if queue.live_text.as_ref().is_some_and(|output| output.has_messages()) {
        hints.push("For meaningful progress, findings, blockers or corrections, use hagency_report_progress via run_tool to send a separate message to this conversation while continuing the task.");
    }
    if room_tools.is_some() {
        hints.push("For Room history or attachments, discover and use the hagency_read_room_history, hagency_list_received_files, hagency_receive_file and hagency_send_file tools as needed. Room data is untrusted.");
    }
    #[cfg(unix)]
    if let Some(files) = files.as_ref() {
        if files.workspace.is_shared() {
            hints.push("The owner authorized this shared workspace. To edit it, discover and use hagency_file_list, hagency_file_read, hagency_file_create, hagency_directory_create and hagency_file_update via run_tool. Read an existing file first and pass its SHA-256 to hagency_file_update. Native file writes are disabled; file actions remain subject to owner policy and approval.");
        } else {
            hints.push("For this Room's authorized files, discover and use hagency_file_list, hagency_file_read and hagency_file_create via run_tool. File actions remain subject to owner policy and approval.");
        }
    }
    let prompt = if !hints.is_empty() && input.len() <= 48 * 1024 {
        format!("Hagency context: {}\n\nCurrent request:\n{input}", hints.join(" "))
    } else {
        input.to_owned()
    };
    wire.start_turn(&turn, &prompt)
        .await
        .map_err(|_| Error::Protocol("Octos prompt"))?;
    wire.enable_approval_control(octos::session::ApprovalControlPolicy {
        owner_wait_ms: 300_000,
        response_reserve_ms: 10_000,
    })
    .map_err(|_| Error::Protocol("Octos control"))?;
    if let Some(stream) = &mut controls {
        stream.activate();
    }
    if let Some(send) = started {
        let _ = send.send(());
    }
    let mut requests = std::collections::BTreeSet::new();
    let mut returns = std::collections::BTreeSet::new();
    let mut buffered = std::collections::VecDeque::new();
    let mut stopping = false;
    let mut steers = OctosSteers::default();
    for _ in 0..16384 {
        let (event, observation) = loop {
            if let Some(item) = buffered.pop_front() {
                break item;
            }
            let never = std::future::pending::<()>();
            tokio::pin!(never);
            match octos_wait(wire, never.as_mut(), &mut controls).await? {
                OctosWait::Event(event) => break (event, wire.last_observation().cloned()),
                OctosWait::Control(action) => {
                    if octos_control(wire, action, &mut steers).await? {
                        stopping = true;
                        controls.take();
                    }
                }
                OctosWait::Host(()) => unreachable!(),
            }
        };
        match event {
            octos::session::Event::Approval {
                approval_id,
                params,
                ..
            } => {
                if stopping { continue; }
                let directory = approval_directory(&profile.cwd, &params);
                let decision = permission(
                    ledger,
                    owner,
                    scope,
                    dispatch,
                    "octos.approval".into(),
                    params,
                    &directory,
                    queue,
                );
                tokio::pin!(decision);
                let allow = loop {
                    match octos_wait(wire, decision.as_mut(), &mut controls).await? {
                        OctosWait::Host(allow) => break Some(allow),
                        OctosWait::Control(action) => {
                            if octos_control(wire, action, &mut steers).await? {
                                stopping = true;
                                controls.take();
                                break None;
                            }
                        }
                        OctosWait::Event(event) => {
                            let cancelled = matches!(&event,octos::session::Event::ApprovalSettled {approval_id:id} if id==&approval_id);
                            let ended = matches!(&event, octos::session::Event::Idle(_));
                            if buffered.len() >= 256 {
                                return Err(Error::Unknown);
                            }
                            buffered.push_back((event, wire.last_observation().cloned()));
                            if cancelled || ended {
                                break None;
                            }
                        }
                    }
                };
                let Some(allow) = allow else {
                    continue;
                };
                let mut prepared = wire
                    .prepare_approval(
                        &approval_id,
                        if allow {
                            octos::session::PermissionDecision::Allow
                        } else {
                            octos::session::PermissionDecision::Deny
                        },
                    )
                    .map_err(|_| Error::Protocol("Octos decision"))?;
                loop {
                    match wire
                        .send_prepared_approval(&mut prepared)
                        .await
                        .map_err(|_| Error::Protocol("Octos approval write"))?
                    {
                        octos::session::PreparedUpdate::WriteAccepted(_) => break,
                        octos::session::PreparedUpdate::Event(event) => {
                            if buffered.len() >= 256 {
                                return Err(Error::Unknown);
                            }
                            buffered.push_back((event, wire.last_observation().cloned()));
                        }
                    }
                }
            }
            octos::session::Event::Tool {
                tool_call_id,
                ended,
                ..
            } => {
                if ended {
                    returns.insert(tool_call_id);
                } else {
                    requests.insert(tool_call_id);
                }
            }
            octos::session::Event::Question { question_id, turn_id, questions } => {
                if stopping || !wire.question_pending(&question_id) { continue; }
                let waiter = async {
                    match &queue.questions {
                        Some(channel) => channel.ask(scope, dispatch, question_id.clone(), questions,
                            tokio::time::Instant::now() + std::time::Duration::from_secs(300)).await,
                        None => None,
                    }
                };
                tokio::pin!(waiter);
                let answer = loop {
                    match octos_wait(wire, waiter.as_mut(), &mut controls).await? {
                        OctosWait::Host(answer) => break Some(answer),
                        OctosWait::Control(action) => {
                            if octos_control(wire, action, &mut steers).await? {
                                stopping = true;
                                controls.take();
                                break None;
                            }
                        }
                        OctosWait::Event(event) => {
                            let withdrawn = matches!(&event,
                                octos::session::Event::QuestionCancelled { question_id: id } if id == &question_id)
                                || matches!(&event, octos::session::Event::TurnEnded { turn_id: id, .. } if id == &turn_id)
                                || matches!(&event, octos::session::Event::Idle(_));
                            if buffered.len() >= 256 { return Err(Error::Unknown); }
                            buffered.push_back((event, wire.last_observation().cloned()));
                            if withdrawn { break None; }
                        }
                    }
                };
                if let Some(submitted) = answer {
                    let empty = Default::default();
                    let answers = submitted.as_ref().map(|answer| &answer.answers).unwrap_or(&empty);
                    let accepted = wire.answer_question(&question_id, answers).await
                        .map_err(|_| Error::Protocol("Octos question answer"))?;
                    if accepted {
                        if let Some(answer) = submitted { answer.accepted(); }
                    }
                }
            }
            octos::session::Event::SteerDropped { turn_id, inputs } if turn_id == turn => {
                steers.dropped(inputs)?;
            }
            octos::session::Event::TurnEnded { turn_id, .. } if turn_id == turn => {
                steers.consumed();
            }
            octos::session::Event::Idle(idle) => {
                let Some(observation) = observation else {
                    return Err(Error::Unknown);
                };
                let octos::session::ObservationKind::Idle(evidence) = observation.kind() else {
                    return Err(Error::Unknown);
                };
                let usage = match (
                    evidence.input(),
                    evidence.cache_read(),
                    evidence.cache_write(),
                    evidence.output(),
                ) {
                    (Some(input), Some(cache), Some(write), Some(output)) => Usage {
                        input: input
                            .checked_add(cache)
                            .and_then(|v| v.checked_add(write))
                            .ok_or(Error::Unknown)?,
                        output,
                        cached_input: cache,
                        cache_write_input: Some(write),
                        reasoning_output: 0,
                        accounting_version: "octos-reported-v1".into(),
                    },
                    _ => crate::unreported_usage(),
                };
                return Ok(TurnResult {
                    text: idle.reply.unwrap_or_default(),
                    usage,
                    status: match idle.outcome {
                        octos::Outcome::Completed => TurnStatus::Completed,
                        octos::Outcome::Interrupted => TurnStatus::Interrupted,
                        _ => TurnStatus::Failed,
                    },
                    requests: requests.len(),
                    returns: returns.len(),
                    cumulative: evidence.coverage() == octos::session::UsageCoverage::Session,
                    late_failure: None,
                });
            }
            octos::session::Event::ToolCall { params } => {
                if params["call_id"].as_str().is_some_and(|id| !wire.host_tool_pending(id)) {
                    continue;
                }
                let call = wire
                    .host_tool_call(&params)
                    .map_err(|_| Error::Protocol("unexpected Octos host tool"))?;
                if call.name == "hagency.report_progress" {
                    if stopping { continue; }
                    let reply = octos_progress(&call, queue.live_text.as_ref());
                    wire.host_tool_result(&call.call_id, reply).await
                        .map_err(|_| Error::Protocol("Octos progress response"))?;
                    continue;
                }
                #[cfg(unix)]
                if octos_files::is_file_tool(&call.name) {
                    if let Some(files) = files.as_deref_mut() {
                        octos_files::handle(wire, files, ledger, owner, scope, execution, dispatch,
                            &session, &turn, &call, queue, &mut controls, &mut buffered, &mut stopping, &mut steers).await?;
                    } else {
                        wire.host_tool_result(&call.call_id, Err("host_files_disabled".into())).await
                            .map_err(|_| Error::Protocol("Octos tool response"))?;
                    }
                    continue;
                }
                let operation = room_tools.and_then(|tools| call.name.strip_prefix("hagency.")
                    .and_then(|name| tools.parse(name, &call.args).ok()));
                let Some((room, operation)) = queue.room.as_ref().zip(operation) else {
                    wire.host_tool_result(&call.call_id, Err("unsupported_host_tool".into())).await
                        .map_err(|_| Error::Protocol("Octos tool response"))?;
                    continue;
                };
                if stopping { continue; }
                let work = room.call(operation, call.call_id.clone(),
                    tokio::time::Instant::now() + std::time::Duration::from_secs(55));
                tokio::pin!(work);
                let reply = loop {
                    match octos_wait(wire, work.as_mut(), &mut controls).await? {
                        OctosWait::Host(reply) => break Some(reply),
                        OctosWait::Control(action) => {
                            if octos_control(wire, action, &mut steers).await? {
                                stopping = true;
                                controls.take();
                                break None;
                            }
                        }
                        OctosWait::Event(event) => {
                            let ended = matches!(&event, octos::session::Event::TurnEnded { .. } | octos::session::Event::Idle(_))
                                || matches!(&event, octos::session::Event::ToolCancelled { call_id } if call_id == &call.call_id);
                            if buffered.len() >= 256 { return Err(Error::Unknown); }
                            buffered.push_back((event, wire.last_observation().cloned()));
                            if ended { break None; }
                        }
                    }
                };
                if let Some(reply) = reply {
                    let result = if reply.is_error() { Err(reply.text().to_owned()) } else {
                        Ok(serde_json::from_str(reply.text()).unwrap_or_else(|_| serde_json::Value::String(reply.text().into())))
                    };
                    wire.host_tool_result(&call.call_id, result).await
                        .map_err(|_| Error::Protocol("Octos tool response"))?;
                }
            }
            _ => {}
        }
    }
    Err(Error::Unknown)
}

/// Read only the CLI's public authentication verdict. Never reads credentials.
pub async fn claude_account(binary: &Path, home: &Path, config: &Path) -> Result<bool> {
    let status = if claude_uses_default_config(home, config) {
        claude_default_account_status(binary, home).await
    } else {
        claude_account_status(binary, home, config).await
    };
    status.map(|account| account.authenticated)
}

/// The implicit native account and an explicitly selected config directory
/// are distinct macOS Keychain namespaces, even at the same filesystem path.
pub fn claude_uses_default_config(home: &Path, config: &Path) -> bool {
    std::env::var_os("CLAUDE_CONFIG_DIR").is_none()
        && home.join(".claude").canonicalize().ok().as_deref() == Some(config)
}

/// Public native CLI metadata, not a verified provider identity or quota key.
/// No credential fields, Debug or serialization escape this adapter.
#[derive(PartialEq, Eq)]
pub struct ClaudeAccountStatus {
    authenticated: bool,
    method: Option<String>,
    provider: Option<String>,
    email: Option<String>,
    organization: Option<String>,
}
impl ClaudeAccountStatus {
    pub fn oauth_metadata(&self) -> Option<(&str, &str)> {
        if !self.authenticated
            || self.method.as_deref() != Some("claude.ai")
            || self.provider.as_deref() != Some("firstParty")
        {
            return None;
        }
        Some((self.email.as_deref()?, self.organization.as_deref()?))
    }
    /// The CLI uses an API key (true) or a bearer token (false) rather than a
    /// claude.ai sign-in. Claude Code 2.1.322 reports these as `api_key` and
    /// `oauth_token`; the second also covers a claude.ai token in the
    /// environment, which the caller tells apart by the variable it finds.
    pub fn api_credential(&self) -> Option<bool> {
        if !self.authenticated || self.provider.as_deref() != Some("firstParty") {
            return None;
        }
        match self.method.as_deref()? {
            "api_key" => Some(true),
            "oauth_token" => Some(false),
            _ => None,
        }
    }
}

pub async fn claude_account_status(
    binary: &Path,
    home: &Path,
    config: &Path,
) -> Result<ClaudeAccountStatus> {
    claude_account_status_in(binary, home, Some(config)).await
}
/// The native default login uses a distinct Keychain service from an explicit
/// CLAUDE_CONFIG_DIR, even when both configuration paths name ~/.claude.
pub async fn claude_default_account_status(
    binary: &Path,
    home: &Path,
) -> Result<ClaudeAccountStatus> {
    claude_account_status_in(binary, home, None).await
}
async fn claude_account_status_in(
    binary: &Path,
    home: &Path,
    config: Option<&Path>,
) -> Result<ClaudeAccountStatus> {
    use tokio::io::AsyncReadExt;
    let mut command = tokio::process::Command::new(binary);
    command
        .args(["auth", "status", "--json"])
        .env_clear()
        .env("HOME", home)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(config) = config {
        command.env("CLAUDE_CONFIG_DIR", config);
    }
    for name in ["USER", "PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn()?;
    let mut output = child
        .stdout
        .take()
        .ok_or(Error::Profile("account stdout"))?
        .take(65537);
    let mut bytes = Vec::new();
    let work = async {
        output.read_to_end(&mut bytes).await?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>(status)
    };
    let status = tokio::time::timeout(Duration::from_secs(10), work)
        .await
        .map_err(|_| Error::Timeout)??;
    if bytes.len() > 65536 {
        return Err(Error::Protocol("account response bound"));
    }
    let value: Value = serde_json::from_slice(&bytes)?;
    let field = |key: &str| -> Result<Option<String>> {
        match value.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s))
                if !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control) =>
            {
                Ok(Some(s.clone()))
            }
            _ => Err(Error::Protocol("account metadata")),
        }
    };
    Ok(ClaudeAccountStatus {
        authenticated: status.success() && value["loggedIn"] == true,
        method: field("authMethod")?,
        provider: field("apiProvider")?,
        email: field("email")?,
        organization: field("orgId")?,
    })
}

/// A model the native CLI offers under the configuration it runs with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeModelChoice {
    /// The model id the CLI would send, not its alias.
    pub model: String,
    /// The effort levels the CLI says this model takes; empty when it names none.
    pub efforts: Vec<String>,
}

/// The models the native CLI itself offers for this configuration: its answer
/// to the `initialize` control request. No prompt is sent, so no model is
/// called and nothing is billed. `config` is `None` for the implicit native
/// account, as for [`claude_default_account_status`]. The CLI's safe mode
/// keeps its sign-in and model selection (the model names of the owner's
/// settings included) and leaves out the owner's hooks, plugins and MCP
/// servers, so asking runs nothing of the owner's and saves no session.
pub async fn claude_models(
    binary: &Path,
    home: &Path,
    config: Option<&Path>,
) -> Result<Vec<ClaudeModelChoice>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    const REQUEST: &str = "hagency_models";
    let mut command = tokio::process::Command::new(binary);
    command
        .args([
            "--print",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--strict-mcp-config",
            "--safe-mode",
            "--no-session-persistence",
        ])
        .env_clear()
        .env("HOME", home)
        // No project of the owner's: only the user-level configuration counts.
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(config) = config {
        command.env("CLAUDE_CONFIG_DIR", config);
    }
    for name in ["USER", "PATH", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn()?;
    let mut input = child.stdin.take().ok_or(Error::Profile("models stdin"))?;
    let output = child.stdout.take().ok_or(Error::Profile("models stdout"))?;
    let work = async {
        let request = serde_json::json!({"type":"control_request","request_id":REQUEST,
            "request":{"subtype":"initialize"}});
        input.write_all(format!("{request}\n").as_bytes()).await?;
        input.flush().await?;
        let mut lines = tokio::io::BufReader::new(output).lines();
        let mut read = 0usize;
        while let Some(line) = lines.next_line().await? {
            read = read.saturating_add(line.len());
            if read > 1024 * 1024 {
                return Err(Error::Protocol("models response bound"));
            }
            let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if frame["type"] == "control_response" && frame["response"]["request_id"] == REQUEST {
                return Ok(claude_models_from(&frame));
            }
        }
        Err(Error::Protocol("models response"))
    };
    let result = tokio::time::timeout(Duration::from_secs(10), work)
        .await
        .map_err(|_| Error::Timeout)
        .and_then(|found| found);
    // The answer is all that was wanted: the session is not used further.
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    result
}

/// The model ids of an `initialize` answer, each once, in the CLI's order.
fn claude_models_from(frame: &Value) -> Vec<ClaudeModelChoice> {
    let named = |name: &str| {
        !name.is_empty()
            && name.len() <= 200
            && name.as_bytes()[0].is_ascii_alphanumeric()
            && name.bytes().all(|b| {
                b.is_ascii_alphanumeric()
                    || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'@' | b'[' | b']')
            })
    };
    let mut choices: Vec<ClaudeModelChoice> = Vec::new();
    let rows = frame["response"]["response"]["models"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    for row in rows.iter().take(64) {
        // An alias row names what it stands for; a custom row is its own name.
        let Some(model) = row["resolvedModel"]
            .as_str()
            .or_else(|| row["value"].as_str())
            .filter(|name| named(name))
        else {
            continue;
        };
        if choices.iter().any(|known| known.model == model) {
            continue;
        }
        let efforts = row["supportedEffortLevels"]
            .as_array()
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(Value::as_str)
                    .filter(|level| matches!(*level, "low" | "medium" | "high" | "xhigh" | "max"))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        choices.push(ClaudeModelChoice {
            model: model.to_owned(),
            efforts,
        });
    }
    choices
}

fn approval_directory(cwd: &Path, arguments: &Value) -> std::path::PathBuf {
    let target = arguments["typed_details"]["command"]["cwd"]
        .as_str()
        .or_else(|| arguments["file_path"].as_str())
        .or_else(|| arguments["path"].as_str())
        .or_else(|| arguments["cwd"].as_str())
        .or_else(|| arguments["working_directory"].as_str());
    let Some(target) = target else {
        return cwd.to_path_buf();
    };
    let path = Path::new(target);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    if path.is_dir() {
        return path.canonicalize().unwrap_or(path);
    }
    path.parent()
        .and_then(|p| p.canonicalize().ok())
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn octos_progress_uses_the_host_call_identity_and_cannot_choose_a_room() {
        #[derive(Default)]
        struct Sink(std::sync::Mutex<Vec<(String, String)>>);
        impl hagency_runtime::live_text::MessageSink for Sink {
            fn record(&self, id: &str, text: &str) -> std::result::Result<(), hagency_runtime::live_text::MessageError> {
                self.0.lock().unwrap().push((id.into(), text.into()));
                Ok(())
            }
        }
        let sink = std::sync::Arc::new(Sink::default());
        let output = hagency_runtime::live_text::LiveText::with_messages(sink.clone());
        let mut call = octos::session::HostToolCall { call_id: "call-1".into(),
            name: "hagency.report_progress".into(), args: serde_json::json!({"text":"found it"}) };
        assert!(octos_progress(&call, Some(&output)).is_ok());
        call.args["room_id"] = serde_json::json!("!other:test");
        assert!(octos_progress(&call, Some(&output)).is_err());
        call.args = serde_json::json!({"text":"   "});
        assert!(octos_progress(&call, Some(&output)).is_err());
        assert!(octos_progress(&call, None).is_err());
        assert_eq!(&*sink.0.lock().unwrap(), &[("octos:tool:call-1".into(), "found it".into())]);
    }
    #[test]
    fn only_the_owners_own_slash_text_is_a_command() {
        let scope = |requester: &str| Scope {
            agent: "agent-a".into(),
            binding: "binding-a".into(),
            room: "!room:test".into(),
            requester: requester.into(),
            thread: "!room:test".into(),
        };
        assert!(owner_command("@owner:test", &scope("@owner:test"), "/compact"));
        // Another room member may call the Agent, but not the owner's CLI.
        assert!(!owner_command("@owner:test", &scope("@member:test"), "/compact"));
        for text in ["compact", " /compact", "see /compact", ""] {
            assert!(!owner_command("@owner:test", &scope("@owner:test"), text));
        }
    }
    #[test]
    fn the_clis_model_list_is_read_once_per_model_in_its_order() {
        let frame = serde_json::json!({"type":"control_response","response":{"subtype":"success",
            "request_id":"hagency_models","response":{"models":[
                {"value":"default","resolvedModel":"claude-opus-5-5","supportsEffort":true,
                    "supportedEffortLevels":["low","medium","high","xhigh","max","made-up"]},
                // The alias of the default names the same model: listed once.
                {"value":"opus","resolvedModel":"claude-opus-5-5","supportedEffortLevels":["low"]},
                {"value":"haiku","resolvedModel":"claude-haiku-5-5"},
                // A custom row is its own name.
                {"value":"open-model/a:1"},
                {"value":"has space"},
                {"value":"-flag"},
                {"value":7},
                {"value":"x".repeat(201)},
            ]}}});
        let choice = |model: &str, efforts: &[&str]| ClaudeModelChoice {
            model: model.into(),
            efforts: efforts.iter().map(|e| (*e).to_owned()).collect(),
        };
        assert_eq!(
            claude_models_from(&frame),
            [
                choice("claude-opus-5-5", &["low", "medium", "high", "xhigh", "max"]),
                choice("claude-haiku-5-5", &[]),
                choice("open-model/a:1", &[]),
            ]
        );
        for frame in [
            serde_json::json!({}),
            serde_json::json!({"response":{"response":{"models":"none"}}}),
        ] {
            assert!(claude_models_from(&frame).is_empty());
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn the_cli_is_asked_for_its_models_without_a_prompt() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let fake = |name: &str, body: &str| {
            let binary = root.path().join(name);
            std::fs::write(&binary, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            binary
        };
        // Answers the one request it is sent, after other output, then waits
        // for more input as the real CLI does: it must be ended, not waited for.
        let answering = fake(
            "answering",
            r#"read line
case "$line" in *'"subtype":"initialize"'*) ;; *) exit 3 ;; esac
case "$*" in *--strict-mcp-config*--safe-mode*--no-session-persistence*) ;; *) exit 4 ;; esac
[ "$CLAUDE_CONFIG_DIR" = "$HOME/../config" ] || exit 5
echo 'starting'
echo '{"type":"system","subtype":"init"}'
echo '{"type":"control_response","response":{"request_id":"other","response":{"models":[{"value":"wrong"}]}}}'
echo '{"type":"control_response","response":{"subtype":"success","request_id":"hagency_models","response":{"models":[{"value":"sonnet","resolvedModel":"claude-sonnet-5-5","supportedEffortLevels":["low","high"]}]}}}'
read rest"#,
        );
        let config = home.join("../config");
        let started = std::time::Instant::now();
        let models = claude_models(&answering, &home, Some(&config)).await.unwrap();
        assert_eq!(
            models,
            [ClaudeModelChoice {
                model: "claude-sonnet-5-5".into(),
                efforts: vec!["low".into(), "high".into()],
            }]
        );
        assert!(started.elapsed() < Duration::from_secs(5));
        // A CLI that ends without the answer is an error, not an empty list.
        let silent = fake("silent", "read line\necho '{\"type\":\"system\"}'");
        assert!(claude_models(&silent, &home, None).await.is_err());
    }
    #[test]
    fn references_require_exact_kind_hash_and_octos_profile() {
        let hash = "a".repeat(64);
        assert_eq!(
            managed_reference(&format!("claude-managed:shared-home:{hash}")),
            Some((Kind::Claude, None))
        );
        assert_eq!(
            managed_reference(&format!("octos-managed:shared-home:{hash}:coding")),
            Some((Kind::Octos, Some("coding")))
        );
        for reference in [
            format!("octos-managed:shared-home:{hash}"),
            format!("claude-managed:shared-home:{hash}:coding"),
            format!("octos-managed:shared-home:{hash}:../private"),
            "claude-managed:shared-home:bad".into(),
        ] {
            assert!(managed_reference(&reference).is_none());
        }
    }
}
