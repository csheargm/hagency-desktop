//! Explicit owner-started, chat-only Codex host. No legacy execution fallback.
mod drain;
pub(in crate::console) mod e2ee;
mod provider_custody;
pub(in crate::console) mod questions;
mod room_tools;
pub(in crate::console) mod run_controls;
mod service;
#[cfg(unix)]
mod working_directory;
use super::lifecycle::{RetainedTask, TaskCustody};
use super::{
    AuthorizedDevice, Console,
    device_execution::{
        DeviceOperation as Op, DeviceResponse as Response, HistorySnapshot, Lease, LeaseRef,
        Outcome, Takeover,
    },
    owned_agents::{ledger_path, profile_identity},
    server_login::OwnerOperation,
};
use hagency_agent_local::{
    Ledger, Scope,
    codex::{self, BudgetMode, Profile},
    inbox::{self, Finish, State},
    runtime,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::{Mutex, mpsc, watch};

/// Construct only from an authenticated owner's explicit start action. Paths
/// select a dedicated provider login; credential_ref is metadata, NOT a resolver.
#[derive(Clone)]
pub(super) struct StartConfig {
    pub agent_id: String,
    pub binding_id: String,
    pub profile: Profile,
    pub credential_ref: String,
    pub takeover: Takeover,
    pub host_files: bool,
    pub shared_workspace: bool,
    pub shared_workspace_pin: Option<String>,
    // Overwritten from authenticated server binding metadata, never a UI option.
    pub owner_direct: bool,
    pub recovery_device_id: Option<String>,
    pub recovery_consent_id: Option<String>,
}
/// Explicit startup consent only. Never contains credentials, approvals, paths,
/// execution IDs or a lease; recovery re-enters all current startup gates.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RuntimeIntent {
    pub agent_id: String,
    pub binding_id: String,
    pub device_id: String,
    pub consent_id: String,
    pub effort: String,
    pub host_files: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shared_workspace: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_workspace_pin: Option<String>,
}
impl RuntimeIntent {
    pub(super) fn start_input(&self) -> serde_json::Value {
        let mut input = serde_json::json!({"bindingId":self.binding_id,
            "effort":self.effort,"hostFiles":self.host_files,
            "takeover":false,"recoveryDeviceId":self.device_id,"recoveryConsentId":self.consent_id});
        if self.shared_workspace { input["sharedWorkspace"] = true.into(); }
        input
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeIntents {
    version: u8,
    profile_identity: String,
    intents: Vec<RuntimeIntent>,
}
const RUNTIME_STATE_VERSION: u8 = 3;

/// A clean cutover never revives startup consent or preferences from an old
/// format. The next explicit start/stop writes a fresh current document.
fn current_runtime_state(bytes: &[u8], identity: &str) -> Result<Option<serde_json::Value>, RuntimeError> {
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| RuntimeError::Profile)?;
    if value.get("profileIdentity").and_then(|v| v.as_str()) != Some(identity) {
        return Err(RuntimeError::Profile);
    }
    let version = value.get("version").and_then(|v| v.as_u64()).ok_or(RuntimeError::Profile)?;
    if version < u64::from(RUNTIME_STATE_VERSION) { return Ok(None); }
    if version != u64::from(RUNTIME_STATE_VERSION) { return Err(RuntimeError::Profile); }
    Ok(Some(value))
}
fn intent_location(
    console: &Console,
    device: &AuthorizedDevice,
) -> Result<(PathBuf, String), RuntimeError> {
    let root = console
        .0
        .server_login
        .state_directory()
        .ok_or(RuntimeError::Profile)?;
    let path = ledger_path(
        &root,
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .map_err(|_| RuntimeError::Profile)?;
    let identity = profile_identity(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .map_err(|_| RuntimeError::Authorization)?;
    Ok((path.with_file_name("runtime-intents.json"), identity))
}
fn read_intents(path: &std::path::Path, identity: &str) -> Result<RuntimeIntents, RuntimeError> {
    use std::io::Read;
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(_) if !path.exists() => {
            return Ok(RuntimeIntents {
                version: RUNTIME_STATE_VERSION,
                profile_identity: identity.into(),
                intents: Vec::new(),
            });
        }
        Err(_) => return Err(RuntimeError::Profile),
    };
    let mut bytes = Vec::new();
    file.take(262145)
        .read_to_end(&mut bytes)
        .map_err(|_| RuntimeError::Profile)?;
    if bytes.len() > 262144 {
        return Err(RuntimeError::Profile);
    }
    let Some(value) = current_runtime_state(&bytes, identity)? else {
        return Ok(RuntimeIntents { version: RUNTIME_STATE_VERSION, profile_identity: identity.into(), intents: Vec::new() });
    };
    let values: RuntimeIntents = serde_json::from_value(value).map_err(|_| RuntimeError::Profile)?;
    let mut scopes = BTreeSet::new();
    if values.version != RUNTIME_STATE_VERSION
        || values.profile_identity != identity
        || values.intents.len() > 1024
        || values.intents.iter().any(|v| {
            v.agent_id.is_empty()
                || v.binding_id.is_empty()
                || v.device_id.is_empty()
                || match (v.shared_workspace, &v.shared_workspace_pin) {
                    (true, Some(pin)) => pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()),
                    (false, None) => false,
                    _ => true,
                }
                || (v.consent_id.len() != 64
                    || !v.consent_id.bytes().all(|b| b.is_ascii_hexdigit()))
                || ![
                    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
                ]
                .contains(&v.effort.as_str())
                || !scopes.insert((&v.agent_id, &v.binding_id))
        })
    {
        return Err(RuntimeError::Profile);
    }
    Ok(values)
}
fn intents_for_device(values: RuntimeIntents, device: &str) -> Vec<RuntimeIntent> {
    let agent_services: BTreeSet<_> = values
        .intents
        .iter()
        .filter(|v| v.device_id == device && v.binding_id == service::ALL_ROOMS)
        .map(|v| v.agent_id.clone())
        .collect();
    values
        .intents
        .into_iter()
        .filter(|v| {
            v.device_id == device
                && (v.binding_id == service::ALL_ROOMS || !agent_services.contains(&v.agent_id))
        })
        .collect()
}
fn change_intent(
    path: &std::path::Path,
    identity: &str,
    agent: &str,
    binding: &str,
    intent: Option<RuntimeIntent>,
) -> Result<(), RuntimeError> {
    change_intent_checked(path, identity, agent, binding, intent, None)
}
fn change_intent_checked(
    path: &std::path::Path,
    identity: &str,
    agent: &str,
    binding: &str,
    intent: Option<RuntimeIntent>,
    expected_consent: Option<&str>,
) -> Result<(), RuntimeError> {
    let lock_path = path.with_file_name("runtime-intents.lock");
    let lock = hagency_store::private::open(&lock_path, true)
        .or_else(|_| hagency_store::private::open(&lock_path, false))
        .map_err(|_| RuntimeError::Profile)?;
    lock.try_lock().map_err(|_| RuntimeError::Profile)?;
    let mut values = read_intents(path, identity)?;
    // This comparison and publication share the journal lock with Stop, even
    // when a different manager read its recovery snapshot before the deletion.
    if let Some(consent) = expected_consent {
        let replacement = intent.as_ref().ok_or(RuntimeError::Stopped)?;
        if replacement.consent_id != consent || !values.intents.iter().any(|v| v == replacement) {
            return Err(RuntimeError::Stopped);
        }
    }
    values
        .intents
        .retain(|v| v.agent_id != agent || v.binding_id != binding);
    if let Some(intent) = intent {
        values.intents.push(intent);
    }
    let bytes = serde_json::to_vec(&values).map_err(|_| RuntimeError::Profile)?;
    if bytes.len() > 262144 || values.intents.len() > 1024 {
        return Err(RuntimeError::Profile);
    }
    hagency_store::private::replace(path, &bytes).map_err(|_| RuntimeError::Profile)
}
fn verify_recovery_intent(
    console: &Console,
    device: &AuthorizedDevice,
    config: &StartConfig,
) -> Result<(), RuntimeError> {
    match (&config.recovery_device_id, &config.recovery_consent_id) {
        (None, None) => Ok(()),
        (Some(expected_device), Some(consent)) if expected_device == device.device_id() => {
            let (path, identity) = intent_location(console, device)?;

            let valid = read_intents(&path, &identity)?.intents.iter().any(|v| {
                v.agent_id == config.agent_id
                    && v.binding_id == config.binding_id
                    && v.device_id == *expected_device
                    && v.consent_id == *consent
                    && v.effort == config.profile.effort
                    && v.host_files == config.host_files
                    && v.shared_workspace == config.shared_workspace
                    && v.shared_workspace_pin == config.shared_workspace_pin
            });
            if valid {
                Ok(())
            } else {
                Err(RuntimeError::Stopped)
            }
        }
        _ => Err(RuntimeError::Authorization),
    }
}
fn shared_workspace_pin(path: &std::path::Path) -> Result<String, RuntimeError> {
    #[cfg(unix)]
    {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::MetadataExt;
        if !path.is_absolute() || path.canonicalize().ok().as_deref() != Some(path) { return Err(RuntimeError::Profile); }
        let metadata = std::fs::metadata(path).map_err(|_| RuntimeError::Profile)?;
        if !metadata.is_dir() { return Err(RuntimeError::Profile); }
        let value = serde_json::json!({"path":path,"device":metadata.dev(),"inode":metadata.ino()});
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value).map_err(|_| RuntimeError::Profile)?)))
    }
    #[cfg(not(unix))]
    { let _ = path; Err(RuntimeError::Profile) }
}
fn new_consent_id() -> Result<String, RuntimeError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| RuntimeError::Profile)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn save_intent(
    console: &Console,
    device: &AuthorizedDevice,
    config: &StartConfig,
) -> Result<(), RuntimeError> {

    let (path, identity) = intent_location(console, device)?;
    change_intent_checked(
        &path,
        &identity,
        &config.agent_id,
        &config.binding_id,
        Some(RuntimeIntent {
            agent_id: config.agent_id.clone(),
            binding_id: config.binding_id.clone(),
            device_id: device.device_id().into(),
            consent_id: match &config.recovery_consent_id {
                Some(id) => id.clone(),
                None => new_consent_id()?,
            },
            effort: config.profile.effort.clone(),
            host_files: config.host_files,
            shared_workspace: config.shared_workspace,
            shared_workspace_pin: config.shared_workspace_pin.clone(),
        }),
        config.recovery_consent_id.as_deref(),
    )
}
#[derive(Debug, thiserror::Error)]
pub(super) enum RuntimeError {
    #[error("owner authorization required")]
    Authorization,
    #[error("Agent runtime already active")]
    AlreadyActive,
    #[error("the previous Agent lease has not expired")]
    LeaseBusy,
    #[error("Agent execution device is assigned to another device or is unset")]
    ExecutionDevice,
    #[error("provider profile or local state unavailable")]
    Profile,
    #[error("shared provider account or environment unavailable")]
    Provider,
    #[error("provider execution unavailable: {0}")]
    Execution(&'static str),
    #[error("device transport unavailable")]
    Transport,
    #[error("the current request is no longer executable")]
    Request,
    #[error("invalid or expired tool approval")]
    Approval,
    #[error("complete owner ledger recovery required")]
    LedgerRecovery,
    #[error("runtime stopped")]
    Stopped,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RuntimeStatus {
    pub agent_id: String,
    pub binding_id: Option<String>,
    pub bindings: Vec<RuntimeStatus>,
    pub phase: &'static str,
    pub last_error: Option<&'static str>,
    pub native_tools: bool,
    pub strict_token_cap: bool,
    pub host_file_tools: bool,
    pub file_replace: bool,
    pub network_tools: bool,
    pub web_search: bool,
    pub capability_version: Option<&'static str>,
}
impl RuntimeStatus {
    fn with_capabilities(self, config: &StartConfig) -> Self {
        let mut status = self.with_host_files(config.host_files || config.shared_workspace);
        status.file_replace = config.shared_workspace;
        if config.shared_workspace { status.capability_version = Some("hagency-shared-files-v1"); }
        // A shared project is changed only through the host file tools: that
        // session runs without native execution.
        status.native_tools = !config.shared_workspace
            && matches!(
                runtime::kind(&config.credential_ref),
                runtime::Kind::Codex | runtime::Kind::Claude
            );
        status.network_tools = status.native_tools;
        status.web_search = config.profile.web_search
            && runtime::kind(&config.credential_ref) == runtime::Kind::Codex;
        status
    }
    fn with_host_files(mut self, enabled: bool) -> Self {
        self.host_file_tools = enabled;
        if enabled {
            self.capability_version = Some("hagency-room-files-v1");
        }
        self
    }
    fn new(agent: &str, phase: &'static str, error: Option<&'static str>) -> Self {
        Self {
            agent_id: agent.into(),
            binding_id: None,
            bindings: vec![],
            phase,
            last_error: error,
            native_tools: false,
            strict_token_cap: false,
            host_file_tools: false,
            file_replace: false,
            network_tools: false,
            web_search: false,
            capability_version: None,
        }
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ApprovalView {
    pub proposal_id: String,
    pub args_digest: String,
    pub proposal: hagency_agent_local::ToolProposal,
}
type DevicePin = super::device_execution::DeviceIdentity;
struct ApprovalSlot {
    view: ApprovalView,
    pending: codex::PendingApproval,
    device: DevicePin,
}
struct BindingHandle {
    cancel: watch::Sender<bool>,
    status: watch::Receiver<RuntimeStatus>,
}
struct BindingWork {
    config: StartConfig,
    cancel: watch::Sender<bool>,
    receiver: watch::Receiver<bool>,
    status: watch::Sender<RuntimeStatus>,
}
fn binding_work(config: StartConfig) -> (BindingWork, BindingHandle, RuntimeStatus) {
    let (cancel, receiver) = watch::channel(false);
    let mut status =
        RuntimeStatus::new(&config.agent_id, "checking_provider", None).with_capabilities(&config);
    status.binding_id = Some(config.binding_id.clone());
    let (sender, rx) = watch::channel(status.clone());
    (
        BindingWork {
            config,
            cancel: cancel.clone(),
            receiver,
            status: sender,
        },
        BindingHandle { cancel, status: rx },
        status,
    )
}
struct Entry {
    commands: mpsc::UnboundedSender<BindingWork>,
    bindings: BTreeMap<String, BindingHandle>,
    cancel: watch::Sender<bool>,
    status: watch::Receiver<RuntimeStatus>,
    task: RetainedTask,
    device: DevicePin,
    approvals: BTreeMap<String, ApprovalSlot>,
}
impl Entry {
    fn binding_active(&self, binding: &str) -> bool {
        self.bindings
            .get(binding)
            .is_some_and(|b| !*b.cancel.borrow() && b.status.borrow().phase != "stopped")
    }
    fn take_approval(
        &mut self,
        pin: &DevicePin,
        agent: &str,
        id: &str,
        digest: &str,
        bindings: &[Binding],
        at: i64,
    ) -> Result<codex::PendingApproval, RuntimeError> {
        if *self.cancel.borrow() || self.task.is_finished() || &self.device != pin {
            return Err(RuntimeError::Authorization);
        }
        let slot = self.approvals.get(id).ok_or(RuntimeError::Approval)?;
        let proposal = &slot.view.proposal;
        if slot.view.args_digest != digest
            || &slot.device != pin
            || proposal.expires <= at
            || slot.pending.is_closed()
            || proposal.scope.agent != agent
            || !self.binding_active(&proposal.scope.binding)
            || !bindings.iter().any(|b| {
                b.id == proposal.scope.binding
                    && b.room_id == proposal.scope.room
                    && b.agent_id == agent
                    && b.state == "active"
            })
        {
            return Err(RuntimeError::Approval);
        }
        Ok(self
            .approvals
            .remove(id)
            .ok_or(RuntimeError::Approval)?
            .pending)
    }
}
struct AbortHeartbeat(tokio::task::AbortHandle);
impl Drop for AbortHeartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}
/// The server lease alone does not distinguish two processes sharing the same
/// local device credentials. Hold a per-Agent OS lock for the entire job.
struct AgentLock {
    _file: std::fs::File,
}
fn lock_agent(path: &std::path::Path, agent: &str) -> Result<AgentLock, RuntimeError> {
    use sha2::{Digest, Sha256};
    let name = format!("runtime_{:x}.lock", Sha256::digest(agent.as_bytes()));
    let path = path.parent().ok_or(RuntimeError::Profile)?.join(name);
    let file = hagency_store::private::open(&path, true)
        .or_else(|_| hagency_store::private::open(&path, false))
        .map_err(|_| RuntimeError::Profile)?;
    file.try_lock().map_err(|_| RuntimeError::AlreadyActive)?;
    Ok(AgentLock { _file: file })
}
#[derive(Default, Clone)]
pub(super) struct OwnedRuntime {
    tasks: TaskCustody,
    entries: Arc<Mutex<BTreeMap<String, Entry>>>,
    services: Arc<Mutex<BTreeMap<String, service::ServiceEntry>>>,
}
fn identity(device: &AuthorizedDevice, agent: &str) -> String {
    serde_json::to_string(&(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
        agent,
    ))
    .expect("string tuple")
}
fn local<T>(r: hagency_agent_local::Result<T>) -> Result<T, RuntimeError> {
    r.map_err(|_| RuntimeError::Profile)
}
fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(i64::MAX)
}
fn now() -> i64 {
    wall_ms() / 1000
}
fn convert(event: super::device_execution::Dispatch) -> Result<inbox::Dispatch, RuntimeError> {
    serde_json::from_value(serde_json::to_value(event).map_err(|_| RuntimeError::Transport)?)
        .map_err(|_| RuntimeError::Transport)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Binding {
    id: String,
    agent_id: String,
    room_id: String,
    state: String,
    #[serde(default)]
    scope_kind: String,
}
fn validate_event_context(
    event: &inbox::Dispatch,
    config: &StartConfig,
    _owner: &str,
) -> Result<(), RuntimeError> {
    if event.binding_id != config.binding_id
        || event.agent_id != config.agent_id
        || (event.thread_root == event.room_id && !config.owner_direct)
        || !(event.thread_root.starts_with('$') || event.thread_root == event.room_id)
    {
        return Err(RuntimeError::Transport);
    }
    Ok(())
}
fn verify_execution_device(
    value: &serde_json::Value,
    agent: &str,
    device: &str,
) -> Result<(), RuntimeError> {
    let record: super::owned_agents::ServerAgent = serde_json::from_value(value["agent"].clone())
        .map_err(|_| RuntimeError::ExecutionDevice)?;
    if record.id != agent
        || record.execution_device_id.as_deref() != Some(device)
        || record.generation <= 0
    {
        return Err(RuntimeError::ExecutionDevice);
    }
    Ok(())
}
impl OwnedRuntime {
    pub(super) async fn recovery_intents(
        &self,
        console: &Console,
    ) -> Result<Vec<RuntimeIntent>, RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let (path, identity) = intent_location(console, &device)?;
        Ok(intents_for_device(
            read_intents(&path, &identity)?,
            device.device_id(),
        ))
    }
    /// Holding this registry lock through publication lets switch revoke first,
    /// then drain: an in-flight old request cannot publish after the drain.
    async fn entries_for(
        &self,
        device: &AuthorizedDevice,
    ) -> Result<tokio::sync::MutexGuard<'_, BTreeMap<String, Entry>>, RuntimeError> {
        let entries = self.entries.lock().await;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        Ok(entries)
    }
    /// Startup is not online until provider handshake/login and device lease are
    /// verified. Cookie is used here for owner scope and never retained by a job.
    pub async fn start(
        &self,
        console: Console,
        _cookie: &str,
        mut config: StartConfig,
    ) -> Result<RuntimeStatus, RuntimeError> {
        let _activity = self.tasks.enter().map_err(|_| RuntimeError::Stopped)?;
        runtime::validate(&config.profile, &config.credential_ref)
            .map_err(|_| RuntimeError::Profile)?;
        if config.profile.executable
            != super::owner_provider::external::executable(runtime::kind(&config.credential_ref))
                .map_err(|_| RuntimeError::Profile)?
        {
            return Err(RuntimeError::Profile);
        }
        if runtime::managed_reference(&config.credential_ref).is_some() {
            super::owner_provider::external::verify_reference(
                &config.profile.codex_home,
                &config.credential_ref,
            )
            .map_err(|_| RuntimeError::Profile)?;
        } else if super::owner_provider::selected_reference(
            &config.profile.codex_home,
            config.profile.shared_auth,
        )
        .map_err(|_| RuntimeError::Profile)?
            != config.credential_ref
        {
            return Err(RuntimeError::Profile);
        }
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        if config
            .recovery_device_id
            .as_deref()
            .is_some_and(|id| id != device.device_id())
        {
            return Err(RuntimeError::ExecutionDevice);
        }
        let assigned = console
            .owner_api_pinned(
                &DevicePin::from(&device),
                OwnerOperation::Agent {
                    agent: config.agent_id.clone(),
                },
            )
            .await
            .map_err(service::owner_error)?;
        if assigned.owner != device.owner_mxid()
            || assigned.origin != device.origin()
            || assigned.issuer != device.issuer()
            || assigned.subject != device.subject()
        {
            return Err(RuntimeError::Authorization);
        }
        verify_execution_device(&assigned.value, &config.agent_id, device.device_id())?;
        if assigned.value["agent"]["ownerUserId"] != device.user_id() {
            return Err(RuntimeError::Authorization);
        }
        let reply = console
            .owner_api_pinned(
                &DevicePin::from(&device),
                OwnerOperation::Bindings {
                    agent: config.agent_id.clone(),
                },
            )
            .await
            .map_err(service::owner_error)?;
        if reply.owner != device.owner_mxid()
            || reply.origin != device.origin()
            || reply.issuer != device.issuer()
            || reply.subject != device.subject()
        {
            return Err(RuntimeError::Authorization);
        }
        let bindings: Vec<Binding> = serde_json::from_value(reply.value["bindings"].clone())
            .map_err(|_| RuntimeError::Profile)?;
        let binding = bindings
            .into_iter()
            .find(|b| {
                b.agent_id == config.agent_id && b.id == config.binding_id && b.state == "active"
            })
            .ok_or(RuntimeError::Profile)?;
        config.owner_direct = binding.scope_kind == "owner_direct";
        let root = console
            .0
            .server_login
            .state_directory()
            .ok_or(RuntimeError::Profile)?;
        let path = ledger_path(
            &root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
        )
        .map_err(|_| RuntimeError::Profile)?;
        // Paths are owner-specific, and must be initialized by the owner. A
        // caller cannot silently repurpose ~/.codex or another owner's login.
        let expected = super::owner_provider::external::paths_for_reference(
            &root,
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
            &config.credential_ref,
        )
        .map_err(|_| RuntimeError::Profile)?;
        if config.profile.codex_home != expected.codex_home
            || config.profile.home != expected.home
            || config.profile.shared_auth != expected.shared
            || config.credential_ref != expected.credential_ref
        {
            return Err(RuntimeError::Profile);
        }
        let mut ledger = local(Ledger::open_scoped(
            &path,
            device.owner_mxid(),
            &profile_identity(
                device.origin(),
                device.issuer(),
                device.subject(),
                device.owner_mxid(),
            )
            .map_err(|_| RuntimeError::Authorization)?,
        ))?;
        let scope = Scope {
            agent: config.agent_id.clone(),
            binding: binding.id,
            room: binding.room_id,
            requester: device.owner_mxid().into(),
            thread: "runtime-profile-validation".into(),
        };
        local(ledger.register_binding(device.owner_mxid(), &scope))?;
        match local(ledger.model_profile(&scope))? {
            Some(p)
                if p.model == config.profile.model
                    && p.workspace_root == config.profile.cwd.to_string_lossy()
                    && p.credential_ref == config.credential_ref
                    && (p.credential_ref.starts_with("keychain:")
                        || p.credential_ref.starts_with("codex-managed:")
                        || runtime::managed_reference(&p.credential_ref).is_some()) => {}
            _ => return Err(RuntimeError::Profile),
        }
        let key = identity(&device, &config.agent_id);
        let mut entries = self.entries_for(&device).await?;
        self.tasks.check_open().map_err(|_| RuntimeError::Stopped)?;
        verify_recovery_intent(&console, &device, &config)?;
        if let Some(entry) = entries
            .get_mut(&key)
            .filter(|entry| !entry.task.is_finished() && !*entry.cancel.borrow())
        {
            if entry.device != DevicePin::from(&device) {
                return Err(RuntimeError::Authorization);
            }
            if entry.binding_active(&config.binding_id) {
                return Err(RuntimeError::AlreadyActive);
            }
            save_intent(&console, &device, &config)?;
            let id = config.binding_id.clone();
            let (work, handle, status) = binding_work(config);
            entry
                .commands
                .send(work)
                .map_err(|_| RuntimeError::Stopped)?;
            entry.bindings.insert(id, handle);
            return Ok(status);
        }
        let process_lock = lock_agent(&path, &config.agent_id)?;
        save_intent(&console, &device, &config)?;
        let agent = config.agent_id.clone();
        let binding = config.binding_id.clone();
        let (initial, handle, status) = binding_work(config);
        let (commands, command_rx) = mpsc::unbounded_channel();
        let (cancel, receiver) = watch::channel(false);
        let (sender, status_receiver) =
            watch::channel(RuntimeStatus::new(&agent, "checking_provider", None));
        let cancellation = cancel.clone();
        let device_pin = DevicePin::from(&device);
        let task = tokio::spawn(async move {
            let _process_lock = process_lock;
            let result = run_host(
                console,
                device,
                path,
                initial,
                command_rx,
                receiver,
                cancellation,
                sender.clone(),
            )
            .await;
            let failure = failure_code(&result);
            sender.send_replace(RuntimeStatus::new(&agent, "stopped", failure));
        });
        let task = self.tasks.track(task);
        entries.insert(
            key,
            Entry {
                commands,
                bindings: BTreeMap::from([(binding, handle)]),
                cancel,
                status: status_receiver,
                task,
                device: device_pin,
                approvals: BTreeMap::new(),
            },
        );
        Ok(status)
    }
    async fn offer_approval(
        &self,
        console: &Console,
        expected: &DevicePin,
        pending: codex::PendingApproval,
    ) -> Result<(), RuntimeError> {
        use sha2::{Digest, Sha256};
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        if expected != &DevicePin::from(&device) {
            return Err(RuntimeError::Authorization);
        }
        let agent = &pending.proposal.scope.agent;
        let mut entries = self.entries_for(&device).await?;
        let entry = entries
            .get_mut(&identity(&device, agent))
            .ok_or(RuntimeError::Authorization)?;
        entry
            .approvals
            .retain(|_, slot| slot.view.proposal.expires > now() && !slot.pending.is_closed());
        if *entry.cancel.borrow()
            || entry.task.is_finished()
            || entry.device != DevicePin::from(&device)
            || !entry.binding_active(&pending.proposal.scope.binding)
            || pending.proposal.expires <= now()
            || entry.approvals.len() >= 128
        {
            return Err(RuntimeError::Authorization);
        }
        let mut nonce = [0u8; 32];
        getrandom::fill(&mut nonce).map_err(|_| RuntimeError::Profile)?;
        let proposal_id = format!("{:x}", Sha256::digest(nonce));
        let args_digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&pending.proposal).map_err(|_| RuntimeError::Profile)?
            )
        );
        let view = ApprovalView {
            proposal_id: proposal_id.clone(),
            args_digest,
            proposal: pending.proposal.clone(),
        };
        entry.approvals.insert(
            proposal_id,
            ApprovalSlot {
                view,
                pending,
                device: DevicePin::from(&device),
            },
        );
        Ok(())
    }
    pub async fn pending(
        &self,
        console: &Console,
        agent: &str,
    ) -> Result<Vec<ApprovalView>, RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let mut entries = self.entries_for(&device).await?;
        let Some(entry) = entries.get_mut(&identity(&device, agent)) else {
            return Ok(vec![]);
        };
        if *entry.cancel.borrow()
            || entry.task.is_finished()
            || entry.device != DevicePin::from(&device)
        {
            return Err(RuntimeError::Authorization);
        }
        entry
            .approvals
            .retain(|_, slot| slot.view.proposal.expires > now() && !slot.pending.is_closed());
        let active: BTreeSet<_> = entry
            .bindings
            .iter()
            .filter(|(_, b)| !*b.cancel.borrow() && b.status.borrow().phase != "stopped")
            .map(|(id, _)| id.clone())
            .collect();
        entry
            .approvals
            .retain(|_, slot| active.contains(&slot.view.proposal.scope.binding));
        Ok(entry
            .approvals
            .values()
            .map(|slot| slot.view.clone())
            .collect())
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn decide(
        &self,
        console: &Console,
        cookie: &str,
        agent: &str,
        proposal_id: &str,
        args_digest: &str,
        approved: bool,
    ) -> Result<(), RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let owner = console
            .owner_api(
                cookie,
                OwnerOperation::Bindings {
                    agent: agent.into(),
                },
            )
            .await
            .map_err(|_| RuntimeError::Authorization)?;
        if owner.owner != device.owner_mxid()
            || owner.origin != device.origin()
            || owner.issuer != device.issuer()
            || owner.subject != device.subject()
        {
            return Err(RuntimeError::Authorization);
        }
        let bindings: Vec<Binding> = serde_json::from_value(owner.value["bindings"].clone())
            .map_err(|_| RuntimeError::Profile)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let mut entries = self.entries_for(&device).await?;
        let entry = entries
            .get_mut(&identity(&device, agent))
            .ok_or(RuntimeError::Authorization)?;
        if *entry.cancel.borrow()
            || entry.task.is_finished()
            || entry.device != DevicePin::from(&device)
        {
            return Err(RuntimeError::Authorization);
        }
        let pin = DevicePin::from(&device);
        let pending =
            entry.take_approval(&pin, agent, proposal_id, args_digest, &bindings, now())?;
        pending.decide(approved);
        Ok(())
    }

    pub async fn status(
        &self,
        console: &Console,
        agent: &str,
    ) -> Result<Option<RuntimeStatus>, RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        let entries = self.entries.lock().await;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        if entries
            .get(&identity(&device, agent))
            .is_some_and(|entry| entry.device != DevicePin::from(&device))
        {
            return Err(RuntimeError::Authorization);
        }
        Ok(entries.get(&identity(&device, agent)).map(|e| {
            let mut status = e.status.borrow().clone();
            status.bindings = e
                .bindings
                .values()
                .map(|b| {
                    let mut child = b.status.borrow().clone();
                    if status.phase == "waiting_for_lease" && child.phase != "stopped" {
                        child.phase = "waiting_for_lease";
                    }
                    if status.phase == "stopped" {
                        child.phase = "stopped";
                        child.last_error = child.last_error.or(status.last_error);
                    }
                    child
                })
                .collect();
            status.native_tools |= status.bindings.iter().any(|child| child.native_tools);
            status
        }))
    }
    pub async fn stop(
        &self,
        console: &Console,
        agent: &str,
        binding: &str,
    ) -> Result<(), RuntimeError> {
        let device = console.authorized_device().await.map_err(device_error)?;
        device.bearer().map_err(|_| RuntimeError::Authorization)?;
        let mut receiver = {
            let mut entries = self.entries.lock().await;
            device.bearer().map_err(|_| RuntimeError::Authorization)?;
            let (path, profile_id) = intent_location(console, &device)?;
            change_intent(&path, &profile_id, agent, binding, None)?;
            let Some(entry) = entries.get_mut(&identity(&device, agent)) else {
                return Ok(());
            };
            if entry.device != DevicePin::from(&device) {
                return Err(RuntimeError::Authorization);
            }
            let Some(handle) = entry.bindings.get(binding) else {
                return Ok(());
            };
            handle.cancel.send_replace(true);
            let receiver = handle.status.clone();
            entry
                .approvals
                .retain(|_, slot| slot.view.proposal.scope.binding != binding);
            if !entry.bindings.keys().any(|id| entry.binding_active(id)) {
                entry.cancel.send_replace(true);
            }
            receiver
        };
        while receiver.borrow().phase != "stopped" {
            if receiver.changed().await.is_err() {
                break;
            }
        }
        Ok(())
    }
    /// Console shutdown/logout must invoke this even when authorization is gone.
    pub(super) fn request_stop_all(&self) {
        if let Ok(services) = self.services.try_lock() {
            for entry in services.values() {
                entry.cancel_now();
            }
        }
        if let Ok(entries) = self.entries.try_lock() {
            for entry in entries.values() {
                entry.cancel.send_replace(true);
            }
        } else if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let manager = self.clone();
            runtime.spawn(async move {
                manager.stop_all().await;
            });
        }
        // retire revokes the shared device atomic before invoking this. If a
        // lock is busy without a Tokio caller, the heartbeat still fails closed.
    }
    pub(super) async fn agent_busy(&self, device: &AuthorizedDevice, agent: &str) -> bool {
        let key = identity(device, agent);
        let pin = DevicePin::from(device);
        self.services
            .lock()
            .await
            .get(&key)
            .is_some_and(|e| e.profile_matches(&pin))
            || self
                .entries
                .lock()
                .await
                .get(&key)
                .is_some_and(|e| !e.task.is_finished())
    }
    pub(super) async fn profile_busy(&self, device: &AuthorizedDevice) -> bool {
        let pin = DevicePin::from(device);
        let same = |p: &DevicePin| {
            p.origin == pin.origin
                && p.issuer == pin.issuer
                && p.subject == pin.subject
                && p.owner == pin.owner
                && p.user == pin.user
        };
        self.services
            .lock()
            .await
            .values()
            .any(|e| e.profile_matches(&pin))
            || self
                .entries
                .lock()
                .await
                .values()
                .any(|e| same(&e.device) && !e.task.is_finished())
    }
    pub(super) async fn stop_profile(&self, device: &AuthorizedDevice) -> Result<(), RuntimeError> {
        self.drain_services(Some(&DevicePin::from(device))).await;
        let pin = DevicePin::from(device);
        let entries = self.entries_for(device).await?;
        let keys: Vec<_> = entries
            .iter()
            .filter(|(_, e)| {
                e.device.origin == pin.origin
                    && e.device.issuer == pin.issuer
                    && e.device.subject == pin.subject
                    && e.device.owner == pin.owner
                    && e.device.user == pin.user
            })
            .map(|(key, _)| key.clone())
            .collect();
        let pending: Vec<_> = keys
            .into_iter()
            .filter_map(|key| {
                entries.get(&key).map(|entry| {
                    entry.cancel.send_replace(true);
                    (key, entry.task.clone())
                })
            })
            .collect();
        drop(entries);
        for (key, task) in pending {
            let result = task.join().await;
            let mut entries = self.entries.lock().await;
            if entries
                .get(&key)
                .is_some_and(|entry| entry.task.same(&task))
            {
                entries.remove(&key);
            }
            if result.is_err() {
                return Err(RuntimeError::Stopped);
            }
        }
        Ok(())
    }
    pub async fn stop_all(&self) {
        self.drain_services(None).await;
        let pending: Vec<_> = self
            .entries
            .lock()
            .await
            .iter()
            .map(|(key, entry)| {
                entry.cancel.send_replace(true);
                (key.clone(), entry.task.clone())
            })
            .collect();
        for (key, task) in pending {
            let _ = task.join().await;
            let mut entries = self.entries.lock().await;
            if entries
                .get(&key)
                .is_some_and(|entry| entry.task.same(&task))
            {
                entries.remove(&key);
            }
        }
        let _ = self.tasks.settle().await;
    }
}
async fn canceled(receiver: &mut watch::Receiver<bool>) {
    loop {
        if *receiver.borrow_and_update() {
            return;
        }
        if receiver.changed().await.is_err() {
            return;
        }
    }
}
trait Transport {
    fn supports_group_crypto(&self) -> bool {
        false
    }
    fn approval_host(&self) -> Option<PinnedConsole> {
        None
    }
    fn execute(
        &self,
        op: Op,
    ) -> impl std::future::Future<Output = Result<Response, RuntimeError>> + Send;
}
#[derive(Clone)]
struct PinnedConsole {
    console: Console,
    pin: DevicePin,
}
impl Transport for PinnedConsole {
    fn supports_group_crypto(&self) -> bool {
        true
    }
    fn approval_host(&self) -> Option<PinnedConsole> {
        Some(self.clone())
    }
    async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
        let acquiring = matches!(&op, Op::Acquire { .. });
        let request = matches!(
            &op,
            Op::Ack { .. }
                | Op::Start { .. }
                | Op::Processing { .. }
                | Op::AuthorizeTool { .. }
                | Op::Reply { .. }
                | Op::ReconcileKnownReply { .. }
                | Op::EncryptedReply { .. }
                | Op::Finish { .. }
        );
        self.console
            .execution_api_pinned(op, &self.pin)
            .await
            .map_err(|e| transport_error(e.status, &e.code, acquiring, request))
    }
}
// Only explicit per-event refusals are isolated. Authentication, device and
// current-lease failures still fence execution; status alone cannot distinguish them.
fn transport_error(status: u16, code: &str, acquiring: bool, request: bool) -> RuntimeError {
    if acquiring && status == 409 && code == "agent_leased_to_another_device" {
        RuntimeError::LeaseBusy
    } else if matches!(
        code,
        "execution_history_changed" | "ledger_recovery_required"
    ) {
        RuntimeError::LedgerRecovery
    } else if request
        && matches!(status, 401 | 403 | 409)
        && matches!(
            code,
            "event_expired"
                | "dispatch_not_offered"
                | "dispatch_not_acknowledged"
                | "execution_not_running"
                | "execution_id_reused"
                | "reply_payload_changed"
                | "original_execution_mismatch"
                | "reply_delivery_permanently_blocked"
                | "reply_sender_busy"
                | "reply_not_reconcilable"
                | "dispatch_epoch_expired"
                | "room_delivery_denied"
                | "dispatch_scope_revoked"
                | "encrypted_reply_requires_encrypted_room"
        )
    {
        RuntimeError::Request
    } else if matches!(status, 401 | 403 | 409) {
        RuntimeError::Authorization
    } else {
        RuntimeError::Transport
    }
}
async fn operation<T: Transport>(
    console: &T,
    receiver: &mut watch::Receiver<bool>,
    op: Op,
) -> Result<Response, RuntimeError> {
    tokio::select! { biased; _=canceled(receiver)=>Err(RuntimeError::Stopped), response=console.execute(op)=>response }
}
fn deadline(lease: &Lease) -> Result<Instant, RuntimeError> {
    let remaining = i128::from(lease.expires_at_ms) - i128::from(wall_ms());
    if remaining <= 0 || remaining > 31_000 {
        return Err(RuntimeError::Authorization);
    }
    Ok(Instant::now() + Duration::from_millis(remaining as u64))
}
fn heartbeat_next(now: Instant, expiry: Instant) -> Instant {
    now + Duration::from_secs(5).min(expiry.saturating_duration_since(now) / 2)
}
async fn heartbeat(
    console: Console,
    mut device: AuthorizedDevice,
    mut lease: Lease,
    stop: watch::Sender<bool>,
    mut receiver: watch::Receiver<bool>,
    failed: Arc<std::sync::atomic::AtomicU8>,
) {
    use std::sync::atomic::Ordering;
    let first_expiry = deadline(&lease).unwrap_or_else(|_| Instant::now());
    let mut next = heartbeat_next(Instant::now(), first_expiry.min(device.valid_until()));
    let reason = loop {
        let Ok(expiry) = deadline(&lease) else {
            break "lease_deadline_elapsed";
        };
        // The old snapshot pins identity only. The RPC obtains a fresh proof;
        // expiry of an earlier snapshot must not prevent same-device recovery.
        if Instant::now() >= next {
            let pin = DevicePin::from(&device);
            let renewed = tokio::select! { biased; _=canceled(&mut receiver)=>return, _=tokio::time::sleep_until(expiry.into())=>break "lease_renewal_deadline", result=console.execution_api_pinned(Op::Renew{lease:lease.reference(),ttl_ms:30_000},&pin)=>result };
            let value = match renewed {
                Ok(Response::Lease(value)) => value,
                Err(error) if !matches!(error.status, 401 | 403 | 409) => {
                    tracing::warn!(
                        stage = "lease_renewal",
                        status = error.status,
                        remaining_ms =
                            expiry.saturating_duration_since(Instant::now()).as_millis() as u64,
                        "Lease renewal transport failure; retrying before deadline"
                    );
                    next = Instant::now() + Duration::from_secs(1);
                    tokio::select! {biased; _=canceled(&mut receiver)=>return,
                    _=tokio::time::sleep_until(next.min(expiry).into())=>{}}
                    continue;
                }
                _ => break "lease_renewal_rejected",
            };
            lease = value;
            // The successful renewal replaces the lease deadline. Waiting for a
            // fresh device against the previous deadline would cancel a valid renewal.
            let Ok(fresh_expiry) = deadline(&lease) else {
                break "renewed_lease_expired";
            };
            let fresh = tokio::select! {biased; _=canceled(&mut receiver)=>return, _=tokio::time::sleep_until(fresh_expiry.into())=>break "device_refresh_deadline", result=console.authorized_device()=>result};
            let fresh = match fresh {
                Ok(fresh) => fresh,
                Err(super::Error::Unavailable) => break "device_refresh_transport",
                _ => break "device_authorization_required",
            };
            if fresh.origin() != device.origin()
                || fresh.issuer() != device.issuer()
                || fresh.subject() != device.subject()
                || fresh.user_id() != device.user_id()
                || fresh.owner_mxid() != device.owner_mxid()
                || fresh.device_id() != device.device_id()
                || fresh.generation() != device.generation()
            {
                break "device_identity_changed";
            }
            device = fresh;
            next = heartbeat_next(Instant::now(), fresh_expiry.min(device.valid_until()));
        }
        tokio::select! { biased; _=canceled(&mut receiver)=>return, _=tokio::time::sleep_until(next.min(expiry).into())=>{} }
    };
    tracing::warn!(stage = reason, "Agent heartbeat stopped");
    let terminal = matches!(
        reason,
        "lease_renewal_rejected" | "device_identity_changed" | "device_authorization_required"
    );
    failed.store(if terminal { 1 } else { 2 }, Ordering::Release);
    stop.send_replace(true);
}
fn device_error(error: super::Error) -> RuntimeError {
    if matches!(error, super::Error::Unavailable) {
        RuntimeError::Transport
    } else {
        RuntimeError::Authorization
    }
}
fn failure_code(result: &Result<(), RuntimeError>) -> Option<&'static str> {
    match result {
        Ok(()) | Err(RuntimeError::Stopped) => None,
        Err(RuntimeError::Authorization) => Some("authorization_or_lease_lost"),
        Err(RuntimeError::LeaseBusy) => Some("agent_lease_busy"),
        Err(RuntimeError::Profile) => Some("profile_or_local_state_unavailable"),
        Err(RuntimeError::Provider) => Some("provider_authorization_lost"),
        Err(RuntimeError::Execution(code)) => Some(code),
        Err(RuntimeError::LedgerRecovery) => Some("ledger_recovery_required"),
        Err(RuntimeError::ExecutionDevice) => Some("agent_execution_device_required"),
        _ => Some("device_transport_unavailable"),
    }
}
/// The check before a room worker goes online: the tool starts, answers and
/// is signed in. The owner is told to look at the account either way; the log
/// names what actually failed, which is often not the account.
fn startup_check_failure(error: &codex::Error) -> RuntimeError {
    let (kind, detail) = error.diagnostic();
    tracing::warn!(
        kind,
        detail,
        event = error.diagnostic_event(),
        "Local provider start-up check failed"
    );
    RuntimeError::Provider
}
fn provider_failure(stage: &'static str, error: &codex::Error) -> RuntimeError {
    let (kind, detail) = error.diagnostic();
    tracing::warn!(
        stage,
        kind,
        detail,
        event = error.diagnostic_event(),
        "Local provider operation failed"
    );
    match error {
        codex::Error::Profile("dedicated owner provider login required") => RuntimeError::Provider,
        codex::Error::Profile(_) | codex::Error::Ledger(_) => RuntimeError::Profile,
        codex::Error::Timeout => RuntimeError::Execution("codex_provider_timeout"),
        codex::Error::Protocol(hagency_runtime::codex::version::REQUIRED) => {
            RuntimeError::Execution("codex_version_unsupported")
        }
        codex::Error::Protocol(_) | codex::Error::EventScope { .. } | codex::Error::Json(_) => {
            RuntimeError::Execution("codex_protocol_mismatch")
        }
        _ => RuntimeError::Execution("codex_provider_unavailable"),
    }
}
/// Consume stable metadata pages without retaining unbounded message/history payloads.
async fn history_coverage<T: Transport>(
    transport: &T,
    ledger: &mut Ledger,
    owner: &str,
    agent: &str,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(HistorySnapshot, bool), RuntimeError> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"hagency-started-executions-v1\n");
    let mut cursor = None;
    let mut snapshot = None;
    let mut seen = 0u64;
    let mut covered = true;
    loop {
        let Response::History(page) = operation(
            transport,
            cancel,
            Op::History {
                agent_id: agent.into(),
                cursor: cursor.clone(),
                snapshot: snapshot.clone(),
            },
        )
        .await?
        else {
            return Err(RuntimeError::LedgerRecovery);
        };
        if page.agent_id != agent || snapshot.as_ref().is_some_and(|s| s != &page.snapshot) {
            return Err(RuntimeError::LedgerRecovery);
        }
        let mut previous = cursor.as_deref();
        for entry in &page.executions {
            hasher.update(
                serde_json::to_vec(&(&entry.dispatch_id, &entry.execution_id))
                    .map_err(|_| RuntimeError::LedgerRecovery)?,
            );
            hasher.update(b"\n");
            if previous.is_some_and(|p| p >= entry.dispatch_id.as_str()) {
                return Err(RuntimeError::LedgerRecovery);
            }
            previous = Some(&entry.dispatch_id);
        }
        if page
            .next_cursor
            .as_ref()
            .is_some_and(|c| Some(c.as_str()) != previous || page.executions.is_empty())
        {
            return Err(RuntimeError::LedgerRecovery);
        }
        seen = seen
            .checked_add(page.executions.len() as u64)
            .ok_or(RuntimeError::LedgerRecovery)?;
        if seen > page.snapshot.count || page.executions.len() > 128 {
            return Err(RuntimeError::LedgerRecovery);
        }
        covered &= local(ledger.covers_execution_history_page(
            owner,
            agent,
            &page.executions,
            page.next_cursor.is_none(),
        ))?;
        snapshot = Some(page.snapshot);
        cursor = page.next_cursor;
        if cursor.is_none() {
            let snapshot = snapshot.ok_or(RuntimeError::LedgerRecovery)?;
            if seen != snapshot.count || format!("{:x}", hasher.finalize()) != snapshot.digest {
                return Err(RuntimeError::LedgerRecovery);
            }
            return Ok((snapshot, covered));
        }
    }
}
async fn acquire_checked<T: Transport>(
    transport: &T,
    ledger: &mut Ledger,
    owner: &str,
    config: &StartConfig,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(Lease, HistorySnapshot, bool), RuntimeError> {
    let (snapshot, covered) =
        history_coverage(transport, ledger, owner, &config.agent_id, cancel).await?;
    if !covered {
        let known = local(ledger.has_known_reply(owner, &config.agent_id, &config.binding_id))?;
        if !known || !matches!(config.takeover, Takeover::OwnerRequested) {
            return Err(RuntimeError::LedgerRecovery);
        }
    }
    let Response::Lease(lease) = operation(
        transport,
        cancel,
        Op::Acquire {
            agent_id: config.agent_id.clone(),
            ttl_ms: 30_000,
            takeover: config.takeover,
            history_snapshot: snapshot.clone(),
        },
    )
    .await?
    else {
        return Err(RuntimeError::Transport);
    };
    Ok((lease, snapshot, covered))
}
#[derive(Clone, Copy)]
struct RecoveryLeaseWait {
    total: Duration,
    interval: Duration,
}
impl Default for RecoveryLeaseWait {
    fn default() -> Self {
        Self {
            total: Duration::from_secs(35),
            interval: Duration::from_secs(1),
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn acquire_for_start<T: Transport>(
    transport: &T,
    ledger: &mut Ledger,
    owner: &str,
    config: &StartConfig,
    cancel: &mut watch::Receiver<bool>,
    status: &watch::Sender<RuntimeStatus>,
    binding_status: &watch::Sender<RuntimeStatus>,
    wait: RecoveryLeaseWait,
) -> Result<(Lease, HistorySnapshot, bool), RuntimeError> {
    if config.recovery_consent_id.is_none()
        || config.recovery_device_id.is_none()
        || !matches!(config.takeover, Takeover::Never)
    {
        return acquire_checked(transport, ledger, owner, config, cancel).await;
    }
    let deadline = tokio::time::Instant::now() + wait.total;
    let mut saw_busy = false;
    loop {
        let mut attempt_cancel = cancel.clone();
        let attempt = tokio::select! {biased;
            _=canceled(&mut attempt_cancel)=>return Err(RuntimeError::Stopped),
            _=tokio::time::sleep_until(deadline)=>return Err(if saw_busy {RuntimeError::LeaseBusy} else {RuntimeError::Transport}),
            result=acquire_checked(transport,ledger,owner,config,cancel)=>result,
        };
        match attempt {
            Err(RuntimeError::LeaseBusy)
                if config.recovery_consent_id.is_some()
                    && config.recovery_device_id.is_some()
                    && matches!(config.takeover, Takeover::Never) =>
            {
                saw_busy = true;
                let mut waiting = RuntimeStatus::new(&config.agent_id, "waiting_for_lease", None)
                    .with_capabilities(config);
                waiting.binding_id = Some(config.binding_id.clone());
                status.send_replace(waiting.clone());
                binding_status.send_replace(waiting);
                tokio::select! {biased;
                    _=canceled(cancel)=>return Err(RuntimeError::Stopped),
                    _=tokio::time::sleep_until(deadline)=>return Err(RuntimeError::LeaseBusy),
                    _=tokio::time::sleep(wait.interval)=>{},
                }
                // Every retry repeats fresh history, complete local accounting
                // coverage and the server's atomic Acquire snapshot check.
            }
            other => return other,
        }
    }
}
async fn post_acquire_coverage<T: Transport>(
    transport: &T,
    ledger: &mut Ledger,
    owner: &str,
    agent: &str,
    expected: &HistorySnapshot,
    cancel: &mut watch::Receiver<bool>,
) -> Result<bool, RuntimeError> {
    let (after, covered) = history_coverage(transport, ledger, owner, agent, cancel).await?;
    if &after != expected {
        return Err(RuntimeError::LedgerRecovery);
    }
    Ok(covered)
}
#[allow(clippy::too_many_arguments)]
async fn run_host(
    console: Console,
    device: AuthorizedDevice,
    path: PathBuf,
    initial: BindingWork,
    commands: mpsc::UnboundedReceiver<BindingWork>,
    mut cancel: watch::Receiver<bool>,
    stop: watch::Sender<bool>,
    status: watch::Sender<RuntimeStatus>,
) -> Result<(), RuntimeError> {
    let provider_tasks = console.0.owned_runtime.tasks.clone();
    let profile_id = profile_identity(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .map_err(|_| RuntimeError::Authorization)?;
    let transport = PinnedConsole {
        console: console.clone(),
        pin: DevicePin::from(&device),
    };
    let owner = device.owner_mxid().to_owned();
    let agent = initial.config.agent_id.clone();
    // Verify the owner's dedicated provider before any acquisition or explicit
    // takeover can disrupt an existing device. This is account/config only.
    let mut provider = tokio::select! {biased;_=canceled(&mut cancel)=>return Err(RuntimeError::Stopped),p=provider_custody::Provider::capture(&provider_tasks,provider_custody::spawn(&initial.config.profile,&initial.config.credential_ref))=>p.map_err(|_|RuntimeError::Provider)?};
    let checked = tokio::select! {biased;_=canceled(&mut cancel)=>Err(RuntimeError::Stopped),r=async {provider.session.initialize().await?;provider.session.verify_host_environment().await?;provider.session.require_local_account().await}=>r.map_err(|error|startup_check_failure(&error))};
    provider.stop().await.map_err(|_| RuntimeError::Provider)?;
    checked?;
    let mut ledger = local(Ledger::open_scoped(&path, &owner, &profile_id))?;
    let (lease, history_snapshot, pre_covered) = acquire_for_start(
        &transport,
        &mut ledger,
        &owner,
        &initial.config,
        &mut cancel,
        &status,
        &initial.status,
        RecoveryLeaseWait::default(),
    )
    .await?;
    drop(ledger);
    // A bounded recovery wait may outlive the initial device snapshot. Acquire
    // used a fresh bearer; heartbeat must begin from that current capability,
    // while preserving the original immutable identity/generation fence.
    let device = console.authorized_device().await.map_err(device_error)?;
    device.bearer().map_err(|_| RuntimeError::Authorization)?;
    if DevicePin::from(&device) != transport.pin {
        return Err(RuntimeError::Authorization);
    }
    let reference = lease.reference();
    let heartbeat_failed = Arc::new(std::sync::atomic::AtomicU8::new(0));
    let monitor = tokio::spawn(heartbeat(
        console.clone(),
        device,
        lease,
        stop.clone(),
        cancel.clone(),
        heartbeat_failed.clone(),
    ));
    let _heartbeat_guard = AbortHeartbeat(monitor.abort_handle());
    let result=async {
        let mut ledger=local(Ledger::open_scoped(&path,&owner,&profile_id))?;
        let post_covered=post_acquire_coverage(&transport,&mut ledger,&owner,&agent,&history_snapshot,&mut cancel).await?;
        let models_allowed=pre_covered && post_covered;
        local(ledger.recover_agent_executions(&owner,&agent))?;
        drop(ledger);
        status.send_replace(RuntimeStatus::new(&agent,if models_allowed{"online"}else{"ledger_recovery_required"},if models_allowed{None}else{Some("ledger_recovery_required")}));
        supervise_bindings(initial,commands,&mut cancel,|work| {
            let transport=transport.clone();let owner=owner.clone();let profile_id=profile_id.clone();let path=path.clone();let reference=reference.clone();
            let provider_tasks=provider_tasks.clone();
            let completion=provider_tasks.checked();
            tokio::spawn(async move {
                let Ok(completion)=completion else {return;};
                let mut work=work;
                let result=async {
                    let mut provider=tokio::select! {biased;_=canceled(&mut work.receiver)=>return Err(RuntimeError::Stopped),p=provider_custody::Provider::capture(&provider_tasks,provider_custody::spawn(&work.config.profile,&work.config.credential_ref))=>p.map_err(|_|RuntimeError::Profile)?};
                    let checked=tokio::select! {biased;_=canceled(&mut work.receiver)=>Err(RuntimeError::Stopped),r=async {provider.session.initialize().await?;provider.session.verify_host_environment().await?;provider.session.require_local_account().await}=>r.map_err(|error|startup_check_failure(&error))};
                    provider.stop().await.map_err(|_|RuntimeError::Provider)?;checked?;
                    let mut ledger=local(Ledger::open_scoped(&path,&owner,&profile_id))?;
                    let mut ready=RuntimeStatus::new(&work.config.agent_id,if models_allowed {if work.config.shared_workspace {"online"} else {"online_chat_only"}} else {"ledger_recovery_required"},if models_allowed{None}else{Some("ledger_recovery_required")}).with_capabilities(&work.config);ready.binding_id=Some(work.config.binding_id.clone());work.status.send_replace(ready);
                    if models_allowed && (work.config.owner_direct || transport.supports_group_crypto()) {e2ee::prepare(&transport,&reference,&work.config,&mut work.receiver).await;}
                    execution_loop_with_gate(&transport,&owner,&mut ledger,&work.config,&reference,&mut work.receiver,&work.status,models_allowed).await
                }.await;
                let mut final_status=RuntimeStatus::new(&work.config.agent_id,"stopped",failure_code(&result)).with_capabilities(&work.config);
                final_status.binding_id=Some(work.config.binding_id.clone());work.status.send_replace(final_status);
                completion.complete();
            })
        }).await
    }.await;
    monitor.abort();
    let _ = monitor.await;
    let _ = transport.execute(Op::Release { lease: reference }).await;
    match heartbeat_failed.load(std::sync::atomic::Ordering::Acquire) {
        1 => Err(RuntimeError::Authorization),
        2 => Err(RuntimeError::Transport),
        _ => result,
    }
}
/// One supervisor owns the lease. Workers can never acquire, renew or release it.
/// All cancellation paths join workers before the supervisor releases the lease.
async fn supervise_bindings<F>(
    initial: BindingWork,
    mut commands: mpsc::UnboundedReceiver<BindingWork>,
    cancel: &mut watch::Receiver<bool>,
    mut spawn: F,
) -> Result<(), RuntimeError>
where
    F: FnMut(BindingWork) -> tokio::task::JoinHandle<()>,
{
    let mut workers: BTreeMap<String, (watch::Sender<bool>, tokio::task::JoinHandle<()>)> =
        BTreeMap::new();
    let id = initial.config.binding_id.clone();
    let stop = initial.cancel.clone();
    workers.insert(id, (stop, spawn(initial)));
    loop {
        if *cancel.borrow() {
            break;
        }
        let finished: Vec<_> = workers
            .iter()
            .filter(|(_, (_, task))| task.is_finished())
            .map(|(id, _)| id.clone())
            .collect();
        for id in finished {
            if let Some((_, task)) = workers.remove(&id) {
                let _ = task.await;
            }
        }
        if workers.is_empty() {
            // A start accepted before the last worker ended must still run.
            if let Ok(work) = commands.try_recv() {
                let id = work.config.binding_id.clone();
                let stop = work.cancel.clone();
                workers.insert(id, (stop, spawn(work)));
                continue;
            }
            // Close before the final drain: concurrent senders either fail or
            // have their accepted work receive a terminal status.
            commands.close();
            finish_queued_starts(&mut commands);
            return Ok(());
        }
        tokio::select! {biased;
            _=canceled(cancel)=>break,
            work=commands.recv()=>{let Some(work)=work else {break;};let id=work.config.binding_id.clone();
                if let Some((old_stop,old))=workers.remove(&id) {old_stop.send_replace(true);let _=old.await;}
                let stop=work.cancel.clone();workers.insert(id,(stop,spawn(work)));
            },
            _=tokio::time::sleep(Duration::from_millis(100))=>{}
        }
    }
    for (stop, _) in workers.values() {
        stop.send_replace(true);
    }
    for (_, (_, task)) in workers {
        let _ = task.await;
    }
    commands.close();
    finish_queued_starts(&mut commands);
    Err(RuntimeError::Stopped)
}
fn finish_queued_starts(commands: &mut mpsc::UnboundedReceiver<BindingWork>) {
    while let Ok(work) = commands.try_recv() {
        let mut status = RuntimeStatus::new(&work.config.agent_id, "stopped", None);
        status.binding_id = Some(work.config.binding_id);
        work.status.send_replace(status);
    }
}
fn same_lease(event: &inbox::Dispatch, lease: &LeaseRef) -> bool {
    event.agent_id == lease.agent_id && event.dispatch_epoch == Some(lease.epoch)
}
#[cfg(test)]
async fn execution_loop<T: Transport>(
    console: &T,
    owner: &str,
    ledger: &mut Ledger,
    config: &StartConfig,
    lease: &LeaseRef,
    cancel: &mut watch::Receiver<bool>,
    status: &watch::Sender<RuntimeStatus>,
) -> Result<(), RuntimeError> {
    execution_loop_with_gate(console, owner, ledger, config, lease, cancel, status, true).await
}
#[allow(clippy::too_many_arguments)]
async fn execution_loop_with_gate<T: Transport>(
    console: &T,
    owner: &str,
    ledger: &mut Ledger,
    config: &StartConfig,
    lease: &LeaseRef,
    cancel: &mut watch::Receiver<bool>,
    status: &watch::Sender<RuntimeStatus>,
    models_allowed: bool,
) -> Result<(), RuntimeError> {
    let mut reported = BTreeSet::new();
    let mut sessions = provider_custody::Sessions::default();
    let mut reply_cursor = String::new();
    let mut finish_cursors = [String::new(), String::new(), String::new()];
    loop {
        if *cancel.borrow() {
            return Err(RuntimeError::Stopped);
        }
        // Original durable output survives restart; never regenerate it.
        for record in local(ledger.inbox_for_binding_after(
            owner,
            &config.agent_id,
            &config.binding_id,
            State::ReplyReady,
            &reply_cursor,
        ))? {
            reply_cursor = record.dispatch.id.clone();
            if record.dispatch.binding_id != config.binding_id
                || record.dispatch.agent_id != lease.agent_id
                || record
                    .dispatch
                    .dispatch_epoch
                    .is_none_or(|epoch| epoch > lease.epoch)
            {
                continue;
            }
            retry_reply(console, owner, ledger, lease, cancel, config, &record).await?;
        }
        for (index, (state, outcome)) in [
            (State::Unknown, Outcome::Unknown),
            (State::Failed, Outcome::Failed),
            (State::Rejected, Outcome::Rejected),
        ]
        .into_iter()
        .enumerate()
        {
            for record in local(ledger.inbox_for_binding_after(
                owner,
                &config.agent_id,
                &config.binding_id,
                state,
                &finish_cursors[index],
            ))? {
                finish_cursors[index] = record.dispatch.id.clone();
                if record.dispatch.binding_id != config.binding_id
                    || !same_lease(&record.dispatch, lease)
                    || reported.contains(&record.dispatch.id)
                {
                    continue;
                }
                if let Some(execution) = record.execution_id {
                    let outcome = operation(
                        console,
                        cancel,
                        Op::Finish {
                            lease: lease.clone(),
                            dispatch_id: record.dispatch.id.clone(),
                            execution_id: execution,
                            outcome,
                        },
                    )
                    .await;
                    if matches!(outcome, Err(RuntimeError::Stopped)) {
                        return Err(RuntimeError::Stopped);
                    }
                    if outcome.is_ok() {
                        reported.insert(record.dispatch.id);
                    }
                }
            }
        }
        if !models_allowed {
            let mut blocked = RuntimeStatus::new(
                &config.agent_id,
                "ledger_recovery_required",
                Some("ledger_recovery_required"),
            )
            .with_capabilities(config);
            blocked.binding_id = Some(config.binding_id.clone());
            status.send_replace(blocked);
            tokio::select! {biased;_=canceled(cancel)=>return Err(RuntimeError::Stopped),_=tokio::time::sleep(Duration::from_millis(250))=>{}}
            continue;
        }
        if console.supports_group_crypto() {
            e2ee::pump_candidates(console, lease, config, cancel).await?;
        }
        let events = match operation(
            console,
            cancel,
            Op::Poll {
                lease: lease.clone(),
                binding_id: config.binding_id.clone(),
                limit: 1,
            },
        )
        .await
        {
            Ok(Response::Events(events)) => events,
            Err(RuntimeError::Transport) => {
                tokio::select! {biased; _=canceled(cancel)=>return Err(RuntimeError::Stopped), _=tokio::time::sleep(Duration::from_secs(1))=>{}}
                continue;
            }
            Err(error) => return Err(error),
            _ => return Err(RuntimeError::Transport),
        };
        for event in events {
            let event = convert(event)?;
            validate_event_context(&event, config, owner)?;
            let scope = event.scope();
            // Poll transport has verified current owner/device/Agent and lease.
            local(ledger.register_binding(owner, &scope))?;
            local(ledger.receive_dispatch(owner, &event, inbox::Limits::default(), now()))?;
            let record = local(ledger.inbox_record(owner, &event.id))?;
            // A redelivery is not permission to repeat provider work. Close the
            // unstarted server offer even after a restart changed the lease epoch.
            if !matches!(record.state, State::Received | State::Acknowledged) {
                if let Some(execution) = record.execution_id {
                    if record.state == State::Prepared {
                        local(ledger.finish_local_execution(
                            owner,
                            &event.id,
                            &execution,
                            Finish::Unknown,
                        ))?;
                    }
                    let outcome = match record.state {
                        State::Failed => Outcome::Failed,
                        State::Rejected => Outcome::Rejected,
                        _ => Outcome::Unknown,
                    };
                    let result = operation(
                        console,
                        cancel,
                        Op::Finish {
                            lease: lease.clone(),
                            dispatch_id: event.id.clone(),
                            execution_id: execution,
                            outcome,
                        },
                    )
                    .await;
                    if matches!(
                        result,
                        Err(RuntimeError::Stopped | RuntimeError::Authorization)
                    ) {
                        return result.map(|_| ());
                    }
                    if result.is_ok() {
                        reported.insert(event.id.clone());
                    }
                }
                continue;
            }
            match operation(
                console,
                cancel,
                Op::Ack {
                    lease: lease.clone(),
                    dispatch_id: event.id.clone(),
                },
            )
            .await
            {
                Ok(Response::Acknowledged) => local(ledger.acknowledge_dispatch(owner, &event.id))?,
                Err(RuntimeError::Transport) => continue,
                Err(RuntimeError::Request) => {
                    // No provider work was started. Retain a terminal witness,
                    // retire this offer, and let the next request be polled.
                    local(ledger.acknowledge_dispatch(owner, &event.id))?;
                    let prepared = local(ledger.prepare_execution(owner, &event.id))?;
                    local(ledger.finish_local_execution(
                        owner,
                        &event.id,
                        &prepared.execution_id,
                        Finish::Rejected,
                    ))?;
                    let result = operation(
                        console,
                        cancel,
                        Op::Finish {
                            lease: lease.clone(),
                            dispatch_id: event.id.clone(),
                            execution_id: prepared.execution_id,
                            outcome: Outcome::Rejected,
                        },
                    )
                    .await;
                    if matches!(
                        result,
                        Err(RuntimeError::Stopped | RuntimeError::Authorization)
                    ) {
                        return result.map(|_| ());
                    }
                    if result.is_ok() {
                        reported.insert(event.id.clone());
                    }
                }
                Err(error) => return Err(error),
                _ => return Err(RuntimeError::Transport),
            }
        }
        for record in local(ledger.inbox_for_binding(
            owner,
            &config.agent_id,
            &config.binding_id,
            State::Acknowledged,
            1000,
        ))? {
            if record.dispatch.binding_id != config.binding_id
                || !same_lease(&record.dispatch, lease)
            {
                continue;
            }
            let scope = record.dispatch.scope();
            // Profile changes require another explicit owner start.
            let profile = local(ledger.model_profile(&scope))?.ok_or(RuntimeError::Profile)?;
            if profile.model != config.profile.model
                || profile.credential_ref != config.credential_ref
                || profile.workspace_root != config.profile.cwd.to_string_lossy()
                || profile.web_search != config.profile.web_search
            {
                return Err(RuntimeError::Profile);
            }
            if local(ledger.inbox_available_bytes(owner))? < 65_536 {
                return Err(RuntimeError::Profile);
            }
            let admission = console
                .approval_host()
                .map(|host| host.console.hosting_admission());
            let turn = match admission.as_ref() {
                Some(gate) => match gate.enter_turn() {
                    Some(turn) => Some(turn),
                    None => continue,
                },
                None => None,
            };
            let prepared = local(ledger.prepare_execution(owner, &record.dispatch.id))?;
            let start = operation(
                console,
                cancel,
                Op::Start {
                    lease: lease.clone(),
                    dispatch_id: prepared.dispatch.id.clone(),
                    execution_id: prepared.execution_id.clone(),
                },
            )
            .await;
            let result = match start {
                Ok(Response::Started(start)) => {
                    let response = inbox::ServerStart {
                        dispatch: convert(start.dispatch)?,
                        newly_started: start.newly_started,
                    };
                    let Some(permit) = local(ledger.confirm_execution_start(owner, &response))?
                    else {
                        continue;
                    };
                    // run_turn is fenced by the renewing device lease, live
                    // dispatch checks and explicit owner stop. A long native
                    // turn does not expire merely because twenty minutes pass.
                    run_turn(
                            console,
                            lease,
                            ledger,
                            owner,
                            config,
                            permit.into_prepared(),
                            turn,
                            cancel,
                            status,
                            &mut sessions,
                    )
                    .await
                }
                Err(error) => Err(error),
                _ => Err(RuntimeError::Transport),
            };
            let turn_error = result.as_ref().err().map(|error| match error {
                RuntimeError::Provider => "provider_authorization_lost",
                RuntimeError::Authorization => "authorization_or_lease_lost",
                RuntimeError::Profile => "profile_or_local_state_unavailable",
                RuntimeError::Execution(code) => code,
                _ => "device_transport_unavailable",
            });
            if let Some(code) = turn_error {
                tracing::warn!(
                    stage = "agent_turn",
                    code,
                    "Agent turn outcome is unknown; execution will not be replayed"
                );
            }
            let authorization_lost = matches!(&result, Err(RuntimeError::Authorization));
            if let Ok(Some(finish)) = result {
                local(ledger.finish_local_execution(
                    owner,
                    &prepared.dispatch.id,
                    &prepared.execution_id,
                    finish,
                ))?;
                let outcome = match finish {
                    Finish::Rejected => Outcome::Rejected,
                    Finish::Failed => Outcome::Failed,
                    Finish::Unknown => Outcome::Unknown,
                };
                let reported_outcome = operation(
                    console,
                    cancel,
                    Op::Finish {
                        lease: lease.clone(),
                        dispatch_id: prepared.dispatch.id.clone(),
                        execution_id: prepared.execution_id.clone(),
                        outcome,
                    },
                )
                .await;
                if matches!(reported_outcome, Err(RuntimeError::Stopped)) {
                    return Err(RuntimeError::Stopped);
                }
            } else if result.is_err() {
                // Network lost after start, process cancellation or storage failure
                // cannot authorize a retry of provider side effects.
                local(ledger.finish_local_execution(
                    owner,
                    &prepared.dispatch.id,
                    &prepared.execution_id,
                    Finish::Unknown,
                ))?;
                if *cancel.borrow() {
                    return Err(RuntimeError::Stopped);
                }
                let reported_outcome = operation(
                    console,
                    cancel,
                    Op::Finish {
                        lease: lease.clone(),
                        dispatch_id: prepared.dispatch.id.clone(),
                        execution_id: prepared.execution_id.clone(),
                        outcome: Outcome::Unknown,
                    },
                )
                .await;
                if authorization_lost {
                    return Err(RuntimeError::Authorization);
                }
                if matches!(reported_outcome, Err(RuntimeError::Stopped)) {
                    return Err(RuntimeError::Stopped);
                }
                if reported_outcome.is_ok() {
                    reported.insert(prepared.dispatch.id);
                }
            } else {
                let record = local(ledger.inbox_record(owner, &prepared.dispatch.id))?;
                retry_reply(console, owner, ledger, lease, cancel, config, &record).await?;
            }
            let mut ready = RuntimeStatus::new(&config.agent_id, if config.shared_workspace {"online"} else {"online_chat_only"}, turn_error)
                .with_capabilities(config);
            ready.binding_id = Some(config.binding_id.clone());
            status.send_replace(ready);
        }
        tokio::select! {biased; _=canceled(cancel)=>return Err(RuntimeError::Stopped),_=tokio::time::sleep(Duration::from_secs(1))=>{}}
    }
}
async fn check_running<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    expected: &inbox::Dispatch,
    execution: &str,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), RuntimeError> {
    let response = operation(
        transport,
        cancel,
        Op::AuthorizeTool {
            lease: lease.clone(),
            dispatch_id: expected.id.clone(),
            execution_id: execution.into(),
        },
    )
    .await?;
    let Response::ToolAuthorized(event) = response else {
        return Err(RuntimeError::Transport);
    };
    let event = convert(event)?;
    if *cancel.borrow() {
        return Err(RuntimeError::Stopped);
    }
    if event.id != expected.id
        || event.scope() != expected.scope()
        || event.event_id != expected.event_id
        || event.body != expected.body
        || event.binding_generation != expected.binding_generation
        || event.dispatch_epoch != Some(lease.epoch)
        || event.dispatch_device_id != expected.dispatch_device_id
        || event.execution_id.as_deref() != Some(execution)
        || event.state != "running"
    {
        return Err(RuntimeError::Request);
    }
    Ok(())
}
async fn fresh_running<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    expected: &inbox::Dispatch,
    execution: &str,
    cancel: &mut watch::Receiver<bool>,
) -> bool {
    check_running(transport, lease, expected, execution, cancel)
        .await
        .is_ok()
}
struct LiveToolGate {
    console: PinnedConsole,
    lease: LeaseRef,
    expected: inbox::Dispatch,
    execution: String,
    cancel: watch::Receiver<bool>,
}
#[cfg(unix)]
impl codex::HostToolGate for LiveToolGate {
    fn authorize<'a>(
        &'a self,
        dispatch: &'a str,
        execution: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send + 'a>> {
        Box::pin(self.check(dispatch, execution))
    }
}
impl LiveToolGate {
    async fn check(&self, dispatch: &str, execution: &str) -> bool {
            if *self.cancel.borrow() || dispatch != self.expected.id || execution != self.execution
            {
                return false;
            }
            let mut receiver = self.cancel.clone();
            let response = operation(
                &self.console,
                &mut receiver,
                Op::AuthorizeTool {
                    lease: self.lease.clone(),
                    dispatch_id: dispatch.into(),
                    execution_id: execution.into(),
                },
            )
            .await;
            let Ok(Response::ToolAuthorized(event)) = response else {
                return false;
            };
            let Ok(event) = convert(event) else {
                return false;
            };
            !*self.cancel.borrow()
                && event.id == self.expected.id
                && event.scope() == self.expected.scope()
                && event.event_id == self.expected.event_id
                && event.body == self.expected.body
                && event.binding_generation == self.expected.binding_generation
                && event.dispatch_epoch == Some(self.lease.epoch)
                && event.dispatch_device_id == self.expected.dispatch_device_id
                && event.execution_id.as_deref() == Some(execution)
                && event.state == "running"
    }
}
/// Best-effort cancellation: 5s sampling plus at most 5s for the proof RPC.
/// An unprovable current running scope cancels only this binding's turn.
async fn monitor_running<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    prepared: &inbox::Prepared,
    mut cancel: watch::Receiver<bool>,
) -> RuntimeError {
    loop {
        tokio::select! {biased;_=canceled(&mut cancel)=>return RuntimeError::Stopped,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
        match tokio::time::timeout(
            Duration::from_secs(5),
            check_running(
                transport,
                lease,
                &prepared.dispatch,
                &prepared.execution_id,
                &mut cancel,
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return error,
            Err(_) => return RuntimeError::Transport,
        }
    }
}
/// Best effort only: this notification cannot trigger or retry provider work.
async fn processing_notice<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    prepared: &inbox::Prepared,
    started: tokio::sync::oneshot::Receiver<()>,
) {
    if started.await.is_err() {
        return;
    }
    for attempt in 0..2 {
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            transport.execute(Op::Processing {
                lease: lease.clone(),
                dispatch_id: prepared.dispatch.id.clone(),
                execution_id: prepared.execution_id.clone(),
            }),
        )
        .await;
        match result {
            Ok(Ok(Response::Processing(_))) => {
                tracing::debug!(
                    stage = "processing_notice",
                    attempt,
                    "Agent processing reaction confirmed"
                );
                return;
            }
            Ok(Err(RuntimeError::Authorization)) => {
                tracing::warn!(
                    stage = "processing_notice",
                    code = "authorization_required",
                    "Agent processing reaction rejected"
                );
                return;
            }
            _ if attempt == 0 => tokio::time::sleep(Duration::from_millis(200)).await,
            _ => tracing::warn!(
                stage = "processing_notice",
                code = "processing_notice_unavailable",
                "Agent processing reaction was not confirmed"
            ),
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn review_request<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    ledger: &mut Ledger,
    owner: &str,
    prepared: &inbox::Prepared,
    input: &str,
    cwd: &std::path::Path,
    queue: &codex::ApprovalQueue,
    cancel: &mut watch::Receiver<bool>,
) -> Result<bool, RuntimeError> {
    let scope = prepared.dispatch.scope();

    use sha2::{Digest, Sha256};
    let revisions = local(ledger.policy_snapshot(&scope))?.map(|v| v.revision);
    let proposal = hagency_agent_local::ToolProposal {
        scope: scope.clone(),
        dispatch: prepared.dispatch.id.clone(),
        tool: "model.request".into(),
        arguments: serde_json::json!({"executionId":prepared.execution_id,"inputSha256":format!("{:x}",Sha256::digest(input.as_bytes())),"inputPreview":input.chars().take(2048).collect::<String>()}),
        canonical_directory: cwd.to_string_lossy().into(),
        risk: "model_request".into(),
        policy_revision: revisions,
        expires: now() + 300,
    };
    let (pending, decision) = codex::pending_approval(proposal.clone());
    let accepted = queue.offer(pending)
        && tokio::select! {biased;_=canceled(cancel)=>false,r=tokio::time::timeout(Duration::from_secs(300),decision)=>matches!(r,Ok(Ok(true)))};
    Ok(accepted
        && fresh_running(
            transport,
            lease,
            &prepared.dispatch,
            &prepared.execution_id,
            cancel,
        )
        .await
        && ledger
            .approve_request_exact(
                owner,
                &scope,
                &prepared.dispatch.id,
                revisions,
                proposal.expires,
                now(),
            )
            .is_ok())
}
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all, fields(agent_id = %config.agent_id, binding_id = %config.binding_id, dispatch_id = %prepared.dispatch.id))]
async fn run_turn<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    ledger: &mut Ledger,
    owner: &str,
    config: &StartConfig,
    prepared: inbox::Prepared,
    mut admission: Option<super::hosting::TurnAdmission>,
    cancel: &mut watch::Receiver<bool>,
    status: &watch::Sender<RuntimeStatus>,
    sessions: &mut provider_custody::Sessions,
) -> Result<Option<Finish>, RuntimeError> {
    let scope = prepared.dispatch.scope();
    if config.shared_workspace && config.shared_workspace_pin.as_deref() != Some(shared_workspace_pin(&config.profile.cwd)?.as_str()) {
        return Err(RuntimeError::Profile);
    }

    // An empty body marks an encrypted owner-direct request. Only the Agent's
    // own device reads it, before any provider work starts.
    let device = if prepared.dispatch.body.is_empty() {
        Some(e2ee::device(transport, lease, config, cancel).await?)
    } else {
        None
    };
    let input = match &device {
        None => prepared.dispatch.body.clone(),
        Some(device) => match device.read_request(&prepared.dispatch, cancel).await? {
            Some(text) => text,
            None => {
                device.notice_unreadable(&prepared.dispatch).await;
                return Ok(Some(Finish::Rejected));
            }
        },
    };
    #[cfg(not(unix))]
    let profile = config.profile.clone();
    #[cfg(unix)]
    let (profile, workspace) = working_directory::for_turn(
        &config.profile,
        owner,
        &scope,
        config.host_files,
        config.shared_workspace,
        config.shared_workspace_pin.as_deref(),
    )?;
    let (queue, mut receiver) = codex::approval_queue(1).map_err(|_| RuntimeError::Profile)?;
    let (question_queue, question_receiver) = hagency_agent_local::questions::question_queue();
    let mut queue = queue.with_questions(question_queue);
    let host = transport.approval_host();
    let controls =
        if host.is_some() && runtime::kind(&config.credential_ref) != runtime::Kind::Octos {
            let kind = if runtime::kind(&config.credential_ref) == runtime::Kind::Claude {
                "claude"
            } else {
                "codex"
            };
            let card = run_controls::optional_card(
                &prepared.dispatch,
                &prepared.execution_id,
                owner,
                kind,
            );
            if let Some(card) = card {
                let (sender, inbox) = hagency_agent_local::run_controls::channel(
                    &scope, &prepared.dispatch.id, &prepared.execution_id,
                ).map_err(|_| RuntimeError::Profile)?;
                queue = queue.with_run_controls(inbox);
                let controls = run_controls::Controller::new(sender, card);
                queue = queue.with_live_text(controls.live_text());
                Some(controls)
            } else { None }
        } else {
            None
        };
    let has_approval_host = host.is_some();
    let bridge = host.clone().map(|console| {
        tokio::spawn(async move {
            while let Some(pending) = receiver.recv().await {
                if console
                    .console
                    .0
                    .owned_runtime
                    .offer_approval(&console.console, &console.pin, pending)
                    .await
                    .is_err()
                {
                    break;
                }
            }
        })
    });
    let _approval_bridge = bridge.map(|task| AbortHeartbeat(task.abort_handle()));
    let disposition = local(ledger.request_disposition(&scope, &prepared.dispatch.id))?;
    let allow = match disposition {
        hagency_agent_local::RequestPolicy::Allow => true,
        hagency_agent_local::RequestPolicy::Deny => false,
        hagency_agent_local::RequestPolicy::AskOwner if !has_approval_host => false,
        hagency_agent_local::RequestPolicy::AskOwner => {
            review_request(
                transport,
                lease,
                ledger,
                owner,
                &prepared,
                &input,
                &profile.cwd,
                &queue,
                cancel,
            )
            .await?
        }
    };
    if !allow {
        return if *cancel.borrow() {
            Err(RuntimeError::Stopped)
        } else {
            local(ledger.persist_no_provider_reply(
                owner,
                &prepared.dispatch.id,
                &prepared.execution_id,
                "⏸️ 本次请求未获 Agent 主人的处理授权，未调用模型。",
            ))?;
            Ok(None)
        };
    }
    let mut room_bridge = if runtime::kind(&config.credential_ref) != runtime::Kind::Octos {
        if let Some(console) = host.as_ref() {
            let gate = Arc::new(LiveToolGate { console: console.clone(), lease: lease.clone(),
                expected: prepared.dispatch.clone(), execution: prepared.execution_id.clone(), cancel: cancel.clone() });
            let host = room_tools::Host::new(gate, config, &profile.cwd)?;
            let (channel, receiver) = hagency_agent_local::room_tools::room_queue(&scope, &prepared.dispatch.id)
                .map_err(|_| RuntimeError::Profile)?;
            queue = queue.with_room_tools(channel);
            Some((host, receiver))
        } else { None }
    } else { None };
    let room_tools = room_bridge.is_some();
    let input = match room_bridge.as_mut() {
        Some((host, _)) => host.prompt(input).await,
        None => input,
    };
    // Native extensions may execute during startup. Admit the request before
    // opening a provider or disturbing the retained conversation.
    let provider_tasks = transport
        .approval_host()
        .map(|host| host.console.0.owned_runtime.tasks.clone())
        .unwrap_or_default();
    let session_key = provider_custody::SessionKey::new(owner, &prepared.dispatch, &profile, config);
    let cached = if let Some(mut provider) = sessions.take(&session_key) {
        let prepared = tokio::select! { biased;
            _ = canceled(cancel) => return Err(RuntimeError::Stopped),
            result = tokio::time::timeout(Duration::from_secs(10), async {
                provider.session.verify_host_environment().await?;
                provider.session.require_local_account().await?;
                provider.session.continue_context(ledger, &scope, &profile).await
            }) => result.unwrap_or(Err(codex::Error::Timeout)),
        };
        if prepared.is_ok() { Some(provider) } else {
            // No new model input or reservation has been sent. A dead cached
            // process may be replaced for this newly admitted dispatch only.
            provider.stop().await.map_err(|_| RuntimeError::Provider)?;
            None
        }
    } else { None };
    let retained = cached.is_some();
    let mut provider = match cached {
        Some(provider) => provider,
        None => {
            let provider = tokio::select! {biased; _=canceled(cancel)=>return Err(RuntimeError::Stopped), p=provider_custody::Provider::capture(&provider_tasks,provider_custody::spawn_for_scope(&profile,&config.credential_ref,ledger,&scope))=>p.map_err(|error|provider_failure("provider_spawn", &error))?};
            // The only discards of a saved session: the native CLI reported
            // its context missing before any input, or the same saved session
            // failed to start three times in a row and a fresh one started. A
            // notice to the room that the earlier conversation is gone would
            // be sent from here.
            if let Some(reason) = provider.discarded_session {
                tracing::warn!(
                    code = "claude_saved_session_discarded",
                    scope = "personal",
                    reason,
                    "A saved Claude session was discarded; this conversation continues in a fresh one"
                );
            }
            provider
        }
    };
    if config.host_files || config.shared_workspace {
        #[cfg(unix)]
        {
            let console = host.as_ref().ok_or(RuntimeError::Profile)?;
            let gate = Arc::new(LiveToolGate {
                console: console.clone(),
                lease: lease.clone(),
                expected: prepared.dispatch.clone(),
                execution: prepared.execution_id.clone(),
                cancel: cancel.clone(),
            });
            let workspace = workspace.ok_or(RuntimeError::Profile)?;
            if retained { provider.session.rebind_host_files(workspace, gate) }
            else { provider.session.enable_host_files(workspace, gate) }
                .map_err(|_| RuntimeError::Profile)?;
        }
        #[cfg(not(unix))]
        {
            return Err(RuntimeError::Profile);
        }
    }
    let mut executing = RuntimeStatus::new(
        &config.agent_id,
        if config.host_files || config.shared_workspace {
            "executing_host_files"
        } else {
            "executing_chat_only"
        },
        None,
    )
    .with_capabilities(config);
    executing.binding_id = Some(config.binding_id.clone());
    status.send_replace(executing);
    // The launch/dispatch has already passed owner, room and provider gates.
    // Keep this identity with the observer, including failed terminal turns.
    let telemetry = transport.approval_host().as_ref().and_then(|host| {
        host.console
            .0
            .dashboard_telemetry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    });
    let binding = hagency_metering::dashboard::ledger::ExecutionBinding {
        execution_id: prepared.execution_id.clone(),
        resource_id: format!("personal-agent:{}", config.agent_id),
        account_id: None,
        allocation_id: None,
        agent_id: Some(config.agent_id.clone()),
        requester_id: Some(scope.requester.clone()),
        room_id: Some(scope.room.clone()),
        runtime: match runtime::kind(&config.credential_ref) {
            runtime::Kind::Codex => hagency_metering::dashboard::ledger::Runtime::Codex,
            runtime::Kind::Claude => hagency_metering::dashboard::ledger::Runtime::Claude,
            runtime::Kind::Octos => hagency_metering::dashboard::ledger::Runtime::Octos,
        },
        runtime_version: None,
        model: Some(profile.model.clone()),
        parent_execution_id: None,
        started_at_ms: super::native::telemetry::now_ms(),
    };
    let usage_observer = |usage: &hagency_agent_local::Usage| {
        if let Some(telemetry) = &telemetry {
            if telemetry.personal_usage(&binding, usage).is_err() {
                tracing::warn!(
                    code = "personal_usage_unavailable",
                    "Personal runtime measurement could not be recorded"
                );
            }
        }
    };
    let mut pre_model_cancel = cancel.clone();
    let permission_cancel = cancel.clone();
    let question_cancel = cancel.clone();
    let control_cancel = cancel.clone();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let result = tokio::select! {biased; _=canceled(cancel)=>Err(RuntimeError::Stopped), error=monitor_running(transport,lease,&prepared,permission_cancel)=>Err(error), r=async {
        let model = async {
        if !retained {
            provider.session.initialize().await.inspect_err(|error|{provider_failure("provider_initialize", error);})?;
            provider.session.verify_host_environment().await.inspect_err(|error|{provider_failure("provider_environment", error);})?;
            provider.session.require_local_account().await.inspect_err(|error|{provider_failure("provider_account", error);})?;
            if room_tools {
                provider.session.enable_room_tools(hagency_runtime::room_tools::RoomTools::history_and_files()).await
                    .inspect_err(|error|{provider_failure("provider_room_tools", error);})?;
            }
            provider.session.open_context(ledger,&scope,&profile).await.inspect_err(|error|{provider_failure("provider_context", error);})?;
        }
        if !fresh_running(transport,lease,&prepared.dispatch,&prepared.execution_id,&mut pre_model_cancel).await {return Err(codex::Error::Protocol("running dispatch authorization lost"));}
        provider.session.run_with_started_observed(ledger,owner,&scope,&prepared.execution_id,&prepared.dispatch.id,&input,BudgetMode::Postpaid,&queue,Some(started_tx),Some(&usage_observer)).await.inspect_err(|error|{provider_failure("provider_turn", error);})
            // The answer was read before the provider stream failed: it is still delivered, with unknown usage.
            .inspect(|completed|{if let Some(error) = &completed.late_failure {provider_failure("provider_turn_after_answer", error);}})
        };
        let model = async {
            tokio::select! {
                result = model => result,
                _ = questions::serve(transport, lease, config, owner, &prepared, question_cancel, question_receiver) => Err(codex::Error::Protocol("question bridge closed")),
                _ = async {
                    match room_bridge {
                        Some((host, receiver)) => host.serve(receiver).await,
                        None => std::future::pending::<()>().await,
                    }
                } => Err(codex::Error::Protocol("room bridge closed")),
                _ = async {
                    if let Some(controls) = &controls {
                        controls.serve(transport, lease, config, &prepared, control_cancel).await;
                    }
                    // An unavailable Matrix card cannot abort a paid turn.
                    std::future::pending::<()>().await
                } => unreachable!(),
            }
        };
        let notice = async {
            if started_rx.await.is_ok() {
                if let Some(turn) = admission.as_mut() { turn.started(); }
                match &device {
                    Some(device) => device.mark_processing(&prepared.dispatch).await,
                    None => {
                        let (send, receive) = tokio::sync::oneshot::channel();
                        let _ = send.send(());
                        processing_notice(transport,lease,&prepared,receive).await;
                    }
                }
            }
        };
        tokio::pin!(model, notice);
        tokio::select! {
            result = &mut model => result,
            _ = &mut notice => model.await,
        }
    }=>match r {
        Ok(completed)=>Ok(Ok(completed)),
        Err(codex::Error::Ledger(hagency_agent_local::Error::Denied))=>Ok(Err("⏸️ 本次请求未获 Agent 主人的处理授权，未调用模型。")),
        Err(codex::Error::Ledger(hagency_agent_local::Error::Budget))=>Ok(Err("⏸️ 已结算用量达到本期额度，暂不启动新任务。请联系 Agent 主人调整配额。本次未调用模型。")),
        Err(codex::Error::Failed)=>{if let Some(controls) = &controls { controls.finish(crate::agent_run_controls::State::Failed); } provider.stop().await.map_err(|_|RuntimeError::Provider)?;return Ok(Some(Finish::Failed));},
        Err(error)=>Err(provider_failure("agent_turn_result", &error)),
    }};
    if let Some(controls) = &controls {
        use crate::agent_run_controls::State;
        controls.finish(match &result {
            Ok(Ok(completed)) if completed.status == codex::TurnStatus::Interrupted => {
                State::Interrupted
            }
            Ok(Ok(_)) => State::Completed,
            Ok(Err(_)) => State::Failed,
            Err(_) => State::Unknown,
        });
    }
    let retain = matches!(&result, Ok(Ok(completed)) if completed.usage.is_reported())
        && provider.session.can_continue();
    // Failed/cancelled/unknown work never enters the idle-session cache.
    if !retain { provider.stop().await.map_err(|_| RuntimeError::Provider)?; }
    let completed = match result? {
        Ok(completed) => completed,
        Err(reason) => {
            local(ledger.persist_no_provider_reply(
                owner,
                &prepared.dispatch.id,
                &prepared.execution_id,
                reason,
            ))?;
            return Ok(None);
        }
    };
    let reply = if completed.status == codex::TurnStatus::Interrupted && completed.text.is_empty() {
        "⏹️ 本次运行已停止。"
    } else {
        &completed.text
    };
    if reply.is_empty() || reply.len() > 65_536 {
        return Err(RuntimeError::Profile);
    }
    // Like shared Agents, a chat reply is only the answer. Turn usage and the
    // Agent budget stay in the ledger and the owner's Agent limits page.
    local(ledger.persist_execution_reply(
        owner,
        &prepared.dispatch.id,
        &prepared.execution_id,
        reply,
    ))?;
    if retain { sessions.retain(session_key, provider); }
    Ok(None)
}

// A historical delivery failure is scoped to its immutable reply. Retry on a
// later pass, leaving polling and later requests free to make progress.
async fn retry_reply<T: Transport>(
    console: &T,
    owner: &str,
    ledger: &mut Ledger,
    lease: &LeaseRef,
    cancel: &mut watch::Receiver<bool>,
    config: &StartConfig,
    record: &inbox::Record,
) -> Result<(), RuntimeError> {
    let sent = if record.dispatch.body.is_empty() {
        send_encrypted_reply(console, owner, ledger, lease, cancel, config, record).await
    } else {
        send_reply(console, owner, ledger, lease, cancel, record).await
    };
    match sent {
        Err(RuntimeError::Stopped) => Err(RuntimeError::Stopped),
        Err(error) => {
            tracing::warn!(stage="agent_reply", dispatch_id=%record.dispatch.id, %error,
                "Reply remains durable; later requests may continue");
            Ok(())
        }
        Ok(()) => Ok(()),
    }
}
/// An empty dispatch body marks an encrypted owner-direct request. The Agent
/// device sends the encrypted answer; the server records only its event ID.
async fn send_encrypted_reply<T: Transport>(
    console: &T,
    owner: &str,
    ledger: &mut Ledger,
    lease: &LeaseRef,
    cancel: &mut watch::Receiver<bool>,
    config: &StartConfig,
    record: &inbox::Record,
) -> Result<(), RuntimeError> {
    let execution = record.execution_id.clone().ok_or(RuntimeError::Profile)?;
    let body = record.reply.as_deref().ok_or(RuntimeError::Profile)?;
    let device = e2ee::device(console, lease, config, cancel).await?;
    let matrix_event_id = device.send_reply(&record.dispatch, body).await?;
    let recorded = operation(
        console,
        cancel,
        Op::EncryptedReply {
            lease: lease.clone(),
            dispatch_id: record.dispatch.id.clone(),
            execution_id: execution.clone(),
            matrix_event_id,
        },
    )
    .await;
    match recorded {
        Ok(Response::ReplyQueued(receipt))
            if receipt.binding_id == record.dispatch.binding_id
                && receipt.room_id == record.dispatch.room_id
                && receipt.binding_generation == record.dispatch.binding_generation => {}
        // The owner already has the answer. Never resend it because the server
        // refused to record this one event (execution expired, lease changed).
        Err(RuntimeError::Request) => {
            tracing::warn!(stage="agent_reply", dispatch_id=%record.dispatch.id,
                "Encrypted reply delivered; the server did not record its receipt");
        }
        Err(error) => return Err(error),
        Ok(_) => return Err(RuntimeError::Transport),
    }
    local(ledger.confirm_matrix_delivery(owner, &record.dispatch.id, &execution))
}
async fn send_reply<T: Transport>(
    console: &T,
    owner: &str,
    ledger: &mut Ledger,
    lease: &LeaseRef,
    cancel: &mut watch::Receiver<bool>,
    record: &inbox::Record,
) -> Result<(), RuntimeError> {
    let execution = record.execution_id.clone().ok_or(RuntimeError::Profile)?;
    let body = record.reply.clone().ok_or(RuntimeError::Profile)?;
    let receipt = match operation(
        console,
        cancel,
        if record.dispatch.dispatch_epoch == Some(lease.epoch) {
            Op::Reply {
                lease: lease.clone(),
                dispatch_id: record.dispatch.id.clone(),
                execution_id: execution.clone(),
                body: body.clone(),
            }
        } else {
            Op::ReconcileKnownReply {
                lease: lease.clone(),
                known: super::device_execution::KnownReply {
                    dispatch_id: record.dispatch.id.clone(),
                    execution_id: execution.clone(),
                    body: body.clone(),
                    binding_id: record.dispatch.binding_id.clone(),
                    room_id: record.dispatch.room_id.clone(),
                    requester_mxid: record.dispatch.requester_mxid.clone(),
                    thread_root: record.dispatch.thread_root.clone(),
                    binding_generation: record.dispatch.binding_generation,
                    original_epoch: record
                        .dispatch
                        .dispatch_epoch
                        .ok_or(RuntimeError::Profile)?,
                },
            }
        },
    )
    .await
    {
        Ok(Response::ReplyQueued(receipt)) => receipt,
        Err(error) => return Err(error),
        _ => return Err(RuntimeError::Transport),
    };
    if receipt.binding_id != record.dispatch.binding_id
        || receipt.room_id != record.dispatch.room_id
        || receipt.binding_generation != record.dispatch.binding_generation
    {
        return Err(RuntimeError::Transport);
    }
    // Durable server acceptance can be cancelled after device lease expiry.
    // Keep the immutable body retryable until a Matrix event proves delivery.
    if receipt.state == "sent"
        && receipt
            .matrix_event_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
    {
        local(ledger.confirm_matrix_delivery(owner, &record.dispatch.id, &execution))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn provider_failure_preserves_execution_category_in_runtime_status() {
        for (error, code) in [
            (
                super::codex::Error::Protocol("cross-turn event"),
                "codex_protocol_mismatch",
            ),
            (
                super::codex::Error::Protocol(hagency_runtime::codex::version::REQUIRED),
                "codex_version_unsupported",
            ),
            (super::codex::Error::Unknown, "codex_provider_unavailable"),
            (super::codex::Error::Timeout, "codex_provider_timeout"),
            (
                super::codex::Error::Profile("missing executable or directory"),
                "profile_or_local_state_unavailable",
            ),
        ] {
            let failure = super::provider_failure("fixture", &error);
            assert_eq!(super::failure_code(&Err(failure)), Some(code));
        }
    }

    #[test]
    fn owner_agent_account_shows_spent_separately_from_holds() {
        use hagency_agent_local::{
            Budget, Layer, Limit, Period, Policy, RequestPolicy, ToolPolicy, Usage,
        };
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut ledger = Ledger::open(tmp.path().join("ledger.db"), OWNER).unwrap();
        let scope = Scope {
            agent: "agent-a".into(),
            binding: "b".into(),
            room: "!r:test".into(),
            requester: OWNER.into(),
            thread: "t".into(),
        };
        ledger.register_binding(OWNER, &scope).unwrap();
        for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
            ledger
                .set_policy(
                    OWNER,
                    &scope,
                    layer,
                    0,
                    &Policy {
                        budget: Budget {
                            limit: Limit::Tokens(100),
                            period: Period::Lifetime,
                        },
                        requests: RequestPolicy::Allow,
                        high_risk: ToolPolicy::Deny,
                    },
                )
                .unwrap();
        }
        ledger
            .reserve(&scope, "call-a", "dispatch-a", 30, now())
            .unwrap();
        let usage = Usage {
            input: 15,
            output: 5,
            cached_input: 3,
            cache_write_input: None,
            reasoning_output: 1,
            accounting_version: "fixture-v1".into(),
        };
        ledger.settle(&scope, "call-a", &usage).unwrap();
        ledger
            .reserve(&scope, "call-b", "dispatch-b", 10, now())
            .unwrap();
        assert_eq!(
            ledger
                .agent_account(OWNER, "agent-a", Period::Lifetime, now())
                .unwrap(),
            (20, 10)
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_workspace_pin_changes_when_directory_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let project = project.canonicalize().unwrap();
        let first = shared_workspace_pin(&project).unwrap();
        std::fs::rename(&project, temp.path().join("old-project")).unwrap();
        std::fs::create_dir(&project).unwrap();
        assert_ne!(first, shared_workspace_pin(&project).unwrap());
    }
    #[test]
    fn heartbeat_schedule_precedes_short_proof_deadline() {
        let now = std::time::Instant::now();
        assert_eq!(
            super::heartbeat_next(now, now + std::time::Duration::from_secs(30)),
            now + std::time::Duration::from_secs(5)
        );
        assert_eq!(
            super::heartbeat_next(now, now + std::time::Duration::from_secs(4)),
            now + std::time::Duration::from_secs(2)
        );
        assert_eq!(super::heartbeat_next(now, now), now);
    }
    use super::super::device_execution::{ExecutionStart, ReplyReceipt};
    use super::*;
    use std::sync::Mutex as StdMutex;
    const OWNER: &str = "@alice:test";
    fn intended(agent: &str, binding: &str, device: &str) -> RuntimeIntent {
        RuntimeIntent {
            agent_id: agent.into(),
            binding_id: binding.into(),
            device_id: device.into(),
            consent_id: new_consent_id().unwrap(),
            effort: "low".into(),
            host_files: false,
            shared_workspace: false,
            shared_workspace_pin: None,
        }
    }
    #[tokio::test]
    async fn runtime_restart_retains_explicit_binding_consent_but_never_a_live_task_or_takeover() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("runtime-intents.json");
        let intent = intended("a", "b", "device-a");
        change_intent(&path, "owner-a", "a", "b", Some(intent.clone())).unwrap();
        let old = OwnedRuntime::default();
        old.stop_all().await; // normal shutdown preserves durable intent
        drop(old);
        let fresh = OwnedRuntime::default();
        assert!(fresh.entries.lock().await.is_empty());
        let recovered = read_intents(&path, "owner-a").unwrap().intents;
        assert_eq!(recovered, vec![intent]);
        assert!(
            intents_for_device(read_intents(&path, "owner-a").unwrap(), "another-device")
                .is_empty()
        );
        let input = recovered[0].start_input();
        assert_eq!(input["takeover"], false);
        assert!(input.get("reservation").is_none());
        assert_eq!(input["recoveryDeviceId"], "device-a");
        assert!(input.get("profile").is_none());
        assert!(input.get("token").is_none());
    }
    #[test]
    fn runtime_stop_removes_only_exact_binding_and_keeps_other_intents_across_reopen() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("runtime-intents.json");
        for (a, b) in [("a", "b1"), ("a", "b2"), ("other", "b3")] {
            change_intent(&path, "owner-a", a, b, Some(intended(a, b, "device-a"))).unwrap();
        }
        change_intent(&path, "owner-a", "a", "b1", None).unwrap();
        let loaded = read_intents(&path, "owner-a").unwrap();
        assert_eq!(loaded.intents.len(), 2);
        assert!(loaded.intents.iter().all(|i| i.binding_id != "b1"));
        assert!(read_intents(&path, "different-account").is_err());
        assert!(change_intent(&path, "different-account", "other", "b3", None).is_err());
        assert_eq!(read_intents(&path, "owner-a").unwrap().intents.len(), 2);
    }
    #[test]
    fn runtime_stale_recovery_cannot_republish_after_other_manager_stop_or_new_start() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("runtime-intents.json");
        let original = intended("a", "b", "device");
        change_intent(&path, "owner-a", "a", "b", Some(original.clone())).unwrap();
        let snapshot = read_intents(&path, "owner-a").unwrap().intents.remove(0);
        change_intent(&path, "owner-a", "a", "b", None).unwrap();
        assert!(matches!(
            change_intent_checked(
                &path,
                "owner-a",
                "a",
                "b",
                Some(snapshot.clone()),
                Some(&snapshot.consent_id)
            ),
            Err(RuntimeError::Stopped)
        ));
        assert!(read_intents(&path, "owner-a").unwrap().intents.is_empty());
        let newer = intended("a", "b", "device");
        change_intent(&path, "owner-a", "a", "b", Some(newer.clone())).unwrap();
        assert!(matches!(
            change_intent_checked(
                &path,
                "owner-a",
                "a",
                "b",
                Some(snapshot.clone()),
                Some(&snapshot.consent_id)
            ),
            Err(RuntimeError::Stopped)
        ));
        assert_eq!(
            read_intents(&path, "owner-a").unwrap().intents,
            vec![newer.clone()]
        );
        assert!(
            change_intent_checked(
                &path,
                "owner-a",
                "a",
                "b",
                Some(newer.clone()),
                Some(&newer.consent_id)
            )
            .is_ok()
        );
    }
    #[test]
    fn postpaid_start_intent_survives_restart_without_an_estimate() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("runtime-intents.json");
        let intent = intended("agent", "binding", "device");
        change_intent(&path, "owner", "agent", "binding", Some(intent.clone())).unwrap();
        assert_eq!(read_intents(&path, "owner").unwrap().intents, vec![intent.clone()]);
        assert!(intent.start_input().get("reservation").is_none());
        let mut old = serde_json::to_value(&intent).unwrap();
        old["reservation"] = 42.into();
        assert!(serde_json::from_value::<RuntimeIntent>(old).is_err());
    }
    #[test]
    fn obsolete_runtime_intents_do_not_restore_or_block_a_fresh_start_or_stop() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("runtime-intents.json");
        let old = serde_json::json!({"version":2,"profileIdentity":"owner","intents":[{
            "agentId":"old","bindingId":"old-room","deviceId":"old-device",
            "consentId":"a".repeat(64),"effort":"low","hostFiles":false,"reservation":42
        }]});
        hagency_store::private::replace(&path, &serde_json::to_vec(&old).unwrap()).unwrap();
        assert!(read_intents(&path, "other-owner").is_err());
        assert!(read_intents(&path, "owner").unwrap().intents.is_empty());
        change_intent(&path, "owner", "old", "old-room", None).unwrap();
        assert_eq!(read_intents(&path, "owner").unwrap().version, RUNTIME_STATE_VERSION);
        let next = intended("current", "current-room", "current-device");
        change_intent(&path, "owner", "current", "current-room", Some(next.clone())).unwrap();
        assert_eq!(read_intents(&path, "owner").unwrap().intents, vec![next]);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("reservation"));
    }
    #[test]
    fn runtime_concurrent_intent_writer_and_malformed_consent_fail_closed() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("runtime-intents.json");
        let held = hagency_store::private::open(&path.with_file_name("runtime-intents.lock"), true)
            .unwrap();
        held.try_lock().unwrap();
        assert!(change_intent(&path, "owner-a", "a", "b", Some(intended("a", "b", "d"))).is_err());
        assert!(!path.exists());
        drop(held);
        change_intent(&path, "owner-a", "a", "b", Some(intended("a", "b", "d"))).unwrap();
        let mut values = read_intents(&path, "owner-a").unwrap();
        values.intents[0].consent_id = "malformed".into();
        hagency_store::private::replace(&path, &serde_json::to_vec(&values).unwrap()).unwrap();
        assert!(read_intents(&path, "owner-a").is_err());
    }
    #[test]
    fn two_process_like_hosts_cannot_share_one_agent_context() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("ledger.db");
        let first = lock_agent(&path, "agent-a").unwrap();
        assert!(matches!(
            lock_agent(&path, "agent-a"),
            Err(RuntimeError::AlreadyActive)
        ));
        assert!(lock_agent(&path, "agent-b").is_ok());
        drop(first);
        assert!(lock_agent(&path, "agent-a").is_ok());
    }
    #[tokio::test]
    async fn multi_profile_revoked_inflight_registration_cannot_reenter_cleared_registry() {
        let runtime = OwnedRuntime::default();
        let (old, revoked) = super::super::server_login::fixture_device(
            "https://server.example/",
            "subject-a",
            OWNER,
        );
        let stale_logout = old.clone();
        let guard = runtime.entries.lock().await;
        let worker = runtime.clone();
        let queued = tokio::spawn(async move {
            let _guard = worker.entries_for(&old).await?;
            Ok::<_, RuntimeError>(())
        });
        tokio::task::yield_now().await;
        revoked.store(true, std::sync::atomic::Ordering::Release);
        drop(guard);
        runtime.stop_all().await;
        assert!(matches!(
            queued.await.unwrap(),
            Err(RuntimeError::Authorization)
        ));
        assert!(runtime.entries.lock().await.is_empty());
        let (fresh, _) = super::super::server_login::fixture_device(
            "https://server.example/",
            "subject-b",
            OWNER,
        );
        assert!(runtime.entries_for(&fresh).await.is_ok());
        // A -> B -> A: the new A session must survive delayed old-A logout.
        let (returned_a, _) = super::super::server_login::fixture_device(
            "https://server.example/",
            "subject-a",
            OWNER,
        );
        let (cancel, mut receiver) = watch::channel(false);
        let (_, status) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        let task = tokio::spawn(async move { canceled(&mut receiver).await });
        runtime.entries_for(&returned_a).await.unwrap().insert(
            "returned-a".into(),
            Entry {
                commands: mpsc::unbounded_channel().0,
                bindings: BTreeMap::new(),
                cancel,
                status,
                task: runtime.tasks.track(task),
                device: DevicePin::from(&returned_a),
                approvals: BTreeMap::new(),
            },
        );
        assert!(matches!(
            runtime.stop_profile(&stale_logout).await,
            Err(RuntimeError::Authorization)
        ));
        let entries = runtime.entries.lock().await;
        assert!(entries.contains_key("returned-a"));
        assert!(!*entries["returned-a"].cancel.borrow());
        drop(entries);
        runtime.stop_profile(&returned_a).await.unwrap();
        assert!(runtime.entries.lock().await.is_empty());
    }
    #[tokio::test]
    async fn retire_while_registry_locked_cancels_worker_without_blocking() {
        let runtime = OwnedRuntime::default();
        let (cancel, mut receiver) = watch::channel(false);
        let (status, status_receiver) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        let (done, finished) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            canceled(&mut receiver).await;
            let _ = done.send(());
            drop(status);
        });
        let mut guard = runtime.entries.lock().await;
        guard.insert(
            "test".into(),
            Entry {
                commands: mpsc::unbounded_channel().0,
                bindings: BTreeMap::new(),
                cancel,
                status: status_receiver,
                task: runtime.tasks.track(task),
                device: DevicePin {
                    origin: "test".into(),
                    issuer: "test-issuer".into(),
                    subject: "test-subject".into(),
                    owner: "test".into(),
                    user: "test".into(),
                    id: "test-device".into(),
                    generation: 1,
                },
                approvals: BTreeMap::new(),
            },
        );
        runtime.request_stop_all();
        drop(guard);
        tokio::time::timeout(Duration::from_secs(2), finished)
            .await
            .unwrap()
            .unwrap();
        runtime.stop_all().await;
        assert!(runtime.entries.lock().await.is_empty());
    }
    #[test]
    fn stable_room_context_requires_trusted_binding_and_isolates_each_member() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut configuration = config(temp.path());
        let mut first = event();
        first.thread_root = first.room_id.clone();
        first.requester_mxid = OWNER.into();
        assert!(validate_event_context(&first, &configuration, OWNER).is_err());
        configuration.owner_direct = true;
        validate_event_context(&first, &configuration, OWNER).unwrap();
        let mut next = first.clone();
        next.id = "dispatch-next".into();
        next.event_id = "$next:test".into();
        next.body = "next message".into();
        validate_event_context(&next, &configuration, OWNER).unwrap();
        assert_eq!(
            first.scope().context_key(OWNER).unwrap(),
            next.scope().context_key(OWNER).unwrap()
        );
        next.thread_root = "$explicit-thread:test".into();
        validate_event_context(&next, &configuration, OWNER).unwrap();
        assert_ne!(
            first.scope().context_key(OWNER).unwrap(),
            next.scope().context_key(OWNER).unwrap()
        );
        next.requester_mxid = "@foreign:test".into();
        validate_event_context(&next, &configuration, OWNER).unwrap();
        assert_ne!(
            first.scope().context_key(OWNER).unwrap(),
            next.scope().context_key(OWNER).unwrap()
        );
        next.requester_mxid = OWNER.into();
        next.thread_root = "!another-room:test".into();
        assert!(validate_event_context(&next, &configuration, OWNER).is_err());
    }
    fn event() -> inbox::Dispatch {
        inbox::Dispatch {
            id: "dispatch-a".into(),
            binding_id: "binding-a".into(),
            agent_id: "agent-a".into(),
            event_id: "$event-a:test".into(),
            room_id: "!room-a:test".into(),
            requester_mxid: "@bob:test".into(),
            thread_root: "$root:test".into(),
            body: "request".into(),
            state: "offered".into(),
            binding_generation: 1,
            dispatch_epoch: Some(1),
            dispatch_device_id: Some("device-a".into()),
            execution_id: None,
            outcome: None,
        }
    }
    fn wire(event: &inbox::Dispatch) -> super::super::device_execution::Dispatch {
        serde_json::from_value(serde_json::to_value(event).unwrap()).unwrap()
    }
    fn lease() -> LeaseRef {
        LeaseRef {
            agent_id: "agent-a".into(),
            epoch: 1,
        }
    }
    fn config(path: &std::path::Path) -> StartConfig {
        StartConfig {
            agent_id: "agent-a".into(),
            binding_id: "binding-a".into(),
            profile: Profile {
                executable: path.join("must-not-spawn"),
                home: path.into(),
                codex_home: path.into(),
                cwd: path.into(),
                model: "test-model".into(),
                effort: "low".into(),
                shared_auth: false,
                web_search: true,
            },

            credential_ref: "keychain:test".into(),
            takeover: Takeover::Never,
            host_files: false,
            shared_workspace: false,
            shared_workspace_pin: None,
            owner_direct: false,
            recovery_device_id: None,
            recovery_consent_id: None,
        }
    }
    fn setup(path: &std::path::Path, root: &std::path::Path) -> Ledger {
        let mut ledger = Ledger::open(path, OWNER).unwrap();
        ledger.register_binding(OWNER, &event().scope()).unwrap();
        ledger
            .set_model_profile(
                OWNER,
                &event().scope(),
                &hagency_agent_local::ModelProfile {
                    model: "test-model".into(),
                    workspace_root: root.to_string_lossy().into(),
                    reasoning_effort: String::new(),
                    web_search: true,
                    credential_ref: "keychain:test".into(),
                },
            )
            .unwrap();
        ledger
    }
    #[derive(Clone, Copy)]
    enum Mode {
        LostStart,
        ExistingStart,
        ReplyRetry,
        CancelModel,
        RevokedModel,
        IdleOtherRoom,
        CrossEpochReply,
        AcceptedOnly,
    }
    struct Fake {
        path: PathBuf,
        mode: Mode,
        calls: StdMutex<Vec<&'static str>>,
        stop: watch::Sender<bool>,
    }
    impl Transport for Fake {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let recovering = matches!(op, Op::ReconcileKnownReply { .. });
            let delivery_epoch = if recovering { 2 } else { 1 };
            let op = match op {
                Op::ReconcileKnownReply { lease, known } => {
                    assert_eq!(known.original_epoch, 1);
                    assert_eq!(known.binding_generation, 1);
                    assert_eq!(known.binding_id, "binding-a");
                    assert_eq!(known.room_id, "!room-a:test");
                    assert_eq!(known.requester_mxid, "@bob:test");
                    assert_eq!(known.thread_root, "$root:test");
                    Op::Reply {
                        lease,
                        dispatch_id: known.dispatch_id,
                        execution_id: known.execution_id,
                        body: known.body,
                    }
                }
                op => op,
            };
            let mut calls = self.calls.lock().unwrap();
            let ledger = Ledger::open(&self.path, OWNER).unwrap();
            match op {
                Op::Poll { binding_id, .. } => {
                    assert_eq!(
                        binding_id,
                        if matches!(self.mode, Mode::IdleOtherRoom) {
                            "binding-b"
                        } else {
                            "binding-a"
                        }
                    );
                    calls.push("poll");
                    if matches!(self.mode, Mode::IdleOtherRoom) {
                        return Ok(Response::Events(vec![]));
                    }
                    if matches!(self.mode, Mode::ReplyRetry | Mode::CrossEpochReply) {
                        if ledger.inbox_record(OWNER, "dispatch-a").unwrap().state == State::Replied
                        {
                            self.stop.send_replace(true);
                        }
                        return Ok(Response::Events(vec![]));
                    }
                    if calls.iter().filter(|c| **c == "poll").count() > 1 {
                        return Ok(Response::Events(vec![]));
                    }
                    Ok(Response::Events(vec![wire(&event())]))
                }
                Op::Ack { dispatch_id, .. } => {
                    assert_eq!(
                        ledger.inbox_record(OWNER, &dispatch_id).unwrap().state,
                        State::Received
                    );
                    calls.push("ack");
                    Ok(Response::Acknowledged)
                }
                Op::Start {
                    dispatch_id,
                    execution_id,
                    ..
                } => {
                    let record = ledger.inbox_record(OWNER, &dispatch_id).unwrap();
                    assert_eq!(record.state, State::Prepared);
                    assert_eq!(record.execution_id.as_ref(), Some(&execution_id));
                    calls.push("start");
                    if matches!(self.mode, Mode::LostStart) {
                        return Err(RuntimeError::Transport);
                    }
                    let mut dispatch = event();
                    dispatch.execution_id = Some(execution_id);
                    dispatch.state = "running".into();
                    Ok(Response::Started(ExecutionStart {
                        dispatch: wire(&dispatch),
                        newly_started: matches!(self.mode, Mode::CancelModel | Mode::RevokedModel),
                    }))
                }
                Op::Processing {
                    lease,
                    dispatch_id,
                    execution_id,
                } => {
                    assert!(matches!(self.mode, Mode::CancelModel | Mode::RevokedModel));
                    assert_eq!(lease.agent_id, "agent-a");
                    assert_eq!(lease.epoch, 1);
                    assert_eq!(dispatch_id, "dispatch-a");
                    let record = ledger.inbox_record(OWNER, &dispatch_id).unwrap();
                    assert_eq!(record.state, State::Running);
                    assert_eq!(record.execution_id.as_deref(), Some(execution_id.as_str()));
                    assert_eq!(record.dispatch.scope(), event().scope());
                    assert_eq!(ledger.outstanding_calls(OWNER).unwrap().len(), 1);
                    calls.push("processing");
                    // A cosmetic notification failure must not mutate replies,
                    // release reservations, or weaken the cancellation fixture.
                    Err(RuntimeError::Transport)
                }
                Op::AuthorizeTool {
                    dispatch_id,
                    execution_id,
                    ..
                } => {
                    calls.push("authorize");
                    if matches!(self.mode, Mode::RevokedModel)
                        && calls.iter().filter(|c| **c == "authorize").count() > 1
                    {
                        return Err(RuntimeError::Authorization);
                    }
                    let mut active = event();
                    assert_eq!(dispatch_id, active.id);
                    active.state = "running".into();
                    active.execution_id = Some(execution_id);
                    Ok(Response::ToolAuthorized(
                        serde_json::from_value(serde_json::to_value(active).unwrap()).unwrap(),
                    ))
                }
                Op::Finish {
                    dispatch_id,
                    outcome,
                    ..
                } => {
                    assert!(matches!(outcome, Outcome::Unknown));
                    assert_eq!(
                        ledger.inbox_record(OWNER, &dispatch_id).unwrap().state,
                        State::Unknown
                    );
                    calls.push("unknown");
                    if !matches!(self.mode, Mode::RevokedModel) {
                        self.stop.send_replace(true);
                    }
                    Ok(Response::Finished)
                }
                Op::Reply {
                    dispatch_id,
                    execution_id,
                    body,
                    ..
                } => {
                    assert_eq!(body, "original response");
                    let record = ledger.inbox_record(OWNER, &dispatch_id).unwrap();
                    assert_eq!(record.state, State::ReplyReady);
                    assert_eq!(record.reply.as_deref(), Some("original response"));
                    let label = if recovering { "reconcile" } else { "reply" };
                    calls.push(label);
                    if !matches!(self.mode, Mode::AcceptedOnly)
                        && calls.iter().filter(|c| **c == label).count() == 1
                    {
                        return Err(RuntimeError::Transport);
                    }
                    Ok(Response::ReplyQueued(ReplyReceipt {
                        id: "reply-a".into(),
                        owner_event_id: dispatch_id.clone(),
                        agent_id: "agent-a".into(),
                        binding_id: "binding-a".into(),
                        owner_user_id: "user-a".into(),
                        room_id: "!room-a:test".into(),
                        puppet_mxid: "@agent:test".into(),
                        binding_generation: 1,
                        dispatch_epoch: 1,
                        delivery_epoch,
                        requester_mxid: "@bob:test".into(),
                        thread_root: "$root:test".into(),
                        body: body.clone(),
                        payload_digest: {
                            use sha2::{Digest, Sha256};
                            format!("{:x}",Sha256::digest(format!("{{\"dispatchId\":\"{dispatch_id}\",\"executionId\":\"{execution_id}\",\"body\":\"{body}\"}}").as_bytes()))
                        },
                        matrix_txn_id: {
                            use sha2::{Digest, Sha256};
                            format!("hagency_{:x}", Sha256::digest(dispatch_id.as_bytes()))
                        },
                        state: if matches!(self.mode, Mode::AcceptedOnly) {
                            "pending"
                        } else {
                            "sent"
                        }
                        .into(),
                        matrix_event_id: if matches!(self.mode, Mode::AcceptedOnly) {
                            None
                        } else {
                            Some("$sent:test".into())
                        },
                    }))
                }
                _ => panic!("unexpected transport call"),
            }
        }
    }
    async fn fixture(mode: Mode) -> (Vec<&'static str>, State) {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("ledger.db");
        let mut ledger = setup(&path, tmp.path());
        if matches!(mode, Mode::ReplyRetry | Mode::CrossEpochReply) {
            ledger
                .receive_dispatch(OWNER, &event(), inbox::Limits::default(), now())
                .unwrap();
            ledger.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
            let prepared = ledger.prepare_execution(OWNER, "dispatch-a").unwrap();
            let mut event = prepared.dispatch.clone();
            event.state = "running".into();
            event.execution_id = Some(prepared.execution_id.clone());
            ledger
                .confirm_execution_start(
                    OWNER,
                    &inbox::ServerStart {
                        dispatch: event,
                        newly_started: true,
                    },
                )
                .unwrap()
                .unwrap();
            ledger
                .persist_execution_reply(
                    OWNER,
                    "dispatch-a",
                    &prepared.execution_id,
                    "original response",
                )
                .unwrap();
            drop(ledger);
            ledger = Ledger::open(&path, OWNER).unwrap();
            ledger.recover_agent_executions(OWNER, "agent-a").unwrap();
        }
        let (stop, mut receiver) = watch::channel(false);
        let fake = Fake {
            path,
            mode,
            calls: StdMutex::new(vec![]),
            stop,
        };
        let (status, _rx) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(5),
                execution_loop(
                    &fake,
                    OWNER,
                    &mut ledger,
                    &config(tmp.path()),
                    &LeaseRef {
                        agent_id: "agent-a".into(),
                        epoch: if matches!(mode, Mode::CrossEpochReply) {
                            2
                        } else {
                            1
                        }
                    },
                    &mut receiver,
                    &status
                )
            )
            .await
            .unwrap(),
            Err(RuntimeError::Stopped)
        ));
        let calls = fake.calls.lock().unwrap().clone();
        (
            calls,
            ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
        )
    }
    #[tokio::test]
    async fn unapproved_request_never_opens_native_runtime_or_reserves_budget() {
        use hagency_agent_local::{Budget, Layer, Limit, Period, Policy, RequestPolicy, ToolPolicy};
        for requests in [RequestPolicy::Deny, RequestPolicy::AskOwner] {
            let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let root = temp.path().canonicalize().unwrap();
            let path = root.join("ledger.db");
            let mut ledger = setup(&path, &root);
            ledger.set_policy(OWNER, &event().scope(), Layer::Room, 0, &Policy {
                budget: Budget { limit: Limit::Unlimited, period: Period::Lifetime },
                requests, high_risk: ToolPolicy::Deny,
            }).unwrap();
            ledger.receive_dispatch(OWNER, &event(), inbox::Limits::default(), now()).unwrap();
            ledger.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
            let prepared = ledger.prepare_execution(OWNER, "dispatch-a").unwrap();
            let mut event = prepared.dispatch.clone();
            event.state = "running".into();
            event.execution_id = Some(prepared.execution_id.clone());
            let permit = ledger.confirm_execution_start(OWNER, &inbox::ServerStart { dispatch: event, newly_started: true }).unwrap().unwrap();
            let (stop, mut cancel) = watch::channel(false);
            let fake = Fake { path, mode: Mode::AcceptedOnly, calls: StdMutex::new(vec![]), stop };
            let (status, _rx) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
            let mut sessions = provider_custody::Sessions::default();
            // This executable does not exist. A launch attempt would fail;
            // denial must instead persist a reply without touching a provider.
            assert!(run_turn(&fake, &lease(), &mut ledger, OWNER, &config(&root), permit.into_prepared(), None, &mut cancel, &status, &mut sessions).await.unwrap().is_none());
            let saved = ledger.inbox_record(OWNER, "dispatch-a").unwrap();
            assert_eq!(saved.state, State::ReplyReady);
            assert!(saved.reply.unwrap().contains("未获 Agent 主人"));
            assert!(ledger.outstanding_calls(OWNER).unwrap().is_empty());
            assert!(fake.calls.lock().unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn last_worker_completion_consumes_already_accepted_new_binding_start() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let first = config(tmp.path());
        let mut next = first.clone();
        next.binding_id = "binding-b".into();
        let (initial, _, _) = binding_work(first);
        let (work, second, _) = binding_work(next);
        let (commands, rx) = mpsc::unbounded_channel();
        assert!(commands.send(work).is_ok());
        let completed = tokio::spawn(async {});
        while !completed.is_finished() {
            tokio::task::yield_now().await;
        }
        let mut completed = Some(completed);
        let (stop, mut cancel) = watch::channel(false);
        let (began, mut started) = mpsc::unbounded_channel();
        let supervisor = tokio::spawn(async move {
            supervise_bindings(initial, rx, &mut cancel, |mut work| {
                if let Some(done) = completed.take() {
                    return done;
                }
                assert_eq!(work.config.binding_id, "binding-b");
                let _ = began.send(());
                tokio::spawn(async move {
                    canceled(&mut work.receiver).await;
                    let mut status = RuntimeStatus::new("agent-a", "stopped", None);
                    status.binding_id = Some(work.config.binding_id);
                    work.status.send_replace(status);
                })
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(2), started.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!supervisor.is_finished());
        stop.send_replace(true);
        assert!(matches!(
            supervisor.await.unwrap(),
            Err(RuntimeError::Stopped)
        ));
        assert_eq!(second.status.borrow().phase, "stopped");
        assert!(commands.is_closed());
    }
    #[tokio::test]
    async fn closing_supervisor_seals_queued_binding_start_instead_of_leaving_starting() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let first = config(tmp.path());
        let mut next = first.clone();
        next.binding_id = "binding-b".into();
        let (initial, _, _) = binding_work(first);
        let (work, second, _) = binding_work(next);
        let (commands, rx) = mpsc::unbounded_channel();
        assert!(commands.send(work).is_ok());
        let (_stop, mut cancel) = watch::channel(true);
        assert!(matches!(
            supervise_bindings(initial, rx, &mut cancel, |mut work| tokio::spawn(
                async move {
                    canceled(&mut work.receiver).await;
                }
            ))
            .await,
            Err(RuntimeError::Stopped)
        ));
        assert_eq!(second.status.borrow().phase, "stopped");
        assert!(commands.is_closed());
        assert!(commands.send(binding_work(config(tmp.path())).0).is_err());
    }
    struct BacklogFailureTransport {
        stop: watch::Sender<bool>,
        calls: StdMutex<Vec<&'static str>>,
    }
    impl Transport for BacklogFailureTransport {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let mut calls = self.calls.lock().unwrap();
            match op {
                Op::Reply { dispatch_id, .. } => {
                    assert_eq!(dispatch_id, "dispatch-a");
                    calls.push("old-reply");
                    Err(RuntimeError::Authorization)
                }
                Op::Finish { .. } => {
                    calls.push("finish");
                    Err(RuntimeError::Transport)
                }
                Op::Poll { .. } => {
                    calls.push("poll");
                    if calls.contains(&"new-start") {
                        self.stop.send_replace(true);
                        return Ok(Response::Events(vec![]));
                    }
                    let mut next = event();
                    next.id = "dispatch-next".into();
                    next.event_id = "$next:test".into();
                    Ok(Response::Events(vec![wire(&next)]))
                }
                Op::Ack { .. } => {
                    calls.push("ack");
                    Ok(Response::Acknowledged)
                }
                Op::Start { dispatch_id, .. } => {
                    assert_eq!(dispatch_id, "dispatch-next");
                    calls.push("new-start");
                    Err(RuntimeError::Transport)
                }
                _ => panic!("historical delivery recovery must never invoke a provider"),
            }
        }
    }
    #[tokio::test]
    async fn historical_reply_denial_and_finish_transport_failure_do_not_block_new_dispatch() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut ledger = setup(&tmp.path().join("ledger"), tmp.path());
        let mut old = event();
        ledger
            .receive_dispatch(OWNER, &old, inbox::Limits::default(), now())
            .unwrap();
        ledger.acknowledge_dispatch(OWNER, &old.id).unwrap();
        let prepared = ledger.prepare_execution(OWNER, &old.id).unwrap();
        old.execution_id = Some(prepared.execution_id.clone());
        old.state = "running".into();
        ledger
            .confirm_execution_start(
                OWNER,
                &inbox::ServerStart {
                    dispatch: old,
                    newly_started: true,
                },
            )
            .unwrap();
        ledger
            .persist_execution_reply(
                OWNER,
                "dispatch-a",
                &prepared.execution_id,
                "original response",
            )
            .unwrap();
        let (stop, mut cancel) = watch::channel(false);
        let transport = BacklogFailureTransport {
            stop,
            calls: StdMutex::new(vec![]),
        };
        let (status, _) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                execution_loop(
                    &transport,
                    OWNER,
                    &mut ledger,
                    &config(tmp.path()),
                    &lease(),
                    &mut cancel,
                    &status
                )
            )
            .await
            .unwrap(),
            Err(RuntimeError::Stopped)
        ));
        assert_eq!(
            ledger
                .inbox_record(OWNER, "dispatch-a")
                .unwrap()
                .reply
                .as_deref(),
            Some("original response")
        );
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-next").unwrap().state,
            State::Unknown
        );
        assert_eq!(
            *transport.calls.lock().unwrap(),
            vec![
                "old-reply",
                "poll",
                "ack",
                "new-start",
                "finish",
                "old-reply",
                "finish",
                "poll"
            ]
        );
    }
    #[derive(Clone)]
    struct SharedLeaseTransport {
        polls: Arc<StdMutex<Vec<(String, i64)>>>,
        fenced: Arc<StdMutex<BTreeSet<String>>>,
    }
    impl Transport for SharedLeaseTransport {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let Op::Poll {
                lease, binding_id, ..
            } = op
            else {
                panic!(
                    "Room workers cannot acquire/renew/release shared lease or start unoffered work"
                )
            };
            assert_eq!(lease.agent_id, "agent-a");
            assert_eq!(lease.epoch, 77);
            self.polls
                .lock()
                .unwrap()
                .push((binding_id.clone(), lease.epoch));
            if self.fenced.lock().unwrap().contains(&binding_id) {
                Err(RuntimeError::Authorization)
            } else {
                Ok(Response::Events(vec![]))
            }
        }
    }
    async fn wait_polls(transport: &SharedLeaseTransport, binding: &str, minimum: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if transport
                    .polls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(id, _)| id == binding)
                    .count()
                    >= minimum
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn multi_binding_fixture(fence: bool) {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let path = root.join("ledger");
        drop(setup(&path, &root));
        let transport = SharedLeaseTransport {
            polls: Arc::new(StdMutex::new(vec![])),
            fenced: Arc::new(StdMutex::new(BTreeSet::new())),
        };
        let a = config(&root);
        let mut b = a.clone();
        b.binding_id = "binding-b".into();
        b.host_files = true;
        b.profile.effort = "high".into();
        let (initial, first, _) = binding_work(a);
        let (next, second, _) = binding_work(b);
        let (commands, rx) = mpsc::unbounded_channel();
        assert!(commands.send(next).is_ok());
        let (stop, mut cancel) = watch::channel(false);
        let wire = transport.clone();
        let seen = Arc::new(StdMutex::new(vec![]));
        let captured = seen.clone();
        let supervisor = tokio::spawn(async move {
            supervise_bindings(initial, rx, &mut cancel, |mut work| {
                captured.lock().unwrap().push((
                    work.config.binding_id.clone(),
                    work.config.host_files,
                    work.config.profile.effort.clone(),
                ));
                let wire = wire.clone();
                let path = path.clone();
                tokio::spawn(async move {
                    let mut ledger = Ledger::open(&path, OWNER).unwrap();
                    let result = execution_loop(
                        &wire,
                        OWNER,
                        &mut ledger,
                        &work.config,
                        &LeaseRef {
                            agent_id: "agent-a".into(),
                            epoch: 77,
                        },
                        &mut work.receiver,
                        &work.status,
                    )
                    .await;
                    let mut status =
                        RuntimeStatus::new(&work.config.agent_id, "stopped", failure_code(&result));
                    status.binding_id = Some(work.config.binding_id);
                    work.status.send_replace(status);
                })
            })
            .await
        });
        wait_polls(&transport, "binding-a", 1).await;
        wait_polls(&transport, "binding-b", 1).await;
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                ("binding-a".into(), false, "low".into()),
                ("binding-b".into(), true, "high".into())
            ]
        );
        let before_b = transport
            .polls
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| id == "binding-b")
            .count();
        if fence {
            transport.fenced.lock().unwrap().insert("binding-a".into());
        } else {
            first.cancel.send_replace(true);
        }
        let mut astatus = first.status.clone();
        tokio::time::timeout(Duration::from_secs(5), async {
            while astatus.borrow().phase != "stopped" {
                astatus.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(
            astatus.borrow().last_error,
            if fence {
                Some("authorization_or_lease_lost")
            } else {
                None
            }
        );
        let a_polls = transport
            .polls
            .lock()
            .unwrap()
            .iter()
            .filter(|(id, _)| id == "binding-a")
            .count();
        wait_polls(&transport, "binding-b", before_b + 2).await;
        assert_eq!(
            transport
                .polls
                .lock()
                .unwrap()
                .iter()
                .filter(|(id, _)| id == "binding-a")
                .count(),
            a_polls
        );
        assert!(!supervisor.is_finished());
        assert!(!*second.cancel.borrow());
        assert!(!*stop.borrow());
        // Shared owner/device cancellation joins all remaining workers before release.
        stop.send_replace(true);
        assert!(matches!(
            supervisor.await.unwrap(),
            Err(RuntimeError::Stopped)
        ));
        assert_eq!(second.status.borrow().phase, "stopped");
        assert!(
            transport
                .polls
                .lock()
                .unwrap()
                .iter()
                .all(|(_, epoch)| *epoch == 77)
        );
    }
    #[tokio::test]
    async fn simultaneous_rooms_share_one_epoch_without_worker_acquire_and_stop_independently() {
        multi_binding_fixture(false).await;
    }
    #[tokio::test]
    async fn room_permission_fence_stops_only_that_binding_and_owner_stop_joins_rest() {
        multi_binding_fixture(true).await;
    }
    #[tokio::test]
    async fn selected_binding_never_runs_or_recovers_other_room_inbox_records() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("ledger.db");
        let mut ledger = setup(&path, tmp.path());
        for (id, target) in [
            ("other-acked", State::Acknowledged),
            ("other-reply", State::ReplyReady),
            ("other-unknown", State::Unknown),
        ] {
            let mut other = event();
            other.id = id.into();
            other.event_id = format!("${id}:test");
            other.binding_id = "binding-b".into();
            other.room_id = "!room-b:test".into();
            ledger.register_binding(OWNER, &other.scope()).unwrap();
            ledger
                .receive_dispatch(OWNER, &other, inbox::Limits::default(), 1)
                .unwrap();
            ledger.acknowledge_dispatch(OWNER, id).unwrap();
            if target != State::Acknowledged {
                let prepared = ledger.prepare_execution(OWNER, id).unwrap();
                other.execution_id = Some(prepared.execution_id.clone());
                other.state = "running".into();
                ledger
                    .confirm_execution_start(
                        OWNER,
                        &inbox::ServerStart {
                            dispatch: other,
                            newly_started: true,
                        },
                    )
                    .unwrap()
                    .unwrap();
                if target == State::ReplyReady {
                    ledger
                        .persist_execution_reply(
                            OWNER,
                            id,
                            &prepared.execution_id,
                            "other original response",
                        )
                        .unwrap();
                } else {
                    ledger
                        .finish_local_execution(OWNER, id, &prepared.execution_id, Finish::Unknown)
                        .unwrap();
                }
            }
        }
        let (stop, mut receiver) = watch::channel(false);
        let fake = Fake {
            path,
            mode: Mode::LostStart,
            calls: StdMutex::new(vec![]),
            stop,
        };
        let (status, _) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        assert!(matches!(
            execution_loop(
                &fake,
                OWNER,
                &mut ledger,
                &config(tmp.path()),
                &lease(),
                &mut receiver,
                &status
            )
            .await,
            Err(RuntimeError::Stopped)
        ));
        assert_eq!(
            *fake.calls.lock().unwrap(),
            vec!["poll", "ack", "start", "unknown"]
        );
        assert_eq!(
            ledger.inbox_record(OWNER, "other-acked").unwrap().state,
            State::Acknowledged
        );
        assert_eq!(
            ledger.inbox_record(OWNER, "other-reply").unwrap().state,
            State::ReplyReady
        );
        assert_eq!(
            ledger.inbox_record(OWNER, "other-unknown").unwrap().state,
            State::Unknown
        );
        assert_eq!(
            ledger
                .inbox_record(OWNER, "other-reply")
                .unwrap()
                .reply
                .as_deref(),
            Some("other original response")
        );
    }
    struct WrongBinding;
    impl Transport for WrongBinding {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let Op::Poll { binding_id, .. } = op else {
                panic!("foreign binding must never be acknowledged or executed")
            };
            assert_eq!(binding_id, "binding-a");
            let mut other = event();
            other.binding_id = "binding-b".into();
            other.room_id = "!room-b:test".into();
            Ok(Response::Events(vec![wire(&other)]))
        }
    }
    #[tokio::test]
    async fn foreign_binding_poll_response_is_rejected_before_inbox_ack_or_model() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut ledger = setup(&tmp.path().join("ledger"), tmp.path());
        let (_stop, mut receiver) = watch::channel(false);
        let (status, _) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        assert!(matches!(
            execution_loop(
                &WrongBinding,
                OWNER,
                &mut ledger,
                &config(tmp.path()),
                &lease(),
                &mut receiver,
                &status
            )
            .await,
            Err(RuntimeError::Transport)
        ));
        assert!(ledger.inbox_record(OWNER, "dispatch-a").is_err());
    }
    #[tokio::test]
    async fn lost_start_response_seals_unknown_without_spawning_provider() {
        let (calls, state) = fixture(Mode::LostStart).await;
        assert_eq!(calls, vec!["poll", "ack", "start", "unknown"]);
        assert_eq!(state, State::Unknown);
    }
    #[tokio::test]
    async fn server_existing_start_never_replays_model() {
        let (calls, state) = fixture(Mode::ExistingStart).await;
        assert_eq!(calls, vec!["poll", "ack", "start", "unknown"]);
        assert_eq!(state, State::Unknown);
    }
    #[tokio::test]
    async fn original_reply_retries_after_restart_and_lost_transport_response() {
        let (calls, state) = fixture(Mode::ReplyRetry).await;
        assert_eq!(calls, vec!["reply", "poll", "reply", "poll"]);
        assert_eq!(state, State::Replied);
    }
    #[cfg(unix)]
    fn install_long_turn_provider(script: &std::path::Path, marker: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;
        let source=r##"#!/usr/bin/python3
import sys,json,time,os
marker=MARKER
for raw in sys.stdin:
    frame=json.loads(raw); method=frame.get('method'); ident=frame.get('id')
    def reply(result):
        print(json.dumps({'id':ident,'result':result}),flush=True)
    if method=='initialize': reply({'userAgent':'fixture/0.167.0'})
    elif method=='initialized': pass
    elif method=='thread/name/set': reply({})
    elif method=='config/read':
        config={'project_doc_max_bytes':0,'skills':{'include_instructions':False,'bundled':{'enabled':False}},'include_apps_instructions':False,'include_environment_context':False,'developer_instructions':'','web_search':'live','mcp_servers':{},'features':{}}
        for name in ['shell_tool','unified_exec','code_mode_host','code_mode','hooks','plugins','multi_agent','multi_agent_v2','skill_search','skill_mcp_dependency_install','shell_snapshot','view_image','browser_use','browser_use_external','browser_use_full_cdp_access','browser_annotation_api','computer_use','in_app_browser','goals','image_generation','apps','tool_search','tool_suggest','web_search','web_search_cached','web_search_request','standalone_web_search','memory_tool']: config['features'][name]=False
        config['features']['standalone_web_search']=True
        config['features']['code_mode_host']=True
        reply({'config':config})
    elif method=='account/read': reply({'requiresOpenaiAuth':True,'account':{'type':'chatgpt'}})
    elif method=='thread/start':
        p=frame['params'];reply({'thread':{'id':'thread-a','cwd':p['cwd']},'cwd':p['cwd'],'model':p['model'],'approvalPolicy':'untrusted','approvalsReviewer':'user','sandbox':{'type':{'read-only':'readOnly','workspace-write':'workspaceWrite'}[p['sandbox']],'networkAccess':False}})
    elif method=='turn/start':
        open(marker,'w').write(str(os.getpid()))
        reply({'turn':{'id':'turn-a','status':'inProgress'}})
        while True: time.sleep(10)
    else: raise Exception(method)
"##.replace("MARKER",&serde_json::to_string(marker.to_str().unwrap()).unwrap());
        std::fs::write(script, source).unwrap();
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    impl Transport for Arc<Fake> {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            self.as_ref().execute(op).await
        }
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn pure_model_room_revoke_kills_provider_preserves_unknown_usage_and_keeps_other_room_running()
     {
        use hagency_agent_local::{Layer, Period};
        use std::os::unix::fs::PermissionsExt;
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for name in ["home", "codex", "workspace"] {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let script = root.join("provider");
        let marker = root.join("model-pid");
        install_long_turn_provider(&script, &marker);
        let path = root.join("ledger");
        let mut configured = setup(&path, &root.join("workspace"));
        configured
            .set_policy(
                OWNER,
                &event().scope(),
                Layer::Room,
                0,
                &hagency_agent_local::Policy {
                    budget: hagency_agent_local::Budget {
                        limit: hagency_agent_local::Limit::Tokens(100),
                        period: Period::Lifetime,
                    },
                    requests: hagency_agent_local::RequestPolicy::Allow,
                    high_risk: hagency_agent_local::ToolPolicy::Deny,
                },
            )
            .unwrap();
        drop(configured);
        let mut a = config(&root.join("workspace"));
        a.profile.executable = script;
        a.profile.home = root.join("home");
        a.profile.codex_home = root.join("codex");
        let mut b = a.clone();
        b.binding_id = "binding-b".into();
        let (initial, first, _) = binding_work(a);
        let (work, second, _) = binding_work(b);
        let (commands, rx) = mpsc::unbounded_channel();
        assert!(commands.send(work).is_ok());
        let (global_stop, mut receiver) = watch::channel(false);
        let fa = Arc::new(Fake {
            path: path.clone(),
            mode: Mode::RevokedModel,
            calls: StdMutex::new(vec![]),
            stop: first.cancel.clone(),
        });
        let fb = Arc::new(Fake {
            path: path.clone(),
            mode: Mode::IdleOtherRoom,
            calls: StdMutex::new(vec![]),
            stop: second.cancel.clone(),
        });
        let wire_a = fa.clone();
        let wire_b = fb.clone();
        let worker_path = path.clone();
        let supervisor = tokio::spawn(async move {
            supervise_bindings(initial, rx, &mut receiver, |mut work| {
                let wire = if work.config.binding_id == "binding-a" {
                    wire_a.clone()
                } else {
                    wire_b.clone()
                };
                let path = worker_path.clone();
                tokio::spawn(async move {
                    let mut ledger = Ledger::open(path, OWNER).unwrap();
                    let result = execution_loop(
                        &wire,
                        OWNER,
                        &mut ledger,
                        &work.config,
                        &lease(),
                        &mut work.receiver,
                        &work.status,
                    )
                    .await;
                    let mut status =
                        RuntimeStatus::new("agent-a", "stopped", failure_code(&result));
                    status.binding_id = Some(work.config.binding_id);
                    work.status.send_replace(status);
                })
            })
            .await
        });
        let mut ast = first.status.clone();
        tokio::time::timeout(Duration::from_secs(15), async {
            while ast.borrow().phase != "stopped" {
                ast.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(ast.borrow().last_error, Some("authorization_or_lease_lost"));
        assert!(marker.exists());
        let pid = std::fs::read_to_string(&marker).unwrap();
        assert!(
            !std::process::Command::new("/bin/kill")
                .args(["-0", pid.trim()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .unwrap()
                .success(),
            "revoked model child must be terminated"
        );
        let ledger = Ledger::open(&path, OWNER).unwrap();
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
            State::Unknown
        );
        assert_eq!(
            ledger
                .account(&event().scope(), Layer::Room, Period::Lifetime, now())
                .unwrap(),
            (0, 0)
        );
        let outstanding = ledger.outstanding_calls(OWNER).unwrap();
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].state, "unknown");
        assert_eq!(outstanding[0].reserved, 0);
        let before = fb.calls.lock().unwrap().len();
        tokio::time::timeout(Duration::from_secs(3), async {
            while fb.calls.lock().unwrap().len() <= before {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(!supervisor.is_finished());
        assert!(!*second.cancel.borrow());
        assert!(!*global_stop.borrow());
        assert_eq!(
            fa.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|c| **c == "start")
                .count(),
            1
        );
        global_stop.send_replace(true);
        assert!(matches!(
            supervisor.await.unwrap(),
            Err(RuntimeError::Stopped)
        ));
        assert_eq!(second.status.borrow().phase, "stopped");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_provider_and_preserves_unknown_charge() {
        cancellation_provider_fixture(false).await;
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn supervised_long_silent_turn_keeps_running_until_owner_stop() {
        cancellation_provider_fixture(true).await;
    }
    #[cfg(unix)]
    async fn cancellation_provider_fixture(long: bool) {
        use hagency_agent_local::{Budget, Layer, Limit, Period, Policy, RequestPolicy, ToolPolicy};
        use std::os::unix::fs::PermissionsExt;
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        for name in ["home", "codex", "workspace"] {
            std::fs::create_dir(root.join(name)).unwrap();
            std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let script = root.join("codex-fixture");
        let marker = root.join("turn-started");
        install_long_turn_provider(&script, &marker);
        let path = root.join("ledger.db");
        let workspace = root.join("workspace");
        let mut ledger = setup(&path, &workspace);
        ledger
            .set_policy(
                OWNER,
                &event().scope(),
                Layer::Room,
                0,
                &Policy {
                    budget: Budget {
                        limit: Limit::Tokens(100),
                        period: Period::Lifetime,
                    },
                    requests: RequestPolicy::Allow,
                    high_risk: ToolPolicy::Deny,
                },
            )
            .unwrap();
        let mut config = config(&workspace);
        config.profile.executable = script;
        config.profile.home = root.join("home");
        config.profile.codex_home = root.join("codex");
        let (stop, mut receiver) = watch::channel(false);
        let fake = Fake {
            path,
            mode: Mode::CancelModel,
            calls: StdMutex::new(vec![]),
            stop: stop.clone(),
        };
        let (status, _) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        let revoke = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !marker.exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            if long {
                // Wait for the original turn acknowledgement, not merely the
                // child writing its marker. Then advance only Tokio's clock:
                // this is a real supervised child but no paid model request.
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !fake.calls.lock().unwrap().contains(&"processing") {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }).await.unwrap();
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(1801)).await;
                for _ in 0..4 { tokio::task::yield_now().await; }
                assert!(!*stop.borrow(), "elapsed time must not finish or retry this turn");
                let check = Ledger::open(&fake.path, OWNER).unwrap();
                assert_eq!(check.inbox_record(OWNER, "dispatch-a").unwrap().state, State::Running);
                assert_eq!(check.outstanding_calls(OWNER).unwrap().len(), 1);
                assert!(fake.calls.lock().unwrap().iter().filter(|&&c| c == "authorize").count() >= 2,
                    "the long turn still rechecks its dispatch authorization");
                tokio::time::resume();
            }
            stop.send_replace(true);
        };
        let reference = lease();
        let (result, ()) = tokio::join!(
            execution_loop(
                &fake,
                OWNER,
                &mut ledger,
                &config,
                &reference,
                &mut receiver,
                &status
            ),
            revoke
        );
        assert!(matches!(result, Err(RuntimeError::Stopped)));
        let calls = fake.calls.lock().unwrap().clone();
        assert_eq!(calls.iter().filter(|&&c| c == "start").count(), 1);
        if !long {
            assert_eq!(calls.into_iter().filter(|&c| c != "processing").collect::<Vec<_>>(),
                vec!["poll", "ack", "start", "authorize"]);
        }
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
            State::Unknown
        );
        assert_eq!(
            ledger
                .account(&event().scope(), Layer::Room, Period::Lifetime, now())
                .unwrap(),
            (0, 0)
        );
        let outstanding = ledger.outstanding_calls(OWNER).unwrap();
        assert_eq!(outstanding.len(), 1);
        assert_eq!(outstanding[0].state, "unknown");
        assert_eq!(outstanding[0].reserved, 0);
    }
    #[tokio::test]
    async fn known_original_reply_reconciles_after_new_epoch_without_model_replay() {
        let (calls, state) = fixture(Mode::CrossEpochReply).await;
        assert_eq!(calls, vec!["reconcile", "poll", "reconcile", "poll"]);
        assert_eq!(state, State::Replied);
    }
    #[tokio::test]
    async fn server_accepted_unsent_reply_survives_restart_and_new_lease_reconciliation() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = tmp.path().join("ledger.db");
        let mut ledger = setup(&path, tmp.path());
        ledger
            .receive_dispatch(OWNER, &event(), inbox::Limits::default(), now())
            .unwrap();
        ledger.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
        let prepared = ledger.prepare_execution(OWNER, "dispatch-a").unwrap();
        let mut envelope = prepared.dispatch.clone();
        envelope.state = "running".into();
        envelope.execution_id = Some(prepared.execution_id.clone());
        ledger
            .confirm_execution_start(
                OWNER,
                &inbox::ServerStart {
                    dispatch: envelope,
                    newly_started: true,
                },
            )
            .unwrap()
            .unwrap();
        ledger
            .persist_execution_reply(
                OWNER,
                "dispatch-a",
                &prepared.execution_id,
                "original response",
            )
            .unwrap();
        let (stop, mut receiver) = watch::channel(false);
        let accepted = Fake {
            path: path.clone(),
            mode: Mode::AcceptedOnly,
            calls: StdMutex::new(vec![]),
            stop: stop.clone(),
        };
        let record = ledger.inbox_record(OWNER, "dispatch-a").unwrap();
        send_reply(
            &accepted,
            OWNER,
            &mut ledger,
            &lease(),
            &mut receiver,
            &record,
        )
        .await
        .unwrap();
        assert_eq!(*accepted.calls.lock().unwrap(), vec!["reply"]);
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
            State::ReplyReady
        );
        drop(ledger);
        let mut ledger = Ledger::open(&path, OWNER).unwrap();
        assert_eq!(
            ledger.recover_agent_executions(OWNER, "agent-a").unwrap(),
            0
        );
        // The server may have cancelled the old epoch's accepted outbox. The
        // only subsequent action is reconciliation of that exact known body.
        let newlease = LeaseRef {
            agent_id: "agent-a".into(),
            epoch: 2,
        };
        let restored = Fake {
            path,
            mode: Mode::CrossEpochReply,
            calls: StdMutex::new(vec![]),
            stop,
        };
        let record = ledger.inbox_record(OWNER, "dispatch-a").unwrap();
        assert!(matches!(
            send_reply(
                &restored,
                OWNER,
                &mut ledger,
                &newlease,
                &mut receiver,
                &record,
            )
            .await,
            Err(RuntimeError::Transport)
        ));
        send_reply(
            &restored,
            OWNER,
            &mut ledger,
            &newlease,
            &mut receiver,
            &record,
        )
        .await
        .unwrap();
        assert_eq!(
            *restored.calls.lock().unwrap(),
            vec!["reconcile", "reconcile"]
        );
        assert_eq!(
            ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
            State::Replied
        );
        assert_eq!(
            ledger
                .inbox_record(OWNER, "dispatch-a")
                .unwrap()
                .reply
                .as_deref(),
            None
        );
    }
    #[tokio::test]
    async fn approval_registry_fences_owner_device_expiry_digest_and_single_consumption() {
        let pin = DevicePin {
            origin: "https://server.test".into(),
            issuer: "https://server.test/auth/".into(),
            subject: "subject-a".into(),
            owner: OWNER.into(),
            user: "subject-a".into(),
            id: "device-a".into(),
            generation: 4,
        };
        let proposal = hagency_agent_local::ToolProposal {
            scope: event().scope(),
            dispatch: "dispatch-a".into(),
            tool: "model.request".into(),
            arguments: serde_json::json!({"executionId":"execution-a"}),
            canonical_directory: "/tmp/room".into(),
            risk: "model_request".into(),
            policy_revision: [1, 1, 1],
            expires: 100,
        };
        let (pending, decision) = codex::pending_approval(proposal.clone());
        let (cancel, _receiver) = watch::channel(false);
        let (_status, status) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
        let mut entry = Entry {
            commands: mpsc::unbounded_channel().0,
            bindings: BTreeMap::from([(
                "binding-a".into(),
                BindingHandle {
                    cancel: watch::channel(false).0,
                    status: status.clone(),
                },
            )]),
            cancel,
            status,
            task: TaskCustody::default().track(tokio::spawn(std::future::pending())),
            device: pin.clone(),
            approvals: BTreeMap::from([(
                "nonce".into(),
                ApprovalSlot {
                    view: ApprovalView {
                        proposal_id: "nonce".into(),
                        args_digest: "exact-proposal-digest".into(),
                        proposal,
                    },
                    pending,
                    device: pin.clone(),
                },
            )]),
        };
        let bindings = vec![Binding {
            id: "binding-a".into(),
            agent_id: "agent-a".into(),
            room_id: "!room-a:test".into(),
            state: "active".into(),
            scope_kind: "project".into(),
        }];
        for changed in 0..7 {
            let mut wrong = pin.clone();
            match changed {
                0 => wrong.owner = "@other:test".into(),
                1 => wrong.origin = "https://other.test".into(),
                2 => wrong.user = "subject-b".into(),
                3 => wrong.id = "device-b".into(),
                4 => wrong.generation += 1,
                5 => wrong.issuer = "https://other.test/auth/".into(),
                _ => wrong.subject = "different-oauth-subject".into(),
            }
            assert!(matches!(
                entry.take_approval(
                    &wrong,
                    "agent-a",
                    "nonce",
                    "exact-proposal-digest",
                    &bindings,
                    10
                ),
                Err(RuntimeError::Authorization)
            ));
        }
        assert!(matches!(
            entry.take_approval(&pin, "agent-a", "nonce", "changed-digest", &bindings, 10),
            Err(RuntimeError::Approval)
        ));
        assert!(matches!(
            entry.take_approval(
                &pin,
                "agent-a",
                "wrong-nonce",
                "exact-proposal-digest",
                &bindings,
                10
            ),
            Err(RuntimeError::Approval)
        ));
        assert!(matches!(
            entry.take_approval(
                &pin,
                "agent-a",
                "nonce",
                "exact-proposal-digest",
                &bindings,
                100
            ),
            Err(RuntimeError::Approval)
        ));
        assert!(matches!(
            entry.take_approval(&pin, "agent-a", "nonce", "exact-proposal-digest", &[], 10),
            Err(RuntimeError::Approval)
        ));
        entry.cancel.send_replace(true);
        assert!(matches!(
            entry.take_approval(
                &pin,
                "agent-a",
                "nonce",
                "exact-proposal-digest",
                &bindings,
                10
            ),
            Err(RuntimeError::Authorization)
        ));
        entry.cancel.send_replace(false);
        let entry = Arc::new(Mutex::new(entry));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let entry = entry.clone();
            let pin = pin.clone();
            tasks.push(tokio::spawn(async move {
                let bindings = vec![Binding {
                    id: "binding-a".into(),
                    agent_id: "agent-a".into(),
                    room_id: "!room-a:test".into(),
                    state: "active".into(),
                    scope_kind: "project".into(),
                }];
                let mut entry = entry.lock().await;
                if let Ok(pending) = entry.take_approval(
                    &pin,
                    "agent-a",
                    "nonce",
                    "exact-proposal-digest",
                    &bindings,
                    10,
                ) {
                    pending.decide(true);
                    true
                } else {
                    false
                }
            }));
        }
        let mut count = 0;
        for task in tasks {
            count += usize::from(task.await.unwrap());
        }
        assert_eq!(count, 1);
        assert!(decision.await.unwrap());
        entry.lock().await.task.abort();
    }
    struct RequestFacts {
        expected: inbox::Dispatch,
        changed: bool,
    }
    impl Transport for RequestFacts {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let Op::AuthorizeTool {
                dispatch_id,
                execution_id,
                ..
            } = op
            else {
                panic!("review must not start a model")
            };
            assert_eq!(dispatch_id, self.expected.id);
            let mut event = self.expected.clone();
            event.state = "running".into();
            event.execution_id = Some(execution_id);
            if self.changed {
                event.requester_mxid = "@different:test".into();
            }
            Ok(Response::ToolAuthorized(
                serde_json::from_value(serde_json::to_value(event).unwrap()).unwrap(),
            ))
        }
    }
    #[tokio::test]
    async fn model_request_review_binds_original_request_and_rechecks_remote_and_policy() {
        use hagency_agent_local::{Budget, Layer, Limit, Period, Policy, RequestPolicy, ToolPolicy};
        for scenario in 0..3 {
            let temp =
                hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let path = temp.path().join("ledger");
            let mut ledger = setup(&path, temp.path());
            let mut policy = Policy {
                budget: Budget {
                    limit: Limit::Tokens(100),
                    period: Period::Lifetime,
                },
                requests: RequestPolicy::AskOwner,
                high_risk: ToolPolicy::Deny,
            };
            ledger
                .set_policy(OWNER, &event().scope(), Layer::Room, 0, &policy)
                .unwrap();
            let prepared = inbox::Prepared {
                dispatch: event(),
                execution_id: "execution-a".into(),
            };
            let facts = RequestFacts {
                expected: prepared.dispatch.clone(),
                changed: scenario == 1,
            };
            let (queue, mut receiver) = codex::approval_queue(1).unwrap();
            let edit_path = path.clone();
            let decision = tokio::spawn(async move {
                let pending = receiver.recv().await.unwrap();
                assert_eq!(pending.proposal.tool, "model.request");
                assert_eq!(pending.proposal.risk, "model_request");
                assert_eq!(pending.proposal.arguments["executionId"], "execution-a");
                if scenario == 2 {
                    let mut edit = Ledger::open(edit_path, OWNER).unwrap();
                    policy.requests = RequestPolicy::Allow;
                    edit.set_policy(OWNER, &event().scope(), Layer::Room, 1, &policy)
                        .unwrap();
                }
                pending.decide(true);
            });
            let (_stop, mut cancel) = watch::channel(false);
            let allowed = review_request(
                &facts,
                &lease(),
                &mut ledger,
                OWNER,
                &prepared,
                &prepared.dispatch.body,
                temp.path(),
                &queue,
                &mut cancel,
            )
            .await
            .unwrap();
            decision.await.unwrap();
            assert_eq!(allowed, scenario == 0);
            if allowed {
                ledger
                    .reserve(&event().scope(), "execution-a", "dispatch-a", 10, now())
                    .unwrap();
            } else {
                assert!(ledger.outstanding_calls(OWNER).unwrap().is_empty());
            }
        }
    }
    struct ProcessingFake {
        calls: std::sync::atomic::AtomicUsize,
        refuse: bool,
    }
    impl Transport for ProcessingFake {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let Op::Processing {
                lease,
                dispatch_id,
                execution_id,
            } = op
            else {
                panic!("notification must not start models or mutate replies");
            };
            assert_eq!(lease.epoch, 1);
            assert_eq!(dispatch_id, "dispatch-a");
            assert_eq!(execution_id, "execution-a");
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(if self.refuse {
                RuntimeError::Authorization
            } else {
                RuntimeError::Transport
            })
        }
    }
    #[tokio::test]
    async fn processing_notice_is_bounded_and_never_retries_rejected_authority() {
        let prepared = inbox::Prepared {
            dispatch: event(),
            execution_id: "execution-a".into(),
        };
        let lease = LeaseRef {
            agent_id: "agent-a".into(),
            epoch: 1,
        };
        for (refuse, expected) in [(true, 1), (false, 2)] {
            let fake = ProcessingFake {
                calls: std::sync::atomic::AtomicUsize::new(0),
                refuse,
            };
            let (send, receive) = tokio::sync::oneshot::channel();
            send.send(()).unwrap();
            processing_notice(&fake, &lease, &prepared, receive).await;
            assert_eq!(
                fake.calls.load(std::sync::atomic::Ordering::SeqCst),
                expected
            );
        }
        let fake = ProcessingFake {
            calls: std::sync::atomic::AtomicUsize::new(0),
            refuse: false,
        };
        let (send, receive) = tokio::sync::oneshot::channel();
        drop(send);
        processing_notice(&fake, &lease, &prepared, receive).await;
        assert_eq!(fake.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
    struct HistoryFake {
        pages: StdMutex<std::collections::VecDeque<Result<Response, RuntimeError>>>,
    }
    impl Transport for HistoryFake {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            match op {
                Op::History { .. } => {}
                Op::Acquire {
                    history_snapshot, ..
                } => {
                    assert!(history_snapshot.count <= 2);
                    assert_eq!(history_snapshot.digest.len(), 64);
                }
                _ => panic!("unexpected history operation"),
            }
            self.pages
                .lock()
                .unwrap()
                .pop_front()
                .expect("bounded page requests")
        }
    }
    fn history_page(
        digest: &str,
        entries: Vec<inbox::HistoryExecution>,
        count: u64,
        next: Option<&str>,
    ) -> Result<Response, RuntimeError> {
        Ok(Response::History(
            super::super::device_execution::ExecutionHistory {
                agent_id: "agent-a".into(),
                snapshot: HistorySnapshot {
                    count,
                    digest: if count == entries.len() as u64 {
                        use sha2::{Digest, Sha256};
                        let mut h = Sha256::new();
                        h.update(b"hagency-started-executions-v1\n");
                        for entry in &entries {
                            h.update(
                                serde_json::to_vec(&(&entry.dispatch_id, &entry.execution_id))
                                    .unwrap(),
                            );
                            h.update(b"\n");
                        }
                        format!("{:x}", h.finalize())
                    } else {
                        digest.repeat(64)
                    },
                },
                executions: entries,
                next_cursor: next.map(str::to_owned),
            },
        ))
    }
    fn history_identity(p: &inbox::Prepared) -> inbox::HistoryExecution {
        let mut value = serde_json::to_value(&p.dispatch).unwrap();
        let immutable = serde_json::to_vec(&serde_json::json!([
            p.dispatch.id,
            p.dispatch.binding_id,
            p.dispatch.agent_id,
            p.dispatch.event_id,
            p.dispatch.room_id,
            p.dispatch.requester_mxid,
            p.dispatch.thread_root,
            p.dispatch.body,
            p.dispatch.binding_generation
        ]))
        .unwrap();
        use sha2::{Digest, Sha256};
        let o = value.as_object_mut().unwrap();
        let id = o.remove("id").unwrap();
        o.insert("dispatchId".into(), id);
        o.insert("executionId".into(), p.execution_id.clone().into());
        o.insert(
            "immutableDigest".into(),
            format!("{:x}", Sha256::digest(immutable)).into(),
        );
        for key in ["body", "state", "outcome"] {
            o.remove(key);
        }
        serde_json::from_value(value).unwrap()
    }
    #[tokio::test]
    async fn history_missing_receipt_does_not_block_with_retained_execution_identity() {
        let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut l = setup(&t.path().join("ledger.db"), t.path());
        let (_stop, mut cancel) = watch::channel(false);
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("a", vec![], 0, None)].into()),
        };
        assert!(
            history_coverage(&fake, &mut l, OWNER, "agent-a", &mut cancel)
                .await
                .unwrap()
                .1
        );
        l.receive_dispatch(OWNER, &event(), inbox::Limits::default(), now())
            .unwrap();
        l.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
        let p = l.prepare_execution(OWNER, "dispatch-a").unwrap();
        let history = history_identity(&p);
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("b", vec![history.clone()], 1, None)].into()),
        };
        assert!(
            history_coverage(&fake, &mut l, OWNER, "agent-a", &mut cancel)
                .await
                .unwrap()
                .1
        );
        let policy = hagency_agent_local::Policy {
            budget: hagency_agent_local::Budget {
                limit: hagency_agent_local::Limit::Tokens(100),
                period: hagency_agent_local::Period::Lifetime,
            },
            requests: hagency_agent_local::RequestPolicy::Allow,
            high_risk: hagency_agent_local::ToolPolicy::Deny,
        };
        for layer in [
            hagency_agent_local::Layer::Agent,
            hagency_agent_local::Layer::Room,
            hagency_agent_local::Layer::Requester,
        ] {
            l.set_policy(OWNER, &event().scope(), layer, 0, &policy)
                .unwrap();
        }
        l.reserve(&event().scope(), &p.execution_id, "dispatch-a", 50, now())
            .unwrap();
        l.settle(
            &event().scope(),
            &p.execution_id,
            &hagency_agent_local::Usage {
                input: 10,
                output: 2,
                cached_input: 0,
                cache_write_input: None,
                reasoning_output: 0,
                accounting_version: "fixture".into(),
            },
        )
        .unwrap();
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("b", vec![history], 1, None)].into()),
        };
        assert!(
            history_coverage(&fake, &mut l, OWNER, "agent-a", &mut cancel)
                .await
                .unwrap()
                .1
        );
    }
    #[tokio::test]
    async fn history_changed_between_pages_after_acquire_and_missing_page_are_never_empty() {
        let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut l = setup(&t.path().join("ledger.db"), t.path());
        let (_stop, mut cancel) = watch::channel(false);
        let expected = HistorySnapshot {
            count: 0,
            digest: "a".repeat(64),
        };
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("b", vec![], 0, None)].into()),
        };
        assert!(matches!(
            post_acquire_coverage(&fake, &mut l, OWNER, "agent-a", &expected, &mut cancel).await,
            Err(RuntimeError::LedgerRecovery)
        ));
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("a", vec![], 1, None)].into()),
        };
        assert!(matches!(
            history_coverage(&fake, &mut l, OWNER, "agent-a", &mut cancel).await,
            Err(RuntimeError::LedgerRecovery)
        ));
        let fake = HistoryFake {
            pages: StdMutex::new([Err(RuntimeError::Transport)].into()),
        };
        assert!(
            history_coverage(&fake, &mut l, OWNER, "agent-a", &mut cancel)
                .await
                .is_err()
        );
        l.receive_dispatch(OWNER, &event(), inbox::Limits::default(), now())
            .unwrap();
        l.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
        let p = l.prepare_execution(OWNER, "dispatch-a").unwrap();
        let h = history_identity(&p);
        let fake = HistoryFake {
            pages: StdMutex::new(
                [
                    history_page("a", vec![h], 2, Some("dispatch-a")),
                    history_page("b", vec![], 2, None),
                ]
                .into(),
            ),
        };
        assert!(matches!(
            history_coverage(&fake, &mut l, OWNER, "agent-a", &mut cancel).await,
            Err(RuntimeError::LedgerRecovery)
        ));
    }
    #[tokio::test]
    async fn recovery_required_reconciles_known_reply_without_poll_ack_or_model() {
        let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = t.path().join("ledger.db");
        let mut l = setup(&path, t.path());
        l.receive_dispatch(OWNER, &event(), inbox::Limits::default(), now())
            .unwrap();
        l.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
        let p = l.prepare_execution(OWNER, "dispatch-a").unwrap();
        let mut d = p.dispatch.clone();
        d.state = "running".into();
        d.execution_id = Some(p.execution_id.clone());
        l.confirm_execution_start(
            OWNER,
            &inbox::ServerStart {
                dispatch: d,
                newly_started: true,
            },
        )
        .unwrap();
        l.persist_execution_reply(OWNER, "dispatch-a", &p.execution_id, "original response")
            .unwrap();
        let (stop, mut cancel) = watch::channel(false);
        let fake = Fake {
            path,
            mode: Mode::CrossEpochReply,
            calls: StdMutex::new(vec![]),
            stop: stop.clone(),
        };
        let (status, observed) = watch::channel(RuntimeStatus::new("agent-a", "starting", None));
        let cfg = config(t.path());
        let reference = LeaseRef {
            agent_id: "agent-a".into(),
            epoch: 2,
        };
        let future = execution_loop_with_gate(
            &fake,
            OWNER,
            &mut l,
            &cfg,
            &reference,
            &mut cancel,
            &status,
            false,
        );
        let result = tokio::select! {r=future=>r,_=tokio::time::sleep(Duration::from_millis(1400))=>{stop.send_replace(true);Err(RuntimeError::Stopped)}};
        assert!(matches!(result, Err(RuntimeError::Stopped)));
        assert_eq!(observed.borrow().phase, "ledger_recovery_required");
        let calls = fake.calls.lock().unwrap();
        assert!(!calls.contains(&"poll"));
        assert!(!calls.contains(&"ack"));
        assert_eq!(
            l.inbox_record(OWNER, "dispatch-a").unwrap().state,
            State::Replied
        );
    }
    #[tokio::test]
    async fn conflicting_history_requires_explicit_known_reply_recovery() {
        let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let path = t.path().join("ledger.db");
        let mut l = setup(&path, t.path());
        l.receive_dispatch(OWNER, &event(), inbox::Limits::default(), now())
            .unwrap();
        l.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
        let p = l.prepare_execution(OWNER, "dispatch-a").unwrap();
        let mut h = history_identity(&p);
        h.dispatch_id = "dispatch-z".into();
        h.execution_id = "execution-z".into();
        h.binding_id = "binding-z".into();
        let mut contradictory = history_identity(&p);
        contradictory.immutable_digest = "contradictory-digest".into();
        let history = vec![contradictory, h];
        let (_stop, mut cancel) = watch::channel(false);
        let mut cfg = config(t.path());
        cfg.takeover = Takeover::OwnerRequested;
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("b", history.clone(), 2, None)].into()),
        };
        assert!(matches!(
            acquire_checked(&fake, &mut l, OWNER, &cfg, &mut cancel).await,
            Err(RuntimeError::LedgerRecovery)
        ));
        assert!(
            fake.pages.lock().unwrap().is_empty(),
            "only history requested; no Acquire"
        );
        let mut d = p.dispatch.clone();
        d.state = "running".into();
        d.execution_id = Some(p.execution_id.clone());
        l.confirm_execution_start(
            OWNER,
            &inbox::ServerStart {
                dispatch: d,
                newly_started: true,
            },
        )
        .unwrap();
        l.persist_execution_reply(OWNER, "dispatch-a", &p.execution_id, "original response")
            .unwrap();
        cfg.takeover = Takeover::Never;
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("b", history.clone(), 2, None)].into()),
        };
        assert!(matches!(
            acquire_checked(&fake, &mut l, OWNER, &cfg, &mut cancel).await,
            Err(RuntimeError::LedgerRecovery)
        ));
        cfg.takeover = Takeover::OwnerRequested;
        let response = Response::Lease(Lease {
            agent_id: "agent-a".into(),
            owner_user_id: "owner-a".into(),
            device_id: "device-a".into(),
            device_generation: 1,
            epoch: 2,
            expires_at_ms: wall_ms() + 30000,
        });
        let fake = HistoryFake {
            pages: StdMutex::new([history_page("b", history, 2, None), Ok(response)].into()),
        };
        let (_, snapshot, covered) = acquire_checked(&fake, &mut l, OWNER, &cfg, &mut cancel)
            .await
            .unwrap();
        assert!(!covered);
        assert_eq!(snapshot.count, 2);
        assert!(fake.pages.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn missing_historical_receipt_allows_new_work_without_owner_reconciliation() {
        let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut ledger = setup(&tmp.path().join("ledger"), tmp.path());
        let old = inbox::Prepared {
            dispatch: event(),
            execution_id: "old-execution".into(),
        };
        let fake = HistoryFake {
            pages: StdMutex::new(
                [
                    history_page("a", vec![history_identity(&old)], 1, None),
                    Ok(Response::Lease(Lease {
                        agent_id: "agent-a".into(),
                        owner_user_id: "owner-a".into(),
                        device_id: "device-a".into(),
                        device_generation: 1,
                        epoch: 2,
                        expires_at_ms: wall_ms() + 30000,
                    })),
                ]
                .into(),
            ),
        };
        let (_stop, mut cancel) = watch::channel(false);
        let (_, snapshot, covered) =
            acquire_checked(&fake, &mut ledger, OWNER, &config(tmp.path()), &mut cancel)
                .await
                .unwrap();
        assert!(covered);
        assert_eq!(snapshot.count, 1);
        assert!(
            ledger.inbox_record(OWNER, "dispatch-a").is_err(),
            "old execution must never be recreated"
        );
        assert!(fake.pages.lock().unwrap().is_empty());
    }

    #[test]
    fn request_refusals_do_not_impersonate_device_or_lease_revocation() {
        for code in [
            "event_expired",
            "execution_not_running",
            "dispatch_scope_revoked",
            "room_delivery_denied",
            "reply_payload_changed",
        ] {
            assert!(matches!(
                transport_error(409, code, false, true),
                RuntimeError::Request
            ));
            assert!(matches!(
                transport_error(401, code, false, true),
                RuntimeError::Request
            ));
            assert!(matches!(
                transport_error(403, code, false, false),
                RuntimeError::Authorization
            ));
        }
        for code in ["lease_expired", "device_revoked", "unknown_auth_refusal"] {
            assert!(matches!(
                transport_error(403, code, false, true),
                RuntimeError::Authorization
            ));
        }
        assert!(matches!(
            transport_error(503, "matrix_state_unavailable", false, true),
            RuntimeError::Transport
        ));
    }
    struct RequestFailureFake {
        stop: watch::Sender<bool>,
        steps: StdMutex<std::collections::VecDeque<(&'static str, Result<Response, RuntimeError>)>>,
    }
    impl Transport for RequestFailureFake {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            let actual = match op {
                Op::Poll { .. } => "poll",
                Op::Ack { .. } => "ack",
                Op::Start { .. } => "start",
                Op::Finish { .. } => "finish",
                _ => panic!("unexpected provider or replay operation"),
            };
            let mut steps = self.steps.lock().unwrap();
            let (expected, result) = steps.pop_front().expect("bounded recovery calls");
            assert_eq!(actual, expected);
            if steps.is_empty() {
                self.stop.send_replace(true);
            }
            result
        }
    }
    #[tokio::test]
    async fn ack_transient_failure_retries_and_request_refusal_does_not_block_followup() {
        for refused in [false, true] {
            let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let mut ledger = setup(&tmp.path().join("ledger"), tmp.path());
            let mut next = event();
            next.id = "dispatch-next".into();
            next.event_id = "$next:test".into();
            let mut steps = vec![
                ("poll", Ok(Response::Events(vec![wire(&event())]))),
                (
                    "ack",
                    Err(if refused {
                        RuntimeError::Request
                    } else {
                        RuntimeError::Transport
                    }),
                ),
            ];
            if refused {
                steps.push(("finish", Ok(Response::Finished)));
            } else {
                steps.extend([
                    ("poll", Ok(Response::Events(vec![wire(&event())]))),
                    ("ack", Ok(Response::Acknowledged)),
                    ("start", Err(RuntimeError::Transport)),
                    ("finish", Ok(Response::Finished)),
                ]);
            }
            steps.extend([
                ("poll", Ok(Response::Events(vec![wire(&next)]))),
                ("ack", Ok(Response::Acknowledged)),
                ("start", Err(RuntimeError::Transport)),
                ("finish", Ok(Response::Finished)),
                ("poll", Ok(Response::Events(vec![]))),
            ]);
            let (stop, mut cancel) = watch::channel(false);
            let fake = RequestFailureFake {
                stop,
                steps: StdMutex::new(steps.into()),
            };
            let (status, _) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
            assert!(matches!(
                tokio::time::timeout(
                    Duration::from_secs(6),
                    execution_loop(
                        &fake,
                        OWNER,
                        &mut ledger,
                        &config(tmp.path()),
                        &lease(),
                        &mut cancel,
                        &status,
                    )
                )
                .await
                .unwrap(),
                Err(RuntimeError::Stopped)
            ));
            assert!(fake.steps.lock().unwrap().is_empty());
            assert_eq!(
                ledger.inbox_record(OWNER, "dispatch-next").unwrap().state,
                State::Unknown
            );
            assert_eq!(
                ledger.inbox_record(OWNER, "dispatch-a").unwrap().state,
                if refused {
                    State::Rejected
                } else {
                    State::Unknown
                }
            );
        }
    }
    #[tokio::test]
    async fn restart_redelivery_of_prepared_or_unknown_start_retires_offer_without_replay() {
        for interrupted in [false, true] {
            let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let mut ledger = setup(&tmp.path().join("ledger"), tmp.path());
            ledger
                .receive_dispatch(OWNER, &event(), inbox::Limits::default(), now())
                .unwrap();
            ledger.acknowledge_dispatch(OWNER, "dispatch-a").unwrap();
            let prepared = ledger.prepare_execution(OWNER, "dispatch-a").unwrap();
            if interrupted {
                ledger
                    .finish_local_execution(
                        OWNER,
                        "dispatch-a",
                        &prepared.execution_id,
                        Finish::Unknown,
                    )
                    .unwrap();
            }
            let mut redelivery = event();
            redelivery.dispatch_epoch = Some(2);
            let mut next = redelivery.clone();
            next.id = "dispatch-next".into();
            next.event_id = "$next:test".into();
            let (stop, mut cancel) = watch::channel(false);
            let fake = RequestFailureFake {
                stop,
                steps: StdMutex::new(
                    [
                        ("poll", Ok(Response::Events(vec![wire(&redelivery)]))),
                        ("finish", Ok(Response::Finished)),
                        ("poll", Ok(Response::Events(vec![wire(&next)]))),
                        ("ack", Ok(Response::Acknowledged)),
                        ("start", Err(RuntimeError::Transport)),
                        ("finish", Ok(Response::Finished)),
                        ("poll", Ok(Response::Events(vec![]))),
                    ]
                    .into(),
                ),
            };
            let mut current = lease();
            current.epoch = 2;
            let (status, _) = watch::channel(RuntimeStatus::new("agent-a", "test", None));
            assert!(matches!(
                tokio::time::timeout(
                    Duration::from_secs(5),
                    execution_loop(
                        &fake,
                        OWNER,
                        &mut ledger,
                        &config(tmp.path()),
                        &current,
                        &mut cancel,
                        &status,
                    )
                )
                .await
                .unwrap(),
                Err(RuntimeError::Stopped)
            ));
            let old = ledger.inbox_record(OWNER, "dispatch-a").unwrap();
            assert_eq!(old.state, State::Unknown);
            assert_eq!(old.execution_id, Some(prepared.execution_id));
            assert_eq!(old.dispatch.dispatch_epoch, Some(1));
            assert!(fake.steps.lock().unwrap().is_empty());
        }
    }
    struct RunningFailureFake {
        authorization: bool,
    }
    impl Transport for RunningFailureFake {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            assert!(matches!(op, Op::AuthorizeTool { .. }));
            Err(if self.authorization {
                RuntimeError::Authorization
            } else {
                RuntimeError::Transport
            })
        }
    }
    #[tokio::test]
    async fn running_monitor_preserves_transport_failure_instead_of_revoking_agent() {
        let (_stop, cancel) = watch::channel(false);
        let prepared = inbox::Prepared {
            dispatch: event(),
            execution_id: "execution-a".into(),
        };
        let result = monitor_running(
            &RunningFailureFake {
                authorization: false,
            },
            &lease(),
            &prepared,
            cancel,
        )
        .await;
        assert!(matches!(result, RuntimeError::Transport));
    }
    struct RestartLeaseFake {
        history_calls: std::sync::atomic::AtomicUsize,
        acquire_calls: std::sync::atomic::AtomicUsize,
        busy_attempts: usize,
        authorization_denied: bool,
    }
    impl Transport for RestartLeaseFake {
        async fn execute(&self, op: Op) -> Result<Response, RuntimeError> {
            use std::sync::atomic::Ordering;
            match op {
                Op::History { .. } => {
                    self.history_calls.fetch_add(1, Ordering::SeqCst);
                    history_page("a", vec![], 0, None)
                }
                Op::Acquire {
                    takeover,
                    history_snapshot,
                    ..
                } => {
                    assert!(matches!(takeover, Takeover::Never));
                    assert_eq!(history_snapshot.count, 0);
                    let attempt = self.acquire_calls.fetch_add(1, Ordering::SeqCst);
                    if self.authorization_denied {
                        Err(RuntimeError::Authorization)
                    } else if attempt < self.busy_attempts {
                        Err(RuntimeError::LeaseBusy)
                    } else {
                        Ok(Response::Lease(Lease {
                            agent_id: "agent-a".into(),
                            owner_user_id: "owner-a".into(),
                            device_id: "device-a".into(),
                            device_generation: 2,
                            epoch: 2,
                            expires_at_ms: wall_ms() + 30000,
                        }))
                    }
                }
                _ => panic!("restart wait must not poll, ACK, start, release, or execute work"),
            }
        }
    }
    fn restart_fake(busy: usize, deny: bool) -> RestartLeaseFake {
        RestartLeaseFake {
            history_calls: std::sync::atomic::AtomicUsize::new(0),
            acquire_calls: std::sync::atomic::AtomicUsize::new(0),
            busy_attempts: busy,
            authorization_denied: deny,
        }
    }
    fn recovery_config(path: &std::path::Path) -> StartConfig {
        let mut cfg = config(path);
        cfg.recovery_device_id = Some("device-a".into());
        cfg.recovery_consent_id = Some(new_consent_id().unwrap());
        cfg
    }
    #[tokio::test]
    async fn restart_lease_busy_waits_for_natural_expiry_and_rechecks_history_without_takeover() {
        let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut l = setup(&t.path().join("ledger.db"), t.path());
        let fake = restart_fake(2, false);
        let (_stop, mut cancel) = watch::channel(false);
        let (status, observed) = watch::channel(RuntimeStatus::new("agent-a", "starting", None));
        let cfg = recovery_config(t.path());
        let (lease, _, covered) = acquire_for_start(
            &fake,
            &mut l,
            OWNER,
            &cfg,
            &mut cancel,
            &status,
            &status,
            RecoveryLeaseWait {
                total: Duration::from_millis(200),
                interval: Duration::from_millis(2),
            },
        )
        .await
        .unwrap();
        assert!(covered);
        assert_eq!(lease.epoch, 2);
        assert_eq!(observed.borrow().phase, "waiting_for_lease");
        assert_eq!(
            fake.acquire_calls.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
        assert_eq!(
            fake.history_calls.load(std::sync::atomic::Ordering::SeqCst),
            3
        );
    }
    #[tokio::test]
    async fn restart_lease_busy_deadline_is_finite_and_stop_interrupts_wait() {
        for stopped in [false, true] {
            let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let mut l = setup(&t.path().join("ledger.db"), t.path());
            let fake = restart_fake(usize::MAX, false);
            let (stop, mut cancel) = watch::channel(false);
            let (status, mut observed) =
                watch::channel(RuntimeStatus::new("agent-a", "starting", None));
            let cfg = recovery_config(t.path());
            let stop_request = stop.clone();
            let cancel_task = tokio::spawn(async move {
                if stopped {
                    observed.changed().await.unwrap();
                    assert_eq!(observed.borrow().phase, "waiting_for_lease");
                    stop_request.send_replace(true);
                }
            });
            let result = acquire_for_start(
                &fake,
                &mut l,
                OWNER,
                &cfg,
                &mut cancel,
                &status,
                &status,
                RecoveryLeaseWait {
                    total: Duration::from_millis(30),
                    interval: Duration::from_millis(5),
                },
            )
            .await;
            cancel_task.await.unwrap();
            assert!(if stopped {
                matches!(result, Err(RuntimeError::Stopped))
            } else {
                matches!(result, Err(RuntimeError::LeaseBusy))
            });
            let attempts = fake.acquire_calls.load(std::sync::atomic::Ordering::SeqCst);
            assert!((1..=8).contains(&attempts));
            if stopped {
                assert_eq!(attempts, 1);
            }
        }
    }
    #[tokio::test]
    async fn restart_wait_never_retries_authorization_denial_or_manual_start_conflict() {
        for deny in [false, true] {
            let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let mut l = setup(&t.path().join("ledger.db"), t.path());
            let fake = restart_fake(usize::MAX, deny);
            let (_stop, mut cancel) = watch::channel(false);
            let (status, observed) =
                watch::channel(RuntimeStatus::new("agent-a", "starting", None));
            let cfg = if deny {
                recovery_config(t.path())
            } else {
                config(t.path())
            };
            let result = acquire_for_start(
                &fake,
                &mut l,
                OWNER,
                &cfg,
                &mut cancel,
                &status,
                &status,
                RecoveryLeaseWait {
                    total: Duration::from_millis(100),
                    interval: Duration::from_millis(5),
                },
            )
            .await;
            assert!(if deny {
                matches!(result, Err(RuntimeError::Authorization))
            } else {
                matches!(result, Err(RuntimeError::LeaseBusy))
            });
            assert_eq!(
                fake.acquire_calls.load(std::sync::atomic::Ordering::SeqCst),
                1
            );
            assert_eq!(observed.borrow().phase, "starting");
        }
    }
    #[tokio::test]
    async fn fresh_history_acquire_sends_snapshot_and_post_check_keeps_models_allowed() {
        let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut l = setup(&t.path().join("ledger.db"), t.path());
        let (_stop, mut cancel) = watch::channel(false);
        let lease = Response::Lease(Lease {
            agent_id: "agent-a".into(),
            owner_user_id: "owner-a".into(),
            device_id: "device-a".into(),
            device_generation: 1,
            epoch: 1,
            expires_at_ms: wall_ms() + 30000,
        });
        let fake = HistoryFake {
            pages: StdMutex::new(
                [
                    history_page("a", vec![], 0, None),
                    Ok(lease),
                    history_page("a", vec![], 0, None),
                ]
                .into(),
            ),
        };
        let (_, snapshot, pre) =
            acquire_checked(&fake, &mut l, OWNER, &config(t.path()), &mut cancel)
                .await
                .unwrap();
        assert!(pre);
        assert!(
            post_acquire_coverage(&fake, &mut l, OWNER, "agent-a", &snapshot, &mut cancel)
                .await
                .unwrap()
        );
        assert!(fake.pages.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn history_wrong_witness_digest_is_not_an_empty_or_complete_ledger() {
        let t = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let mut l = setup(&t.path().join("ledger.db"), t.path());
        let (_stop, mut cancel) = watch::channel(false);
        let mut page = history_page("a", vec![], 0, None).unwrap();
        if let Response::History(page) = &mut page {
            page.snapshot.digest = "0".repeat(64);
        }
        let fake = HistoryFake {
            pages: StdMutex::new([Ok(page)].into()),
        };
        assert!(matches!(
            history_coverage(&fake, &mut l, OWNER, "agent-a", &mut cancel).await,
            Err(RuntimeError::LedgerRecovery)
        ));
    }
}

#[cfg(all(test, unix))]
mod http_tests;

#[cfg(test)]
mod device_assignment_tests {
    use super::*;
    #[test]
    fn assignment_is_required_and_cannot_be_bypassed_by_takeover() {
        let correct = serde_json::json!({"agent":{"id":"a","ownerUserId":"owner","puppetMxid":"@a:test","displayName":"A","state":"active","executionDeviceId":"d","generation":1}});
        assert!(verify_execution_device(&correct, "a", "d").is_ok());
        for mode in [Takeover::Never, Takeover::OwnerRequested] {
            let _ = mode;
            assert!(matches!(
                verify_execution_device(&correct, "a", "other"),
                Err(RuntimeError::ExecutionDevice)
            ));
        }
        assert!(matches!(
            verify_execution_device(&correct, "other", "d"),
            Err(RuntimeError::ExecutionDevice)
        ));
        assert!(matches!(
            verify_execution_device(&serde_json::json!({"agent":null}), "a", "d"),
            Err(RuntimeError::ExecutionDevice)
        ));
        let mut invalid = correct.clone();
        invalid["agent"]["generation"] = serde_json::json!(0);
        assert!(matches!(
            verify_execution_device(&invalid, "a", "d"),
            Err(RuntimeError::ExecutionDevice)
        ));
        invalid = correct.clone();
        invalid["agent"]["executionDeviceId"] = serde_json::Value::Null;
        assert!(matches!(
            verify_execution_device(&invalid, "a", "d"),
            Err(RuntimeError::ExecutionDevice)
        ));
        invalid = correct.clone();
        invalid["agent"]
            .as_object_mut()
            .unwrap()
            .remove("executionDeviceId");
        assert!(matches!(
            verify_execution_device(&invalid, "a", "d"),
            Err(RuntimeError::ExecutionDevice)
        ));
    }
}
