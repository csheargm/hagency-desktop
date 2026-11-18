//! The room information pane's "Watches" tab (ADR 0019): which of this
//! account's Agents watch which GitHub repository with reports to this room,
//! and whether each watch is active or paused. Read-only; the same facts the
//! `room_watches` Agent tool returns. Limits, tokens and watches reporting to
//! other rooms are never shown here.
pub mod panel;
pub mod review_setting;
#[cfg(test)]
mod tests;

use ::hagency::issue_watch::{IssueWatch, OwnerRules, RoomWatch, watch_entries};
use makepad_widgets::{Cx, ScriptVm};
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub fn script_mod(vm: &mut ScriptVm) {
    review_setting::script_mod(vm);
    panel::script_mod(vm);
}

/// Posted when a room's list (or its failure) is loaded.
#[derive(Clone, Debug)]
pub struct WatchesChanged {
    pub room_id: OwnedRoomId,
}

type Loaded = Result<Vec<RoomWatch>, String>;
/// Keyed by signed-in account and room, so another account on this device
/// never sees these rows (GPT review); each key keeps its newest request.
type Key = (String, OwnedRoomId);
#[derive(Default)]
struct Store {
    loaded: HashMap<Key, Loaded>,
    latest: HashMap<Key, u64>,
    next: u64,
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Default::default);

fn account() -> String {
    // The account session epoch too: a reconnect as the same user is a new session (GPT review).
    let user = crate::sliding_sync::current_user_id().map(|id| id.to_string()).unwrap_or_default();
    format!("{user}#{}", crate::account_session::epoch())
}
fn store() -> std::sync::MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(|p| p.into_inner())
}
pub fn current(room: &RoomId) -> Option<Loaded> {
    store().loaded.get(&(account(), room.to_owned())).cloned()
}
/// Starts a request for `key`; its ticket.
fn begin(key: &Key) -> u64 {
    let mut store = store();
    store.next += 1;
    let ticket = store.next;
    store.latest.insert(key.clone(), ticket);
    ticket
}
/// Keeps `loaded` only when `ticket` is still the newest request for `key`.
fn finish(key: Key, ticket: u64, loaded: Loaded) -> bool {
    let mut store = store();
    if store.latest.get(&key) != Some(&ticket) {
        return false;
    }
    store.loaded.insert(key, loaded);
    true
}

/// The rows for `room` from the Agents list and each Agent's saved watch
/// (`(agent id, owner API issue-watch reply)`). A watch is active only while
/// it is on and its Agent is active.
pub fn rows(room: &str, roster: &Value, saved: &[(String, Value)]) -> Result<Vec<RoomWatch>, String> {
    let agents = roster["agents"].as_array().ok_or_else(|| crate::i18n::tr("Unreadable Agents list").to_owned())?;
    let owner = roster["ownerMxid"].as_str().unwrap_or_default();
    let mut out = Vec::new();
    for agent in agents {
        let id = agent["id"].as_str().unwrap_or_default();
        let Some((_, reply)) = saved.iter().find(|(a, _)| a == id) else { continue };
        let watch: IssueWatch = serde_json::from_value(reply["issueWatch"].clone()).map_err(|e| format!("Unreadable issue watch: {e}"))?;
        let work: Vec<OwnerRules> = match &reply["work"] {
            Value::Null => Vec::new(),
            work => serde_json::from_value(work.clone()).map_err(|e| format!("Unreadable work list: {e}"))?,
        };
        let name = agent["displayName"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
        let running = agent["state"] == "active";
        out.extend(watch_entries(&watch, &work).into_iter().filter(|e| e.room_id == room).map(|e| RoomWatch {
            agent: name.to_owned(), owner: owner.to_owned(), repo: e.repo, active: e.active && running,
        }));
    }
    Ok(out)
}

async fn load(room: &str) -> Loaded {
    use crate::hagency::{backend::Service, model::Command};
    let service = Service::for_current()?;
    let roster = service.execute(Command::Agents).await?;
    let mut saved = Vec::new();
    for agent in roster["agents"].as_array().into_iter().flatten() {
        let Some(id) = agent["id"].as_str() else { continue };
        saved.push((id.to_owned(), crate::hagency::issue_watch::live::saved(&service, id, None).await?));
    }
    rows(room, &roster, &saved)
}

/// Reloads `room`'s list; the result (or the reason it failed) is shown in the tab.
pub fn refresh(room: &RoomId) {
    let key = (account(), room.to_owned());
    let ticket = begin(&key);
    crate::sliding_sync::spawn_async_task(async move {
        let loaded = load(key.1.as_str()).await;
        // A reply for an account that signed out meanwhile is dropped.
        if account() != key.0 {
            return;
        }
        let room_id = key.1.clone();
        if finish(key, ticket, loaded) {
            Cx::post_action(WatchesChanged { room_id });
        }
    });
}
