//! The lines an Agent's hover card and profile show under its name, from
//! what [`super::agent_facts`] knows: whose Agent it is (name and Matrix ID),
//! its runtime, its status, and who may see its settings.

use makepad_widgets::Cx;
use matrix_sdk::ruma::{OwnedRoomId, UserId};
use super::agent_facts::{AgentFacts, OwnerBasis};

/// The owner's name as `room` (or their profile) shows it, else their Matrix ID.
pub fn owner_name(cx: &mut Cx, owner: &UserId, room: Option<&OwnedRoomId>) -> String {
    super::user_profile_cache::get_user_display_name_for_room(cx, owner.to_owned(), room, true)
        .into_option().filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| owner.to_string())
}

/// "Betty (@bird:beta)": the owner's shown name and, when it is not
/// already their Matrix ID, that ID, so two people named alike stay apart.
pub fn owner_label(name: &str, owner: Option<&UserId>) -> String {
    match owner {
        Some(id) if name != id.as_str() => format!("{name} ({id})"),
        _ => name.to_owned(),
    }
}

/// The owner line: "Agent of …" on the profile, "Owner: …" on the card; an
/// owner only the Agent states is worded as its statement, never as fact.
fn owner_line(facts: &AgentFacts, owner: Option<&str>, profile: bool) -> String {
    use crate::i18n::{format, tr};
    let labelled = owner.map(|name| owner_label(name, facts.owner.as_deref()));
    let key = match (facts.owner_basis, profile) {
        (OwnerBasis::Conflicting, _) => return tr("Owner unknown: the Agent names different owners").to_owned(),
        (OwnerBasis::Listed, true) => "Agent of {owner}",
        (OwnerBasis::Listed, false) => "Owner: {owner}",
        (OwnerBasis::StatedByAgent, true) => "Agent of {owner}, as stated by the Agent",
        (OwnerBasis::StatedByAgent, false) => "Owner (as stated by the Agent): {owner}",
    };
    match labelled {
        Some(label) => format(key, &[("owner", label)]),
        None => tr("Owner not shared in this room").to_owned(),
    }
}

/// The fact lines under an Agent's name. `owner` is the owner's shown name.
/// Another owner's Agent ends with who may see its settings, named only when
/// its owner is established.
pub fn fact_lines(facts: &AgentFacts, owner: Option<&str>, profile: bool) -> Vec<String> {
    use crate::i18n::{format, tr};
    let mut lines = vec![owner_line(facts, owner, profile)];
    if let Some(runtime) = facts.runtime {
        lines.push(format("Runtime: {runtime}", &[("runtime", runtime.to_owned())]));
    }
    let status = facts.status.map(|key| tr(key).to_owned()).or_else(|| facts.activity.clone());
    if let Some(status) = status {
        lines.push(format("Status: {status}", &[("status", status)]));
    }
    if let Some(share) = facts.shared {
        lines.push(crate::hagency::room_share::summary(share, tr("this room"), owner.unwrap_or(tr("its owner"))));
    }
    if facts.own_agent_id.is_none() {
        lines.push(match owner.filter(|_| facts.owner_basis == OwnerBasis::Listed) {
            Some(name) => format("Settings are visible only to {owner}", &[("owner", name.to_owned())]),
            None => tr("Settings are visible only to its owner").to_owned(),
        });
    }
    lines
}
