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
        write_private(&temp, &bytes)?;
        std::fs::rename(temp, path)
    }
}

/// Creates the file readable by its owner only: the JSON names rooms.
pub(super) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}

#[derive(Default)]
pub struct Store {
    loaded: HashMap<OwnedRoomId, Loaded>,
    progress: HashMap<(OwnedRoomId, String), Progress>,
    errors: HashMap<OwnedRoomId, String>,
    /// Initial loads asked for, per (account, room): after an account switch
    /// the new account still loads its own copy.
    requested: HashSet<(String, OwnedRoomId)>,
    /// Newest load ticket per room; an older load finishing later is stale.
    generation: HashMap<OwnedRoomId, u64>,
    /// A write went through but the list could not be reloaded: not an error.
    stale: HashMap<OwnedRoomId, String>,
    /// The signed-in account's dismissals, keyed by that account.
    dismissed: Option<(String, Dismissed)>,
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
/// One file per account, so switching accounts never inherits dismissals.
pub(super) fn dismissed_path(me: &str) -> std::path::PathBuf {
    account_file("room-memory-dismissed", me)
}
/// `<prefix>-<hex of the account id>.json` in the app's data folder.
pub(super) fn account_file(prefix: &str, me: &str) -> std::path::PathBuf {
    let account: String = me.bytes().map(|b| format!("{b:02x}")).collect();
    crate::app_data_dir().join(format!("{prefix}-{account}.json"))
}

impl Store {
    /// Memory loaded for the signed-in account, never another's.
    pub fn loaded(&self, room: &RoomId, me: Option<&str>) -> Option<&Loaded> {
        self.loaded.get(room).filter(|l| Some(l.me.as_str()) == me)
    }
    pub fn set_loaded(&mut self, loaded: Loaded) {
        self.errors.remove(&loaded.room_id);
        self.stale.remove(&loaded.room_id);
        self.loaded.insert(loaded.room_id.clone(), loaded);
    }
    /// Starts a load: only the newest ticket's result may replace the list.
    pub fn begin_load(&mut self, room: &RoomId) -> u64 {
        let ticket = self.generation.entry(room.to_owned()).or_default();
        *ticket += 1;
        *ticket
    }
    /// Applies a finished load unless a newer one was started meanwhile.
    pub fn set_loaded_at(&mut self, ticket: u64, loaded: Loaded) -> bool {
        let current = self.generation.get(&loaded.room_id).copied().unwrap_or(0);
        if ticket < current {
            return false;
        }
        self.set_loaded(loaded);
        true
    }
    pub fn set_stale(&mut self, room: &RoomId, message: String) {
        self.stale.insert(room.to_owned(), message);
    }
    pub fn stale(&self, room: &RoomId) -> Option<&str> {
        self.stale.get(room).map(String::as_str)
    }
    pub fn set_error(&mut self, room: &RoomId, message: String) {
        self.errors.insert(room.to_owned(), message);
    }
    pub fn error(&self, room: &RoomId) -> Option<&str> {
        self.errors.get(room).map(String::as_str)
    }
    pub fn clear_error(&mut self, room: &RoomId) {
        self.errors.remove(room);
        self.stale.remove(room);
    }
    /// True once per room: the first card or panel asks for the room's memory.
    /// Drops another account's request for the room, whose cache this replaces.
    pub fn first_request(&mut self, room: &RoomId, me: Option<&str>) -> bool {
        let me = me.unwrap_or_default();
        self.requested.retain(|(m, r)| r != room || m == me);
        self.requested.insert((me.to_owned(), room.to_owned()))
    }
    /// Whether a card or the panel has asked for this room's memory.
    pub fn was_requested(&self, room: &RoomId, me: Option<&str>) -> bool {
        self.requested.contains(&(me.unwrap_or_default().to_owned(), room.to_owned()))
    }
    pub fn forget_requests(&mut self, room: &RoomId) {
        self.requested.retain(|(_, r)| r != room);
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
    fn dismissals(&mut self, me: &str) -> &mut Dismissed {
        if self.dismissed.as_ref().is_none_or(|(owner, _)| owner != me) {
            self.dismissed = Some((me.to_owned(), Dismissed::load(&dismissed_path(me))));
        }
        self.dismissed.as_mut().map(|(_, d)| d).expect("set above")
    }
    pub fn is_dismissed(&mut self, room: &RoomId, me: Option<&str>, proposal: &str) -> bool {
        me.is_some_and(|me| self.dismissals(me).contains(room, proposal))
    }
    pub fn dismiss(&mut self, room: &RoomId, me: Option<&str>, proposal: &str) -> std::io::Result<()> {
        let me = me.ok_or_else(|| std::io::Error::other("not signed in"))?;
        self.dismissals(me).insert(&dismissed_path(me), room, proposal)
    }
    /// The state a proposal card shows right now.
    pub fn card_state(&mut self, room: &RoomId, me: Option<&str>, proposal: &str) -> CardState {
        let in_memory = self.loaded(room, me).is_some_and(|l| l.entries.iter().any(|e| e.id == proposal));
        let dismissed = self.is_dismissed(room, me, proposal);
        let (working, failure) = match self.progress(room, proposal) {
            Some(Progress::Working) => (true, None),
            Some(Progress::Failed(message)) => (false, Some(message.as_str())),
            None => (false, None),
        };
        super::model::card_state(in_memory, dismissed, working, failure)
    }
}
