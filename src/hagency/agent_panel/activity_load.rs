//! Loads the Activity tab: this device's recent requests for the Agent plus
//! its waiting approvals, with room and requester names this client knows.
use crate::hagency::{backend::Service, model::{Command, LocalAction}};
use serde_json::{Value, json};

fn local(agent_id: &str, action: LocalAction) -> Command {
    Command::Local { agent_id: agent_id.into(), action, input: None, binding_id: None, requester: None }
}

/// `{requests, approvals}`; approvals are best effort (the runtime may be stopped).
pub(super) async fn load(service: Service, agent_id: String) -> Result<Value, String> {
    let activity = service.execute(local(&agent_id, LocalAction::RecentActivity)).await?;
    // A stopped runtime has no waiting approvals to show; the requests still load.
    let approvals = service.local_approvals(agent_id.clone()).await
        .map(|v| v["approvals"].clone()).unwrap_or(Value::Null);
    let mut requests = activity["requests"].as_array().cloned().unwrap_or_default();
    for request in &mut requests {
        name(request).await;
    }
    Ok(json!({"requests": requests, "approvals": approvals}))
}

/// Adds `roomName` and `requesterName` when this client knows them.
async fn name(request: &mut Value) {
    let me = crate::sliding_sync::current_user_id().map(|id| id.to_string());
    let requester = request["requester"].as_str().unwrap_or("").to_owned();
    if me.as_deref() == Some(requester.as_str()) {
        request["requesterName"] = crate::i18n::tr("You").into();
    }
    let Some(client) = crate::sliding_sync::get_client() else { return };
    let Ok(room_id) = matrix_sdk::ruma::OwnedRoomId::try_from(request["roomId"].as_str().unwrap_or("")) else { return };
    let Some(room) = client.get_room(&room_id) else { return };
    request["roomName"] = crate::host::hagency::local::room_name(&room).into();
    if request["requesterName"].is_string() { return; }
    let Ok(user_id) = matrix_sdk::ruma::OwnedUserId::try_from(requester) else { return };
    if let Ok(Some(member)) = room.get_member_no_sync(&user_id).await
        && let Some(display) = member.display_name()
    {
        request["requesterName"] = display.into();
    }
}
