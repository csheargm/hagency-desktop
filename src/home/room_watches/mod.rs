//! The room information pane's "Status" tab (ADR 0019, ADR 0023): the
//! room's open Agent tasks; which of this account's Agents watch which GitHub
//! repository with reports to this room (active or paused); and, when the
//! room has repositories, one status table per repository with every
//! owner's published tracker states. The same facts the `room_watches` and
//! `repo_status` Agent tools return. Limits, tokens and watches reporting to
//! other rooms are never shown here. The room's merge rules (ADR 0029) are
//! shown to every member and editable by room admins (`merge_rules`).
pub mod autonomy;
pub mod autonomy_setup;
mod autonomy_store;
mod autonomy_watch;
pub use autonomy_watch::attach as watch_room_changes;
pub mod merge_rules;
pub mod merge_rules_model;
pub mod panel;
pub mod review_setting;
pub mod lines;
pub mod load;
pub mod status;
pub mod tasks;
#[cfg(test)]
mod tests;

use ::hagency::issue_watch::{IssueWatch, OwnerRules, RoomWatch, watch_entries};
use makepad_widgets::{Cx, ScriptVm};
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub fn script_mod(vm: &mut ScriptVm) {
    autonomy::script_mod(vm);
    review_setting::script_mod(vm);
    merge_rules::script_mod(vm);
    panel::script_mod(vm);
}

/// Posted when a room's list (or its failure) is loaded.
#[derive(Clone, Debug)]
pub struct WatchesChanged {
    pub room_id: OwnedRoomId,
}

/// This account's watch rows and the room's status (whose failure is shown on its own).
#[derive(Clone, Debug)]
pub struct Watches {
    pub rows: Vec<RoomWatch>,
    pub status: Result<status::RoomStatus, String>,
}
type Loaded = Result<Watches, String>;
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
    Ok(watches_of(room, roster, saved)?.into_iter().map(|(_, row)| row).collect())
}

/// [`rows`] with each row's Agent ID, for the status table's token choice.
fn watches_of(room: &str, roster: &Value, saved: &[(String, Value)]) -> Result<Vec<(String, RoomWatch)>, String> {
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
        out.extend(watch_entries(&watch, &work).into_iter().filter(|e| e.room_id == room).map(|e| (id.to_owned(), RoomWatch {
            agent: name.to_owned(), owner: owner.to_owned(), repo: e.repo, active: e.active && running,
        })));
    }
    Ok(out)
}

/// The newest link or unlink failure, or a note on a link, per room, shown above the list.
static LINK_ERRORS: LazyLock<Mutex<HashMap<Key, String>>> = LazyLock::new(Default::default);
pub fn link_error(room: &RoomId) -> Option<String> {
    LINK_ERRORS.lock().unwrap_or_else(|p| p.into_inner()).get(&(account(), room.to_owned())).cloned()
}

/// Links (`true`) or unlinks `repo` for `room`, then reloads the tab.
pub fn set_link(room: &RoomId, repo: String, link: bool) {
    let key = (account(), room.to_owned());
    // The client of the account that clicked, taken now; a later switch refuses (GPT review).
    let client = crate::sliding_sync::get_client();
    crate::sliding_sync::spawn_async_task(async move {
        let outcome = match client {
            Some(_) if account() != key.0 => Err(crate::i18n::tr("The signed-in account changed; nothing was changed").to_owned()),
            Some(client) => status::set_link(&client, &key.1, &repo, link).await,
            None => Err("Signed out".to_owned()),
        };
        let mut errors = LINK_ERRORS.lock().unwrap_or_else(|p| p.into_inner());
        match outcome {
            Ok(None) => errors.remove(&key),
            Ok(Some(message)) | Err(message) => errors.insert(key.clone(), message),
        };
        drop(errors);
        refresh(&key.1);
    });
}

/// "bird-claude's" activity in `room` for the members list, once the tab loaded it.
pub fn activity(room: &RoomId, agent: &str, owner: Option<&str>) -> Option<String> {
    let loaded = current(room)?.ok()?;
    let status = loaded.status.ok()?;
    Some(status::activity(&status, agent, owner))
}

/// Reloads `room`'s list; the result (or the reason it failed) is shown in the tab.
pub fn refresh(room: &RoomId) {
    let key = (account(), room.to_owned());
    let ticket = begin(&key);
    crate::sliding_sync::spawn_async_task(async move {
        // The timeout is shown at once, then retried once after a pause.
        for attempt in 0..2 {
            let loaded = load::load(key.1.as_str()).await;
            let again = attempt == 0 && load::is_too_long(&loaded);
            if !publish(key.clone(), ticket, loaded) || !again {
                return;
            }
            tokio::time::sleep(load::RETRY_BACKOFF).await;
        }
    });
}

/// Stores and announces `loaded`; false once the account changed or a newer request began.
fn publish(key: Key, ticket: u64, loaded: Loaded) -> bool {
    // A reply for an account that signed out meanwhile is dropped.
    if account() != key.0 {
        return false;
    }
    let room_id = key.1.clone();
    let kept = finish(key, ticket, loaded);
    if kept {
        Cx::post_action(WatchesChanged { room_id });
    }
    kept
}
