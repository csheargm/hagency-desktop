//! Provider selection belongs to the authenticated host, never a Matrix command.
use super::super::managed_runtime::{LiveCheck, PreparedLaunch};
use super::*;
use crate::console::{AuthorizedDevice, owner_provider};
use hagency_agent_local::{codex::Profile, runtime::Kind};
use hagency_runtime::{
    claude::account::AccountBinding,
    owned::{OwnedClaudeSession, SharedClaudePolicy},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub(super) enum Launch {
    Codex(PreparedLaunch),
    Claude(ClaudeLaunch),
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
            Kind::Claude,
        )
        .is_ok_and(|p| {
            p.home == self.paths.home
                && p.codex_home == self.paths.codex_home
                && p.credential_ref == self.paths.credential_ref
                && p.shared == self.paths.shared
        })
    }
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
                v.profile.cwd = v.profile.cwd.join(agent);
                hagency_store::private::directory(&v.profile.cwd).map_err(store)?;
                hagency_agent_local::runtime::validate(
                    &v.profile,
                    &v.selection.paths.credential_ref,
                )
                .map_err(|_| failure(409, "shared_execution_launch_unavailable"))?;
                Ok(Self::Claude(v))
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
                let proof = tokio::time::timeout_at(
                    until.into(),
                    manager.verified_resource_provider_account(
                        &v.registry,
                        v.profile_authority.clone(),
                        super::super::ManagementRuntime::Claude,
                        v.resource.clone(),
                        v.revision.clone(),
                    ),
                )
                .await
                .map_err(|_| failure(409, "shared_execution_expired"))??;
                let binding = proof.claude_binding().map_err(store)?.clone();
                let verified = v
                    .registry
                    .verify_provider_resource(
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
                Ok(Self::Claude(v))
            }
        }
    }
}
impl ClaudeLaunch {
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
        digest(&serde_json::json!({"version":1,"executable":self.profile.executable,
            "home":self.profile.home,"config":self.profile.codex_home,"cwd":self.profile.cwd,
            "model":self.profile.model,"effort":self.profile.effort,"policy":self.policy.name(),
            "roomTools":cfg!(feature = "shared-context")}))
    }
    pub fn spawn(&self, until: Instant, resume: Option<&str>) -> Result<OwnedClaudeSession, NativeError> {
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
        for name in ["PATH", "USER", "TMPDIR"] {
            if let Some(value) = std::env::var_os(name) {
                environment.insert(name.into(), value);
            }
        }
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
        let guardian = std::env::current_exe()
            .map_err(|_| failure(409, "shared_execution_launch_unavailable"))?;
        #[cfg(test)]
        let guardian = if self.selection.fixture_mode.is_some() {
            self.profile.executable.clone()
        } else {
            guardian
        };
        OwnedClaudeSession::spawn_shared_resuming(&guardian, &launch, Default::default(), self.policy, resume)
            .map_err(|_| failure(409, "shared_execution_launch_unavailable"))
    }
}
