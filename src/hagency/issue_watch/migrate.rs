//! One-time move of an Agent's old watch settings to the room work loop
//! (ADR 0030). The single-repository issue watch and the owner's work list
//! become: the repository linked to the watch's room (when missing), "may
//! work in this room" on for the Agent, and — only where the room has no
//! level yet and this account may set it — the room's level: Autonomous when
//! the Agent had "push freely" and "Pull request merges" on, else Assist.
//! Trial rows are ignored. Two phases (GPT review): every room's repositories
//! are linked first; only when all are linked is "may work" turned on (each
//! room told in one line), the old watch carried into the work list (its
//! issue filter and start time kept) and switched off, and the Agent recorded
//! as moved on this device. A room that refuses (no right to link) is
//! reported and the move retried next check, so a watch is never dropped
//! silently. Each work-list entry is planned until it is done (its room may
//! work and has it linked), whatever other rooms already may work, so a
//! partial move or a room set up first never loses one (batch GPT review).
//! A label watch stays as it is: the work list has no label filter.
//! Idempotent: a moved Agent's work list never moves again (an owner who
//! later unticks every room is not overruled).
use super::room_loop::{Rooms, may_work_rooms};
use std::collections::BTreeSet;
use crate::hagency::backend::Service;
use ::hagency::issue_watch::safeguards::room_autonomy::Level;
use ::hagency::issue_watch::{IssueFilter, OwnerRules};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// One room's share of the move: its repositories and the level it would get.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Move {
    pub repos: Vec<String>,
    pub level: Level,
}

/// The work-list filter an old watch's mode keeps; `None`: it cannot move
/// (off, or a label watch, which the work list cannot express).
fn carried_filter(watch: &Value) -> Option<IssueFilter> {
    match watch["mode"].as_str() {
        Some("trusted") => Some(IssueFilter::Trusted),
        Some("all") => Some(IssueFilter::All),
        _ => None,
    }
}

/// Whether `repo` of `room` already moved: the room may work and links it.
fn entry_done(saved: &Value, rooms: &Rooms, room: &str, repo: &str) -> bool {
    may_work_rooms(saved).iter().any(|r| r == room)
        && rooms.0.get(room).is_some_and(|s| s.as_ref().is_ok_and(|s| s.linked.iter().any(|l| l.eq_ignore_ascii_case(repo))))
}

/// What the old settings ask for, per room. `auto_merge` is the retired
/// per-Agent merge switch; `moved`: this device already moved the Agent's
/// work list. Empty when nothing is left to move.
pub(crate) fn planned(saved: &Value, rooms: &Rooms, auto_merge: bool, moved: bool) -> BTreeMap<String, Move> {
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
    if carried_filter(watch).is_some() {
        add(watch["roomId"].as_str().unwrap_or_default(), watch["repo"].as_str().unwrap_or_default(), watch["pushTrusted"] == true);
    }
    // The work list moves once per device, entry by entry, until each is done.
    if !moved {
        for entry in saved["work"].as_array().into_iter().flatten() {
            let (room, repo) = (entry["roomId"].as_str().unwrap_or_default(), entry["repo"].as_str().unwrap_or_default());
            if !entry_done(saved, rooms, room, repo) {
                add(room, repo, entry["pushFreely"] == true);
            }
        }
    }
    plan
}

/// The work list with the old watch carried in (its filter and start time
/// kept, so its eligible backlog stays eligible); `None` when the watch
/// cannot move or its repository is listed already (whose entry already
/// governed it: the watch never ran beside it), so nothing is rewritten.
pub(crate) fn work_with_watch(saved: &Value) -> Result<Option<Vec<Value>>, String> {
    let watch = &saved["issueWatch"];
    let mut work = saved["work"].as_array().cloned().unwrap_or_default();
    let repo = watch["repo"].as_str().unwrap_or_default();
    let Some(issues) = carried_filter(watch) else { return Ok(None) };
    if repo.is_empty() || work.iter().any(|w| w["repo"].as_str().is_some_and(|r| r.eq_ignore_ascii_case(repo))) {
        return Ok(None);
    }
    let entry = OwnerRules {
        repo: repo.to_owned(),
        room_id: watch["roomId"].as_str().unwrap_or_default().to_owned(),
        issues,
        since: watch["enabledAt"].as_i64().unwrap_or_default(),
        push_freely: watch["pushTrusted"] == true,
        ..Default::default()
    };
    work.push(serde_json::to_value(entry).map_err(|e| e.to_string())?);
    Ok(Some(work))
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

/// Phase two for one room: the level when unset and allowed, then may-work
/// on — in that order, so a failed level write is retried (a room that may
/// work counts as moved; GPT review). The level it ended at (`None`: left
/// for a maintainer).
async fn enable_room(service: &Service, agent_id: &str, room_id: &str, plan: &Move, rooms: &Rooms) -> Result<Option<Level>, String> {
    let state = rooms.0.get(room_id).cloned().unwrap_or_else(|| Err(String::new()))?;
    let room = crate::sliding_sync::get_client().zip(matrix_sdk::ruma::RoomId::parse(room_id).ok()).and_then(|(c, id)| c.get_room(&id));
    let unset = state.level_by.is_empty() && state.level == Ok(Level::Off);
    let level = match room {
        Some(room) if unset && state.can_change => {
            crate::hagency::room_autonomy::server::set_level(&room, plan.level).await?;
            Some(plan.level)
        }
        _ => state.level.ok().filter(|l| *l != Level::Off),
    };
    super::live::saved(service, agent_id, Some(json!({"mayWork": {"roomId": room_id, "on": true}}))).await?;
    Ok(level)
}

/// The line for one moved room (room content: English, like the loop's notices).
async fn tell(agent_id: &str, name: &str, room_id: &str, repos: &[String], level: Option<Level>) {
    let content = matrix_sdk::ruma::events::room::message::RoomMessageEventContent::text_plain(moved_line(name, repos, level));
    let txn = matrix_sdk::ruma::OwnedTransactionId::from(format!("hagency-moved-{}", blake3::hash(format!("{agent_id}\n{room_id}").as_bytes()).to_hex()));
    if let Ok(room_id) = matrix_sdk::ruma::OwnedRoomId::try_from(room_id) {
        // Deliberately not an error: the room may work; a lost line only goes unsaid.
        let _ = crate::hagency::room_send::send_routed_with(room_id, content, Default::default(), txn).await;
    }
}

/// Move `agent`'s old settings, when any are left. Errors name the room and
/// why; nothing is switched on or off until every room's repositories are linked.
pub(crate) async fn run(service: &Service, agent: &Value, saved: &Value, rooms: &Rooms) -> Vec<String> {
    let id = agent["id"].as_str().unwrap_or_default();
    let moved_here = moved(id);
    let plan = planned(saved, rooms, super::live::auto_merge(id).await, moved_here);
    if plan.is_empty() {
        // Every entry is done: record it, so a room unticked later is never ticked again.
        return if moved_here { Vec::new() } else { record_moved(id).err().into_iter().collect() };
    }
    let failed = |room: &str, error: String| crate::i18n::format("Could not move the watch to room {room}: {error}", &[("room", room.to_owned()), ("error", error)]);
    let mut errors = Vec::new();
    for (room_id, part) in &plan {
        if let Err(error) = link_room(room_id, part, rooms).await {
            errors.push(failed(room_id, error));
        }
    }
    if !errors.is_empty() {
        return errors;
    }
    let name = agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or("Agent");
    for (room_id, part) in &plan {
        match enable_room(service, id, room_id, part, rooms).await {
            Ok(level) => tell(id, name, room_id, &part.repos, level).await,
            Err(error) => return vec![failed(room_id, error)],
        }
    }
    finish(service, id).await.err().into_iter().collect()
}

/// The old watch carried into the work list and switched off in one save,
/// then the Agent recorded as moved on this device. Built from the saved
/// state read now, not the check's earlier snapshot, so an owner's edit made
/// during the room calls is kept (GPT review); the work list is written only
/// when the watch adds an entry.
async fn finish(service: &Service, id: &str) -> Result<(), String> {
    let saved = super::live::saved(service, id, None).await?;
    let mut change = json!({});
    if let Some(work) = work_with_watch(&saved)? {
        change["work"] = Value::Array(work);
    }
    if carried_filter(&saved["issueWatch"]).is_some() {
        change["issueWatch"] = json!({"mode": "off"});
    }
    if change != json!({}) {
        super::live::saved(service, id, Some(change)).await?;
    }
    record_moved(id)
}

#[cfg(test)]
#[path = "migrate_tests.rs"]
mod tests;
