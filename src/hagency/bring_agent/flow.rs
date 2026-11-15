//! The async steps: read the owner's default Agent, look at the room, then
//! bind through `Service::invite_agent_to_room` (the Add agent path). Binds run
//! one after another so two binds of one Agent never race in the journal.
use super::{AgentPick, RoomFacts, default_agent, should_auto_join};
use crate::hagency::{backend::Service, model::Command};
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use crate::utils::RoomNameId;
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, UserId, events::room::member::MembershipState};

/// How long a bind outcome stays on screen.
const RESULT_POPUP_SECONDS: f64 = 8.0;

/// One bind finished; the UI thread shows its outcome.
#[derive(Debug)]
pub(super) struct BindDone {
    pub epoch: u64,
    pub agent: String,
    pub room: String,
    pub result: Result<bool, String>,
}

/// Binds the Agent `puppet` into each `(room id, name)` in order, reporting each outcome.
pub fn bind_into_rooms(agent: String, puppet: String, rooms: Vec<(String, String)>) {
    if rooms.is_empty() { return; }
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        let service = Service::for_current();
        for (room_id, room) in rooms {
            let result = match &service {
                Ok(service) => service.invite_agent_to_room(&room_id, &puppet).await,
                Err(code) => Err(code.clone()),
            };
            Cx::post_action(BindDone { epoch, agent: agent.clone(), room, result });
        }
    });
}

/// Every bind outcome of the current account becomes a popup.
pub fn observe(actions: &Actions) {
    for action in actions {
        if let Some(done) = action.downcast_ref::<BindDone>() && done.epoch == crate::account_session::epoch() {
            let kind = match done.result { Ok(true) => PopupKind::Success, Ok(false) => PopupKind::Info, Err(_) => PopupKind::Error };
            enqueue_popup_notification(super::bind_result_text(&done.agent, &done.room, &done.result), kind, Some(RESULT_POPUP_SECONDS));
        }
    }
}

/// After a room the owner created (New room) exists: add the default Agent.
pub fn owner_created_room(room_id: OwnedRoomId) {
    crate::account_session::spawn(async move {
        match prepare(&room_id).await {
            Ok(Some((agent, room, facts))) if should_auto_join(facts) => {
                bind_into_rooms(agent.name, agent.puppet, vec![(room_id.to_string(), room)]);
            }
            Ok(_) => {}
            // Optional convenience: the room was created either way and Add
            // agent still works there, so a failed lookup only skips it.
            Err(code) => warning!("Bring agent: no automatic add to new room {room_id}: {code}"),
        }
    });
}

/// The default Agent, the room's name and what the room is, or None when the
/// owner has no Agent yet.
async fn prepare(room_id: &OwnedRoomId) -> Result<Option<(AgentPick, String, RoomFacts)>, String> {
    let client = crate::sliding_sync::get_client().ok_or("matrix_sign_in_required")?;
    let room = client.get_room(room_id).ok_or("room_join_required")?;
    let service = Service::for_current()?;
    let Some(agent) = default_agent(&service.execute(Command::Agents).await?) else { return Ok(None) };
    let facts = room_facts(&room, &agent).await;
    Ok(Some((agent, RoomNameId::from_room(&room).await.to_string(), facts)))
}

async fn room_facts(room: &matrix_sdk::Room, agent: &AgentPick) -> RoomFacts {
    let private_chat = agent.direct_room.as_deref() == Some(room.room_id().as_str());
    let member = match UserId::parse(agent.puppet.as_str()) {
        Ok(puppet) => room.get_member_no_sync(&puppet).await.ok().flatten()
            .is_some_and(|m| matches!(m.membership(), MembershipState::Join | MembershipState::Invite)),
        Err(_) => false,
    };
    // An unreadable DM flag counts as a DM: never add an Agent into a private chat by mistake.
    let direct = room.is_direct().await.unwrap_or(true);
    RoomFacts { space: room.is_space(), direct, agent_present: private_chat || member }
}
