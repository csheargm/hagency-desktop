//! One-time move of an Agent's old watch settings to the room work loop
//! (ADR 0030). The single-repository issue watch and the owner's work list
//! become: the repository linked to the watch's room (when missing), "may
//! work in this room" on for the Agent, and — only where the room has no
//! level yet and this account may set it — the room's level: Autonomous when
//! the Agent had "push freely" and "Pull request merges" on, else Assist.
//! Trial rows are ignored. Two phases (GPT review): every room's repositories
//! are linked first; only when all are linked is "may work" turned on, the
//! old watch switched off, the Agent recorded as moved on this device and
//! each moved room told in one line. A room that refuses (no right to link)
//! is reported and the whole move retried next check, so a watch is never
//! dropped silently. Idempotent: a moved Agent's work list never moves again
//! (an owner who later unticks every room is not overruled).
use super::room_loop::{Rooms, may_work_rooms};
use std::collections::BTreeSet;
use crate::hagency::backend::Service;
use ::hagency::issue_watch::safeguards::room_autonomy::Level;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// One room's share of the move: its repositories and the level it would get.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Move {
    pub repos: Vec<String>,
    pub level: Level,
}

/// What the old settings ask for, per room. `auto_merge` is the retired
/// per-Agent merge switch; `moved`: this device already moved the Agent's
/// work list. Empty when nothing is left to move.
pub(crate) fn planned(saved: &Value, auto_merge: bool, moved: bool) -> BTreeMap<String, Move> {
    let mut plan: BTreeMap<String, Move> = BTreeMap::new();
    let watch = &saved["issueWatch"];
    let mut add = |room: &str, repo: &str, push: bool| {
        if room.is_empty() || repo.is_empty() {
            return;
        }
        let entry = plan.entry(room.to_owned()).or_default();
        if !entry.repos.iter().any(|r| r.eq_ignore_ascii_case(repo)) {
            entry.repos.push(repo.to_owned());
        }
        if push && auto_merge {
            entry.level = Level::Autonomous;
        } else if entry.level == Level::Off {
            entry.level = Level::Assist;
        }
    };
    if watch["mode"].as_str().is_some_and(|m| m != "off") {
        add(watch["roomId"].as_str().unwrap_or_default(), watch["repo"].as_str().unwrap_or_default(), watch["pushTrusted"] == true);
    }
    // The work list moves once: never after this device moved it, nor once may-work rooms exist.
    if !moved && may_work_rooms(saved).is_empty() {
        for entry in saved["work"].as_array().into_iter().flatten() {
            add(entry["roomId"].as_str().unwrap_or_default(), entry["repo"].as_str().unwrap_or_default(), entry["pushFreely"] == true);
        }
    }
    plan
}

/// The room line for a moved room.
pub(crate) fn moved_line(agent: &str, repos: &[String], level: Option<Level>) -> String {
    let level = level.map_or_else(
        || "the room's owner or a maintainer still needs to choose Assist or Autonomous in Room info → Status".to_owned(),
        |l| format!("agent work here is {}", crate::hagency::room_autonomy::lines::level_name(l)),
    );
    format!("{agent}'s issue watching moved to this room's settings: it may work on {} here; {level}.", repos.join(", "))
}

/// The file in the app data folder naming the Agents whose work list moved.
const MOVED_FILE: &str = "room-work-moved.json";

fn moved_path() -> std::path::PathBuf {
    crate::app_data_dir().join(MOVED_FILE)
}

/// Whether this device already moved `agent_id`'s work list.
pub(crate) fn moved(agent_id: &str) -> bool {
    // Unreadable or absent: not moved yet; a move is idempotent (links and may-work only add).
    let agents: BTreeSet<String> = std::fs::read(moved_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    agents.contains(agent_id)
}

fn record_moved(agent_id: &str) -> Result<(), String> {
    let mut agents: BTreeSet<String> = std::fs::read(moved_path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    agents.insert(agent_id.to_owned());
    let bytes = serde_json::to_vec(&agents).map_err(|e| e.to_string())?;
    std::fs::write(moved_path(), bytes).map_err(|e| crate::i18n::format("Could not record the move: {error}", &[("error", e.to_string())]))
}

/// Phase one for one room: link its repositories (this account must be allowed).
async fn link_room(room_id: &str, plan: &Move, rooms: &Rooms) -> Result<(), String> {
    let client = crate::sliding_sync::get_client().ok_or_else(|| crate::i18n::tr("Signed out").to_owned())?;
    let id = matrix_sdk::ruma::RoomId::parse(room_id).map_err(|e| e.to_string())?;
    let state = rooms.0.get(room_id).cloned().unwrap_or_else(|| Err(crate::i18n::tr("the room was not read").to_owned()))?;
    for repo in plan.repos.iter().filter(|r| !state.linked.iter().any(|l| l.eq_ignore_ascii_case(r))) {
        if !state.can_link {
            return Err(crate::i18n::format("you cannot link {repo} in this room; ask its owner or a maintainer to link it", &[("repo", repo.clone())]));
        }
        crate::home::room_watches::status::set_link(&client, &id, repo, true).await?;
    }
    Ok(())
}

/// Phase two for one room: may-work on, and the level when unset and allowed.
/// The level it ended at (`None`: left for a maintainer).
async fn enable_room(service: &Service, agent_id: &str, room_id: &str, plan: &Move, rooms: &Rooms) -> Result<Option<Level>, String> {
    super::live::saved(service, agent_id, Some(json!({"mayWork": {"roomId": room_id, "on": true}}))).await?;
    let state = rooms.0.get(room_id).cloned().unwrap_or_else(|| Err(String::new()))?;
    let room = crate::sliding_sync::get_client().zip(matrix_sdk::ruma::RoomId::parse(room_id).ok()).and_then(|(c, id)| c.get_room(&id));
    let unset = state.level_by.is_empty() && state.level == Ok(Level::Off);
    match room {
        Some(room) if unset && state.can_change => {
            crate::hagency::room_autonomy::server::set_level(&room, plan.level).await?;
            Ok(Some(plan.level))
        }
        _ => Ok(state.level.ok().filter(|l| *l != Level::Off)),
    }
}

/// One line per moved room (room content: English, like the loop's notices).
async fn tell(agent_id: &str, name: &str, moved: Vec<(String, Vec<String>, Option<Level>)>) {
    for (room_id, repos, level) in moved {
        let content = matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(moved_line(name, &repos, level));
        let txn = matrix_sdk::ruma::OwnedTransactionId::from(format!("hagency-moved-{}", blake3::hash(format!("{agent_id}\n{room_id}").as_bytes()).to_hex()));
        if let Ok(room_id) = matrix_sdk::ruma::OwnedRoomId::try_from(room_id.as_str()) {
            // Deliberately not an error: the move is done and saved; a lost line only goes unsaid.
            let _ = crate::hagency::room_send::send_routed_with(room_id, content, Default::default(), txn).await;
        }
    }
}

/// Move `agent`'s old settings, when any are left. Errors name the room and
/// why; nothing is switched on or off until every room's repositories are linked.
pub(crate) async fn run(service: &Service, agent: &Value, saved: &Value, rooms: &Rooms) -> Vec<String> {
    let id = agent["id"].as_str().unwrap_or_default();
    let plan = planned(saved, super::live::auto_merge(id).await, moved(id));
    if plan.is_empty() {
        return Vec::new();
    }
    let mut errors = Vec::new();
    for (room_id, part) in &plan {
        if let Err(error) = link_room(room_id, part, rooms).await {
            errors.push(crate::i18n::format("Could not move the watch to room {room}: {error}", &[("room", room_id.clone()), ("error", error)]));
        }
    }
    if !errors.is_empty() {
        return errors;
    }
    let mut done = Vec::new();
    for (room_id, part) in &plan {
        match enable_room(service, id, room_id, part, rooms).await {
            Ok(level) => done.push((room_id.clone(), part.repos.clone(), level)),
            Err(error) => return vec![crate::i18n::format("Could not move the watch to room {room}: {error}", &[("room", room_id.clone()), ("error", error)])],
        }
    }
    if let Err(error) = super::live::saved(service, id, Some(json!({"issueWatch": {"mode": "off"}}))).await {
        return vec![error];
    }
    if let Err(error) = record_moved(id) {
        return vec![error];
    }
    tell(id, agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent"), done).await;
    Vec::new()
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
