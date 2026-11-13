//! Closed trusted-host launch selection; shared configuration requires a
//! separately retained metadata inspector and final-process requalification.
use super::*;
use crate::console::{AuthorizedDevice, owner_provider};
use hagency_platform::Launch;
use hagency_runtime::{
    codex::{session::Settings, transport::Limits},
    owned::{
        MANAGED_DISABLED_FEATURES, MANAGED_INSTRUCTIONS, MANAGED_INSTRUCTIONS_FILE, OwnedSession,
    },
};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

pub(in crate::console::native) struct PreparedLaunch {
    guardian: PathBuf,
    launch: Launch,
    instructions: PathBuf,
    discovery_pending: bool,
    web_search: bool,
    tool_mode: Option<String>,
    text_model: Option<(String, String)>,
    namespace: Option<Arc<ResourceNamespace>>,
    #[cfg(test)]
    fixture_prefix: Vec<std::ffi::OsString>,
}
impl PreparedLaunch {
    pub(in crate::console::native) async fn for_resource(
        root: &Path,
        device: &AuthorizedDevice,
        registry: &OwnerResourceRegistry,
        profile: &ProfileAuthority,
        resource: &hagency_store::management::ResourceSummary,
        shared_text: bool,
    ) -> Result<Self, NativeError> {
        Self::for_resource_inner(root, device, registry, profile, resource, shared_text, None).await
    }
    pub(in crate::console::native) async fn for_resource_inner(
        root: &Path,
        device: &AuthorizedDevice,
        registry: &OwnerResourceRegistry,
        profile: &ProfileAuthority,
        resource: &hagency_store::management::ResourceSummary,
        shared_text: bool,
        executables: Option<(PathBuf, PathBuf)>,
    ) -> Result<Self, NativeError> {
        if resource.framework != "codex" {
            return Err(failure(409, "managed_runtime_provider_profile_unavailable"));
        }
        let namespace = registry
            .resource_namespace(
                profile.clone(),
                resource.id.clone(),
                resource.revision.clone(),
            )
            .await
            .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
        if let Some(namespace) = namespace {
            let paths = owner_provider::managed::namespace_paths(namespace.launch())
                .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
            let text = shared_text.then(|| {
                (
                    resource.model.clone(),
                    resource
                        .reasoning
                        .clone()
                        .unwrap_or_else(|| "medium".into()),
                )
            });
            let (guardian, provider) = match executables {
                Some((guardian, provider)) => (guardian, Some(provider)),
                None => (
                    hagency_platform::helper_executable()
                        .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?,
                    None,
                ),
            };
            let mut prepared = Self::from_paths_policy_inner(paths, guardian, text, provider)?;
            namespace
                .check()
                .await
                .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
            prepared.namespace = Some(Arc::new(namespace));
            Ok(prepared)
        } else if shared_text {
            Self::for_shared_text(
                root,
                device,
                resource.model.clone(),
                resource.reasoning.clone(),
            )
        } else {
            Self::for_owner(root, device)
        }
    }
    pub(in crate::console::native) fn namespace(&self) -> Option<Arc<ResourceNamespace>> {
        self.namespace.clone()
    }
    pub(in crate::console::native) fn guard_metadata(&self, check: LiveCheck) -> LiveCheck {
        let Some(namespace) = self.namespace() else {
            return check;
        };
        Arc::new(move || {
            let namespace = namespace.clone();
            let check = check.clone();
            Box::pin(async move {
                check().await?;
                namespace
                    .check()
                    .await
                    .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
                check().await
            })
        })
    }
    pub(in crate::console::native) fn for_owner(
        root: &Path,
        device: &AuthorizedDevice,
    ) -> Result<Self, NativeError> {
        let paths = owner_provider::paths(
            root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
        )
        .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
        Self::from_paths(
            paths,
            hagency_platform::helper_executable()
                .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?,
        )
    }
    pub(in crate::console::native) fn for_shared_text(
        root: &Path,
        device: &AuthorizedDevice,
        model: String,
        reasoning: Option<String>,
    ) -> Result<Self, NativeError> {
        let paths = owner_provider::paths(
            root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
        )
        .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
        let guardian = hagency_platform::helper_executable()
            .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?;
        Self::from_paths_policy(
            paths,
            guardian,
            Some((model, reasoning.unwrap_or_else(|| "medium".into()))),
        )
    }
    fn from_paths(
        paths: owner_provider::ProviderPaths,
        guardian: PathBuf,
    ) -> Result<Self, NativeError> {
        Self::from_paths_policy(paths, guardian, None)
    }
    fn from_paths_policy(
        paths: owner_provider::ProviderPaths,
        guardian: PathBuf,
        text_model: Option<(String, String)>,
    ) -> Result<Self, NativeError> {
        Self::from_paths_policy_inner(paths, guardian, text_model, None)
    }
    fn from_paths_policy_inner(
        paths: owner_provider::ProviderPaths,
        guardian: PathBuf,
        text_model: Option<(String, String)>,
        provider: Option<PathBuf>,
    ) -> Result<Self, NativeError> {
        let (folder, instruction_file, instruction_bytes) = if text_model.is_some() {
            (
                "managed-text",
                hagency_runtime::owned::SHARED_TEXT_INSTRUCTIONS_FILE,
                hagency_runtime::owned::SHARED_TEXT_INSTRUCTIONS,
            )
        } else {
            (
                "managed-qualification",
                MANAGED_INSTRUCTIONS_FILE,
                MANAGED_INSTRUCTIONS,
            )
        };
        owner_provider::selected_reference(&paths.codex_home, paths.shared)
            .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
        let directory = paths.home.join(folder);
        hagency_store::private::directory(&directory)
            .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?;
        if directory.canonicalize().ok().as_ref() != Some(&directory) {
            return Err(failure(409, "managed_runtime_launch_unavailable"));
        }
        let instructions = directory.join(instruction_file);
        if !instructions
            .try_exists()
            .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?
        {
            hagency_store::private::write_new(&instructions, instruction_bytes.as_bytes())
                .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?;
        }
        let mut content = Vec::new();
        hagency_store::private::open(&instructions, false)
            .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?
            .take((instruction_bytes.len() + 1) as u64)
            .read_to_end(&mut content)
            .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?;
        if content != instruction_bytes.as_bytes() {
            return Err(failure(409, "managed_runtime_launch_unavailable"));
        }
        let mut arguments = vec!["app-server".into(), "--listen".into(), "stdio://".into()];
        for setting in [
            if paths.shared {
                "cli_auth_credentials_store=\"auto\""
            } else {
                "cli_auth_credentials_store=\"keyring\""
            },
            "model_provider=\"openai\"",
            "chatgpt_base_url=\"https://chatgpt.com/backend-api\"",
            "instructions=\"\"",
            "notify=[]",
            "web_search=\"disabled\"",
            "project_doc_max_bytes=0",
            "skills.include_instructions=false",
            "skills.bundled.enabled=false",
            "developer_instructions=\"\"",
            "include_apps_instructions=false",
            "include_environment_context=false",
        ] {
            arguments.extend(["-c".into(), setting.into()]);
        }
        arguments.extend([
            "-c".into(),
            format!(
                "model_instructions_file={}",
                serde_json::to_string(&instructions)
                    .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?
            )
            .into(),
        ]);
        for feature in MANAGED_DISABLED_FEATURES {
            arguments.extend(["-c".into(), format!("features.{feature}=false").into()]);
        }
        let mut environment = BTreeMap::new();
        environment.insert(
            "HOME".into(),
            hagency_platform::codex_process_home(&paths.home)
                .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?
                .into_os_string(),
        );
        environment.insert("CODEX_HOME".into(), paths.codex_home.into_os_string());
        environment.insert("PATH".into(), "".into());
        if let Some(value) = std::env::var_os("SystemRoot") {
            environment.insert("SystemRoot".into(), value);
        }
        Ok(Self {
            guardian,
            launch: Launch {
                executable: match provider {
                    Some(executable) => executable,
                    None => owner_provider::executable()
                        .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?,
                },
                arguments,
                directory,
                environment,
                // Preserve ADR-029 observed-supervision policy. Metadata and
                // explicitly enabled text execution use distinct fixed policies;
                // unknown cleanup remains held, without a crash-containment claim.
                require_crash_containment: false,
            },
            instructions,
            // An isolated CODEX_HOME still has provider-version defaults and
            // optional feature entries. Discover every restrictive override
            // before final-process qualification, regardless of credential
            // storage. Namespace isolation does not prove the no-tools policy.
            discovery_pending: true,
            web_search: false,
            tool_mode: None,
            text_model,
            namespace: None,
            #[cfg(test)]
            fixture_prefix: Vec::new(),
        })
    }
    pub(in crate::console::native) async fn discover(
        &self,
        process: &mut OwnedSession,
    ) -> Result<
        hagency_runtime::owned::ManagedPolicyOverrides,
        hagency_runtime::codex::session::Error,
    > {
        if self.text_model.is_some() {
            process
                .discover_shared_tool_policy(
                    &self.instructions,
                    self.web_search,
                    self.tool_mode.as_deref(),
                )
                .await
        } else {
            process.discover_managed_policy(&self.instructions).await
        }
    }
    pub(super) fn discovery_required(&self) -> bool {
        self.discovery_pending
    }
    pub(super) fn apply_discovery(
        &mut self,
        observed: &hagency_runtime::owned::ManagedPolicyOverrides,
    ) -> Result<(), NativeError> {
        for value in observed.values() {
            self.launch.arguments.extend(["-c".into(), value.into()]);
        }
        self.launch
            .validate()
            .map_err(|_| failure(409, "managed_runtime_policy_bounds"))?;
        self.discovery_pending = false;
        Ok(())
    }
    pub(in crate::console::native) fn with_live_web(
        mut self,
        enabled: bool,
    ) -> Result<Self, NativeError> {
        if !enabled {
            return Ok(self);
        }
        if self.text_model.is_none() || !self.discovery_pending {
            return Err(failure(409, "shared_web_policy_unavailable"));
        }
        let path = self
            .launch
            .directory
            .join(hagency_runtime::owned::SHARED_WEB_INSTRUCTIONS_FILE);
        let bytes = hagency_runtime::owned::SHARED_WEB_INSTRUCTIONS.as_bytes();
        if !path
            .try_exists()
            .map_err(|_| failure(409, "shared_web_policy_unavailable"))?
        {
            hagency_store::private::write_new(&path, bytes)
                .map_err(|_| failure(409, "shared_web_policy_unavailable"))?;
        }
        let mut content = Vec::new();
        hagency_store::private::open(&path, false)
            .map_err(|_| failure(409, "shared_web_policy_unavailable"))?
            .take((bytes.len() + 1) as u64)
            .read_to_end(&mut content)
            .map_err(|_| failure(409, "shared_web_policy_unavailable"))?;
        if content != bytes {
            return Err(failure(409, "shared_web_policy_unavailable"));
        }
        self.launch.arguments.extend([
            "-c".into(),
            "web_search=\"live\"".into(),
            "-c".into(),
            "features.standalone_web_search=true".into(),
            "-c".into(),
            "features.code_mode_host=true".into(),
            "-c".into(),
            format!(
                "model_instructions_file={}",
                serde_json::to_string(&path)
                    .map_err(|_| failure(409, "shared_web_policy_unavailable"))?
            )
            .into(),
        ]);
        self.instructions = path;
        self.web_search = true;
        Ok(self)
    }
    pub(in crate::console::native) fn with_agent_workspace(
        mut self,
        agent: &str,
    ) -> Result<Self, NativeError> {
        if agent.is_empty()
            || agent.len() > 128
            || !agent
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(failure(409, "shared_tool_workspace_unavailable"));
        }
        // Process startup uses the Agent root; the admitted work selects a room
        // below it before thread/start. Credentials stay outside both directories.
        let workspaces = self.launch.directory.join("workspaces");
        hagency_store::private::directory(&workspaces)
            .map_err(|_| failure(409, "shared_tool_workspace_unavailable"))?;
        let directory = workspaces.join(agent);
        hagency_store::private::directory(&directory)
            .map_err(|_| failure(409, "shared_tool_workspace_unavailable"))?;
        if directory.canonicalize().ok().as_ref() != Some(&directory) {
            return Err(failure(409, "shared_tool_workspace_unavailable"));
        }
        self.launch.directory = directory;
        Ok(self)
    }
    pub(in crate::console::native) fn with_agent_tools(
        mut self,
        agent: &str,
        mode: Option<&str>,
    ) -> Result<Self, NativeError> {
        let Some(mode) = mode else {
            return self.with_agent_workspace(agent);
        };
        if !matches!(mode, "standard" | "yolo")
            || !self.discovery_pending
            || self.text_model.is_none()
        {
            return Err(failure(409, "shared_tool_policy_unavailable"));
        }
        let path = self
            .launch
            .directory
            .join(hagency_runtime::owned::SHARED_TOOLS_INSTRUCTIONS_FILE);
        let bytes = hagency_runtime::owned::SHARED_TOOLS_INSTRUCTIONS.as_bytes();
        if !path
            .try_exists()
            .map_err(|_| failure(409, "shared_tool_policy_unavailable"))?
        {
            hagency_store::private::write_new(&path, bytes)
                .map_err(|_| failure(409, "shared_tool_policy_unavailable"))?;
        }
        let mut content = Vec::new();
        hagency_store::private::open(&path, false)
            .map_err(|_| failure(409, "shared_tool_policy_unavailable"))?
            .take((bytes.len() + 1) as u64)
            .read_to_end(&mut content)
            .map_err(|_| failure(409, "shared_tool_policy_unavailable"))?;
        if content != bytes {
            return Err(failure(409, "shared_tool_policy_unavailable"));
        }
        // Native tool execution inherits the owner's capabilities and project
        // configuration. The metadata/text-only launch's restrictive overrides
        // must not survive into this separately qualified execution process.
        let shared_auth = self
            .launch
            .arguments
            .iter()
            .any(|arg| arg == "cli_auth_credentials_store=\"auto\"");
        self.launch.arguments =
            hagency_runtime::codex::native_arguments(shared_auth, Some(self.web_search))
                .into_iter()
                .map(Into::into)
                .collect();
        self.launch.arguments.extend([
            "-c".into(),
            "model_provider=\"openai\"".into(),
            "-c".into(),
            "chatgpt_base_url=\"https://chatgpt.com/backend-api\"".into(),
        ]);
        #[cfg(test)]
        self.launch.arguments.splice(0..0, self.fixture_prefix.clone());
        self.launch.environment.insert(
            "PATH".into(),
            std::env::var_os("PATH")
                .filter(|path| !path.is_empty())
                .unwrap_or_else(|| {
                    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin".into()
                }),
        );
        self.instructions = path;
        self.tool_mode = Some(mode.into());
        self.with_agent_workspace(agent)
    }
    pub(in crate::console::native) fn instructions(&self) -> &Path {
        &self.instructions
    }
    pub(in crate::console::native) fn spawn(
        &self,
        deadline: Instant,
    ) -> Result<OwnedSession, NativeError> {
        if let Some(namespace) = &self.namespace {
            namespace
                .launch()
                .check()
                .map_err(|_| failure(409, "managed_runtime_provider_profile_unavailable"))?;
        }
        let remaining = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(30_000) as u64;
        if remaining == 0 {
            return Err(failure(409, "managed_runtime_lease_expired"));
        }
        // Metadata qualification never starts a thread. The separate text path
        // uses only the model/reasoning bound to the canonical resource.
        let settings = Settings::new(
            self.launch.directory.clone(),
            self.text_model
                .as_ref()
                .map_or_else(|| "qualification-only".into(), |v| v.0.clone()),
            self.text_model
                .as_ref()
                .map_or_else(|| "medium".into(), |v| v.1.clone()),
        )
        .map_err(|_| failure(409, "managed_runtime_launch_unavailable"))?;
        let settings = match self.tool_mode.as_deref() {
            Some("yolo") => settings.with_execution_policy(true),
            Some("standard") => settings,
            _ => settings.read_only(),
        };
        OwnedSession::spawn(
            &self.guardian,
            &self.launch,
            settings,
            Limits {
                write_timeout_ms: remaining.min(1000),
                // Account/quota reads are network-backed. Bound all waits by
                // the original pinned lease rather than a new two-second cap.
                event_wait_ms: remaining,
                lifetime_ms: remaining,
            },
            remaining,
        )
        .map_err(|_| failure(409, "managed_runtime_start_outcome_unknown"))
    }
    #[cfg(all(test, feature = "shared-execution"))]
    pub(in crate::console::native) fn text_fixture(
        binary: PathBuf,
        directory: PathBuf,
        mode: &str,
    ) -> Self {
        let mut launch = Self::fixture(binary, directory.clone(), mode);
        launch.instructions = directory.join(hagency_runtime::owned::SHARED_TEXT_INSTRUCTIONS_FILE);
        launch.text_model = Some(("gpt-5.6-sol".into(), "medium".into()));
        launch
    }
    #[cfg(test)]
    pub(in crate::console::native) fn shared_fixture(mut self) -> Self {
        self.discovery_pending = true;
        self
    }
    #[cfg(test)]
    pub(in crate::console::native) fn fixture(
        binary: PathBuf,
        directory: PathBuf,
        mode: &str,
    ) -> Self {
        Self {
            guardian: binary.clone(),
            instructions: directory.join(MANAGED_INSTRUCTIONS_FILE),
            discovery_pending: false,
            web_search: false,
            tool_mode: None,
            text_model: None,
            namespace: None,
            fixture_prefix: vec![mode.into(), directory.as_os_str().into()],
            launch: Launch {
                executable: binary,
                arguments: vec![mode.into(), directory.as_os_str().into()],
                directory,
                environment: BTreeMap::new(),
                require_crash_containment: false,
            },
        }
    }
}

#[cfg(test)]
mod selected_provider_test {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn shared_tools_first_launch_creates_private_workspaces_without_changing_credentials() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let credential = root.join("auth.json");
        hagency_store::private::write_new(&credential, b"unchanged").unwrap();
        let launch = |agent: &str, mode: &str| {
            let mut prepared = PreparedLaunch::fixture("/unused".into(), root.clone(), "unused");
            prepared.discovery_pending = true;
            prepared.text_model = Some(("test".into(), "low".into()));
            prepared.with_agent_tools(agent, Some(mode))
        };
        let first = launch("agent_one", "standard").unwrap();
        let second = launch("agent_two", "yolo").unwrap();
        assert_eq!(first.launch.directory, root.join("workspaces/agent_one"));
        assert_eq!(second.launch.directory, root.join("workspaces/agent_two"));
        if let Some(path) = std::env::var_os("PATH").filter(|path| !path.is_empty()) {
            assert_eq!(
                first.launch.environment.get(std::ffi::OsStr::new("PATH")),
                Some(&path)
            );
        }
        assert_eq!(
            launch("agent_one", "standard").unwrap().launch.directory,
            first.launch.directory
        );
        let readonly = PreparedLaunch::fixture("/unused".into(), root.clone(), "unused")
            .with_agent_tools("agent_one", None)
            .unwrap();
        assert_eq!(readonly.launch.directory, first.launch.directory);
        hagency_store::private::directory(&first.launch.directory).unwrap();
        assert_eq!(std::fs::read(&credential).unwrap(), b"unchanged");
        assert!(launch("../escape", "standard").is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&root, root.join("workspaces/escape")).unwrap();
            assert!(launch("escape", "standard").is_err());
        }
    }
    #[tokio::test]
    #[ignore = "requires explicit owner namespace; performs one real hosted weather search without Matrix delivery"]
    async fn native_shared_live_web_actual_weather_query() {
        use hagency_runtime::codex::session::{ObservationKind, TurnResult};
        let codex_home = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_ISOLATED_PROVIDER_HOME")
                .expect("explicit owner namespace"),
        );
        let guardian = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("supervised guardian"),
        );
        let credential_ref = owner_provider::selected_reference(&codex_home, false).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap().join("weather");
        hagency_store::private::directory(&home).unwrap();
        let launch = PreparedLaunch::from_paths_policy(
            owner_provider::ProviderPaths {
                home,
                codex_home,
                credential_ref,
                shared: false,
            },
            guardian,
            Some(("gpt-6-astra".into(), "low".into())),
        )
        .unwrap()
        .with_live_web(true)
        .unwrap();
        let manager = Manager::default();
        let launch = manager
            .prepare(
                launch,
                Instant::now() + Duration::from_secs(30),
                Arc::new(|| Box::pin(async { Ok(()) })),
            )
            .await
            .unwrap();
        let mut process = launch
            .spawn(Instant::now() + Duration::from_secs(30))
            .unwrap();
        process.initialize().await.unwrap();
        process
            .qualify_shared_text_with_web(launch.instructions(), true)
            .await
            .unwrap();
        let maximum = Instant::now() + Duration::from_secs(120);
        process
            .install_shared_text_activity(1, Instant::now() + Duration::from_secs(25), maximum)
            .unwrap();
        let control = process.shared_text_activity_control().unwrap();
        let renewal = tokio::spawn(async move {
            for epoch in 2..14 {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if control
                    .renew(
                        epoch,
                        (Instant::now() + Duration::from_secs(25)).min(maximum),
                        maximum,
                    )
                    .is_err()
                {
                    break;
                }
            }
        });
        process.start_shared_text_thread().await.unwrap();
        process
            .start_shared_text_turn(
                "北京现在的天气如何？请联网查询，并给出数据时间和来源链接。".into(),
            )
            .await
            .unwrap();
        let mut searches = 0;
        let mut body = String::new();
        loop {
            let event = process.next_shared_text_event().await.unwrap();
            if event.is_authorized_web_search() {
                searches += 1;
            }
            if let Some(delta) = event.delta() {
                body.push_str(delta);
            }
            if let ObservationKind::TurnEnded(result) = event.observation().kind() {
                assert!(matches!(result, TurnResult::Completed));
                break;
            }
        }
        renewal.abort();
        let stopped = process.stop_shared_text_observation();
        manager.drain().await.unwrap();
        assert_eq!(stopped.state(), ManagedProcessState::Stopped);
        assert!(searches >= 2, "no actual hosted search observed: {body}");
        assert!(body.contains("https://"), "no source link: {body}");
        println!(
            "{}",
            serde_json::json!({"hostedSearchEvents":searches,"reply":body,"matrixMessageSent":false})
        );
    }
    #[tokio::test]
    #[ignore = "requires explicit isolated provider namespace and supervised guardian"]
    async fn native_managed_runtime_actual_isolated_namespace_text_policy() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("hagency_runtime::owned::managed=debug")
            .with_ansi(false)
            .try_init();
        let codex_home = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_ISOLATED_PROVIDER_HOME")
                .expect("explicit original managed namespace"),
        );
        let guardian = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("explicit supervised guardian"),
        );
        let credential_ref = owner_provider::selected_reference(&codex_home, false).unwrap();
        let protected = [codex_home.join("config.toml"), codex_home.join("auth.json")];
        // Isolated namespaces use the original keyring entry. Inspect only
        // file existence; never read credential contents for this acceptance.
        let files = || {
            protected
                .iter()
                .map(|path| path.try_exists().unwrap())
                .collect::<Vec<_>>()
        };
        let before = files();
        assert!(before.iter().all(|exists| !exists));
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap().join("qualification");
        hagency_store::private::directory(&home).unwrap();
        let launch = PreparedLaunch::from_paths_policy(
            owner_provider::ProviderPaths {
                home,
                codex_home,
                credential_ref,
                shared: false,
            },
            guardian,
            Some(("gpt-6.1-sol".into(), "low".into())),
        )
        .unwrap();
        assert!(launch.discovery_required());
        let manager = Manager::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        let launch = manager
            .prepare(launch, deadline, Arc::new(|| Box::pin(async { Ok(()) })))
            .await
            .unwrap();
        assert!(!launch.discovery_required());
        let mut process = launch.spawn(deadline).unwrap();
        let result = async {
            process.initialize().await?;
            process.qualify_shared_text(launch.instructions()).await
        }
        .await;
        let stopped = process.stop_shared_text_observation();
        assert_eq!(stopped.state(), ManagedProcessState::Stopped);
        manager.drain().await.unwrap();
        assert_eq!(before, files(), "protected namespace configuration changed");
        let proof = result.expect("actual isolated text-policy qualification");
        assert!(proof.provider_digest().is_some() && proof.policy_digest().is_some());
        eprintln!(
            "isolated-namespace: account-stable=true; no-tools-policy=true; original-inspector-stopped=true; original-qualified-process-stopped=true; protected-inputs-unchanged=true; model-turns=0"
        );
    }
    #[tokio::test]
    #[ignore = "requires explicit provider namespace; metadata only, no model turns"]
    async fn native_shared_tools_actual_isolated_namespace_policy() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("hagency_runtime::owned::managed=debug")
            .with_ansi(false)
            .try_init();
        let codex_home = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_ISOLATED_PROVIDER_HOME")
                .expect("explicit original namespace"),
        );
        let guardian = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("supervised guardian"),
        );
        let credential_ref = owner_provider::selected_reference(&codex_home, false).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap().join("qualification");
        hagency_store::private::directory(&home).unwrap();
        let launch = PreparedLaunch::from_paths_policy(
            owner_provider::ProviderPaths {
                home,
                codex_home,
                credential_ref,
                shared: false,
            },
            guardian,
            Some(("gpt-6-astra".into(), "low".into())),
        )
        .unwrap()
        .with_live_web(true)
        .unwrap()
        .with_agent_tools("diagnostic_agent", Some("yolo"))
        .unwrap();
        let manager = Manager::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        let result = manager
            .prepare(launch, deadline, Arc::new(|| Box::pin(async { Ok(()) })))
            .await;
        let launch = match result {
            Ok(v) => v,
            Err(e) => {
                manager.drain().await.unwrap();
                panic!("preflight failed: {}", e.code)
            }
        };
        let mut process = launch.spawn(deadline).unwrap();
        process.initialize().await.unwrap();
        let result = process
            .qualify_shared_tools(launch.instructions(), true, Some("yolo"))
            .await;
        let stopped = process.stop_shared_text_observation();
        manager.drain().await.unwrap();
        assert_eq!(stopped.state(), ManagedProcessState::Stopped);
        result.unwrap();
        eprintln!("actual live-web YOLO policy qualified; model-turns=0");
    }
    // Explicit local metadata-only acceptance. No NativeOwner/profile opener,
    // credential copy, configuration write, thread, prompt, or tool invocation.
    #[tokio::test]
    #[ignore = "requires explicit selected profile and supervised guardian"]
    async fn native_managed_runtime_actual_selected_shared_provider() {
        let _ = tracing_subscriber::fmt()
            .with_env_filter("hagency_runtime::owned::managed=debug")
            .with_ansi(false)
            .try_init();
        let choice = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_SELECTED_PROVIDER_CHOICE").expect("explicit selection"),
        );
        let guardian = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("explicit guardian"),
        );
        let mut bytes = Vec::new();
        hagency_store::private::open(&choice, false)
            .unwrap()
            .take(8193)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(bytes.len() <= 8192);
        let selected: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(selected.as_object().unwrap().len(), 2);
        assert_eq!(selected["version"], 1);
        let codex_home = PathBuf::from(selected["home"].as_str().unwrap());
        let credential_ref = owner_provider::selected_reference(&codex_home, true).unwrap();
        let protected = [
            choice,
            codex_home.join("config.toml"),
            codex_home.join("auth.json"),
        ];
        let hashes = || {
            protected
                .iter()
                .map(|p| match std::fs::read(p) {
                    Ok(v) => Some(Sha256::digest(v).to_vec()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(_) => panic!("protected input cannot be observed"),
                })
                .collect::<Vec<_>>()
        };
        let before = hashes();
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap().join("home");
        hagency_store::private::directory(&home).unwrap();
        let launch = PreparedLaunch::from_paths(
            owner_provider::ProviderPaths {
                home,
                codex_home,
                credential_ref,
                shared: true,
            },
            guardian,
        )
        .unwrap();
        assert!(
            !launch.launch.require_crash_containment,
            "metadata launch must retain the existing observed-supervisor contract"
        );
        let manager = Manager::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        let check: LiveCheck = Arc::new(|| Box::pin(async { Ok(()) }));
        let result = manager.prepare(launch, deadline, check).await;
        let qualification = match result {
            Ok(launch) => {
                let mut process = launch.spawn(deadline).unwrap();
                let observed = async {
                    process.initialize().await?;
                    process
                        .qualify_managed_no_tools(launch.instructions())
                        .await
                }
                .await;
                let stopped = process.stop_managed_observation();
                assert_eq!(stopped.state(), ManagedProcessState::Stopped);
                observed
                    .map(|proof| {
                        (
                            proof.provider_digest().is_some(),
                            proof.policy_digest().is_some(),
                        )
                    })
                    .map_err(|error| {
                        eprintln!("selected-provider final qualification category={error:?}");
                        "actual_same_process_qualification_failed"
                    })
            }
            Err(e) => {
                eprintln!("selected-provider metadata inspection status={}", e.code);
                Err("actual_metadata_inspection_failed")
            }
        };
        let drained = manager.drain().await;
        assert_eq!(before, hashes(), "protected profile/configuration changed");
        drained.unwrap();
        assert_eq!(qualification, Ok((true, true)));
        eprintln!(
            "selected-provider: shared=true; account-stable=true; no-tools-policy=true; original-inspector-stopped=true; original-qualified-process-stopped=true; protected-inputs-unchanged=true; model-turns=0; production-crash-containment-qualified=false"
        );
    }
}
