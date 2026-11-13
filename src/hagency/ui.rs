//! Native owner management. Server identities and local execution policy are
//! separate controls; no web console, transfer or administrator resource gate.
use makepad_widgets::*;
use serde_json::{Value, json};
use super::{backend::Service, model::*, approvals::Notices};
use std::{collections::HashSet, time::Instant};
use crate::sliding_sync::spawn_async_task;

/// `shared_workspace` is the project directory shown to the owner when the
/// shared-workspace opt-in is selected; the service refuses the start when the
/// saved directory is no longer that one.
fn runtime_start_input(takeover: bool, effort: String, shared_workspace: Option<String>) -> Value {
    serde_json::to_value(::hagency::native_owner::RuntimeStartRequest {
        takeover, effort: Some(effort),
        shared_workspace: shared_workspace.is_some(),
        expected_workspace: shared_workspace,
        ..Default::default()
    }).expect("runtime start request contains only JSON values")
}

#[derive(Clone, Debug)]
pub enum HagencyAction {
    OpenProjects,
    NewProject,
    ProjectDetails {
        project_id: String,
    },
    ProjectSection {
        project_id: String,
        section: ProjectDetailSection,
    },
    OpenAgents,
    /// Opens one Agent's local settings (private chat, encryption, Rooms).
    OpenAgent {agent_id:String},
    AgentRun {agent_id:String,binding_id:String},
    NewAgent {
        project_id: Option<String>,
    },
    Close,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectDetailSection {
    Overview,
    Rooms,
    Members,
}
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Page {
    #[default]
    Projects,
    Agents,
}
#[derive(Clone, Copy, Debug)]
enum Target {
    Journal,
    Projects,
    Rooms,
    Spaces,
    Agents,
    Bindings,
    BindingChanged,
    ReplyPolicyChanged,
    RoomRoster,
    ScopeState,
    AgentJournal,
    AgentResumed,
    AgentDetails,
    AgentAssigned,
    Devices,
    OwnerDirect,
    OwnerDirectCreated,
    ProviderModels,
    ConfigurationStatus,
    ExecutionEnvironment,
    WorkspacePrepared,
    ScopeChanged,
    Policy,
    Creation,
    Provider,
    Runtime,
    ListService,
    ServiceRoomChanged,
    ApprovalDecision,
    Members,
    Changed,
    ProjectAdopted,
    RoomAdopted,
    AgentCreated,
    AgentBound,
}
#[derive(Debug)]
struct Completed {
    identity: String,
    request: u64,
    target: Target,
    configuration_fence: Option<(String, i64, String)>,
    configuration_digest: Option<String>,
    result: Result<Value, String>,
}
#[derive(Debug)]
struct ConfigurationCompleted {identity:String,request:u64,agent:String,generation:i64,device:String,result:Result<Value,String>}
/// Opens a local Agent's private chat in the main window.
#[derive(Clone, Debug)]
pub struct OpenAgentChat {pub room_id: matrix_sdk::ruma::OwnedRoomId}
pub(crate) fn open_agent_chat(cx: &mut Cx, room_id: matrix_sdk::ruma::OwnedRoomId) {
    navigate_agent_chat(cx, room_id, true, None);
}
/// Outcome of turning on end-to-end encryption in an owner-direct private chat.
#[derive(Clone, Debug)]
pub struct OwnerDirectEncryption {pub room_id:String,pub enabled:bool}
#[derive(Debug)]
struct ApprovalsCompleted { identity:String, request:u64, agent:String, binding:String, room:String, result:Result<Value,String> }

type ServiceRowTargets = std::collections::HashMap<WidgetUid,(String,String)>;
type AgentCardTargets = std::collections::HashMap<WidgetUid,String>;
type ConfigurationKeys = std::collections::HashMap<String, String>;
type RuntimeStates = std::collections::HashMap<String,Result<Value,String>>;
#[derive(Debug)]
struct RuntimeListCompleted { identity:String, request:u64, states:Vec<(String,Result<Value,String>)> }
pub(crate) fn runtime_list_label(runtime:Option<&Result<Value,String>>,agent:&str)->&'static str {
    let Some(runtime)=runtime else {return "Checking local processing…";};
    let Ok(value)=runtime else {return "Local processing could not be verified. Refresh to retry.";};
    if value["service"]["enabled"] == false && value["runtime"].is_null() {return "Not running on this device. Open Service to configure and start.";}
    if value["service"]["enabled"] == true && (value["service"]["watcherRunning"] == false || !string(&value["service"],"lastError").is_empty()) {return "Service needs attention. Refresh status or stop and start again.";}
    if !array(&value["service"],"problems").is_empty() {return "Some Rooms could not start. The service will retry automatically.";}
    if value["service"]["enabled"] == true && value["runtime"].is_null() {return if value["service"]["phase"] == "starting" {"Starting service. Discovering connected Rooms…"} else {"Service enabled. Waiting for an available Room."};}
    let r=&value["runtime"];
    if r.is_null() {return "Not running on this device. Open Service to configure and start.";}
    if string(r,"agentId")!=agent {return "Local processing could not be verified. Refresh to retry.";}
    let bindings=array(r,"bindings");
    let phases=if bindings.is_empty() {vec![string(r,"phase")]} else {bindings.iter().map(|b|string(b,"phase")).collect::<Vec<_>>()};
    if string(r,"phase")=="waiting_for_lease" || phases.contains(&"waiting_for_lease") {"Waiting for the previous runtime lease to be released…"}
    else if phases.iter().any(|p|matches!(*p,"executing"|"executing_chat_only"|"executing_host_files")) {"Processing requests on this device."}
    else if phases.iter().any(|p|matches!(*p,"online"|"online_chat_only")) {if value["service"]["enabled"] == false {"Running in previously selected Rooms. Stop and start to enable automatic Room discovery."} else {"Running in all connected Rooms on this device. New Rooms are included automatically; configured budgets and policy permissions still apply."}}
    else if phases.iter().any(|p|matches!(*p,"awaiting_approval"|"waiting_owner")) {"Waiting for your approval before continuing."}
    else if phases.iter().any(|p|matches!(*p,"ledger_recovery_required"|"reconciliation_only")) {"Restore this owner's complete ledger before new requests. Only known original replies may be recovered."}
    else if !string(r,"lastError").is_empty() || bindings.iter().any(|b|!string(b,"lastError").is_empty()) {"The runtime stopped with an error. Check the details below."}
    else if phases.contains(&"checking_provider") {"Checking this device's local runtime connection…"}
    else if phases.contains(&"starting") {"Starting service. Discovering connected Rooms…"}
    else if phases.iter().all(|p|matches!(*p,"stopped"|"")) {"Not running on this device. Open Service to configure and start."}
    else {"The runtime has not confirmed it is ready. Refresh its status."}
}
fn runtime_diagnostic(code: &str) -> String {
    // Only engine-defined codes are rendered; arbitrary backend text is discarded.
    let message = match code {
        "agent_lease_busy" => "The previous runtime lease is still active. Wait briefly and retry.",
        "authorization_or_lease_lost" => "Runtime authorization or its lease was lost. Stop the service, check your account and device assignment, then start again.",
        "device_transport_unavailable" => "The runtime could not reach the server. Check the connection; the service will retry automatically.",
        "agent_process_closing" => "The previous runtime is still stopping. The service will retry automatically.",
        "agent_execution_device_required" => "Assign this Agent to this device before starting it here.",
        "ledger_recovery_required" => "Restore this owner's complete ledger before new requests. Only known original replies may be recovered.",
        "room_worker_stopped" | "runtime_start_failed" => "A Room worker stopped before becoming ready. The service will retry automatically.",
        // The engine reports a failed run of any local tool (Codex, Claude Code, Octos) under one code family.
        // An Agent's own run must not be told to check Codex when it runs another tool.
        "codex_provider_timeout" | "codex_provider_unavailable" => "The Agent's coding tool did not respond. Check that it can run on this computer, then retry.",
        "codex_protocol_mismatch" => "The Agent's coding tool returned a response Hagency could not use. Check its installed version and retry.",
        _ => return visible_error(code),
    };
    format!("{} ({code})", crate::i18n::tr(message))
}
pub(crate) fn runtime_service_description(runtime: Option<&Result<Value,String>>, agent: &str) -> String {
    let mut text=crate::i18n::tr(runtime_list_label(runtime,agent)).to_owned();
    let Some(runtime)=runtime else {return text;};
    let value=match runtime {Ok(value)=>value,Err(code)=>{text.push_str(&format!("\n{}",runtime_diagnostic(code)));return text;}};
    let mut diagnostics=Vec::new();
    let mut add=|code:&str| {if !code.is_empty() {let message=runtime_diagnostic(code);if !diagnostics.contains(&message) {diagnostics.push(message);}}};
    add(string(&value["service"],"lastError"));
    // A mismatched runtime must never supply another Agent's diagnostics.
    if string(&value["runtime"],"agentId")==agent {
        add(string(&value["runtime"],"lastError"));
        for binding in array(&value["runtime"],"bindings") {add(string(&binding,"lastError"));}
    }
    for problem in array(&value["service"],"problems") {
        let mut message=runtime_diagnostic(string(&problem,"error"));
        if let Some(seconds)=problem["retryInSeconds"].as_u64() {message.push_str(&format!(" · {} {seconds}s",crate::i18n::tr("Retry in")));}
        if !diagnostics.contains(&message) {diagnostics.push(message);}
    }
    for message in diagnostics {text.push_str(&format!("\n{message}"));}
    text
}
fn service_enabled(value:&Value)->bool {
    value["service"]["enabled"] == true || {
        let runtime=&value["runtime"];
        matches!(string(runtime,"phase"),"online"|"online_chat_only"|"executing"|"executing_chat_only"|"executing_host_files"|"awaiting_approval"|"waiting_owner"|"waiting_for_lease")
    }
}
const MANAGEMENT_READ_WAIT: std::time::Duration = std::time::Duration::from_secs(45);
struct ReadWait {
    identity: String,
    request: u64,
    deadline: std::time::Instant,
}
fn nonce() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
fn string<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn current_completion(active: bool, identity: &str, request: u64, done: &Completed) -> bool {
    active && done.identity == identity && done.request == request
}
// This whitelist is deliberately narrower than local management. Credentials and
// Room/requester policy remain managed only on the execution device.
fn remote_configuration_command(command: &Command) -> bool {
    matches!(command,
        Command::Local {action: LocalAction::AgentModel | LocalAction::AgentPolicy
            | LocalAction::SaveAgentModel | LocalAction::SaveAgentPolicy,
            binding_id: None, requester: None, ..}
        | Command::PrepareAgentWorkspace {..}
        | Command::Provider {action: ProviderAction::Status | ProviderAction::Models | ProviderAction::UseLocal}
        | Command::RuntimeProvider {action: ProviderAction::Status | ProviderAction::Models | ProviderAction::UseLocal, ..})
}
fn configuration_outcome_unconfirmed(error: &str) -> bool {
    matches!(error,"invalid_configuration_response"|"configuration_outcome_unknown"|"configuration_request_unknown"|"configuration_request_timeout"|"server_unavailable"|"device_transport_unavailable"|"matrix_authorization_unavailable")
}
fn configuration_connection_lost(error: &str) -> bool {
    matches!(error,"configuration_device_offline"|"configuration_device_unavailable"|"execution_device_offline"|"configuration_target_changed"|"server_unavailable"|"device_transport_unavailable"|"matrix_authorization_unavailable"|"device_authorization_required")
}
fn configuration_matches(configuration: &Value, agent: &Value) -> bool {
    configuration["agentId"] == agent["id"] && configuration["agentGeneration"] == agent["generation"]
        && configuration["executionDeviceId"] == agent["executionDeviceId"]
        && configuration["deviceGeneration"].as_i64().is_some_and(|generation| generation > 0)
}
fn policy_matches(policy: &Value, agent: &str, binding: &str, requester: &str) -> bool {
    string(policy, "agentId") == agent
        && string(policy, "bindingId") == binding
        && string(policy, "requester") == requester
}
fn editor_matches(
    policy: &Value,
    agent: &str,
    binding: Option<&str>,
    requester: &str,
    layer: usize,
) -> bool {
    if layer == 0 {
        string(policy, "agentId") == agent && policy["scope"] == "agent"
    } else {
        binding.is_some_and(|binding| policy_matches(policy, agent, binding, requester))
    }
}
fn members_match(target: &Option<(String, bool)>, value: &Value) -> bool {
    target.as_ref().is_some_and(|(room, space)| {
        string(value, "roomId") == room && value["space"].as_bool() == Some(*space)
    })
}
fn agent_command_label(command:&Value)->String {
    let state=if string(command,"state") == "unknown" {"result unconfirmed"} else {"still being confirmed"};
    if string(command,"operation") == "agent.create" {format!("Create {} · {state}",string(&command["input"],"displayName"))}
    else {format!("Connect to {} · {state}",string(&command["input"],"roomId"))}
}
fn agent_command_details(command:&Value)->String {
    let heading=agent_command_label(command);
    if string(command,"operation") == "agent.create" {heading}
    else {format!("{heading}\nAgent: {}\nProject: {}",string(command,"agentId"),string(&command["input"],"projectId"))}
}
pub(crate) fn runtime_summary(runtime:&Value,agent:&str,binding:Option<&str>)->String {
    if runtime.is_null() {return crate::i18n::tr("Not running on this device.").into();}
    if string(runtime,"agentId") != agent {return crate::i18n::tr("Refresh this Agent's runtime status.").into();}
    let selected=binding.and_then(|id|array(runtime,"bindings").into_iter().find(|b|string(b,"bindingId")==id));
    let value=selected.as_ref().unwrap_or(runtime);
    if binding.is_some() && selected.is_none() && runtime["bindingId"].as_str() != binding && string(runtime,"lastError") != "ledger_recovery_required" && string(runtime,"phase") != "waiting_for_lease" {
        return crate::i18n::tr("This Room is not running. Other selected Rooms may still be active.").into();
    }
    let text=match string(value,"phase") {
        "online"|"online_chat_only"=>"Responding in this Room. Configured limits and request permissions still apply.",
        "waiting_for_lease"=>"Waiting for the previous runtime lease to be released…",
        "checking_provider"=>"Checking this device's local runtime connection…",
        "executing"|"executing_chat_only"|"executing_host_files"=>"Processing a request in this Room…",
        "awaiting_approval"|"waiting_owner"=>"Waiting for your approval before continuing.",
        "ledger_recovery_required"|"reconciliation_only"=>"Restore this owner's complete ledger before new requests. Only known original replies may be recovered.",
        "stopped"=>"Stopped on this device.",
        _=>"The runtime has not confirmed it is ready. Refresh its status.",
    };
    let failure=string(value,"lastError");
    let text = crate::i18n::tr(text);
    if failure.is_empty() {text.into()} else if failure=="ledger_recovery_required" {crate::i18n::tr("Restore this owner's complete ledger before new requests. Stop, restore, then start again.").into()} else {format!("{text} {}",user_error(failure))}
}
// Only these static native error codes are safe to expose as diagnostics.
// Never render a provider's raw JSON, stderr, filesystem paths, or credentials.
fn provider_error_message(error: &str) -> Option<&'static str> {
    Some(match error {
        "owner_daemon_update_required" => "The resource owner's Hagency service needs an update. Update Hagency and start its service; the original message can then continue.",
        "owner_daemon_unavailable" => "The resource owner's Hagency service is unavailable. The original message is waiting for that service to reconnect.",
        "claude_binary_unavailable" => "Claude Code is not installed or could not be started. Check the local installation.",
        "shared_claude_version_unsupported" | "claude_version_unsupported" => "Update Claude Code to version 2.1.322 or newer, then retry.",
        "octos_binary_unavailable" => "Octos is not installed or could not be started. Check the local installation.",
        "claude_account_unavailable" | "local_provider_not_signed_in" => "Check your local Claude sign-in, then connect the account again.",
        "local_provider_home_unavailable" => "The local runtime's account directory is unavailable. Check its local setup.",
        "octos_profiles_unavailable" => "No usable Octos profiles were found. Configure a local Octos profile, then refresh.",
        "provider_authorization_lost" => "The local runtime's account or configuration is unavailable. Check Model & resources and reconnect.",
        "local_provider_login_managed_locally" => "Sign in through the local runtime, then connect it here.",
        "codex_binary_unavailable" => "Codex is not installed or could not be started. Check the local Codex installation.",
        "codex_version_unsupported" => "Update Codex to version 0.167.0 or newer, then retry.",
        "local_codex_home_unavailable" | "provider_directory_unavailable" | "provider_directory_not_canonical" => "Codex's local account or workspace directory is unavailable. Check its local permissions and retry.",
        "local_codex_configuration_unsupported" | "provider_file_credentials_or_config_rejected" => "This Codex configuration cannot be safely used by Hagency. Your existing account has not been changed. Check the configuration or use a separate Codex account.",
        "provider_profile_mismatch" | "provider_choice_unavailable" => "The saved local account selection is unavailable or does not match this profile. Reconnect the intended account and retry.",
        "local_codex_account_not_signed_in" => "Codex is not signed in. Sign in through your local Codex, then check the account again.",
        "provider_keyring_unavailable" => "Codex cannot access the system credential store. Unlock it or check its permissions, then retry.",
        "codex_model_catalog_unavailable" => "Codex could not load its model list. Check the account and connection, then refresh the models.",
        "codex_provider_timeout" | "codex_provider_unavailable" => "Codex did not respond. Check that it can run locally, then retry.",
        "codex_protocol_mismatch" => "This Codex version returned an unsupported response. Check the installed Codex version and retry.",
        "codex_provider_request_rejected" => "Codex rejected this request. Check the local account and configuration before retrying.",
        "provider_login_callback_unavailable" | "provider_authorization_url_rejected" | "provider_login_method_rejected" => "The separate Codex sign-in could not be started safely. Retry or connect your signed-in local Codex account.",
        "shared_codex_login_is_managed_locally" | "shared_codex_logout_is_managed_locally" => "This account is managed by your local Codex. Use Disconnect here to keep your everyday Codex signed in.",
        "profile_or_local_state_unavailable" => "The saved model or workspace is unavailable. Check this Agent's local resources and save them before starting.",
        _ => return None,
    })
}
fn visible_error(error: &str) -> String {
    if let Some(message) = provider_error_message(error) {
        format!("{} ({error})", crate::i18n::tr(message))
    } else if let Some(message) = crate::host::hagency::errors::message(error) {
        message.to_owned()
    } else {
        crate::i18n::tr(user_error(error)).to_owned()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentDeviceState { Here, Other, Unassigned, Unavailable, Checking }
fn agent_device_state(agent: &Value, current: &str, devices: &[Value], loaded: bool) -> AgentDeviceState {
    if !loaded || current.is_empty() || agent["generation"].as_i64().is_none_or(|generation|generation <= 0) {return AgentDeviceState::Checking;}
    let Some(device) = agent["executionDeviceId"].as_str().filter(|id|!id.is_empty()) else {return AgentDeviceState::Unassigned;};
    let Some(record) = devices.iter().find(|d|string(d,"id") == device) else {return AgentDeviceState::Unavailable;};
    if record["revoked"] != false {return AgentDeviceState::Unavailable;}
    if device == current {AgentDeviceState::Here} else {AgentDeviceState::Other}
}
fn render_device_badge(cx: &mut Cx, badge: &WidgetRef, state: AgentDeviceState, text: &str) {
    let mut badge = badge.clone();
    badge.label(cx, ids!(device_label)).set_text(cx, text);
    match state {
        AgentDeviceState::Here => {
            script_apply_eval!(cx, badge, {draw_bg +: {color: mod.widgets.RINX_SUCCESS_BG}});
            let mut label = badge.label(cx, ids!(device_label));
            script_apply_eval!(cx, label, {draw_text +: {color: mod.widgets.RINX_SUCCESS_FG}});
        }
        AgentDeviceState::Other => {
            script_apply_eval!(cx, badge, {draw_bg +: {color: mod.widgets.RINX_SELECTED}});
            let mut label = badge.label(cx, ids!(device_label));
            script_apply_eval!(cx, label, {draw_text +: {color: mod.widgets.RINX_ACCENT}});
        }
        AgentDeviceState::Unassigned | AgentDeviceState::Unavailable => {
            script_apply_eval!(cx, badge, {draw_bg +: {color: mod.widgets.RINX_WARNING_BG}});
            let mut label = badge.label(cx, ids!(device_label));
            script_apply_eval!(cx, label, {draw_text +: {color: mod.widgets.RINX_WARNING_FG}});
        }
        AgentDeviceState::Checking => {
            script_apply_eval!(cx, badge, {draw_bg +: {color: mod.widgets.RINX_HOVER}});
            let mut label = badge.label(cx, ids!(device_label));
            script_apply_eval!(cx, label, {draw_text +: {color: mod.widgets.RINX_MUTED}});
        }
    }
}
#[derive(Clone, Debug)]
pub(crate) struct ProjectRoomNavigation {
    pub identity: String,
    pub project_id: String,
}
impl ProjectRoomNavigation {
    pub(crate) fn current(&self) -> bool {
        Service::for_current().is_ok_and(|service| service.identity() == self.identity)
    }
}
fn navigate_agent_chat(cx: &mut Cx, room: matrix_sdk::ruma::OwnedRoomId, owner_direct: bool, project: Option<(&str, &str)>) {
    if owner_direct {
        cx.action(crate::home::main_desktop_ui::MainDesktopUiAction::ShowChats);
    } else if let Some((identity, project_id)) = project {
        cx.action(ProjectRoomNavigation {identity: identity.into(), project_id: project_id.into()});
    }
    let name = crate::sliding_sync::get_client().and_then(|client| client.get_room(&room))
        .and_then(|sdk_room| sdk_room.cached_display_name().or_else(|| sdk_room.name().map(matrix_sdk::RoomDisplayName::Named)))
        .map(|name| crate::utils::RoomNameId::new(name, room.clone()))
        .or_else(|| if cx.has_global::<crate::home::rooms_list::RoomsListRef>() {
            cx.global::<crate::home::rooms_list::RoomsListRef>().get_room_name(&room)
        } else {None})
        .unwrap_or_else(|| crate::utils::RoomNameId::empty(room));
    cx.action(crate::app::AppStateAction::NavigateToRoom {
        room_to_close: None,
        destination_room: crate::room::BasicRoomDetails::RoomId(name),
    });
}
fn agent_empty_message(loaded: bool, loading: bool) -> &'static str {
    if loading { "Loading your Agents…" }
    else if !loaded { "Your Agent list could not be loaded. Refresh to retry." }
    else { "No Agents yet. Create an Agent, then configure this device and add it to Rooms." }
}
fn user_error(error: &str) -> &str {
    if let Some(message) = provider_error_message(error) { return message; }
    match error {
        "shared_workspace_changed" => "The workspace changed. Refresh its settings and confirm the directory again.",
        "configuration_unsupported" | "unsupported_hagency_capabilities" => "This server does not support execution device configuration. Upgrade the server and execution device client, then refresh.",
        "server_unavailable" | "configuration_local_state_unavailable" => "Could not reach execution device configuration. Check your connection and refresh.",
        "configuration_authority_changed" | "configuration_authority_invalidated" => "Account or device authorization changed. Sign in again and reopen this Agent.",
        "configuration_device_offline" | "execution_device_offline" | "configuration_device_unavailable" => "Execution device offline. Cached settings are read-only.",
        "invalid_configuration_response" | "configuration_outcome_unknown" | "configuration_request_unknown" | "configuration_request_expired" | "configuration_request_timeout" => "Application result is unknown. Refresh the execution device settings before retrying; the original request is retained.",
        "configuration_request_busy" | "configuration_runtime_busy" | "configuration_queue_full" => "The execution device is busy. Stop its active service there or wait for the current configuration request.",
        "configuration_generation_conflict" | "configuration_assignment_changed" | "configuration_target_changed" => "Execution device assignment changed. Reopen this Agent.",
        "configuration_model_unavailable" | "configuration_provider_unavailable" => "The selected model, profile or reasoning effort is unavailable on the execution device. Refresh its environment and choose an available option.",
        "invalid_model_profile" | "invalid_local_model" | "workspace_not_canonical" | "workspace_root_invalid" => "The model profile or workspace is invalid on the execution device. Choose its catalog entry and an existing absolute directory, or leave the workspace empty.",
        "invalid_local_policy" => "The execution device rejected the workspace or policy settings. Custom workspaces must be existing absolute directories; check token allowances and exact tool and directory rules.",
        "policy_revision_conflict" | "local_revision_conflict" => "The Agent policy changed. Reload the execution device settings before saving again.",
        "agent_lease_busy"=>"The previous runtime lease is still active. Wait briefly and retry.",
        "room_join_required" => {
            "Join this Space or Room first. Discussion Rooms have independent membership."
        }
        "agent_command_recovery_required" => {
            "An original Agent request needs confirmation. Refresh Agents and resume that original request."
        }
        "management_request_unavailable" => "This management read is still waiting. You can refresh to retry; any late response from this view will be ignored.",
        "members_unavailable" => "Could not load members. Check your connection and retry.",
        "forbidden" | "creation_not_allowed" | "project_forbidden" => {
            "You do not have permission for this action in this Project."
        }
        "project_not_found" | "room_not_found" | "room_kind_mismatch" => {
            "This Project or Room is no longer available. Refresh the list."
        }
        "device_authorization_required" | "local_access_required"
        | "native_resume_missing" | "native_resume_revoked" => {
            "Your account authorization expired. Sign in again to continue."
        }
        value if value.contains('_') && !value.contains(' ') => {
            "This action could not be completed. Refresh and retry."
        }
        _ => "This action could not be completed. Refresh and retry.",
    }
}
fn member_summary(member: &Value) -> String {
    let state = if string(member, "membership") == "invite" {
        "Invited"
    } else {
        "Joined"
    };
    let role = match string(member, "role") {
        "Creator" => "Creator",
        "Administrator" => "Administrator",
        "Moderator" => "Moderator",
        _ => "Member",
    };
    format!("{} · {}", crate::i18n::tr(state), crate::i18n::tr(role))
}
fn mutation_refresh(target: Target) -> Option<Command> {
    match target {
        Target::ProjectAdopted | Target::RoomAdopted => Some(Command::Projects),
        Target::AgentCreated | Target::AgentBound => Some(Command::Agents),
        _ => None,
    }
}
fn recovery_command(input: Value, phase: &str, room: String) -> Command {
    if phase == "unrecorded" {
        Command::CreateMatrix { input }
    } else {
        Command::ResumeCreation {
            id: string(&input, "commandId").into(),
            room_id: (!room.is_empty()).then_some(room),
        }
    }
}
// PortalList requests indices beyond its logical range to fill its viewport.
// Only the real empty-state index carries text; all other entries are blank.
fn card_template(count: usize, index: usize) -> LiveId {
    if index < count {
        id!(Card)
    } else if count == 0 && index == 0 {
        id!(Empty)
    } else {
        id!(Filler)
    }
}
fn array(v: &Value, key: &str) -> Vec<Value> {
    v[key].as_array().cloned().unwrap_or_default()
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let Text = Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text +: {color: mod.widgets.RINX_INK text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}}
    let Input = RobrixTextInput {
        width: Fill height: 40 margin: 0 padding: Inset{left: 12 right: 12 top: 8 bottom: 8}
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
    }
    let Select = RobrixSettingsDropDown {
        width: Fill height: 40 margin: 0
        popup_menu_position: PopupMenuPosition.BelowInput
        draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
        popup_menu: RobrixSettingsPopupMenu {
            width: 360
            menu_item: RobrixSettingsPopupMenuItem {
                height: 36
                draw_text.text_style: theme.font_regular{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}
            }
        }
    }
    let Action = RobrixNeutralIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Primary = RobrixIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Danger = RobrixNegativeIconButton {height: 40 icon_walk: Walk{width: 0 height: 0} spacing: 0 padding: Inset{left: 14 right: 14 top: 0 bottom: 0}}
    let Section = RoundedView {width: Fill height: Fit flow: Down padding: 16 spacing: 10
        draw_bg +: {color: mod.widgets.RINX_SURFACE border_color: mod.widgets.RINX_BORDER border_size: 1 border_radius: 8}
    }
    let DeviceBadge = RoundedView {width: Fit height: 28 align: Align{y:0.5} padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
        draw_bg +: {color: mod.widgets.RINX_HOVER border_radius: 8}
        device_label := Label {width: Fit height: Fit padding: 0 draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_bold{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
    }
    let Cards = PortalList {width: Fill height: Fill
        Card := RoundedView {width: Fill height: Fit flow: Right padding: 16 spacing: 16 margin: Inset{bottom: 12} align: Align{y: 0.5}
            draw_bg +: {color: mod.widgets.RINX_SURFACE border_color: mod.widgets.RINX_BORDER border_size: 1 border_radius: 8}
            View {width: Fill height: Fit flow: Down spacing: 6
                title := Text {draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                badge_row := View {width: Fill height: Fit flow: Right spacing: 6
                    device_badge := DeviceBadge {visible: false}
                    runtime_badge := DeviceBadge {visible: false}
                }
                summary := Text {draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
            }
            service_control := RobrixNeutralIconButton {visible: false height: 40 spacing: 6 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 16 height: 16} draw_icon.svg: ICON_SERVICE_PLAY text: "Start"}
            open := Action {text: #(crate::i18n::tr("View details")) i18n_text: "View details"}
        }
        Filler := View {width: Fill height: 64}
        Empty := RoundedView {width: Fill height: Fit flow: Down padding: 24 spacing: 12 margin: Inset{top: 8}
            draw_bg +: {color: mod.widgets.RINX_SURFACE border_color: mod.widgets.RINX_BORDER border_size: 1 border_radius: theme.corner_radius}
            empty_message := Text {text: #(crate::i18n::tr("No Agent spaces yet. Choose Agent space in the creation wizard to get started.")) i18n_text: "No Agent spaces yet. Choose Agent space in the creation wizard to get started."}
        }
    }
    mod.widgets.HagencyPanel = #(HagencyPanel::register_widget(vm)) {
        ..mod.widgets.SolidView
        width: Fill height: Fill flow: Down padding: Inset{top: 16 left: 20 right: 0 bottom: 0} spacing: 12 draw_bg.color: mod.widgets.RINX_PAGE
        View {width: Fill height: 40 margin: Inset{right: 20} spacing: 10 align: Align{y: 0.5}
            back := RinxBackButton {visible: false}
            heading := Text {text: #(crate::i18n::tr("Agent spaces")) i18n_text: "Agent spaces" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
            loading_status := PacedLoadingSpinner {visible: false width: 18 height: 18 draw_bg +: {color: mod.widgets.RINX_MUTED stroke_width: 2.0}}
            new_item := Primary {text: #(crate::i18n::tr("Create a space or room…")) i18n_text: "Create a space or room…"}
            refresh := Action {text: #(crate::i18n::tr("Refresh")) i18n_text: "Refresh"}
        }
        owner := Text {visible: false}
        status_holder := View {visible: false width: Fill height: Fit status := Text {width: Fill height: Fit visible: false}}
        login := Action {visible: false text: #(crate::i18n::tr("Sign in again")) i18n_text: "Sign in again"}
        lists_pane := View {width: Fill height: Fill margin: Inset{right: 20 bottom: 16} flow: Down spacing: 10
            list_intro := Section {
                View {width: Fill height: Fit flow: Right spacing: 16 align: Align{y: 0.5}
                    RoundedView {width: 48 height: 48 align: Align{x: 0.5 y: 0.5} draw_bg +: {color: mod.widgets.RINX_SELECTED border_radius: theme.corner_radius}
                        context_project_icon := View {width: 24 height: 24
                            Icon {width: Fill height: Fill draw_icon +: {svg: crate_resource("self://resources/icons/hagency-project.svg") color: mod.widgets.RINX_ACCENT}}
                        }
                        context_agent_icon := View {visible: false width: 24 height: 24
                            Icon {width: Fill height: Fill draw_icon +: {svg: crate_resource("self://resources/icons/person.svg") color: mod.widgets.RINX_ACCENT}}
                        }
                    }
                    View {width: Fill height: Fit flow: Down spacing: 8
                        context_title := Text {draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                        context_description := Text {draw_text.color: mod.widgets.RINX_MUTED}
                    }
                }
                context_steps := Text {draw_text.color: mod.widgets.RINX_ACCENT}
                context_scope := Text {draw_text.color: mod.widgets.RINX_MUTED}
            }
                agent_recovery := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                    Text {text: #(crate::i18n::tr("Unfinished Agent requests")) i18n_text: "Unfinished Agent requests" draw_text.text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
                    agent_pending_count := Text {draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
                    agent_pending := Select {labels: [#(crate::i18n::tr("Select an original request"))]}
                    agent_pending_details := Text {}
                    resume_agent_command := Action {text: #(crate::i18n::tr("Resume original Agent request")) i18n_text: "Resume original Agent request"}
                }
            project_list_holder := View {width: Fill height: Fill project_list := Cards {}}
            agent_list_holder := View {visible: false width: Fill height: Fill agent_list := Cards {}}
        }
        details_scroll := ScrollYView {visible: false width: Fill height: Fill flow: Down padding: Inset{right: 20 bottom: 16} spacing: 12
            scroll_bars +: {scroll_bar_y +: {bar_size: 10 bar_side_margin: 2
                draw_bg +: {color: mod.widgets.RINX_MUTED color_hover: mod.widgets.RINX_INK color_drag: mod.widgets.RINX_ACCENT size: 5 border_size: 0 border_radius: 2.5}
            }}
            projects_page := View {width: Fill height: Fit flow: Down spacing: 10
                View {visible: false width: 0 height: 0 projects := Select {labels: [#(crate::i18n::tr("Select a Project"))]}}
                project_detail := View {visible: false width: Fill height: Fit flow: Down spacing: 10
                    View {width: Fill height: 40 spacing: 8
                        overview_tab := Action {text: #(crate::i18n::tr("Overview")) i18n_text: "Overview"}
                        rooms_tab := Action {text: #(crate::i18n::tr("Rooms")) i18n_text: "Rooms"}
                        members_tab := Action {text: #(crate::i18n::tr("Members")) i18n_text: "Members"}
                        permissions_tab := Action {text: #(crate::i18n::tr("Access")) i18n_text: "Access"}
                    }
                    overview_section := Section {width: Fill height: Fit flow: Down spacing: 10
                    project_details := Text {}
                    Text {text: #(crate::i18n::tr("Discussion Rooms have their own members.")) i18n_text: "Discussion Rooms have their own members." draw_text.color: mod.widgets.RINX_MUTED}
                    open_space := Action {text: #(crate::i18n::tr("Open Space")) i18n_text: "Open Space"}
                    }
                    rooms_section := Section {visible: false width: Fill height: Fit flow: Down spacing: 10
                    Text {text: #(crate::i18n::tr("Choose a discussion Room to open it or manage its members.")) i18n_text: "Choose a discussion Room to open it or manage its members." draw_text.color: mod.widgets.RINX_MUTED}
                    new_room := Action {text: #(crate::i18n::tr("Add discussion Room")) i18n_text: "Add discussion Room"}
                    rooms := Select {labels: [#(crate::i18n::tr("Select a Room"))]}
                    room_details := Text {}
                    View {width: Fill height: 40 spacing: 8
                        room_members := Action {text: #(crate::i18n::tr("Room members")) i18n_text: "Room members"}
                        open_room := Action {text: #(crate::i18n::tr("Open Room")) i18n_text: "Open Room"}
                    }
                    room_permissions := Action {text: #(crate::i18n::tr("Room Agent permissions")) i18n_text: "Room Agent permissions"}
                    room_agents := Action {text: #(crate::i18n::tr("Manage my Agents")) i18n_text: "Manage my Agents"}
                    }
                    permissions_section := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                        permission_heading := Text {text: #(crate::i18n::tr("Access")) i18n_text: "Access"}
                        permission_status := Text {}
                        permission_editor := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                            Text {text: #(crate::i18n::tr("Access rule")) i18n_text: "Access rule"}
                            creation_mode := Select {labels: [#(crate::i18n::tr("Allow Project members by default")) #(crate::i18n::tr("Only specified Project members"))]}
                            Text {text: #(crate::i18n::tr("Allowed members")) i18n_text: "Allowed members"}
                            creation_allow := Input {empty_text: #(crate::i18n::tr("Allowed Matrix user IDs, comma separated")) i18n_empty_text: "Allowed Matrix user IDs, comma separated"}
                            Text {text: #(crate::i18n::tr("Blocked members")) i18n_text: "Blocked members"}
                            creation_deny := Input {empty_text: #(crate::i18n::tr("Denied Matrix user IDs, comma separated")) i18n_empty_text: "Denied Matrix user IDs, comma separated"}
                            save_creation_policy := Primary {text: #(crate::i18n::tr("Save Agent access permissions")) i18n_text: "Save Agent access permissions"}
                            Hr {width: Fill height: 1}
                            Text {text: #(crate::i18n::tr("Service availability")) i18n_text: "Service availability"}
                            scope_pause := CheckBox {text: #(crate::i18n::tr("Pause Agent services in this Project or Room")) i18n_text: "Pause Agent services in this Project or Room"}
                            save_scope_pause := Primary {text: #(crate::i18n::tr("Apply service pause")) i18n_text: "Apply service pause"}
                            Text {text: #(crate::i18n::tr("Access permissions and service pause are separate. Resuming services does not change access permissions.")) i18n_text: "Access permissions and service pause are separate. Resuming services does not change access permissions." draw_text.color: mod.widgets.RINX_MUTED}
                        }
                    }
                    members_section := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                        members_heading := Text {text: #(crate::i18n::tr("Members")) i18n_text: "Members"}
                        members_note := Text {}
                        members_status := Text {}
                        View {width: Fill height: 40 spacing: 8
                            refresh_members := Action {text: #(crate::i18n::tr("Refresh members")) i18n_text: "Refresh members"}
                            invite_member := Action {text: #(crate::i18n::tr("Invite to Space")) i18n_text: "Invite to Space" enabled: false}
                        }
                        members_list := PortalList {width: Fill height: Fill
                            Member := View {width: Fill height: Fit flow: Down padding: Inset{left: 8 right: 8 top: 8 bottom: 8} spacing: 2
                                name := Text {max_lines: 1 text_overflow: Ellipsis}
                                identity := Text {max_lines: 1 text_overflow: Ellipsis draw_text.color: mod.widgets.RINX_MUTED}
                                membership := Text {draw_text.color: mod.widgets.RINX_MUTED}
                            }
                            Filler := View {width: Fill height: 64}
                        }
                    }
                }
                project_form := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                    creation_heading := Text {visible: false text: #(crate::i18n::tr("Create Matrix workspace")) i18n_text: "Create Matrix workspace" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                    creation_note := Text {text: #(crate::i18n::tr("Choose a new Space or connect one you already belong to.")) i18n_text: "Choose a new Space or connect one you already belong to." draw_text.color: mod.widgets.RINX_MUTED}
                    Text {text: #(crate::i18n::tr("Create or connect")) i18n_text: "Create or connect"}
                    creation_source := Select {labels: [#(crate::i18n::tr("Create a new Space (default)")) #(crate::i18n::tr("Connect an existing Space"))]}
                    creation_name_holder := View {width: Fill height: Fit flow: Down spacing: 4 Text {text: #(crate::i18n::tr("Name")) i18n_text: "Name"} creation_name := Input {empty_text: "Project name"}}
                    candidates_holder := View {visible: false width: Fill height: Fit flow: Down spacing: 4 Text {text: #(crate::i18n::tr("Space")) i18n_text: "Space"}
 candidates := Select {labels: [#(crate::i18n::tr("Select a joined Space"))]}}
                    candidates_more := Action {visible: false text: #(crate::i18n::tr("Refresh / load more Spaces")) i18n_text: "Refresh / load more Spaces"}
                    existing_room_holder := View {visible: false width: Fill height: Fit flow: Down spacing: 4 Text {text: #(crate::i18n::tr("Room ID")) i18n_text: "Room ID"}
 existing_room := Input {empty_text: "!existing-room:server"}}
                    create_project_room := Primary {text: #(crate::i18n::tr("Create Matrix workspace")) i18n_text: "Create Matrix workspace"}
                    creation_result := Text {}
                    recovery := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("An earlier creation has not been confirmed. Resume the original request to avoid a duplicate.")) i18n_text: "An earlier creation has not been confirmed. Resume the original request to avoid a duplicate."}
                        found_room := Input {empty_text: #(crate::i18n::tr("Created Matrix ID, if known")) i18n_empty_text: "Created Matrix ID, if known"}
                        resume_creation := Action {text: #(crate::i18n::tr("Resume original request")) i18n_text: "Resume original request"}
                    }
                }
            }
            agents_page := View {visible: false width: Fill height: Fit flow: Down spacing: 10
                View {visible: false width: 0 height: 0 agents := Select {labels: [#(crate::i18n::tr("Select my Agent"))]}}
                agent_create := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                    agent_creation_heading := Text {visible: false text: #(crate::i18n::tr("Create local agent")) i18n_text: "Create local agent" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                    Text {text: #(crate::i18n::tr("Create an Agent on this device. Connect a local runtime and add Rooms afterward.")) i18n_text: "Create an Agent on this device. Connect a local runtime and add Rooms afterward." draw_text.color: mod.widgets.RINX_MUTED}
                    agent_binding_target := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                        agent_project := Select {labels: [#(crate::i18n::tr("Select a Project"))]}
                        agent_room := Select {labels: [#(crate::i18n::tr("Select a visible registered Room"))]}
                    }
                    agent_name_holder := View {width: Fill height: Fit flow: Down spacing: 4 Text {text: #(crate::i18n::tr("Agent name")) i18n_text: "Agent name"}
 agent_name := Input {empty_text: #(crate::i18n::tr("Agent display name")) i18n_empty_text: "Agent display name"}}
                    create_agent := Primary {text: #(crate::i18n::tr("Create local agent")) i18n_text: "Create local agent"}
                    bind_agent := Primary {text: #(crate::i18n::tr("Bind selected existing Agent to Room")) i18n_text: "Bind selected existing Agent to Room"}
                }
                agent_detail := View {visible: false width: Fill height: Fit flow: Down spacing: 10
                    View {width: Fill height: 40 spacing: 8
                        setup_tab := Action {text: #(crate::i18n::tr("Agent")) i18n_text: "Agent"}
                        model_tab := Action {text: #(crate::i18n::tr("Model & resources")) i18n_text: "Model & resources"}
                        policy_tab := Action {text: #(crate::i18n::tr("Limits")) i18n_text: "Limits"}
                        runtime_tab := Action {text: #(crate::i18n::tr("Service")) i18n_text: "Service"}
                    }
                    configuration_notice := Text {visible: false draw_text.color: mod.widgets.RINX_MUTED}
                    configuration_refresh := Action {visible: false text: #(crate::i18n::tr("Refresh execution device environment")) i18n_text: "Refresh execution device environment"}
                    local_device_notice := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                        local_device_message := Text {}
                        assignment_confirm := CheckBox {text: #(crate::i18n::tr("Switching devices stops the previous lease. Its configuration is not copied. Restore this owner's complete ledger and configure resources before running here.")) i18n_text: "Switching devices stops the previous lease. Its configuration is not copied. Restore this owner's complete ledger and configure resources before running here."}
                        assign_current_device := Primary {text: #(crate::i18n::tr("Handle on this device")) i18n_text: "Handle on this device"}
                    }
                    setup_section := View {width: Fill height: Fit flow: Down spacing: 10
                    identity_section := Section {width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Identity")) i18n_text: "Identity" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                        agent_details := Text {max_lines: 3 text_overflow: Ellipsis draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
                        Text {text: #(crate::i18n::tr("Execution device")) i18n_text: "Execution device"}
                        execution_badge := DeviceBadge {}
                        owner_direct_status := Text {}
                        View {width: Fill height: 40 spacing: 8
                            ensure_owner_direct := Action {text: #(crate::i18n::tr("Set up / retry private chat")) i18n_text: "Set up / retry private chat"}
                            open_owner_direct := Action {text: #(crate::i18n::tr("Open private chat")) i18n_text: "Open private chat" enabled: false}
                        }
                        encrypt_owner_direct := ToggleFlat {active: false text: #(crate::i18n::tr("End-to-end encryption")) i18n_text: "End-to-end encryption"}
                    }
                    View {width: Fill height: 40 spacing: 8
                        pause_agent := Action {text: #(crate::i18n::tr("Pause Agent")) i18n_text: "Pause Agent"}
                        resume_agent := Action {text: #(crate::i18n::tr("Resume Agent")) i18n_text: "Resume Agent"}
                        another_binding := Action {text: #(crate::i18n::tr("Add Room binding")) i18n_text: "Add Room binding"}
                    }
                    }
                    retirement_section := Section {visible: false width: Fill height: Fit padding: 12 spacing: 6
                    Text {text: #(crate::i18n::tr("Dangerous actions")) i18n_text: "Dangerous actions" draw_text.color: mod.widgets.RINX_MUTED}
                    retire_confirm := CheckBox {text: #(crate::i18n::tr("I confirm retirement stops every Room; this identity remains mine")) i18n_text: "I confirm retirement stops every Room; this identity remains mine"}
                    retire_agent := Danger {text: #(crate::i18n::tr("Retire Agent permanently")) i18n_text: "Retire Agent permanently"}
                    }
                    policy_section := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Usage limits")) i18n_text: "Usage limits" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                        Text {text: #(crate::i18n::tr("No token limit is required. Set optional limits for this Agent, one Room, or one member in a Room. All applicable limits are checked together.")) i18n_text: "No token limit is required. Set optional limits for this Agent, one Room, or one member in a Room. All applicable limits are checked together." draw_text.color: mod.widgets.RINX_MUTED}
                        Text {text: #(crate::i18n::tr("Apply to")) i18n_text: "Apply to"}
                        policy_layer := Select {labels: [#(crate::i18n::tr("Entire Agent")) #(crate::i18n::tr("One Room")) #(crate::i18n::tr("One member in a Room"))] i18n_labels: ["Entire Agent" "One Room" "One member in a Room"]}
                        policy_room_holder := View {visible: false width: Fill height: Fit flow: Down spacing: 4
                            Text {text: #(crate::i18n::tr("Room")) i18n_text: "Room"}
                            policy_binding := Select {labels: [#(crate::i18n::tr("Select a Room binding"))]}
                        }
                        policy_target := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        requester_holder := View {width: Fill height: Fit flow: Down spacing: 8
                            Text {text: #(crate::i18n::tr("Member")) i18n_text: "Member"}
                            requester := Input {empty_text: "@requester:server"}
                            load_policy := Action {text: #(crate::i18n::tr("Load member settings")) i18n_text: "Load member settings"}
                        }
                        policy_usage := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        Text {text: #(crate::i18n::tr("Token allowance")) i18n_text: "Token allowance"}
                        limit_mode := Select {labels: [#(crate::i18n::tr("No token limit")) #(crate::i18n::tr("Limited tokens"))] i18n_labels: ["No token limit" "Limited tokens"]}
                        token_amount_holder := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                            Text {text: #(crate::i18n::tr("Tokens")) i18n_text: "Tokens"}
                            token_limit := Input {empty_text: #(crate::i18n::tr("Nonnegative token quota")) i18n_empty_text: "Nonnegative token quota"}
                        }
                        period_holder := View {visible: false width: Fill height: Fit flow: Down spacing: 4
                            Text {text: #(crate::i18n::tr("Reset allowance")) i18n_text: "Reset allowance"}
                            period := Select {labels: [#(crate::i18n::tr("Never (cumulative)")) #(crate::i18n::tr("Daily (UTC)")) #(crate::i18n::tr("Monthly (UTC)"))] i18n_labels: ["Never (cumulative)" "Daily (UTC)" "Monthly (UTC)"]}
                        }
                        zero_limit_note := Text {visible: false text: #(crate::i18n::tr("Use zero tokens to stop usage in this scope.")) i18n_text: "Use zero tokens to stop usage in this scope." draw_text.color: mod.widgets.RINX_MUTED}
                        Hr {width: Fill height: 1}
                        Text {text: #(crate::i18n::tr("Request permissions")) i18n_text: "Request permissions" draw_text.text_style: theme.font_bold{font_size: (13 * mod.widgets.RINX_TEXT_SCALE)}}
                        Text {text: #(crate::i18n::tr("Request handling")) i18n_text: "Request handling"}
                        requests := Select {labels: [#(crate::i18n::tr("Allow requests")) #(crate::i18n::tr("Deny requests")) #(crate::i18n::tr("Ask owner per request"))] i18n_labels: ["Allow requests" "Deny requests" "Ask owner per request"]}
                        Text {text: #(crate::i18n::tr("High-risk tools")) i18n_text: "High-risk tools"}
                        tool_risk := Select {labels: [#(crate::i18n::tr("Deny high-risk tools")) #(crate::i18n::tr("Ask owner per tool")) #(crate::i18n::tr("Allow only exact tools/directories"))] i18n_labels: ["Deny high-risk tools" "Ask owner per tool" "Allow only exact tools/directories"]}
                        allowed_tools_holder := View {visible: false width: Fill height: Fit flow: Down spacing: 4 Text {text: #(crate::i18n::tr("Allowed tools")) i18n_text: "Allowed tools"}
 allowed_tools := Input {empty_text: #(crate::i18n::tr("Exact tool names, comma separated")) i18n_empty_text: "Exact tool names, comma separated"}}
                        allowed_directories_holder := View {visible: false width: Fill height: Fit flow: Down spacing: 4 Text {text: #(crate::i18n::tr("Allowed directories")) i18n_text: "Allowed directories"}
 allowed_directories := Input {empty_text: #(crate::i18n::tr("Exact absolute directories, comma separated")) i18n_empty_text: "Exact absolute directories, comma separated"}}
                        Text {text: #(crate::i18n::tr("Saving keeps recorded usage and pending reservations. Any applicable deny rule still blocks the request.")) i18n_text: "Saving keeps recorded usage and pending reservations. Any applicable deny rule still blocks the request."}
                        save_policy := Primary {text: #(crate::i18n::tr("Save settings")) i18n_text: "Save settings"}
                        reset_policy := Action {text: #(crate::i18n::tr("Reset this scope")) i18n_text: "Reset this scope"}
                    }
                    model_section := Section {visible: false width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Model & resources")) i18n_text: "Model & resources" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                        Text {text: #(crate::i18n::tr("Model settings apply to this Agent across its Rooms.")) i18n_text: "Model settings apply to this Agent across its Rooms." draw_text.color: mod.widgets.RINX_MUTED}
                        Text {text: #(crate::i18n::tr("Local agent runtime")) i18n_text: "Local agent runtime"}
                        runtime_kind := Select {labels: ["Codex" "Claude Code" "Octos"]}
                        runtime_provider_note := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        provider_status := Text {}
                        provider_controls := View {width: Fill height: Fit flow: Down spacing: 8
                            use_local_provider := Action {text: #(crate::i18n::tr("Connect my local account")) i18n_text: "Connect my local account"}
                            provider_login_actions := View {width: Fill height: 40 spacing: 8
                                provider_check := Action {text: #(crate::i18n::tr("Check local account")) i18n_text: "Check local account"}
                                provider_login := Action {text: #(crate::i18n::tr("Sign in to Codex")) i18n_text: "Sign in to Codex"}
                            }
                            provider_pending_actions := View {visible: false width: Fill height: 40 spacing: 8
                                provider_cancel := Action {visible: false text: #(crate::i18n::tr("Cancel sign-in")) i18n_text: "Cancel sign-in"}
                            }
                            provider_open := Action {visible: false text: #(crate::i18n::tr("Open provider sign-in URL")) i18n_text: "Open provider sign-in URL"}
                        }
                        Text {text: #(crate::i18n::tr("Local model or Octos profile")) i18n_text: "Local model or Octos profile"}
                        model := Select {labels: [#(crate::i18n::tr("Load available models or profiles"))]}
                        model_note := Text {}
                        Text {text: #(crate::i18n::tr("Reasoning effort")) i18n_text: "Reasoning effort"}
                        effort := Select {labels: [#(crate::i18n::tr("Checking model choices…"))]}
                        effort_note := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        web_search_holder := View {width: Fill height: Fit flow: Down spacing: 4
                            web_search := CheckBox {text: #(crate::i18n::tr("Live web search")) i18n_text: "Live web search"}
                            Text {text: #(crate::i18n::tr("Enabled by default. Save and restart the service to apply changes.")) i18n_text: "Enabled by default. Save and restart the service to apply changes." draw_text.color: mod.widgets.RINX_MUTED}
                        }
                        refresh_models := Action {text: #(crate::i18n::tr("Refresh available models")) i18n_text: "Refresh available models"}
                        advanced_toggle := Action {text: #(crate::i18n::tr("Advanced settings")) i18n_text: "Advanced settings" spacing: 6 icon_walk: Walk{width: 12 height: 12} draw_icon.svg: ICON_CHEVRON_RIGHT}
                        advanced_settings := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Local account")) i18n_text: "Local account"}
                        View {width: Fill height: 40 spacing: 8
                            provider_advanced_check := Action {text: #(crate::i18n::tr("Check local account")) i18n_text: "Check local account"}
                            disconnect_provider := Action {visible: false text: #(crate::i18n::tr("Disconnect local account")) i18n_text: "Disconnect local account"}
                            provider_logout := Action {visible: false text: #(crate::i18n::tr("Sign out of separate Codex account")) i18n_text: "Sign out of separate Codex account"}
                        }
                        Text {text: #(crate::i18n::tr("Disconnecting here keeps your local runtime signed in.")) i18n_text: "Disconnecting here keeps your local runtime signed in." draw_text.color: mod.widgets.RINX_MUTED}
                        workspace_label := Text {text: #(crate::i18n::tr("Agent workspace on this device")) i18n_text: "Agent workspace on this device"}
                        workspace := Input {empty_text: #(crate::i18n::tr("Automatic: private workspace for this Agent")) i18n_empty_text: "Automatic: private workspace for this Agent"}
                        Text {text: #(crate::i18n::tr("Leave empty to use this Agent's private workspace. Custom directories must already exist.")) i18n_text: "Leave empty to use this Agent's private workspace. Custom directories must already exist." draw_text.color: mod.widgets.RINX_MUTED}
                        prepare_workspace := Action {text: #(crate::i18n::tr("Prepare this Agent's private workspace")) i18n_text: "Prepare this Agent's private workspace"}
                        }
                        credential := Text {visible: false}
                        save_model := Primary {text: #(crate::i18n::tr("Save local model profile")) i18n_text: "Save local model profile"}
                        Text {text: #(crate::i18n::tr("Restart the Agent service to apply saved model settings.")) i18n_text: "Restart the Agent service to apply saved model settings." draw_text.color: mod.widgets.RINX_MUTED}
                    }
                    runtime_section := View {visible: false width: Fill height: Fit flow: Down spacing: 12
                        Section {width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Agent service")) i18n_text: "Agent service" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                        runtime_chat_target := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        runtime_status := Text {}
                        runtime_start_options := View {width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Usage is charged after each task. Running tasks may exceed the quota and finish normally. Missing usage remains pending measurement and does not reserve tokens.")) i18n_text: "Usage is charged after each task. Running tasks may exceed the quota and finish normally. Missing usage remains pending measurement and does not reserve tokens."}
                        start_requirements := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        service_advanced_toggle := Action {text: #(crate::i18n::tr("Advanced service options")) i18n_text: "Advanced service options" spacing: 6 icon_walk: Walk{width: 12 height: 12} draw_icon.svg: ICON_CHEVRON_RIGHT}
                        service_advanced := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                        takeover := CheckBox {text: #(crate::i18n::tr("Explicitly take over this Agent's device lease")) i18n_text: "Explicitly take over this Agent's device lease"}
                        host_files := CheckBox {visible: false text: #(crate::i18n::tr("Enable this Room's scoped host files (list/read/create)")) i18n_text: "Enable this Room's scoped host files (list/read/create)"}
                        shared_workspace_path := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        shared_workspace := CheckBox {text: #(crate::i18n::tr("Allow this Agent to edit the selected workspace in all current and future connected Rooms")) i18n_text: "Allow this Agent to edit the selected workspace in all current and future connected Rooms"}
                        Text {text: #(crate::i18n::tr("Codex only; UTF-8 files up to 128 KiB. File changes require tool permissions and may require approval. Updates preserve mode bits but not extended attributes or ACLs; concurrent external edits may be overwritten.")) i18n_text: "Codex only; UTF-8 files up to 128 KiB. File changes require tool permissions and may require approval. Updates preserve mode bits but not extended attributes or ACLs; concurrent external edits may be overwritten." draw_text.color: mod.widgets.RINX_MUTED}
                        }
                        }
                        runtime_model_settings := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        runtime_restart_note := Text {visible: false text: #(crate::i18n::tr("Stop responding before changing start options, then start again.")) i18n_text: "Stop responding before changing start options, then start again." draw_text.color: mod.widgets.RINX_MUTED}
                        View {width: Fill height: 40 spacing: 8
                            service_toggle := Primary {spacing: 6 icon_walk: Walk{width: 16 height: 16} draw_icon.svg: ICON_SERVICE_PLAY text: #(crate::i18n::tr("Checking…"))}
                            runtime_check := Action {text: #(crate::i18n::tr("Refresh status")) i18n_text: "Refresh status"}
                        }
                        }
                        Section {width: Fill height: Fit flow: Down spacing: 8
                            Text {text: #(crate::i18n::tr("Connected Rooms")) i18n_text: "Connected Rooms" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                            Text {text: #(crate::i18n::tr("Pause only where needed. Requests in a paused Room receive a notice.")) i18n_text: "Pause only where needed. Requests in a paused Room receive a notice." draw_text.color: mod.widgets.RINX_MUTED}
                            service_rooms := PortalList {width: Fill height: 260
                                Room := View {width: Fill height: Fit flow: Right padding: Inset{top: 8 bottom: 8} spacing: 8 align: Align{y: 0.5}
                                    View {width: Fill height: Fit flow: Down spacing: 4
                                        room_name := Text {draw_text.text_style: theme.font_bold{font_size: (12 * mod.widgets.RINX_TEXT_SCALE)}}
                                        room_status := Text {draw_text.color: mod.widgets.RINX_MUTED}
                                    }
                                    open_room_chat := Action {text: #(crate::i18n::tr("Open chat")) i18n_text: "Open chat"}
                                    room_toggle := RobrixNeutralIconButton {height: 40 text: "Pause" spacing: 6 padding: Inset{left: 12 right: 12 top: 0 bottom: 0} icon_walk: Walk{width: 16 height: 16} draw_icon.svg: ICON_SERVICE_PAUSE}
                                }
                                Empty := View {width: Fill height: 48 Text {text: #(crate::i18n::tr("No connected Rooms yet.")) i18n_text: "No connected Rooms yet."}}
                                Filler := View {width: Fill height: 32}
                            }
                        }
                        Section {width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Pending approvals")) i18n_text: "Pending approvals" draw_text.text_style: theme.font_bold{font_size: (16 * mod.widgets.RINX_TEXT_SCALE)}}
                        Text {text: #(crate::i18n::tr("Only requests or tools configured to ask you appear here.")) i18n_text: "Only requests or tools configured to ask you appear here." draw_text.color: mod.widgets.RINX_MUTED}
                        approvals_check := Action {text: #(crate::i18n::tr("Refresh pending requests/tools")) i18n_text: "Refresh pending requests/tools"}
                        approvals := Select {labels: [#(crate::i18n::tr("Select a pending approval"))]}
                        approval_count := Text {draw_text.color: mod.widgets.RINX_MUTED}
                        approval_feedback := Text {}
                        approval_details := Text {}
                        View {width: Fill height: 40 spacing: 8
                            approve := Action {text: #(crate::i18n::tr("Approve exact request once")) i18n_text: "Approve exact request once"}
                            reject := Danger {text: #(crate::i18n::tr("Reject request")) i18n_text: "Reject request"}
                        }
                        }
                    }
                    room_controls := Section {width: Fill height: Fit flow: Down spacing: 8
                        room_advanced_toggle := Action {text: #(crate::i18n::tr("Advanced Room settings")) i18n_text: "Advanced Room settings" spacing: 6 icon_walk: Walk{width: 12 height: 12} draw_icon.svg: ICON_CHEVRON_RIGHT}
                        room_advanced_content := View {visible: false width: Fill height: Fit flow: Down spacing: 8
                        Text {text: #(crate::i18n::tr("Choose a chat")) i18n_text: "Choose a chat"}
                        Text {text: #(crate::i18n::tr("Private chat is selected by default. Choose another Room to manage its service separately.")) i18n_text: "Private chat is selected by default. Choose another Room to manage its service separately." draw_text.color: mod.widgets.RINX_MUTED}
                        bindings := Select {labels: [#(crate::i18n::tr("Select a Room binding"))]}
                        binding_details := Text {}
                        reply_policy := View {width: Fill height: Fit flow: Down spacing: 8
                            Text {text: #(crate::i18n::tr("Room reply behavior")) i18n_text: "Room reply behavior"}
                            reply_policy_status := Text {draw_text.color: mod.widgets.RINX_MUTED}
                            reply_policy_toggle := Action {text: #(crate::i18n::tr("Enable automatic thread follow-ups")) i18n_text: "Enable automatic thread follow-ups"}
                        }
                        open_binding_chat := Action {text: #(crate::i18n::tr("Open selected Room chat")) i18n_text: "Open selected Room chat"}
                        room_roster_check := Action {text: #(crate::i18n::tr("Agents in this Room")) i18n_text: "Agents in this Room"}
                        room_roster := Text {}
                        room_service_actions := View {width: Fill height: 40 spacing: 8
                            pause_binding := Action {text: #(crate::i18n::tr("Pause Room")) i18n_text: "Pause Room"}
                            resume_binding := Action {text: #(crate::i18n::tr("Resume Room")) i18n_text: "Resume Room"}
                        }
                        room_danger_heading := Text {text: #(crate::i18n::tr("Dangerous actions")) i18n_text: "Dangerous actions" draw_text.color: mod.widgets.RINX_MUTED}
                        leave_confirm := CheckBox {text: #(crate::i18n::tr("I want this Agent to leave the selected Room")) i18n_text: "I want this Agent to leave the selected Room"}
                        leave_binding := Danger {text: #(crate::i18n::tr("Leave this Room")) i18n_text: "Leave this Room"}
                    }
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct HagencyPanel {
    #[source]
    source: ScriptObjectRef,
    #[deref]
    view: View,
    #[rust]
    active: bool,
    #[rust]
    identity: String,
    #[rust]
    request: u64,
    #[rust]
    busy: bool,
    #[rust]
    members: Vec<Value>,
    #[rust]
    members_room: Option<(String, bool)>,
    #[rust]
    page: Page,
    #[rust]
    projects: Vec<Value>,
    #[rust]
    rooms: Vec<Value>,
    #[rust]
    agent_rooms: Vec<Value>,
    #[rust]
    spaces: Vec<Value>,
    #[rust]
    cursor: Option<String>,
    #[rust]
    agents: Vec<Value>,
    #[rust] runtime_list: RuntimeStates,
    #[rust] runtime_list_request:u64,
    #[rust] runtime_list_timer:Timer,
    #[rust] runtime_list_loading:bool,
    #[rust] display_blocked:bool,
    #[rust] detail_scroll_key:String,
    #[rust]
    agents_loaded: bool,
    #[rust]
    agents_loading: bool,
    #[rust]
    bindings: Vec<Value>,
    #[rust]
    approvals: Vec<Value>,
    #[rust]
    policy: Value,
    #[rust]
    scope_state: Value,
    #[rust]
    creation: Option<Value>,
    #[rust]
    pending_input: Option<Value>,
    #[rust]
    create_room: bool,
    #[rust]
    wanted_project: Option<String>,
    #[rust]
    wanted_project_section: Option<(String, ProjectDetailSection)>,
    #[rust]
    credential_ref: String,
    #[rust]
    provider_url: Option<String>,
    #[rust]
    provider_models: Vec<Value>,
    #[rust]
    provider_authenticated: bool,
    #[rust]
    runtime_efforts: Vec<String>,
    #[rust]
    model_advanced: bool,
    #[rust]
    service_advanced: bool,
    #[rust]
    room_advanced: bool,
    #[rust]
    runtime_room_active: bool,
    #[rust] agent_service_state: Value,
    #[rust] runtime_pending_action: Option<bool>,
    #[rust] runtime_action_error: String,
    #[rust] service_row_targets: ServiceRowTargets,
    #[rust] agent_card_targets: AgentCardTargets,
    #[rust] list_service_pending_agent: String,
    #[rust]
    runtime_resources_loading: bool,
    #[rust]
    runtime_resources_checked: bool,
    #[rust]
    agent_command: Option<(Value, String)>,
    #[rust]
    agent_pending_commands: Vec<Value>,
    #[rust]
    agent_journal_loaded: bool,
    #[rust] configuration_status: Value,
    #[rust] configuration_environment: Value,
    #[rust] configuration_timer: Timer,
    #[rust] configuration_discover: bool,
    #[rust] configuration_feedback: String,
    #[rust] configuration_metadata_error: String,
    #[rust] configuration_poll_request: Option<u64>,
    #[rust] configuration_keys: ConfigurationKeys,
    #[rust]
    execution_agent_record: Value,
    #[rust]
    execution_devices: Vec<Value>,
    #[rust]
    current_device_id: String,
    #[rust]
    devices_loaded: bool,
    #[rust] direct_agent: String,
    #[rust] direct_state: Value,
    #[rust] wanted_agent: Option<String>,
    #[rust] wanted_run: Option<(String,String)>,
    #[rust] direct_timer: Timer,
    #[rust] direct_retry: Option<(String,String,u8)>,
    #[rust] direct_encrypting: Option<String>,
    #[rust] direct_encrypted: HashSet<String>,
    #[rust] approval_timer: Timer,
    #[rust] approval_poll: Option<u64>,
    #[rust] approval_started: Option<Instant>,
    #[rust] approval_notices: Notices,
    #[rust] approval_checked: bool,
    #[rust] approval_blocked: HashSet<String>,
    #[rust] read_timer: Timer,
    #[rust] read_wait: Option<ReadWait>,
}
impl HagencyPanel {
    fn approval_scope(&self,cx:&mut Cx)->Option<(String,String,String)> {
        let agent=self.agent(cx)?;
        let b=self.selected(cx,ids!(bindings),&self.bindings)?;
        let binding=string(&b,"id").to_owned();let room=string(&b,"roomId").to_owned();
        if binding.is_empty() || room.is_empty() {return None;} Some((agent,binding,room))
    }
    fn clear_approval_state(&mut self,cx:&mut Cx) {
        self.approval_poll=None;self.approval_started=None;self.approval_checked=false;self.approval_notices=Default::default();
        cx.stop_timer(self.approval_timer);
        self.view.label(cx,ids!(approval_count)).set_text(cx,"");
        self.view.label(cx,ids!(approval_feedback)).set_text(cx,"");
    }
    fn approval_view_open(&self,cx:&mut Cx)->bool {
        self.active && self.page==Page::Agents && self.view.view(cx,ids!(agent_detail)).visible()
            && self.view.view(cx,ids!(runtime_section)).visible()
    }
    fn refresh_approvals(&mut self,cx:&mut Cx) {
        if self.approval_poll.is_some() || self.busy || !self.approval_view_open(cx) || !self.local_agent(cx) {return;}
        let Some((agent,binding,room))=self.approval_scope(cx) else {return;};
        let Ok(service)=Service::for_current() else {return;};
        if service.identity()!=self.identity {return;}
        let request=nonce();self.approval_poll=Some(request);self.approval_started=Some(Instant::now());let identity=self.identity.clone();
        cx.stop_timer(self.approval_timer);self.approval_timer=cx.start_timeout(3.0);
        spawn_async_task(async move {
            // A UI waiting deadline must not cancel owner/device bootstrap work.
            let result=service.execute(Command::Local {
                agent_id:agent.clone(),action:LocalAction::Approvals,input:None,binding_id:Some(binding.clone()),requester:None,
            }).await;
            Cx::post_action(ApprovalsCompleted {identity,request,agent,binding,room,result});
        });
    }
    fn accepts_approval_poll(&self,cx:&mut Cx,done:&ApprovalsCompleted)->bool {
        self.approval_view_open(cx) && self.approval_poll==Some(done.request) && done.identity==self.identity
            && self.approval_scope(cx)==Some((done.agent.clone(),done.binding.clone(),done.room.clone()))
    }
    fn apply_approvals(&mut self,cx:&mut Cx,value:&Value) {
        let selected=self.selected(cx,ids!(approvals),&self.approvals).map(|v|string(&v,"proposalId").to_owned());
        let scope=self.approval_scope(cx);
        self.approvals=array(value,"approvals").into_iter().filter(|row|scope.as_ref().is_some_and(|(a,b,r)|super::approvals::matches(row,a,b,r,super::approvals::now()))).collect();
        self.approval_checked=true;
        let new=self.approval_notices.new_count(&self.approvals);
        if new>0 {
            crate::shared::popup_list::enqueue_popup_notification(
                crate::i18n::format("{count} new approvals in this Room. Review them in Service.",&[("count",new.to_string())]),
                crate::shared::popup_list::PopupKind::Warning,Some(8.0));
        }
        self.view.drop_down(cx,ids!(approvals)).set_labels(cx,std::iter::once(crate::i18n::tr("Select a pending approval").to_owned()).chain(self.approvals.iter().map(|v|format!("{} · {}",string(&v["proposal"],"tool"),string(&v["proposal"]["scope"],"requester")))).collect());
        let index=selected.and_then(|id|self.approvals.iter().position(|v|string(v,"proposalId")==id)).map(|i|i+1).unwrap_or(0);
        self.view.drop_down(cx,ids!(approvals)).set_selected_item(cx,index);
        self.view.label(cx,ids!(approval_count)).set_text(cx,&crate::i18n::format("{count} pending approvals in this Room",&[("count",self.approvals.len().to_string())]));
        self.render_approval_details(cx);
        self.sync_navigation(cx);
    }
    fn render_approval_details(&self,cx:&mut Cx) {
        let text=self.selected(cx,ids!(approvals),&self.approvals).map(|p|super::approvals::details(&p,super::approvals::now())).unwrap_or_else(||crate::i18n::tr(if self.approvals.is_empty() {"No pending approvals."} else {"Select a request to review its exact action."}).into());
        self.view.label(cx,ids!(approval_details)).set_text(cx,&text);
    }
    fn reset_detail_scroll(&mut self,cx:&mut Cx) {
        self.detail_scroll_key.clear();self.view.view(cx,ids!(details_scroll)).set_scroll_pos(cx,dvec2(0.0,0.0));
    }
    fn enter_detail(&mut self,cx:&mut Cx,kind:&str,id:&str) {
        let key=format!("{}:{kind}:{id}",self.identity);
        if self.detail_scroll_key!=key {
            self.view.view(cx,ids!(details_scroll)).set_scroll_pos(cx,dvec2(0.0,0.0));self.detail_scroll_key=key;
        }
    }
    fn refresh_runtime_list(&mut self,cx:&mut Cx) {
        if self.runtime_list_loading || !self.active || self.page!=Page::Agents || !self.view.view(cx,ids!(agent_list_holder)).visible() {return;}
        let Ok(service)=Service::for_current() else {return;};
        if service.identity()!=self.identity {return;}
        let agents=self.agents.iter().filter(|a| agent_device_state(a,&self.current_device_id,&self.execution_devices,self.devices_loaded)==AgentDeviceState::Here).map(|a|string(a,"id").to_owned()).collect::<Vec<_>>();
        self.runtime_list_request=nonce(); let request=self.runtime_list_request; let identity=self.identity.clone();
        self.runtime_list_loading=true;self.view.redraw(cx);
        cx.stop_timer(self.runtime_list_timer);
        spawn_async_task(async move {
            let states=futures::future::join_all(agents.into_iter().map(|agent|{let service=service.clone(); async move {
                let result=tokio::time::timeout(std::time::Duration::from_secs(15),service.execute(Command::Local {agent_id:agent.clone(),action:LocalAction::AgentServiceStatus,input:None,binding_id:None,requester:None})).await.unwrap_or_else(|_|Err("runtime_status_unavailable".into()));
                (agent,result)
            }})).await;
            Cx::post_action(RuntimeListCompleted {identity,request,states});
        });
    }
    fn local_agent(&self, cx: &mut Cx) -> bool {
        self.agent(cx).as_deref() == Some(string(&self.execution_agent_record, "id"))
            && agent_device_state(&self.execution_agent_record, &self.current_device_id, &self.execution_devices, self.devices_loaded) == AgentDeviceState::Here
    }
    fn remote_agent(&self, cx: &mut Cx) -> bool {
        self.agent(cx).as_deref() == Some(string(&self.execution_agent_record, "id"))
            && matches!(string(&self.execution_agent_record,"state"),"active"|"suspended"|"creating")
            && agent_device_state(&self.execution_agent_record, &self.current_device_id, &self.execution_devices, self.devices_loaded) == AgentDeviceState::Other
    }
    fn configuration_online(&self) -> bool {
        self.configuration_metadata_error.is_empty() && configuration_matches(&self.configuration_status, &self.execution_agent_record) && self.configuration_status["online"] == true
    }
    fn clear_configuration(&mut self, cx: &mut Cx) {
        self.configuration_status = Value::Null;
        self.configuration_environment = Value::Null;
        self.configuration_discover = false;
        self.configuration_poll_request=None;
        self.configuration_feedback.clear();
        self.configuration_metadata_error.clear();
        cx.stop_timer(self.configuration_timer);
        self.provider_models.clear(); self.runtime_efforts.clear(); self.credential_ref.clear();
        self.provider_authenticated = false; self.provider_url = None; self.policy = Value::Null;
        self.render_models(cx);
        self.view.text_input(cx,ids!(workspace)).set_text(cx, "");
        self.view.label(cx,ids!(provider_status)).set_text(cx, "");
        self.view.label(cx,ids!(policy_usage)).set_text(cx, "");
    }
    fn finish_configuration_intent(&mut self, digest: Option<&String>, result: &Result<Value, String>) {
        if let Some(digest)=digest && result.as_ref().err().is_none_or(|error|!configuration_outcome_unconfirmed(error)) {
            self.configuration_keys.remove(digest);
        }
        match result {
            Ok(_) if digest.is_some()=>self.configuration_feedback=crate::i18n::tr("Settings saved on the execution device.").into(),
            Err(error)=> {
                if digest.is_some() {self.configuration_feedback=visible_error(error);}
                else {self.configuration_metadata_error=visible_error(error);}
                if configuration_connection_lost(error) {
                    self.configuration_status["online"]=false.into();
                    self.configuration_status["cached"]=true.into();
                }
            }
            Ok(_)=>self.configuration_metadata_error.clear(),
        }
    }
    fn load_configuration(&mut self, cx: &mut Cx) {
        let Some(agent_id) = self.agent(cx) else {return;};
        self.configuration_poll_request=None;
        self.configuration_discover = true;
        self.run(cx,Target::ConfigurationStatus,Command::AgentConfigurationStatus {agent_id});
    }
    fn poll_configuration_status(&mut self, cx: &mut Cx) {
        if self.busy || self.configuration_poll_request.is_some() || !self.remote_agent(cx) {self.configuration_timer=cx.start_timeout(3.0);return;}
        let Some(agent)=self.agent(cx) else {return;};
        let Ok(service)=Service::for_current() else {return;};
        if service.identity()!=self.identity {return;}
        let request=nonce();self.configuration_poll_request=Some(request);
        let identity=self.identity.clone();
        let generation=self.execution_agent_record["generation"].as_i64().unwrap_or(0);
        let device=string(&self.execution_agent_record,"executionDeviceId").to_owned();
        spawn_async_task(async move {
            let result=service.execute(Command::AgentConfigurationStatus {agent_id:agent.clone()}).await;
            Cx::post_action(ConfigurationCompleted {identity,request,agent,generation,device,result});
        });
    }
    fn render_configuration(&self, cx: &mut Cx) {
        let remote = self.remote_agent(cx);
        self.view.label(cx,ids!(configuration_notice)).set_visible(cx, remote);
        self.view.button(cx,ids!(configuration_refresh)).set_visible(cx,remote);
        if !remote {self.view.label(cx,ids!(workspace_label)).set_text(cx,crate::i18n::tr("Agent workspace on this device"));return;}
        let state = if !self.configuration_metadata_error.is_empty() {self.configuration_metadata_error.as_str()}
            else if !configuration_matches(&self.configuration_status,&self.execution_agent_record) {crate::i18n::tr("Checking execution device environment…")}
            else if self.configuration_status["online"] == true {crate::i18n::tr("Execution device online")}
            else {crate::i18n::tr("Execution device offline. Cached settings are read-only.")};
        let freshness = if self.configuration_status["cached"] == true {crate::i18n::tr("Cached observation")} else {crate::i18n::tr("Current observation")};
        let timestamp = |field:&str| self.configuration_status[field].as_i64()
            .and_then(|ms|chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms))
            .map(|at|at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M:%S %:z").to_string())
            .unwrap_or_else(||crate::i18n::tr("Not observed yet").into());
        let text=crate::i18n::format("{device} · {state}\n{freshness}: {observed} · Last heartbeat: {heartbeat}", &[
            ("device",self.device_badge(&self.execution_agent_record)),("state",state.into()),("freshness",freshness.into()),
            ("observed",timestamp("observedAtMs")),("heartbeat",timestamp("lastSeenAtMs"))]);
        let text=if self.configuration_feedback.is_empty() {text} else {format!("{text}\n{}",self.configuration_feedback)};
        self.view.label(cx,ids!(configuration_notice)).set_text(cx,&text);
        self.view.label(cx,ids!(workspace_label)).set_text(cx,&crate::i18n::format("Agent workspace on {device}",&[("device",self.device_badge(&self.execution_agent_record))]));
    }
    fn device_badge(&self, agent: &Value) -> String {
        let state = agent_device_state(agent, &self.current_device_id, &self.execution_devices, self.devices_loaded);
        let label = crate::i18n::tr(match state {
            AgentDeviceState::Here => "This device",
            AgentDeviceState::Other => "Another device",
            AgentDeviceState::Unassigned => "Not assigned",
            AgentDeviceState::Unavailable => "Device unavailable",
            AgentDeviceState::Checking => "Checking execution device…",
        });
        if state == AgentDeviceState::Other {
            let name = self.execution_devices.iter().find(|d| string(d,"id") == string(agent,"executionDeviceId")).map(|d|string(d,"name")).unwrap_or("");
            if !name.is_empty() {return format!("{label} · {name}");}
        }
        label.to_owned()
    }
    fn load_runtime_resources(&mut self, cx: &mut Cx) {
        if !self.local_agent(cx) {self.status(cx, "This Agent is not handled on this device. Assign it here before configuring local resources.");return;}
        self.runtime_resources_loading = true;
        self.runtime_resources_checked = false;
        self.runtime_efforts.clear();
        self.policy = Value::Null;
        self.view.drop_down(cx, ids!(effort)).set_labels(cx, vec!["Checking model choices…".into()]);
        self.view.label(cx, ids!(runtime_status)).set_text(cx, crate::i18n::tr("Checking the saved model and connected local account…"));
        self.local(cx, Target::Policy, LocalAction::AgentModel, None);
    }
    fn cancel_read_wait(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.read_timer);
        self.read_timer = Timer::default();
        self.read_wait = None;
    }
    fn arm_read_wait(&mut self, cx: &mut Cx, command: &Command) {
        self.cancel_read_wait(cx);
        if super::backend::management_read(command) {
            self.read_wait = Some(ReadWait {
                identity: self.identity.clone(), request: self.request,
                deadline: std::time::Instant::now() + MANAGEMENT_READ_WAIT,
            });
            self.read_timer = cx.start_timeout(MANAGEMENT_READ_WAIT.as_secs_f64());
        }
    }
    fn expire_read_wait(&mut self, cx: &mut Cx, now: std::time::Instant) -> bool {
        let Some(wait) = &self.read_wait else { return false; };
        if now < wait.deadline { return false; }
        let current = self.active && self.busy && wait.identity == self.identity && wait.request == self.request;
        self.cancel_read_wait(cx);
        if !current { return false; }
        // Do not cancel the account-scoped task: first-read authorization may
        // already have side effects. Only stop accepting its old UI response.
        self.request = nonce();
        self.agents_loading = false;
        if self.runtime_resources_loading {self.runtime_resources_loading=false;self.runtime_resources_checked=false;}
        self.set_busy(cx, false);
        self.status(cx, crate::i18n::tr(user_error("management_request_unavailable")));
        true
    }
    fn cancel_direct_retry(&mut self, cx:&mut Cx) {
        cx.stop_timer(self.direct_timer);
        self.direct_timer = Timer::default();
        self.direct_retry = None;
    }
    fn schedule_direct_retry(&mut self, cx:&mut Cx) {
        cx.stop_timer(self.direct_timer);
        let safe = self.direct_state["state"] == "pending"
            && matches!(string(&self.direct_state,"phase"),"agent_provisioning"|"waiting_identity"|"server_joining"|"pending")
            && self.direct_state["lastError"].as_str().is_none();
        if !safe || self.direct_agent.is_empty() {self.cancel_direct_retry(cx);return;}
        let attempts = self.direct_retry.as_ref().filter(|(identity,agent,_)| identity == &self.identity && agent == &self.direct_agent).map_or(0,|(_,_,n)|*n);
        if attempts >= 8 {self.cancel_direct_retry(cx);return;}
        self.direct_retry=Some((self.identity.clone(),self.direct_agent.clone(),attempts+1));
        self.direct_timer=cx.start_timeout(2.0_f64.powi(i32::from(attempts.min(3))));
    }
    fn render_models(&mut self, cx:&mut Cx) {
        let codex = self.provider_runtime(cx)=="codex";
        self.view.view(cx,ids!(web_search_holder)).set_visible(cx,codex);
        let configured = string(&self.policy["modelProfile"],"model");
        let previous = self.selected(cx,ids!(model),&self.provider_models).map(|m|string(&m,"model").to_owned());
        let selected = self.provider_models.iter().position(|m| string(m,"model") == configured && (string(m,"credentialRef").is_empty() || string(m,"credentialRef") == string(&self.policy["modelProfile"],"credential_ref")))
            .or_else(|| previous.as_deref().and_then(|id|self.provider_models.iter().position(|m|string(m,"model")==id)))
            .or_else(||self.provider_models.iter().position(|m|m["isDefault"] == true));
        let labels=std::iter::once("Select a model or profile".to_owned()).chain(self.provider_models.iter().map(|m|format!("{}{}",string(m,"displayName"),if m["isDefault"] == true {" (default)"} else {""}))).collect();
        self.view.drop_down(cx,ids!(model)).set_labels(cx,labels);
        self.view.drop_down(cx,ids!(model)).set_selected_item(cx,selected.map_or(0,|i|i+1));
        self.render_efforts(cx);
    }
    fn render_efforts(&mut self, cx:&mut Cx) {
        let model=self.selected(cx,ids!(model),&self.provider_models);
        self.runtime_efforts=model.as_ref().map(|m|array(m,"supportedReasoningEfforts").iter().filter_map(|r|r["reasoningEffort"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
        let default=model.as_ref().map(|m|string(m,"defaultReasoningEffort")).unwrap_or("");
        let saved = if model.as_ref().is_some_and(|m|m["model"] == self.policy["modelProfile"]["model"]) {string(&self.policy["modelProfile"],"reasoning_effort")} else {""};
        let selected=self.runtime_efforts.iter().position(|s|s==saved).or_else(||self.runtime_efforts.iter().position(|s|s==default)).unwrap_or(0);
        let octos = self.provider_runtime(cx)=="octos";
        self.view.drop_down(cx,ids!(effort)).set_labels(cx,if self.runtime_efforts.is_empty(){vec![crate::i18n::tr("Load this model's reasoning choices").into()]}else{self.runtime_efforts.iter().map(|effort|format!("{}{}",if octos {crate::i18n::tr("Use profile settings")} else {effort},if effort==default {crate::i18n::tr(" (default)")} else {""})).collect()});
        self.view.drop_down(cx,ids!(effort)).set_selected_item(cx,selected);
        self.render_effort_note(cx);
        self.view.label(cx,ids!(model_note)).set_text(cx,model.as_ref().map(|m|string(m,"description")).unwrap_or_else(||crate::i18n::tr("Connect a local runtime and refresh its models or profiles. Availability is confirmed when an actual request runs.")));
    }
    fn render_effort_note(&self, cx:&mut Cx) {
        let effort=self.runtime_efforts.get(self.view.drop_down(cx,ids!(effort)).selected_item());
        let model=self.selected(cx,ids!(model),&self.provider_models);
        let description=model.as_ref().and_then(|m|array(m,"supportedReasoningEfforts").into_iter().find(|e|e["reasoningEffort"].as_str()==effort.map(String::as_str))).map(|e|string(&e,"description").to_owned()).unwrap_or_default();
        self.view.label(cx,ids!(effort_note)).set_text(cx,&description);
        self.view.label(cx,ids!(runtime_model_settings)).set_text(cx,&format!("{} · {}",string(&self.policy["modelProfile"],"model"),effort.map(String::as_str).unwrap_or("")));
    }
    fn render_room_advanced(&self, cx: &mut Cx) {
        self.view.view(cx, ids!(room_advanced_content)).set_visible(cx, self.room_advanced);
        let mut toggle = self.view.button(cx, ids!(room_advanced_toggle));
        if self.room_advanced {script_apply_eval!(cx, toggle, {draw_icon.svg: mod.widgets.ICON_CHEVRON_DOWN});}
        else {script_apply_eval!(cx, toggle, {draw_icon.svg: mod.widgets.ICON_CHEVRON_RIGHT});}
    }
    fn render_service_advanced(&self, cx:&mut Cx) {
        self.view.view(cx,ids!(service_advanced)).set_visible(cx,self.service_advanced);
        let mut toggle=self.view.button(cx,ids!(service_advanced_toggle));
        if self.service_advanced {script_apply_eval!(cx,toggle,{draw_icon.svg: mod.widgets.ICON_CHEVRON_DOWN});}
        else {script_apply_eval!(cx,toggle,{draw_icon.svg: mod.widgets.ICON_CHEVRON_RIGHT});}
    }
    fn render_model_advanced(&self, cx:&mut Cx) {
        self.view.view(cx,ids!(advanced_settings)).set_visible(cx,self.model_advanced);
        self.view.view(cx,ids!(provider_controls)).set_visible(cx,!self.provider_authenticated);
        let mut toggle=self.view.button(cx,ids!(advanced_toggle));
        if self.model_advanced {script_apply_eval!(cx,toggle,{draw_icon.svg: mod.widgets.ICON_CHEVRON_DOWN});}
        else {script_apply_eval!(cx,toggle,{draw_icon.svg: mod.widgets.ICON_CHEVRON_RIGHT});}
    }
    fn status(&self, cx: &mut Cx, message: &str) {
        let loading = message == "Loading…";
        self.view.widget(cx, ids!(loading_status)).set_visible(cx, loading);
        let mut label=self.view.label(cx,ids!(status));
        let text=crate::i18n::tr(message).to_owned();
        let source=message.to_owned();
        script_apply_eval!(cx,label,{text:#(text) i18n_text:#(source)});
        self.view.view(cx, ids!(status_holder)).set_visible(cx, !message.is_empty() && !loading);
        self.view
            .label(cx, ids!(status))
            .set_visible(cx, !message.is_empty());
    }
    fn input(&self, cx: &mut Cx, id: &[LiveId]) -> String {
        self.view.text_input(cx, id).text().trim().to_owned()
    }
    fn labels(&self, cx: &mut Cx, id: &[LiveId], prompt: &str, rows: &[Value], key: &str) {
        self.view.drop_down(cx, id).set_labels(
            cx,
            std::iter::once(crate::i18n::tr(prompt).to_owned())
                .chain(rows.iter().map(|v| {
                    let name = string(v, "name");
                    let name = if name.is_empty() {
                        string(v, "displayName")
                    } else {
                        name
                    };
                    let identity = string(v, key);
                    if name.is_empty() {
                        identity.into()
                    } else {
                        format!("{name} · {identity}")
                    }
                }))
                .collect(),
        );
        self.view.drop_down(cx, id).set_selected_item(cx, 0);
    }
    fn selected(&self, cx: &mut Cx, id: &[LiveId], rows: &[Value]) -> Option<Value> {
        self.view
            .drop_down(cx, id)
            .selected_item()
            .checked_sub(1)
            .and_then(|i| rows.get(i))
            .cloned()
    }
    fn project(&self, cx: &mut Cx) -> Option<String> {
        self.selected(cx, ids!(projects), &self.projects)
            .map(|v| string(&v, "id").into())
    }
    fn agent(&self, cx: &mut Cx) -> Option<String> {
        self.selected(cx, ids!(agents), &self.agents)
            .map(|v| string(&v, "id").into())
    }
    fn binding(&self, cx: &mut Cx) -> Option<String> {
        self.selected(cx, ids!(bindings), &self.bindings)
            .map(|v| string(&v, "id").into())
    }
    fn binding_label(&self, cx:&mut Cx, binding:&Value)->String {
        if string(binding,"scopeKind") == "owner_direct" {return crate::i18n::tr("Private chat with this Agent").into();}
        let id=string(binding,"roomId");
        let room=matrix_sdk::ruma::RoomId::parse(id).ok();
        let name=room.as_ref().and_then(|room| crate::sliding_sync::get_client().and_then(|client|client.get_room(room)))
            .and_then(|room|room.cached_display_name().or_else(||room.name().map(matrix_sdk::RoomDisplayName::Named)))
            .map(|name|name.to_string())
            .or_else(||room.as_ref().and_then(|room| {
                if cx.has_global::<crate::home::rooms_list::RoomsListRef>() {cx.global::<crate::home::rooms_list::RoomsListRef>().get_room_name(room).map(|name|name.to_string())} else {None}
            }));
        name.filter(|name|!name.is_empty()).unwrap_or_else(||id.into())
    }
    fn label_bindings(&self,cx:&mut Cx,id:&[LiveId]) {
        let labels=std::iter::once(crate::i18n::tr("Select a Room").into()).chain(self.bindings.iter().map(|binding|self.binding_label(cx,binding))).collect();
        self.view.drop_down(cx,id).set_labels(cx,labels);
        self.view.drop_down(cx,id).set_selected_item(cx,0);
    }
    fn select_policy_room(&mut self, cx: &mut Cx, index: usize) {
        self.view.drop_down(cx, ids!(bindings)).set_selected_item(cx, index);
        self.view.drop_down(cx, ids!(policy_binding)).set_selected_item(cx, index);
        self.policy = Value::Null;
        self.runtime_room_active=false;self.agent_service_state=Value::Null;self.service_row_targets.clear();self.runtime_pending_action=None;self.runtime_action_error.clear();
        self.clear_approval_state(cx);
        self.approvals.clear();
        self.labels(cx,ids!(approvals),"Select a pending approval",&self.approvals,"proposalId");
        self.view.label(cx,ids!(approval_details)).set_text(cx,"");
        self.view.label(cx, ids!(runtime_status)).set_text(cx,crate::i18n::tr("Refresh status to check this Room."));
        self.view.label(cx, ids!(policy_usage)).set_text(cx, "");
    }
    fn set_busy(&mut self, cx: &mut Cx, busy: bool) {
        self.busy = busy;
        for id in [
            ids!(back),
            ids!(setup_tab),
            ids!(policy_tab),
            ids!(model_tab),
            ids!(runtime_tab),
            ids!(projects),
            ids!(rooms),
            ids!(agents),
            ids!(bindings),
            ids!(policy_binding),
            ids!(agent_project),
            ids!(agent_room),
            ids!(creation_source),
            ids!(candidates),
            ids!(policy_layer),
            ids!(limit_mode),
            ids!(period),
            ids!(requests),
            ids!(tool_risk),
            ids!(approvals),
            ids!(agent_pending),
            ids!(assignment_confirm),
            ids!(assign_current_device),
            ids!(requester),
            ids!(creation_name),
            ids!(existing_room),
            ids!(agent_name),
            ids!(token_limit),
            ids!(allowed_tools),
            ids!(allowed_directories),
            ids!(model),
            ids!(workspace),
            ids!(prepare_workspace),
            ids!(refresh_models),
            ids!(effort),
            ids!(found_room),
            ids!(takeover),
            ids!(host_files),
            ids!(shared_workspace),
        ] {
            self.view.widget(cx, id).set_disabled(cx, busy);
        }
    }
    fn list_page(&self, cx: &mut Cx) {
        self.view.label(cx, ids!(heading)).set_text(
            cx,
            crate::i18n::tr(if self.page == Page::Projects {"Agent spaces"} else {"Local agents"}),
        );
        self.view
            .view(cx, ids!(project_list_holder))
            .set_visible(cx, true);
        self.view
            .view(cx, ids!(project_detail))
            .set_visible(cx, false);
        self.view
            .view(cx, ids!(project_form))
            .set_visible(cx, false);
        self.view
            .view(cx, ids!(agent_list_holder))
            .set_visible(cx, true);
        self.view
            .view(cx, ids!(agent_detail))
            .set_visible(cx, false);
        self.view
            .view(cx, ids!(agent_create))
            .set_visible(cx, false);
    }
    fn project_detail(&mut self, cx: &mut Cx, index: usize) {
        let Some(p) = self.projects.get(index).cloned() else {
            return;
        };
        self.enter_detail(cx,"project",string(&p,"id"));
        let mut heading=self.view.label(cx,ids!(heading));script_apply_eval!(cx,heading,{i18n_text:""});
        self.view
            .label(cx, ids!(heading))
            .set_text(cx, string(&p, "name"));
        let requested = self.wanted_project_section.as_ref()
            .filter(|(project, _)| project == string(&p, "id"))
            .map(|(_, section)| *section);
        self.project_section(cx, match requested {
            Some(ProjectDetailSection::Rooms) => ids!(rooms_section),
            Some(ProjectDetailSection::Members) => ids!(members_section),
            _ => ids!(overview_section),
        });
        self.clear_members(cx);
        self.scope_state = Value::Null;
        self.view
            .drop_down(cx, ids!(projects))
            .set_selected_item(cx, index + 1);
        self.view
            .view(cx, ids!(project_list_holder))
            .set_visible(cx, false);
        self.view
            .view(cx, ids!(project_form))
            .set_visible(cx, false);
        self.view
            .view(cx, ids!(project_detail))
            .set_visible(cx, true);
        self.view.label(cx, ids!(project_details)).set_text(
            cx,
            &format!(
                "{}\n{}",
                string(&p, "spaceId"),
                string(&p, "topic")
            ),
        );
        self.rooms.clear();
        self.labels(cx, ids!(rooms), "Select a Room", &self.rooms, "roomId");
        self.view.label(cx, ids!(room_details)).set_text(cx, "");
        self.run(
            cx,
            Target::Rooms,
            Command::ProjectRooms {
                project_id: string(&p, "id").into(),
            },
        );
    }
    fn agent_detail(&mut self, cx: &mut Cx, index: usize) {
        let Some(a) = self.agents.get(index).cloned() else {
            return;
        };
        self.enter_detail(cx,"agent",string(&a,"id"));
        self.view.view(cx,ids!(details_scroll)).set_scroll_pos(cx,dvec2(0.0,0.0));
        self.model_advanced=false;
        self.service_advanced=false;
        self.room_advanced=false;
        self.render_room_advanced(cx);
        self.runtime_room_active=false;self.agent_service_state=Value::Null;self.service_row_targets.clear();self.runtime_pending_action=None;self.runtime_action_error.clear();
        self.render_service_advanced(cx);
        self.render_model_advanced(cx);
        let mut heading=self.view.label(cx,ids!(heading));script_apply_eval!(cx,heading,{i18n_text:""});
        self.view
            .label(cx, ids!(heading))
            .set_text(cx, string(&a, "displayName"));
        self.view
            .drop_down(cx, ids!(agents))
            .set_selected_item(cx, index + 1);
        self.view
            .view(cx, ids!(agent_list_holder))
            .set_visible(cx, false);
        self.view
            .view(cx, ids!(agent_create))
            .set_visible(cx, false);
        self.view.view(cx, ids!(agent_detail)).set_visible(cx, true);
        self.section(cx, ids!(setup_section));
        self.view
            .drop_down(cx, ids!(policy_layer))
            .set_selected_item(cx, 0);
        for id in [
            ids!(retire_confirm),
            ids!(leave_confirm),
            ids!(takeover),
            ids!(host_files),
            ids!(shared_workspace),
        ] {
            self.view
                .check_box(cx, id)
                .set_active(cx, false, Animate::No);
        }
        self.policy = Value::Null;
        self.runtime_efforts.clear();
        self.runtime_resources_checked = false;
        self.runtime_resources_loading = false;
        self.clear_configuration(cx);
        self.execution_agent_record = Value::Null;

        if self.direct_agent != string(&a,"id") {self.cancel_direct_retry(cx);}
        self.render_owner_direct(cx);
        self.view.view(cx, ids!(execution_badge)).label(cx, ids!(device_label)).set_text(cx, crate::i18n::tr("Checking execution device…"));
        self.view.check_box(cx, ids!(assignment_confirm)).set_active(cx, false, Animate::No);
        self.clear_approval_state(cx);
        self.approvals.clear();
        self.labels(
            cx,
            ids!(approvals),
            "Select a pending approval",
            &[],
            "proposalId",
        );
        self.view.label(cx, ids!(approval_details)).set_text(cx, "");
        self.view
            .label(cx, ids!(runtime_status))
            .set_text(cx, crate::i18n::tr("Select a Room and refresh its runtime status."));
        self.view.label(cx, ids!(room_roster)).set_text(cx, "");
        self.bindings.clear();
        self.view.label(cx, ids!(agent_details)).set_text(
            cx,
            &format!(
                "{}\n{}",
                string(&a, "state"),
                string(&a, "puppetMxid")
            ),
        );
        self.run(
            cx,
            Target::AgentDetails,
            Command::AgentDetails {agent_id: string(&a, "id").into()},
        );
    }
    fn render_owner_direct(&self,cx:&mut Cx) {
        let exact=self.agent(cx).is_some_and(|a|a == self.direct_agent);
        let state=if exact {string(&self.direct_state,"state")} else {""};
        let room=string(&self.direct_state,"roomId");
        let encrypted=state == "active" && (self.direct_encrypted.contains(room) || matrix_sdk::ruma::OwnedRoomId::try_from(room).ok()
            .and_then(|id|crate::sliding_sync::get_client()?.get_room(&id))
            .is_some_and(|room|room.encryption_state().is_encrypted()));
        let encrypting=!room.is_empty() && self.direct_encrypting.as_deref()==Some(room);
        self.view.label(cx,ids!(owner_direct_status)).set_text(cx,match state {
            "active" if encrypted=>crate::i18n::tr("Your private chat with this Agent is ready and end-to-end encrypted. Matrix cannot turn encryption off again."),
            "active"=>"Your private chat with this Agent is ready.",
            "pending"=>"Your Agent exists. Private chat setup is still pending; retry its original setup when connected.",
            _=>"Set up a private chat with this Agent. Retrying preserves the original setup request.",
        });
        self.view.button(cx,ids!(open_owner_direct)).set_enabled(cx,exact && state == "active" && !string(&self.direct_state,"roomId").is_empty() && !self.busy);
        // Matrix has no way to turn a room's encryption off: on stays on.
        self.view.check_box(cx,ids!(encrypt_owner_direct)).set_active(cx,encrypted || encrypting,Animate::No);
        self.view.widget(cx,ids!(encrypt_owner_direct)).set_disabled(cx,!(exact && state == "active" && !encrypted && !encrypting && !room.is_empty() && !self.busy));
    }
    fn project_section(&self, cx: &mut Cx, selected: &[LiveId]) {
        for id in [
            ids!(overview_section),
            ids!(rooms_section),
            ids!(members_section),
            ids!(permissions_section),
        ] {
            self.view.view(cx, id).set_visible(cx, id == selected);
        }
        for (tab, section) in [
            (ids!(overview_tab), ids!(overview_section)),
            (ids!(rooms_tab), ids!(rooms_section)),
            (ids!(members_tab), ids!(members_section)),
            (ids!(permissions_tab), ids!(permissions_section)),
        ] {
            let mut button = self.view.button(cx, tab);
            if section == selected {
                script_apply_eval!(cx, button, { draw_bg +: {color: mod.widgets.RINX_ACCENT color_hover: mod.widgets.RINX_ACCENT_HOVER color_down: mod.widgets.RINX_ACCENT_DOWN}
                    draw_text +: {color: mod.widgets.RINX_ON_ACCENT color_hover: mod.widgets.RINX_ON_ACCENT color_down: mod.widgets.RINX_ON_ACCENT} });
            } else {
                script_apply_eval!(cx, button, { draw_bg +: {color: mod.widgets.RINX_SURFACE color_hover: mod.widgets.RINX_HOVER color_down: mod.widgets.RINX_PRESSED}
                    draw_text +: {color: mod.widgets.RINX_INK color_hover: mod.widgets.RINX_INK color_down: mod.widgets.RINX_INK} });
            }
        }
    }
    fn clear_members(&mut self, cx: &mut Cx) {
        self.members.clear();
        self.members_room = None;
        self.view.label(cx, ids!(members_status)).set_text(cx, "");
        self.view
            .button(cx, ids!(invite_member))
            .set_enabled(cx, false);
        self.view
            .button(cx, ids!(invite_member))
            .set_disabled(cx, true);
    }
    fn show_members(&mut self, cx: &mut Cx, room: String, space: bool) {
        if self.busy || !self.active {
            return;
        }
        let service = match Service::for_current() {
            Ok(service) if service.identity() == self.identity => service,
            _ => {
                self.status(
                    cx,
                    "Your account changed. Reopen Projects for the current account.",
                );
                return;
            }
        };
        self.clear_members(cx);
        self.members_room = Some((room.clone(), space));
        self.project_section(cx, ids!(members_section));
        self.view.label(cx, ids!(members_heading)).set_text(
            cx,
            if space {
                "Project members"
            } else {
                "Discussion Room members"
            },
        );
        self.view.label(cx, ids!(members_note)).set_text(cx, if space {
            "Members of this Project's Space. Each discussion Room has its own members and invitations."
        } else { "Only this discussion Room's members and invitations are shown." });
        self.view.button(cx, ids!(invite_member)).set_text(
            cx,
            if space {
                "Invite to Space"
            } else {
                "Invite to Room"
            },
        );
        self.view
            .label(cx, ids!(members_status))
            .set_text(cx, crate::i18n::tr("Loading members…"));
        self.request = nonce();
        let request = self.request;
        let identity = self.identity.clone();
        self.set_busy(cx, true);
        spawn_async_task(async move {
            let result = service.room_members(room, space).await;
            Cx::post_action(Completed {
                identity,
                request,
                target: Target::Members,
                configuration_fence: None,
                configuration_digest: None,
                result,
            });
        });
    }
    fn sync_navigation(&self, cx: &mut Cx) {
        self.render_owner_direct(cx);
        let selected_project_room = self.selected(cx, ids!(rooms), &self.rooms).is_some();
        for id in [ids!(room_members), ids!(open_room), ids!(room_permissions)] {self.view.button(cx, id).set_visible(cx, selected_project_room);}
        self.view.label(cx, ids!(room_details)).set_visible(cx, selected_project_room);
        let binding_form = self.view.button(cx, ids!(bind_agent)).visible();
        self.view.view(cx, ids!(agent_binding_target)).set_visible(cx, binding_form);
        for id in [ids!(create_agent), ids!(bind_agent)] {
            self.view
                .button(cx, id)
                .set_enabled(cx, self.agent_journal_loaded && !self.busy);
        }
        let assigned_here = self.local_agent(cx);
        let remote = self.remote_agent(cx);
        let configurable = assigned_here || (remote && self.configuration_online());
        self.render_configuration(cx);
        self.view.view(cx,ids!(runtime_start_options)).set_visible(cx,!self.runtime_room_active);
        let runtime_known=self.agent_service_state.is_object();
        let mut service_toggle=self.view.button(cx,ids!(service_toggle));
        service_toggle.set_text(cx,crate::i18n::tr(if self.runtime_pending_action==Some(true) {"Starting…"} else if self.runtime_pending_action==Some(false) {"Stopping…"} else if !runtime_known {"Checking…"} else if self.runtime_room_active {"Stop responding"} else {"Start responding"}));
        if self.runtime_room_active {script_apply_eval!(cx,service_toggle,{draw_icon +: {svg: mod.widgets.ICON_SERVICE_STOP}});} else {script_apply_eval!(cx,service_toggle,{draw_icon +: {svg: mod.widgets.ICON_SERVICE_PLAY}});}
        self.view.label(cx,ids!(runtime_restart_note)).set_visible(cx,self.runtime_room_active);
        let approval_selected=self.approval_checked && self.selected(cx,ids!(approvals),&self.approvals).is_some_and(|p| {
            !self.approval_blocked.contains(string(&p,"proposalId")) && self.approval_scope(cx).is_some_and(|(a,b,r)|super::approvals::matches(&p,&a,&b,&r,super::approvals::now()))
        });
        for id in [ids!(approve),ids!(reject)] {let enabled=assigned_here && approval_selected && !self.busy;self.view.button(cx,id).set_enabled(cx,enabled);self.view.widget(cx,id).set_disabled(cx,!enabled);}
        let badge = self.view.widget(cx, ids!(execution_badge));
        render_device_badge(cx, &badge, agent_device_state(&self.execution_agent_record, &self.current_device_id, &self.execution_devices, self.devices_loaded), &self.device_badge(&self.execution_agent_record));
        self.view.view(cx, ids!(local_device_notice)).set_visible(cx, !assigned_here);
        self.view.label(cx, ids!(local_device_message)).set_text(cx, if self.devices_loaded && !self.execution_agent_record.is_null() {
            crate::i18n::tr("Model and Agent limits configure the assigned execution device. Service controls remain on that device.")
        } else {crate::i18n::tr("Checking execution device…")});
        let assignable = self.devices_loaded && self.execution_agent_record["id"].as_str() == self.agent(cx).as_deref() && self.execution_agent_record["generation"].as_i64().is_some_and(|generation| generation > 0) && matches!(string(&self.execution_agent_record, "state"), "creating" | "active" | "suspended") && !self.current_device_id.is_empty() && self.execution_devices.iter().any(|d|string(d,"id") == self.current_device_id && d["revoked"] == false);
        self.view.button(cx, ids!(assign_current_device)).set_enabled(cx, assignable && !assigned_here && !self.busy && self.view.check_box(cx, ids!(assignment_confirm)).active(cx));
        self.view.check_box(cx, ids!(assignment_confirm)).set_visible(cx, assignable && !assigned_here);
        self.view.button(cx, ids!(assign_current_device)).set_visible(cx, assignable && !assigned_here);
        for id in [ids!(model_tab), ids!(policy_tab)] {self.view.button(cx, id).set_enabled(cx, (assigned_here || remote) && !self.busy);}
        self.view.button(cx,ids!(runtime_tab)).set_enabled(cx,assigned_here && !self.busy);
        self.view.drop_down(cx,ids!(policy_layer)).set_disabled(cx,remote || self.busy);
        self.view.button(cx,ids!(reset_policy)).set_visible(cx,!remote);
        self.view.button(cx,ids!(configuration_refresh)).set_enabled(cx,!self.busy);
        if remote {self.view.drop_down(cx,ids!(policy_layer)).set_selected_item(cx,0);}
        for id in [ids!(provider_login),ids!(provider_logout),ids!(provider_cancel),ids!(provider_open),ids!(disconnect_provider)] {
            if remote {self.view.button(cx,id).set_visible(cx,false);}
        }
        for id in [ids!(use_local_provider),ids!(provider_check),ids!(provider_advanced_check),ids!(refresh_models),ids!(prepare_workspace)] {
            self.view.button(cx,id).set_enabled(cx,configurable && !self.busy);
        }
        let runtime=self.provider_runtime(cx);
        self.view.drop_down(cx,ids!(runtime_kind)).set_disabled(cx,self.busy || self.runtime_room_active || (remote && !configurable));
        self.view.label(cx,ids!(runtime_provider_note)).set_text(cx,crate::i18n::tr(if remote {"Connect uses the owner's provider on the execution device and affects that owner's other Agents. Sign-in and credentials stay on that device."} else {match runtime {
            "claude"=>"Claude uses its local sign-in. Sign in through Claude Code before connecting.",
            "octos"=>"Octos runs the selected local profile. Provider credentials stay in Octos; reasoning uses the profile's settings.",
            _=>"Codex uses its local sign-in or a separate account.",
        }}));
        let profile=&self.policy["modelProfile"];
        self.view.label(cx,ids!(shared_workspace_path)).set_text(cx,&format!("{}: {}",crate::i18n::tr("Workspace"),string(profile,"workspace_root")));
        let current_model=self.selected(cx,ids!(model),&self.provider_models);
        let model_ready=profile["model"].as_str().is_some_and(|m|!m.is_empty())
            && current_model.as_ref().is_some_and(|m|m["model"] == profile["model"])
            && !self.credential_ref.is_empty() && string(profile,"credential_ref") == self.selected_credential(cx)
            && (string(profile,"reasoning_effort").is_empty() || self.runtime_efforts.iter().any(|e|e==string(profile,"reasoning_effort")))
            && !self.input(cx,ids!(workspace)).is_empty() && string(profile,"workspace_root") == self.input(cx,ids!(workspace));
        let reason=if self.runtime_resources_loading {"Checking the saved model and connected local account…"} else if self.view.view(cx,ids!(runtime_section)).visible() && !self.runtime_resources_checked {"Model resources could not be verified. Refresh this Service page to retry."} else if !assigned_here {"Assign this Agent to this device in Agent and device."} else if !self.provider_authenticated {"Connect your account in Model & resources."} else if !model_ready || self.runtime_efforts.is_empty() {"Save a model in Model & resources. A private workspace is created automatically."} else {"Ready to start. Actual usage is charged after each task."};
        self.view.label(cx,ids!(start_requirements)).set_text(cx,crate::i18n::tr(reason));
        let start_ready=self.provider_authenticated && model_ready && !self.runtime_efforts.is_empty() && !self.runtime_resources_loading && self.runtime_resources_checked;
        self.view.button(cx,ids!(service_toggle)).set_enabled(cx, assigned_here && runtime_known && !self.busy && (self.runtime_room_active || start_ready));
        let selected_agent = self.selected(cx, ids!(agents), &self.agents);
        let can_retire = selected_agent
            .as_ref()
            .is_some_and(|a| !["retiring", "retired"].contains(&string(a, "state")))
            && self.view.check_box(cx, ids!(retire_confirm)).active(cx);
        self.view
            .button(cx, ids!(retire_agent))
            .set_enabled(cx, can_retire && !self.busy);
        let selected_binding = self.selected(cx, ids!(bindings), &self.bindings);
        let state = selected_binding.as_ref().map(|b| string(b, "state"));
        self.view.button(cx,ids!(open_binding_chat)).set_enabled(cx,selected_binding.as_ref().is_some_and(|b|string(b,"state")=="active" && !string(b,"roomId").is_empty()) && !self.busy);
        let project_binding = selected_binding.as_ref().is_some_and(|b| b["projectId"].as_str().is_some_and(|id| !id.is_empty()) && string(b,"scopeKind") != "owner_direct");
        self.view.view(cx, ids!(reply_policy)).set_visible(cx, project_binding);
        self.view.label(cx, ids!(reply_policy_status)).set_text(cx, crate::i18n::tr("In group chats, every message must @ the Agent. In a one-to-one chat with its owner, no @ is needed."));
        self.view.button(cx, ids!(reply_policy_toggle)).set_visible(cx, false);
        self.view.button(cx, ids!(room_roster_check)).set_visible(cx, project_binding);
        self.view.button(cx, ids!(room_roster_check)).set_enabled(cx, project_binding && !self.busy);
        if selected_binding.as_ref().is_some_and(|b| string(b,"scopeKind") == "owner_direct") {
            self.view.label(cx, ids!(room_roster)).set_text(cx, crate::i18n::tr("This is your private chat with the Agent. Open its chat information to view members."));
        }
        for (id, enabled) in [
            (ids!(pause_binding), state == Some("active")),
            (ids!(resume_binding), state == Some("suspended")),
            (
                ids!(leave_binding),
                state.is_some_and(|s| s != "left" && s != "leaving")
                    && self.view.check_box(cx, ids!(leave_confirm)).active(cx),
            ),
        ] {
            self.view
                .button(cx, id)
                .set_enabled(cx, enabled && !self.busy);
        }
        let layer = self
            .view
            .drop_down(cx, ids!(policy_layer))
            .selected_item()
            .min(2);
        let binding = self.binding(cx);
        let agent = self.agent(cx);
        let requester = self.input(cx, ids!(requester));
        let exact = agent.as_ref().is_some_and(|agent| {
            editor_matches(&self.policy, agent, binding.as_deref(), &requester, layer)
        });
        for id in [ids!(save_policy), ids!(reset_policy)] {
            self.view
                .button(cx, id)
                .set_enabled(cx, exact && configurable && (!remote || id == ids!(save_policy)) && !self.busy);
        }
        let model_exact = agent.as_ref().is_some_and(|agent| {
            string(&self.policy, "agentId") == agent
                && self.policy["scope"] == "agent"
                && self.policy["modelLoaded"] == true
        });
        let model_selected=self.selected(cx,ids!(model),&self.provider_models).is_some();
        self.view.button(cx, ids!(save_model)).set_enabled(cx, configurable && model_exact && self.provider_authenticated && !self.credential_ref.is_empty() && model_selected && !self.runtime_efforts.is_empty() && !self.busy);
        let policy_visible = self.view.view(cx, ids!(policy_section)).visible();
        if !assigned_here && ((!remote && (self.view.view(cx, ids!(model_section)).visible() || self.view.view(cx, ids!(policy_section)).visible())) || self.view.view(cx, ids!(runtime_section)).visible()) {self.section(cx, ids!(setup_section));}
        self.view.label(cx, ids!(runtime_chat_target)).set_text(cx, crate::i18n::tr("Automatically serves all connected Rooms, including Rooms added later."));
        let room_scoped = self.view.view(cx, ids!(setup_section)).visible();
        let setup_visible = self.view.view(cx, ids!(setup_section)).visible();
        self.view.view(cx, ids!(retirement_section)).set_visible(cx, setup_visible);
        self.view.view(cx, ids!(room_service_actions)).set_visible(cx, setup_visible);
        for id in [ids!(room_danger_heading), ids!(leave_confirm), ids!(leave_binding), ids!(pause_binding), ids!(resume_binding)] {
            self.view.widget(cx, id).set_visible(cx, setup_visible);
        }
        self.view.view(cx, ids!(policy_room_holder)).set_visible(cx, policy_visible && layer != 0);
        let binding_index = self.view.drop_down(cx, ids!(bindings)).selected_item();
        self.view.drop_down(cx, ids!(policy_binding)).set_selected_item(cx, binding_index);
        let target = if layer == 0 {
            "Agent total applies across all Rooms.".to_owned()
        } else if let Some(binding) = self.selected(cx, ids!(bindings), &self.bindings) {
            let room = self.binding_label(cx,&binding);
            if layer == 2 { format!("{} · {}", room, self.input(cx, ids!(requester))) } else { room.to_owned() }
        } else { "Select a Room binding".to_owned() };
        self.view.label(cx, ids!(policy_target)).set_text(cx, crate::i18n::tr(&target));
        self.view
            .view(cx, ids!(room_controls))
            .set_visible(cx, room_scoped);
        self.view
            .view(cx, ids!(requester_holder))
            .set_visible(cx, policy_visible && layer == 2);
        let list_visible = !self.view.view(cx,ids!(agent_detail)).visible() && !self.view.view(cx,ids!(agent_create)).visible();
        self.view.view(cx,ids!(agent_recovery)).set_visible(cx, self.page == Page::Agents && list_visible && !self.agent_pending_commands.is_empty());
        let detail = match self.page {
            Page::Projects => {
                self.view.view(cx, ids!(project_detail)).visible()
                    || self.view.view(cx, ids!(project_form)).visible()
            }
            Page::Agents => {
                self.view.view(cx, ids!(agent_detail)).visible()
                    || self.view.view(cx, ids!(agent_create)).visible()
            }
        };
        if !detail {
            let projects = self.page == Page::Projects;
            for (id, text) in [
                (ids!(context_title), if projects {"Spaces for people and Agents"} else {"Manage agents you run locally"}),
                (ids!(context_description), if projects {"An Agent space is a Matrix Space connected to a Hagency project. The creation wizard handles this setup."} else {"Configure this account's local agents, their models, rooms and execution permissions."}),
                (ids!(context_steps), if projects {"1 · Create an Agent space    2 · Create a room    3 · Invite people and Agents"} else {"1 · Create an agent    2 · Choose a model    3 · Connect rooms"}),
                (ids!(context_scope), if projects {"To use contributed capacity, open Hagency → Resource pools and request an Agent."} else {"To use contributed capacity, open Hagency → Resource pools and request an Agent."}),
            ] { self.view.label(cx,id).set_text(cx,crate::i18n::tr(text)); }
            self.view.view(cx,ids!(context_project_icon)).set_visible(cx,projects);
            self.view.view(cx,ids!(context_agent_icon)).set_visible(cx,!projects);
            self.view.label(cx,ids!(heading)).set_text(cx,crate::i18n::tr(if self.page==Page::Projects {"Agent spaces"} else {"Local agents"}));
            self.view.button(cx,ids!(new_item)).set_text(cx,crate::i18n::tr(if self.page==Page::Projects {"Create a space or room…"} else {"New local agent"}));
        }
        self.view.view(cx,ids!(lists_pane)).set_visible(cx,!detail && !self.display_blocked);
        self.view.view(cx,ids!(details_scroll)).set_visible(cx,detail && !self.display_blocked);
        self.view.view(cx,ids!(project_list_holder)).set_visible(cx,!detail && self.page==Page::Projects);
        self.view.view(cx,ids!(agent_list_holder)).set_visible(cx,!detail && self.page==Page::Agents);
        let members=self.page==Page::Projects && detail && self.view.view(cx,ids!(members_section)).visible();
        for id in [ids!(projects_page),ids!(project_detail),ids!(members_section)] {
            let mut view=self.view.view(cx,id);
            let height=if members {Size::fill()} else {Size::fit()};
            script_apply_eval!(cx,view,{height:#(height)});
        }
        self.view
            .button(cx, ids!(new_item))
            .set_visible(cx, !detail);
        self.view.button(cx, ids!(refresh)).set_visible(cx, !detail);
        self.view.button(cx, ids!(back)).set_visible(cx, detail);
        if self.page == Page::Projects && self.view.view(cx, ids!(project_form)).visible() {
            let title = self.view.label(cx, ids!(creation_heading)).text();
            self.view.label(cx, ids!(heading)).set_text(cx, &title);
        } else if self.page == Page::Agents && self.view.view(cx, ids!(agent_create)).visible() {
            let title = if self.view.button(cx, ids!(bind_agent)).visible() {
                "Add Room to Agent"
            } else {
                "Create local agent"
            };
            self.view
                .label(cx, ids!(agent_creation_heading))
                .set_text(cx, crate::i18n::tr(title));
            self.view.label(cx, ids!(heading)).set_text(cx, crate::i18n::tr(title));
        }
    }
    fn open_card(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        if !self.view.view(cx,ids!(lists_pane)).visible() {return false;}
        let list = self.view.portal_list(
            cx,
            if self.page == Page::Projects {
                ids!(project_list)
            } else {
                ids!(agent_list)
            },
        );
        for (index, row) in list.items_with_actions(actions) {
            if self.page == Page::Agents && !list.was_scrolling() && row.button(cx,ids!(service_control)).clicked(actions) {
                let Some(agent)=self.agents.get(index).cloned() else {return true;};
                if agent_device_state(&agent,&self.current_device_id,&self.execution_devices,self.devices_loaded)!=AgentDeviceState::Here {return true;}
                let id=string(&agent,"id").to_owned();
                if self.agent_card_targets.get(&row.widget_uid()) != Some(&id) {return true;}
                let Some(Ok(status))=self.runtime_list.get(&id) else {return true;};
                let enabled=service_enabled(status);
                self.list_service_pending_agent=id.clone();
                self.run(cx,Target::ListService,Command::Local {agent_id:id,action:if enabled {LocalAction::StopAgentService} else {LocalAction::StartAgentService},input:if enabled {None} else {Some(json!({}))},binding_id:None,requester:None});
                return true;
            }
            if !list.was_scrolling() && row.button(cx, ids!(open)).clicked(actions) {
                if self.page == Page::Projects {
                    self.project_detail(cx, index);
                } else {
                    self.agent_detail(cx, index);
                }
                return true;
            }
        }
        false
    }
    fn back_to_list(&mut self, cx: &mut Cx, actions: &Actions) -> bool {
        if !self.view.button(cx, ids!(back)).clicked(actions) {
            return false;
        }
        self.cancel_direct_retry(cx);
        self.wanted_project = None;
        self.wanted_project_section = None;
        self.wanted_agent = None;
        self.list_page(cx);
        self.refresh_runtime_list(cx);
        self.status(cx, "");
        true
    }
    fn section(&self, cx: &mut Cx, selected: &[LiveId]) {
        if !self.view.view(cx,selected).visible() {self.view.view(cx,ids!(details_scroll)).set_scroll_pos(cx,dvec2(0.0,0.0));}
        for id in [
            ids!(setup_section),
            ids!(policy_section),
            ids!(model_section),
            ids!(runtime_section),
        ] {
            self.view.view(cx, id).set_visible(cx, id == selected);
        }
        for (tab, section) in [
            (ids!(setup_tab), ids!(setup_section)),
            (ids!(policy_tab), ids!(policy_section)),
            (ids!(model_tab), ids!(model_section)),
            (ids!(runtime_tab), ids!(runtime_section)),
        ] {
            let mut button = self.view.button(cx, tab);
            if section == selected {
                script_apply_eval!(cx, button, { draw_bg +: {color: mod.widgets.RINX_ACCENT color_hover: mod.widgets.RINX_ACCENT color_down: mod.widgets.RINX_ACCENT color_focus: mod.widgets.RINX_ACCENT} draw_text +: {color: mod.widgets.RINX_ON_ACCENT color_hover: mod.widgets.RINX_ON_ACCENT color_down: mod.widgets.RINX_ON_ACCENT color_focus: mod.widgets.RINX_ON_ACCENT} });
            } else {
                script_apply_eval!(cx, button, { draw_bg +: {color: mod.widgets.RINX_SURFACE color_hover: mod.widgets.RINX_HOVER color_down: mod.widgets.RINX_PRESSED color_focus: mod.widgets.RINX_SELECTED} draw_text +: {color: mod.widgets.RINX_INK color_hover: mod.widgets.RINX_INK color_down: mod.widgets.RINX_INK color_focus: mod.widgets.RINX_INK} });
            }
        }
    }
    fn begin_creation_form(&mut self,cx:&mut Cx,room:bool)->bool {
        let unresolved=self.creation.as_ref().is_some_and(|v|v["phase"] != "complete") || (self.creation.is_none() && self.pending_input.is_some());
        if unresolved {
            self.view.view(cx,ids!(project_form)).set_visible(cx,true);
            self.view.view(cx,ids!(project_list_holder)).set_visible(cx,false);
            self.view.view(cx,ids!(project_detail)).set_visible(cx,false);
            self.view.view(cx,ids!(recovery)).set_visible(cx,true);
            self.status(cx,"Resume the original unfinished creation before starting another one.");
            return false;
        }
        self.creation=None;self.pending_input=None;self.create_room=room;
        for id in [ids!(creation_name),ids!(found_room),ids!(existing_room)] {self.view.text_input(cx,id).set_text(cx, "");}
        self.view.label(cx,ids!(creation_result)).set_text(cx, "");
        self.view.view(cx,ids!(recovery)).set_visible(cx,false);
        self.view.button(cx,ids!(create_project_room)).set_enabled(cx,!self.busy);
        true
    }
    fn creation_source(&self, cx: &mut Cx) {
        let existing = self
            .view
            .drop_down(cx, ids!(creation_source))
            .selected_item()
            == 1;
        let (title, note) = match (self.create_room, existing) {
            (false, false) => (
                "Create Matrix workspace",
                "Create a new shared Space for this Project.",
            ),
            (false, true) => (
                "Connect existing Space",
                "Select a Space you already belong to.",
            ),
            (true, false) => (
                "Create discussion Room",
                "Create a Room inside this Project. It has its own members.",
            ),
            (true, true) => (
                "Connect existing Room",
                "Connect a Room already linked to this Project's Space.",
            ),
        };
        self.view
            .label(cx, ids!(creation_heading))
            .set_text(cx, crate::i18n::tr(title));
        self.view.label(cx, ids!(creation_note)).set_text(cx, crate::i18n::tr(note));
        let mut name = self.view.text_input(cx, ids!(creation_name));
        if self.create_room {
            let placeholder=crate::i18n::tr("Room name"); script_apply_eval!(cx, name, {empty_text: #(placeholder)});
        } else {
            let placeholder=crate::i18n::tr("Project name"); script_apply_eval!(cx, name, {empty_text: #(placeholder)});
        }
        self.view
            .button(cx, ids!(create_project_room))
            .set_text(cx, crate::i18n::tr(title));
        self.view
            .view(cx, ids!(creation_name_holder))
            .set_visible(cx, !existing);
        self.view
            .view(cx, ids!(candidates_holder))
            .set_visible(cx, existing && !self.create_room);
        self.view
            .button(cx, ids!(candidates_more))
            .set_visible(cx, existing && !self.create_room);
        self.view
            .view(cx, ids!(existing_room_holder))
            .set_visible(cx, existing && self.create_room);
    }
    fn load_permissions(&mut self, cx: &mut Cx, room: bool) {
        let Some(project_id) = self.project(cx) else {
            return;
        };
        let room_id = if room {
            let Some(value) = self.selected(cx, ids!(rooms), &self.rooms) else {
                self.status(cx, "Select a discussion Room first.");
                return;
            };
            Some(string(&value, "roomId").to_owned())
        } else {
            None
        };
        self.scope_state = Value::Null;
        self.view
            .view(cx, ids!(permission_editor))
            .set_visible(cx, false);
        self.project_section(cx, ids!(permissions_section));
        self.run(
            cx,
            Target::ScopeState,
            Command::ScopeServiceState {
                project_id,
                room_id,
            },
        );
    }
    fn set_page(&mut self, cx: &mut Cx, page: Page) {
        self.page = page;
        self.view.label(cx, ids!(heading)).set_text(
            cx,
            crate::i18n::tr(if page == Page::Projects {"Agent spaces"} else {"Local agents"}),
        );
        self.view.button(cx, ids!(new_item)).set_text(
            cx,
            crate::i18n::tr(if page == Page::Projects {"Create a space or room…"} else {"New local agent"}),
        );
        self.view
            .view(cx, ids!(projects_page))
            .set_visible(cx, page == Page::Projects);
        self.view
            .view(cx, ids!(agents_page))
            .set_visible(cx, page == Page::Agents);
        self.view.redraw(cx);
    }
    fn provider_runtime(&self, cx:&mut Cx)->&'static str {
        match self.view.drop_down(cx,ids!(runtime_kind)).selected_item(){1=>"claude",2=>"octos",_=>"codex"}
    }
    fn selected_credential(&self, cx:&mut Cx)->String {
        self.selected(cx,ids!(model),&self.provider_models).and_then(|m|m["credentialRef"].as_str().map(str::to_owned)).unwrap_or_else(||self.credential_ref.clone())
    }
    fn run(&mut self, cx: &mut Cx, target: Target, command: Command) {
        let command=match command {
            Command::Provider {action} if self.provider_runtime(cx)!="codex"=>Command::RuntimeProvider {runtime:self.provider_runtime(cx).into(),action},
            other=>other,
        };
        let list_control = matches!(target,Target::ListService) && matches!(&command,Command::Local {agent_id,action:LocalAction::StartAgentService | LocalAction::StopAgentService,binding_id:None,requester:None,..} if self.agents.iter().any(|a|string(a,"id")==agent_id && agent_device_state(a,&self.current_device_id,&self.execution_devices,self.devices_loaded)==AgentDeviceState::Here));
        let remote = self.remote_agent(cx);
        let mut configuration_fence = None;
        let mut configuration_digest = None;
        let command = if matches!(&command, Command::Local {..} | Command::Provider {..} | Command::RuntimeProvider {..} | Command::PrepareAgentWorkspace {..}) && !self.local_agent(cx) && !list_control {
            if !remote || !remote_configuration_command(&command) {self.status(cx, "This action is available only on the execution device.");return;}
            if !self.configuration_online() {self.status(cx,"Execution device offline. Cached settings are read-only.");return;}
            let agent_id=string(&self.execution_agent_record,"id").to_owned();
            let expected_generation=self.execution_agent_record["generation"].as_i64().unwrap_or(0);
            configuration_fence=Some((agent_id.clone(),expected_generation,string(&self.execution_agent_record,"executionDeviceId").into()));
            let mutation=matches!(&command,Command::Local {action:LocalAction::SaveAgentModel|LocalAction::SaveAgentPolicy,..}|Command::PrepareAgentWorkspace {..}|Command::Provider {action:ProviderAction::UseLocal}|Command::RuntimeProvider {action:ProviderAction::UseLocal,..});
            let idempotency_key=if mutation {
                let digest=format!("{}:{agent_id}:{expected_generation}:{}:{}:{command:?}",self.identity,string(&self.execution_agent_record,"executionDeviceId"),self.configuration_status["deviceGeneration"]);
                configuration_digest=Some(digest.clone());
                self.configuration_keys.entry(digest).or_insert_with(||uuid::Uuid::new_v4().to_string()).clone()
            } else {uuid::Uuid::new_v4().to_string()};
            Command::AgentConfiguration {agent_id,expected_generation,idempotency_key,command:Box::new(command)}
        } else {command};
        if matches!(&command,Command::AgentConfigurationStatus {..}|Command::ExecutionEnvironment {..}) {
            configuration_fence=Some((string(&self.execution_agent_record,"id").into(),self.execution_agent_record["generation"].as_i64().unwrap_or(0),string(&self.execution_agent_record,"executionDeviceId").into()));
        }
        if self.busy || !self.active {
            return;
        }
        let service = match Service::for_current() {
            Ok(s) if s.identity() == self.identity => s,
            _ => {
                self.status(
                    cx,
                    "Matrix account changed. Reopen this panel for the current account.",
                );
                return;
            }
        };
        if matches!(target, Target::Agents) { self.agents_loading = true; }
        self.request = nonce();
        self.arm_read_wait(cx, &command);
        let request = self.request;
        let identity = self.identity.clone();
        if let Command::Local {action:LocalAction::StartAgentService | LocalAction::StopAgentService,..}=&command {
            if matches!(target,Target::Runtime) {
                self.runtime_pending_action=Some(matches!(&command,Command::Local {action:LocalAction::StartAgentService,..}));
                self.runtime_action_error.clear();
                self.view.label(cx,ids!(runtime_status)).set_text(cx,crate::i18n::tr(if self.runtime_pending_action==Some(true) {"Starting service. Discovering connected Rooms…"} else {"Stopping…"}));
            }
        }
        if configuration_digest.is_some() {self.configuration_feedback=crate::i18n::tr("Applying settings on the execution device…").into();}
        self.set_busy(cx, true);
        self.sync_navigation(cx);
        let quiet = matches!(target,Target::ListService | Target::ServiceRoomChanged | Target::ConfigurationStatus) || (matches!(target,Target::Runtime) && self.agent_service_state.is_object());
        if !matches!(target,Target::ConfigurationStatus) {self.status(
            cx,
            if quiet {""} else {match target {
                Target::Policy if remote && configuration_digest.is_some() => "Applying settings on the execution device…",
                Target::Changed => "Saving changes…",
                Target::AgentCreated => "Creating Agent…",
                Target::AgentBound | Target::ProjectAdopted | Target::RoomAdopted => "Connecting…",
                Target::Creation => "Checking creation…",
                _ => "Loading…",
            }},
        );}
        spawn_async_task(async move {
            let result = service.execute(command).await;
            Cx::post_action(Completed {
                identity,
                request,
                target,
                configuration_fence,
                configuration_digest,
                result,
            });
        });
    }
    /// The save names the profile this screen last read, so a change made
    /// elsewhere in between is not overwritten. `None` when that profile
    /// cannot be read back as one.
    fn model_save_input(&self, cx: &mut Cx, model: &Value, effort: &str) -> Option<Value> {
        ::hagency::native_owner::model_save_request(
            &self.policy["modelProfile"],
            json!({"model":model["model"],"credential_ref":self.selected_credential(cx),"workspace_root":self.input(cx,ids!(workspace)),"reasoning_effort":effort,"web_search":self.view.check_box(cx,ids!(web_search)).active(cx)}),
        )
    }
    fn render_policy(&self, cx: &mut Cx) {
        self.view.check_box(cx,ids!(web_search)).set_active(cx,self.policy["modelProfile"]["web_search"].as_bool().unwrap_or(true),Animate::No);
        if self.policy["modelLoaded"] == true {
            let profile = &self.policy["modelProfile"];
            let selected=self.provider_models.iter().position(|m|string(m,"model") == string(profile,"model") && (string(m,"credentialRef").is_empty() || string(m,"credentialRef") == string(profile,"credential_ref")));
            let runtime=if string(profile,"credential_ref").starts_with("claude-managed:"){1}else if string(profile,"credential_ref").starts_with("octos-managed:"){2}else{0};
            self.view.drop_down(cx,ids!(runtime_kind)).set_selected_item(cx,runtime);
            self.view.drop_down(cx,ids!(model)).set_selected_item(cx,selected.map_or(0,|i|i+1));
            self.view
                .text_input(cx, ids!(workspace))
                .set_text(cx, string(profile, "workspace_root"));
        }
        let layer = self
            .view
            .drop_down(cx, ids!(policy_layer))
            .selected_item()
            .min(2);
        let p = &self.policy["policies"][layer]["policy"];
        if !p.is_object() {
            return;
        }
        let limit = &p["budget"]["limit"];
        let mode = usize::from(limit.get("Tokens").is_some());
        self.view
            .drop_down(cx, ids!(limit_mode))
            .set_selected_item(cx, mode);
        self.view.view(cx,ids!(token_amount_holder)).set_visible(cx,mode==1);
        self.view.view(cx,ids!(period_holder)).set_visible(cx,mode==1);
        self.view.label(cx,ids!(zero_limit_note)).set_visible(cx,mode==1);
        self.view
            .text_input(cx, ids!(token_limit))
            .set_text(cx, &limit["Tokens"].as_u64().unwrap_or(0).to_string());
        self.view.drop_down(cx, ids!(period)).set_selected_item(
            cx,
            match string(&p["budget"], "period") {
                "UtcDay" => 1,
                "UtcMonth" => 2,
                _ => 0,
            },
        );
        self.view.drop_down(cx, ids!(requests)).set_selected_item(
            cx,
            match string(p, "requests") {
                "Deny" => 1,
                "AskOwner" => 2,
                _ => 0,
            },
        );
        self.view.drop_down(cx, ids!(tool_risk)).set_selected_item(
            cx,
            if p["high_risk"] == "AskOwner" {
                1
            } else if p["high_risk"].is_object() {
                2
            } else {
                0
            },
        );
        for (id, key) in [
            (ids!(allowed_tools), "tools"),
            (ids!(allowed_directories), "directories"),
        ] {
            let values = p["high_risk"]["AllowWithRules"][key]
                .as_array()
                .map(|v| {
                    v.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            self.view.text_input(cx, id).set_text(cx, &values);
        }
        self.view.label(cx, ids!(policy_usage)).set_text(
            cx,
            &crate::i18n::format("Used {spent} tokens · reserved {held}", &[
                ("scope", crate::i18n::tr(["Entire Agent", "Selected Room", "Selected requester in Room"][layer]).into()),
                ("spent", self.policy["usage"][layer]["spent"].to_string()),
                ("held", self.policy["usage"][layer]["held"].to_string()),
                
            ]),
        );
        let rules = p["high_risk"].is_object();
        self.view
            .view(cx, ids!(allowed_tools_holder))
            .set_visible(cx, rules);
        self.view
            .view(cx, ids!(allowed_directories_holder))
            .set_visible(cx, rules);
    }
    fn apply(&mut self, cx: &mut Cx, target: Target, value: &Value) {
        match target {
            Target::Members => {
                if !members_match(&self.members_room, value) {
                    return;
                }
                self.members = array(value, "members");
                self.view.label(cx, ids!(members_status)).set_text(
                    cx,
                    &crate::i18n::format("{count} members and pending invitations", &[("count", self.members.len().to_string())]),
                );
                self.view
                    .button(cx, ids!(invite_member))
                    .set_enabled(cx, value["canInvite"] == true);
                self.view
                    .button(cx, ids!(invite_member))
                    .set_disabled(cx, value["canInvite"] != true);
            }
            Target::Journal => {
                if let Some(row) = array(value, "creations")
                    .into_iter()
                    .find(|r| r["creation"]["phase"] != "complete")
                {
                    self.pending_input = Some(row["input"].clone());
                    self.creation = Some(row["creation"].clone());
                    self.view
                        .view(cx, ids!(project_list_holder))
                        .set_visible(cx, false);
                    self.view.view(cx, ids!(project_form)).set_visible(cx, true);
                    self.view.view(cx, ids!(recovery)).set_visible(cx, true);
                    self.view.label(cx,ids!(creation_result)).set_text(cx, crate::i18n::tr("An original creation is unfinished. Resume it; a new command cannot replace it."));
                }
                self.run(cx, Target::Projects, Command::Projects);
            }
            Target::Projects => {
                self.projects = array(value, "projects");
                self.labels(cx, ids!(projects), "Select a Project", &self.projects, "id");
                self.labels(
                    cx,
                    ids!(agent_project),
                    "Select a Project",
                    &self.projects,
                    "id",
                );
                if let Some(wanted) = self.wanted_project.take() {
                    if let Some(index) =
                        self.projects.iter().position(|p| string(p, "id") == wanted)
                    {
                        self.view
                            .drop_down(cx, ids!(projects))
                            .set_selected_item(cx, index + 1);
                        self.view
                            .drop_down(cx, ids!(agent_project))
                            .set_selected_item(cx, index + 1);
                        self.project_detail(cx, index);
                    }
                }
            }
            Target::Rooms => {
                if self.page == Page::Agents {
                    self.agent_rooms = array(value, "rooms")
                        .into_iter()
                        .filter(|r| r["active"] == true)
                        .collect();
                    self.labels(
                        cx,
                        ids!(agent_room),
                        "Select a visible registered Room",
                        &self.agent_rooms,
                        "roomId",
                    );
                } else {
                    self.rooms = array(value, "rooms");
                    self.labels(cx, ids!(rooms), "Select a Room", &self.rooms, "roomId");
                    if let Some((project_id, section)) = self.wanted_project_section.take()
                        && let Some(project) = self.selected(cx, ids!(projects), &self.projects)
                        && string(&project, "id") == project_id
                    {
                        match section {
                            ProjectDetailSection::Overview => {
                                self.project_section(cx, ids!(overview_section))
                            }
                            ProjectDetailSection::Rooms => {
                                self.project_section(cx, ids!(rooms_section))
                            }
                            ProjectDetailSection::Members => {
                                self.show_members(cx, string(&project, "spaceId").into(), true)
                            }
                        }
                    }
                }
            }
            Target::Spaces => {
                let mut rows = array(value, "spaces")
                    .into_iter()
                    .filter(|s| s["projectId"].is_null())
                    .collect::<Vec<_>>();
                if self.cursor.is_some() {
                    self.spaces.append(&mut rows);
                } else {
                    self.spaces = rows;
                }
                self.cursor = value["nextCursor"].as_str().map(str::to_owned);
                self.labels(
                    cx,
                    ids!(candidates),
                    "Select an unconnected joined Space",
                    &self.spaces,
                    "spaceId",
                );
                if value["incomplete"] == true {
                    self.status(cx, "Some Spaces could not be read. Refresh to retry.");
                }
            }
            Target::Agents => {
                self.agents_loaded = true;
                self.agents_loading = false;
                self.agents = array(value, "agents");
                self.runtime_list.clear(); self.runtime_list_loading=false;
                self.execution_devices = array(value, "devices");
                self.current_device_id = string(value, "currentDeviceId").into();
                self.devices_loaded = value["devices"].is_array() && !self.current_device_id.is_empty();
                self.projects = array(value, "projects");
                self.refresh_runtime_list(cx);
                self.labels(cx, ids!(agents), "Select my Agent", &self.agents, "id");
                self.labels(
                    cx,
                    ids!(agent_project),
                    "Select a Project",
                    &self.projects,
                    "id",
                );
                self.view
                    .text_input(cx, ids!(requester))
                    .set_text(cx, string(value, "ownerMxid"));
                if let Some(wanted)=self.wanted_agent.take() {
                    if let Some(index)=self.agents.iter().position(|a|string(a,"id") == wanted) {
                        self.agent_detail(cx,index);
                        return;
                    }
                }
                if let Some(wanted) = self.wanted_project.take() {
                    if let Some(index) =
                        self.projects.iter().position(|p| string(p, "id") == wanted)
                    {
                        self.view
                            .drop_down(cx, ids!(agent_project))
                            .set_selected_item(cx, index + 1);
                        self.run(
                            cx,
                            Target::Rooms,
                            Command::ProjectRooms { project_id: wanted },
                        );
                    }
                }
            }
            Target::AgentAssigned | Target::AgentDetails => {
                let Some(agent) = self.agent(cx) else {return;};
                let record = &value["agent"];
                if string(record, "id") != agent || record["generation"].as_i64().is_none() {return;}
                self.execution_agent_record = record.clone();
                if let Some(existing) = self.agents.iter_mut().find(|a|string(a,"id") == agent) {*existing = record.clone();}
                if matches!(target, Target::AgentAssigned) {
                    self.policy = Value::Null;
                    self.provider_authenticated = false;
                    self.runtime_resources_checked = false;
                    self.runtime_resources_loading = false;
                    self.view.check_box(cx, ids!(assignment_confirm)).set_active(cx, false, Animate::No);
                    self.section(cx, ids!(setup_section));
                    self.status(cx, "This Agent is now handled here. Configure a local runtime and budgets before starting.");
                }
                if !self.devices_loaded {self.run(cx, Target::Devices, Command::Devices);}
                else {self.run(cx, Target::Bindings, Command::Bindings {agent_id: agent});}
            }
            Target::Devices => {
                self.execution_devices = array(value,"devices");
                self.current_device_id = string(value,"currentDeviceId").into();
                self.devices_loaded = value["devices"].is_array() && !self.current_device_id.is_empty();
                if let Some(agent_id) = self.agent(cx) {self.run(cx,Target::Bindings,Command::Bindings {agent_id});}
            }
            Target::AgentJournal => {
                self.agent_journal_loaded = true;
                self.agent_pending_commands = array(value, "commands").into_iter().filter(|c|matches!(string(c,"state"),"unknown"|"pending") && !string(c,"id").is_empty()).collect();
                self.view
                    .view(cx, ids!(agent_recovery))
                    .set_visible(cx, !self.agent_pending_commands.is_empty());
                self.view.label(cx,ids!(agent_pending_count)).set_text(cx,&crate::i18n::format("{count} unfinished requests. Resume the original request instead of creating it again.",&[("count",self.agent_pending_commands.len().to_string())]));
                let placeholder=crate::i18n::tr("Select an unfinished request").to_owned();
                let labels=std::iter::once(placeholder).chain(self.agent_pending_commands.iter().map(agent_command_label)).collect();
                self.view.drop_down(cx,ids!(agent_pending)).set_labels(cx,labels);
                self.view.drop_down(cx,ids!(agent_pending)).set_selected_item(cx,0);
                self.view
                    .label(cx, ids!(agent_pending_details))
                    .set_text(cx, "");
                self.run(cx, Target::Agents, Command::Agents);
            }
            Target::AgentResumed => {
                self.agent_command = None;
                self.list_page(cx);
                self.run(cx, Target::AgentJournal, Command::AgentCommands);
            }
            Target::ScopeChanged => {
                let room = self.scope_state["roomId"].is_string();
                self.load_permissions(cx, room);
            }
            Target::ScopeState => {
                if self.project(cx).as_deref() != value["projectId"].as_str() {
                    return;
                }
                if let Some(room) = value["roomId"].as_str() {
                    if self
                        .selected(cx, ids!(rooms), &self.rooms)
                        .as_ref()
                        .map(|r| string(r, "roomId"))
                        != Some(room)
                    {
                        return;
                    }
                }
                self.scope_state = value.clone();
                let room = value["roomId"].is_string();
                self.view.label(cx, ids!(permission_heading)).set_text(
                    cx,
                    if room {
                        "Discussion Room Agent permissions"
                    } else {
                        "Project Agent permissions"
                    },
                );
                self.view.label(cx, ids!(permission_status)).set_text(
                    cx,
                    if value["servicePaused"] == true {
                        "Agent services are paused here."
                    } else {
                        "Agent services are not paused here."
                    },
                );
                self.view
                    .view(cx, ids!(permission_editor))
                    .set_visible(cx, value["canManagePolicy"] == true);
                let policy = if value["creationPolicy"].is_string() {
                    serde_json::from_str::<Value>(string(value, "creationPolicy"))
                        .unwrap_or(Value::Null)
                } else {
                    value["creationPolicy"].clone()
                };
                self.view
                    .view(cx, ids!(permission_editor))
                    .set_visible(cx, value["canManagePolicy"] == true && policy.is_object());
                self.scope_state["policy"] = policy.clone();
                self.view.drop_down(cx, ids!(creation_mode)).set_labels(
                    cx,
                    if room {
                        vec![
                            crate::i18n::tr("Inherit Project permissions").into(),
                            crate::i18n::tr("Only specified Room members").into(),
                            crate::i18n::tr("Disable new Agent connections").into(),
                        ]
                    } else {
                        vec![
                            crate::i18n::tr("Allow Project members by default").into(),
                            crate::i18n::tr("Only specified Project members").into(),
                        ]
                    },
                );
                let selected = if room {
                    match string(&policy, "mode") {
                        "allow_list" => 1,
                        "disabled" => 2,
                        _ => 0,
                    }
                } else {
                    usize::from(policy["defaultAllow"] != true)
                };
                self.view
                    .drop_down(cx, ids!(creation_mode))
                    .set_selected_item(cx, selected);
                for (id, key) in [
                    (ids!(creation_allow), "allow"),
                    (ids!(creation_deny), "deny"),
                ] {
                    let values = policy[key]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    self.view.text_input(cx, id).set_text(cx, &values);
                }
                self.view.check_box(cx, ids!(scope_pause)).set_active(
                    cx,
                    if room {
                        value["roomPaused"] == true
                    } else {
                        value["projectPaused"] == true
                    },
                    Animate::No,
                );
            }
            Target::RoomRoster => {
                let roster = array(value, "agents")
                    .iter()
                    .map(|a| {
                        format!(
                            "{} · {} · {}",
                            string(a, "displayName"),
                            string(a, "puppetMxid"),
                            string(a, "ownerMxid")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.view.label(cx, ids!(room_roster)).set_text(
                    cx,
                    if roster.is_empty() {
                        "No Agents are currently visible in this Room."
                    } else {
                        &roster
                    },
                );
            }
            Target::ListService => {self.refresh_runtime_list(cx);}
            Target::ServiceRoomChanged => {self.local(cx,Target::Runtime,LocalAction::AgentServiceStatus,None);}
            Target::ReplyPolicyChanged => {
                let updated = &value["binding"];
                if let Some(binding) = self.bindings.iter_mut().find(|b| string(b, "id") == string(updated, "id")) {
                    *binding = updated.clone();
                }
                self.status(cx, "Room reply behavior saved.");
            }
            Target::BindingChanged => {
                self.policy = Value::Null;
                if let Some(agent_id) = self.agent(cx) {
                    self.run(cx, Target::Bindings, Command::Bindings { agent_id });
                }
            }
            Target::Bindings => {
                self.clear_approval_state(cx);self.approvals.clear();
                self.bindings = array(value, "bindings");
                // The private chat is the Agent's owner_direct binding: show it as
                // ready (open / encrypt) without a setup retry. A retry in flight wins.
                if let Some(agent)=self.agent(cx)
                    && self.direct_retry.as_ref().is_none_or(|(_,retry,_)|retry != &agent)
                    && let Some(binding)=self.bindings.iter().find(|b|string(b,"scopeKind")=="owner_direct" && string(b,"agentId")==agent && b["projectId"].is_null() && !string(b,"roomId").is_empty()) {
                    self.direct_state=json!({"roomId":string(binding,"roomId"),"bindingId":string(binding,"id"),"state":string(binding,"state"),"phase":string(binding,"state"),"lastError":null});
                    self.direct_agent=agent;
                    self.render_owner_direct(cx);
                }
                self.label_bindings(cx,ids!(bindings));
                self.label_bindings(cx,ids!(policy_binding));
                let initial = self.bindings.iter().position(|b| string(b,"scopeKind") == "owner_direct" && string(b,"state") == "active").or_else(||self.bindings.iter().position(|b| string(b,"state") == "active"));
                self.select_policy_room(cx, initial.map_or(0,|i|i+1));
                if let Some(index)=initial {let label=self.binding_label(cx,&self.bindings[index]);self.view.label(cx,ids!(binding_details)).set_text(cx,&format!("{} · {}",label,string(&self.bindings[index],"state")));}
                self.view.drop_down(cx, ids!(policy_layer)).set_selected_item(cx, 0);
                if let Some((agent,binding))=self.wanted_run.take()
                    && self.agent(cx).as_deref()==Some(agent.as_str())
                    && let Some(index)=self.bindings.iter().position(|b|string(b,"id")==binding) {
                    self.select_policy_room(cx,index+1);
                    self.section(cx,ids!(runtime_section));
                    if self.local_agent(cx) {self.load_runtime_resources(cx);}
                    return;
                }
                if self.local_agent(cx) {self.local(cx, Target::Policy, LocalAction::AgentPolicy, None);}
                else if self.remote_agent(cx) {self.load_configuration(cx);}
            }
            Target::ConfigurationStatus => {
                let metadata=&value["configuration"];
                if !configuration_matches(metadata,&self.execution_agent_record) {self.clear_configuration(cx);self.status(cx,"Execution device assignment changed. Reopen this Agent.");return;}
                let was_online=configuration_matches(&self.configuration_status,&self.execution_agent_record) && self.configuration_status["online"]==true;
                let device_changed=self.configuration_status["deviceGeneration"].is_number()
                    && self.configuration_status["deviceGeneration"]!=metadata["deviceGeneration"];
                let requested=std::mem::take(&mut self.configuration_discover);
                if device_changed {
                    let feedback=self.configuration_feedback.clone();
                    self.clear_configuration(cx);self.configuration_feedback=feedback;
                }
                self.configuration_status=metadata.clone();
                self.configuration_metadata_error.clear();
                let discover=requested || (metadata["online"]==true && (!was_online || device_changed));
                if discover && self.configuration_online() {
                    if let Some(agent_id)=self.agent(cx) {self.run(cx,Target::ExecutionEnvironment,Command::ExecutionEnvironment {agent_id});}
                } else if discover && metadata["environment"].is_object() {
                    self.apply(cx,Target::ExecutionEnvironment,&metadata["environment"]);
                }
                cx.stop_timer(self.configuration_timer);self.configuration_timer=cx.start_timeout(3.0);
                self.render_configuration(cx);
                self.sync_navigation(cx);
            }
            Target::ExecutionEnvironment => {
                if value["configuration"]["agentId"].as_str()!=self.agent(cx).as_deref() {return;}
                self.configuration_environment=value.clone();
                let v=&value["configuration"];
                self.policy=json!({"agentId":v["agentId"],"scope":"agent","policies":[v["policy"]],"usage":[v["usage"]],"modelProfile":v["modelProfile"],"modelLoaded":true});
                self.render_policy(cx);
                let runtime=self.provider_runtime(cx);
                if let Some(provider)=array(value,"providers").iter().find(|p|string(p,"runtime")==runtime).cloned() {
                    self.apply(cx,Target::ProviderModels,&provider);
                } else {
                    self.provider_models.clear();self.runtime_efforts.clear();self.credential_ref.clear();self.provider_authenticated=false;self.render_models(cx);
                    self.view.label(cx,ids!(provider_status)).set_text(cx,crate::i18n::tr("This runtime is unavailable on the execution device."));
                }
                self.render_configuration(cx);self.sync_navigation(cx);
                // Refresh the server observation time after the execution device
                // publishes a live environment; cached/offline reads stay inert.
                if self.configuration_online() && let Some(agent_id)=self.agent(cx) {
                    self.run(cx,Target::ConfigurationStatus,Command::AgentConfigurationStatus {agent_id});
                }
            }
            Target::Policy => {
                self.policy = if value["policy"].is_object() {
                    json!({"agentId":value["agentId"],"scope":"agent","policies":[value["policy"]],"usage":[value["usage"]],"modelProfile":value["modelProfile"],"modelLoaded":value.get("modelProfile").is_some()})
                } else {
                    value.clone()
                };
                self.render_policy(cx);
                if self.view.view(cx,ids!(model_section)).visible() || (self.runtime_resources_loading && self.view.view(cx,ids!(runtime_section)).visible()) {
                    self.run(cx,Target::ProviderModels,Command::Provider {action:ProviderAction::Models});
                }
            }
            Target::Creation => {
                let next = value["creation"].clone();
                self.view.label(cx, ids!(creation_result)).set_text(
                    cx,
                    &format!(
                        "{} · {} · {}",
                        string(&next, "phase"),
                        string(&next, "roomId"),
                        string(&next, "lastError")
                    ),
                );
                self.creation = Some(next.clone());
                self.view
                    .view(cx, ids!(recovery))
                    .set_visible(cx, next["phase"] != "complete");
                if next["phase"] == "complete" {
                    cx.action(crate::home::project_tree::ProjectTreeChanged {
                        identity: self.identity.clone(),
                    });
                    self.pending_input = None;
                    self.list_page(cx);
                    self.status(cx, "Creation completed. Refresh Projects to see its Rooms.");
                    self.run(cx, Target::Projects, Command::Projects);
                }
            }
            Target::WorkspacePrepared => {
                self.view.text_input(cx,ids!(workspace)).set_text(cx,string(value,"workspace"));
                self.status(cx,"Private workspace is ready. Save the model profile to use it.");
            }
            Target::ProviderModels => {
                let runtime_check = self.runtime_resources_loading && self.view.view(cx,ids!(runtime_section)).visible();
                self.apply(cx,Target::Provider,value);
                self.provider_models=array(value,"models");
                self.provider_authenticated=value["authenticated"] == true;
                self.credential_ref=string(value,"credentialRef").into();
                self.render_models(cx);
                self.runtime_resources_checked = true;
                self.runtime_resources_loading = false;
                if runtime_check {
                    self.local(cx,Target::Runtime,LocalAction::AgentServiceStatus,None);
                }
            }
            Target::Provider => {
                self.credential_ref = string(value, "credentialRef").into();
                self.provider_url = value["authUrl"].as_str().map(str::to_owned);
                self.provider_authenticated = value["authenticated"] == true;
                self.view.label(cx, ids!(credential)).set_text(cx, "");
                let signed_in = value["authenticated"] == true;
                let external=matches!(string(value,"runtime"),"claude"|"octos");
                let shared=value["shared"] == true && (value["credentialSource"] == "local_codex" || external);
                let signing_in = !shared && !signed_in && self.provider_url.is_some();
                self.view.button(cx,ids!(use_local_provider)).set_visible(cx,!shared || (external && !signed_in));
                self.view.button(cx,ids!(disconnect_provider)).set_visible(cx,shared);
                self.view.button(cx,ids!(provider_login)).set_visible(cx,!external && !shared && !signed_in && !signing_in);
                self.view.button(cx,ids!(provider_cancel)).set_visible(cx,signing_in);
                self.view.button(cx,ids!(provider_logout)).set_visible(cx,signed_in && !shared);
                self.view.button(cx,ids!(provider_open)).set_visible(cx,signing_in);
                self.view.view(cx,ids!(provider_pending_actions)).set_visible(cx,signing_in);
                self.view.label(cx, ids!(provider_status)).set_text(
                    cx,
                    if external && signed_in {crate::i18n::tr("Local runtime connected. Credentials remain managed by its own app.")} else if external {crate::i18n::tr("Sign in or configure profiles in the local runtime, then connect it here.")} else if shared && signed_in {crate::i18n::tr("Using your signed-in local Codex account on this device.")} else if shared {crate::i18n::tr("Local Codex is signed out. Sign in through Codex, then check the account.")} else if signed_in {crate::i18n::tr("Separate Codex account connected on this device.")} else if signing_in {crate::i18n::tr("Finish Codex sign-in in your browser, then check the account.")} else {crate::i18n::tr("Connect your own local account to run this Agent here.")},
                );
                if self.remote_agent(cx) {
                    self.provider_url=None;
                    let installation=if value["installed"]==true {crate::i18n::tr("Installed")} else {crate::i18n::tr("Not installed")};
                    let version=value["version"].as_str().unwrap_or(crate::i18n::tr("Version unknown"));
                    let readiness=match string(value,"state") {"ready"=>crate::i18n::tr("Ready"),"unsupported"=>crate::i18n::tr("Unsupported"),"disconnected"=>crate::i18n::tr("Disconnected"),_=>crate::i18n::tr("Unavailable")};
                    let verified=if value["accountVerified"]==true {crate::i18n::tr("Account verified")} else {crate::i18n::tr("Account verification unavailable")};
                    self.view.label(cx,ids!(provider_status)).set_text(cx,&format!("{installation} · {version} · {readiness} · {verified}"));
                    self.view.label(cx,ids!(runtime_provider_note)).set_text(cx,crate::i18n::tr("Connect uses the owner's provider on the execution device and affects that owner's other Agents. Sign-in and credentials stay on that device."));
                    for id in [ids!(provider_login),ids!(provider_logout),ids!(provider_cancel),ids!(provider_open),ids!(disconnect_provider)] {self.view.button(cx,id).set_visible(cx,false);}
                }
                self.render_model_advanced(cx);
            }
            Target::Runtime => {
                cx.stop_timer(self.runtime_list_timer);
                let label=runtime_list_label(Some(&Ok(value.clone())),self.agent(cx).as_deref().unwrap_or(""));
                self.runtime_list_timer=cx.start_timeout(if label.starts_with("Starting") || label.starts_with("Checking") {2.0} else {10.0});
                self.agent_service_state=value.clone();
                let height=(array(&value["service"],"rooms").len() as f64 * 88.0).clamp(48.0,352.0);
                let mut list=self.view.portal_list(cx,ids!(service_rooms));
                script_apply_eval!(cx,list,{height: #(height)});
                self.runtime_room_active=service_enabled(value);
                let current=Ok(value.clone());
                let mut summary=runtime_service_description(Some(&current),self.agent(cx).as_deref().unwrap_or(""));
                if !self.runtime_action_error.is_empty() {summary.push_str(&format!("\n{}",self.runtime_action_error));}
                self.view.label(cx, ids!(runtime_status)).set_text(cx,&summary);
                if value.to_string().contains("ledger_recovery_required") {
                    self.status(cx,"Restore the complete current-schema ledger of this exact Matrix owner, including spent costs and unresolved holds. No new model requests are allowed; known original replies may reconcile. Stop, restore, then start again.");
                }
                self.refresh_approvals(cx);
            }
            Target::ApprovalDecision => {
                self.view.label(cx,ids!(approval_feedback)).set_text(cx,crate::i18n::tr(if value["decided"]==true {
                    "Decision submitted. Execution is not yet confirmed. Check the Room reply and runtime status."
                } else {"Decision delivery is unconfirmed. Refresh status; do not resubmit this request."}));
                self.refresh_approvals(cx);
            }
            Target::AgentCreated => {
                cx.action(crate::home::project_tree::ProjectTreeChanged {identity:self.identity.clone()});
                self.agent_command=None;
                self.list_page(cx);
                let id=string(&value["creation"]["agent"],"id");
                if !id.is_empty() {
                    self.direct_agent=id.into();
                    self.direct_state=json!({"state":"pending"});
                    self.wanted_agent=Some(id.into());
                    self.run(cx,Target::OwnerDirectCreated,Command::EnsureAgentOwnerDirect {agent_id:id.into()});
                } else {self.run(cx,Target::AgentJournal,Command::AgentCommands);}
            }
            Target::OwnerDirect | Target::OwnerDirectCreated => {
                self.direct_state=value["ownerDirect"].clone();
                self.schedule_direct_retry(cx);
                self.render_owner_direct(cx);
                if matches!(target,Target::OwnerDirectCreated) {self.run(cx,Target::AgentJournal,Command::AgentCommands);}
            }
            Target::ProjectAdopted
            | Target::RoomAdopted
            | Target::AgentBound => {
                cx.action(crate::home::project_tree::ProjectTreeChanged {
                    identity: self.identity.clone(),
                });
                if matches!(target, Target::RoomAdopted) {
                    self.wanted_project = self.project(cx);
                }
                self.list_page(cx);
                if matches!(target, Target::AgentCreated | Target::AgentBound) {
                    self.agent_command = None;
                }
                let query = if matches!(target, Target::AgentCreated | Target::AgentBound) {
                    Command::AgentCommands
                } else {
                    mutation_refresh(target).expect("confirmed mutation refresh")
                };
                self.run(
                    cx,
                    if matches!(target, Target::AgentCreated | Target::AgentBound) {
                        Target::AgentJournal
                    } else {
                        Target::Projects
                    },
                    query,
                );
            }
            Target::Changed => {
                cx.action(crate::home::project_tree::ProjectTreeChanged {
                    identity: self.identity.clone(),
                });
                self.status(cx, "Saved. Refresh to load the confirmed state.");
            }
        }
        self.view.redraw(cx);
    }
}
impl HagencyPanel {
    fn runtime_feedback(&mut self, cx:&mut Cx, message:&str) {
        self.runtime_action_error=crate::i18n::tr(message).to_owned();
        self.view.label(cx,ids!(runtime_status)).set_text(cx,&self.runtime_action_error);
        self.status(cx,message);
    }
    fn local(&mut self, cx: &mut Cx, target: Target, action: LocalAction, input: Option<Value>) {
        if !self.local_agent(cx) && !self.remote_agent(cx) {self.status(cx, "This action is available only on the execution device.");return;}
        if matches!(
            action,
            LocalAction::AgentPolicy
                | LocalAction::SaveAgentPolicy
                | LocalAction::ResetAgentPolicy
                | LocalAction::AgentModel
                | LocalAction::SaveAgentModel
                | LocalAction::StartAgentService
                | LocalAction::StopAgentService
                | LocalAction::AgentServiceStatus
        ) {
            if let Some(agent_id) = self.agent(cx) {
                self.run(
                    cx,
                    target,
                    Command::Local {
                        agent_id,
                        action,
                        input,
                        binding_id: None,
                        requester: None,
                    },
                );
            }
            return;
        }
        let (Some(agent_id), Some(binding_id)) = (self.agent(cx), self.binding(cx)) else {
            self.status(cx, "Select an owned Agent and Room binding first.");
            return;
        };
        let requester = self.input(cx, ids!(requester));
        self.run(
            cx,
            target,
            Command::Local {
                agent_id,
                action,
                input,
                binding_id: Some(binding_id),
                requester: Some(requester),
            },
        );
    }
    fn create_matrix(&mut self, cx: &mut Cx) {
        if self
            .creation
            .as_ref()
            .is_some_and(|v| v["phase"] != "complete")
        {
            self.status(
                cx,
                "An uncertain original creation must be resumed first. Do not create a duplicate.",
            );
            return;
        }
        let existing = self
            .view
            .drop_down(cx, ids!(creation_source))
            .selected_item()
            == 1;
        if existing {
            if self.create_room {
                if let Some(project_id) = self.project(cx) {
                    let room_id = self.input(cx, ids!(existing_room));
                    self.run(
                        cx,
                        Target::RoomAdopted,
                        Command::AdoptRoom {
                            project_id,
                            room_id,
                        },
                    );
                }
            } else if let Some(space) = self.selected(cx, ids!(candidates), &self.spaces) {
                self.run(
                    cx,
                    Target::ProjectAdopted,
                    Command::AdoptProject {
                        space_id: string(&space, "spaceId").into(),
                    },
                );
            } else {
                self.status(cx, "Choose an unconnected Space candidate first.");
            }
            return;
        }
        let name = self.input(cx, ids!(creation_name));
        if name.is_empty() {
            self.status(cx, "Enter a name.");
            return;
        }
        let mut input = json!({"kind":if self.create_room{"room"}else{"space"},"name":name,"commandId":uuid::Uuid::new_v4().to_string()});
        if self.create_room {
            let Some(project) = self.selected(cx, ids!(projects), &self.projects) else {
                self.status(cx, "Select a Project before adding a Room.");
                return;
            };
            input["projectId"] = project["id"].clone();
            input["spaceId"] = project["spaceId"].clone();
        }
        self.pending_input = Some(input.clone());
        self.creation = Some(json!({"commandId":input["commandId"],"phase":"unknown"}));
        self.run(cx, Target::Creation, Command::CreateMatrix { input });
    }
    fn save_policy(&mut self, cx: &mut Cx) {
        let Some(agent) = self.agent(cx) else {
            return;
        };
        let binding = self.binding(cx);
        let requester = self.input(cx, ids!(requester));
        let index = self
            .view
            .drop_down(cx, ids!(policy_layer))
            .selected_item()
            .min(2);
        if !editor_matches(&self.policy, &agent, binding.as_deref(), &requester, index) {
            self.status(cx, "Load the exact Agent or Room policy before saving.");
            return;
        }
        let Some(revision) = self.policy["policies"][index]["revision"].as_i64() else {
            self.status(cx, "Load the exact policy before editing.");
            return;
        };
        let limit = match self.view.drop_down(cx, ids!(limit_mode)).selected_item() {
            0 => json!("Unlimited"),
            _ => {
                let Ok(tokens) = self.input(cx, ids!(token_limit)).parse::<u64>() else {
                    self.status(cx, "Enter a nonnegative whole token count.");
                    return;
                };
                if tokens > 9_007_199_254_740_991 {
                    self.status(cx, "Token count exceeds the supported exact integer range.");
                    return;
                }
                json!({"Tokens":tokens})
            }
        };
        let period = ["Lifetime", "UtcDay", "UtcMonth"]
            [self.view.drop_down(cx, ids!(period)).selected_item().min(2)];
        let requests = ["Allow", "Deny", "AskOwner"][self
            .view
            .drop_down(cx, ids!(requests))
            .selected_item()
            .min(2)];
        let risk = match self.view.drop_down(cx, ids!(tool_risk)).selected_item() {
            1 => json!("AskOwner"),
            2 => {
                let tools = self
                    .input(cx, ids!(allowed_tools))
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                let dirs = self
                    .input(cx, ids!(allowed_directories))
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                if tools.is_empty() || dirs.is_empty() {
                    self.status(
                        cx,
                        "Exact allow rules require both tools and absolute directories.",
                    );
                    return;
                }
                json!({"AllowWithRules":{"tools":tools,"directories":dirs}})
            }
            _ => json!("Deny"),
        };
        let policy =
            json!({"budget":{"limit":limit,"period":period},"requests":requests,"high_risk":risk});
        if index == 0 {
            self.local(
                cx,
                Target::Policy,
                LocalAction::SaveAgentPolicy,
                Some(json!({"expectedRevision":revision,"policy":policy})),
            );
        } else if let Some(binding_id) = binding {
            self.local(cx, Target::Policy, LocalAction::SavePolicy, Some(json!({"bindingId":binding_id,"requester":requester,"layer":(["agent","room","requester"][index]),"expectedRevision":revision,"policy":policy})));
        }
    }
}
impl Widget for HagencyPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if !self.active {
            return;
        }
        let Ok(service) = Service::for_current() else {
            self.cancel_direct_retry(cx);
            self.cancel_read_wait(cx);
            self.active = false;
            self.request = nonce();
            self.view
                .view(cx, ids!(projects_page))
                .set_visible(cx, false);
            self.view.view(cx, ids!(agents_page)).set_visible(cx, false);
            self.display_blocked=true;
            self.status(cx, "Sign in with a Matrix account to use Hagency.");
            return;
        };
        if service.identity() != self.identity {
            self.cancel_direct_retry(cx);
            self.cancel_read_wait(cx);
            self.active = false;
            self.busy = false;
            self.request = nonce();
            self.view
                .view(cx, ids!(projects_page))
                .set_visible(cx, false);
            self.view.view(cx, ids!(agents_page)).set_visible(cx, false);
            self.display_blocked=true;
            self.status(cx,"Account changed. Reopen management for this account; no old operations are adopted.");
            return;
        }
        if self.approval_timer.is_event(event).is_some() {
            if self.approval_started.is_some_and(|at|at.elapsed().as_secs()>=10) {
                self.approval_checked=false;
                self.view.label(cx,ids!(approval_count)).set_text(cx,crate::i18n::tr("Approvals could not be refreshed. Decisions are disabled until a fresh check succeeds."));
            }
            self.sync_navigation(cx);self.render_approval_details(cx);self.refresh_approvals(cx);
            self.approval_timer=cx.start_timeout(3.0);
        }
        if self.runtime_list_timer.is_event(event).is_some() {
            if self.view.view(cx,ids!(runtime_section)).visible() && self.local_agent(cx) {
                if !self.busy {self.local(cx,Target::Runtime,LocalAction::AgentServiceStatus,None);} else {self.runtime_list_timer=cx.start_timeout(2.0);}
            } else {self.refresh_runtime_list(cx);}
        }
        if self.configuration_timer.is_event(event).is_some() {
            if self.remote_agent(cx) && self.view.view(cx,ids!(agent_detail)).visible() {
                self.poll_configuration_status(cx);
            }
        }
        if self.read_timer.is_event(event).is_some() {
            self.expire_read_wait(cx, std::time::Instant::now());
            return;
        }
        if self.direct_timer.is_event(event).is_some() {
            self.direct_timer = Timer::default();
            if let Some((identity,agent,_))=self.direct_retry.clone() {
                let target = self.agent(cx).as_deref() == Some(agent.as_str()) || self.wanted_agent.as_deref() == Some(agent.as_str());
                if identity != self.identity || self.page != Page::Agents || !target {self.cancel_direct_retry(cx);}
                else if self.busy {self.schedule_direct_retry(cx);}
                else {self.run(cx,Target::OwnerDirect,Command::EnsureAgentOwnerDirect {agent_id:agent});}
            }
            return;
        }
        let Event::Actions(actions) = event else {
            return;
        };
        for action in actions {
            if let Some(result)=action.downcast_ref::<OwnerDirectEncryption>() {
                if self.direct_encrypting.as_deref()==Some(result.room_id.as_str()) {self.direct_encrypting=None;}
                if result.enabled {self.direct_encrypted.insert(result.room_id.clone());}
                if string(&self.direct_state,"roomId")==result.room_id {
                    self.status(cx,if result.enabled {"End-to-end encryption is on for this private chat. New messages are encrypted."} else {"Could not enable end-to-end encryption. Try again."});
                    self.render_owner_direct(cx);
                }
                continue;
            }
            if let Some(done)=action.downcast_ref::<ConfigurationCompleted>()
                && self.configuration_poll_request==Some(done.request) {
                self.configuration_poll_request=None;
                if self.busy {self.configuration_timer=cx.start_timeout(3.0);continue;}
                if done.identity==self.identity && self.agent(cx).as_deref()==Some(done.agent.as_str())
                    && self.execution_agent_record["generation"].as_i64()==Some(done.generation)
                    && string(&self.execution_agent_record,"executionDeviceId")==done.device {
                    match &done.result {
                        Ok(value)=>self.apply(cx,Target::ConfigurationStatus,value),
                        Err(error)=> {self.finish_configuration_intent(None,&Err(error.clone()));self.sync_navigation(cx);self.configuration_timer=cx.start_timeout(3.0);}
                    }
                }
            }
            if let Some(done)=action.downcast_ref::<ApprovalsCompleted>()
                && self.approval_poll==Some(done.request) {
                let accepted=self.accepts_approval_poll(cx,done);
                self.approval_poll=None;self.approval_started=None;
                if !accepted {self.approval_checked=false;continue;}
                match &done.result {
                    Ok(value)=>self.apply_approvals(cx,value),
                    Err(_)=> {self.approval_checked=false;self.sync_navigation(cx);self.view.label(cx,ids!(approval_count)).set_text(cx,crate::i18n::tr("Approvals could not be refreshed. Decisions are disabled until a fresh check succeeds."));}
                }
                cx.stop_timer(self.approval_timer);self.approval_timer=cx.start_timeout(3.0);
            }
            if let Some(done)=action.downcast_ref::<RuntimeListCompleted>()
                && self.active && self.identity==done.identity && self.runtime_list_request==done.request {
                self.runtime_list_loading=false;
                self.runtime_list=done.states.iter().cloned().collect();
                self.runtime_list_timer=cx.start_timeout(10.0);
                self.view.redraw(cx);
            }
            if let Some(done) = action.downcast_ref::<Completed>()
                && current_completion(self.active, &self.identity, self.request, done)
            {
                self.cancel_read_wait(cx);
                if done.configuration_fence.as_ref().is_some_and(|(agent,generation,device)|self.agent(cx).as_deref()!=Some(agent.as_str()) || self.execution_agent_record["generation"].as_i64()!=Some(*generation) || string(&self.execution_agent_record,"executionDeviceId")!=device) {
                    self.set_busy(cx,false);self.clear_configuration(cx);continue;
                }
                if matches!(done.target,Target::ListService) {self.list_service_pending_agent.clear();}
                if matches!(done.target,Target::Runtime) && self.runtime_pending_action.take().is_some() {
                    self.runtime_action_error=done.result.as_ref().err().map(|error|runtime_diagnostic(error)).unwrap_or_default();
                }
                self.set_busy(cx, false);
                self.sync_navigation(cx);
                if matches!(done.target, Target::Agents) { self.agents_loading = false; }
                if done.configuration_fence.is_some() {self.finish_configuration_intent(done.configuration_digest.as_ref(),&done.result);}
                match &done.result {
                    Ok(value) => {
                        self.view.button(cx, ids!(login)).set_visible(cx, false);
                        if !matches!(done.target,Target::ConfigurationStatus) {self.status(cx, "");}
                        self.apply(cx, done.target, value);
                        if matches!(done.target,Target::Provider) && value["authenticated"] == true && self.view.view(cx,ids!(model_section)).visible() {
                            self.run(cx,Target::ProviderModels,Command::Provider {action:ProviderAction::Models});
                        }
                    }
                    Err(error) => {
                        if done.configuration_fence.is_some() {self.sync_navigation(cx);}
                        if matches!(done.target,Target::ApprovalDecision) {
                            self.view.label(cx,ids!(approval_feedback)).set_text(cx,crate::i18n::tr("Decision delivery is unconfirmed. Refresh status; do not resubmit this request."));
                            self.refresh_approvals(cx);
                        }
                        if self.runtime_resources_loading && matches!(done.target, Target::Policy | Target::ProviderModels) {self.runtime_resources_loading=false;self.runtime_resources_checked=false;}
                        let reauthenticate = matches!(
                            error.as_str(),
                            "matrix_oauth_sign_in_required"
                                | "matrix_authorization_required"
                                | "matrix_sign_in_required"
                                | "sign_in_required"
                                | "native_resume_missing"
                                | "native_resume_revoked"
                        );
                        self.view
                            .button(cx, ids!(login))
                            .set_visible(cx, reauthenticate);
                        let message = if error.contains("ledger_recovery_required") {
                            "Restore this exact owner's complete ledger before running new model requests. Stop, restore, then restart.".to_owned()
                        } else if reauthenticate {
                            "Sign in through Pasion to restore this account. Chats and Agent management use the same login.".to_owned()
                        } else if error == "matrix_authorization_unavailable" {
                            "Cannot verify this account right now. Check the server connection and retry.".to_owned()
                        } else {
                            visible_error(error)
                        };
                        if !matches!(done.target,Target::ConfigurationStatus) {self.status(cx, &message);}
                        if matches!(done.target,Target::Runtime) {
                            let current=Ok(self.agent_service_state.clone());
                            let mut summary=runtime_service_description(Some(&current),self.agent(cx).as_deref().unwrap_or(""));
                            summary.push_str(&format!("\n{}",if self.runtime_action_error.is_empty() {message.as_str()} else {self.runtime_action_error.as_str()}));
                            self.view.label(cx,ids!(runtime_status)).set_text(cx,&summary);
                        }
                        if matches!(done.target,Target::OwnerDirect | Target::OwnerDirectCreated) {
                            self.cancel_direct_retry(cx);
                            self.direct_state["state"]="pending".into();
                            self.render_owner_direct(cx);
                            if matches!(done.target,Target::OwnerDirectCreated) {self.run(cx,Target::AgentJournal,Command::AgentCommands);}
                        }
                        if matches!(done.target, Target::Members) {
                            self.view.label(cx, ids!(members_status)).set_text(cx, if error == "room_join_required" {
                                "Join this Space or Room first to view its members. Joining the Project does not join its Rooms."
                            } else { "Could not load members. Check your connection and retry." });
                        }
                        if matches!(done.target, Target::Creation) && self.pending_input.is_some() {
                            self.view.view(cx, ids!(recovery)).set_visible(cx, true);
                            self.view.label(cx, ids!(creation_result)).set_text(cx, crate::i18n::tr("Result unconfirmed. Resume this original creation; do not start a replacement."));
                        }
                        if matches!(done.target, Target::Creation)
                            && error.contains("creation_command_not_found")
                        {
                            if let Some(creation) = self.creation.as_mut() {
                                creation["phase"] = "unrecorded".into();
                            }
                            self.view.label(cx, ids!(creation_result)).set_text(
                                cx,
                                "Original command not recorded yet; retry the SAME ID and input.",
                            );
                        }
                    }
                }
            }
        }
        if self.busy {
            return;
        }
        if self.view.view(cx,ids!(runtime_section)).visible() {
            let list=self.view.portal_list(cx,ids!(service_rooms));
            let rooms=array(&self.agent_service_state["service"],"rooms");
            for (_,row) in list.items_with_actions(actions) {
                let Some((agent,binding_id))=self.service_row_targets.get(&row.widget_uid()) else {continue;};
                if self.agent(cx).as_deref()!=Some(agent.as_str()) {continue;}
                let Some(binding)=rooms.iter().find(|b|string(b,"id")==binding_id) else {continue;};
                if row.button(cx,ids!(open_room_chat)).clicked(actions) {
                    if let Ok(room)=matrix_sdk::ruma::OwnedRoomId::try_from(string(binding,"roomId")) {navigate_agent_chat(cx,room,string(binding,"scopeKind")=="owner_direct",None);}
                    return;
                }
                if row.button(cx,ids!(room_toggle)).clicked(actions) && let Some(agent_id)=self.agent(cx) {
                    let paused=binding["ownerServicePaused"] == true;
                    if !paused && binding["state"] != "active" {return;}
                    self.run(cx,Target::ServiceRoomChanged,Command::Binding{agent_id,binding_id:string(binding,"id").into(),action:if paused {BindingAction::Resume} else {BindingAction::Pause}});
                    return;
                }
            }
        }
        if self.back_to_list(cx, actions) {
            return;
        }
        if self.view.button(cx, ids!(new_item)).clicked(actions) {
            self.reset_detail_scroll(cx);
            self.list_page(cx);
            if self.page == Page::Projects {
                cx.action(HagencyAction::NewProject);
                return;
            } else {
                self.view
                    .view(cx, ids!(agent_list_holder))
                    .set_visible(cx, false);
                self.view.view(cx, ids!(agent_create)).set_visible(cx, true);
                self.view
                    .view(cx, ids!(agent_name_holder))
                    .set_visible(cx, true);
                self.view
                    .button(cx, ids!(create_agent))
                    .set_visible(cx, true);
                self.view
                    .button(cx, ids!(bind_agent))
                    .set_visible(cx, false);
            }
            return;
        }
        if self.view.button(cx, ids!(permissions_tab)).clicked(actions)
            || self
                .view
                .button(cx, ids!(room_permissions))
                .clicked(actions)
        {
            let room = self
                .view
                .button(cx, ids!(room_permissions))
                .clicked(actions);
            self.load_permissions(cx, room);
            return;
        }
        if self
            .view
            .button(cx, ids!(save_creation_policy))
            .clicked(actions)
            || self
                .view
                .button(cx, ids!(save_scope_pause))
                .clicked(actions)
        {
            let Some(project_id) = self.project(cx) else {
                return;
            };
            if self.scope_state["canManagePolicy"] != true
                || string(&self.scope_state, "projectId") != project_id
            {
                return;
            }
            let room_id = self.scope_state["roomId"].as_str().map(str::to_owned);
            if let Some(room) = &room_id {
                if self
                    .selected(cx, ids!(rooms), &self.rooms)
                    .as_ref()
                    .map(|r| string(r, "roomId"))
                    != Some(room.as_str())
                {
                    return;
                }
            }
            let command = if self
                .view
                .button(cx, ids!(save_scope_pause))
                .clicked(actions)
            {
                Command::SetScopeServicePause {
                    project_id,
                    room_id,
                    paused: self.view.check_box(cx, ids!(scope_pause)).active(cx),
                }
            } else {
                let Some(expected_revision) = self.scope_state["revision"].as_i64() else {
                    return;
                };
                let users = |text: String| {
                    text.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect::<std::collections::BTreeSet<String>>()
                };
                let allow = users(self.input(cx, ids!(creation_allow)));
                let deny = users(self.input(cx, ids!(creation_deny)));
                let mode = self.view.drop_down(cx, ids!(creation_mode)).selected_item();
                if let Some(room_id) = room_id {
                    let policy = match mode {
                        1 => RoomCreationPolicy::AllowList { allow, deny },
                        2 => RoomCreationPolicy::Disabled,
                        _ => RoomCreationPolicy::InheritProject { deny },
                    };
                    Command::SetRoomCreationPolicy {
                        project_id,
                        room_id,
                        expected_revision,
                        policy,
                    }
                } else {
                    Command::SetProjectCreationPolicy {
                        project_id,
                        expected_revision,
                        policy: ProjectCreationPolicy {
                            default_allow: mode == 0,
                            allow,
                            deny,
                        },
                    }
                }
            };
            self.run(cx, Target::ScopeChanged, command);
            return;
        }
        if self.view.button(cx, ids!(overview_tab)).clicked(actions) {
            self.project_section(cx, ids!(overview_section));
            return;
        }
        if self.view.button(cx, ids!(rooms_tab)).clicked(actions) {
            self.project_section(cx, ids!(rooms_section));
            return;
        }
        if self.view.button(cx, ids!(members_tab)).clicked(actions) {
            if let Some(p) = self.selected(cx, ids!(projects), &self.projects) {
                self.show_members(cx, string(&p, "spaceId").into(), true);
            }
            return;
        }
        if self.view.button(cx, ids!(room_members)).clicked(actions) {
            if let Some(r) = self.selected(cx, ids!(rooms), &self.rooms) {
                self.show_members(cx, string(&r, "roomId").into(), false);
            } else {
                self.status(cx, "Select a discussion Room first.");
            }
            return;
        }
        if self.view.button(cx, ids!(refresh_members)).clicked(actions) {
            if let Some((room, space)) = self.members_room.clone() {
                self.show_members(cx, room, space);
            }
            return;
        }
        if self.view.button(cx, ids!(invite_member)).clicked(actions) {
            if self
                .view
                .button(cx, ids!(invite_member))
                .borrow()
                .is_some_and(|b| b.enabled())
                && let Some((room, _)) = &self.members_room
                && let Ok(id) = matrix_sdk::ruma::OwnedRoomId::try_from(room.as_str())
            {
                cx.action(crate::home::invite_modal::InviteModalAction::Open(
                    crate::utils::RoomNameId::empty(id),
                ));
            }
            return;
        }
        for (button, selector, rows, field) in [
            (ids!(open_space), ids!(projects), &self.projects, "spaceId"),
            (ids!(open_room), ids!(rooms), &self.rooms, "roomId"),
        ] {
            if self.view.button(cx, button).clicked(actions) {
                if let Some(row) = self.selected(cx, selector, rows)
                    && let Ok(id) = matrix_sdk::ruma::OwnedRoomId::try_from(string(&row, field))
                {
                    cx.action(crate::app::AppStateAction::NavigateToRoom {
                        room_to_close: None,
                        destination_room: crate::room::BasicRoomDetails::RoomId(
                            crate::utils::RoomNameId::empty(id),
                        ),
                    });
                } else {
                    self.status(cx, "Select a discussion Room first.");
                }
                return;
            }
        }
        if self.open_card(cx, actions) {
            return;
        }
        for (id, section) in [
            (ids!(setup_tab), ids!(setup_section)),
            (ids!(policy_tab), ids!(policy_section)),
            (ids!(model_tab), ids!(model_section)),
            (ids!(runtime_tab), ids!(runtime_section)),
        ] {
            if self.view.button(cx, id).clicked(actions) {
                self.section(cx, section);
                if section == ids!(runtime_section) {self.load_runtime_resources(cx); return;}
                if section == ids!(policy_section) || section == ids!(model_section) {
                    if self.remote_agent(cx) {
                        self.view.drop_down(cx,ids!(policy_layer)).set_selected_item(cx,0);
                        self.clear_configuration(cx);
                        self.load_configuration(cx);
                        return;
                    }
                    self.policy = Value::Null;
                    let global = section == ids!(policy_section)
                        && self.view.drop_down(cx, ids!(policy_layer)).selected_item() == 0;
                    self.local(
                        cx,
                        Target::Policy,
                        if section == ids!(model_section) {
                            LocalAction::AgentModel
                        } else if global {
                            LocalAction::AgentPolicy
                        } else {
                            LocalAction::Policy
                        },
                        None,
                    );
                }
                return;
            }
        }
        if self.view.button(cx, ids!(another_binding)).clicked(actions) {
            self.view
                .view(cx, ids!(agent_detail))
                .set_visible(cx, false);
            self.view.view(cx, ids!(agent_create)).set_visible(cx, true);
            self.view.button(cx, ids!(bind_agent)).set_visible(cx, true);
            self.view
                .view(cx, ids!(agent_name_holder))
                .set_visible(cx, false);
            self.view
                .button(cx, ids!(create_agent))
                .set_visible(cx, false);
            return;
        }
        if self.view.drop_down(cx,ids!(limit_mode)).selected(actions).is_some() {
            let visible=self.view.drop_down(cx,ids!(limit_mode)).selected_item()==1;
            self.view.view(cx,ids!(token_amount_holder)).set_visible(cx,visible);
            self.view.view(cx,ids!(period_holder)).set_visible(cx,visible);
            self.view.label(cx,ids!(zero_limit_note)).set_visible(cx,visible);
        }
        if self
            .view
            .drop_down(cx, ids!(tool_risk))
            .selected(actions)
            .is_some()
        {
            let rules = self.view.drop_down(cx, ids!(tool_risk)).selected_item() == 2;
            self.view
                .view(cx, ids!(allowed_tools_holder))
                .set_visible(cx, rules);
            self.view
                .view(cx, ids!(allowed_directories_holder))
                .set_visible(cx, rules);
            return;
        }
        if self.view.button(cx, ids!(login)).clicked(actions) {
            cx.action(crate::accounts::AccountAction::Reauthenticate);
            return;
        }
        if self.view.button(cx,ids!(configuration_refresh)).clicked(actions) {self.load_configuration(cx);return;}
        if self.view.button(cx, ids!(refresh)).clicked(actions) {
            if self.page == Page::Projects {
                self.run(cx, Target::Journal, Command::Creations);
            } else {
                self.run(cx, Target::AgentJournal, Command::AgentCommands);
            }
            return;
        }
        if self
            .view
            .drop_down(cx, ids!(projects))
            .selected(actions)
            .is_some()
        {
            if let Some(p) = self.selected(cx, ids!(projects), &self.projects) {
                self.view.label(cx, ids!(project_details)).set_text(
                    cx,
                    &format!(
                        "{}\n{}",
                        string(&p, "spaceId"),
                        string(&p, "topic")
                    ),
                );
                self.run(
                    cx,
                    Target::Rooms,
                    Command::ProjectRooms {
                        project_id: string(&p, "id").into(),
                    },
                );
            }
            return;
        }
        if self
            .view
            .drop_down(cx, ids!(rooms))
            .selected(actions)
            .is_some()
        {
            if let Some(r) = self.selected(cx, ids!(rooms), &self.rooms) {
                self.view.label(cx, ids!(room_details)).set_text(
                    cx,
                    &format!(
                        "{} · {}\n{}",
                        string(&r, "name"),
                        string(&r, "roomId"),
                        string(&r, "topic")
                    ),
                );
            }
            return;
        }
        if self.view.button(cx, ids!(new_room)).clicked(actions) {
            if !self.begin_creation_form(cx,true) {return;}
            self.reset_detail_scroll(cx);
            self.view
                .view(cx, ids!(project_detail))
                .set_visible(cx, false);
            self.create_room = self.view.button(cx, ids!(new_room)).clicked(actions);
            self.view.view(cx, ids!(project_form)).set_visible(cx, true);
            self.view.drop_down(cx, ids!(creation_source)).set_labels(
                cx,
                if self.create_room {
                    vec![
                        crate::i18n::tr("Create a private Room").into(),
                        crate::i18n::tr("Connect an existing child Room").into(),
                    ]
                } else {
                    vec![
                        crate::i18n::tr("Create a new Space").into(),
                        crate::i18n::tr("Connect an existing Space").into(),
                    ]
                },
            );
            self.view
                .drop_down(cx, ids!(creation_source))
                .set_selected_item(cx, 0);
            self.creation_source(cx);

            return;
        }
        if self.view.button(cx, ids!(room_agents)).clicked(actions) {
            cx.action(HagencyAction::OpenAgents);
            return;
        }
        if self
            .view
            .drop_down(cx, ids!(creation_source))
            .selected(actions)
            .is_some()
        {
            self.creation_source(cx);
        }
        if self
            .view
            .drop_down(cx, ids!(creation_source))
            .selected(actions)
            == Some(1)
            && !self.create_room
        {
            self.cursor = None;
            self.run(
                cx,
                Target::Spaces,
                Command::SpaceCandidates { cursor: None },
            );
            return;
        }
        if self.view.button(cx, ids!(candidates_more)).clicked(actions) {
            self.run(
                cx,
                Target::Spaces,
                Command::SpaceCandidates {
                    cursor: self.cursor.clone(),
                },
            );
            return;
        }
        if self
            .view
            .button(cx, ids!(create_project_room))
            .clicked(actions)
        {
            self.create_matrix(cx);
            return;
        }
        if self.view.button(cx, ids!(resume_creation)).clicked(actions) {
            if let Some(input) = self.pending_input.clone() {
                let phase = self
                    .creation
                    .as_ref()
                    .map(|v| string(v, "phase"))
                    .unwrap_or("unknown");
                let command = recovery_command(input, phase, self.input(cx, ids!(found_room)));
                self.run(cx, Target::Creation, command);
            } else {
                self.status(cx, "No uncertain creation is recorded for this account.");
            }
            return;
        }
        if self
            .view
            .drop_down(cx, ids!(agent_project))
            .selected(actions)
            .is_some()
        {
            if let Some(p) = self.selected(cx, ids!(agent_project), &self.projects) {
                self.run(
                    cx,
                    Target::Rooms,
                    Command::ProjectRooms {
                        project_id: string(&p, "id").into(),
                    },
                );
            }
            return;
        }
        if self.view.button(cx, ids!(create_agent)).clicked(actions)
            || self.view.button(cx, ids!(bind_agent)).clicked(actions)
        {
            if !self.agent_journal_loaded {
                self.status(cx,"Load the original Agent request history before creating or binding again.");
                self.run(cx,Target::AgentJournal,Command::AgentCommands);
                return;
            }
            let bind = self.view.button(cx, ids!(bind_agent)).clicked(actions);
            let mut input = if bind {
                let (Some(project), Some(room)) = (self.selected(cx, ids!(agent_project), &self.projects), self.selected(cx, ids!(agent_room), &self.agent_rooms)) else {self.status(cx,"Choose a Project and a visible registered Room."); return;};
                json!({"projectId":project["id"],"roomId":room["roomId"]})
            } else {
                let name = self.input(cx, ids!(agent_name));
                if name.is_empty() { self.status(cx,"Enter the Agent name."); return; }
                json!({"displayName":name})
            };
            if self
                .agent_command
                .as_ref()
                .is_none_or(|(original, _)| original != &input)
            {
                self.agent_command = Some((input.clone(), uuid::Uuid::new_v4().to_string()));
            }
            input["idempotencyKey"] = self.agent_command.as_ref().unwrap().1.clone().into();
            if bind {
                if let Some(agent_id) = self.agent(cx) {
                    self.run(
                        cx,
                        Target::AgentBound,
                        Command::BindAgent { agent_id, input },
                    );
                }
            } else {
                self.run(cx, Target::AgentCreated, Command::CreateAgent { input });
            }
            return;
        }
        if self
            .view
            .drop_down(cx, ids!(agents))
            .selected(actions)
            .is_some()
        {
            if let Some(index) = self.view.drop_down(cx,ids!(agents)).selected_item().checked_sub(1) {self.agent_detail(cx,index);}
            return;
        }
        for (id, action) in [
            (ids!(pause_agent), AgentAction::Pause),
            (ids!(resume_agent), AgentAction::Resume),
        ] {
            if self.view.button(cx, id).clicked(actions) {
                if let Some(agent_id) = self.agent(cx) {
                    self.run(cx, Target::Changed, Command::Agent { agent_id, action });
                }
                return;
            }
        }
        if self
            .view
            .drop_down(cx, ids!(agent_pending))
            .selected(actions)
            .is_some()
        {
            if let Some(command) =
                self.selected(cx, ids!(agent_pending), &self.agent_pending_commands)
            {
                self.view.label(cx, ids!(agent_pending_details)).set_text(
                    cx,
                    &agent_command_details(&command),
                );
            }
            return;
        }
        if self
            .view
            .button(cx, ids!(resume_agent_command))
            .clicked(actions)
        {
            if let Some(command) =
                self.selected(cx, ids!(agent_pending), &self.agent_pending_commands)
            {
                self.run(
                    cx,
                    Target::AgentResumed,
                    Command::ResumeAgentCommand {
                        id: string(&command, "id").into(),
                    },
                );
            } else {
                self.status(cx, "Select the original Agent request first.");
            }
            return;
        }
        if self.view.button(cx, ids!(retire_agent)).clicked(actions) {
            if !self.view.check_box(cx, ids!(retire_confirm)).active(cx) {
                self.status(cx, "Confirm permanent retirement first. The Agent cannot transfer to another owner.");
                return;
            }
            if let Some(agent_id) = self.agent(cx) {
                self.run(
                    cx,
                    Target::Changed,
                    Command::Agent {
                        agent_id,
                        action: AgentAction::Retire,
                    },
                );
            }
            return;
        }
        if self.view.button(cx,ids!(open_binding_chat)).clicked(actions) {
            if let Some(binding)=self.selected(cx,ids!(bindings),&self.bindings)
                && string(&binding,"state") == "active"
                && Service::for_current().is_ok_and(|s|s.identity()==self.identity)
                && let Ok(id)=matrix_sdk::ruma::OwnedRoomId::try_from(string(&binding,"roomId")) {
                navigate_agent_chat(cx, id, string(&binding,"scopeKind") == "owner_direct", Some((&self.identity, string(&binding,"projectId"))));
            }
            return;
        }
        if self
            .view
            .button(cx, ids!(room_roster_check))
            .clicked(actions)
        {
            if let Some(binding) = self.selected(cx, ids!(bindings), &self.bindings) {
                if string(&binding,"scopeKind") == "owner_direct" || string(&binding,"projectId").is_empty() {
                    self.status(cx,"Open the private chat information to view its members.");
                    return;
                }
                self.run(
                    cx,
                    Target::RoomRoster,
                    Command::RoomRoster {
                        project_id: string(&binding, "projectId").into(),
                        room_id: string(&binding, "roomId").into(),
                    },
                );
            }
            return;
        }
        let budget_room_changed = self.view.drop_down(cx, ids!(policy_binding)).selected(actions).is_some();
        if self.view.button(cx, ids!(load_policy)).clicked(actions)
            || budget_room_changed
            || self
                .view
                .drop_down(cx, ids!(bindings))
                .selected(actions)
                .is_some()
        {
            let index = self.view.drop_down(cx, if budget_room_changed { ids!(policy_binding) } else { ids!(bindings) }).selected_item();
            self.select_policy_room(cx, index);
            for id in [
                ids!(leave_confirm),
                ids!(takeover),
                ids!(host_files),
            ] {
                self.view
                    .check_box(cx, id)
                    .set_active(cx, false, Animate::No);
            }
            self.view
                .label(cx, ids!(runtime_status))
                .set_text(cx, crate::i18n::tr("Refresh this Room's runtime status."));
            self.view.label(cx, ids!(room_roster)).set_text(cx, "");
            if let Some(binding) = self.selected(cx, ids!(bindings), &self.bindings) {
                let room_label = self.binding_label(cx, &binding);
                self.view.label(cx, ids!(binding_details)).set_text(
                    cx,
                    &format!(
                        "{} · {}",
                        room_label,
                        string(&binding, "state")
                    ),
                );
            }
            self.clear_approval_state(cx);
        self.approvals.clear();
            self.labels(
                cx,
                ids!(approvals),
                "Select a pending approval",
                &[],
                "proposalId",
            );
            self.view.label(cx, ids!(approval_details)).set_text(cx, "");
            let global = self.view.view(cx, ids!(policy_section)).visible()
                && self.view.drop_down(cx, ids!(policy_layer)).selected_item() == 0;
            self.local(
                cx,
                Target::Policy,
                if global {
                    LocalAction::AgentPolicy
                } else {
                    LocalAction::Policy
                },
                None,
            );
            return;
        }
        if self
            .view
            .drop_down(cx, ids!(policy_layer))
            .selected(actions)
            .is_some()
        {
            self.policy = Value::Null;
            let action = if self.view.drop_down(cx, ids!(policy_layer)).selected_item() == 0 {
                LocalAction::AgentPolicy
            } else {
                LocalAction::Policy
            };
            self.local(cx, Target::Policy, action, None);
            return;
        }
        if self.view.button(cx, ids!(reset_policy)).clicked(actions) {
            let Some(agent) = self.agent(cx) else {
                return;
            };
            let binding = self.binding(cx);
            let requester = self.input(cx, ids!(requester));
            let index = self
                .view
                .drop_down(cx, ids!(policy_layer))
                .selected_item()
                .min(2);
            if !editor_matches(&self.policy, &agent, binding.as_deref(), &requester, index) {
                self.status(cx, "Load the exact policy before resetting.");
                return;
            }
            if let Some(revision) = self.policy["policies"][index]["revision"].as_i64() {
                if index == 0 {
                    self.local(
                        cx,
                        Target::Policy,
                        LocalAction::ResetAgentPolicy,
                        Some(json!({"expectedRevision":revision})),
                    );
                } else if let Some(binding) = binding {
                    self.local(cx, Target::Policy, LocalAction::ResetPolicy, Some(json!({"bindingId":binding,"requester":requester,"layer":(["agent","room","requester"][index]),"expectedRevision":revision})));
                }
            }
            return;
        }
        for (button, action) in [
            (ids!(pause_binding), BindingAction::Pause),
            (ids!(resume_binding), BindingAction::Resume),
            (ids!(leave_binding), BindingAction::Leave),
        ] {
            if self.view.button(cx, button).clicked(actions) {
                if matches!(action, BindingAction::Leave)
                    && !self.view.check_box(cx, ids!(leave_confirm)).active(cx)
                {
                    self.status(cx, "Confirm leaving the selected Room first. Other Room bindings remain separate.");
                    return;
                }
                if let (Some(agent_id), Some(binding_id)) = (self.agent(cx), self.binding(cx)) {
                    self.run(
                        cx,
                        Target::BindingChanged,
                        Command::Binding {
                            agent_id,
                            binding_id,
                            action,
                        },
                    );
                } else {
                    self.status(cx, "Select a Room binding first.");
                }
                return;
            }
        }
        if self.view.button(cx, ids!(save_policy)).clicked(actions) {
            self.save_policy(cx);
            return;
        }
        for (id, action) in [
            (ids!(provider_check), ProviderAction::Status),
            (ids!(provider_advanced_check), ProviderAction::Status),
            (ids!(use_local_provider), ProviderAction::UseLocal),
            (ids!(disconnect_provider), ProviderAction::Disconnect),
            (ids!(provider_login), ProviderAction::Login),
            (ids!(provider_cancel), ProviderAction::Cancel),
            (ids!(provider_logout), ProviderAction::Logout),
        ] {
            if self.view.button(cx, id).clicked(actions) {
                self.run(cx, Target::Provider, Command::Provider { action });
                return;
            }
        }
        if self.view.button(cx, ids!(provider_open)).clicked(actions) {
            if let Some(url) = &self.provider_url {
                let _ = robius_open::Uri::new(url).open();
            }
            return;
        }
        if self.view.button(cx,ids!(refresh_models)).clicked(actions) {
            self.run(cx,Target::ProviderModels,Command::Provider {action:ProviderAction::Models});return;
        }
        if self.view.button(cx,ids!(prepare_workspace)).clicked(actions) {
            if let Some(agent_id)=self.agent(cx) {self.run(cx,Target::WorkspacePrepared,Command::PrepareAgentWorkspace {agent_id});}return;
        }
        if self.view.button(cx, ids!(room_advanced_toggle)).clicked(actions) {
            self.room_advanced = !self.room_advanced;
            self.render_room_advanced(cx);
            return;
        }
        if self.view.button(cx,ids!(service_advanced_toggle)).clicked(actions) {
            self.service_advanced=!self.service_advanced;self.render_service_advanced(cx);return;
        }
        if self.view.button(cx,ids!(advanced_toggle)).clicked(actions) {
            self.model_advanced=!self.model_advanced;self.render_model_advanced(cx);return;
        }
        if self.view.drop_down(cx,ids!(runtime_kind)).selected(actions).is_some() {
            if self.busy || self.runtime_room_active {return;}
            self.provider_models.clear();self.runtime_efforts.clear();self.credential_ref.clear();self.provider_authenticated=false;self.runtime_resources_checked=false;
            self.render_models(cx);
            self.run(cx,Target::ProviderModels,Command::Provider {action:ProviderAction::Models});return;
        }
        if self.view.drop_down(cx,ids!(model)).selected(actions).is_some() {self.render_efforts(cx);return;}
        if self.view.drop_down(cx,ids!(effort)).selected(actions).is_some() {self.render_effort_note(cx);return;}
        if self.view.button(cx, ids!(save_model)).clicked(actions) {
            if self.credential_ref.is_empty() {
                self.status(cx, "Check your connected local account first.");
                return;
            }
            let Some(agent) = self.agent(cx) else {
                return;
            };
            if string(&self.policy, "agentId") != agent
                || self.policy["scope"] != "agent"
                || self.policy["modelLoaded"] != true
            {
                self.status(cx, "Load this Agent's local resources before saving.");
                return;
            }
            let Some(model)=self.selected(cx,ids!(model),&self.provider_models) else {self.status(cx,"Select a model or profile from this device's catalog.");return;};
            if !self.provider_authenticated || self.credential_ref.is_empty() {self.status(cx,"Connect your local account first.");return;}
            let Some(effort)=self.runtime_efforts.get(self.view.drop_down(cx,ids!(effort)).selected_item()).cloned() else {self.status(cx,"Select a supported reasoning effort.");return;};
            let Some(input) = self.model_save_input(cx,&model,&effort) else {self.status(cx, "Load this Agent's local resources before saving.");return;};
            self.local(cx, Target::Policy, LocalAction::SaveAgentModel, Some(input));
            return;
        }
        if self.view.button(cx,ids!(ensure_owner_direct)).clicked(actions) {
            if let Some(agent_id)=self.agent(cx) {
                self.cancel_direct_retry(cx);
                self.direct_agent=agent_id.clone();self.direct_state=json!({"state":"pending"});
                self.run(cx,Target::OwnerDirect,Command::EnsureAgentOwnerDirect {agent_id});
            }
            return;
        }
        if self.view.button(cx,ids!(open_owner_direct)).clicked(actions) {
            if self.agent(cx).as_deref()==Some(self.direct_agent.as_str()) && self.direct_state["state"] == "active" && Service::for_current().is_ok_and(|s|s.identity()==self.identity)
                && let Ok(id)=matrix_sdk::ruma::OwnedRoomId::try_from(string(&self.direct_state,"roomId")) {
                navigate_agent_chat(cx, id, true, None);
            }
            return;
        }
        // The Agent's own device on its execution host reads and answers the
        // encrypted private chat; the server only routes ciphertext.
        if let Some(on)=self.view.check_box(cx,ids!(encrypt_owner_direct)).changed(actions) {
            if on && self.direct_encrypting.is_none() && self.agent(cx).as_deref()==Some(self.direct_agent.as_str()) && self.direct_state["state"] == "active" && Service::for_current().is_ok_and(|s|s.identity()==self.identity)
                && let Ok(room_id)=matrix_sdk::ruma::OwnedRoomId::try_from(string(&self.direct_state,"roomId")) {
                self.direct_encrypting=Some(room_id.to_string());
                self.status(cx,"Turning on end-to-end encryption…");
                crate::sliding_sync::submit_async_request(crate::sliding_sync::MatrixRequest::EnableEncryption {room_id});
            }
            // Always show the real state; a refused change snaps back.
            self.render_owner_direct(cx);
            return;
        }
        if self.view.button(cx, ids!(assign_current_device)).clicked(actions) {
            let Some(agent_id) = self.agent(cx) else {return;};
            if !self.devices_loaded || string(&self.execution_agent_record,"id") != agent_id || !self.view.check_box(cx, ids!(assignment_confirm)).active(cx) {return;}
            let Some(expected_generation) = self.execution_agent_record["generation"].as_i64() else {return;};
            self.run(cx, Target::AgentAssigned, Command::AssignAgentToCurrentDevice {agent_id, expected_generation});
            return;
        }
        if self.view.button(cx, ids!(service_toggle)).clicked(actions) {
            if self.view.button(cx,ids!(service_toggle)).borrow().is_none_or(|button|!button.enabled()) {return;}
            if self.runtime_room_active {
                self.local(cx,Target::Runtime,LocalAction::StopAgentService,None);
                return;
            }
            if !self.local_agent(cx) {
                self.runtime_feedback(cx,"Assign this Agent to this device before starting it here."); return;
            }
            let selected_model=self.selected(cx,ids!(model),&self.provider_models);
            if selected_model.as_ref().is_none_or(|m|string(m,"model") != string(&self.policy["modelProfile"],"model")) || string(&self.policy["modelProfile"],"model").is_empty() {
                self.runtime_feedback(cx,"Save the selected model or profile before starting this Room.");return;
            }
            let effort=self.runtime_efforts.get(self.view.drop_down(cx,ids!(effort)).selected_item()).cloned();
            let Some(effort)=effort else {self.runtime_feedback(cx,"Load local resources and select a supported reasoning effort before starting.");return;};
            let saved_effort=string(&self.policy["modelProfile"],"reasoning_effort");
            if !saved_effort.is_empty() && saved_effort!=effort {self.runtime_feedback(cx,"Save the selected reasoning effort in model settings before starting.");return;}
            let shared_workspace=self.view.check_box(cx,ids!(shared_workspace)).active(cx);
            if shared_workspace && self.provider_runtime(cx)!="codex" {self.runtime_feedback(cx,"Shared workspace editing currently supports Codex only.");return;}
            let shared_workspace=shared_workspace.then(||string(&self.policy["modelProfile"],"workspace_root").to_owned());
            let input = runtime_start_input(self.view.check_box(cx,ids!(takeover)).active(cx), effort, shared_workspace);
            self.local(cx, Target::Runtime, LocalAction::StartAgentService, Some(input));
            return;
        }
        if self.view.button(cx, ids!(runtime_check)).clicked(actions) {
            self.load_runtime_resources(cx);
            return;
        }
        if self.view.button(cx, ids!(approvals_check)).clicked(actions) {
            self.refresh_approvals(cx);
            return;
        }
        if self
            .view
            .drop_down(cx, ids!(approvals))
            .selected(actions)
            .is_some()
        {
            self.render_approval_details(cx);self.sync_navigation(cx);
            return;
        }
        if self.view.button(cx, ids!(approve)).clicked(actions)
            || self.view.button(cx, ids!(reject)).clicked(actions)
        {
            if let Some(p) = self.selected(cx, ids!(approvals), &self.approvals) {
                if self.busy || !self.local_agent(cx) || !self.approval_checked || self.approval_blocked.contains(string(&p,"proposalId"))
                    || !self.approval_scope(cx).is_some_and(|(a,b,r)|super::approvals::matches(&p,&a,&b,&r,super::approvals::now())) {return;}
                let approved = self.view.button(cx, ids!(approve)).clicked(actions);
                self.approval_blocked.insert(string(&p,"proposalId").into());
                self.view.label(cx,ids!(approval_feedback)).set_text(cx,crate::i18n::tr("Submitting decision…"));
                self.local(
                    cx,
                    Target::ApprovalDecision,
                    LocalAction::Decision {
                        approval_id: string(&p, "proposalId").into(),
                    },
                    Some(json!({"argsDigest":p["argsDigest"],"approved":approved})),
                );
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.sync_navigation(cx);
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let is_members =
                step.widget_uid() == self.view.portal_list(cx, ids!(members_list)).widget_uid();
            let Some(mut list) = step.borrow_mut::<PortalList>() else {
                continue;
            };
            if step.widget_uid() == self.view.portal_list(cx,ids!(service_rooms)).widget_uid() {
                let rooms=array(&self.agent_service_state["service"],"rooms");
                list.set_item_range(cx,0,rooms.len().max(1));
                while let Some(index)=list.next_visible_item(cx) {
                    let Some(binding)=rooms.get(index) else {list.item(cx,index,if index==0 && rooms.is_empty() {id!(Empty)} else {id!(Filler)}).draw_all(cx,scope);continue;};
                    let row=list.item(cx,index,id!(Room));
                    self.service_row_targets.insert(row.widget_uid(),(self.agent(cx).unwrap_or_default(),string(binding,"id").into()));
                    let name=self.binding_label(cx,binding);
                    row.label(cx,ids!(room_name)).set_text(cx,&name);
                    let paused=binding["ownerServicePaused"] == true;
                    let status=if paused {"Paused · requests receive a pause notice"} else if binding["state"] == "pending" {"Joining…"} else if binding["state"] != "active" {"Unavailable · check membership and permissions"} else if !self.runtime_room_active {"Service stopped on this device"} else if array(&self.agent_service_state["service"],"problems").iter().any(|p|string(p,"bindingId")==string(binding,"id")) {"Waiting to retry · check service status"} else {"Enabled · limits and permissions apply"};
                    row.label(cx,ids!(room_status)).set_text(cx,crate::i18n::tr(status));
                    row.button(cx,ids!(room_toggle)).set_text(cx,crate::i18n::tr(if paused {"Resume"} else {"Pause"}));
                    row.button(cx,ids!(room_toggle)).set_enabled(cx,!self.busy && (paused || binding["state"] == "active"));
                    let mut button=row.button(cx,ids!(room_toggle));
                    if paused {script_apply_eval!(cx,button,{draw_icon +: {svg: mod.widgets.ICON_SERVICE_PLAY}});} else {script_apply_eval!(cx,button,{draw_icon +: {svg: mod.widgets.ICON_SERVICE_PAUSE}});}
                    row.draw_all(cx,scope);
                }
                continue;
            }
            if is_members {
                let total = self.members.len();
                if list.first_id() >= total {
                    list.set_first_id_and_scroll(0, 0.0);
                }
                list.set_item_range(cx, 0, total);
                while let Some(index) = list.next_visible_item(cx) {
                    if let Some(member) = self.members.get(index) {
                        let row = list.item(cx, index, id!(Member));
                        row.label(cx, ids!(name))
                            .set_text(cx, string(member, "name"));
                        row.label(cx, ids!(identity))
                            .set_text(cx, string(member, "userId"));
                        row.label(cx, ids!(membership))
                            .set_text(cx, &member_summary(member));
                        row.draw_all(cx, scope);
                    } else {
                        list.item(cx, index, id!(Filler)).draw_all(cx, scope);
                    }
                }
                continue;
            }
            let rows = if self.page == Page::Projects {
                &self.projects
            } else {
                &self.agents
            };
            let total = rows.len().max(1);
            if list.first_id() >= total {
                list.set_first_id_and_scroll(0, 0.0);
            }
            list.set_item_range(cx, 0, total);
            while let Some(index) = list.next_visible_item(cx) {
                let Some(value) = rows.get(index) else {
                    let row = list.item(cx, index, card_template(rows.len(), index));
                    if rows.is_empty() && index == 0 {
                        row.label(cx, ids!(empty_message)).set_text(
                            cx,
                            crate::i18n::tr(if self.page == Page::Projects {
                                "No Agent spaces yet. Choose Agent space in the creation wizard to get started."
                            } else {
                                agent_empty_message(self.agents_loaded, self.agents_loading)
                            }),
                        );
                    }
                    row.draw_all(cx, scope);
                    continue;
                };
                let row = list.item(cx, index, id!(Card));
                let title = if self.page == Page::Projects {
                    string(value, "name")
                } else {
                    string(value, "displayName")
                };
                row.label(cx, ids!(title))
                    .set_text(cx, if title.is_empty() { "Untitled" } else { title });
                let summary = if self.page == Page::Projects {
                    let mut description = crate::i18n::tr("Connected Matrix Space").to_owned();
                    if let Some(count) = value["roomCount"].as_u64() {
                        description.push_str(&format!(" · {count} Rooms"));
                    }
                    let topic = string(value, "topic");
                    if !topic.is_empty() {
                        description.push_str(&format!("\n{topic}"));
                    }
                    description
                } else {
                    let identity_state=crate::i18n::tr(if string(value,"state")=="active" {"Server identity active"} else {"Server identity pending"});
                    let local=match agent_device_state(value,&self.current_device_id,&self.execution_devices,self.devices_loaded) {
                        AgentDeviceState::Here=>runtime_service_description(self.runtime_list.get(string(value,"id")),string(value,"id")),
                        AgentDeviceState::Other=>crate::i18n::tr("Processing is assigned to another device; this device does not run it.").to_owned(),
                        AgentDeviceState::Checking=>crate::i18n::tr("Checking execution device…").to_owned(),
                        _=>crate::i18n::tr("No available execution device. Open Agent settings to assign this device.").to_owned(),
                    };
                    format!("{identity_state} · {}\n{local}", string(value,"puppetMxid"))
                };
                row.view(cx, ids!(device_badge)).set_visible(cx, self.page == Page::Agents);
                if self.page == Page::Agents {
                    let badge = row.widget(cx, ids!(device_badge));
                    let state=agent_device_state(value, &self.current_device_id, &self.execution_devices, self.devices_loaded);
                    render_device_badge(cx, &badge, state, &self.device_badge(value));
                    let runtime=row.view(cx,ids!(runtime_badge));runtime.set_visible(cx,state==AgentDeviceState::Here);
                    let label=runtime_list_label(self.runtime_list.get(string(value,"id")),string(value,"id"));
                    let badge=if label.starts_with("Not running") {"Not running"} else if label.starts_with("Running") || label.starts_with("Processing") {"Running"} else if label.starts_with("Service enabled") {"Enabled"} else if label.starts_with("Starting") {"Starting…"} else if label.starts_with("Checking") {"Checking…"} else if label=="Waiting for the previous runtime lease to be released…" {"Waiting for lease"} else {"Needs attention"};
                    runtime.label(cx,ids!(device_label)).set_text(cx,crate::i18n::tr(badge));
                }
                self.agent_card_targets.insert(row.widget_uid(),string(value,"id").into());
                let control=row.button(cx,ids!(service_control));
                let here=self.page==Page::Agents && agent_device_state(value,&self.current_device_id,&self.execution_devices,self.devices_loaded)==AgentDeviceState::Here;
                control.set_visible(cx,here);
                let status=self.runtime_list.get(string(value,"id")).and_then(|r|r.as_ref().ok());
                let enabled=status.is_some_and(|v|service_enabled(v));
                control.set_enabled(cx,here && status.is_some() && !self.busy && (enabled || string(value,"state")=="active"));
                control.set_text(cx,crate::i18n::tr(if self.busy && self.list_service_pending_agent==string(value,"id") {"Working…"} else if enabled {"Stop"} else {"Start"}));
                let mut control=control;
                if enabled {script_apply_eval!(cx,control,{draw_icon +: {svg: mod.widgets.ICON_SERVICE_STOP}});} else {script_apply_eval!(cx,control,{draw_icon +: {svg: mod.widgets.ICON_SERVICE_PLAY}});}
                row.label(cx, ids!(summary)).set_text(cx, &summary);
                row.label(cx, ids!(summary))
                    .set_visible(cx, !summary.is_empty());
                row.draw_all(cx, scope);
            }
        }
        DrawStep::done()
    }
}
impl HagencyPanelRef {
    pub fn action(&self, cx: &mut Cx, action: &HagencyAction) {
        let Some(mut panel) = self.borrow_mut() else {
            return;
        };
        panel.cancel_direct_retry(cx);
        panel.cancel_read_wait(cx);
        if matches!(action,HagencyAction::AgentRun {..}|HagencyAction::OpenAgent {..}|HagencyAction::NewAgent {..}|HagencyAction::NewProject) {panel.reset_detail_scroll(cx);}
        panel.wanted_run=None;
        if matches!(action, HagencyAction::Close) {
            panel.active = false;
            panel.request = nonce();
            return;
        }
        let service = match Service::for_current() {
            Ok(s) => s,
            Err(error) => {
                panel.display_blocked=true;
                panel.status(cx, &error);
                return;
            }
        };
        panel.display_blocked=false;
        let identity = service.identity();
        if panel.identity != identity {
            panel.clear_members(cx);
            panel.projects.clear();
            panel.rooms.clear();
            panel.agent_rooms.clear();
            panel.spaces.clear();
            panel.agents.clear(); panel.runtime_list.clear();panel.runtime_list_loading=false;panel.runtime_list_request=nonce();
            panel.devices_loaded = false; panel.current_device_id.clear(); panel.execution_devices.clear(); panel.execution_agent_record = Value::Null;
            panel.agents_loaded = false;
            panel.agents_loading = false;
            panel.bindings.clear();
            panel.clear_approval_state(cx);panel.approval_blocked.clear();
            panel.approvals.clear();
            panel.clear_configuration(cx);panel.configuration_keys.clear();
            panel.policy = Value::Null;
            panel.creation = None;
            panel.pending_input = None;
            panel.credential_ref.clear();
            panel.direct_agent.clear();panel.direct_state=Value::Null;panel.wanted_agent=None;
            panel.provider_url = None;
            panel.provider_models.clear();panel.runtime_efforts.clear();panel.provider_authenticated=false;panel.runtime_resources_loading=false;panel.runtime_resources_checked=false;
            panel.agent_command = None;
            for id in [
                ids!(creation_name),
                ids!(existing_room),
                ids!(found_room),
                ids!(agent_name),
                ids!(requester),
                ids!(token_limit),
                ids!(allowed_tools),
                ids!(allowed_directories),
                ids!(model),
                ids!(workspace),
            ] {
                panel.view.text_input(cx, id).set_text(cx, "");
            }
            for id in [
                ids!(credential),
                ids!(approval_details),
                ids!(creation_result),
                ids!(project_details),
                ids!(agent_details),
                ids!(room_details),
                ids!(runtime_status),
                ids!(provider_status),
                ids!(policy_usage),
            ] {
                panel.view.label(cx, id).set_text(cx, "");
            }
            for id in [ids!(takeover), ids!(host_files)] {
                panel
                    .view
                    .check_box(cx, id)
                    .set_active(cx, false, Animate::No);
            }
        }
        panel.wanted_project = None;
        panel.wanted_project_section = None;
        panel.list_page(cx);
        panel.identity = identity;
        panel.active = true;
        panel.agent_journal_loaded = false;
        panel.agent_pending_commands.clear();
        panel.busy = false;
        panel.request = nonce();
        panel
            .view
            .label(cx, ids!(owner))
            .set_text(cx, crate::i18n::tr("Agent spaces and local agents for your signed-in account."));
        panel
            .view
            .view(cx, ids!(project_form))
            .set_visible(cx, matches!(action, HagencyAction::NewProject));
        match action {
            HagencyAction::OpenProjects
            | HagencyAction::NewProject
            | HagencyAction::ProjectDetails { .. }
            | HagencyAction::ProjectSection { .. } => {
                panel.set_page(cx, Page::Projects);
                if matches!(action, HagencyAction::NewProject) {
                    panel
                        .view
                        .view(cx, ids!(project_list_holder))
                        .set_visible(cx, false);
                    panel
                        .view
                        .view(cx, ids!(project_form))
                        .set_visible(cx, true);
                }
                panel.create_room = false;
                panel.view.drop_down(cx, ids!(creation_source)).set_labels(
                    cx,
                    vec![
                        crate::i18n::tr("Create a new Space (default)").into(),
                        crate::i18n::tr("Connect an existing Space").into(),
                    ],
                );
                panel
                    .view
                    .label(cx, ids!(creation_heading))
                    .set_text(cx, crate::i18n::tr("Create Matrix workspace"));
                panel
                    .view
                    .drop_down(cx, ids!(creation_source))
                    .set_selected_item(cx, 0);
                panel.creation_source(cx);
                if let HagencyAction::ProjectDetails { project_id } = action {
                    panel.wanted_project = Some(project_id.clone());
                }
                if let HagencyAction::ProjectSection {
                    project_id,
                    section,
                } = action
                {
                    panel.wanted_project = Some(project_id.clone());
                    panel.wanted_project_section = Some((project_id.clone(), *section));
                }
                panel.run(cx, Target::Journal, Command::Creations);
            }
            HagencyAction::OpenAgents | HagencyAction::OpenAgent { .. } | HagencyAction::NewAgent { .. } | HagencyAction::AgentRun {..} => {
                panel.set_page(cx, Page::Agents);
                if let HagencyAction::AgentRun {agent_id,binding_id}=action {panel.wanted_agent=Some(agent_id.clone()); panel.wanted_run=Some((agent_id.clone(),binding_id.clone()));}
                if let HagencyAction::OpenAgent {agent_id}=action {panel.wanted_agent=Some(agent_id.clone());}
                if matches!(action, HagencyAction::NewAgent { .. }) {
                    panel
                        .view
                        .view(cx, ids!(agent_name_holder))
                        .set_visible(cx, true);
                    panel
                        .view
                        .button(cx, ids!(create_agent))
                        .set_visible(cx, true);
                    panel
                        .view
                        .button(cx, ids!(bind_agent))
                        .set_visible(cx, false);
                    panel
                        .view
                        .view(cx, ids!(agent_list_holder))
                        .set_visible(cx, false);
                    panel
                        .view
                        .view(cx, ids!(agent_create))
                        .set_visible(cx, true);
                }
                if let HagencyAction::NewAgent { project_id } = action {
                    panel.wanted_project = project_id.clone();
                }
                panel.run(cx, Target::AgentJournal, Command::AgentCommands);
            }
            HagencyAction::Close => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_start_button_body_matches_the_native_request_parser() {
        for takeover in [false, true] {
            let body = runtime_start_input(takeover, "high".into(), None);
            let parsed: ::hagency::native_owner::RuntimeStartRequest = serde_json::from_value(body.clone()).unwrap();
            assert_eq!(parsed.takeover, takeover);
            assert_eq!(parsed.effort.as_deref(), Some("high"));
            assert!(parsed.recovery_device_id.is_none() && parsed.recovery_consent_id.is_none());
            assert!(body.get("mode").is_none() && body.get("reservation").is_none());
            // A start without the opt-in sends the same body as before it existed.
            assert!(!parsed.shared_workspace && parsed.expected_workspace.is_none());
            assert!(body.get("sharedWorkspace").is_none() && body.get("expectedWorkspace").is_none());
        }
        let body = runtime_start_input(false, "high".into(), Some("/project".into()));
        assert_eq!(body["sharedWorkspace"], true);
        assert_eq!(body["expectedWorkspace"], "/project");
        let parsed: ::hagency::native_owner::RuntimeStartRequest = serde_json::from_value(body).unwrap();
        assert!(parsed.shared_workspace && parsed.expected_workspace.as_deref() == Some("/project"));
    }
    #[test]
    fn web_search_default_toggle_save_and_reload_use_the_agent_profile() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm| {makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.policy=json!({"modelLoaded":true,"modelProfile":{"model":"probe","credential_ref":"keychain:probe","workspace_root":"/work"}});
        panel.render_policy(&mut cx);
        assert!(panel.view.check_box(&mut cx,ids!(web_search)).active(&mut cx));
        panel.view.check_box(&mut cx,ids!(web_search)).set_active(&mut cx,false,Animate::No);
        let saved=panel.model_save_input(&mut cx,&json!({"model":"probe"}),"low").unwrap();
        assert_eq!(saved["profile"]["web_search"],false);
        // The save names the profile that was read: its digest goes with it,
        // and another one is sent once the saved profile is read back.
        let digest=saved["expectedProfileDigest"].as_str().unwrap().to_owned();
        assert!(digest.len()==64 && saved.as_object().unwrap().len()==2);
        let none=std::mem::replace(&mut panel.policy["modelProfile"],Value::Null);
        let first=panel.model_save_input(&mut cx,&json!({"model":"probe"}),"low").unwrap();
        assert_ne!(first["expectedProfileDigest"],digest);
        panel.policy["modelProfile"]=json!({"model":1});
        assert!(panel.model_save_input(&mut cx,&json!({"model":"probe"}),"low").is_none());
        panel.policy["modelProfile"]=none;
        panel.view.check_box(&mut cx,ids!(web_search)).set_active(&mut cx,true,Animate::No);
        panel.policy["modelProfile"]=saved["profile"].clone();
        panel.render_policy(&mut cx);
        assert!(!panel.view.check_box(&mut cx,ids!(web_search)).active(&mut cx));
    }
    #[test]
    fn remote_configuration_retry_window_ends_after_confirmed_save_or_validation_failure() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm| {makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        let digest="account:a:3:model-A".to_owned();
        let first_key=uuid::Uuid::new_v4().to_string();
        panel.configuration_keys.insert(digest.clone(),first_key.clone());
        panel.configuration_status=json!({"online":true,"cached":false});
        panel.finish_configuration_intent(Some(&digest),&Err("configuration_outcome_unknown".into()));
        assert_eq!(panel.configuration_keys[&digest],first_key,"unconfirmed saves reuse the original idempotency key");
        assert!(panel.configuration_status["online"]==true,"unknown application result does not assert device offline");
        panel.finish_configuration_intent(Some(&digest),&Err("invalid_configuration_response".into()));
        assert_eq!(panel.configuration_keys[&digest],first_key,"an invalid post-save response cannot prove that no write occurred");
        panel.finish_configuration_intent(Some(&digest),&Ok(json!({"agentId":"a"})));
        assert!(!panel.configuration_keys.contains_key(&digest));
        let later_key=panel.configuration_keys.entry(digest.clone()).or_insert_with(||uuid::Uuid::new_v4().to_string()).clone();
        assert_ne!(later_key,first_key,"model A → B → A cannot replay the first A save receipt");
        panel.finish_configuration_intent(Some(&digest),&Err("invalid_local_policy".into()));
        assert!(!panel.configuration_keys.contains_key(&digest));
        assert!(panel.configuration_status["online"]==true,"a validation failure leaves the execution device online");
        let validation=panel.configuration_feedback.clone();
        panel.finish_configuration_intent(None,&Ok(json!({"configuration":{"online":true}})));
        assert_eq!(panel.configuration_feedback,validation,"quiet metadata reads preserve visible mutation errors");
        panel.finish_configuration_intent(None,&Err("configuration_device_offline".into()));
        assert!(panel.configuration_status["online"]==false);assert!(panel.configuration_status["cached"]==true);
        assert_eq!(panel.configuration_feedback,validation);
    }
    #[test]
    fn remote_configuration_allowlist_rejects_room_credentials_and_runtime_controls() {
        let local=|action,binding_id,requester|Command::Local {agent_id:"a".into(),action,input:None,binding_id,requester};
        for action in [LocalAction::AgentModel,LocalAction::AgentPolicy,LocalAction::SaveAgentModel,LocalAction::SaveAgentPolicy] {
            assert!(remote_configuration_command(&local(action.clone(),None,None)));
            assert!(!remote_configuration_command(&local(action.clone(),Some("room".into()),None)));
            assert!(!remote_configuration_command(&local(action,None,Some("member".into()))));
        }
        for action in [LocalAction::Policy,LocalAction::SavePolicy,LocalAction::ResetAgentPolicy,LocalAction::ResetPolicy,LocalAction::RuntimeStart,LocalAction::RuntimeStop,LocalAction::Approvals,LocalAction::StartAgentService,LocalAction::StopAgentService] {
            assert!(!remote_configuration_command(&local(action,None,None)));
        }
        for action in [ProviderAction::Status,ProviderAction::Models,ProviderAction::UseLocal] {
            assert!(remote_configuration_command(&Command::RuntimeProvider {runtime:"octos".into(),action}));
        }
        for action in [ProviderAction::Login,ProviderAction::Logout,ProviderAction::Disconnect,ProviderAction::Cancel] {
            assert!(!remote_configuration_command(&Command::Provider {action}));
        }
    }
    #[test]
    fn remote_configuration_metadata_rejects_stale_generation_agent_and_device() {
        let agent=json!({"id":"a","generation":3,"executionDeviceId":"A"});
        let fresh=json!({"agentId":"a","agentGeneration":3,"executionDeviceId":"A","deviceGeneration":2});
        assert!(configuration_matches(&fresh,&agent));
        for (field,wrong) in [("agentId",json!("b")),("agentGeneration",json!(2)),("executionDeviceId",json!("B")),("deviceGeneration",Value::Null)] {
            let mut stale=fresh.clone();stale[field]=wrong;
            assert!(!configuration_matches(&stale,&agent),"{field} must bind the cached snapshot");
        }
    }
    #[test]
    fn cached_remote_environment_uses_target_catalog_and_disables_writes_and_local_controls() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm| {makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.agents=vec![json!({"id":"a","generation":3,"executionDeviceId":"A","state":"active"})];
        panel.labels(&mut cx,ids!(agents),"Select",&panel.agents,"id");panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        panel.execution_agent_record=panel.agents[0].clone();panel.current_device_id="B".into();panel.devices_loaded=true;
        panel.execution_devices=vec![json!({"id":"A","name":"Execution workstation","revoked":false}),json!({"id":"B","revoked":false})];
        panel.section(&mut cx,ids!(model_section));
        panel.configuration_status=json!({"agentId":"a","agentGeneration":3,"executionDeviceId":"A","deviceGeneration":1,"online":false,"cached":true,"observedAtMs":1710000000000i64,"lastSeenAtMs":1710000000001i64});
        panel.provider_models=vec![json!({"model":"B-only"})];panel.credential_ref="B-credential".into();
        let reference="octos-managed:shared-home:A:review";
        let env=json!({"providers":[{"runtime":"octos","installed":true,"version":"1.2.3","versionSupported":true,"state":"ready","authenticated":true,"accountVerified":false,"credentialRef":reference,"shared":true,"models":[{"model":"A-only","credentialRef":reference,"displayName":"A profile","isDefault":true,"defaultReasoningEffort":"none","supportedReasoningEfforts":[{"reasoningEffort":"none"}]}]}],"configuration":{"agentId":"a","policy":{"revision":1,"policy":{"budget":{"limit":"Unlimited","period":"Lifetime"},"requests":"Allow","high_risk":"Deny"}},"usage":{"spent":2,"held":0},"modelProfile":{"model":"A-only","credential_ref":reference,"workspace_root":"/A/workspace","reasoning_effort":"none"}}});
        panel.apply(&mut cx,Target::ExecutionEnvironment,&env);panel.sync_navigation(&mut cx);
        assert_eq!(panel.provider_models.len(),1);assert_eq!(panel.provider_models[0]["model"],"A-only");assert_eq!(panel.selected_credential(&mut cx),reference);
        assert_eq!(panel.view.text_input(&mut cx,ids!(workspace)).text(),"/A/workspace");
        assert!(panel.view.label(&mut cx,ids!(workspace_label)).text().contains("Execution workstation"));
        assert!(panel.view.label(&mut cx,ids!(configuration_notice)).text().contains("Cached observation"));
        assert!(panel.view.label(&mut cx,ids!(provider_status)).text().contains("1.2.3"));
        for id in [ids!(save_model),ids!(save_policy),ids!(prepare_workspace),ids!(use_local_provider),ids!(runtime_tab)] {
            assert!(!panel.view.button(&mut cx,id).borrow().unwrap().enabled());
        }
        for id in [ids!(provider_login),ids!(provider_logout),ids!(disconnect_provider),ids!(reset_policy)] {assert!(!panel.view.button(&mut cx,id).visible());}
        panel.configuration_status["online"]=true.into();panel.configuration_status["cached"]=false.into();panel.sync_navigation(&mut cx);
        assert!(panel.view.button(&mut cx,ids!(save_model)).borrow().unwrap().enabled());
        assert!(panel.view.button(&mut cx,ids!(save_policy)).borrow().unwrap().enabled());
        let mut status=panel.configuration_status.clone();status["deviceGeneration"]=2.into();status["online"]=false.into();status["environment"]=Value::Null;
        panel.apply(&mut cx,Target::ConfigurationStatus,&json!({"configuration":status}));
        assert!(panel.provider_models.is_empty(),"a different execution device generation cannot display the old catalog");
        assert!(panel.policy.is_null());assert_eq!(panel.view.text_input(&mut cx,ids!(workspace)).text(),"");
        panel.clear_configuration(&mut cx);panel.sync_navigation(&mut cx);
        assert!(panel.provider_models.is_empty());assert!(panel.credential_ref.is_empty());assert!(panel.policy.is_null());
        assert!(!panel.view.button(&mut cx,ids!(save_model)).borrow().unwrap().enabled());
        panel.finish_configuration_intent(None,&Err("unsupported_hagency_capabilities".into()));panel.sync_navigation(&mut cx);
        assert!(panel.view.label(&mut cx,ids!(configuration_notice)).text().contains("does not support execution device configuration"));
        assert!(!panel.view.label(&mut cx,ids!(configuration_notice)).text().contains("Checking execution device environment"));
    }
    #[test]
    fn external_runtime_resources_restore_kind_and_exact_octos_profile() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            HagencyPanel::script_from_value(vm, value)
        });
        for (kind, index, reference) in [("claude",1,"claude-managed:shared-home:saved"), ("octos",2,"octos-managed:shared-home:saved:review")] {
            panel.policy=json!({"modelLoaded":true,"modelProfile":{"model":"same","credential_ref":reference,"reasoning_effort":"none"}});
            panel.render_policy(&mut cx);
            assert_eq!(panel.view.drop_down(&mut cx,ids!(runtime_kind)).selected_item(),index);
            panel.apply(&mut cx,Target::ProviderModels,&json!({"runtime":kind,"authenticated":true,"shared":true,"credentialRef":"provider-level-reference","models":[
                {"model":"same","displayName":"Coding","credentialRef":"other-reference","isDefault":true,"defaultReasoningEffort":"none","supportedReasoningEfforts":[{"reasoningEffort":"none"}]},
                {"model":"same","displayName":"Review","credentialRef":reference,"defaultReasoningEffort":"none","supportedReasoningEfforts":[{"reasoningEffort":"none"}]}
            ]}));
            assert_eq!(panel.view.drop_down(&mut cx,ids!(model)).selected_item(),2);
            assert_eq!(panel.selected_credential(&mut cx),reference);
            assert_eq!(panel.runtime_efforts,vec!["none"]);
            assert!(!panel.view.button(&mut cx,ids!(provider_login)).visible());
            assert!(!panel.view.button(&mut cx,ids!(provider_logout)).visible());
        }
    }
    #[test]
    fn approval_widget_filters_rooms_keeps_selection_and_disables_stale_decisions() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);vm.bx.captured_errors=Some(Vec::new());let v=script_eval!(vm,{mod.widgets.HagencyPanel {}});let p=HagencyPanel::script_from_value(vm,v);assert!(vm.take_errors().is_empty());p});
        panel.agents=vec![json!({"id":"a","executionDeviceId":"device-here","state":"active","generation":1})];
        local_device_fixture(&mut panel,"a");
        panel.labels(&mut cx,ids!(agents),"Select my Agent",&panel.agents,"id");
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        panel.bindings=vec![json!({"id":"b1","roomId":"!one:s"}),json!({"id":"b2","roomId":"!two:s"})];
        panel.labels(&mut cx,ids!(bindings),"Select a Room binding",&panel.bindings,"id");
        panel.select_policy_room(&mut cx,1);
        let agent=panel.agent(&mut cx).unwrap();
        let row=|id:&str,b:&str,r:&str|json!({"proposalId":id,"argsDigest":"a".repeat(64),"proposal":{"scope":{"agent":agent,"binding":b,"room":r,"requester":"@owner:s"},"tool":"model.request","arguments":{"inputPreview":"Exact request"},"risk":"model_request","expires":crate::hagency::approvals::now()+300}});
        let data=json!({"approvals":[row("one","b1","!one:s"),row("two","b2","!two:s"),row("wrong-room","b1","!two:s")]});
        panel.apply_approvals(&mut cx,&data);
        assert_eq!(panel.approvals.len(),1);assert_eq!(panel.approvals[0]["proposalId"],"one");
        panel.view.drop_down(&mut cx,ids!(approvals)).set_selected_item(&mut cx,1);
        panel.apply_approvals(&mut cx,&data);
        assert_eq!(panel.view.drop_down(&mut cx,ids!(approvals)).selected_item(),1);
        panel.sync_navigation(&mut cx);assert!(panel.view.button(&mut cx,ids!(approve)).borrow().unwrap().enabled());
        panel.approval_blocked.insert("one".into());panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(approve)).borrow().unwrap().enabled());
        panel.approval_blocked.clear();panel.approval_checked=false;panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(approve)).borrow().unwrap().enabled());
        panel.approval_checked=true;panel.approvals[0]["proposal"]["expires"]=json!(0);panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(approve)).borrow().unwrap().enabled());
        panel.active=true;panel.page=Page::Agents;
        panel.view.view(&mut cx,ids!(agent_detail)).set_visible(&mut cx,true);
        panel.view.view(&mut cx,ids!(runtime_section)).set_visible(&mut cx,true);
        panel.identity="owner-session".into();panel.approval_poll=Some(12);
        let mut late=ApprovalsCompleted {identity:panel.identity.clone(),request:12,agent:agent.clone(),binding:"b1".into(),room:"!one:s".into(),result:Ok(data.clone())};
        assert!(panel.accepts_approval_poll(&mut cx,&late));
        panel.page=Page::Projects;assert!(!panel.accepts_approval_poll(&mut cx,&late));panel.page=Page::Agents;
        late.identity="old-session".into();assert!(!panel.accepts_approval_poll(&mut cx,&late));late.identity=panel.identity.clone();
        panel.select_policy_room(&mut cx,2);assert!(panel.approval_poll.is_none());assert!(panel.approvals.is_empty());assert!(!panel.accepts_approval_poll(&mut cx,&late));
        panel.apply_approvals(&mut cx,&data);assert_eq!(panel.approvals.len(),1);assert_eq!(panel.approvals[0]["proposalId"],"two");
        panel.active=false;
        panel.apply(&mut cx,Target::ApprovalDecision,&json!({"decided":true}));
        assert_eq!(panel.view.label(&mut cx,ids!(approval_feedback)).text(),crate::i18n::tr("Decision submitted. Execution is not yet confirmed. Check the Room reply and runtime status."));
        panel.apply(&mut cx,Target::ApprovalDecision,&Value::Null);
        assert_eq!(panel.view.label(&mut cx,ids!(approval_feedback)).text(),crate::i18n::tr("Decision delivery is unconfirmed. Refresh status; do not resubmit this request."));
    }

    use super::*;
    fn local_device_fixture(panel: &mut HagencyPanel, agent: &str) {
        panel.execution_agent_record = json!({"id":agent,"executionDeviceId":"device-here","generation":1});
        panel.current_device_id = "device-here".into();
        panel.execution_devices = vec![json!({"id":"device-here","name":"This computer","revoked":false})];
        panel.devices_loaded = true;
    }
    #[test]
    fn registered_management_lists_fill_available_height_and_badges_share_a_row() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);vm.bx.captured_errors=Some(Vec::new());let v=script_eval!(vm,{mod.widgets.HagencyPanel {}});let p=HagencyPanel::script_from_value(vm,v);assert!(vm.take_errors().is_empty());p});
        panel.projects=vec![json!({"id":"p1","name":"First project"}),json!({"id":"p2","name":"Second project"})];
        panel.agents=vec![json!({"id":"a1","displayName":"First agent","generation":1,"state":"active","executionDeviceId":"here","puppetMxid":"@agent1:server"}),json!({"id":"a2","displayName":"Second agent","generation":1,"state":"active","executionDeviceId":"here","puppetMxid":"@agent2:server"})];
        panel.devices_loaded=true;panel.current_device_id="here".into();panel.execution_devices=vec![json!({"id":"here","revoked":false})];
        for page in [Page::Projects,Page::Agents] {for height in [760.0,500.0] {
            panel.set_page(&mut cx,page);panel.list_page(&mut cx);
            {cx.redraw_all();let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(980.0,height));let mut dl=DrawList2d::new(&mut cx);let overlay=Overlay{draw_list:DrawList::new(&mut cx)};let event=DrawEvent::default();let mut draw=CxDraw::new(&mut cx,&event);let mut cx2d=Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass,None);dl.begin_always(&mut cx2d);overlay.begin(&mut cx2d);cx2d.begin_root_turtle(dvec2(980.0,height),Layout::flow_overlay());
            panel.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk::fixed(980.0,height));
            overlay.end(&mut cx2d);cx2d.end_pass_sized_turtle();dl.end(&mut cx2d);cx2d.end_pass(&pass);}
            let list=panel.view.portal_list(&mut cx,if page==Page::Projects {ids!(project_list)} else {ids!(agent_list)});
            let rect=list.area().rect(&cx);
            let intro=panel.view.view(&mut cx,ids!(list_intro)).area().rect(&cx);
            assert!(rect.pos.y>=intro.pos.y+intro.size.y,"list follows its guidance: {rect:?}, {intro:?}");
            assert!(rect.size.y>0.0 && (rect.pos.y+rect.size.y-(height-16.0)).abs()<1.0,"list fills all remaining body space: {rect:?}, height {height}");
            // A compact window scrolls cards below the guidance instead of
            // requiring every card to fit inside the initial viewport.
            let second=list.item(&mut cx,1,id!(Card)).area().rect(&cx);assert!(second.size.y>0.0 && second.pos.y>=rect.pos.y,"second card participates in the scrollable list: {second:?}");
            if page==Page::Agents {
                let row=list.item(&mut cx,0,id!(Card));let device=row.view(&mut cx,ids!(device_badge)).area().rect(&cx);let runtime=row.view(&mut cx,ids!(runtime_badge)).area().rect(&cx);
                assert!((device.pos.y-runtime.pos.y).abs()<1.0);assert!(device.size.y<=28.0 && runtime.size.y<=28.0);assert!(runtime.pos.x>=device.pos.x+device.size.x);
            }
        }}
        panel.set_page(&mut cx,Page::Projects);panel.view.view(&mut cx,ids!(project_detail)).set_visible(&mut cx,true);panel.project_section(&mut cx,ids!(members_section));panel.members=vec![json!({"name":"Alice","userId":"@alice:server","membership":"join"})];
        let height=760.0;{cx.redraw_all();let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(980.0,height));let mut dl=DrawList2d::new(&mut cx);let overlay=Overlay{draw_list:DrawList::new(&mut cx)};let event=DrawEvent::default();let mut draw=CxDraw::new(&mut cx,&event);let mut cx2d=Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass,None);dl.begin_always(&mut cx2d);overlay.begin(&mut cx2d);cx2d.begin_root_turtle(dvec2(980.0,height),Layout::flow_overlay());panel.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk::fixed(980.0,height));overlay.end(&mut cx2d);cx2d.end_pass_sized_turtle();dl.end(&mut cx2d);cx2d.end_pass(&pass);}
        let members=panel.view.portal_list(&mut cx,ids!(members_list)).area().rect(&cx);assert!(members.size.y>300.0,"members use remaining space: {members:?}");assert!(members.pos.y+members.size.y<=height,"members stay in panel: {members:?}");
        panel.display_blocked=true;panel.sync_navigation(&mut cx);assert!(!panel.view.view(&mut cx,ids!(details_scroll)).visible());panel.list_page(&mut cx);panel.sync_navigation(&mut cx);assert!(!panel.view.view(&mut cx,ids!(lists_pane)).visible(),"account authorization failure hides the new common list pane too");
    }
    #[test]
    fn lease_wait_is_explicit_in_list_room_and_dm_shared_summary() {
        let value=json!({"runtime":{"agentId":"a","phase":"waiting_for_lease"}});
        assert_eq!(runtime_list_label(Some(&Ok(value.clone())),"a"),"Waiting for the previous runtime lease to be released…");
        assert_eq!(runtime_summary(&value["runtime"],"a",Some("binding")),"Waiting for the previous runtime lease to be released…");
    }
    #[test]
    fn detail_scroll_resets_on_identity_target_change_but_preserves_same_detail() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        fn draw_small(cx:&mut Cx,panel:&mut HagencyPanel)->f64 {
            {cx.redraw_all();let pass=DrawPass::new(cx);pass.set_size(cx,dvec2(980.0,360.0));let mut dl=DrawList2d::new(cx);let overlay=Overlay{draw_list:DrawList::new(cx)};let e=DrawEvent::default();let mut draw=CxDraw::new(cx,&e);let mut cx2d=Cx2d::new(&mut draw);cx2d.begin_pass(&pass,None);dl.begin_always(&mut cx2d);overlay.begin(&mut cx2d);cx2d.begin_root_turtle(dvec2(980.0,360.0),Layout::flow_overlay());panel.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk::fixed(980.0,360.0));overlay.end(&mut cx2d);cx2d.end_pass_sized_turtle();dl.end(&mut cx2d);cx2d.end_pass(&pass);}
            panel.view.view(cx,ids!(agent_detail)).area().rect(cx).pos.y
        }
        let mut cx=Cx::new(Box::new(|_,_|{}));let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);vm.bx.captured_errors=Some(Vec::new());let v=script_eval!(vm,{mod.widgets.HagencyPanel {}});let p=HagencyPanel::script_from_value(vm,v);assert!(vm.take_errors().is_empty());p});
        panel.identity="owner".into();panel.agents=vec![json!({"id":"a","displayName":"First"}),json!({"id":"b","displayName":"Second"})];panel.labels(&mut cx,ids!(agents),"Select",&panel.agents,"id");panel.set_page(&mut cx,Page::Agents);panel.agent_detail(&mut cx,0);local_device_fixture(&mut panel,"a");panel.section(&mut cx,ids!(model_section));
        let top=draw_small(&mut cx,&mut panel);panel.view.view(&mut cx,ids!(details_scroll)).set_scroll_pos(&mut cx,dvec2(0.0,100.0));let scrolled=draw_small(&mut cx,&mut panel);assert!(scrolled<top-50.0,"real detail scrolling must move content: {top}, {scrolled}");
        panel.enter_detail(&mut cx,"agent","a");assert!((draw_small(&mut cx,&mut panel)-scrolled).abs()<1.0,"same detail preserves scrolling");
        panel.agent_detail(&mut cx,1);local_device_fixture(&mut panel,"b");panel.section(&mut cx,ids!(model_section));assert!((draw_small(&mut cx,&mut panel)-top).abs()<1.0,"cross-Agent entry resets to top");
        panel.view.view(&mut cx,ids!(details_scroll)).set_scroll_pos(&mut cx,dvec2(0.0,100.0));draw_small(&mut cx,&mut panel);panel.enter_detail(&mut cx,"project","same-id");assert!((draw_small(&mut cx,&mut panel)-top).abs()<1.0,"cross-Project target resets too");
        panel.view.view(&mut cx,ids!(details_scroll)).set_scroll_pos(&mut cx,dvec2(0.0,100.0));draw_small(&mut cx,&mut panel);panel.reset_detail_scroll(&mut cx);assert!((draw_small(&mut cx,&mut panel)-top).abs()<1.0,"explicit Run/new-form entry resets");
    }
    #[test]
    fn local_runtime_badge_does_not_confuse_server_identity_with_processing() {
        assert_eq!(runtime_list_label(None,"a"),"Checking local processing…");
        assert!(runtime_list_label(Some(&Ok(json!({"runtime":null}))),"a").starts_with("Not running"));
        assert!(runtime_list_label(Some(&Ok(json!({"runtime":{"agentId":"foreign","phase":"online"}}))),"a").contains("could not be verified"));
        assert!(runtime_list_label(Some(&Err("failure".into())),"a").contains("could not be verified"));
        assert!(runtime_list_label(Some(&Ok(json!({"runtime":{"agentId":"a","phase":"online"}}))),"a").contains("budgets and policy"));
        assert!(runtime_list_label(Some(&Ok(json!({"runtime":{"agentId":"a","phase":"stopped"}}))),"a").starts_with("Not running"));
        assert!(runtime_list_label(Some(&Ok(json!({"runtime":{"agentId":"a","phase":"reconciliation_only"}}))),"a").contains("Only known original replies"));
    }
    #[test]
    fn service_readiness_and_failures_are_consistent_in_list_and_details() {
        let starting=json!({"service":{"enabled":true,"watcherRunning":true,"phase":"starting"},"runtime":null});
        assert!(runtime_service_description(Some(&Ok(starting)),"a").starts_with("Starting"));
        let checking=json!({"service":{"enabled":true,"watcherRunning":true},"runtime":{"agentId":"a","phase":"checking_provider","bindings":[{"phase":"checking_provider"}]}});
        assert!(runtime_service_description(Some(&Ok(checking)),"a").starts_with("Checking this device"));
        let unknown=json!({"service":{"enabled":true,"watcherRunning":true},"runtime":{"agentId":"a","phase":"online","bindings":[{"phase":"online_chat_only","lastError":"profile_or_local_state_unavailable"}]}});
        assert!(runtime_service_description(Some(&Ok(unknown)),"a").starts_with("Running"));
        let stopped=json!({"service":{"enabled":true,"watcherRunning":true},"runtime":{"agentId":"a","phase":"stopped","lastError":"authorization_or_lease_lost","bindings":[]}});
        assert!(runtime_service_description(Some(&Ok(stopped)),"a").contains("stopped with an error"));
        let failed=json!({"service":{"enabled":true,"watcherRunning":false,"lastError":"authorization_or_lease_lost"},"runtime":null});
        let description=runtime_service_description(Some(&Ok(failed)),"a");
        assert!(description.contains("needs attention"));
        assert!(description.contains("authorization_or_lease_lost"));
        let retry=json!({"service":{"enabled":true,"watcherRunning":true,"problems":[{"error":"agent_lease_busy","retryInSeconds":8}]},"runtime":{"agentId":"a","phase":"stopped","lastError":"provider_authorization_lost"}});
        let description=runtime_service_description(Some(&Ok(retry)),"a");
        assert!(description.contains("could not start"));
        assert!(description.contains("agent_lease_busy"));
        assert!(description.contains("provider_authorization_lost"));
        assert!(description.contains("8s"));
        assert!(!runtime_diagnostic("secret /private/provider-stderr token=123").contains("secret"));
        // A Claude or Octos Agent's failed run is not worded as a Codex fault.
        for code in ["codex_protocol_mismatch", "codex_provider_timeout", "codex_provider_unavailable"] {
            let message = runtime_diagnostic(code);
            assert!(message.contains(code) && !message.replace(code, "").contains("Codex"), "{message}");
        }
    }
    #[test]
    fn service_detail_does_not_claim_readiness_and_preserves_action_failure() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.agents=vec![json!({"id":"a"})];
        panel.labels(&mut cx,ids!(agents),"Select",&panel.agents,"id");
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        local_device_fixture(&mut panel,"a");
        let checking=json!({"service":{"enabled":true,"watcherRunning":true},"runtime":{"agentId":"a","phase":"checking_provider"}});
        panel.apply(&mut cx,Target::Runtime,&checking);
        assert_eq!(panel.view.label(&mut cx,ids!(runtime_status)).text(),runtime_service_description(Some(&Ok(checking.clone())),"a"));
        panel.runtime_pending_action=Some(true);panel.busy=true;panel.sync_navigation(&mut cx);
        assert_eq!(panel.view.button(&mut cx,ids!(service_toggle)).text(),"Starting…");
        assert!(!panel.view.button(&mut cx,ids!(service_toggle)).borrow().unwrap().enabled());
        panel.runtime_pending_action=Some(false);panel.sync_navigation(&mut cx);
        assert_eq!(panel.view.button(&mut cx,ids!(service_toggle)).text(),"Stopping…");
        panel.runtime_pending_action=None;panel.runtime_action_error=runtime_diagnostic("agent_lease_busy");
        panel.apply(&mut cx,Target::Runtime,&checking);
        assert!(panel.view.label(&mut cx,ids!(runtime_status)).text().contains("agent_lease_busy"),"polling must retain the failed action diagnostic");
    }
    #[test]
    fn agent_service_can_run_without_a_selected_room_and_stop_drains_legacy_children() {
        let waiting=json!({"runtime":null,"service":{"enabled":true,"running":true,"rooms":[]}});
        assert!(service_enabled(&waiting));
        assert!(runtime_list_label(Some(&Ok(waiting)),"a").contains("Waiting for an available Room"));
        assert!(!service_enabled(&json!({"runtime":{"agentId":"a","phase":"stopped"},"service":{"enabled":false}})));
        assert!(service_enabled(&json!({"runtime":{"agentId":"a","phase":"online_chat_only"},"service":{"enabled":false}})),"legacy workers must remain stoppable before explicit Agent-wide start");
        assert!(!service_enabled(&json!({"runtime":null,"service":{"enabled":false}})));
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.agents=vec![json!({"id":"a"})];
        panel.labels(&mut cx,ids!(agents),"Select",&panel.agents,"id");
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        local_device_fixture(&mut panel,"a");
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(service_toggle)).borrow().unwrap().enabled(),"unknown service state cannot be toggled");
        panel.apply(&mut cx,Target::Runtime,&json!({"runtime":null,"service":{"enabled":false}}));
        panel.sync_navigation(&mut cx);
        assert_eq!(panel.view.button(&mut cx,ids!(service_toggle)).text(),crate::i18n::tr("Start responding"));
        panel.apply(&mut cx,Target::Runtime,&json!({"runtime":null,"service":{"enabled":true,"rooms":[]}}));
        panel.sync_navigation(&mut cx);
        assert_eq!(panel.view.button(&mut cx,ids!(service_toggle)).text(),crate::i18n::tr("Stop responding"));
        assert!(panel.view.button(&mut cx,ids!(service_toggle)).borrow().unwrap().enabled(),"an enabled service remains stoppable without Codex resources or active Rooms");
        assert!(!panel.view.view(&mut cx,ids!(runtime_start_options)).visible());
        panel.busy=true;
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(service_toggle)).borrow().unwrap().enabled(),"in-flight operation cannot be toggled twice");
    }
    #[test]
    fn token_amount_is_hidden_for_unset_and_unlimited_without_changing_policy() {
        let mut cx=Cx::new(Box::new(|_,_| {}));let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);vm.bx.captured_errors=Some(Vec::new());crate::app::register_widgets(vm);let v=script_eval!(vm,{mod.widgets.HagencyPanel{}});let p=HagencyPanel::script_from_value(vm,v);assert!(vm.take_errors().is_empty());p});
        for limit in [json!("Unset"),json!("Unlimited"),json!({"Tokens":70})] {
            panel.policy=json!({"policies":[{"policy":{"budget":{"limit":limit,"period":"Lifetime"},"requests":"Allow","high_risk":"Deny"}}]});
            panel.render_policy(&mut cx);
            assert_eq!(panel.view.view(&mut cx,ids!(token_amount_holder)).visible(),limit.is_object());
            assert_eq!(panel.view.view(&mut cx,ids!(period_holder)).visible(),limit.is_object());
            assert_eq!(panel.view.drop_down(&mut cx,ids!(limit_mode)).selected_item(),usize::from(limit.is_object()));
            assert_eq!(panel.policy["policies"][0]["policy"]["budget"]["limit"],limit);
        }
    }
    #[test]
    fn agent_run_link_selects_only_its_exact_binding_without_starting() {
        let mut cx=Cx::new(Box::new(|_,_| {}));let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);vm.bx.captured_errors=Some(Vec::new());crate::app::register_widgets(vm);let v=script_eval!(vm,{mod.widgets.HagencyPanel{}});let p=HagencyPanel::script_from_value(vm,v);assert!(vm.take_errors().is_empty());p});
        panel.agents=vec![json!({"id":"a"})];panel.labels(&mut cx,ids!(agents),"Select my Agent",&panel.agents,"id");panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        let bindings=json!({"bindings":[{"id":"other","roomId":"!other:s","state":"active"},{"id":"direct","roomId":"!direct:s","scopeKind":"owner_direct","state":"active"}]});
        // Default selection must not depend on the order returned by the server.
        panel.apply(&mut cx,Target::Bindings,&bindings);
        assert_eq!(panel.binding(&mut cx).as_deref(),Some("direct"));
        // A project Room entry must keep its exact target instead of falling back to the private chat.
        panel.wanted_run=Some(("a".into(),"other".into()));
        panel.apply(&mut cx,Target::Bindings,&bindings);
        assert_eq!(panel.binding(&mut cx).as_deref(),Some("other"));
        assert!(panel.view.view(&mut cx,ids!(runtime_section)).visible());
        assert!(!panel.view.view(&mut cx,ids!(room_advanced_content)).visible());
        // Expanding management options is presentation only; it must keep the selected target.
        panel.room_advanced=true; panel.render_room_advanced(&mut cx);
        assert_eq!(panel.binding(&mut cx).as_deref(),Some("other"));

    }
    #[test]
    fn device_metadata_loading_revocation_and_assignment_have_distinct_badges() {
        let local = json!({"id":"a","executionDeviceId":"here","generation":4});
        let remote = json!({"id":"a","executionDeviceId":"there","generation":4});
        let devices = vec![json!({"id":"here","name":"Laptop","revoked":false}),json!({"id":"there","name":"Desktop","revoked":false}),json!({"id":"old","name":"Old laptop","revoked":true})];
        assert_eq!(agent_device_state(&local, "here", &devices, false), AgentDeviceState::Checking);
        assert_eq!(agent_device_state(&local, "here", &devices, true), AgentDeviceState::Here);
        assert_eq!(agent_device_state(&remote, "here", &devices, true), AgentDeviceState::Other);
        assert_eq!(agent_device_state(&json!({"generation":4,"executionDeviceId":null}), "here", &devices, true), AgentDeviceState::Unassigned);
        for id in ["old", "missing"] {assert_eq!(agent_device_state(&json!({"generation":4,"executionDeviceId":id}), "here", &devices, true), AgentDeviceState::Unavailable);}
    }
    #[test]
    fn budgets_room_picker_keeps_runtime_target_and_old_revision_separate() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.HagencyPanel {} });
            let panel = HagencyPanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            panel
        });
        panel.agents = vec![json!({"id":"agent-a"})];
        panel.bindings = vec![json!({"id":"direct","roomId":"!direct:server","state":"active"}), json!({"id":"project-room","roomId":"!project:server","state":"active"})];
        panel.labels(&mut cx, ids!(agents), "Select", &panel.agents, "id");
        panel.labels(&mut cx, ids!(bindings), "Select", &panel.bindings, "roomId");
        panel.labels(&mut cx, ids!(policy_binding), "Select", &panel.bindings, "roomId");
        panel.view.drop_down(&mut cx, ids!(agents)).set_selected_item(&mut cx, 1);
        local_device_fixture(&mut panel, "agent-a");
        panel.section(&mut cx, ids!(policy_section));
        panel.view.drop_down(&mut cx, ids!(policy_layer)).set_selected_item(&mut cx, 1);
        panel.policy = json!({"agentId":"agent-a","bindingId":"direct","requester":"@alice:server","policies":[],"usage":[{"spent":12,"held":7}]});
        panel.approvals.push(json!({"proposalId":"old-room"}));
        panel.select_policy_room(&mut cx, 2);
        assert!(panel.approvals.is_empty(),"changing Room must discard another Room approval selection");
        panel.sync_navigation(&mut cx);
        assert_eq!(panel.binding(&mut cx).as_deref(), Some("project-room"));
        assert_eq!(panel.view.drop_down(&mut cx, ids!(policy_binding)).selected_item(), 2);
        assert!(panel.policy.is_null(), "another Room cannot reuse the old policy or expense display");
        assert!(!panel.view.button(&mut cx, ids!(save_policy)).borrow().unwrap().enabled());
        assert!(panel.view.view(&mut cx, ids!(policy_room_holder)).visible());
        assert!(!panel.view.view(&mut cx, ids!(room_controls)).visible(), "budgets must not expose Run's pause/leave card");
        assert!(!panel.view.view(&mut cx, ids!(requester_holder)).visible());
        assert_eq!(panel.view.label(&mut cx, ids!(policy_target)).text(), "!project:server");
        panel.view.text_input(&mut cx, ids!(requester)).set_text(&mut cx, "@alice:server");
        panel.view.drop_down(&mut cx, ids!(policy_layer)).set_selected_item(&mut cx, 2);
        panel.sync_navigation(&mut cx);
        assert!(panel.view.view(&mut cx, ids!(requester_holder)).visible());
        assert_eq!(panel.view.label(&mut cx, ids!(policy_target)).text(), "!project:server · @alice:server");
        panel.view.drop_down(&mut cx, ids!(policy_layer)).set_selected_item(&mut cx, 0);
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.view(&mut cx, ids!(policy_room_holder)).visible());
        assert!(!panel.view.view(&mut cx, ids!(requester_holder)).visible());
        assert!(panel.view.label(&mut cx, ids!(policy_target)).text().contains("all Rooms"));
        assert_eq!(panel.binding(&mut cx).as_deref(), Some("project-room"), "global layer must not replace Room selection");
    }
    #[test]
    fn registered_resource_editor_keeps_room_targets_and_policy_layers_distinct() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.HagencyPanel {} });
            let panel = HagencyPanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty());
            panel
        });
        panel.agents = vec![json!({"id":"agent-a"}), json!({"id":"agent-b"})];
        panel.bindings = vec![
            json!({"id":"binding-a", "roomId":"!a:server"}),
            json!({"id":"binding-b", "roomId":"!b:server"}),
        ];
        panel.labels(&mut cx, ids!(agents), "Select", &panel.agents, "id");
        panel.labels(&mut cx, ids!(bindings), "Select", &panel.bindings, "roomId");
        panel
            .view
            .drop_down(&mut cx, ids!(agents))
            .set_selected_item(&mut cx, 1);
        panel
            .view
            .drop_down(&mut cx, ids!(bindings))
            .set_selected_item(&mut cx, 1);
        panel
            .view
            .text_input(&mut cx, ids!(requester))
            .set_text(&mut cx, "@alice:server");
        local_device_fixture(&mut panel, "agent-a");
        panel.policy = json!({"agentId":"agent-a", "bindingId":"binding-a", "requester":"@alice:server",
            "policies":[{"revision":1,"policy":{"budget":{"limit":{"Tokens":900},"period":"Lifetime"},"requests":"Allow","high_risk":"Deny"}},
            {"revision":2,"policy":{"budget":{"limit":{"Tokens":200},"period":"UtcDay"},"requests":"AskOwner","high_risk":"AskOwner"}}],
            "usage":[{"spent":50,"held":20},{"spent":30,"held":10}]});
        panel
            .view
            .drop_down(&mut cx, ids!(policy_layer))
            .set_selected_item(&mut cx, 0);
        panel.render_policy(&mut cx);
        assert_eq!(panel.input(&mut cx, ids!(token_limit)), "900");
        panel
            .view
            .drop_down(&mut cx, ids!(policy_layer))
            .set_selected_item(&mut cx, 1);
        panel.render_policy(&mut cx);
        assert_eq!(panel.input(&mut cx, ids!(token_limit)), "200");
        assert_eq!(
            panel
                .view
                .drop_down(&mut cx, ids!(requests))
                .selected_item(),
            2
        );
        panel.section(&mut cx, ids!(model_section));
        assert!(panel.view.view(&mut cx, ids!(model_section)).visible());
        assert!(!panel.view.view(&mut cx, ids!(policy_section)).visible());
        panel
            .view
            .drop_down(&mut cx, ids!(bindings))
            .set_selected_item(&mut cx, 2);
        panel.sync_navigation(&mut cx);
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(save_model))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(reset_policy))
                .borrow()
                .unwrap()
                .enabled()
        );
        panel.save_policy(&mut cx);
        assert!(
            !panel.busy,
            "a stale policy must not dispatch a write for the new Room"
        );
        panel.section(&mut cx, ids!(runtime_section));
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.widget(&mut cx, ids!(leave_confirm)).visible());
        assert!(!panel.view.view(&mut cx, ids!(room_service_actions)).visible());
        assert!(!panel.view.view(&mut cx, ids!(retirement_section)).visible());
        assert_eq!(panel.binding(&mut cx).as_deref(), Some("binding-b"));
        panel.section(&mut cx, ids!(setup_section));
        panel.sync_navigation(&mut cx);
        assert!(panel.view.view(&mut cx, ids!(retirement_section)).visible());
        assert!(panel.view.widget(&mut cx, ids!(leave_confirm)).visible());
        panel
            .view
            .drop_down(&mut cx, ids!(agents))
            .set_selected_item(&mut cx, 2);
        panel.save_policy(&mut cx);
        assert!(
            !panel.busy,
            "a stale policy must not dispatch a write for another Agent"
        );
    }

    #[test]
    fn agent_budget_is_editable_without_room_and_admin_controls_require_exact_permission() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.HagencyPanel {} });
            HagencyPanel::script_from_value(vm, value)
        });
        panel.agents = vec![json!({"id":"agent-a"})];
        panel.labels(&mut cx, ids!(agents), "Select", &panel.agents, "id");
        panel
            .view
            .drop_down(&mut cx, ids!(agents))
            .set_selected_item(&mut cx, 1);
        local_device_fixture(&mut panel, "agent-a");
        panel.section(&mut cx, ids!(policy_section));
        panel
            .view
            .drop_down(&mut cx, ids!(policy_layer))
            .set_selected_item(&mut cx, 0);
        panel.apply(&mut cx, Target::Policy, &json!({"agentId":"agent-a","policy":{"revision":9,"policy":{"budget":{"limit":{"Tokens":700},"period":"Lifetime"},"requests":"Allow","high_risk":"Deny"}},"usage":{"spent":123,"held":20}}));
        panel.sync_navigation(&mut cx);
        assert!(
            panel
                .view
                .button(&mut cx, ids!(save_policy))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert!(!panel.view.view(&mut cx, ids!(room_controls)).visible());
        assert!(!panel.view.view(&mut cx, ids!(requester_holder)).visible());
        assert_eq!(panel.input(&mut cx, ids!(token_limit)), "700");
        assert!(editor_matches(&panel.policy, "agent-a", None, "", 0));
        assert!(!editor_matches(&panel.policy, "agent-b", None, "", 0));
        assert!(!editor_matches(
            &panel.policy,
            "agent-a",
            Some("binding"),
            "@user:server",
            1
        ));
        panel.section(&mut cx, ids!(model_section));
        panel
            .view
            .drop_down(&mut cx, ids!(policy_layer))
            .set_selected_item(&mut cx, 1);
        local_device_fixture(&mut panel, "agent-a");
        panel.apply(&mut cx,Target::Policy,&json!({"agentId":"agent-a","policy":{"revision":9,"policy":{"budget":{"limit":"Unlimited","period":"Lifetime"},"requests":"Allow","high_risk":"Deny"}},"usage":{"spent":123,"held":20},"modelProfile":{"model":"test-model","workspace_root":"/owned/workspace","credential_ref":"keychain:owned"}}));
        panel.apply(&mut cx,Target::ProviderModels,&json!({"authenticated":true,"credentialRef":"keychain:owned","state":"ready","models":[{"model":"test-model","displayName":"Test model","isDefault":true,"description":"Actual catalog entry","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}]}]}));
        panel.sync_navigation(&mut cx);
        assert!(
            panel
                .view
                .button(&mut cx, ids!(save_model))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert!(!panel.view.view(&mut cx, ids!(room_controls)).visible());
        assert_eq!(panel.policy["modelProfile"]["model"], "test-model");
        panel.policy["agentId"] = "another-agent".into();
        panel.sync_navigation(&mut cx);
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(save_model))
                .borrow()
                .unwrap()
                .enabled()
        );
        panel.projects = vec![
            json!({"id":"project-a","creationPolicy":"{\"defaultAllow\":true,\"allow\":[],\"deny\":[]}"}),
        ];
        panel.labels(&mut cx, ids!(projects), "Select", &panel.projects, "id");
        panel
            .view
            .drop_down(&mut cx, ids!(projects))
            .set_selected_item(&mut cx, 1);
        panel.apply(&mut cx,Target::ScopeState,&json!({"projectId":"project-a","roomId":null,"revision":1,"canManagePolicy":false,"servicePaused":false,"creationPolicy":{"defaultAllow":true,"allow":[],"deny":[]}}));
        assert!(!panel.view.view(&mut cx, ids!(permission_editor)).visible());
        panel.apply(&mut cx,Target::ScopeState,&json!({"projectId":"project-a","roomId":null,"revision":1,"canManagePolicy":true,"servicePaused":true,"creationPolicy":{"defaultAllow":false,"allow":["@alice:server"],"deny":[]}}));
        assert!(panel.view.view(&mut cx, ids!(permission_editor)).visible());
        assert_eq!(
            panel
                .view
                .drop_down(&mut cx, ids!(creation_mode))
                .selected_item(),
            1
        );
        assert_eq!(panel.input(&mut cx, ids!(creation_allow)), "@alice:server");
        panel.apply(
            &mut cx,
            Target::ScopeState,
            &json!({"projectId":"project-b","roomId":null,"revision":8,"canManagePolicy":true}),
        );
        assert_eq!(
            panel.scope_state["projectId"], "project-a",
            "late response for another Project must not replace its permission target"
        );
    }
    #[test]
    fn original_agent_journal_requires_history_but_keeps_other_requests_available() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.HagencyPanel {} });
            HagencyPanel::script_from_value(vm, value)
        });
        panel.sync_navigation(&mut cx);
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(create_agent))
                .borrow()
                .unwrap()
                .enabled()
        );
        panel.page=Page::Agents;
        let original = json!({"commands":[{"id":"original-id","operation":"agent.create","state":"unknown","input":{"idempotencyKey":"original-id","projectId":"project-a","roomId":"!a:server","displayName":"Original"}}]});
        let global=json!({"operation":"agent.create","state":"pending","input":{"displayName":"My Agent"}});
        assert!(agent_command_label(&global).contains("My Agent"));
        assert!(!agent_command_details(&global).contains("Room:"));
        assert!(!agent_command_details(&global).contains("Project:"));
        panel.apply(&mut cx, Target::AgentJournal, &original);
        panel.sync_navigation(&mut cx);
        assert!(panel.view.view(&mut cx, ids!(agent_recovery)).visible());
        assert!(
            panel
                .view
                .button(&mut cx, ids!(create_agent))
                .borrow()
                .unwrap()
                .enabled(),
            "a pending request in another Room must not block distinct requests"
        );
        assert!(
            panel
                .view
                .button(&mut cx, ids!(bind_agent))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert_eq!(
            panel.agent_pending_commands[0]["input"],
            original["commands"][0]["input"]
        );
        panel.apply(&mut cx, Target::AgentJournal, &json!({"commands":[]}));
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.view(&mut cx, ids!(agent_recovery)).visible());
        assert!(
            panel
                .view
                .button(&mut cx, ids!(create_agent))
                .borrow()
                .unwrap()
                .enabled()
        );
    }
    #[test]
    fn agent_execution_device_blocks_remote_local_settings_and_requires_exact_agent() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::app::register_widgets(vm);
            let value = script_eval!(vm,{mod.widgets.HagencyPanel {}});
            let panel = HagencyPanel::script_from_value(vm,value);
            assert!(vm.take_errors().is_empty());
            panel
        });
        panel.agents = vec![json!({"id":"agent-a"}),json!({"id":"agent-b"})];
        panel.labels(&mut cx,ids!(agents),"Select",&panel.agents,"id");
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        panel.apply(&mut cx,Target::AgentDetails,&json!({"agent":{"id":"agent-a","executionDeviceId":"device-other","state":"active","generation":1}}));
        panel.apply(&mut cx,Target::Devices,&json!({"currentDeviceId":"device-here","devices":[{"id":"device-here","name":"Laptop","revoked":false},{"id":"device-other","name":"Desktop","revoked":false},{"id":"revoked","name":"Old","revoked":true}]}));
        panel.sync_navigation(&mut cx);
        assert_eq!(panel.execution_devices.len(),3, "revoked metadata must remain available for device badges");
        assert!(panel.view.view(&mut cx, ids!(execution_badge)).label(&mut cx, ids!(device_label)).text().contains("Desktop"));
        assert!(panel.view.button(&mut cx, ids!(policy_tab)).borrow().unwrap().enabled(), "remote Agent policy is accessible without moving assignment");
        assert!(panel.view.button(&mut cx, ids!(model_tab)).borrow().unwrap().enabled());
        assert!(!panel.view.button(&mut cx, ids!(runtime_tab)).borrow().unwrap().enabled());
        assert!(!panel.view.button(&mut cx, ids!(assign_current_device)).borrow().unwrap().enabled(), "moving execution requires explicit confirmation");
        panel.view.check_box(&mut cx, ids!(assignment_confirm)).set_active(&mut cx, true, Animate::No);
        panel.sync_navigation(&mut cx);
        assert!(panel.view.button(&mut cx, ids!(assign_current_device)).borrow().unwrap().enabled());
        for state in ["retiring", "retired"] {
            panel.execution_agent_record["state"] = state.into();
            panel.sync_navigation(&mut cx);
            assert!(!panel.view.button(&mut cx, ids!(assign_current_device)).visible(), "{state} Agent cannot move execution");
            assert!(!panel.view.button(&mut cx, ids!(assign_current_device)).borrow().unwrap().enabled());
            assert!(!panel.view.check_box(&mut cx, ids!(assignment_confirm)).visible());
        }
        panel.execution_agent_record["state"] = "active".into();
        panel.execution_agent_record["generation"] = 0.into();
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx, ids!(assign_current_device)).visible(), "unconfirmed generation cannot assign");
        panel.execution_agent_record["generation"] = 1.into();
        for state in ["creating", "active", "suspended"] {
            panel.execution_agent_record["state"] = state.into();
            panel.sync_navigation(&mut cx);
            assert!(panel.view.button(&mut cx, ids!(assign_current_device)).visible());
            assert!(panel.view.button(&mut cx, ids!(assign_current_device)).borrow().unwrap().enabled());
        }

        panel.policy = json!({"agentId":"agent-a","scope":"agent","modelLoaded":true});
        panel.local(&mut cx, Target::Policy, LocalAction::AgentPolicy, None);
        assert!(!panel.busy, "remote Agent never initiates a local resource request");
        assert!(panel.view.widget(&cx, ids!(execution_devices)).is_empty());
        assert!(panel.view.widget(&cx, ids!(execution_name)).is_empty());
        panel.apply(&mut cx, Target::AgentAssigned, &json!({"agent":{"id":"agent-a","executionDeviceId":"device-here","state":"active","generation":2}}));
        assert_eq!(panel.agents[0]["executionDeviceId"], "device-here");
        assert!(panel.policy.is_null(), "moving execution must not reuse or copy former resource policy");
        assert!(!panel.runtime_resources_checked);
        assert!(!panel.view.check_box(&mut cx, ids!(assignment_confirm)).active(&cx));

        assert!(!panel.view.button(&mut cx,ids!(service_toggle)).borrow().unwrap().enabled());
        panel.apply(&mut cx,Target::AgentDetails,&json!({"agent":{"id":"agent-a","executionDeviceId":"device-here","state":"active","generation":2}}));
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(service_toggle)).borrow().unwrap().enabled(),"assignment alone does not prove Codex and Room readiness");
        assert!(panel.view.label(&mut cx,ids!(start_requirements)).text().contains("Model & resources"));
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,2);
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(service_toggle)).borrow().unwrap().enabled());
        panel.apply(&mut cx,Target::AgentDetails,&json!({"agent":{"id":"agent-a","executionDeviceId":"device-here","state":"active","generation":3}}));
        assert_eq!(panel.execution_agent_record["generation"],2,"late Agent response for another Agent cannot overwrite current target");
        panel.bindings = vec![json!({"id":"direct","roomId":"!private:server","scopeKind":"owner_direct","projectId":null,"state":"suspended"}),json!({"id":"project-room","roomId":"!project:server","scopeKind":"project","projectId":"project-a","state":"active"})];
        panel.labels(&mut cx,ids!(bindings),"Select",&panel.bindings,"roomId");
        panel.view.drop_down(&mut cx,ids!(bindings)).set_selected_item(&mut cx,1);
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(room_roster_check)).visible());
        assert!(!panel.view.button(&mut cx,ids!(room_roster_check)).borrow().unwrap().enabled());
        assert!(panel.view.label(&mut cx,ids!(room_roster)).text().contains("private chat"));
        assert!(panel.view.button(&mut cx,ids!(resume_binding)).borrow().unwrap().enabled(),"private binding resumes by binding ID, without a synthetic Project");
        panel.view.drop_down(&mut cx,ids!(bindings)).set_selected_item(&mut cx,2);
        panel.sync_navigation(&mut cx);
        assert!(panel.view.button(&mut cx,ids!(room_roster_check)).visible());

    }
    #[test]
    fn room_reply_policy_defaults_to_mentions_and_tracks_only_selected_binding() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.bindings = vec![
            json!({"id":"direct","scopeKind":"owner_direct","state":"active"}),
            json!({"id":"room-a","scopeKind":"project","projectId":"project","state":"active","threadAutoReply":false}),
            json!({"id":"room-b","scopeKind":"project","projectId":"project","state":"active","threadAutoReply":true})];
        panel.labels(&mut cx,ids!(bindings),"Select",&panel.bindings,"id");
        panel.view.drop_down(&mut cx,ids!(bindings)).set_selected_item(&mut cx,1);
        panel.sync_navigation(&mut cx);
        assert!(!panel.view.view(&mut cx,ids!(reply_policy)).visible());
        panel.view.drop_down(&mut cx,ids!(bindings)).set_selected_item(&mut cx,2);
        panel.sync_navigation(&mut cx);
        assert!(panel.view.view(&mut cx,ids!(reply_policy)).visible());
        assert!(panel.view.label(&mut cx,ids!(reply_policy_status)).text().contains("every message must @"));
        panel.apply(&mut cx,Target::ReplyPolicyChanged,&json!({"binding":{"id":"room-a","scopeKind":"project","projectId":"project","state":"active","threadAutoReply":true}}));
        panel.sync_navigation(&mut cx);
        assert_eq!(panel.view.drop_down(&mut cx,ids!(bindings)).selected_item(),2);
        assert!(panel.view.label(&mut cx,ids!(reply_policy_status)).text().contains("every message must @"));
        panel.apply(&mut cx,Target::ReplyPolicyChanged,&json!({"binding":{"id":"room-b","scopeKind":"project","projectId":"project","state":"active","threadAutoReply":false}}));
        panel.sync_navigation(&mut cx);
        assert!(panel.view.label(&mut cx,ids!(reply_policy_status)).text().contains("every message must @"),"another Room response cannot change this Room's behavior");
        panel.busy=true;panel.sync_navigation(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(reply_policy_toggle)).visible());
    }

    #[test]
    fn direct_chat_setup_keeps_created_identity_and_only_opens_confirmed_same_agent_chat() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.agents=vec![json!({"id":"agent-a"}),json!({"id":"agent-b"})];
        panel.labels(&mut cx,ids!(agents),"Select",&panel.agents,"id");
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        panel.agent_command=Some((json!({"displayName":"Created"}),"original-create-key".into()));
        panel.apply(&mut cx,Target::AgentCreated,&json!({"creation":{"agent":{"id":"agent-a"}}}));
        assert!(panel.agent_command.is_none(),"confirmed Agent creation is retained independently of contact setup");
        assert_eq!(panel.wanted_agent.as_deref(),Some("agent-a"));
        assert_eq!(panel.direct_agent,"agent-a");
        panel.apply(&mut cx,Target::OwnerDirectCreated,&json!({"ownerDirect":{"state":"pending","phase":"unknown","roomId":"!private:server"}}));
        assert!(!panel.view.button(&mut cx,ids!(open_owner_direct)).borrow().unwrap().enabled());
        assert_eq!(panel.wanted_agent.as_deref(),Some("agent-a"));
        panel.apply(&mut cx,Target::OwnerDirect,&json!({"ownerDirect":{"state":"pending","phase":"agent_provisioning","lastError":null}}));
        assert_eq!(panel.direct_retry.as_ref().map(|(_,agent,n)|(agent.as_str(),*n)),Some(("agent-a",1)));
        panel.apply(&mut cx,Target::OwnerDirect,&json!({"ownerDirect":{"state":"pending","phase":"server_joining","lastError":null}}));
        assert_eq!(panel.direct_retry.as_ref().map(|(_,_,n)|*n),Some(2));
        panel.apply(&mut cx,Target::OwnerDirect,&json!({"ownerDirect":{"state":"pending","phase":"creating","lastError":"owner_direct_creation_outcome_unknown"}}));
        assert!(panel.direct_retry.is_none(),"unknown create must not be silently retried");
        panel.direct_retry=Some((panel.identity.clone(),"agent-a".into(),8));
        panel.apply(&mut cx,Target::OwnerDirect,&json!({"ownerDirect":{"state":"pending","phase":"server_joining","lastError":null}}));
        assert!(panel.direct_retry.is_none(),"automatic retries are finite");
        panel.apply(&mut cx,Target::OwnerDirect,&json!({"ownerDirect":{"state":"active","phase":"complete","roomId":"!private:server","bindingId":"direct-binding"}}));
        assert!(panel.view.button(&mut cx,ids!(open_owner_direct)).borrow().unwrap().enabled());
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,2);
        panel.render_owner_direct(&mut cx);
        assert!(!panel.view.button(&mut cx,ids!(open_owner_direct)).borrow().unwrap().enabled());
    }
    #[test]
    fn known_private_chat_is_ready_to_open_and_encrypt_from_bindings() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.agents=vec![json!({"id":"agent-a"}),json!({"id":"agent-b"})];
        panel.labels(&mut cx,ids!(agents),"Select",&panel.agents,"id");
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,1);
        let bindings=json!({"bindings":[
            {"id":"room-a","agentId":"agent-a","scopeKind":"project","projectId":"project","roomId":"!room:server","state":"active"},
            {"id":"direct-a","agentId":"agent-a","scopeKind":"owner_direct","projectId":null,"roomId":"!private:server","state":"active"}]});
        panel.apply(&mut cx,Target::Bindings,&bindings);
        assert_eq!(panel.direct_agent,"agent-a");
        assert_eq!(panel.direct_state["roomId"],"!private:server");
        assert!(panel.view.button(&mut cx,ids!(open_owner_direct)).borrow().unwrap().enabled());
        assert!(!panel.view.widget(&mut cx,ids!(encrypt_owner_direct)).disabled(&cx));
        assert!(!panel.view.check_box(&mut cx,ids!(encrypt_owner_direct)).active(&mut cx));
        // Once encrypted, the switch stays on and cannot be turned off.
        panel.direct_encrypted.insert("!private:server".into());
        panel.render_owner_direct(&mut cx);
        assert!(panel.view.check_box(&mut cx,ids!(encrypt_owner_direct)).active(&mut cx));
        assert!(panel.view.widget(&mut cx,ids!(encrypt_owner_direct)).disabled(&cx));
        panel.direct_encrypted.clear();
        // A setup retry in flight keeps its own state.
        panel.direct_state=json!({"state":"pending"});
        panel.direct_retry=Some((panel.identity.clone(),"agent-a".into(),1));
        panel.apply(&mut cx,Target::Bindings,&bindings);
        assert_eq!(panel.direct_state["state"],"pending");
        // Another Agent's bindings never mark that Agent's chat ready.
        panel.direct_retry=None;
        panel.view.drop_down(&mut cx,ids!(agents)).set_selected_item(&mut cx,2);
        panel.apply(&mut cx,Target::Bindings,&bindings);
        panel.render_owner_direct(&mut cx);
        assert!(panel.view.widget(&mut cx,ids!(encrypt_owner_direct)).disabled(&cx));
    }
    #[test]
    fn entering_run_reads_saved_resources_without_reauth_and_uses_their_actual_model_choices() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            let panel = HagencyPanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty()); panel
        });
        panel.page = Page::Agents; panel.agents = vec![json!({"id":"exact-agent"})];
        panel.labels(&mut cx, ids!(agents), "Select my Agent", &panel.agents.clone(), "id");
        panel.view.drop_down(&mut cx, ids!(agents)).set_selected_item(&mut cx, 1);
        panel.section(&mut cx, ids!(runtime_section));
        local_device_fixture(&mut panel, "exact-agent");
        panel.load_runtime_resources(&mut cx);
        panel.sync_navigation(&mut cx);
        assert!(panel.runtime_resources_loading); assert!(!panel.runtime_resources_checked);
        assert!(panel.view.label(&mut cx, ids!(start_requirements)).text().contains("Checking"));
        assert!(!panel.view.button(&mut cx, ids!(service_toggle)).borrow().unwrap().enabled());
        let saved = json!({"model":"existing-model","workspace_root":"/existing-owner-workspace","credential_ref":"existing-owner-reference"});
        panel.apply(&mut cx, Target::Policy, &json!({"agentId":"exact-agent","policy":{"revision":1},"usage":{"spent":0,"held":0},"modelProfile":saved}));
        assert!(panel.runtime_resources_loading, "saved profile alone is not proof that Codex is connected");
        panel.apply(&mut cx, Target::ProviderModels, &json!({"authenticated":true,"shared":true,"credentialSource":"local_codex","credentialRef":"existing-owner-reference","models":[{"model":"existing-model","isDefault":true,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"medium"}]}]}));
        assert!(panel.runtime_resources_checked); assert!(!panel.runtime_resources_loading);
        assert!(panel.provider_authenticated); assert_eq!(panel.policy["modelProfile"], saved, "read initialization never overwrites the saved profile");
        assert_eq!(panel.runtime_efforts, vec!["low","medium"]);
        assert_eq!(panel.view.drop_down(&mut cx, ids!(effort)).selected_item(), 0);
        assert!(!panel.view.button(&mut cx, ids!(provider_login)).visible());
        assert!(!panel.view.button(&mut cx, ids!(provider_logout)).visible());
        assert!(!panel.busy, "registration tests contain no SDK account or executable model request");
    }
    #[tokio::test]
    async fn management_wait_expires_at_45_seconds_without_cancelling_original_work() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            HagencyPanel::script_from_value(vm, value)
        });
        panel.active = true; panel.busy = true; panel.identity = "same-account".into(); panel.request = nonce();
        let request = panel.request;
        let (release, waiting) = tokio::sync::oneshot::channel();
        // Simulates a bootstrap already having side effects. UI expiration
        // must neither abort its cleanup nor cause an automatic second call.
        let original = tokio::spawn(async move {waiting.await.unwrap(); Completed {identity:"same-account".into(),request,target:Target::Agents,configuration_fence:None,configuration_digest:None,result:Ok(json!({"agents":[]}))}});
        let before = std::time::Instant::now(); panel.arm_read_wait(&mut cx, &Command::Agents);
        let deadline = panel.read_wait.as_ref().unwrap().deadline;
        assert!(deadline >= before + std::time::Duration::from_secs(45));
        assert!(!panel.expire_read_wait(&mut cx, deadline - std::time::Duration::from_nanos(1)));
        assert!(panel.busy); assert!(panel.expire_read_wait(&mut cx, deadline));
        assert!(!panel.busy); assert!(panel.read_wait.is_none());
        assert!(!original.is_finished(), "UI timeout leaves the original future running");
        release.send(()).unwrap(); let done = original.await.unwrap();
        assert!(!current_completion(panel.active, &panel.identity, panel.request, &done), "late old response cannot repopulate this view");
        assert!(panel.agents.is_empty());
        panel.busy = true; panel.arm_read_wait(&mut cx, &Command::CreateAgent {input:json!({"idempotencyKey":"unchanged"})});
        assert!(panel.read_wait.is_none(), "mutations never receive a read watchdog");
    }
    #[test]
    fn expired_old_account_wait_does_not_unlock_or_replace_a_new_account_view() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            HagencyPanel::script_from_value(vm, value)
        });
        panel.active = true; panel.busy = true; panel.identity = "old-account".into(); panel.request = nonce();
        panel.arm_read_wait(&mut cx, &Command::Agents); let deadline = panel.read_wait.as_ref().unwrap().deadline;
        panel.identity = "new-account".into(); panel.request = nonce(); let new_request = panel.request;
        assert!(!panel.expire_read_wait(&mut cx, deadline));
        assert!(panel.busy); assert_eq!(panel.request, new_request);
    }
    #[test]
    fn agent_direct_navigation_selects_chats_but_project_binding_keeps_project_context() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        for direct in [true, false] {
            let room = matrix_sdk::ruma::OwnedRoomId::try_from("!exact-room:server").unwrap();
            let actions = cx.capture_actions(|cx| navigate_agent_chat(cx, room.clone(), direct, Some(("owner-pin", "project-a"))));
            assert_eq!(actions.iter().filter(|a| matches!(a.downcast_ref::<crate::home::main_desktop_ui::MainDesktopUiAction>(), Some(crate::home::main_desktop_ui::MainDesktopUiAction::ShowChats))).count(), usize::from(direct));
            let projects: Vec<_> = actions.iter().filter_map(|a| a.downcast_ref::<ProjectRoomNavigation>()).collect();
            assert_eq!(projects.len(), usize::from(!direct));
            if !direct {assert_eq!(projects[0].project_id, "project-a");assert_eq!(projects[0].identity, "owner-pin");}
            assert!(!actions.iter().any(|a| a.downcast_ref::<HagencyAction>().is_some()), "opening a binding must not replace its chat with management");
            let destinations: Vec<_> = actions.iter().filter_map(|a| match a.downcast_ref::<crate::app::AppStateAction>() {
                Some(crate::app::AppStateAction::NavigateToRoom {destination_room, ..}) => Some(destination_room.room_id().to_owned()),
                _ => None,
            }).collect();
            assert_eq!(destinations, vec![room]);
        }
    }
    #[test]
    fn empty_status_does_not_reserve_space_above_agent_controls() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            let panel = HagencyPanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty()); panel
        });
        panel.page = Page::Agents;
        panel.view.view(&mut cx, ids!(agents_page)).set_visible(&mut cx, true);
        panel.view.view(&mut cx, ids!(agent_detail)).set_visible(&mut cx, true);
        panel.view.view(&mut cx, ids!(agent_list_holder)).set_visible(&mut cx, false);
        panel.section(&mut cx, ids!(model_section));
        let mut positions = Vec::new();
        for message in ["", "Loading…", ""] {
            panel.status(&mut cx, message);
            cx.redraw_all(); let pass = DrawPass::new(&mut cx); pass.set_size(&mut cx, dvec2(980.0, 900.0));
            let mut list = DrawList2d::new(&mut cx); let overlay = Overlay {draw_list: DrawList::new(&mut cx)};
            let event = DrawEvent::default(); let mut draw = CxDraw::new(&mut cx, &event); let mut cx2d = Cx2d::new(&mut draw);
            cx2d.begin_pass(&pass, None); list.begin_always(&mut cx2d); overlay.begin(&mut cx2d); cx2d.begin_root_turtle(dvec2(980.0, 900.0), Layout::flow_overlay());
            panel.draw_walk_all(&mut cx2d, &mut Scope::empty(), Walk {width: Size::Fixed(980.0), height: Size::Fixed(860.0), ..Walk::default()});
            overlay.end(&mut cx2d); cx2d.end_pass_sized_turtle(); list.end(&mut cx2d); cx2d.end_pass(&pass);
            drop(cx2d); drop(draw);
            assert!(!panel.view.view(&mut cx, ids!(status_holder)).visible());
            assert_eq!(panel.view.widget(&mut cx, ids!(loading_status)).visible(), message == "Loading…");
            positions.push(panel.view.view(&mut cx, ids!(projects_page)).area().rect(&cx).pos.y);
        }
        assert_eq!(positions[0], positions[2]);
        assert_eq!(positions[1], positions[0], "loading in the title row must not move the tabs");
    }
    #[test]
    fn provider_diagnostics_are_explicit_but_never_render_untrusted_details() {
        for code in ["local_codex_configuration_unsupported", "codex_binary_unavailable", "provider_directory_not_canonical", "provider_keyring_unavailable", "codex_model_catalog_unavailable", "codex_protocol_mismatch", "codex_version_unsupported", "claude_version_unsupported", "owner_daemon_update_required", "owner_daemon_unavailable"] {
            let message = visible_error(code);
            assert!(message.contains(code), "approved static diagnostic is visible");
            assert!(!message.starts_with(code), "explain the problem before the code");
            assert_ne!(user_error(code), user_error("unknown_error"));
        }
        for unsafe_detail in ["/Users/owner/private/auth.json", "{\"token\":\"secret\"}", "codex_provider_unavailable /Users/owner/secret"] {
            let message = visible_error(unsafe_detail);
            assert!(!message.contains(unsafe_detail));
            assert!(!message.contains("secret"));
            assert!(!message.contains("/Users"));
        }
    }
    #[test]
    fn shared_protocol_errors_explain_update_or_reconnection_without_resending() {
        for (code, expected) in [
            ("owner_daemon_update_required", "Update Hagency and start its service"),
            ("owner_daemon_unavailable", "waiting for that service to reconnect"),
        ] {
            assert!(user_error(code).contains(expected));
            assert!(user_error(code).contains("original message"));
            assert!(visible_error(code).contains(code));
            assert!(!user_error(code).contains("retry"));
            let untrusted = format!("{code} /Users/owner/private/token");
            assert_eq!(provider_error_message(&untrusted), None);
            assert!(!visible_error(&untrusted).contains("/Users"));
        }
    }
    #[test]
    fn agent_empty_state_requires_a_successful_list_and_creation_placeholder_tracks_kind() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm); crate::app::register_widgets(vm);
            vm.bx.captured_errors = Some(Vec::new());
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            let panel = HagencyPanel::script_from_value(vm, value);
            assert!(vm.take_errors().is_empty()); panel
        });
        assert!(!agent_empty_message(panel.agents_loaded, panel.agents_loading).contains("No Agents"));
        panel.agents_loading = true;
        assert_eq!(agent_empty_message(panel.agents_loaded, panel.agents_loading), "Loading your Agents…");
        // A failed load stops the spinner without declaring the account empty.
        panel.agents_loading = false;
        assert!(agent_empty_message(panel.agents_loaded, panel.agents_loading).contains("could not be loaded"));
        panel.apply(&mut cx, Target::Agents, &json!({"agents":[], "projects":[]}));
        assert!(agent_empty_message(panel.agents_loaded, panel.agents_loading).starts_with("No Agents yet"));
        panel.create_room = true; panel.creation_source(&mut cx);
        assert_eq!(panel.view.text_input(&mut cx, ids!(creation_name)).empty_text(), "Room name");
        panel.create_room = false; panel.creation_source(&mut cx);
        assert_eq!(panel.view.text_input(&mut cx, ids!(creation_name)).empty_text(), "Project name");
    }
    #[test]
    fn agent_resources_draw_without_recovery_or_setup_and_model_choices_are_real() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let mut panel=cx.with_vm(|vm| {makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);vm.bx.captured_errors=Some(Vec::new());let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});let panel=HagencyPanel::script_from_value(vm,value);assert!(vm.take_errors().is_empty());panel});
        panel.page=Page::Agents;
        panel.view.view(&mut cx,ids!(agents_page)).set_visible(&mut cx,true);
        panel.view.view(&mut cx,ids!(agent_detail)).set_visible(&mut cx,true);
        panel.view.view(&mut cx,ids!(agent_list_holder)).set_visible(&mut cx,false);
        panel.agent_pending_commands=vec![json!({"id":"original","state":"unknown"})];
        panel.agents = vec![json!({"id":"agent-a"})];
        panel.labels(&mut cx, ids!(agents), "Select", &panel.agents, "id");
        panel.view.drop_down(&mut cx, ids!(agents)).set_selected_item(&mut cx, 1);
        local_device_fixture(&mut panel, "agent-a");
        panel.section(&mut cx,ids!(model_section));
        panel.policy=json!({"agentId":"a","scope":"agent","modelLoaded":true,"modelProfile":{"model":"catalog-b","reasoning_effort":"medium"}});
        panel.apply(&mut cx,Target::ProviderModels,&json!({"authenticated":true,"credentialRef":"safe-reference","models":[{"model":"catalog-a","displayName":"First","isDefault":true,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]},{"model":"catalog-b","displayName":"Second","description":"Real catalog description","defaultReasoningEffort":"high","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}]}]}));
        assert_eq!(panel.view.drop_down(&mut cx,ids!(model)).selected_item(),2,"saved exact model takes priority over provider default");
        assert_eq!(panel.runtime_efforts,vec!["medium","high"]);
        assert_eq!(panel.view.drop_down(&mut cx,ids!(effort)).selected_item(),0,"saved reasoning effort takes priority over the model default");
        assert!(!panel.view.button(&mut cx,ids!(provider_login)).visible());
        assert!(panel.view.button(&mut cx,ids!(provider_logout)).visible());
        {
        cx.redraw_all();let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(980.0,900.0));let mut list=DrawList2d::new(&mut cx);let overlay=Overlay {draw_list:DrawList::new(&mut cx)};let event=DrawEvent::default();let mut draw=CxDraw::new(&mut cx,&event);let mut cx2d=Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass,None);list.begin_always(&mut cx2d);overlay.begin(&mut cx2d);cx2d.begin_root_turtle(dvec2(980.0,900.0),Layout::flow_overlay());
        panel.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk {width:Size::Fixed(980.0),height:Size::Fixed(860.0),..Walk::default()});
        overlay.end(&mut cx2d);cx2d.end_pass_sized_turtle();list.end(&mut cx2d);cx2d.end_pass(&pass);
        }
        assert!(!panel.view.view(&mut cx,ids!(agent_recovery)).visible());
        assert!(!panel.view.view(&mut cx,ids!(setup_section)).visible());
        assert!(panel.view.view(&mut cx,ids!(model_section)).area().rect(&cx).size.y>0.0);
        assert!(!panel.view.view(&mut cx,ids!(advanced_settings)).visible(),"workspace is hidden by default");
        assert!(panel.view.drop_down(&mut cx,ids!(effort)).area().rect(&cx).size.y>0.0,"model reasoning settings are visible on the Codex page");
        panel.model_advanced=true;panel.render_model_advanced(&mut cx);
        assert!(panel.view.view(&mut cx,ids!(advanced_settings)).visible());
        panel.model_advanced=false;panel.render_model_advanced(&mut cx);
        panel.apply(&mut cx,Target::Provider,&json!({"authenticated":true,"state":"ready","shared":true,"credentialSource":"local_codex"}));
        assert!(!panel.view.button(&mut cx,ids!(provider_logout)).visible());
        assert!(!panel.view.button(&mut cx,ids!(provider_login)).visible());
        assert!(panel.view.button(&mut cx,ids!(disconnect_provider)).visible());
        assert!(!panel.view.button(&mut cx,ids!(use_local_provider)).visible());
        panel.apply(&mut cx,Target::Provider,&json!({"authenticated":false,"state":"signed_out"}));
        assert!(panel.view.button(&mut cx,ids!(provider_login)).visible());
        assert!(!panel.view.button(&mut cx,ids!(provider_logout)).visible());
        assert!(runtime_summary(&json!({"agentId":"a","phase":"unknown"}),"a",None).contains("not confirmed"));
        assert!(runtime_summary(&json!({"agentId":"a","phase":"ledger_recovery_required","lastError":"ledger_recovery_required"}),"a",None).contains("complete ledger"));
        assert_eq!(runtime_summary(&Value::Null,"a",None),"Not running on this device.");
        panel.apply(&mut cx,Target::ProviderModels,&json!({"authenticated":false,"models":[]}));
        assert!(panel.provider_models.is_empty());assert!(panel.runtime_efforts.is_empty());
    }
    #[test]
    fn completed_space_creation_opens_fresh_room_form_but_unknown_keeps_original_intent() {
        let mut cx=Cx::new(Box::new(|_,_|{}));let mut panel=cx.with_vm(|vm|{makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);let value=script_eval!(vm,{mod.widgets.HagencyPanel {}});HagencyPanel::script_from_value(vm,value)});
        panel.creation=Some(json!({"phase":"complete","commandId":"completed-space","spaceId":"!space:server"}));panel.pending_input=Some(json!({"kind":"space","name":"Old Project","commandId":"completed-space"}));
        panel.view.text_input(&mut cx,ids!(creation_name)).set_text(&mut cx,"Old Project");panel.view.label(&mut cx,ids!(creation_result)).set_text(&mut cx,"complete · !space:server");panel.view.button(&mut cx,ids!(create_project_room)).set_enabled(&mut cx,false);
        assert!(panel.begin_creation_form(&mut cx,true));assert!(panel.create_room);assert!(panel.creation.is_none());assert!(panel.pending_input.is_none());assert!(panel.input(&mut cx,ids!(creation_name)).is_empty());assert!(panel.view.label(&mut cx,ids!(creation_result)).text().is_empty());assert!(panel.view.button(&mut cx,ids!(create_project_room)).borrow().unwrap().enabled());
        let original=json!({"kind":"room","commandId":"original-room","name":"Original","projectId":"project-a","spaceId":"!space:server"});panel.pending_input=Some(original.clone());panel.creation=Some(json!({"phase":"unknown","commandId":"original-room"}));
        assert!(!panel.begin_creation_form(&mut cx,false));assert_eq!(panel.pending_input.as_ref(),Some(&original));assert_eq!(panel.creation.as_ref().unwrap()["commandId"],"original-room");assert!(panel.view.view(&mut cx,ids!(recovery)).visible());
        panel.creation=Some(json!({"phase":"complete","commandId":"original-room"}));assert!(panel.begin_creation_form(&mut cx,false));assert!(!panel.create_room);assert!(panel.pending_input.is_none());
    }
    #[test]
    fn native_management_widget_registers_and_instantiates_without_script_errors() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            vm.bx.captured_errors = Some(Vec::new());
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.HagencyPanel {} });
            let _panel = HagencyPanel::script_from_value(vm, value);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "Makepad registration errors: {errors:?}");
        });
    }
    #[test]
    fn management_responses_require_same_account_epoch_and_open_request() {
        let done = Completed {
            identity: "server/account/epoch1".into(),
            request: 7,
            target: Target::Changed,
            configuration_fence: None,
            configuration_digest: None,
            result: Ok(Value::Null),
        };
        assert!(current_completion(true, "server/account/epoch1", 7, &done));
        assert!(!current_completion(true, "server/account/epoch2", 7, &done));
        assert!(!current_completion(true, "server/account/epoch1", 8, &done));
        assert!(!current_completion(
            false,
            "server/account/epoch1",
            7,
            &done
        ));
    }
    #[test]
    fn policy_revision_cannot_be_reused_for_another_requester_or_room() {
        let p = json!({"agentId":"agent", "bindingId":"room-a", "requester":"@alice:server"});
        assert!(policy_matches(&p, "agent", "room-a", "@alice:server"));
        assert!(!policy_matches(&p, "agent", "room-a", "@bob:server"));
        assert!(!policy_matches(&p, "agent", "room-b", "@alice:server"));
        assert!(!policy_matches(
            &p,
            "other-agent",
            "room-a",
            "@alice:server"
        ));
    }
    #[test]
    fn uncertain_matrix_creation_only_resumes_original_id_and_input() {
        let input = json!({"commandId":"original-id","kind":"room","name":"Original","projectId":"project-a"});
        assert!(
            matches!(recovery_command(input.clone(), "unknown", String::new()), Command::ResumeCreation { id, room_id: None } if id == "original-id")
        );
        assert!(
            matches!(recovery_command(input.clone(), "room_created", "!known:server".into()), Command::ResumeCreation { id, room_id: Some(room) } if id == "original-id" && room == "!known:server")
        );
        assert!(
            matches!(recovery_command(input.clone(), "unrecorded", String::new()), Command::CreateMatrix { input: original } if original == input)
        );
    }
    #[test]
    fn confirmed_create_and_adopt_refresh_reads_without_replaying_mutations() {
        assert!(matches!(
            mutation_refresh(Target::AgentCreated),
            Some(Command::Agents)
        ));
        assert!(matches!(
            mutation_refresh(Target::AgentBound),
            Some(Command::Agents)
        ));
        assert!(matches!(
            mutation_refresh(Target::ProjectAdopted),
            Some(Command::Projects)
        ));
        assert!(matches!(
            mutation_refresh(Target::RoomAdopted),
            Some(Command::Projects)
        ));
        assert!(mutation_refresh(Target::Changed).is_none());
        assert!(mutation_refresh(Target::Policy).is_none());
        assert!(mutation_refresh(Target::Creation).is_none());
    }
    #[test]
    fn native_confirmed_mutations_close_forms_but_policy_saves_keep_selection() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.HagencyPanel {} });
            HagencyPanel::script_from_value(vm, value)
        });
        panel.projects = vec![json!({"id":"project-original","spaceId":"!space:server"})];
        panel.labels(&mut cx, ids!(projects), "Select", &panel.projects, "id");
        panel
            .view
            .drop_down(&mut cx, ids!(projects))
            .set_selected_item(&mut cx, 1);
        panel
            .view
            .view(&mut cx, ids!(project_form))
            .set_visible(&mut cx, true);
        panel.apply(&mut cx, Target::RoomAdopted, &Value::Null);
        assert!(!panel.view.view(&mut cx, ids!(project_form)).visible());
        assert_eq!(panel.wanted_project.as_deref(), Some("project-original"));
        panel
            .view
            .view(&mut cx, ids!(agent_create))
            .set_visible(&mut cx, true);
        panel.agent_command = Some((json!({"displayName":"A"}), "same-id".into()));
        panel.apply(&mut cx, Target::Changed, &Value::Null);
        assert!(panel.view.view(&mut cx, ids!(agent_create)).visible());
        assert_eq!(
            panel.agent_command.as_ref().map(|v| v.1.as_str()),
            Some("same-id")
        );
        panel.apply(&mut cx, Target::AgentCreated, &Value::Null);
        assert!(!panel.view.view(&mut cx, ids!(agent_create)).visible());
        assert!(panel.agent_command.is_none());
    }
    #[test]
    fn portal_empty_state_appears_once_and_real_rows_never_become_placeholders() {
        let templates = (0..8).map(|i| card_template(0, i)).collect::<Vec<_>>();
        assert_eq!(templates.iter().filter(|&&id| id == id!(Empty)).count(), 1);
        assert!(templates[1..].iter().all(|&id| id == id!(Filler)));
        assert_eq!(card_template(2, 0), id!(Card));
        assert_eq!(card_template(2, 1), id!(Card));
        assert_eq!(card_template(2, 2), id!(Filler));
        assert_eq!(card_template(2, 7), id!(Filler));
    }
    #[test]
    fn registered_portal_card_clicks_and_back_return_to_the_same_list() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.HagencyPanel {} });
            HagencyPanel::script_from_value(vm, value)
        });
        panel.projects = vec![
            json!({"id":"project-a","name":"A"}),
            json!({"id":"project-b","name":"B"}),
        ];
        panel.agents = vec![
            json!({"id":"agent-a","displayName":"Agent A"}),
            json!({"id":"agent-b","displayName":"Agent B"}),
        ];
        panel.labels(&mut cx, ids!(projects), "Select", &panel.projects, "id");
        panel.labels(&mut cx, ids!(agents), "Select", &panel.agents, "id");
        for page in [Page::Projects, Page::Agents] {
            panel.set_page(&mut cx, page);
            panel.list_page(&mut cx);
            panel.sync_navigation(&mut cx);
            assert!(!panel.view.button(&mut cx, ids!(back)).visible());
            assert!(panel.view.button(&mut cx, ids!(close)).borrow().is_none());
            assert!(panel.view.button(&mut cx, ids!(new_item)).visible());
            assert!(panel.view.button(&mut cx, ids!(refresh)).visible());
            let list = panel.view.portal_list(
                &mut cx,
                if page == Page::Projects {
                    ids!(project_list)
                } else {
                    ids!(agent_list)
                },
            );
            let row = list.item(&mut cx, 1, id!(Card));
            assert!(!row.is_empty());
            let button = row.button(&mut cx, ids!(open));
            assert!(
                button.borrow().is_some(),
                "registered card must contain a real Button"
            );
            let actions = cx.capture_actions(|cx| {
                cx.group_widget_actions(list.widget_uid(), row.widget_uid(), |cx| {
                    cx.widget_action(
                        button.widget_uid(),
                        ButtonAction::Clicked(KeyModifiers::default()),
                    );
                });
            });
            assert!(panel.open_card(&mut cx, &actions));
            panel.sync_navigation(&mut cx);
            assert!(panel.view.button(&mut cx, ids!(back)).visible());
            assert!(panel.view.button(&mut cx, ids!(close)).borrow().is_none());
            assert!(!panel.view.button(&mut cx, ids!(new_item)).visible());
            assert!(!panel.view.button(&mut cx, ids!(refresh)).visible());
            let (selection, detail, list_id) = if page == Page::Projects {
                (
                    ids!(projects),
                    ids!(project_detail),
                    ids!(project_list_holder),
                )
            } else {
                (ids!(agents), ids!(agent_detail), ids!(agent_list_holder))
            };
            assert_eq!(panel.view.drop_down(&mut cx, selection).selected_item(), 2);
            assert!(panel.view.view(&mut cx, detail).visible());
            assert!(!panel.view.view(&mut cx, list_id).visible());
            let back = panel.view.button(&mut cx, ids!(back));
            let actions = cx.capture_actions(|cx| {
                cx.widget_action(back.widget_uid(), ButtonAction::Clicked(KeyModifiers::default()));
            });
            assert!(panel.back_to_list(&mut cx, &actions));
            panel.sync_navigation(&mut cx);
            assert!(!panel.view.button(&mut cx, ids!(back)).visible());
            assert!(panel.view.view(&mut cx, list_id).visible());
            assert!(!panel.view.view(&mut cx, detail).visible());
            assert!(panel.view.button(&mut cx, ids!(close)).borrow().is_none());
        }
    }
    #[test]
    fn project_and_room_members_have_independent_targets_permissions_and_late_result_fences() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            HagencyPanel::script_from_value(vm, value)
        });
        let space = json!({"roomId":"!space:server","space":true,"canInvite":true,"members":[
            {"userId":"@joined:server","membership":"join","role":"Administrator","power":"100"},
            {"userId":"@invited:server","membership":"invite","role":"User","power":"0"}]});
        panel.members_room = Some(("!space:server".into(), true));
        panel.apply(&mut cx, Target::Members, &space);
        assert_eq!(panel.members.len(), 2);
        assert!(
            panel
                .view
                .button(&mut cx, ids!(invite_member))
                .borrow()
                .unwrap()
                .enabled()
        );
        assert_eq!(member_summary(&panel.members[1]), "Invited · Member");
        panel.clear_members(&mut cx);
        panel.members_room = Some(("!private:server".into(), false));
        panel.apply(&mut cx, Target::Members, &space);
        assert!(
            panel.members.is_empty(),
            "Space members must not populate a private Room"
        );
        assert!(
            !panel
                .view
                .button(&mut cx, ids!(invite_member))
                .borrow()
                .unwrap()
                .enabled()
        );
        let room = json!({"roomId":"!private:server","space":false,"canInvite":false,"members":[{"userId":"@room-only:server","membership":"join"}]});
        panel.apply(&mut cx, Target::Members, &room);
        assert_eq!(panel.members.len(), 1);
        assert_eq!(string(&panel.members[0], "userId"), "@room-only:server");
        panel.clear_members(&mut cx);
        panel.apply(&mut cx, Target::Members, &room);
        assert!(
            panel.members.is_empty(),
            "clearing an account/target blocks late members"
        );
        assert!(user_error("room_join_required").contains("independent membership"));
    }
    #[test]
    fn project_menu_section_waits_for_its_own_project_rooms() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            HagencyPanel::script_from_value(vm, value)
        });
        panel.projects = vec![json!({"id":"project-a","name":"Design","spaceId":"!space:server"})];
        panel.labels(&mut cx, ids!(projects), "Select", &panel.projects, "id");
        panel
            .view
            .drop_down(&mut cx, ids!(projects))
            .set_selected_item(&mut cx, 1);
        panel.wanted_project_section = Some(("project-a".into(), ProjectDetailSection::Rooms));
        panel.project_detail(&mut cx, 0);
        assert!(panel.view.view(&mut cx, ids!(rooms_section)).visible(), "requested tab is visible during loading too");
        assert!(!panel.view.view(&mut cx, ids!(overview_section)).visible());
        panel.apply(&mut cx, Target::Rooms, &json!({"rooms":[]}));
        assert!(panel.view.view(&mut cx, ids!(rooms_section)).visible());
        assert!(!panel.view.view(&mut cx, ids!(overview_section)).visible());
        assert!(panel.wanted_project_section.is_none());
        panel.project_section(&mut cx, ids!(overview_section));
        panel.wanted_project_section = Some(("project-b".into(), ProjectDetailSection::Members));
        panel.apply(&mut cx, Target::Rooms, &json!({"rooms":[]}));
        assert!(panel.view.view(&mut cx, ids!(overview_section)).visible());
        assert!(!panel.view.view(&mut cx, ids!(members_section)).visible());
        assert!(
            panel.members_room.is_none(),
            "another project's response cannot start a members request"
        );
    }

    #[test]
    fn project_detail_sections_are_exclusive_and_preserve_project_location() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut panel = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.HagencyPanel {}});
            HagencyPanel::script_from_value(vm, value)
        });
        panel.projects = vec![json!({"id":"project","name":"Design","spaceId":"!space:server"})];
        panel.labels(&mut cx, ids!(projects), "Select", &panel.projects, "id");
        panel.project_detail(&mut cx, 0);
        assert_eq!(
            panel
                .view
                .drop_down(&mut cx, ids!(projects))
                .selected_item(),
            1
        );
        for selected in [
            ids!(overview_section),
            ids!(rooms_section),
            ids!(members_section),
        ] {
            panel.project_section(&mut cx, selected);
            for id in [
                ids!(overview_section),
                ids!(rooms_section),
                ids!(members_section),
            ] {
                assert_eq!(panel.view.view(&mut cx, id).visible(), id == selected);
            }
            assert!(panel.view.view(&mut cx, ids!(project_detail)).visible());
            assert!(
                !panel
                    .view
                    .view(&mut cx, ids!(project_list_holder))
                    .visible()
            );
        }
    }
}
