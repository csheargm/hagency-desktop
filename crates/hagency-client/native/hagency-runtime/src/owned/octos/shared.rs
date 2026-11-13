//! Scoped process construction only. A pool still needs an authenticated
//! custody record, renewable server activity and durable usage accounting.
use super::{Error, Limits, OwnedOctosSession, Operation, Phase, SharedScope, StartError};
use crate::octos::profile_binding::ProfileBinding;
use std::path::Path;

/// A closed, owner-selected policy for one isolated process. The v5 root
/// remains a text root; successor commands bind their selected tool policy in
/// the process observation and durable custody.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SharedOctosPolicy {
    Text,
    LiveWeb,
    Standard { web: bool },
    Yolo { web: bool },
}
impl SharedOctosPolicy {
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
            Self::Text => "octos_text_v1",
            Self::LiveWeb => "octos_text_live_web_v1",
            Self::Standard { web: false } => "octos_tools_standard_v1",
            Self::Standard { web: true } => "octos_tools_standard_web_v1",
            Self::Yolo { web: false } => "octos_tools_yolo_v1",
            Self::Yolo { web: true } => "octos_tools_yolo_web_v1",
        }
    }
    pub fn tools(self) -> bool { matches!(self, Self::Standard { .. } | Self::Yolo { .. }) }
    pub fn web(self) -> bool { matches!(self, Self::LiveWeb | Self::Standard { web: true } | Self::Yolo { web: true }) }
    pub fn permissions(self) -> crate::octos::session::Permissions {
        if self.tools() { crate::octos::session::Permissions::WorkspaceWrite }
        else { crate::octos::session::Permissions::ReadOnly }
    }
    pub(crate) fn generic_tools(self) -> Vec<&'static str> {
        // Registry filtering also constrains discover_tools/run_tool. These
        // file tools are replaced by the scoped Octos build with component-
        // wise workspace opens. The host verifies that build's capability.
        let mut names = vec!["ask_user_question", "discover_tools", "run_tool", "update_plan"];
        if self.tools() { names.extend(["read_file", "write_file", "edit_file", "list_dir", "glob", "grep"]); }
        // Shell runs in a separately supervised sandbox owned by Hagency;
        // macOS cannot apply a nested sandbox from the confined Octos process.
        if self.web() { names.extend(["web_search", "web_fetch"]); }
        names
    }
}

#[cfg(target_os = "macos")]
pub(super) struct RuntimeAlias {
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
}

#[cfg(target_os = "macos")]
impl RuntimeAlias {
    fn new(target: &Path) -> Result<Self, StartError> {
        use std::{
            fs,
            io::Read,
            os::unix::fs::{DirBuilderExt, symlink},
        };
        let mut random = fs::File::open("/dev/urandom").map_err(|_| StartError::Settings)?;
        for _ in 0..16 {
            let mut bytes = [0u8; 12];
            random
                .read_exact(&mut bytes)
                .map_err(|_| StartError::Settings)?;
            let name = bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let directory = Path::new("/private/tmp").join(format!("hgo-{name}"));
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return Err(StartError::Settings),
            }
            let path = directory.join("r");
            if symlink(target, &path).is_err()
                || path.canonicalize().ok().as_deref() != Some(target)
            {
                let _ = fs::remove_file(&path);
                let _ = fs::remove_dir(&directory);
                return Err(StartError::Settings);
            }
            return Ok(Self { directory, path });
        }
        Err(StartError::Settings)
    }
}

#[cfg(target_os = "macos")]
impl Drop for RuntimeAlias {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

impl OwnedOctosSession {
    /// Start with just the selected profile in a temporary room scope. The
    /// caller owns the exclusive scope and must destroy `tmp` after the whole
    /// process tree is proven stopped. This does not itself authorize a pool
    /// dispatch or establish an authenticated usage record.
    pub fn spawn_shared_isolated(
        guardian: &Path,
        executable: &Path,
        scope: &Path,
        profile: ProfileBinding,
        limits: Limits,
    ) -> Result<Self, StartError> {
        Self::spawn_shared_isolated_with_policy(guardian, executable, scope, profile, limits, SharedOctosPolicy::Text)
    }
    pub fn spawn_shared_isolated_with_policy(
        guardian: &Path,
        executable: &Path,
        scope: &Path,
        profile: ProfileBinding,
        limits: Limits,
        policy: SharedOctosPolicy,
    ) -> Result<Self, StartError> {
        Self::spawn_shared_isolated_with_auth(guardian, executable, scope, profile, limits, policy, None)
    }

    /// `auth_home` is chosen by the local host, never by the room command.
    /// Only this profile's provider record is copied into the process scope.
    pub fn spawn_shared_isolated_with_auth(
        guardian: &Path,
        executable: &Path,
        scope: &Path,
        profile: ProfileBinding,
        limits: Limits,
        policy: SharedOctosPolicy,
        auth_home: Option<&Path>,
    ) -> Result<Self, StartError> {
        #[cfg(target_os = "macos")]
        {
            spawn_macos(guardian, executable, scope, profile, limits, policy, auth_home)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (guardian, executable, scope, profile, limits, policy, auth_home);
            Err(StartError::Unsupported)
        }
    }

    /// The retained isolated process is the only source of this handle.
    pub fn shared_process_source(&self) -> Result<super::super::SharedTextProcessSource, Error> {
        self.shared_scope
            .as_ref()
            .map(|scope| scope.source.clone())
            .ok_or(Error::State)
    }

    /// Native-only proof of the exact model and configuration captured for
    /// this retained process. It is separate from the shared profile budget.
    pub fn shared_profile_selection_key(&self, installation_key: &[u8; 32]) -> Result<String, Error> {
        self.shared_scope.as_ref().ok_or(Error::State)?.profile
            .selected_model_key(installation_key).map_err(|_| Error::Identity)
    }

    /// The host's private installation key binds the exact selected profile to
    /// the existing budget identity. Pool custody must already have claimed
    /// this process; the store checks that this digest matches its original
    /// resource before any activity lease is issued. This is not a verified
    /// provider account or a claim about provider quota.
    pub fn qualify_shared_profile(
        &mut self,
        installation_key: &[u8; 32],
    ) -> Result<super::super::SharedTextProcessObservation, Error> {
        let operation = Operation::new(self)?;
        let result = (|| {
            if operation.runner.phase() != Phase::Ready
                || !operation.runner.supports_host_scoped_profile()
            {
                return Err(Error::State);
            }
            let scope = operation.runner.shared_scope.as_ref().ok_or(Error::State)?;
            let digest = scope
                .profile
                .accounting_key(installation_key)
                .map_err(|_| Error::Identity)?;
            if !matches!(
                operation
                    .runner
                    .owner
                    .observe_leader(std::time::Duration::from_millis(100)),
                Ok(true)
            ) {
                return Err(Error::State);
            }
            scope.profile.check().map_err(|_| Error::Identity)?;
            scope
                .source
                .qualify_native(&digest, scope.policy.name(), scope.policy.web(), scope.policy.tools())
                .map_err(|_| Error::Identity)
        })();
        operation.finish(result)
    }

    /// Attach only the retained process's already-qualified, renewable lease.
    pub fn install_renewable_shared_activity(
        &mut self,
        epoch: u64,
        until: std::time::Instant,
    ) -> Result<(), Error> {
        let operation = Operation::new(self)?;
        let result = (|| {
            let source = operation.runner.shared_process_source()?;
            source
                .install_renewable_activity(epoch, until)
                .map_err(|_| Error::State)?;
            let restriction = source.clone();
            let profile = operation.runner.shared_scope.as_ref().ok_or(Error::State)?.profile_guard.clone();
            operation
                .runner
                .session
                .restrict_activity(std::sync::Arc::new(move || {
                    if profile.check().is_err() {
                        return None;
                    }
                    restriction
                        .current_deadline()
                        .map(tokio::time::Instant::from_std)
                }))?;
            operation
                .runner
                .shared_scope
                .as_mut()
                .ok_or(Error::State)?
                .activity_restricted = true;
            Ok(())
        })();
        operation.finish(result)
    }

    pub fn shared_activity_control(
        &self,
    ) -> Result<super::super::SharedTextActivityControl, Error> {
        self.shared_process_source()?
            .activity_control()
            .map_err(|_| Error::State)
    }

    pub fn shared_call_matches(
        &self,
        source: &super::super::SharedTextProcessSource,
        input_digest: &str,
    ) -> bool {
        self.shared_scope.as_ref().is_some_and(|scope| {
            scope.source.same(source)
                && scope.source.is_live()
                && scope.source.matches_input_digest(input_digest)
                && matches!(self.phase(), Phase::Running | Phase::Idle)
        })
    }

    pub fn stop_shared_observation(
        &mut self,
    ) -> Result<super::super::SharedTextProcessObservation, Error> {
        let source = self.shared_process_source()?;
        let stopped = self.stop();
        let all_shells_stopped = self.shared_scope.as_ref().is_some_and(|scope|
            scope.pending_shells.load(std::sync::atomic::Ordering::Acquire) == 0);
        Ok(source.stopped(if all_shells_stopped { stopped } else {
            super::Cleanup::Unknown { kind: std::io::ErrorKind::Other }
        }))
    }
}

#[cfg(target_os = "macos")]
fn spawn_macos(
    guardian: &Path,
    executable: &Path,
    scope: &Path,
    profile: ProfileBinding,
    limits: Limits,
    policy: SharedOctosPolicy,
    auth_home: Option<&Path>,
) -> Result<OwnedOctosSession, StartError> {
    use hagency_platform::Launch;
    use std::{
        collections::BTreeMap,
        fs,
        io::Write,
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };

    if limits.validate().is_err()
        || tokio::runtime::Handle::try_current().is_err()
        || !scope.is_absolute()
        || scope.canonicalize().ok().as_deref() != Some(scope)
    {
        return Err(StartError::Settings);
    }
    let executable = executable
        .canonicalize()
        .map_err(|_| StartError::Settings)?;
    if !executable.is_file() {
        return Err(StartError::Settings);
    }
    let patch_executable = executable.clone();
    let home = scope.join("home");
    let archived_config = scope.join("config");
    let workspace = scope.join("work");
    let temporary = scope.join("tmp");
    for path in [&home, &archived_config, &workspace, &temporary] {
        if !path.is_dir() || path.canonicalize().ok().as_deref() != Some(path) {
            return Err(StartError::Settings);
        }
    }
    profile.check().map_err(|_| StartError::Settings)?;
    let (auth_guard, auth_snapshot) = if let Some(source) = auth_home {
        let (guard, snapshot) = crate::octos::profile_binding::auth::Selection::capture(source, &profile.model().family)
            .map_err(|_| StartError::Settings)?;
        (Some(guard), snapshot)
    } else { (None, None) };
    let profile_guard = std::sync::Arc::new(profile.running_guard_with_auth(auth_guard).map_err(|_| StartError::Settings)?);
    let runtime_alias = RuntimeAlias::new(&temporary)?;
    let data = temporary.join("octos-profile");
    let auth = temporary.join("octos-auth");
    // A fresh instance for each retained process makes session totals cover
    // this process only. Prior conversation context must come from Hagency's
    // authorized room history, never an unaccounted native-session restore.
    let instance = temporary.join("octos-instance");
    let settings = temporary.join("hagency-octos.json");
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(&data).map_err(|_| StartError::Settings)?;
    if builder.create(&instance).is_err() {
        let _ = fs::remove_dir(&data);
        return Err(StartError::Settings);
    }
    if builder.create(&auth).is_err() {
        let _ = fs::remove_dir(&data);
        let _ = fs::remove_dir(&instance);
        return Err(StartError::Settings);
    }
    let mut settings_created = false;
    let prepared = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
            .open(&settings)
            .map_err(|_| StartError::Settings)?;
        settings_created = true;
        file.write_all(crate::octos::CONFIG)
            .and_then(|_| file.sync_all())
            .map_err(|_| StartError::Settings)?;
        let arguments = crate::octos::serve_arguments_scoped(
            workspace.to_str().ok_or(StartError::Settings)?,
            instance.to_str().ok_or(StartError::Settings)?,
            settings.to_str().ok_or(StartError::Settings)?,
            profile.profile_id(),
        )
        .map_err(|_| StartError::Settings)?;
        let mut environment = BTreeMap::new();
        environment.insert("HOME".into(), home.as_os_str().into());
        environment.insert("OCTOS_HOME".into(), data.as_os_str().into());
        environment.insert("XDG_CONFIG_HOME".into(), auth.as_os_str().into());
        environment.insert(
            "XDG_RUNTIME_DIR".into(),
            runtime_alias.path.as_os_str().into(),
        );
        environment.insert("TMPDIR".into(), temporary.as_os_str().into());
        environment.insert("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into());
        let mut launch = Launch {
            executable,
            arguments: arguments.into_iter().map(Into::into).collect(),
            directory: workspace.clone(),
            environment,
            require_crash_containment: false,
        };
        super::super::claude_shared::isolation::confine_octos(&mut launch, scope)?;
        profile
            .snapshot_into_with_ambient(&data, auth_home.is_some())
            .map_err(|_| StartError::Settings)?;
        if let Some(snapshot) = &auth_snapshot {
            crate::octos::profile_binding::auth::snapshot_into(&auth, snapshot).map_err(|_| StartError::Settings)?;
        }
        profile_guard.check().map_err(|_| StartError::Settings)?;
        Ok(launch)
    })();
    let launch = match prepared {
        Ok(launch) => launch,
        Err(error) => {
            let _ = fs::remove_dir_all(&data);
            let _ = fs::remove_dir_all(&instance);
            let _ = fs::remove_dir_all(&auth);
            if settings_created {
                let _ = fs::remove_file(&settings);
            }
            return Err(error);
        }
    };
    // On an uncertain spawn, only the scope owner can determine when it is
    // safe to retire plaintext: never erase or recycle it as proof of stop.
    let mut session = OwnedOctosSession::spawn(guardian, &launch, limits)?;
    session.shared_scope = Some(SharedScope {
        profile,
        profile_guard,
        workspace,
        policy,
        guardian: guardian.to_path_buf(),
        executable: patch_executable,
        pending_shells: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        opened: false,
        tools_restricted: false,
        activity_restricted: false,
        source: super::super::SharedTextProcessSource::new(),
        _runtime_alias: runtime_alias,
    });
    Ok(session)
}
