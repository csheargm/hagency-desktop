//! Team skills in the human client: the Skills list in the room information
//! pane and the skill proposal cards in the timeline. Rules and storage are the
//! backend's (`room_skills`); writes go through the signed-in person's own
//! Matrix client, exactly like room memory, so the homeserver (or, in an
//! encrypted room, every reader) enforces who may change a skill.
pub mod card;
pub mod io;
pub mod model;
pub mod ops;
pub mod panel;
#[cfg(test)]
mod tests;

use makepad_widgets::ScriptVm;

pub fn script_mod(vm: &mut ScriptVm) {
    panel::script_mod(vm);
    card::script_mod(vm);
}

/// The skill proposal an Agent's message carries, if it parses and the event's
/// real sender is an Agent of this room's homeserver.
pub fn proposal_of(event: &matrix_sdk_ui::timeline::EventTimelineItem, room: &matrix_sdk::ruma::RoomId) -> Option<model::Proposal> {
    if !crate::home::room_memory::authentic::event_authentic(event, room) {
        return None;
    }
    let raw = event.latest_json()?;
    let value: serde_json::Value = serde_json::from_str(raw.json().get()).ok()?;
    model::trusted_proposal(&value["content"], event.sender().as_str(), room.as_str())
}

/// The line that replaces a proposal message's body above its card.
pub fn proposal_intro(sender: &str) -> String {
    crate::i18n::format("{agent} suggests a team skill for this room:", &[("agent", sender.to_owned())])
}
