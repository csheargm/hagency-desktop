//! What the Memory list and the proposal cards share: each room's loaded
//! memory, writes in flight and their failures, and the proposals this person
//! dismissed. Widgets read it on every sync; background tasks write it and
//! post [`MemoryChanged`].
use super::io::Loaded;
use super::model::CardState;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};
use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, path::Path, sync::{LazyLock, Mutex, MutexGuard}};

/// Posted whenever a room's memory, a write or a dismissal changed.
#[derive(Clone, Debug)]
pub struct MemoryChanged {
    pub room_id: OwnedRoomId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Progress {
    Working,
    Failed(String),
}

/// Proposals dismissed on this device, per room. Local on purpose: a dismissal
/// marker in the room would show as clutter to everyone and needs the right to
/// send; another approver still sees the card and may accept it.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Dismissed(BTreeMap<String, BTreeSet<String>>);
impl Dismissed {
    pub fn load(path: &Path) -> Self {
        // A missing or unreadable file means nothing was dismissed yet; the
        // worst case is that an old card shows again.
        let parsed = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self(parsed.unwrap_or_default())
    }
    pub fn contains(&self, room: &RoomId, proposal: &str) -> bool {
        self.0.get(room.as_str()).is_some_and(|ids| ids.contains(proposal))
    }
    /// Remembers a dismissal on disk; an error means it holds for this session only.
    pub fn insert(&mut self, path: &Path, room: &RoomId, proposal: &str) -> std::io::Result<()> {
        self.0.entry(room.to_string()).or_default().insert(proposal.into());
        let bytes = serde_json::to_vec(&self.0).map_err(std::io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, bytes)?;
        std::fs::rename(temp, path)
    }
}

#[derive(Default)]
pub struct Store {
    loaded: HashMap<OwnedRoomId, Loaded>,
    progress: HashMap<(OwnedRoomId, String), Progress>,
    errors: HashMap<OwnedRoomId, String>,
    requested: HashSet<OwnedRoomId>,
    dismissed: Option<Dismissed>,
}
#[cfg(test)]
thread_local! {
    /// The signed-in user a test pretends to be (no Matrix client in unit tests).
    pub static TEST_ME: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}
/// The signed-in user's id.
pub fn me() -> Option<String> {
    #[cfg(test)]
    if let Some(me) = TEST_ME.with(|me| me.borrow().clone()) {
        return Some(me);
    }
    crate::sliding_sync::current_user_id().map(|user| user.to_string())
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Mutex::default);
pub fn lock() -> MutexGuard<'static, Store> {
    // A panic elsewhere cannot leave this plain data half-written.
    STORE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
fn dismissed_path() -> std::path::PathBuf {
    crate::app_data_dir().join("room-memory-dismissed.json")
}

impl Store {
    /// Memory loaded for the signed-in account, never another's.
    pub fn loaded(&self, room: &RoomId, me: Option<&str>) -> Option<&Loaded> {
        self.loaded.get(room).filter(|l| Some(l.me.as_str()) == me)
    }
    pub fn set_loaded(&mut self, loaded: Loaded) {
        self.errors.remove(&loaded.room_id);
        self.loaded.insert(loaded.room_id.clone(), loaded);
    }
    pub fn set_error(&mut self, room: &RoomId, message: String) {
        self.errors.insert(room.to_owned(), message);
    }
    pub fn error(&self, room: &RoomId) -> Option<&str> {
        self.errors.get(room).map(String::as_str)
    }
    pub fn clear_error(&mut self, room: &RoomId) {
        self.errors.remove(room);
    }
    /// True once per room: the first card or panel asks for the room's memory.
    pub fn first_request(&mut self, room: &RoomId) -> bool {
        self.requested.insert(room.to_owned())
    }
    pub fn forget_requests(&mut self, room: &RoomId) {
        self.requested.remove(room);
    }
    pub fn set_progress(&mut self, room: &RoomId, subject: &str, progress: Option<Progress>) {
        let key = (room.to_owned(), subject.to_owned());
        match progress {
            Some(p) => self.progress.insert(key, p),
            None => self.progress.remove(&key),
        };
    }
    pub fn busy(&self, room: &RoomId) -> bool {
        self.progress.iter().any(|((r, _), p)| r == room && *p == Progress::Working)
    }
    pub fn progress(&self, room: &RoomId, subject: &str) -> Option<&Progress> {
        self.progress.get(&(room.to_owned(), subject.to_owned()))
    }
    pub fn is_dismissed(&mut self, room: &RoomId, proposal: &str) -> bool {
        self.dismissed.get_or_insert_with(|| Dismissed::load(&dismissed_path())).contains(room, proposal)
    }
    pub fn dismiss(&mut self, room: &RoomId, proposal: &str) -> std::io::Result<()> {
        let path = dismissed_path();
        self.dismissed.get_or_insert_with(|| Dismissed::load(&path)).insert(&path, room, proposal)
    }
    /// The state a proposal card shows right now.
    pub fn card_state(&mut self, room: &RoomId, me: Option<&str>, proposal: &str) -> CardState {
        let in_memory = self.loaded(room, me).is_some_and(|l| l.entries.iter().any(|e| e.id == proposal));
        let dismissed = self.is_dismissed(room, proposal);
        let (working, failure) = match self.progress(room, proposal) {
            Some(Progress::Working) => (true, None),
            Some(Progress::Failed(message)) => (false, Some(message.as_str())),
            None => (false, None),
        };
        super::model::card_state(in_memory, dismissed, working, failure)
    }
}
