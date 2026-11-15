//! "Bring your agent along": the owner's Agent follows them into rooms.
//!
//! Two entry points share one bind path (`Service::invite_agent_to_room`,
//! the same one the room's Add agent picker and `/invite` use):
//! - the Create agent dialog's "Add to rooms" checklist (`create_agent`);
//! - rooms the owner creates with New room: the newest Agent joins at once.
//!
//! This file holds the pure rules; `flow` runs the async steps.
mod flow;
pub use flow::{bind_into_rooms, observe, owner_created_room};

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

/// The owner's default Agent (`Command::Agents` listing): the newest active one.
pub(crate) fn default_agent(listing: &Value) -> Option<AgentPick> {
    let row = active_agents_newest_first(listing).into_iter().next()?;
    let text = |key: &str| row[key].as_str().unwrap_or_default().to_owned();
    let name = row["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent").to_owned();
    Some(AgentPick { id: text("id"), name, puppet: text("puppetMxid"), direct_room: row["ownerDirectRoomId"].as_str().map(str::to_owned) })
}

/// What the desktop knows about a room the owner just created.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RoomFacts {
    pub space: bool,
    pub direct: bool,
    /// The Agent is already joined or invited, or this is its private chat.
    pub agent_present: bool,
}

/// Whether a room the owner just created gets the default Agent at once.
pub(crate) fn should_auto_join(room: RoomFacts) -> bool {
    !room.space && !room.direct && !room.agent_present
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
