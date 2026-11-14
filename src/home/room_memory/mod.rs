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
mod tests;

use makepad_widgets::ScriptVm;

pub fn script_mod(vm: &mut ScriptVm) {
    panel::script_mod(vm);
    card::script_mod(vm);
}

/// The memory proposal an Agent's message carries, if it parses.
pub fn proposal_of(event: &matrix_sdk_ui::timeline::EventTimelineItem) -> Option<model::Proposal> {
    let raw = event.latest_json()?;
    let value: serde_json::Value = serde_json::from_str(raw.json().get()).ok()?;
    model::parse_proposal(&value["content"])
}

/// The line that replaces a proposal message's body above its card.
pub fn proposal_intro(proposal: &model::Proposal) -> String {
    crate::i18n::format("{agent} suggests a note for this room's memory:", &[("agent", proposal.entry.author.clone())])
}
