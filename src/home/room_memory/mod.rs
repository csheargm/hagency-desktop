//! Room memory in the human client (ADR 0013): the Memory list in the room
//! information pane and the proposal cards in the timeline. Storage and rules
//! are the backend's; this module only reads and writes through the signed-in
//! person's own Matrix client, so the homeserver (or, in an encrypted room,
//! the readers) enforce who may change memory.
pub mod card;
pub(crate) mod io;
pub mod model;
pub(crate) mod ops;
pub(crate) mod server_state;
pub mod people;
pub mod panel;
pub(crate) mod store;
#[cfg(test)]
mod hardening_tests;
#[cfg(test)]
mod tests;

use makepad_widgets::ScriptVm;

pub fn script_mod(vm: &mut ScriptVm) {
    panel::script_mod(vm);
    card::script_mod(vm);
}

/// The memory proposal an Agent's message carries, if it parses and the
/// event's actual sender is an Agent of this room's homeserver.
pub fn proposal_of(event: &matrix_sdk_ui::timeline::EventTimelineItem, room: &matrix_sdk::ruma::RoomId) -> Option<model::Proposal> {
    let raw = event.latest_json()?;
    let value: serde_json::Value = serde_json::from_str(raw.json().get()).ok()?;
    model::trusted_proposal(&value["content"], event.sender().as_str(), room.as_str())
}

/// The line that replaces a proposal message's body above its card. `sender`
/// is the Agent's display name from the event, never the payload's.
pub fn proposal_intro(sender: &str) -> String {
    crate::i18n::format("{agent} suggests a note for this room's memory:", &[("agent", sender.to_owned())])
}

/// The intro line for a proposal message (text or notice), when it carries a proposal:
/// the Agent's display name, or "Agent" while the profile is unknown (never a raw id).
pub fn proposal_intro_of(event: &matrix_sdk_ui::timeline::EventTimelineItem, room: &matrix_sdk::ruma::RoomId) -> Option<String> {
    proposal_of(event, room)?;
    Some(proposal_intro(&sender_name(event)))
}
/// The sender as cards name them: display name, or "Agent" for an unnamed Agent.
pub fn sender_name(event: &matrix_sdk_ui::timeline::EventTimelineItem) -> String {
    let name = match event.sender_profile() {
        matrix_sdk_ui::timeline::TimelineDetails::Ready(profile) => profile.display_name.clone(),
        _ => None,
    };
    intro_name(name, event.sender().localpart())
}
fn intro_name(display_name: Option<String>, localpart: &str) -> String {
    display_name.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| {
        if localpart.contains("agt_") { crate::i18n::tr("Agent").to_owned() } else { localpart.to_owned() }
    })
}
#[cfg(test)]
mod intro_tests {
    #[test]
    fn intro_never_shows_a_raw_agent_id() {
        assert_eq!(super::intro_name(None, "_hagency_agt_01m7dqpq"), crate::i18n::tr("Agent"));
        assert_eq!(super::intro_name(Some("demoup-claude".into()), "_hagency_agt_01"), "demoup-claude");
        assert_eq!(super::intro_name(None, "alice"), "alice");
    }
}
