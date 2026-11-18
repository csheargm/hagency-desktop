//! One-time move of an Agent's old watch settings to the room work loop
//! (ADR 0030). The single-repository issue watch and the owner's work list
//! become: the repository linked to the watch's room (when missing), "may
//! work in this room" on for the Agent, and — only where the room has no
//! level yet and this account may set it — the room's level: Autonomous when
//! the Agent had "push freely" and "Pull request merges" on, else Assist.
//! Trial rows are ignored. Only when every room took the move is the old
//! watch switched off, and each moved room gets one line saying so; a room
//! that refuses (no right to link) is reported and retried next check, so a
//! watch is never dropped silently. Idempotent: a moved Agent has may-work
//! rooms and no active old watch, so nothing is left to move.
use super::room_loop::{Rooms, may_work_rooms};
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
/// per-Agent merge switch. Empty when the Agent was moved already.
pub(crate) fn planned(saved: &Value, auto_merge: bool) -> BTreeMap<String, Move> {
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
    // The work list moves once: before any may-work room exists.
    if may_work_rooms(saved).is_empty() {
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

/// Move one room: link its repositories, may-work on, level if unset.
/// The level it ended at (`None`: left for a maintainer).
async fn move_room(service: &Service, agent_id: &str, room_id: &str, plan: &Move, rooms: &Rooms) -> Result<Option<Level>, String> {
    let client = crate::sliding_sync::get_client().ok_or_else(|| crate::i18n::tr("Signed out").to_owned())?;
    let id = matrix_sdk::ruma::RoomId::parse(room_id).map_err(|e| e.to_string())?;
    let room = client.get_room(&id).ok_or_else(|| crate::i18n::tr("this desktop has not joined the room").to_owned())?;
    let state = rooms.0.get(room_id).cloned().unwrap_or_else(|| Err(String::new()))?;
    for repo in plan.repos.iter().filter(|r| !state.linked.iter().any(|l| l.eq_ignore_ascii_case(r))) {
        if !state.can_link {
            return Err(crate::i18n::format("you cannot link {repo} in this room; ask its owner or a maintainer to link it", &[("repo", repo.clone())]));
        }
        crate::home::room_watches::status::set_link(&client, &id, repo, true).await?;
    }
    super::live::saved(service, agent_id, Some(json!({"mayWork": {"roomId": room_id, "on": true}}))).await?;
    let unset = state.level_by.is_empty() && state.level == Ok(Level::Off);
    if unset && state.can_change {
        crate::hagency::room_autonomy::server::set_level(&room, plan.level).await?;
        return Ok(Some(plan.level));
    }
    Ok(state.level.ok().filter(|l| *l != Level::Off))
}

/// Move `agent`'s old settings, when any are left. Errors name the room and
/// why; nothing is switched off until every room took the move.
pub(crate) async fn run(service: &Service, agent: &Value, saved: &Value, rooms: &Rooms) -> Vec<String> {
    let id = agent["id"].as_str().unwrap_or_default();
    let plan = planned(saved, super::live::auto_merge(id).await);
    if plan.is_empty() {
        return Vec::new();
    }
    let name = agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent");
    let mut errors = Vec::new();
    let mut moved = Vec::new();
    for (room_id, part) in &plan {
        match move_room(service, id, room_id, part, rooms).await {
            Ok(level) => moved.push((room_id.clone(), part.repos.clone(), level)),
            Err(error) => errors.push(crate::i18n::format("Could not move the watch to room {room}: {error}", &[("room", room_id.clone()), ("error", error)])),
        }
    }
    if !errors.is_empty() {
        return errors;
    }
    if let Err(error) = super::live::saved(service, id, Some(json!({"issueWatch": {"mode": "off"}}))).await {
        return vec![error];
    }
    for (room_id, repos, level) in moved {
        let content = matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(moved_line(name, &repos, level));
        let txn = matrix_sdk::ruma::OwnedTransactionId::from(format!("hagency-moved-{}", blake3::hash(format!("{id}\n{room_id}").as_bytes()).to_hex()));
        if let Ok(room_id) = matrix_sdk::ruma::OwnedRoomId::try_from(room_id.as_str()) {
            // Deliberately not an error: the move is done and saved; a lost line only goes unsaid.
            let _ = crate::hagency::room_send::send_routed_with(room_id, content, Default::default(), txn).await;
        }
    }
    errors
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
