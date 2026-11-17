//! A private Agent's bind into a group room is refused by the server
//! (`private_agent_owner_direct_only`). Every follow path (New room, joined
//! room, catch-up, Create agent's checklist, composer hint) then offers
//! "Allow in group rooms?" for that one room instead of failing quietly. Only
//! the owner's click records the choice (owner `AllowGroupRooms`), then the
//! same Add agent bind runs again. Nothing here publishes an Agent by itself.
use super::{AgentPick, flow::BindDone, prompt::BringAgentAction};
use crate::hagency::{backend::Service, group_rooms::PRIVATE_AGENT_REFUSAL, model::{AgentAction, Command}};
use makepad_widgets::*;
use matrix_sdk::ruma::OwnedRoomId;

/// Whether this bind outcome is the private Agent's refusal.
pub(crate) fn needs_consent(result: &Result<bool, String>) -> bool {
    matches!(result, Err(code) if code == PRIVATE_AGENT_REFUSAL)
}

/// The prompt line for a private Agent.
pub(crate) fn consent_text(agent: &str, room: &str) -> String {
    crate::i18n::format("{agent} is private: it only answers in your private chat with it. Allow it in group rooms and add it to {room}? Members of rooms you add it to can then @ it. This does not contribute its resources to a pool.", &[("agent", agent.to_owned()), ("room", room.to_owned())])
}

/// A refused private bind becomes the consent prompt; true when it did.
pub(super) fn offer_if_private(done: &BindDone) -> bool {
    if !needs_consent(&done.result) { return false; }
    let (room_id, puppet) = &done.target;
    let Ok(room_id) = OwnedRoomId::try_from(room_id.as_str()) else { return false };
    let agent = AgentPick { id: String::new(), name: done.agent.clone(), puppet: puppet.clone(), direct_room: None };
    Cx::post_action(BringAgentAction::Consent { room_id, room_name: done.room.clone(), agent });
    true
}

/// The Agents listing's id for this puppet.
pub(crate) fn agent_id_for(listing: &serde_json::Value, puppet: &str) -> Option<String> {
    listing["agents"].as_array()?.iter().find(|a| a["puppetMxid"] == puppet)?["id"].as_str().map(str::to_owned)
}

/// Records the owner's "Allow in group rooms" for `puppet`, then binds it into `room_id`.
async fn allow_and_bind(puppet: &str, room_id: &str) -> Result<bool, String> {
    let service = Service::for_current()?;
    let id = agent_id_for(&service.execute(Command::Agents).await?, puppet).ok_or("agent_owner_binding_required")?;
    service.execute(Command::Agent { agent_id: id, action: AgentAction::AllowGroupRooms }).await?;
    service.invite_agent_to_room(room_id, puppet).await
}

/// The owner clicked "Allow in group rooms": record it, then add the Agent.
pub(super) fn allow_then_bind(agent: AgentPick, room_id: String, room: String) {
    let epoch = crate::account_session::epoch();
    crate::account_session::spawn(async move {
        let result = allow_and_bind(&agent.puppet, &room_id).await;
        Cx::post_action(BindDone { epoch, agent: agent.name, room, result, target: (room_id, agent.puppet) });
    });
}

/// As [`allow_then_bind`] for a picker that shows the outcome itself: the
/// result goes to `done` (off the UI thread) instead of a popup.
pub(crate) fn allow_then(puppet: String, room_id: String, done: impl FnOnce(Result<bool, String>) + Send + 'static) {
    crate::account_session::spawn(async move { done(allow_and_bind(&puppet, &room_id).await) });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_the_private_refusal_asks_for_consent() {
        assert!(needs_consent(&Err(PRIVATE_AGENT_REFUSAL.into())));
        assert!(!needs_consent(&Err("server_unavailable".into())));
        assert!(!needs_consent(&Ok(true)));
    }
    #[test]
    fn a_private_refusal_settles_the_pair_so_it_is_offered_once() {
        assert!(super::super::retry::bind_settles(&Err(PRIVATE_AGENT_REFUSAL.into())));
    }
    #[test]
    fn the_consent_names_agent_room_and_pool() {
        let text = consent_text("betty-claude", "fix-it");
        assert!(text.contains("betty-claude") && text.contains("fix-it") && text.contains("pool") && text.contains("private"), "{text}");
    }
    #[test]
    fn the_allowed_agent_is_found_by_its_puppet() {
        let listing = serde_json::json!({"agents":[{"id":"agt_a","puppetMxid":"@a:x"},{"id":"agt_b","puppetMxid":"@b:x"}]});
        assert_eq!(agent_id_for(&listing, "@b:x").as_deref(), Some("agt_b"));
        assert_eq!(agent_id_for(&listing, "@c:x"), None);
    }
}
