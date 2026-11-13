//! The selected original namespace, never the profile-default provider home.
use super::*;
use hagency_store::ManagedLaunch;

/// SIGKILL normally reaps within milliseconds. The read-only probes report a
/// child not observed exiting in time instead of waiting on it: `TrackedChild`
/// keeps its reap custody, which the provider drain still observes.
const PROBE_STOP_TIMEOUT: Duration = Duration::from_secs(10);

async fn stop_probe(process: &mut Process) -> Result<(), ProviderError> {
    match tokio::time::timeout(PROBE_STOP_TIMEOUT, process.child.kill()).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(failure(409, "provider_stop_unknown")),
    }
}

pub(in crate::console) struct NamespaceLogin {
    process: Process,
    paths: ProviderPaths,
}
pub(in crate::console) struct LoginState {
    pub authenticated: bool,
    pub waiting: bool,
    pub authorization_url: Option<String>,
}
impl NamespaceLogin {
    pub async fn start(
        launch: &ManagedLaunch,
        tasks: &TaskCustody,
    ) -> Result<(Self, LoginState), ProviderError> {
        Self::start_with(launch, tasks, &executable()?).await
    }
    pub(in crate::console) async fn start_with(
        launch: &ManagedLaunch,
        tasks: &TaskCustody,
        executable: &Path,
    ) -> Result<(Self, LoginState), ProviderError> {
        let paths = namespace_paths(launch)?;
        launch
            .check()
            .map_err(|_| failure(409, "provider_namespace_unavailable"))?;
        let process = Process::spawn_inner(&paths, executable, true, tasks).await?;
        let mut login = Self { process, paths };
        launch
            .check()
            .map_err(|_| failure(409, "provider_namespace_unavailable"))?;
        let status = login.process.start(&login.paths).await?;
        Ok((login, Self::project(status)))
    }
    fn project(status: ProviderStatus) -> LoginState {
        LoginState {
            authenticated: status.authenticated,
            waiting: status.state == "signing_in",
            authorization_url: status.auth_url,
        }
    }
    pub async fn observe(&mut self) -> Result<LoginState, ProviderError> {
        self.process.status(&self.paths).await.map(Self::project)
    }
    pub async fn stop(&mut self) -> Result<(), ProviderError> {
        // Stop pending authorization; do not log out a completed provider login.
        let _ = self.process.cancel().await;
        self.process
            .child
            .kill()
            .await
            .map_err(|_| failure(409, "provider_stop_unknown"))
    }
}

pub(in crate::console) fn namespace_paths(launch: &ManagedLaunch) -> Result<ProviderPaths, ProviderError> {
    let mut environment = BTreeMap::new();
    launch
        .apply_codex_environment(&mut environment)
        .map_err(|_| failure(409, "provider_namespace_unavailable"))?;
    let home = environment
        .get(std::ffi::OsStr::new("CODEX_HOME"))
        .ok_or(failure(409, "provider_namespace_unavailable"))?;
    let home = PathBuf::from(home);
    Ok(ProviderPaths {
        home: home.clone(),
        codex_home: home.clone(),
        credential_ref: selected_reference(&home, false)?,
        shared: false,
    })
}

/// Short-lived metadata child: no login/start, logout, thread or turn RPCs.
pub(in crate::console) async fn inspect_models(
    launch: &ManagedLaunch,
    provider: &OwnerProvider,
    executable: &Path,
) -> Result<ProviderStatus, ProviderError> {
    let tasks = &provider.tasks;
    let _activity = tasks
        .enter()
        .map_err(|_| failure(409, "provider_closing"))?;
    let paths = namespace_paths(launch)?;
    launch
        .check()
        .map_err(|_| failure(409, "provider_namespace_unavailable"))?;
    let mut process = Process::spawn_inner(&paths, executable, true, tasks).await?;
    let result = tokio::time::timeout(Duration::from_secs(25), async {
        launch
            .check()
            .map_err(|_| failure(409, "provider_namespace_unavailable"))?;
        let result = identity::inspect_models(&mut process, &paths).await?;
        launch
            .check()
            .map_err(|_| failure(409, "provider_namespace_unavailable"))?;
        Ok(result)
    })
    .await
    .map_err(|_| failure(504, "provider_catalog_timeout"))
    .and_then(|v| v);
    // Do not return a successful snapshot without observing child cleanup.
    stop_probe(&mut process).await?;
    result
}

/// Read fresh catalog and quota metadata, then observe child termination before
/// returning proof. This never initiates authorization or a model turn.
pub(in crate::console) async fn inspect_resource(
    launch: &ManagedLaunch,
    provider: &OwnerProvider,
    executable: &Path,
    deadline: std::time::Instant,
) -> Result<(hagency_store::owner_resources::VerifiedProviderAccount, ProviderStatus), ProviderError> {
    let tasks = &provider.tasks;
    let _activity = tasks.enter().map_err(|_| failure(409, "provider_closing"))?;
    if deadline <= std::time::Instant::now() { return Err(failure(409,"provider_identity_unavailable")); }
    let paths = namespace_paths(launch)?;
    launch.check().map_err(|_| failure(409,"provider_namespace_unavailable"))?;
    let mut process = Process::spawn_inner(&paths, executable, true, tasks).await?;
    let remaining = deadline.saturating_duration_since(std::time::Instant::now()).min(Duration::from_secs(25));
    let result = tokio::time::timeout(remaining, async {
        launch.check().map_err(|_| failure(409,"provider_namespace_unavailable"))?;
        let result = identity::inspect_resource(&mut process, &paths, deadline).await?;
        launch.check().map_err(|_| failure(409,"provider_namespace_unavailable"))?;
        Ok(result)
    }).await.map_err(|_| failure(504,"provider_identity_timeout")).and_then(|v|v);
    stop_probe(&mut process).await?;
    if deadline <= std::time::Instant::now() { return Err(failure(409,"provider_identity_unavailable")); }
    result
}

/// Read current plan/quota in the attested resource namespace, without login
/// or a model turn. Keep child custody until termination is observed.
#[cfg(any(unix, test))]
pub(in crate::console) async fn inspect_quota(
    launch: &ManagedLaunch, provider: &OwnerProvider, executable: &Path,
    deadline: std::time::Instant,
) -> Result<(identity::ObservedQuota, hagency_store::owner_resources::VerifiedProviderAccount), ProviderError> {
    let paths=namespace_paths(launch)?;
    launch.check().map_err(|_|failure(409,"provider_namespace_unavailable"))?;
    let result=inspect_quota_inner(&paths,provider,executable,deadline,
        || launch.check().map_err(|_|failure(409,"provider_namespace_unavailable"))).await;
    launch.check().map_err(|_|failure(409,"provider_namespace_unavailable"))?;
    result
}

/// The caller supplies the original authenticated profile selection and checks
/// it again after this probe. This function never discovers an ambient home.
#[cfg(any(unix, test))]
pub(in crate::console) async fn inspect_original_quota(
    paths: &ProviderPaths, provider: &OwnerProvider, executable: &Path,
    deadline: std::time::Instant,
) -> Result<(identity::ObservedQuota, hagency_store::owner_resources::VerifiedProviderAccount), ProviderError> {
    inspect_quota_inner(paths,provider,executable,deadline,
        || selected_reference(&paths.codex_home,paths.shared).map(|_|())).await
}

#[cfg(any(unix, test))]
async fn inspect_quota_inner(
    paths: &ProviderPaths, provider: &OwnerProvider, executable: &Path,
    deadline: std::time::Instant,
    check: impl Fn() -> Result<(),ProviderError>,
) -> Result<(identity::ObservedQuota, hagency_store::owner_resources::VerifiedProviderAccount), ProviderError> {
    let tasks=&provider.tasks;
    let _activity=tasks.enter().map_err(|_|failure(409,"provider_closing"))?;
    if deadline<=std::time::Instant::now() {return Err(failure(409,"provider_identity_unavailable"));}
    selected_reference(&paths.codex_home,paths.shared)?;
    let remaining=deadline.saturating_duration_since(std::time::Instant::now()).min(Duration::from_secs(25));
    let mut process=tokio::time::timeout(remaining,Process::spawn_inner(paths,executable,true,tasks))
        .await.map_err(|_|failure(504,"provider_identity_timeout"))??;
    let remaining=deadline.saturating_duration_since(std::time::Instant::now()).min(Duration::from_secs(25));
    let result=tokio::time::timeout(remaining,async {
        check()?;
        let report=identity::observe_bound_quota(&mut process,paths,deadline).await?;
        check()?;
        Ok(report)
    })
        .await.map_err(|_|failure(504,"provider_identity_timeout")).and_then(|v|v);
    stop_probe(&mut process).await?;
    selected_reference(&paths.codex_home,paths.shared)?;
    if deadline<=std::time::Instant::now() {return Err(failure(409,"provider_identity_unavailable"));}
    result
}
