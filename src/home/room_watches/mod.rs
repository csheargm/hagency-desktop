//! The room information pane's "Watches" tab (ADR 0019): which of this
//! account's Agents watch which GitHub repository with reports to this room,
//! and whether each watch is active or paused. Read-only; the same facts the
//! `room_watches` Agent tool returns. Limits, tokens and watches reporting to
//! other rooms are never shown here.
pub mod panel;
#[cfg(test)]
mod tests;

use ::hagency::issue_watch::{IssueWatch, OwnerRules, RoomWatch, watch_entries};
use makepad_widgets::{Cx, ScriptVm};
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

pub fn script_mod(vm: &mut ScriptVm) {
    panel::script_mod(vm);
}

/// Posted when a room's list (or its failure) is loaded.
#[derive(Clone, Debug)]
pub struct WatchesChanged {
    pub room_id: OwnedRoomId,
}

type Loaded = Result<Vec<RoomWatch>, String>;
static STORE: LazyLock<Mutex<HashMap<OwnedRoomId, Loaded>>> = LazyLock::new(Default::default);

pub fn current(room: &RoomId) -> Option<Loaded> {
    STORE.lock().unwrap_or_else(|p| p.into_inner()).get(room).cloned()
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
    let room = room.to_owned();
    crate::sliding_sync::spawn_async_task(async move {
        let loaded = load(room.as_str()).await;
        STORE.lock().unwrap_or_else(|p| p.into_inner()).insert(room.clone(), loaded);
        Cx::post_action(WatchesChanged { room_id: room });
    });
}
