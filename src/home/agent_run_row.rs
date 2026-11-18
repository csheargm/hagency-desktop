//! The Agent's answer to a request, drawn as the Agent's own message on the
//! left (its avatar and name), under the requester's bubble. Before this the
//! run card sat inside the requester's bubble, so the answer read as if the
//! asker had written it. Its name, "agent" tag and clickable avatar match
//! the Agent's own timeline messages (Nov 18: the row had a bare name, the
//! raw `_hagency_agt_…` ID at first, and an avatar a click passed through).
use makepad_widgets::*;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId};
use crate::hagency::run_controls::Entry;
use crate::shared::avatar::AvatarWidgetRefExt;
use super::agent_run_card::AgentRunCardWidgetRefExt;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.widgets.AgentRunRow = View {
        visible: false
        width: Fill height: Fit flow: Right spacing: 10
        padding: Inset{top: 0 bottom: 10 left: (1.5 * mod.widgets.RINX_GUTTER) right: (1.5 * mod.widgets.RINX_GUTTER)}
        agent_profile := View {
            width: 49.0 height: Fit flow: Down align: Align{x: 0.5 y: 0.0} margin: Inset{top: 4.5}
            agent_avatar := Avatar {width: 32 height: 32}
        }
        agent_column := View {
            width: Fill height: Fit flow: Down
            agent_name_row := View {
                width: Fill height: Fit flow: Right spacing: 5 align: Align{y: 0.5} padding: Inset{top: 8 bottom: 4}
                agent_name := Label {
                    width: Fit height: Fit
                    draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular {font_size: 11}}
                }
                agent_badge := mod.widgets.AgentBadge {margin: 0}
            }
            agent_run_card := mod.widgets.AgentRunCard {width: Fill{max: mod.widgets.RINX_READING_WIDTH}}
            agent_run_card_1 := mod.widgets.AgentRunCard {width: Fill{max: mod.widgets.RINX_READING_WIDTH}}
            agent_run_card_2 := mod.widgets.AgentRunCard {width: Fill{max: mod.widgets.RINX_READING_WIDTH}}
        }
    }
}

/// The card slots inside the row, in page order.
pub fn card_paths() -> [&'static [LiveId]; 3] {
    [
        ids!(agent_runs.agent_run_card).as_slice(),
        ids!(agent_runs.agent_run_card_1).as_slice(),
        ids!(agent_runs.agent_run_card_2).as_slice(),
    ]
}

/// The name shown over the answer: one Agent's name, or every distinct one.
pub fn row_name(entries: &[Entry]) -> String {
    names(entries, |entry| if entry.name.is_empty() { entry.agent.clone() } else { entry.name.clone() })
}

/// Every distinct `name_of` the entries, in order, joined.
fn names(entries: &[Entry], mut name_of: impl FnMut(&Entry) -> String) -> String {
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let name = name_of(entry);
        if !names.contains(&name) { names.push(name); }
    }
    names.join(" · ")
}

/// An answering Agent's name as the room knows it (see `agent_run_requester::agent_name`).
fn entry_name(cx: &mut Cx, entry: &Entry, room: &OwnedRoomId) -> String {
    match OwnedUserId::try_from(entry.agent.as_str()) {
        Ok(agent) => super::agent_run_requester::agent_name(cx, &agent, room, Some(&entry.name)),
        Err(_) => entry.name.clone(),
    }
}

/// The row's name and "agent" tag, and an avatar that opens the Agent's
/// profile (or, for one's own Agent, its panel) like its timeline avatar.
/// `profile_name` is the name of the Agent the avatar opens (the first one
/// when the row names several).
fn show_agent(cx: &mut Cx, item: &WidgetRef, name: &str, profile_name: String, agent: Option<OwnedUserId>, room: &OwnedRoomId) {
    item.label(cx, ids!(agent_runs.agent_name)).set_text(cx, name);
    let badge = item.widget(cx, ids!(agent_runs.agent_badge));
    badge.set_visible(cx, true);
    let label = item.label(cx, ids!(agent_runs.agent_badge.agent_badge_label));
    label.set_text(cx, crate::i18n::tr("agent"));
    #[cfg(feature = "agent_chat")]
    {
        // A plain Agent's tag in the timeline uses the neutral pair.
        let (mut badge, mut label) = (badge, label);
        script_apply_eval!(cx, badge, {draw_bg +: {color: (mod.widgets.RBX_NEUTRAL_BG)}});
        script_apply_eval!(cx, label, {draw_text +: {color: (mod.widgets.RBX_NEUTRAL_FG)}});
    }
    let color = crate::shared::avatar::avatar_color(agent.as_ref().map_or(name, |a| a.as_str()));
    let info = agent.map(|user_id| crate::shared::avatar::AvatarTextInfo { user_id, username: Some(profile_name), room_id: room.clone() });
    item.avatar(cx, ids!(agent_runs.agent_avatar)).show_text(cx, Some(color), info, name);
}

/// Puts the progress thread's reply count on the answer, as "N updates".
pub fn set_updates(cx: &mut Cx, item: &WidgetRef, count: usize) {
    for path in card_paths() {
        item.agent_run_card(cx, path).set_updates(cx, count);
    }
}

/// Shows this request's runs as the Agent's message, or hides the row. With
/// no run yet, a request the viewer sent (`pending`) shows the Agent's row
/// with its waiting line (`agent_run_requester`).
pub fn populate(cx: &mut Cx, item: &WidgetRef, room: &OwnedRoomId, entries: &[Entry], pending: Option<super::agent_run_requester::PendingRequest>) {
    let pending = pending.filter(|_| entries.is_empty());
    let row = item.view(cx, ids!(agent_runs));
    row.set_visible(cx, !entries.is_empty() || pending.is_some());
    for (index, (path, shown)) in card_paths().into_iter().zip(super::agent_run_pager::slot_entries(entries)).enumerate() {
        let card = item.agent_run_card(cx, path);
        card.set_pending(cx, if index == 0 { pending.clone() } else { None });
        card.set_entries(cx, shown);
    }
    let (name, first_name, agent) = match (entries.first(), &pending) {
        (Some(first), _) => (names(entries, |e| entry_name(cx, e, room)), entry_name(cx, first, room), OwnedUserId::try_from(first.agent.as_str()).ok()),
        (None, Some(p)) => (p.name.clone(), p.name.clone(), Some(p.agent.clone())),
        (None, None) => return,
    };
    show_agent(cx, item, &name, first_name, agent, room);
}

#[cfg(test)]
#[path = "agent_run_row_tests.rs"]
mod tests;
