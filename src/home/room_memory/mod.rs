//! Room memory in the human client (ADR 0013): the Memory list in the room
//! information pane and the proposal cards in the timeline. Storage and rules
//! are the backend's; this module only reads and writes through the signed-in
//! person's own Matrix client, so the homeserver (or, in an encrypted room,
//! the readers) enforce who may change memory.
pub mod card;
mod io;
pub mod model;
mod ops;
pub mod panel;
mod store;
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
