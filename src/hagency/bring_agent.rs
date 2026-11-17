//! "Bring your agent along": the owner's Agent follows them into rooms.
//!
//! Three entry points share one bind path (`Service::invite_agent_to_room`,
//! the same one the room's Add agent picker and `/invite` use):
//! - the Create agent dialog's "Add to rooms" checklist (`create_agent`);
//! - rooms the owner creates with New room: every active Agent with its
//!   "Join rooms I join" setting on joins at once;
//! - an Agent not created with the Create agent dialog's checklist (older
//!   Agents, ones made elsewhere) joins the group rooms its owner created, once;
//! - group rooms the owner joins (accepted invite or join): every active
//!   Agent with the setting on joins; one with it off gets a one-click prompt.
//!
//! A private Agent's bind is refused by the server; any of these paths then
//! asks "Allow in group rooms?" once for that room (`consent`), never silently.
//!
//! This file holds the pure rules; `prefs` keeps the per-account choices on
//! disk, `flow` runs the async steps and `prompt` is the one-click prompt.
mod consent;
mod flow;
mod prefs;
mod prompt;
mod retry;
pub use flow::{bind_into_rooms, bind_into_rooms_then, catch_up_agents, observe, owner_created_room};
pub use prompt::{BringAgentAction, route as route_prompt, script_mod};

use serde_json::Value;

/// How many rooms the Create agent dialog lists; the open room always fits.
pub(crate) const MAX_ROOM_CHOICES: usize = 8;

/// The Agent that follows its owner, as the Agents listing names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentPick {
    pub id: String,
    pub name: String,
    pub puppet: String,
    /// The Agent's private chat with its owner, never a room to bring it into.
    pub direct_room: Option<String>,
}

/// Active Agents that have a Matrix identity, most recently created first.
/// Agent ids are ULIDs (`agt_01…`), which sort by creation time; a
/// `createdAtMs` field, when the listing carries one, is compared first.
pub(crate) fn active_agents_newest_first(listing: &Value) -> Vec<&Value> {
    let mut rows: Vec<&Value> = listing["agents"].as_array().map(|rows| rows.iter().filter(|row| {
        row["state"] == "active" && row["id"].as_str().is_some_and(|id| !id.is_empty())
            && row["puppetMxid"].as_str().is_some_and(|p| !p.is_empty())
    }).collect()).unwrap_or_default();
    rows.sort_by_cached_key(|row| std::cmp::Reverse((row["createdAtMs"].as_u64(), row["id"].as_str().unwrap_or_default().to_ascii_lowercase())));
    rows
}

/// Every listed Agent id, in any state: the upgrade seed must cover Agents
/// that are suspended or still creating, not only the active ones.
pub(crate) fn listed_agent_ids(listing: &Value) -> Vec<String> {
    listing["agents"].as_array().map(|rows| rows.iter().filter_map(|row| row["id"].as_str())
        .filter(|id| !id.is_empty()).map(str::to_owned).collect()).unwrap_or_default()
}

/// Every active Agent with an identity, newest first.
pub(crate) fn active_agents(listing: &Value) -> Vec<AgentPick> {
    active_agents_newest_first(listing).into_iter().map(agent_pick).collect()
}

fn agent_pick(row: &Value) -> AgentPick {
    let text = |key: &str| row[key].as_str().unwrap_or_default().to_owned();
    let name = row["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent").to_owned();
    AgentPick { id: text("id"), name, puppet: text("puppetMxid"), direct_room: row["ownerDirectRoomId"].as_str().map(str::to_owned) }
}

/// What the desktop knows about a room the owner just created or joined.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RoomFacts {
    pub space: bool,
    pub direct: bool,
    /// The Agent is already joined or invited, or this is its private chat.
    pub agent_present: bool,
    /// The owner may invite members here (room power levels).
    pub can_invite: bool,
}

/// Whether a room the owner just created gets the default Agent at once.
pub(crate) fn should_auto_join(joins_rooms: bool, room: RoomFacts) -> bool {
    joins_rooms && room.can_invite && !room.space && !room.direct && !room.agent_present
}

/// Whether an Agent catching up joins this already-existing room: only rooms
/// the owner created, once per pair, with the same limits as a new room.
pub(crate) fn should_catch_up(joins_rooms: bool, handled: bool, owner_created: bool, room: RoomFacts) -> bool {
    owner_created && !handled && should_auto_join(joins_rooms, room)
}

/// What happens to one Agent after its owner joins a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Follow {
    /// "Join rooms I join" is on: add it now.
    Join,
    /// The setting is off: ask "Bring <agent> into <room>?".
    Offer,
    /// The owner may not invite here: say so instead of trying.
    NoInviteRights,
    /// A DM, a space, already there, or this pair was handled before.
    Skip,
}

/// The rule for one Agent after a join; `handled` is a pair already added or asked.
pub(crate) fn follow_on_join(joins_rooms: bool, handled: bool, room: RoomFacts) -> Follow {
    if handled || room.space || room.direct || room.agent_present { return Follow::Skip; }
    match (room.can_invite, joins_rooms) {
        (false, _) => Follow::NoInviteRights,
        (true, true) => Follow::Join,
        (true, false) => Follow::Offer,
    }
}

/// Whether this Agent follows its owner into rooms (default on).
pub(crate) fn joins_rooms(agent_id: &str) -> bool {
    prefs::load_current().joins_rooms(agent_id)
}

/// Marks an Agent as caught up without joining anything (its rooms were chosen
/// explicitly). A failed save is logged: the next catch-up may then add it.
pub(crate) fn skip_catch_up(agent_id: &str) {
    if let Err(code) = prefs::update_current(|prefs| prefs.claim_catch_up(agent_id)) {
        makepad_widgets::warning!("Bring agent: could not record {agent_id}'s room choice: {code}");
    }
}

/// The Agent panel's quiet notice: earlier rooms the catch-up gave up on.
pub(crate) fn catch_up_notice(agent_id: &str) -> Option<String> {
    let given_up = prefs::load_current().catch_up_given_up(agent_id);
    (given_up > 0).then(|| crate::i18n::format("Could not join {count} of your earlier rooms. Use Add agent in each room.", &[("count", given_up.to_string())]))
}

/// Saves this Agent's "Join rooms I join" for the signed-in account.
pub(crate) fn save_joins_rooms(agent_id: &str, on: bool) -> Result<(), String> {
    prefs::update_current(|prefs| { prefs.set_joins_rooms(agent_id, on); Ok(()) })
}

/// One row of the Create agent dialog's "Add to rooms" checklist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomChoice {
    pub room_id: String,
    pub name: String,
    pub ticked: bool,
}

/// The checklist: the open room first and ticked, then the other rooms by
/// name, at most `limit` rows. `rooms` are joined rooms that are neither DMs
/// nor spaces, as `(room id, display name)`.
pub(crate) fn room_choices(mut rooms: Vec<(String, String)>, current: Option<&str>, limit: usize) -> Vec<RoomChoice> {
    rooms.sort_by_cached_key(|(id, name)| (Some(id.as_str()) != current, name.to_lowercase(), id.clone()));
    rooms.dedup_by(|a, b| a.0 == b.0);
    rooms.into_iter().take(limit).map(|(room_id, name)| {
        let ticked = Some(room_id.as_str()) == current;
        RoomChoice { room_id, name, ticked }
    }).collect()
}

/// The ticked rooms, in checklist order.
pub(crate) fn ticked_rooms(choices: &[RoomChoice]) -> Vec<(String, String)> {
    choices.iter().filter(|c| c.ticked).map(|c| (c.room_id.clone(), c.name.clone())).collect()
}

/// The prompt line, e.g. "Bring ann-claude into fix-it?".
pub(crate) fn offer_text(agent: &str, room: &str) -> String {
    crate::i18n::format("Bring {agent} into {room}?", &[("agent", agent.to_owned()), ("room", room.to_owned())])
}

/// Why an Agent did not follow into a room where the owner cannot invite.
pub(crate) fn no_invite_rights_text(agent: &str, room: &str) -> String {
    crate::i18n::format("{agent} was not added to {room}: you do not have permission to invite members there.", &[("agent", agent.to_owned()), ("room", room.to_owned())])
}

/// The popup after one bind: joined, still joining, or why not.
pub(crate) fn bind_result_text(agent: &str, room: &str, result: &Result<bool, String>) -> String {
    let args = [("agent", agent.to_owned()), ("room", room.to_owned())];
    match result {
        Ok(true) => crate::i18n::format("{agent} joined {room}.", &args),
        Ok(false) => crate::i18n::format("{agent} is still joining {room}.", &args),
        Err(code) => {
            let reason = crate::i18n::tr(crate::home::mobile_chat_info::agent_binding_error(code)).to_owned();
            crate::i18n::format("Could not add {agent} to {room}: {reason}", &[args[0].clone(), args[1].clone(), ("reason", reason)])
        }
    }
}

#[cfg(test)]
mod tests;
