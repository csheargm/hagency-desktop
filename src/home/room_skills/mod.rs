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

/// A proposal's fallback body that names the Agent by its raw id
/// ("agt_01m7k… proposes a new team skill", "Memory proposal from agt_…:"),
/// with the sender's display name in its place. Cards posted before the
/// runtime named the Agent, or not drawn as a card, still read well.
pub fn named_fallback(body: &str, sender_name: &str) -> Option<String> {
    const MEMORY_FROM: &str = "Memory proposal from ";
    let (lead, rest) = body.strip_prefix(MEMORY_FROM).map_or(("", body), |rest| (MEMORY_FROM, rest));
    let id_len = rest.strip_prefix("agt_")?.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))? + 4;
    let tail = &rest[id_len..];
    let known = [" proposes ", ::hagency::room_memory::PROPOSAL_BODY_MARK].iter().any(|mark| tail.starts_with(mark))
        || (!lead.is_empty() && tail.starts_with(':'));
    known.then(|| format!("{lead}{sender_name}{tail}"))
}
