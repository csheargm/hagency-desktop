//! Provider selection belongs to the authenticated host, never a Matrix command.
use super::super::managed_runtime::{LiveCheck, PreparedLaunch};
use super::*;
use crate::console::{AuthorizedDevice, owner_provider};
use hagency_agent_local::{codex::Profile, runtime::Kind};
use hagency_runtime::{
    claude::account::{AccountBinding, SharedCredential},
    octos::profile_binding::ProfileBinding,
    owned::{OwnedClaudeSession, OwnedOctosSession, SharedClaudePolicy, SharedOctosPolicy},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(super) enum Launch {
    Codex(PreparedLaunch),
    Claude(ClaudeLaunch),
    Octos(OctosLaunch),
}
impl From<PreparedLaunch> for Launch {
    fn from(value: PreparedLaunch) -> Self {
        Self::Codex(value)
    }
}
#[derive(Clone)]
struct Selection {
    root: PathBuf,
    origin: String,
    issuer: String,
    subject: String,
    owner: String,
    paths: owner_provider::ProviderPaths,
    kind: Kind,
    #[cfg(test)]
    fixture_mode: Option<String>,
}
impl Selection {
    fn check(&self) -> bool {
        #[cfg(test)]
        if self.fixture_mode.is_some() {
            return true;
        }
        owner_provider::external::connected_paths(
            &self.root,
            &self.origin,
            &self.issuer,
            &self.subject,
            &self.owner,
            self.kind,
        )
        .is_ok_and(|p| {
            p.home == self.paths.home
                && p.codex_home == self.paths.codex_home
                && p.credential_ref == self.paths.credential_ref
                && p.shared == self.paths.shared
        })
    }
}
pub(super) struct OctosLaunch {
    selection: Arc<Selection>,
    profile_authority: ProfileAuthority,
    registry: OwnerResourceRegistry,
    resource: String,
    revision: String,
    profile_id: String,
    model: String,
    executable: PathBuf,
    base: PathBuf,
    running_guard: Arc<std::sync::OnceLock<OctosRunningGuard>>,
    binding: Option<VerifiedOwnerResourceBinding>,
    workspaces: Option<super::claude_workspace::Workspaces>,
    workspace: Option<super::claude_workspace::Lease>,
    pub policy: SharedOctosPolicy,
    #[cfg(test)]
    fixture_guardian: Option<PathBuf>,
}
pub(super) struct ClaudeLaunch {
    selection: Arc<Selection>,
    profile_authority: ProfileAuthority,
    registry: OwnerResourceRegistry,
    resource: String,
    revision: String,
    pub profile: Profile,
    pub policy: SharedClaudePolicy,
    binding: Option<AccountBinding>,
    credential: Option<SharedCredential>,
    workspaces: Option<super::claude_workspace::Workspaces>,
    workspace: Option<super::claude_workspace::Lease>,
}
impl Launch {
    pub async fn for_resource(
        root: &Path,
        device: &AuthorizedDevice,
        registry: &OwnerResourceRegistry,
        profile: &ProfileAuthority,
        resource: &hagency_store::management::ResourceSummary,
    ) -> Result<Self, NativeError> {
        if resource.framework == "codex" {
            return PreparedLaunch::for_resource(root, device, registry, profile, resource, true)
                .await
                .map(Self::Codex);
        }
        if resource.framework == "octos" {
            let profile_id = registry.octos_profile_for_resource(
                profile.clone(), resource.id.clone(), resource.revision.clone(),
            ).await.map_err(store)?;
            let paths = owner_provider::external::connected_paths(
                root, device.origin(), device.issuer(), device.subject(), device.owner_mxid(), Kind::Octos,
            ).map_err(|e| failure(e.status, e.code))?;
            let base = crate::console::owned_agents::journal_profile_directory(
                root, device.origin(), device.issuer(), device.subject(), device.owner_mxid(),
            ).map_err(|_| failure(409, "shared_execution_launch_unavailable"))?.join("managed-octos");
            hagency_store::private::directory(&base).map_err(store)?;
            let selection = Selection {
                root: root.into(), origin: device.origin().into(), issuer: device.issuer().into(),
                subject: device.subject().into(), owner: device.owner_mxid().into(), paths,
                kind: Kind::Octos,
                #[cfg(test)] fixture_mode: None,
            };
            return Ok(Self::Octos(OctosLaunch {
                selection: Arc::new(selection), profile_authority: profile.clone(),
                registry: registry.clone(), resource: resource.id.clone(), revision: resource.revision.clone(),
                profile_id, model: resource.model.clone(), executable: owner_provider::external::executable(Kind::Octos)
                    .map_err(|e| failure(e.status, e.code))?, base,
                running_guard: Arc::new(std::sync::OnceLock::new()), binding: None,
                workspaces: None, workspace: None, policy: SharedOctosPolicy::Text,
                #[cfg(test)] fixture_guardian: None,
            }));
        }
        if resource.framework != "claude"
            || registry
                .resource_namespace(
                    profile.clone(),
                    resource.id.clone(),
                    resource.revision.clone(),
                )
                .await
                .map_err(store)?
                .is_some()
        {
            return Err(failure(409, "shared_execution_resource_changed"));
        }
        let paths = owner_provider::external::connected_paths(
            root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
            Kind::Claude,
        )
        .map_err(|e| failure(e.status, e.code))?;
        let base = crate::console::owned_agents::journal_profile_directory(
            root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
        )
        .map_err(|_| failure(409, "shared_execution_launch_unavailable"))?
        .join("managed-claude");
        hagency_store::private::directory(&base).map_err(store)?;
        let selected = Selection {
            root: root.into(),
            origin: device.origin().into(),
            issuer: device.issuer().into(),
            subject: device.subject().into(),
            owner: device.owner_mxid().into(),
            paths: paths.clone(),
            kind: Kind::Claude,
            #[cfg(test)]
            fixture_mode: None,
        };
        Ok(Self::Claude(ClaudeLaunch {
            selection: Arc::new(selected),
            profile_authority: profile.clone(),
            registry: registry.clone(),
            resource: resource.id.clone(),
            revision: resource.revision.clone(),
            binding: None,
            credential: None,
            workspaces: None,
            workspace: None,
            profile: Profile {
                executable: owner_provider::external::executable(Kind::Claude)
                    .map_err(|e| failure(e.status, e.code))?,
                home: paths.home,
                codex_home: paths.codex_home,
                cwd: base,
                model: resource.model.clone(),
                effort: resource
                    .reasoning
                    .clone()
                    .unwrap_or_else(|| "medium".into()),
                shared_auth: true,
                web_search: false,
            },
            policy: SharedClaudePolicy::Text,
        }))
    }
    pub fn with_live_web(self, web: bool) -> Result<Self, NativeError> {
        match self {
            Self::Codex(v) => v.with_live_web(web).map(Self::Codex),
            Self::Claude(mut v) => {
                v.policy = SharedClaudePolicy::from_options(web, None).map_err(|_| invalid())?;
                v.profile.web_search = web;
                Ok(Self::Claude(v))
            }
            Self::Octos(mut v) => {
                v.policy = SharedOctosPolicy::from_options(web, None).map_err(|_| invalid())?;
                Ok(Self::Octos(v))
            }
        }
    }
    pub fn with_agent_workspace(self, agent: &str) -> Result<Self, NativeError> {
        self.with_agent_tools(agent, None)
    }
    pub fn with_agent_tools(self, agent: &str, mode: Option<&str>) -> Result<Self, NativeError> {
        match self {
            Self::Codex(v) => v.with_agent_tools(agent, mode).map(Self::Codex),
            Self::Claude(mut v) => {
                super::super::key(agent)?;
                v.policy = SharedClaudePolicy::from_options(v.profile.web_search, mode)
                    .map_err(|_| invalid())?;
                hagency_agent_local::runtime::validate(
                    &v.profile,
                    &v.selection.paths.credential_ref,
                )
                .map_err(|_| failure(409, "shared_execution_launch_unavailable"))?;
                Ok(Self::Claude(v))
            }
            Self::Octos(mut v) => {
                super::super::key(agent)?;
                v.policy = SharedOctosPolicy::from_options(v.policy.web(), mode).map_err(|_| invalid())?;
                Ok(Self::Octos(v))
            }
        }
    }
    pub fn io(&self, io: &Io) -> Io {
        match self {
            Self::Codex(v) => io.with_namespace(v.namespace()),
            Self::Claude(v) => {
                let selection = v.selection.clone();
                io.with_provider_gate(Arc::new(move || selection.check()))
            }
            Self::Octos(v) => {
                let selection = v.selection.clone();
                let guard = v.running_guard.clone();
                io.with_provider_gate(Arc::new(move || selection.check() && guard.get().is_none_or(|g| g.check().is_ok())))
            }
        }
    }
    pub async fn prepare(
        self,
        manager: &NativeManagement,
        until: Instant,
        check: LiveCheck,
    ) -> Result<Self, NativeError> {
        match self {
            Self::Codex(v) => manager
                .owner
                .0
                .managed_runtime
                .prepare(v, until, check)
                .await
                .map(Self::Codex),
            Self::Claude(mut v) => {
                check().await?;
                v.workspaces = Some(manager.owner.0.shared_execution.claude_workspaces.clone());
                #[cfg(test)]
                if v.selection.fixture_mode.is_some() {
                    let proof = VerifiedProviderAccount::from_claude_binding(v.binding()?.clone())
                        .map_err(store)?;
                    let verified = v
                        .registry
                        .verify_provider_resource(
                            v.profile_authority.clone(),
                            v.resource.clone(),
                            proof,
                        )
                        .await
                        .map_err(store)?;
                    if verified.resource_revision() != v.revision {
                        return Err(invalid());
                    }
                    return Ok(Self::Claude(v));
                }
                if !v.selection.check() {
                    return Err(failure(409, "provider_account_changed"));
                }
                let credential = tokio::time::timeout_at(
                    until.into(),
                    manager.shared_claude_credential(&v.selection.paths, until),
                )
                .await
                .map_err(|_| failure(409, "shared_execution_expired"))??;
                let binding = credential.binding().clone();
                let proof =
                    VerifiedProviderAccount::from_claude_binding(binding.clone()).map_err(store)?;
                // Any verified Claude sign-in of the owner may run the pool.
                let verified = v
                    .registry
                    .verify_claude_launch_resource(
                        v.profile_authority.clone(),
                        v.resource.clone(),
                        proof,
                    )
                    .await
                    .map_err(store)?;
                if verified.resource_revision() != v.revision || !v.selection.check() {
                    return Err(failure(409, "provider_account_changed"));
                }
                check().await?;
                v.binding = Some(binding);
                v.credential = Some(credential);
                Ok(Self::Claude(v))
            }
            Self::Octos(mut v) => {
                check().await?;
                if !v.selection.check() { return Err(failure(409, "provider_account_changed")); }
                let expires = until.min(Instant::now() + Duration::from_secs(30));
                let witness = ProfileBinding::observe_model(&v.selection.paths.codex_home, &v.profile_id, &v.model, expires)
                    .map_err(|_| failure(409, "provider_account_changed"))?;
                let observed = v.registry.observe_octos_profile(witness).await.map_err(store)?;
                let binding = v.registry.verify_octos_resource(v.profile_authority.clone(), v.resource.clone(), observed).await.map_err(store)?;
                if binding.resource_revision() != v.revision || !v.selection.check() {
                    return Err(failure(409, "provider_account_changed"));
                }
                let guard = binding.octos_running_guard().map_err(store)?;
                v.running_guard.set(guard).map_err(|_| invalid())?;
                v.binding = Some(binding);
                v.workspaces = Some(manager.owner.0.shared_execution.claude_workspaces.clone());
                check().await?;
                Ok(Self::Octos(v))
            }
        }
    }
}
impl OctosLaunch {
    #[cfg(test)]
    pub(in crate::console::native) fn fixture(
        registry: OwnerResourceRegistry,
        authority: ProfileAuthority,
        resource: &hagency_store::management::ResourceSummary,
        home: PathBuf,
        root: PathBuf,
        profile_id: String,
    ) -> Self {
        let base = root.join("managed-octos");
        hagency_store::private::directory(&base).unwrap();
        let paths = owner_provider::ProviderPaths {
            home: home.clone(), codex_home: home.clone(), shared: true,
            credential_ref: hagency_agent_local::runtime::local_reference(Kind::Octos, &home, None),
        };
        Self {
            selection: Arc::new(Selection {
                root, origin: String::new(), issuer: String::new(), subject: String::new(), owner: String::new(),
                paths, kind: Kind::Octos, fixture_mode: Some("octos".into()),
            }),
            profile_authority: authority, registry, resource: resource.id.clone(), revision: resource.revision.clone(),
            profile_id, model: resource.model.clone(), executable: PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap()), base,
            running_guard: Arc::new(std::sync::OnceLock::new()), binding: None, workspaces: None, workspace: None,
            policy: SharedOctosPolicy::Text,
            fixture_guardian: Some(PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_GUARDIAN").unwrap())),
        }
    }
    pub fn check(&self) -> Result<(), NativeError> {
        if !self.selection.check() || !self.running_guard.get().is_some_and(|g| g.check().is_ok()) {
            return Err(failure(409, "provider_account_changed"));
        }
        Ok(())
    }
    pub fn session_settings_digest(&self) -> Result<String, NativeError> {
        digest(&serde_json::json!({"version":1,"runtime":"octos","executable":self.executable,
            "profile":self.profile_id,"model":self.model,"resource":self.resource,"revision":self.revision,
            "isolation":"host-owned-room-v1","nativeSession":"fresh","policy":self.policy.name()}))
    }
    pub fn bind_scope(&mut self, root: &SharedRuntimeCommand, scope: &str) -> Result<(), NativeError> {
        self.check()?;
        hash(scope)?;
        if self.workspace.is_some() || root.runtime().map_err(store)? != "octos" {
            return Err(invalid());
        }
        // A route selects exactly one conversation; without one the original
        // command digest is a private bootstrap scope.
        self.workspace = Some(self.workspaces.as_ref().ok_or_else(invalid)?.open(
            &self.base, scope, &root.assignment_id, root.assignment_generation,
        )?);
        Ok(())
    }
    pub fn profile_id(&self) -> &str { &self.profile_id }
    pub fn workspace(&self) -> Result<PathBuf, NativeError> {
        Ok(self.workspace.as_ref().ok_or_else(invalid)?.path().join("work"))
    }
    pub fn spawn(&self, until: Instant) -> Result<OwnedOctosSession, NativeError> {
        self.check()?;
        let expires = until.min(Instant::now() + Duration::from_secs(30));
        let witness = ProfileBinding::observe_model(&self.selection.paths.codex_home, &self.profile_id, &self.model, expires)
            .map_err(|_| failure(409, "provider_account_changed"))?;
        let guardian = std::env::current_exe().map_err(|_| failure(409, "shared_execution_launch_unavailable"))?;
        #[cfg(test)]
        let guardian = self.fixture_guardian.clone().unwrap_or(guardian);
        let auth_home = match () {
            #[cfg(test)]
            _ if self.fixture_guardian.is_some() => None,
            _ => Some(hagency_runtime::octos::profile_binding::auth_home_for_owner(&self.selection.paths.home)
                .map_err(|_| failure(409, "shared_execution_launch_unavailable"))?),
        };
        OwnedOctosSession::spawn_shared_isolated_with_auth(
            &guardian, &self.executable, self.workspace.as_ref().ok_or_else(invalid)?.path(),
            witness, hagency_runtime::octos::session::Limits { event_wait_ms: 180_000, ..Default::default() }, self.policy,
            auth_home.as_deref(),
        ).map_err(|_| failure(409, "shared_execution_launch_unavailable"))
    }
    pub fn qualify(&self, process: &mut OwnedOctosSession) -> Result<hagency_runtime::owned::SharedTextProcessObservation, NativeError> {
        self.check()?;
        self.binding.as_ref().ok_or_else(invalid)?.qualify_octos_process(process).map_err(store)
    }
    pub fn bind_admitted_scope(&self, session: &SharedOctosSession) -> Result<(), NativeError> {
        self.workspace.as_ref().ok_or_else(invalid)?.bind_admitted_scope(session.storage_scope())
    }
    pub fn seal_stopped(&self) -> Result<(), NativeError> {
        self.workspace.as_ref().ok_or_else(invalid)?.seal_stopped()
    }
    pub fn retire_plaintext(&self) {
        if let Some(workspace) = &self.workspace { workspace.retire(); }
    }
}
impl ClaudeLaunch {
    pub(super) fn turn_policy_path(&self) -> Result<PathBuf, NativeError> {
        #[cfg(test)]
        if self.selection.fixture_mode.is_some() {
            return Ok(self.selection.root.join("shared-claude-turn-policy.json"));
        }
        crate::console::owned_agents::journal_profile_directory(
            &self.selection.root,
            &self.selection.origin,
            &self.selection.issuer,
            &self.selection.subject,
            &self.selection.owner,
        )
        .map(|root| root.join("shared-claude-turn-policy.json"))
        .map_err(|_| failure(409, "shared_execution_launch_unavailable"))
    }
    #[cfg(test)]
    pub(in crate::console::native) fn fixture(
        registry: OwnerResourceRegistry,
        authority: ProfileAuthority,
        resource: &hagency_store::management::ResourceSummary,
        root: PathBuf,
        binding: AccountBinding,
        mode: &str,
    ) -> Self {
        let binary = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_CLAUDE_RUNTIME_PROBE")
                .expect("explicit offline Claude probe"),
        );
        let workspace = root.join("work");
        hagency_store::private::directory(&workspace).unwrap();
        let paths = owner_provider::ProviderPaths {
            home: root.clone(),
            codex_home: root.clone(),
            shared: true,
            credential_ref: hagency_agent_local::runtime::local_reference(
                Kind::Claude,
                &root,
                None,
            ),
        };
        Self {
            selection: Arc::new(Selection {
                root: root.clone(),
                origin: String::new(),
                issuer: String::new(),
                subject: String::new(),
                owner: String::new(),
                paths,
                kind: Kind::Claude,
                fixture_mode: Some(mode.into()),
            }),
            profile_authority: authority,
            registry,
            resource: resource.id.clone(),
            revision: resource.revision.clone(),
            profile: Profile {
                executable: binary,
                home: root.clone(),
                codex_home: root,
                cwd: workspace,
                model: resource.model.clone(),
                effort: "medium".into(),
                shared_auth: true,
                web_search: false,
            },
            policy: SharedClaudePolicy::Text,
            binding: Some(binding),
            credential: None,
            workspaces: None,
            workspace: None,
        }
    }
    pub fn check(&self) -> Result<(), NativeError> {
        if !self.selection.check() {
            return Err(failure(409, "provider_account_changed"));
        }
        Ok(())
    }
    pub fn binding(&self) -> Result<&AccountBinding, NativeError> {
        self.binding
            .as_ref()
            .ok_or_else(|| failure(409, "provider_identity_unavailable"))
    }
    pub fn session_settings_digest(&self) -> Result<String, NativeError> {
        digest(
            &serde_json::json!({"version":2,"executable":self.profile.executable,
            "isolation":"host-owned-room-v1","resource":self.resource,"revision":self.revision,
            "model":self.profile.model,"effort":self.profile.effort,"policy":self.policy.name(),
            "roomTools":cfg!(feature = "shared-execution")}),
        )
    }
    pub fn bind_session_scope(
        &mut self,
        session: &SharedClaudeSession,
        command: &SharedRuntimeCommand,
    ) -> Result<(), NativeError> {
        if self.workspace.is_some() {
            return Err(invalid());
        }
        let lease = self.workspaces.as_ref().ok_or_else(invalid)?.open(
            &self.profile.cwd,
            session.storage_scope(),
            &command.assignment_id,
            command.assignment_generation,
        )?;
        self.profile.home = lease.path().join("home");
        self.profile.codex_home = lease.path().join("config");
        self.profile.cwd = lease.path().join("work");
        self.workspace = Some(lease);
        Ok(())
    }
    pub fn retire_plaintext(&self) {
        if let Some(scope) = &self.workspace {
            scope.retire();
        }
    }
    pub fn bind_admitted_scope(&self, session: &SharedClaudeSession) -> Result<(), NativeError> {
        self.workspace
            .as_ref()
            .ok_or_else(invalid)?
            .bind_admitted_scope(session.storage_scope())
    }
    pub fn seal_stopped(&self) -> Result<(), NativeError> {
        self.workspace.as_ref().ok_or_else(invalid)?.seal_stopped()
    }
    pub fn spawn(
        &self,
        until: Instant,
        resume: Option<&str>,
    ) -> Result<OwnedClaudeSession, NativeError> {
        self.check()?;
        self.binding()?
            .check()
            .map_err(|_| failure(409, "provider_identity_unavailable"))?;
        if Instant::now() >= until {
            return Err(failure(409, "shared_execution_expired"));
        }
        let mut environment = BTreeMap::new();
        environment.insert("HOME".into(), self.profile.home.as_os_str().into());
        environment.insert(
            "CLAUDE_CONFIG_DIR".into(),
            self.profile.codex_home.as_os_str().into(),
        );
        let workspace = self.workspace.as_ref().ok_or_else(invalid)?;
        environment.insert("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into());
        for name in ["TMPDIR", "CLAUDE_CODE_TMPDIR"] {
            environment.insert(name.into(), workspace.path().join("tmp").into_os_string());
        }
        environment.insert(
            "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC".into(),
            "1".into(),
        );
        for name in [
            "CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS",
            "CLAUDE_CODE_BG_TASKS_REPORT_RUNNING",
        ] {
            environment.insert(name.into(), "1".into());
        }
        #[cfg(test)]
        if let Some(mode) = &self.selection.fixture_mode {
            environment.insert("HAGENCY_OFFLINE_CLAUDE_MODE".into(), mode.into());
        }
        let launch = hagency_platform::Launch {
            executable: self.profile.executable.clone(),
            directory: self.profile.cwd.clone(),
            arguments: self
                .policy
                .arguments_resuming(&self.profile.model, &self.profile.effort, resume)
                .map_err(|_| invalid())?
                .into_iter()
                .map(Into::into)
                .collect(),
            environment,
            require_crash_containment: false,
        };
        let guardian = hagency_platform::helper_executable()
            .map_err(|_| failure(409, "shared_execution_launch_unavailable"))?;
        #[cfg(test)]
        let guardian = if self.selection.fixture_mode.is_some() {
            self.profile.executable.clone()
        } else {
            guardian
        };
        #[cfg(test)]
        if self.selection.fixture_mode.is_some() {
            return OwnedClaudeSession::spawn_shared_resuming(
                &guardian,
                &launch,
                Default::default(),
                self.policy,
                resume,
            )
            .map_err(|_| failure(409, "shared_execution_launch_unavailable"));
        }
        OwnedClaudeSession::spawn_shared_isolated_resuming(
            &guardian,
            launch,
            workspace.path(),
            self.credential.as_ref().ok_or_else(invalid)?,
            Default::default(),
            self.policy,
            resume,
        )
        .map_err(|_| failure(409, "shared_execution_launch_unavailable"))
    }
}
