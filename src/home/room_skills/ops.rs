//! The skills each room has loaded, writes in flight and their failures, and
//! the background work that changes them. A write always re-reads the room
//! first, so revision, storage and power level are current when planned.
use super::io::{self, Loaded};
use super::model::WriteError;
use crate::home::room_memory::model::Write;
use crate::shared::popup_list::{PopupKind, enqueue_popup_notification};
use crate::sliding_sync::{current_user_id, get_client, spawn_async_task};
use makepad_widgets::Cx;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex, MutexGuard};

/// Posted whenever a room's skills or a write changed.
#[derive(Clone, Debug)]
pub struct SkillsChanged {
    pub room_id: OwnedRoomId,
}

#[derive(Default)]
pub struct Store {
    pub loaded: HashMap<OwnedRoomId, Loaded>,
    pub errors: HashMap<OwnedRoomId, String>,
    /// (room, subject): a skill name, or a proposal id for a card.
    pub working: HashSet<(OwnedRoomId, String)>,
    pub requested: HashSet<OwnedRoomId>,
}
impl Store {
    pub fn current(&self, room: &RoomId, me: Option<&str>) -> Option<&Loaded> {
        self.loaded.get(room).filter(|l| Some(l.me.as_str()) == me)
    }
    pub fn busy(&self, room: &RoomId) -> bool {
        self.working.iter().any(|(r, _)| r == room)
    }
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Mutex::default);
pub fn lock() -> MutexGuard<'static, Store> {
    // A panic elsewhere cannot leave this plain data half-written.
    STORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub type Planner = Box<dyn FnOnce(&Loaded) -> Result<Write, WriteError> + Send>;

fn changed(room_id: &RoomId) {
    Cx::post_action(SkillsChanged { room_id: room_id.to_owned() });
}
fn fail(room_id: &RoomId, message: String) {
    lock().errors.insert(room_id.to_owned(), message.clone());
    enqueue_popup_notification(message, PopupKind::Error, None);
    changed(room_id);
}

/// Reloads the room's skills into the store.
pub fn refresh(room_id: &RoomId) {
    let (Some(client), Some(owner)) = (get_client(), current_user_id()) else { return };
    let room_id = room_id.to_owned();
    spawn_async_task(async move {
        let result = io::load(&client, &room_id).await;
        if current_user_id().as_ref() != Some(&owner) {
            return;
        }
        let mut store = lock();
        match result {
            Ok(loaded) => {
                store.errors.remove(&room_id);
                store.loaded.insert(room_id.clone(), loaded);
            }
            Err(message) => {
                store.requested.remove(&room_id);
                store.errors.insert(room_id.clone(), message);
            }
        }
        drop(store);
        changed(&room_id);
    });
}

/// Loads a room's skills the first time a card or the panel asks.
pub fn ensure_loaded(room_id: &RoomId) {
    if lock().requested.insert(room_id.to_owned()) {
        refresh(room_id);
    }
}

/// Runs one write for `subject`, then reloads.
pub fn submit(room_id: &RoomId, subject: &str, plan: Planner) {
    let (Some(client), Some(owner)) = (get_client(), current_user_id()) else {
        return fail(room_id, crate::i18n::tr("Sign in to change team skills.").to_owned());
    };
    let key = (room_id.to_owned(), subject.to_owned());
    {
        let mut store = lock();
        store.errors.remove(room_id);
        store.working.insert(key.clone());
    }
    changed(room_id);
    let room_id = room_id.to_owned();
    spawn_async_task(async move {
        let outcome = run(&client, &room_id, plan).await;
        if current_user_id().as_ref() != Some(&owner) {
            return;
        }
        lock().working.remove(&key);
        match outcome {
            Ok(Some(loaded)) => {
                lock().loaded.insert(room_id.clone(), loaded);
                changed(&room_id);
            }
            // Saved, but the reload failed: the next sync shows it.
            Ok(None) => refresh(&room_id),
            Err(message) => fail(&room_id, message),
        }
    });
}

async fn run(client: &matrix_sdk::Client, room_id: &RoomId, plan: Planner) -> Result<Option<Loaded>, String> {
    let loaded = io::load(client, room_id).await?;
    let write = plan(&loaded).map_err(|error| crate::i18n::tr(error.message()).to_owned())?;
    io::send(client, room_id, loaded.storage, &write).await?;
    Ok(io::load(client, room_id).await.ok())
}
