//! Native management reuses the current SDK OAuth identity. The SDK alone owns
//! OAuth login, refresh and logout; the owner engine never starts another login.
use super::model::{BindingAction, Command, LocalAction, ProviderAction};
use ::hagency::native_owner::{MatrixAccessToken, MatrixTokenSource, NativeError, NativeOwner};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex, Weak},
};

fn matching_binding_recovery(command: &Command, saved: &Value) -> Option<Command> {
    let Command::BindAgent { agent_id, input } = command else {return None;};
    let mut matches = saved["commands"].as_array()?.iter().filter(|record|
        record["operation"] == "agent.bind" && record["agentId"] == *agent_id
        && record["input"]["projectId"] == input["projectId"]
        && record["input"]["roomId"] == input["roomId"]
        && matches!(record["state"].as_str(), Some("unknown" | "pending")));
    let id = matches.next()?["id"].as_str()?.to_owned();
    if matches.next().is_some() {return None;}
    Some(Command::ResumeAgentCommand { id })
}

/// A reserved puppet name selects the binding protocol, never authority. The
/// owner's server roster must still identify the exact Agent before any write.
pub(crate) fn is_agent_invitation(user: &matrix_sdk::ruma::UserId) -> bool {
    user.localpart().starts_with("_hagency_agt_")
}

fn invited_agent_command(context: &Value, room: &str, user: &str) -> Result<Command, String> {
    if context["roomId"] != room {return Err("invalid_server_response".into());}
    let project = context["projectId"].as_str().filter(|id| !id.is_empty()).ok_or("invalid_server_response")?;
    let agents = context["agents"].as_array().ok_or("invalid_server_response")?;
    let agent = agents.iter().find(|a| a["puppetMxid"] == user).ok_or("agent_owner_binding_required")?;
    if !matches!(agent["state"].as_str(), Some("active" | "creating" | "suspended")) {
        return Err("agent_retired".into());
    }
    let id = agent["id"].as_str().filter(|id| !id.is_empty()).ok_or("invalid_server_response")?;
    Ok(Command::BindAgent {agent_id:id.into(), input:serde_json::json!({
        "projectId":project,"roomId":room,"idempotencyKey":uuid::Uuid::new_v4().to_string()
    })})
}

fn invited_agent_joined(value: &Value, agent: &str, room: &str) -> Result<bool, String> {
    let binding = &value["creation"]["binding"];
    if binding["roomId"] != room || binding["agentId"] != agent {
        return Err("invalid_server_response".into());
    }
    match value["commandState"].as_str() {
        Some("active") if binding["state"] == "active" => Ok(true),
        Some("pending") => Ok(false),
        Some("suspended") => Err("agent_binding_suspended".into()),
        _ => Err("invalid_server_response".into()),
    }
}

/// A UI waiting deadline may detach only these reads. The request itself must
/// keep running: even a first read bootstraps owner authorization and a device.
/// Never extend this list to actions that create, resume, save or run anything.
pub(super) fn management_read(command: &Command) -> bool {
    matches!(command,
        Command::Projects | Command::ProjectRooms {..} | Command::SpaceCandidates {..}
        | Command::RoomRoster {..} | Command::Agents | Command::RoomAgents | Command::Bindings {..}
        | Command::Devices | Command::AgentDetails {..} | Command::AgentConfigurationStatus {..}
        | Command::AgentOwnerDirectStatus {..} | Command::ScopeServiceState {..}
        | Command::LoginStatus | Command::AgentCommands | Command::Creations | Command::Creation {..}
        | Command::Binding {action: BindingAction::Status, ..}
        | Command::Provider {action: ProviderAction::Status | ProviderAction::Models}
        | Command::RuntimeProvider {action: ProviderAction::Status | ProviderAction::Models, ..}
        | Command::Local {action: LocalAction::AgentModel | LocalAction::AgentPolicy
            | LocalAction::Policy | LocalAction::Runtime
            | LocalAction::AgentServiceStatus | LocalAction::Approvals, input: None, ..}
    )
}

/// The Matrix SDK owns OAuth rotation and persistence. This helper never reads
/// a refresh token, and checks the caller's identity boundary on both sides.
pub(crate) async fn refresh_sdk_oauth(
    client: &matrix_sdk::Client,
    live_check: impl Fn() -> Result<(), NativeError> + Send + Sync,
) -> Result<(), NativeError> {
    live_check()?;
    tokio::time::timeout(std::time::Duration::from_secs(8), client.oauth().refresh_access_token())
        .await
        .map_err(|_| source_error("matrix_authorization_unavailable"))?
        .map_err(|error| {
            let code = crate::session_auth::refresh_failure_code(&error);
            makepad_widgets::warning!("Matrix OAuth refresh failed: {code}");
            source_error(code)
        })?;
    makepad_widgets::log!("Matrix OAuth refresh completed");
    live_check()?;
    Ok(())
}

/// Only engine-defined static codes may enter startup diagnostics. Neither a
/// provider response nor arbitrary source error text is logged.
fn recovery_error_code(code: &str) -> &'static str {
    match code {
        "runtime_profile_unavailable" => "runtime_profile_unavailable",
        "runtime_stopped" => "runtime_stopped",
        "agent_runtime_already_active" => "agent_runtime_already_active",
        "agent_execution_device_required" => "agent_execution_device_required",
        "ledger_recovery_required" => "ledger_recovery_required",
        "owner_authorization_required" => "owner_authorization_required",
        "owner_scope_required" => "owner_scope_required",
        "sign_in_required" => "sign_in_required",
        "invalid_arguments" => "invalid_arguments",
        "invalid_server_response" => "invalid_server_response",
        "invalid_runtime_start" => "invalid_runtime_start",
        "codex_strict_quota_unavailable" => "codex_strict_quota_unavailable",
        "provider_profile_mismatch" => "provider_profile_mismatch",
        "provider_authorization_lost" => "provider_authorization_lost",
        "provider_unavailable" => "provider_unavailable",
        "local_state_unavailable" => "local_state_unavailable",
        "device_transport_unavailable" => "device_transport_unavailable",
        "matrix_account_changed" => "matrix_account_changed",
        "matrix_authorization_unavailable" => "matrix_authorization_unavailable",
        "matrix_authorization_required" => "matrix_authorization_required",
        "matrix_oauth_sign_in_required" => "matrix_oauth_sign_in_required",
        "runtime_recovery_state_unavailable" => "runtime_recovery_state_unavailable",
        "server_unavailable" => "server_unavailable",
        _ => "runtime_recovery_unrecognized_error",
    }
}
fn log_runtime_recovery(result: Result<Value, NativeError>) {
    match result {
        Ok(value) => {
            if let Some(items)=value["recoveries"].as_array() {
                makepad_widgets::log!("Agent startup recovery: {} eligible bindings",items.len());
                for (index,item) in items.iter().enumerate() {
                    if item["restored"]==false {
                        let code=recovery_error_code(item["error"].as_str().unwrap_or(""));
                        makepad_widgets::warning!("Agent startup recovery item {index} failed: {code}");
                    } else {
                        makepad_widgets::log!("Agent startup recovery item {index}: start accepted; readiness not yet confirmed");
                    }
                }
            }
        }
        Err(error) => {
            let code=recovery_error_code(&error.code);
            makepad_widgets::warning!("Agent startup recovery unavailable: {code}");
        }
    }
}

struct SdkTokens(Weak<Inner>);
fn source_error(code: &str) -> NativeError {
    NativeError {
        status: if code == "matrix_authorization_unavailable" {503} else {401},
        code: code.into(),
    }
}
impl MatrixTokenSource for SdkTokens {
    fn permits_explicit_reenrollment(&self) -> bool {
        self.0.upgrade().is_some_and(|inner| {
            inner.explicit_reenrollment.load(std::sync::atomic::Ordering::Acquire)
                && Service(inner).check().is_ok()
        })
    }
    fn consume_explicit_reenrollment(&self) -> bool {
        self.0.upgrade().is_some_and(|inner| {
            let service = Service(inner);
            service.check().is_ok()
                && service.0.explicit_reenrollment.swap(false, std::sync::atomic::Ordering::AcqRel)
                && service.check().is_ok()
        })
    }
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async move {
            let service = Service(
                self.0
                    .upgrade()
                    .ok_or_else(|| source_error("matrix_account_changed"))?,
            );
            service.check().map_err(|e| source_error(&e))?;
            let client = crate::sliding_sync::get_client()
                .ok_or_else(|| source_error("matrix_sign_in_required"))?;
            let oauth = client.oauth();
            if oauth.client_id().is_none() {
                return Err(source_error("matrix_oauth_sign_in_required"));
            }
            let metadata = tokio::time::timeout(
                std::time::Duration::from_secs(8),
                oauth.cached_server_metadata(),
            )
            .await
            .map_err(|_| source_error("matrix_authorization_unavailable"))?
            .map_err(|_| source_error("matrix_authorization_unavailable"))?;
            service.check().map_err(|e| source_error(&e))?;
            if metadata.issuer.as_str() != format!("{}auth/", service.0.origin) {
                return Err(source_error("unsupported_hagency_issuer"));
            }
            // This actual SDK request refreshes expired OAuth credentials. Do not
            // call the owner service here: its session lock may be awaiting us.
            let identity = tokio::time::timeout(std::time::Duration::from_secs(8), client.whoami())
                .await
                .map_err(|_| source_error("matrix_authorization_unavailable"))?
                .map_err(|error| {
                    source_error(
                        if crate::session_auth::http_requires_login(&error)
                        {
                            "matrix_authorization_required"
                        } else {
                            "matrix_authorization_unavailable"
                        },
                    )
                })?;
            service.check().map_err(|e| source_error(&e))?;
            if identity.user_id.as_str() != service.0.owner
                || identity.device_id.as_deref() != client.device_id()
            {
                return Err(source_error("matrix_account_mismatch"));
            }
            Ok(MatrixAccessToken {
                access_token: client
                    .access_token()
                    .ok_or_else(|| source_error("matrix_authorization_required"))?,
                client_id: oauth
                    .client_id()
                    .ok_or_else(|| source_error("matrix_oauth_sign_in_required"))?
                    .as_str()
                    .into(),
            })
        })
    }
    fn refresh_access_token(
        &self,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>> {
        Box::pin(async move {
            let service = Service(self.0.upgrade().ok_or_else(|| source_error("matrix_account_changed"))?);
            service.check().map_err(|e| source_error(&e))?;
            let client = crate::sliding_sync::get_client().ok_or_else(|| source_error("matrix_sign_in_required"))?;
            if client.oauth().client_id().is_none() {
                return Err(source_error("matrix_oauth_sign_in_required"));
            }
            refresh_sdk_oauth(&client, || service.check().map_err(|e| source_error(&e))).await?;
            // Fresh SDK whoami and issuer/device checks prove the rotated token.
            // A late account transition cannot authorize its old snapshot.
            self.access_token().await
        })
    }

}

static CURRENT: LazyLock<Mutex<Option<Service>>> = LazyLock::new(|| Mutex::new(None));
static PROCESS_CLOSING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
mod lifecycle;
mod daemon_bridge;
pub use daemon_bridge::LocalAgentHost;
mod signin;
mod adoption;
mod allocation_worker;
#[cfg(feature = "shared-execution")]
mod shared_worker;
#[cfg(feature = "shared-successors")]
pub(crate) mod shared_successors;
#[cfg(feature = "shared-intake")]
mod shared_intake;
#[cfg(feature="shared-budget-execution")]
mod shared_budget;

struct Inner {
    encrypted_approvals: Option<::hagency::encrypted_approvals::Inbox>,
    origin: String,
    owner: String,
    epoch: u64,
    client_revision: u64,
    // A completed OAuth sign-in permits one native registration. Restoring an
    // active native session also discards it; it cannot authorize a later revoke.
    explicit_reenrollment: std::sync::atomic::AtomicBool,
    /// The registry and daemon home that decide who executes this owner.
    locations: crate::hagency::hosting::Locations,
    /// The account's one owner root on this computer (ADR-192). An error is
    /// reported by the operations that need the root, not by Matrix-only ones.
    state: Result<PathBuf, String>,
    native: tokio::sync::Mutex<Option<NativeOwner>>,
    remote: Mutex<Option<Arc<::hagency::daemon::bridge::ControllerClient>>>,
    // Separate from the async initialization/cleanup lock: account transitions
    // must revoke queued ledger work before awaiting SDK or native shutdown.
    native_retirement: Mutex<Option<NativeOwner>>,
    current_initialization: Mutex<Option<::hagency::native_owner::CurrentProfileSelection>>,
    setup_pending: std::sync::atomic::AtomicBool,
    activation: tokio::sync::Mutex<adoption::ActivationTask>,
    allocations: allocation_worker::Worker,
    #[cfg(feature = "shared-execution")]
    shared: shared_worker::Worker,
    retired: std::sync::atomic::AtomicBool,
    logout_requested: std::sync::atomic::AtomicBool,
    matrix_checked: tokio::sync::Mutex<Option<std::time::Instant>>,
}
#[derive(Clone)]
pub struct Service(Arc<Inner>);
impl Service {
    /// The account's one owner root on this computer (ADR-192): see
    /// [`crate::hagency::hosting::sign_in_root`].
    fn state_for(locations: &crate::hagency::hosting::Locations, origin: &str, owner: &str) -> Result<PathBuf, String> {
        crate::hagency::hosting::sign_in_root(locations, crate::app_data_dir(), origin, owner)
    }
    /// The owner root, for operations that open or revoke it in this process.
    fn root(&self) -> Result<&std::path::Path, String> {
        self.0.state.as_deref().map_err(Clone::clone)
    }
    pub fn for_current() -> Result<Self, String> {
        if PROCESS_CLOSING.load(std::sync::atomic::Ordering::Acquire)
            || crate::logout::logout_state_machine::is_logout_in_progress()
            || crate::sliding_sync::token_expired()
            || crate::account_session::is_closing()
        {
            return Err("matrix_account_changing".into());
        }
        let epoch = crate::account_session::epoch();
        let client_revision = crate::sliding_sync::client_revision();
        let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
        let owner = client
            .user_id()
            .ok_or("matrix_sign_in_required")?
            .to_string();
        let origin = client.homeserver().to_string();
        if epoch != crate::account_session::epoch() || crate::sliding_sync::account_changing() {
            return Err("matrix_account_changing".into());
        }
        let mut current = CURRENT.lock().map_err(|_| "native_owner_unavailable")?;
        if epoch != crate::account_session::epoch()
            || PROCESS_CLOSING.load(std::sync::atomic::Ordering::Acquire)
            || client_revision != crate::sliding_sync::client_revision()
            || crate::sliding_sync::account_changing()
            || crate::sliding_sync::token_expired()
            || crate::logout::logout_state_machine::is_logout_in_progress()
            || crate::account_session::is_closing()
        {
            return Err("matrix_account_changing".into());
        }
        if let Some(service) = current.as_ref()
            && service.0.origin == origin
            && service.0.owner == owner
            && service.0.epoch == epoch
            && service.0.client_revision == client_revision
            && !service.0.retired.load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(service.clone());
        }
        if let Some(old) = current.take() {
            old.retire();
            crate::sliding_sync::backend_runtime().spawn(async move {
                if old.suspend().await.is_err() {
                    makepad_widgets::warning!("Previous native owner drain requires recovery");
                }
            });
        }
        let locations = crate::hagency::hosting::Locations::system()?;
        let state = Self::state_for(&locations, &origin, &owner);
        if let Err(code) = &state {
            makepad_widgets::warning!("Owner root unavailable: {code}");
        }
        let service = Self(Arc::new(Inner {
            encrypted_approvals: Some(::hagency::encrypted_approvals::Inbox::attach(&client)),
            origin,
            owner,
            epoch,
            client_revision,
            explicit_reenrollment: std::sync::atomic::AtomicBool::new(
                signin::consume(&client, epoch, client_revision),
            ),
            locations,
            state,
            native: tokio::sync::Mutex::new(None),
            remote: Mutex::new(None),
            native_retirement: Mutex::new(None),
            current_initialization: Mutex::new(None),
            setup_pending: std::sync::atomic::AtomicBool::new(false),
            activation: Default::default(),
            allocations: Default::default(),
            #[cfg(feature = "shared-execution")]
            shared: Default::default(),
            retired: std::sync::atomic::AtomicBool::new(false),
            logout_requested: std::sync::atomic::AtomicBool::new(false),
            matrix_checked: tokio::sync::Mutex::new(None),
        }));
        *current = Some(service.clone());
        service.watch_daemon_assignment();
        let startup = service.clone();
        crate::sliding_sync::backend_runtime().spawn(async move {
            let _ = startup.execute(Command::LoginStatus).await;
        });
        Ok(service)
    }
    /// Request pin includes SDK replacement even for the same Matrix account.
    /// The engine independently pins issuer/subject in every ledger/provider path.
    pub fn identity(&self) -> String {
        serde_json::to_string(&(self.0.epoch, self.0.client_revision, &self.0.origin, &self.0.owner)).unwrap_or_default()
    }
    fn check(&self) -> Result<(), String> {
        if PROCESS_CLOSING.load(std::sync::atomic::Ordering::Acquire)
            || self.0.retired.load(std::sync::atomic::Ordering::Acquire)
            || self.0.epoch != crate::account_session::epoch()
            || self.0.client_revision != crate::sliding_sync::client_revision()
            || crate::sliding_sync::account_changing()
            || crate::sliding_sync::token_expired()
            || crate::logout::logout_state_machine::is_logout_in_progress()
            || crate::account_session::is_closing()
        {
            return Err("matrix_account_changed".into());
        }
        let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
        if self.0.client_revision != crate::sliding_sync::client_revision()
            || client.homeserver().as_str() != self.0.origin
            || client
                .user_id()
                .is_none_or(|id| id.as_str() != self.0.owner)
        {
            return Err("matrix_account_changed".into());
        }
        Ok(())
    }
    async fn verify_matrix(&self, fresh: bool) -> Result<(), String> {
        self.check()?;
        let mut checked = self.0.matrix_checked.lock().await;
        self.check()?;
        if !fresh && checked.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(30)) {
            return Ok(());
        }
        let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
        let response = tokio::time::timeout(std::time::Duration::from_secs(8), client.whoami())
            .await
            .map_err(|_| "matrix_authorization_unavailable")?;
        self.check()?;
        match response {
            Ok(identity) if identity.user_id.as_str() == self.0.owner => {
                *checked = Some(std::time::Instant::now());
                Ok(())
            }
            Ok(_) => {
                drop(checked);
                self.shutdown().await;
                Err("matrix_account_mismatch".into())
            }
            Err(error) => {
                *checked = None;
                if crate::session_auth::http_requires_login(&error)
                {
                    drop(checked);
                    self.shutdown().await;
                    Err("matrix_authorization_required".into())
                } else {
                    Err("matrix_authorization_unavailable".into())
                }
            }
        }
    }
    /// Reads the selected Space/Room through this exact SDK account. Never
    /// joins a Room or grants access based on parent Space membership.
    pub async fn room_members(&self, room_id: String, space: bool) -> Result<Value, String> {
        self.verify_matrix(false).await?;
        let id = matrix_sdk::ruma::OwnedRoomId::try_from(room_id.as_str())
            .map_err(|_| "invalid_room_id")?;
        let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
        // Pin the captured SDK client itself: account switching may happen
        // between verification and acquisition of the global client.
        if client.homeserver().as_str() != self.0.origin
            || client.user_id().is_none_or(|id| id.as_str() != self.0.owner)
        {
            return Err("matrix_account_changed".into());
        }
        self.check()?;
        let room = client.get_room(&id).ok_or("room_join_required")?;
        if room.state() != matrix_sdk::RoomState::Joined {
            return Err("room_join_required".into());
        }
        if room.is_space() != space {
            return Err("room_kind_mismatch".into());
        }
        let members = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            room.members(matrix_sdk::RoomMemberships::JOIN | matrix_sdk::RoomMemberships::INVITE),
        )
        .await
        .map_err(|_| "members_unavailable")?
        .map_err(|_| "members_unavailable")?;
        self.check()?;
        if room.state() != matrix_sdk::RoomState::Joined {
            return Err("room_join_required".into());
        }
        let can_invite = members.iter().any(|m| {
            m.user_id().as_str() == self.0.owner
                && m.membership() == &matrix_sdk::ruma::events::room::member::MembershipState::Join
                && m.can_invite()
        });
        let mut members = members.iter().map(|m| {
            let power = match m.power_level() {
                matrix_sdk::ruma::events::room::power_levels::UserPowerLevel::Int(v) => v.to_string(),
                _ => "Creator".into(),
            };
            serde_json::json!({"userId":m.user_id().as_str(),"name":m.display_name().unwrap_or(m.user_id().as_str()),
                "membership":m.membership().as_str(),"role":format!("{:?}",m.suggested_role_for_power_level()),"power":power})
        }).collect::<Vec<_>>();
        members.sort_by_key(|m| {
            (
                m["name"].as_str().unwrap_or("").to_lowercase(),
                m["userId"].as_str().unwrap_or("").to_owned(),
            )
        });
        self.check()?;
        Ok(
            serde_json::json!({"roomId":room_id,"space":space,"canInvite":can_invite,"members":members}),
        )
    }
    /// Resolve only an existing, visible Project/Room association. This never
    /// adopts a Matrix Room and never substitutes an SDK invite for a binding.
    pub async fn agents_for_registered_room(&self, room_id: &str) -> Result<Value, String> {
        self.check()?;
        let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
        let room = client.get_room(matrix_sdk::ruma::RoomId::parse(room_id).map_err(|_| "invalid_room_id")?.as_ref()).ok_or("room_join_required")?;
        if room.state() != matrix_sdk::RoomState::Joined { return Err("room_join_required".into()); }
        let result = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let roster = self.execute(Command::RoomAgents).await?;
            let projects = self.execute(Command::Projects).await?;
            let projects = projects["projects"].as_array().ok_or("room_discovery_unavailable")?;
            if projects.len() > 128 { return Err("room_discovery_incomplete".into()); }
            for project in projects {
                let Some(id) = project["id"].as_str() else {continue;};
                let rooms = self.execute(Command::ProjectRooms {project_id:id.into()}).await?;
                if rooms["rooms"].as_array().is_some_and(|rooms| rooms.iter().any(|room| room["roomId"] == room_id && room["active"] == true)) {
                    self.check()?;
                    return Ok(serde_json::json!({"roomId":room_id,"projectId":id,"agents":roster["agents"]}));
                }
            }
            Err("room_project_registration_required".into())
        }).await.map_err(|_| "room_discovery_unavailable")?;
        self.check()?;
        result
    }
    /// Read this owner's existing Room services without adopting or starting any.
    pub async fn room_agent_services(&self, room_id: &str) -> Result<Value, String> {
        self.check()?;
        let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
        let room = client.get_room(matrix_sdk::ruma::RoomId::parse(room_id).map_err(|_| "invalid_room_id")?.as_ref()).ok_or("room_join_required")?;
        if room.state() != matrix_sdk::RoomState::Joined {return Err("room_join_required".into());}
        let result = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            let roster = self.execute(Command::Agents).await?;
            let devices = self.execute(Command::Devices).await?;
            let agents = roster["agents"].as_array().ok_or("room_discovery_unavailable")?;
            if agents.len() > 128 {return Err("room_discovery_incomplete".into());}
            let mut services = Vec::new();
            for agent in agents {
                if agent["state"] != "active" {continue;}
                let Some(id) = agent["id"].as_str() else {continue;};
                let bindings = self.execute(Command::Bindings {agent_id:id.into()}).await?;
                let Some(binding) = bindings["bindings"].as_array().and_then(|rows| rows.iter().find(|b|b["roomId"] == room_id && matches!(b["state"].as_str(),Some("active"|"pending"|"suspended")))) else {continue;};
                let Some(binding_id) = binding["id"].as_str() else {continue;};
                let here = agent["executionDeviceId"].as_str().is_some_and(|device| devices["currentDeviceId"].as_str() == Some(device));
                let runtime = if here {
                    self.execute(Command::Local {agent_id:id.into(),action:LocalAction::AgentServiceStatus,input:None,binding_id:None,requester:None}).await
                } else {Ok(Value::Null)};
                services.push(serde_json::json!({"agentId":id,"bindingId":binding_id,"bindingState":binding["state"],"ownerServicePaused":binding["ownerServicePaused"],"service":runtime.as_ref().ok().and_then(|v|v.get("service")),"displayName":agent["displayName"],"thisDevice":here,"runtime":runtime.as_ref().ok().and_then(|v|v.get("runtime")),"statusAvailable":runtime.is_ok()}));
            }
            Ok(serde_json::json!({"roomId":room_id,"services":services}))
        }).await.map_err(|_|"room_discovery_unavailable")?;
        self.check()?;
        result
    }
    pub async fn execute(&self, command: Command) -> Result<Value, String> {
        self.check()?;
        if matches!(command, Command::BeginLogin) {
            return Err("matrix_oauth_sign_in_required".into());
        }
        // Logout may always stop this pinned owner even when Matrix is offline.
        if !matches!(command, Command::Logout) {
            self.verify_matrix(false).await?;
        }
        let result = if let Some(remote) = self.remote_client().await? {
            if matches!(command, Command::Logout) { remote.sign_out().await.map(|_| Value::Null) }
            else { remote.execute(command).await }
        } else {
            self.native_owner().await?.execute(command).await
        }.map_err(|e| e.code);
        if let Err(e) = self.check() {
            if self.suspend().await.is_err() {
                return Err("native_owner_drain_required".into());
            }
            return Err(e);
        }
        result
    }
    /// A picker retry recovers the exact saved intent, including after reopening
    /// the panel. Pending joins are checked for a bounded period; mutation
    /// futures are never cancelled by the UI's waiting deadline.
    pub async fn bind_agent_to_room(&self, mut command: Command) -> Result<Value, String> {
        let original = command.clone();
        let mut result = self.execute(command.clone()).await;
        if result.as_ref().err().is_some_and(|e| e == "agent_command_recovery_required") {
            let saved = self.execute(Command::AgentCommands).await?;
            command = matching_binding_recovery(&original, &saved)
                .ok_or("agent_command_recovery_required")?;
            result = self.execute(command.clone()).await;
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        while result.as_ref().is_ok_and(|value| value["commandState"] == "pending")
            && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            self.check()?;
            result = self.execute(command.clone()).await;
        }
        result
    }
    /// Human-invitation entry points use the same durable Agent binding as the
    /// dedicated picker. A Matrix puppet invite alone cannot authorize service.
    pub(crate) async fn invite_agent_to_room(&self, room: &str, user: &str) -> Result<bool, String> {
        let context = self.agents_for_registered_room(room).await?;
        let command = invited_agent_command(&context, room, user)?;
        let Command::BindAgent {agent_id, ..} = &command else {unreachable!()};
        let agent = agent_id.clone();
        let value = self.bind_agent_to_room(command).await?;
        self.check()?;
        invited_agent_joined(&value, &agent, room)
    }
    async fn native_owner(&self) -> Result<NativeOwner, String> {
        // On macOS only the owner daemon executes (ADR-192). An account the
        // daemon does not host (another account is hosted, or hosting is not
        // set up) still opens its own root here for management requests such
        // as permission and Agent requests, but restores no runtime and starts
        // no pool worker. Once the daemon hosts it, `remote_client` retires
        // this owner before attaching.
        let (native, management_only) = {
            let mut native = self.0.native.lock().await;
            // Recheck after waiting: a completed daemon handoff must not be
            // followed by a caller reopening the old in-process owner.
            let management_only = self.local_management_only()?;
            self.check()?;
            self.drain_setup_owner(&mut native).await?;
            if native.is_none() {
                self.cleanup_unpublished_native_owner().await?;
                self.check()?;
                let selected = self.load_current_initialization().await?;
                let opened = if let Some(selection) = &selected {
                    selection.open_with_matrix_guarded(
                        self.root()?, &self.0.origin, &self.0.owner,
                        Arc::new(SdkTokens(Arc::downgrade(&self.0))),
                        |owner| self.register_native_owner(owner).map_err(|code| source_error(&code)),
                    ).await
                } else {
                    self.open_current_owner().await
                };
                let owner = match opened {
                    Ok(owner) => owner,
                    Err(error) => {
                        self.cleanup_unpublished_native_owner().await?;
                        let code = recovery_error_code(&error.code);
                        makepad_widgets::warning!("Native owner initialization failed: {code}");
                        return Err(error.code);
                    }
                };
                if let Err(error) = self.check() {
                    self.close_native_owner(&owner).await?;
                    return Err(error);
                }
                // Saved explicit consent is not authority: restoration invokes
                // every fresh device/binding/provider/history startup gate.
                // A failure leaves intent for review, never a blind takeover.
                let restore_personal = selected.is_none();
                if restore_personal && !management_only {
                    log_runtime_recovery(owner.restore_runtime_intents().await);
                }
                if let Err(error) = self.check() {
                    self.close_native_owner(&owner).await?;
                    return Err(error);
                }
                #[cfg(feature = "onboarding-test-support")]
                if selected.is_none() && std::env::args_os().any(|arg|arg=="--hagency-seed-empty-personal-fixture") {
                    if let Err(error)=owner.seed_empty_personal_activation_fixture().await {
                        self.close_native_owner(&owner).await?;return Err(error.code);
                    }
                }
                *native = Some(owner);
            }
            (native.as_ref().unwrap().clone(), management_only)
        };
        if !management_only {
            self.start_allocation_worker(&native).await;
            #[cfg(feature = "shared-execution")]
            self.start_shared_worker(&native).await;
        }
        Ok(native)
    }
    async fn open_current_owner(&self) -> Result<NativeOwner, NativeError> {
        let root = self.root().map_err(|code| NativeError { status: 409, code })?;
        NativeOwner::open_with_matrix_guarded(
            root, &self.0.origin, &self.0.owner,
            Arc::new(SdkTokens(Arc::downgrade(&self.0))),
            |owner| self.register_native_owner(owner).map_err(|code| source_error(&code)),
        ).await
    }
    /// The admission requests that wait for this account's decision, for the app-wide reminder:
    /// the server's page of them, or null when this account is not its server's administrator.
    pub async fn admission_requests(&self) -> Result<Value, String> {
        use ::hagency::native_owner::{ManagementCommand, ManagementPackage, ManagementPermission};
        let manager = self.management(ManagementPackage {
            digest: blake3::hash(b"hagency-native-admission-reminder-v1").to_hex().to_string(),
            instance: 1,
            account_epoch: 1,
            permissions: vec![ManagementPermission::ReadConnections, ManagementPermission::ReadInbox],
        }).await?;
        let result = async {
            let session = manager.central(ManagementCommand::Session {}).await.map_err(|e| e.code)?;
            if session["canAdmitResources"] != true {
                return Ok(Value::Null);
            }
            manager.central(ManagementCommand::Inbox {
                after: None,
                needs_my_action: true,
                include_projects: false,
                include_allocations: false,
            }).await.map_err(|e| e.code)
        }.await;
        // This handle belongs to this one read.
        manager.retire();
        self.check()?;
        result
    }
    pub async fn shared_permissions(&self, command: ::hagency::native_owner::ManagementCommand) -> Result<Value,String> {
        use ::hagency::native_owner::{ManagementCommand,ManagementPackage,ManagementPermission};
        if !matches!(&command,ManagementCommand::SharedApprovals{..}|ManagementCommand::SharedApprovalDecide{..}) {return Err("invalid_permission_command".into());}
        let manager=self.management(ManagementPackage{digest:blake3::hash(b"hagency-native-chat-permissions-v1").to_hex().to_string(),instance:1,account_epoch:1,permissions:vec![ManagementPermission::ReadAgentWork,ManagementPermission::DecideAgentRequests]}).await?;
        // This polling handle belongs to this request, unlike the retained
        // mini-app manager passed directly to shared_permissions_with.
        let result=self.shared_permissions_with(manager.clone(),command).await;
        manager.retire();
        result
    }
    pub(crate) async fn shared_permissions_with(&self,manager:Arc<::hagency::daemon::bridge::ManagementClient>,command: ::hagency::native_owner::ManagementCommand)->Result<Value,String> {
        use ::hagency::native_owner::ManagementCommand;
        self.check()?;
        if !matches!(&command,ManagementCommand::SharedApprovals{..}|ManagementCommand::SharedApprovalDecide{..}) {return Err("invalid_permission_command".into());}
        if let ManagementCommand::SharedApprovalDecide{id,digest,decision}=&command {
            let row=match manager.central(ManagementCommand::SharedApprovalGet{id:id.clone()}).await {
                Ok(row)=>row,
                // Older servers have only plaintext approvals.
                Err(e) if e.status==404=>{let result=manager.central(command).await.map_err(|e|e.code)?;self.check()?;return Ok(result);},
                Err(e)=>return Err(e.code),
            };
            self.check()?;
            if row["approval"]["id"]!=*id || row["digest"]!=*digest {return Err("permission_request_changed".into());}
            if row["approval"]["request"].get("encrypted").is_some() {
                if row["canDecide"]!=true || self.0.encrypted_approvals.as_ref().ok_or("encrypted_approval_unavailable")?.details(&row).map_err(str::to_owned)?.is_none(){return Err("encrypted_approval_details_pending".into());}
                let client=crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
                // Keep the SDK crypto future out of every mini-app host request's
                // inline state, including requests that never send a decision.
                tokio::time::timeout(std::time::Duration::from_secs(15),Box::pin(::hagency::encrypted_approvals::send_decision(&client,&row,decision))).await.map_err(|_|"encrypted_approval_delivery_pending")?.map_err(str::to_owned)?;
                self.check()?;
            }
        }
        let mut result=manager.central(command).await.map_err(|e|e.code)?;self.check()?;
        if let Some(rows)=result.get_mut("approvals").and_then(Value::as_array_mut) {
            for row in rows {
                if row["approval"]["request"].get("encrypted").is_none(){continue;}
                let details=self.0.encrypted_approvals.as_ref().ok_or("encrypted_approval_unavailable")?.details(row).map_err(str::to_owned)?;
                row["encryptedPending"]=Value::Bool(details.is_none());
                if let Some(details)=details {row["approval"]["request"]=details;}else{row["canDecide"]=Value::Bool(false);}
            }
        }
        Ok(result)
    }
    pub async fn management(&self, package: ::hagency::native_owner::ManagementPackage) -> Result<Arc<::hagency::daemon::bridge::ManagementClient>, String> {
        self.management_scoped(package,None).await
    }
    pub async fn management_until(&self,package: ::hagency::native_owner::ManagementPackage,until:std::time::Instant)->Result<Arc<::hagency::daemon::bridge::ManagementClient>,String>{self.management_scoped(package,Some(until)).await}
    async fn management_scoped(&self,mut package: ::hagency::native_owner::ManagementPackage,until:Option<std::time::Instant>)->Result<Arc<::hagency::daemon::bridge::ManagementClient>,String>{
        self.check()?;
        self.verify_matrix(false).await?;
        package.account_epoch = self.0.epoch.checked_add(1).ok_or("matrix_account_changed")?;
        let management = if let Some(remote) = self.remote_client().await? {
            match until {Some(until)=>remote.management_until(package,until).await,None=>remote.management(package).await}.map_err(|e|e.code)?
        } else {
            let native=self.native_owner().await?.management(package).await.map_err(|e|e.code)?;
            if let Some(until)=until {native.restrict_to_deadline(until).map_err(|e|e.code)?;}
            ::hagency::daemon::bridge::ManagementClient::local(native)
        };
        if let Err(error) = self.check() {
            management.retire();
            return Err(error);
        }
        Ok(management)
    }
    // Caller holds the async native initialization lock with its slot empty.
    async fn cleanup_unpublished_native_owner(&self) -> Result<(), String> {
        let registered = self.0.native_retirement.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        if let Some(owner) = registered {
            // Keep the registered handle until cleanup completes: cancellation
            // during shutdown must leave it available to the next retry.
            self.close_native_owner(&owner).await?;
            self.0.native_retirement.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        }
        Ok(())
    }
    fn register_native_owner(&self, owner: &NativeOwner) -> Result<(), String> {
        // Invoked before Matrix authorization starts background workers. A
        // late initializer becomes visible to retire(), or fences itself.
        let mut handle = self.0.native_retirement.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.0.retired.load(std::sync::atomic::Ordering::Acquire) {
            owner.retire();
            if self.0.logout_requested.load(std::sync::atomic::Ordering::Acquire) {
                owner.retire_and_revoke_resumption().map_err(|e| e.code)?;
            }
            return Err("matrix_account_changed".into());
        }
        *handle = Some(owner.clone());
        Ok(())
    }
    fn retire(&self) {
        if let Some(remote) = self.0.remote.lock().unwrap_or_else(|p| p.into_inner()).as_ref() { remote.retire(); }
        self.0
            .retired
            .store(true, std::sync::atomic::Ordering::Release);
        self.0.allocations.retire();
        #[cfg(feature = "shared-execution")]
        self.0.shared.retire();
        let handle = self.0.native_retirement.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(native) = handle.as_ref() {
            native.retire();
        }
    }
    /// Revoke the current native owner before any account-transition await.
    /// Already queued SQLite work observes the same irreversible native fence.
    pub fn retire_current() {
        let service = CURRENT.lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        if let Some(service) = service {
            service.retire();
        }
    }
}

#[cfg(test)]
pub(crate) mod retirement_tests;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_recovery_diagnostic_never_logs_provider_or_source_text() {
        assert_eq!(recovery_error_code("runtime_profile_unavailable"),"runtime_profile_unavailable");
        assert_eq!(recovery_error_code("owner_authorization_required"),"owner_authorization_required");
        assert_eq!(recovery_error_code("/Users/example/.codex/auth.json bearer secret"),"runtime_recovery_unrecognized_error");
        assert_eq!(recovery_error_code("provider_error: access_token=secret"),"runtime_recovery_unrecognized_error");
    }
    #[test]
    fn waiting_deadline_accepts_reads_but_never_detaches_actions_or_recovery() {
        for command in [Command::AgentDetails {agent_id:"a".into()}, Command::Agents, Command::Projects, Command::AgentCommands,
            Command::Creations, Command::Creation {id: "original".into()},
            Command::Provider {action: ProviderAction::Models},
            Command::Binding {agent_id:"a".into(),binding_id:"b".into(),action:BindingAction::Status},
            Command::Local {agent_id:"a".into(),action:LocalAction::Runtime,input:None,binding_id:None,requester:None}] {
            assert!(management_read(&command), "{command:?}");
        }
        for command in [Command::AssignAgentToCurrentDevice {agent_id:"a".into(),expected_generation:4}, Command::CreateAgent {input:serde_json::json!({})},
            Command::BindAgent {agent_id:"a".into(),input:serde_json::json!({})},
            Command::CreateMatrix {input:serde_json::json!({})},
            Command::ResumeCreation {id:"original".into(),room_id:None},
            Command::ResumeAgentCommand {id:"original".into()},
            Command::EnsureAgentOwnerDirect {agent_id:"a".into()},
            Command::PrepareAgentWorkspace {agent_id:"a".into()},
            Command::Provider {action:ProviderAction::UseLocal}, Command::Logout,
            Command::Local {agent_id:"a".into(),action:LocalAction::RuntimeStart,input:None,binding_id:None,requester:None},
            Command::Local {agent_id:"a".into(),action:LocalAction::RuntimeStop,input:None,binding_id:None,requester:None},
            Command::Local {agent_id:"a".into(),action:LocalAction::SaveAgentPolicy,input:None,binding_id:None,requester:None},
            Command::Local {agent_id:"a".into(),action:LocalAction::Decision {approval_id:"p".into()},input:None,binding_id:None,requester:None},
            Command::Local {agent_id:"a".into(),action:LocalAction::Runtime,input:Some(serde_json::json!({})),binding_id:None,requester:None}] {
            assert!(!management_read(&command), "{command:?}");
        }
    }
}

#[cfg(test)]
mod binding_recovery_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn invite_routes_only_hagency_puppets_and_requires_exact_owned_roster_entry() {
        let puppet: matrix_sdk::ruma::OwnedUserId="@_hagency_agt_a:s".try_into().unwrap();
        assert!(is_agent_invitation(&puppet));
        assert!(!is_agent_invitation(matrix_sdk::ruma::user_id!("@friend:s")));
        let context=json!({"roomId":"!r:s","projectId":"p","agents":[{"id":"a","puppetMxid":puppet,"state":"active"}]});
        let command=invited_agent_command(&context,"!r:s",puppet.as_str()).unwrap();
        assert!(matches!(command,Command::BindAgent {agent_id,input} if agent_id=="a" && input["projectId"]=="p" && input["roomId"]=="!r:s" && input["idempotencyKey"].as_str().is_some()));
        assert_eq!(invited_agent_command(&context,"!other:s",puppet.as_str()).unwrap_err(),"invalid_server_response");
        assert_eq!(invited_agent_command(&context,"!r:s","@_hagency_agt_a:foreign").unwrap_err(),"agent_owner_binding_required");
        assert_eq!(invited_agent_command(&json!({"roomId":"!r:s","agents":[]}),"!r:s",puppet.as_str()).unwrap_err(),"invalid_server_response");
        let mut retired=context;retired["agents"][0]["state"]=json!("retired");
        assert_eq!(invited_agent_command(&retired,"!r:s",puppet.as_str()).unwrap_err(),"agent_retired");
    }
    #[test]
    fn invitation_success_requires_confirmed_binding_for_exact_agent_and_room() {
        let mut result=json!({"commandState":"pending","creation":{"binding":{"roomId":"!r:s","agentId":"a","state":"pending"}}});
        assert_eq!(invited_agent_joined(&result,"a","!r:s"),Ok(false));
        result["commandState"]=json!("active");
        assert!(invited_agent_joined(&result,"a","!r:s").is_err());
        result["creation"]["binding"]["state"]=json!("active");
        assert_eq!(invited_agent_joined(&result,"a","!r:s"),Ok(true));
        assert!(invited_agent_joined(&result,"other","!r:s").is_err());
        assert!(invited_agent_joined(&result,"a","!other:s").is_err());
        assert!(invited_agent_joined(&json!({"commandState":"active"}),"a","!r:s").is_err());
    }
    #[test]
    fn binding_retry_recovers_only_the_exact_agent_room_and_project() {
        let original = Command::BindAgent {agent_id:"a".into(),input:json!({"projectId":"p","roomId":"!r:s","idempotencyKey":"new"})};
        let record = json!({"operation":"agent.bind","agentId":"a","id":"original-journal","state":"unknown","input":{"projectId":"p","roomId":"!r:s","idempotencyKey":"original"}});
        let saved = json!({"commands":[record.clone()]});
        assert!(matches!(matching_binding_recovery(&original,&saved),Some(Command::ResumeAgentCommand{id}) if id=="original-journal"));
        for field in ["projectId","roomId"] {
            let mut foreign=record.clone();foreign["input"][field]=json!("different");
            assert!(matching_binding_recovery(&original,&json!({"commands":[foreign]})).is_none());
        }
        let mut foreign=record.clone();foreign["agentId"]=json!("other");
        assert!(matching_binding_recovery(&original,&json!({"commands":[foreign]})).is_none());
        assert!(matching_binding_recovery(&original,&json!({"commands":[record.clone(),record]})).is_none());
    }
}
