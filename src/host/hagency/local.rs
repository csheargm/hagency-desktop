//! Local-agent controls for the Agent menu: service, private chat, encryption
//! and Rooms. Uses the same native owner engine and Matrix client as the
//! native panel. Scripts pass only an Agent ID; Room IDs come from bindings.
use super::{parse, Id, ManagementPermission, Value};
use crate::hagency::{
    backend::Service,
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
    running: bool,
}
pub(super) enum Call {
    Status(String),
    Service(String, bool),
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
            Call::Service(input.id, input.running)
        }
        CHAT_OPEN => Call::ChatOpen(parse::<Id>(args)?.id),
        CHAT_ENCRYPT => Call::ChatEncrypt(parse::<Id>(args)?.id),
        _ => return Ok(None),
    };
    match &call {
        Call::Status(id) | Call::Service(id, _) | Call::ChatOpen(id) | Call::ChatEncrypt(id) => agent_id(id)?,
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
        || (service["enabled"] == true && service["watcherRunning"] == false)
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
fn room_name(room: &matrix_sdk::Room) -> String {
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
async fn agent(service: &Service, id: &str) -> Result<Agent, String> {
    let details = service.execute(Command::AgentDetails { agent_id: id.into() }).await?;
    let record = &details["agent"];
    if text(record, "id") != id {
        return Err("personal_agent_not_found".into());
    }
    let devices = service.execute(Command::Devices).await?;
    let current = text(&devices, "currentDeviceId");
    let device = text(record, "executionDeviceId");
    let record = devices["devices"]
        .as_array()
        .and_then(|all| all.iter().find(|d| text(d, "id") == device && d["revoked"] == false));
    let here = !current.is_empty() && device == current && record.is_some();
    let device_name = record.map(|d| text(d, "name").to_owned()).unwrap_or_default();
    let bindings = service.execute(Command::Bindings { agent_id: id.into() }).await?;
    let bindings = bindings["bindings"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|b| text(b, "agentId") == id)
        .collect();
    Ok(Agent { here, device_name, bindings })
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
    let service = service_for(account)?;
    let agent = agent(&service, id).await?;
    let state = if agent.here {
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
            Ok(value) => service_state(&value, id),
            Err(_) => "unknown",
        }
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
            json!({"id": text(b, "id"), "name": name, "state": text(b, "state")})
        })
        .collect();
    // Daily and monthly limits reset at UTC midnight; show it in local time.
    let reset = chrono::Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .map(|midnight| midnight.and_utc().with_timezone(&chrono::Local).format("%H:%M").to_string());
    Ok(json!({"agentId": id, "here": agent.here, "deviceName": agent.device_name, "service": {"state": state}, "chat": chat, "rooms": rooms, "resetLocalTime": reset}))
}

impl Call {
    pub(super) async fn execute(self, account: &str) -> Result<Value, String> {
        match self {
            Self::Status(id) => status(account, &id).await,
            Self::Service(id, running) => {
                let service = service_for(account)?;
                if !agent(&service, &id).await?.here {
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
            Self::ChatOpen(id) => {
                let service = service_for(account)?;
                let room_id = direct_room(&agent(&service, &id).await?)?;
                makepad_widgets::Cx::post_action(crate::hagency::ui::OpenAgentChat { room_id: room_id.clone() });
                Ok(json!({"roomId": room_id.to_string()}))
            }
            Self::ChatEncrypt(id) => {
                let service = service_for(account)?;
                let room_id = direct_room(&agent(&service, &id).await?)?;
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
        let mut discovering = no_rooms.clone();
        discovering["service"]["phase"] = "starting".into();
        assert_eq!(service_state(&discovering, agent), "starting");
        // Another Agent's runtime never reports this Agent as running.
        let other = json!({"service": {"enabled": false}, "runtime": {"agentId": "agt_2", "phase": "online"}});
        assert_eq!(service_state(&other, agent), "stopped");
    }
    #[test]
    fn only_agent_ids_are_accepted() {
        assert!(parse_call(STATUS, json!({"id": "agt_01m5"})).unwrap().is_some());
        assert!(parse_call(STATUS, json!({"id": "!room:server"})).is_err());
        assert!(parse_call(SERVICE, json!({"id": "agt_1", "running": true, "roomId": "!r:s"})).is_err());
        assert!(parse_call("hagency.personal_agents.list", json!({})).unwrap().is_none());
    }
}
