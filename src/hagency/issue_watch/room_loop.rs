//! The room work loop on the poller (ADR 0030). Each check reads every room
//! an Agent's loop touches from the homeserver, records the rooms' level and
//! Pause through the owner API (so the gate and push trust judge by fresh
//! values), derives the Agent's work list from "may work in this room" and
//! each such room's linked repositories, and runs only the rooms whose loop
//! runs (Assist or Autonomous, not paused, readable). The work list always
//! claims an issue before its hand-off, so two Agents never fix one issue.
use crate::hagency::room_autonomy::state::RoomState;
use ::hagency::issue_watch::OwnerRules;
use ::hagency::issue_watch::safeguards::room_autonomy::{AutonomySnapshot, MAX_ROOM_SNAPSHOTS};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// Each loop room's state as this check read it (or why it could not).
#[derive(Clone, Debug, Default)]
pub(crate) struct Rooms(pub BTreeMap<String, Result<RoomState, String>>);

/// The rooms `agent` may work in, from the owner API's `mayWork`.
pub(crate) fn may_work_rooms(saved: &Value) -> Vec<String> {
    saved["mayWork"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect()
}

/// Every room the Agent's loop touches: its may-work rooms, its work list's
/// rooms and its legacy watch's room (until the migration moves it).
pub(crate) fn loop_rooms(saved: &Value) -> BTreeSet<String> {
    let mut rooms: BTreeSet<String> = may_work_rooms(saved).into_iter().collect();
    rooms.extend(saved["work"].as_array().into_iter().flatten().filter_map(|w| w["roomId"].as_str()).map(str::to_owned));
    let watch = &saved["issueWatch"];
    if watch["mode"].as_str().is_some_and(|m| m != "off") {
        rooms.extend(watch["roomId"].as_str().filter(|r| !r.is_empty()).map(str::to_owned));
    }
    rooms
}

impl Rooms {
    /// What the owner API records: an unread room as unread (fail closed),
    /// the loop's own rooms (`first`) before every discovered one (round-2
    /// GPT review: discovered rooms must never crowd out a room the loop works in).
    pub(crate) fn snapshots(&self, first: &BTreeSet<String>) -> Vec<AutonomySnapshot> {
        let snapshot = |(room, state): (&String, &Result<RoomState, String>)| match state {
            Ok(state) => state.snapshot(room),
            Err(_) => AutonomySnapshot { room_id: room.clone(), unread: true, ..Default::default() },
        };
        let (mine, rest): (Vec<_>, Vec<_>) = self.0.iter().partition(|(room, _)| first.contains(*room));
        mine.into_iter().chain(rest).map(snapshot).collect()
    }
    /// Whether the loop may start work in `room` now, else why not.
    pub(crate) fn runs(&self, room: &str) -> Result<(), String> {
        match self.0.get(room) {
            Some(Ok(state)) => state.runs(),
            Some(Err(error)) => Err(error.clone()),
            None => Err(crate::i18n::tr("the room was not read").to_owned()),
        }
    }
    /// The rooms that are paused now.
    pub(crate) fn paused(&self) -> Vec<String> {
        self.0.iter().filter(|(_, s)| s.as_ref().is_ok_and(|s| s.pause.is_some())).map(|(r, _)| r.clone()).collect()
    }
}

/// The work list "may work" implies: each may-work room's linked
/// repositories, push steps on (for that room's repositories only), an
/// existing entry's own settings and start time kept, a new one starting
/// `now`. A room that could not be read keeps its entries as they are, so
/// one failed read never drops work. A repository linked in two rooms goes
/// to the first whose loop runs, else the first (batch GPT review: a paused
/// room must not take it from a running one).
pub(crate) fn derived_work(saved: &Value, rooms: &Rooms, now: i64) -> Vec<OwnerRules> {
    let current: Vec<OwnerRules> = serde_json::from_value(saved["work"].clone()).unwrap_or_default();
    let mut out: Vec<OwnerRules> = Vec::new();
    let (running, held): (Vec<String>, Vec<String>) = may_work_rooms(saved).into_iter().partition(|r| rooms.runs(r).is_ok());
    for room in running.into_iter().chain(held) {
        let repos: Vec<String> = match rooms.0.get(&room) {
            Some(Ok(state)) => state.linked.clone(),
            _ => current.iter().filter(|w| w.room_id == room).map(|w| w.repo.clone()).collect(),
        };
        for repo in repos {
            if out.iter().any(|w| w.repo.eq_ignore_ascii_case(&repo)) {
                continue;
            }
            let base = current.iter().find(|w| w.repo.eq_ignore_ascii_case(&repo)).cloned();
            let entry = base.unwrap_or_else(|| OwnerRules { repo: repo.clone(), since: now, ..Default::default() });
            out.push(OwnerRules { room_id: room.clone(), push_freely: true, ..entry });
        }
    }
    out
}

/// Whether the saved work list already is `derived` (same repositories, rooms and settings).
pub(crate) fn same_work(saved: &Value, derived: &[OwnerRules]) -> bool {
    let current: Vec<OwnerRules> = serde_json::from_value(saved["work"].clone()).unwrap_or_default();
    let key = |w: &OwnerRules| (w.repo.to_ascii_lowercase(), serde_json::to_string(&OwnerRules { repo: w.repo.to_ascii_lowercase(), ..w.clone() }).unwrap_or_default());
    let mut a: Vec<_> = current.iter().map(key).collect();
    let mut b: Vec<_> = derived.iter().map(key).collect();
    a.sort();
    b.sort();
    a == b
}

/// `saved` with only the work entries whose room runs now, and one status
/// part per room that holds them ("acme/app: the room is paused …").
pub(crate) fn running_only(saved: &Value, rooms: &Rooms) -> (Value, Vec<String>) {
    let mut kept = saved.clone();
    let mut held = Vec::new();
    let work = saved["work"].as_array().cloned().unwrap_or_default();
    let running: Vec<Value> = work.into_iter().filter(|w| {
        let room = w["roomId"].as_str().unwrap_or_default();
        match rooms.runs(room) {
            Ok(()) => true,
            Err(why) => {
                held.push(format!("{}: {why}", w["repo"].as_str().unwrap_or_default()));
                false
            }
        }
    }).collect();
    kept["work"] = Value::Array(running);
    (kept, held)
}

/// How many rooms' state is read from the homeserver at once.
const ROOM_READS_AT_ONCE: usize = 16;
/// One room's read may take this long; past it the room counts as unread (fails closed).
const ROOM_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Read every loop room of `saved`, and every joined project room, from the
/// homeserver now — in parallel and each bounded, so many discovered rooms can
/// never push the check past its deadline (final GPT check: serial reads did).
pub(crate) async fn read(saved: &Value) -> Rooms {
    use futures_util::StreamExt;
    let mut all = loop_rooms(saved);
    all.extend(super::room_discovery::project_rooms().await);
    let reads = futures_util::stream::iter(all).map(|room_id| async move {
        let state = tokio::time::timeout(ROOM_READ_TIMEOUT, read_one(&room_id)).await
            .unwrap_or_else(|_| Err(crate::i18n::tr("the room's state did not arrive in time").to_owned()));
        (room_id, state)
    });
    Rooms(reads.buffer_unordered(ROOM_READS_AT_ONCE).collect().await)
}

/// One joined room's autonomy state from the homeserver.
async fn read_one(room_id: &str) -> Result<RoomState, String> {
    let client = crate::sliding_sync::get_client();
    let joined = matrix_sdk::ruma::RoomId::parse(room_id).ok().and_then(|id| client.as_ref()?.get_room(&id));
    match joined {
        Some(room) => crate::hagency::room_autonomy::server::read(&room).await,
        None => Err(crate::i18n::tr("this desktop has not joined the room").to_owned()),
    }
}

/// Everything before a check's work: the saved state, its rooms read now,
/// the one-time move of old settings, the rooms' level and Pause recorded,
/// the work list "may work" implies saved, and the loop's runs in paused
/// rooms stopped. `None` when the Agent has no loop at all.
pub(super) async fn prepare(service: &crate::hagency::backend::Service, agent: &Value) -> Result<Option<(Value, Rooms, Vec<String>)>, String> {
    let id = agent["id"].as_str().unwrap_or_default();
    let mut saved = super::live::saved(service, id, None).await?;
    let plans = saved["plans"].as_array().is_some_and(|p| !p.is_empty());
    if loop_rooms(&saved).is_empty() && !plans {
        // No loop here, but the device's project rooms are still recorded (batch GPT review).
        record(service, id, &saved, &read(&saved).await).await?;
        return Ok(None);
    }
    let mut rooms = read(&saved).await;
    let moving = !super::migrate::planned(&saved, &rooms, false, super::migrate::moved(id)).is_empty();
    let held = super::migrate::run(service, agent, &saved, &rooms).await;
    if moving {
        // The move linked repositories: read the rooms again so the work list keeps them.
        rooms = read(&saved).await;
    }
    saved = super::live::saved(service, id, None).await?;
    // The work list follows "may work" only once the old settings moved (never drops a
    // watch), and before the rooms are recorded running, so an unlinked repository's
    // push grant is gone first (GPT review).
    if held.is_empty() {
        let derived = derived_work(&saved, &rooms, chrono::Utc::now().timestamp());
        if !same_work(&saved, &derived) {
            saved = super::live::saved(service, id, Some(serde_json::json!({"work": derived}))).await?;
        }
    }
    // Not recorded: nothing runs this check (GPT review: the last positive snapshot
    // would otherwise keep counting for up to its TTL while this check dispatched).
    record(service, id, &saved, &rooms).await?;
    stop_paused(&rooms).await;
    Ok(Some((saved, rooms, held)))
}

/// Record `rooms`' level and Pause through the owner API, `saved`'s loop
/// rooms first, in changes of at most [`MAX_ROOM_SNAPSHOTS`] each, so no room
/// is ever left out (round-2 GPT review: an unrecorded project room read as a private chat).
async fn record(service: &crate::hagency::backend::Service, id: &str, saved: &Value, rooms: &Rooms) -> Result<(), String> {
    for chunk in rooms.snapshots(&loop_rooms(saved)).chunks(MAX_ROOM_SNAPSHOTS) {
        super::live::saved(service, id, Some(serde_json::json!({"roomAutonomy": chunk}))).await
            .map_err(|error| crate::i18n::format("The rooms' agent work settings could not be recorded: {error}", &[("error", error)]))?;
    }
    Ok(())
}

/// Pause stops the loop's own runs in progress in each paused room.
async fn stop_paused(rooms: &Rooms) {
    let Some(client) = crate::sliding_sync::get_client() else { return };
    for room_id in rooms.paused() {
        let room = matrix_sdk::ruma::RoomId::parse(&room_id).ok().and_then(|id| client.get_room(&id));
        if let Some(room) = room {
            crate::hagency::room_autonomy::stop::stop_loop_runs(&room).await;
        }
    }
}

#[cfg(test)]
#[path = "room_loop_tests.rs"]
mod tests;
