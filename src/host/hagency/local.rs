//! Local-agent controls for the Agent menu: service, private chat, encryption
//! and Rooms. Uses the same native owner engine and Matrix client as the
//! native panel. Scripts pass only an Agent ID; Room IDs come from bindings.
// TODO(R4): over budget; split the tests into local_tests.rs and the device/binding reads into local_agent.rs.
use super::{parse, Id, ManagementPermission, Value};
use crate::hagency::{
    backend::{LocalAgentHost, Service},
    model::{Command, LocalAction},
};
use serde::Deserialize;
use serde_json::json;
pub(super) const STATUS: &str = "hagency.personal_agents.status";
pub(super) const SERVICE: &str = "hagency.personal_agents.service";
pub(super) const CHAT_OPEN: &str = "hagency.personal_agents.chat.open";
pub(super) const CHAT_ENCRYPT: &str = "hagency.personal_agents.chat.encrypt";
pub(super) fn permissions(service: &str) -> &'static [ManagementPermission] {
    use ManagementPermission::*;
    match service {
        STATUS => &[ReadPersonalAgentSettings],
        SERVICE | CHAT_OPEN | CHAT_ENCRYPT => &[ReadPersonalAgentSettings, EditPersonalAgentPolicy],
        _ => &[],
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SetService {
    id: String,
    #[serde(default)]
    running: Option<bool>,
    /// Revoke one remembered "Always allow" rule `{tool, directory, request}`.
    #[serde(default)]
    forget: Option<Value>,
    /// "ask", "remember" or "outside" (ask only for outside changes).
    #[serde(default)]
    approval_mode: Option<String>,
    /// The opt-in agent auto-merge switch (off by default).
    #[serde(default)]
    auto_merge: Option<bool>,
}
// TODO(R4): local.rs is over budget; move service_state and the status reply into host/hagency/local/status.rs.
pub(crate) enum Call {
    Status(String),
    Service(String, bool),
    /// `{forget}`, `{mode}`, `{autoMerge}` or `{roomShare}` for the Agent's approval settings.
    Approvals(String, Value),
    ChatOpen(String),
    ChatEncrypt(String),
}
fn agent_id(id: &str) -> Result<(), String> {
    if (1..=128).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        Ok(())
    } else {
        Err("invalid_management_arguments".into())
    }
}
pub(super) fn parse_call(service: &str, args: Value) -> Result<Option<Call>, String> {
    let call = match service {
        STATUS => Call::Status(parse::<Id>(args)?.id),
        SERVICE => {
            let input: SetService = parse(args)?;
            match (input.running, input.forget, input.approval_mode, input.auto_merge) {
                (Some(running), None, None, None) => Call::Service(input.id, running),
                (None, Some(rule), None, None) if rule.is_object() => Call::Approvals(input.id, json!({"forget": rule})),
                (None, None, Some(mode), None) if ["ask", "remember", "outside"].contains(&mode.as_str()) => Call::Approvals(input.id, json!({"mode": mode})),
                (None, None, None, Some(on)) => Call::Approvals(input.id, json!({"autoMerge": on})),
                _ => return Err("invalid_management_arguments".into()),
            }
        }
        CHAT_OPEN => Call::ChatOpen(parse::<Id>(args)?.id),
        CHAT_ENCRYPT => Call::ChatEncrypt(parse::<Id>(args)?.id),
        _ => return Ok(None),
    };
    match &call {
        Call::Status(id) | Call::Service(id, _) | Call::Approvals(id, _) | Call::ChatOpen(id) | Call::ChatEncrypt(id) => agent_id(id)?,
    }
    Ok(Some(call))
}

/// The service's own account must be the mini app's account.
fn service_for(account: &str) -> Result<Service, String> {
    let service = Service::for_current()?;
    if crate::sliding_sync::current_user_id().is_none_or(|user| user.as_str() != account) {
        return Err("matrix_account_changing".into());
    }
    Ok(service)
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
/// `hagency.session` says whether this computer runs this account's Agents,
/// so the page offers a local Agent only where one can run, and names what
/// is missing otherwise. Display only: it authorizes nothing, and it is left
/// out when it cannot be told right now.
pub(super) fn display(service: &str, mut value: Value) -> Value {
    if service != "hagency.session" {
        return value;
    }
    if let Ok(host) = Service::for_current().and_then(|service| service.local_agent_host()) {
        host_fields(&mut value, host);
    }
    value
}
fn host_fields(value: &mut Value, host: LocalAgentHost) {
    let Some(object) = value.as_object_mut() else { return };
    object.insert("localAgentsHere".into(), json!(host == LocalAgentHost::Here));
    if let Some(need) = host.need() {
        object.insert("localAgentsNeed".into(), json!(need));
    }
}
/// Started by the owner, but its host on this computer is not running: the
/// service watcher, and with it every Room worker, has stopped.
pub(crate) fn host_stopped(value: &Value) -> bool {
    value["service"]["enabled"] == true && value["service"]["watcherRunning"] == false
}
/// One state per Agent, worded by the mini app: processing, running,
/// starting, waiting, attention or stopped.
fn service_state(value: &Value, agent: &str) -> &'static str {
    let service = &value["service"];
    let runtime = &value["runtime"];
    let ours = text(runtime, "agentId") == agent;
    let phases: Vec<&str> = if !ours {
        Vec::new()
    } else if let Some(bindings) = runtime["bindings"].as_array().filter(|b| !b.is_empty()) {
        bindings.iter().map(|b| text(b, "phase")).collect()
    } else {
        vec![text(runtime, "phase")]
    };
    let any = |set: &[&str]| phases.iter().any(|p| set.contains(p));
    let attention = !text(service, "lastError").is_empty()
        || service["problems"].as_array().is_some_and(|p| !p.is_empty())
        // The watcher can be healthy with no connected Rooms. Its completed
        // discovery is not an Agent startup still making progress.
        || (service["enabled"] == true && service["phase"] == "serving"
            && service["rooms"].as_array().is_some_and(|rooms| rooms.is_empty())
            && !ours)
        || (ours && !text(runtime, "lastError").is_empty());
    if any(&["executing", "executing_chat_only", "executing_host_files"]) {
        "processing"
    } else if any(&["awaiting_approval", "waiting_owner"]) {
        "waiting"
    } else if host_stopped(value) {
        // The host watchdog (backend/host_watchdog.rs) starts it again.
        "host_restarting"
    } else if attention {
        "attention"
    } else if any(&["online", "online_chat_only"]) {
        "running"
    } else if service["enabled"] == true
        || any(&["starting", "checking_provider", "waiting_for_lease"])
    {
        "starting"
    } else {
        "stopped"
    }
}
pub(crate) fn room_name(room: &matrix_sdk::Room) -> String {
    room.cached_display_name()
        .map(|name| name.to_string())
        .or_else(|| room.name())
        .unwrap_or_default()
}

struct Agent {
    here: bool,
    device_name: String,
    bindings: Vec<Value>,
}
/// `tolerate`: a failed Rooms read leaves the Rooms empty (status, service,
/// approvals); chat actions need the bindings and pass the failure on.
async fn agent(service: &Service, id: &str, tolerate: bool) -> Result<Agent, String> {
    let details = service.execute(Command::AgentDetails { agent_id: id.into() }).await.inspect_err(|code| log_failure("details", code))?;
    let record = &details["agent"];
    if text(record, "id") != id {
        return Err("personal_agent_not_found".into());
    }
    let devices = service.execute(Command::Devices).await.inspect_err(|code| log_failure("devices", code))?;
    let (here, device_name) = device_of(record, &devices);
    // Rooms are a part of the status, not a condition for it: a failed read
    // must not hide that the Agent runs here.
    let bindings = match service.execute(Command::Bindings { agent_id: id.into() }).await {
        Ok(reply) => agent_bindings(&reply, id),
        Err(code) if tolerate => {
            log_failure("bindings", &code);
            Vec::new()
        }
        Err(code) => return Err(code),
    };
    Ok(Agent { here, device_name, bindings })
}
/// Whether `record` runs on this device, by the same check as My Agents'
/// "This device" badge, and the assigned device's name.
fn device_of(record: &Value, devices: &Value) -> (bool, String) {
    use crate::hagency::ui::{AgentDeviceState, agent_device_state};
    let current = text(devices, "currentDeviceId");
    let all = devices["devices"].as_array().map(Vec::as_slice).unwrap_or_default();
    let here = agent_device_state(record, current, all, devices["devices"].is_array()) == AgentDeviceState::Here;
    let device = text(record, "executionDeviceId");
    let name = all.iter().find(|d| text(d, "id") == device && d["revoked"] == false).map(|d| text(d, "name").to_owned());
    (here, name.unwrap_or_default())
}
fn agent_bindings(reply: &Value, id: &str) -> Vec<Value> {
    reply["bindings"].as_array().into_iter().flatten().filter(|b| text(b, "agentId") == id).cloned().collect()
}
/// Codes the status reads are known to return; only these are ever logged.
const LOGGED_CODES: &[&str] = &[
    "personal_agent_not_found", "matrix_account_changing", "sign_in_required", "owner_authorization_required",
    "local_state_unavailable", "invalid_server_response", "invalid_arguments", "not_found", "device_setup_required",
    "native_owner_drain_required", "management_outcome_unknown", "agent_paused", "storage_unavailable",
];
/// The logged form of `code`: a known static code, else "unrecognized".
fn logged_code(code: &str) -> &'static str {
    LOGGED_CODES.iter().copied().find(|known| *known == code).unwrap_or("unrecognized")
}
/// Logs which part of the panel's status failed, without echoing any reply text.
fn log_failure(stage: &'static str, code: &str) {
    makepad_widgets::warning!("Agent panel status: {stage} failed: {}", logged_code(code));
}
/// The Agent's active owner-direct Room, never a Room the script names.
fn direct_room(agent: &Agent) -> Result<matrix_sdk::ruma::OwnedRoomId, String> {
    let binding = agent
        .bindings
        .iter()
        .find(|b| text(b, "scopeKind") == "owner_direct" && b["projectId"].is_null() && text(b, "state") == "active")
        .ok_or("personal_agent_chat_unavailable")?;
    matrix_sdk::ruma::OwnedRoomId::try_from(text(binding, "roomId")).map_err(|_| "personal_agent_chat_unavailable".into())
}

async fn status(account: &str, id: &str) -> Result<Value, String> {
    let service = service_for(account).inspect_err(|code| log_failure("account", code))?;
    let agent = agent(&service, id, true).await?;
    let state = if agent.here && service.local_agent_host().is_ok_and(|host| host != LocalAgentHost::Here) {
        // The Agent is assigned to this device, but nothing on this computer
        // runs this account's Agents (another account is hosted here, or
        // hosting is not set up). Its saved service state would only mislead.
        "no_host"
    } else if agent.here {
        local_service_state(&service, account, id).await
    } else {
        "elsewhere"
    };
    let client = crate::sliding_sync::get_client();
    let chat = direct_room(&agent).ok().map(|room_id| {
        let room = client.as_ref().and_then(|c| c.get_room(&room_id));
        json!({
            "roomId": room_id.to_string(),
            "encrypted": room.as_ref().is_some_and(|r| r.encryption_state().is_encrypted()),
        })
    });
    let rooms: Vec<Value> = agent
        .bindings
        .iter()
        .filter(|b| text(b, "scopeKind") != "owner_direct")
        .map(|b| {
            let name = matrix_sdk::ruma::OwnedRoomId::try_from(text(b, "roomId"))
                .ok()
                .and_then(|room_id| client.as_ref().and_then(|c| c.get_room(&room_id)))
                .map(|room| room_name(&room))
                .unwrap_or_default();
            room_entry(b, name)
        })
        .collect();
    // Daily and monthly limits reset at UTC midnight; show it in local time.
    let reset = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|midnight| midnight.and_utc().with_timezone(&chrono::Local).format("%H:%M").to_string());
    // Only the device that runs the Agent holds its approval settings.
    let approvals = if agent.here { approval_settings(&service, id, None).await.inspect_err(|code| log_failure("approvals", code)).ok() } else { None };
    Ok(json!({"agentId": id, "here": agent.here, "deviceName": agent.device_name, "service": {"state": state}, "chat": chat, "rooms": rooms, "resetLocalTime": reset, "approvals": approvals}))
}
/// The running service's state on this device, worded for the mini app.
async fn local_service_state(service: &Service, account: &str, id: &str) -> &'static str {
    let status = service
        .execute(Command::Local {
            agent_id: id.into(),
            action: LocalAction::AgentServiceStatus,
            input: None,
            binding_id: None,
            requester: None,
        })
        .await;
    match status {
        // Stopped by the owner's Stop all: shown as "Paused by you".
        Ok(value) if service_state(&value, id) == "stopped" && crate::hagency::kill_switch::is_paused(&crate::hagency::kill_switch::default_path(), account, id) => "paused",
        Ok(value) => service_state(&value, id),
        Err(code) => {
            log_failure("service", &code);
            "unknown"
        }
    }
}
/// One project Room of the status reply. `roomId` is what issue watching
/// saves; without it the Agent's Rooms were never offered there.
pub(crate) fn room_entry(binding: &Value, name: String) -> Value {
    json!({"id": text(binding, "id"), "roomId": text(binding, "roomId"), "name": name, "state": text(binding, "state")})
}
/// The Agent's approval mode and "Always allow" rules, after `change` if given.
async fn approval_settings(service: &Service, id: &str, change: Option<Value>) -> Result<Value, String> {
    let action = if change.is_some() { LocalAction::SaveApprovalSettings } else { LocalAction::ApprovalSettings };
    service.execute(Command::Local { agent_id: id.into(), action, input: change, binding_id: None, requester: None }).await
}

impl Call {
    pub(crate) async fn execute(self, account: &str) -> Result<Value, String> {
        match self {
            Self::Status(id) => status(account, &id).await,
            Self::Service(id, running) => {
                let service = service_for(account)?;
                if !agent(&service, &id, true).await?.here {
                    return Err("personal_agent_not_on_this_device".into());
                }
                service
                    .execute(Command::Local {
                        agent_id: id.clone(),
                        action: if running { LocalAction::StartAgentService } else { LocalAction::StopAgentService },
                        input: running.then(|| json!({})),
                        binding_id: None,
                        requester: None,
                    })
                    .await?;
                status(account, &id).await
            }
            Self::Approvals(id, change) => {
                let service = service_for(account)?;
                if !agent(&service, &id, true).await?.here {
                    return Err("personal_agent_not_on_this_device".into());
                }
                approval_settings(&service, &id, Some(change)).await?;
                status(account, &id).await
            }
            Self::ChatOpen(id) => {
                let service = service_for(account)?;
                let room_id = direct_room(&agent(&service, &id, false).await?)?;
                makepad_widgets::Cx::post_action(crate::hagency::ui::OpenAgentChat { room_id: room_id.clone() });
                Ok(json!({"roomId": room_id.to_string()}))
            }
            Self::ChatEncrypt(id) => {
                let service = service_for(account)?;
                let room_id = direct_room(&agent(&service, &id, false).await?)?;
                let room = crate::sliding_sync::get_client()
                    .and_then(|client| client.get_room(&room_id))
                    .ok_or("personal_agent_chat_unavailable")?;
                // Matrix cannot turn a room's encryption off again.
                room.enable_encryption().await.map_err(|_| "personal_agent_encryption_failed")?;
                status(account, &id).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_state_reduces_runtime_phases() {
        let agent = "agt_1";
        let runtime = |phase: &str| json!({"service": {"enabled": true, "watcherRunning": true},
            "runtime": {"agentId": agent, "phase": "online", "bindings": [{"phase": phase}]}});
        assert_eq!(service_state(&runtime("online_chat_only"), agent), "running");
        assert_eq!(service_state(&runtime("executing_chat_only"), agent), "processing");
        assert_eq!(service_state(&runtime("waiting_owner"), agent), "waiting");
        assert_eq!(service_state(&runtime("starting"), agent), "starting");
        assert_eq!(service_state(&json!({"service": {"enabled": false}, "runtime": null}), agent), "stopped");
        assert_eq!(service_state(&json!({"service": {"enabled": true, "lastError": "provider"}, "runtime": null}), agent), "attention");
        let no_rooms = json!({"service":{"enabled":true,"watcherRunning":true,"phase":"serving","rooms":[]},"runtime":null});
        assert_eq!(service_state(&no_rooms, agent), "attention");
        // Regression (2026-11-17): a stopped host must not read as running or
        // as a vague "attention"; the watchdog is restarting it.
        let down = json!({"service":{"enabled":true,"watcherRunning":false,"lastError":"authorization_or_lease_lost"},"runtime":null});
        assert_eq!(service_state(&down, agent), "host_restarting");
        assert!(!host_stopped(&json!({"service":{"enabled":false,"watcherRunning":false}})));
        let mut discovering = no_rooms.clone();
        discovering["service"]["phase"] = "starting".into();
        assert_eq!(service_state(&discovering, agent), "starting");
        // Another Agent's runtime never reports this Agent as running.
        let other = json!({"service": {"enabled": false}, "runtime": {"agentId": "agt_2", "phase": "online"}});
        assert_eq!(service_state(&other, agent), "stopped");
    }
    #[test]
    fn session_names_what_a_local_agent_needs_here() {
        use crate::hagency::hosting::{Execution, SetupReason};
        let reply = |execution: Execution| {
            let mut value = json!({"protocol": "hagency-management-v1"});
            host_fields(&mut value, LocalAgentHost::of(&execution));
            value
        };
        let hosted = reply(Execution::Daemon { home: "/daemon".into() });
        assert_eq!(hosted["localAgentsHere"], true);
        assert!(hosted.get("localAgentsNeed").is_none());
        assert_eq!(reply(Execution::InProcess)["localAgentsHere"], true);
        let other = reply(Execution::Setup(SetupReason::OtherOwner));
        assert_eq!((other["localAgentsHere"].clone(), other["localAgentsNeed"].clone()), (json!(false), json!("other_account")));
        for reason in [SetupReason::NotEnrolled, SetupReason::StaleRoot] {
            let setup = reply(Execution::Setup(reason));
            assert_eq!((setup["localAgentsHere"].clone(), setup["localAgentsNeed"].clone()), (json!(false), json!("hosting_setup")));
        }
        // A reply that is not an object is left alone.
        let mut odd = json!("text");
        host_fields(&mut odd, LocalAgentHost::OtherAccount);
        assert_eq!(odd, json!("text"));
    }
    /// The panel's `here` and My Agents' "This device" are one check: an
    /// Agent shown as running on this device is editable in the panel.
    #[test]
    fn panel_here_matches_my_agents_badge() {
        let devices = json!({"currentDeviceId": "dev_a", "devices": [
            {"id": "dev_a", "name": "Hagency Desktop", "revoked": false}, {"id": "dev_b", "name": "Old", "revoked": true}]});
        let agent = |device: &str| json!({"id": "agt_1", "executionDeviceId": device, "state": "active", "generation": 1});
        assert_eq!(device_of(&agent("dev_a"), &devices), (true, "Hagency Desktop".to_owned()));
        assert_eq!(device_of(&agent("dev_b"), &devices), (false, String::new()), "a revoked device is not here");
        assert_eq!(device_of(&agent("dev_c"), &devices).0, false);
        assert_eq!(device_of(&agent("dev_a"), &json!({"devices": []})).0, false, "no current device");
    }
    /// A Room joined by auto-join is a project binding of this Agent and is listed.
    #[test]
    fn bindings_keep_only_this_agents_rows() {
        let reply = json!({"bindings": [
            {"id": "b1", "agentId": "agt_1", "scopeKind": "owner_direct", "roomId": "!dm:x", "state": "active"},
            {"id": "b2", "agentId": "agt_1", "scopeKind": "project", "roomId": "!fix:x", "state": "active"},
            {"id": "b3", "agentId": "agt_2", "scopeKind": "project", "roomId": "!other:x", "state": "active"}]});
        let ids: Vec<_> = agent_bindings(&reply, "agt_1").iter().map(|b| text(b, "id").to_owned()).collect();
        assert_eq!(ids, ["b1", "b2"]);
        assert!(agent_bindings(&json!({}), "agt_1").is_empty());
    }
    /// Only known static codes reach the log; reply text never does.
    #[test]
    fn only_known_codes_are_logged() {
        assert_eq!(logged_code("personal_agent_not_found"), "personal_agent_not_found");
        assert_eq!(logged_code("secret_value"), "unrecognized");
        assert_eq!(logged_code("token=abc"), "unrecognized");
    }
    #[test]
    fn only_agent_ids_are_accepted() {
        assert!(parse_call(STATUS, json!({"id": "agt_01m5"})).unwrap().is_some());
        assert!(parse_call(STATUS, json!({"id": "!room:server"})).is_err());
        assert!(parse_call(SERVICE, json!({"id": "agt_1", "running": true, "roomId": "!r:s"})).is_err());
        // Approval settings: one change per call, known modes only.
        assert!(matches!(parse_call(SERVICE, json!({"id": "agt_1", "approvalMode": "remember"})), Ok(Some(Call::Approvals(..)))));
        assert!(matches!(parse_call(SERVICE, json!({"id": "agt_1", "forget": {"tool": "claude.Bash"}})), Ok(Some(Call::Approvals(..)))));
        assert!(parse_call(SERVICE, json!({"id": "agt_1", "approvalMode": "trusted"})).is_err());
        assert!(matches!(parse_call(SERVICE, json!({"id": "agt_1", "autoMerge": true})), Ok(Some(Call::Approvals(_, v))) if v == json!({"autoMerge": true})));
        assert!(parse_call(SERVICE, json!({"id": "agt_1", "autoMerge": true, "approvalMode": "ask"})).is_err());
        assert!(parse_call(SERVICE, json!({"id": "agt_1", "running": true, "approvalMode": "ask"})).is_err());
        assert!(parse_call(SERVICE, json!({"id": "agt_1"})).is_err());
        assert!(parse_call("hagency.personal_agents.list", json!({})).unwrap().is_none());
    }
}
