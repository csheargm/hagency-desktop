//! Each Agent's "Join rooms I join" setting and the (room, Agent) pairs
//! already handled, kept per Matrix account in that account's persistent
//! state directory. A missing entry means on, also for Agents created before
//! the setting existed. An unreadable or corrupt file fails closed: every
//! Agent is treated as off (the owner gets the one-click prompt instead), and
//! the bad file is kept beside it as `<file>.corrupt` for diagnosis.
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, path::{Path, PathBuf}, sync::Mutex};

/// The one file that holds these choices (R3: the only place it is named).
pub(crate) const FILE_NAME: &str = "agent_follow.json";
const VERSION: u32 = 1;
/// More (room, Agent) pairs than anyone joins by hand; a hostile file cannot grow past it.
const MAX_HANDLED: usize = 8192;
/// Serializes load-modify-save between the UI thread and backend tasks.
static WRITE: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Prefs {
    version: u32,
    /// Agent id → "Join rooms I join". Absent means on.
    #[serde(default)]
    join_rooms: BTreeMap<String, bool>,
    /// `room id + " " + agent id` pairs already added, offered or declined:
    /// one add (or one prompt) per Agent per room.
    #[serde(default)]
    handled: BTreeSet<String>,
    /// Agent ids that already joined the owner's earlier rooms once.
    #[serde(default)]
    caught_up: BTreeSet<String>,
    #[serde(skip)]
    corrupt: bool,
}

pub(super) fn pair(room_id: &str, agent_id: &str) -> String {
    format!("{room_id} {agent_id}")
}

impl Prefs {
    pub fn load(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(error) => return Self::corrupt(path, &error.to_string()),
        };
        match serde_json::from_slice::<Prefs>(&bytes) {
            Ok(prefs) if prefs.version == VERSION => prefs,
            Ok(prefs) => Self::corrupt(path, &format!("unknown version {}", prefs.version)),
            Err(error) => Self::corrupt(path, &error.to_string()),
        }
    }
    /// Fail closed and keep a copy of the bad file. The copy is best effort:
    /// failing to make it must not block the (safe) closed settings, so both
    /// the read error and a copy error are logged.
    fn corrupt(path: &Path, reason: &str) -> Self {
        makepad_widgets::log!("Agent follow settings unreadable ({reason}); Agents ask before joining until saved");
        if let Err(error) = std::fs::copy(path, path.with_extension("json.corrupt")) {
            makepad_widgets::log!("Could not keep a copy of the unreadable settings: {error}");
        }
        Self { corrupt: true, ..Self::default() }
    }
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let file = Prefs { version: VERSION, join_rooms: self.join_rooms.clone(), handled: self.handled.clone(), caught_up: self.caught_up.clone(), corrupt: false };
        let bytes = serde_json::to_vec_pretty(&file).map_err(|e| e.to_string())?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        // Write then rename: a crash leaves the old choices, never half a file.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }
    pub fn joins_rooms(&self, agent_id: &str) -> bool {
        self.join_rooms.get(agent_id).copied().unwrap_or(!self.corrupt)
    }
    pub fn set_joins_rooms(&mut self, agent_id: &str, on: bool) {
        self.join_rooms.insert(agent_id.to_owned(), on);
    }
    pub fn is_handled(&self, room_id: &str, agent_id: &str) -> bool {
        self.handled.contains(&pair(room_id, agent_id))
    }
    /// Whether this Agent still has to catch up (never with a corrupt file).
    pub fn needs_catch_up(&self, agent_id: &str) -> bool {
        !self.corrupt && !self.caught_up.contains(agent_id)
    }
    /// Records that this Agent caught up on earlier rooms; false when it already had.
    /// A corrupt file never catches up (fail closed, like the setting).
    pub fn claim_catch_up(&mut self, agent_id: &str) -> Result<bool, String> {
        if self.corrupt || self.caught_up.contains(agent_id) { return Ok(false); }
        if self.caught_up.len() >= MAX_HANDLED { return Err("too_many_room_choices".into()); }
        Ok(self.caught_up.insert(agent_id.to_owned()))
    }
    /// Records the pair; false when it was already handled (nothing to do again).
    pub fn claim(&mut self, room_id: &str, agent_id: &str) -> Result<bool, String> {
        let key = pair(room_id, agent_id);
        if self.handled.contains(&key) { return Ok(false); }
        if self.handled.len() >= MAX_HANDLED { return Err("too_many_room_choices".into()); }
        Ok(self.handled.insert(key))
    }
}

/// This account's file, or None when nobody is signed in.
fn current_path() -> Option<PathBuf> {
    crate::sliding_sync::current_user_id().map(|user| crate::persistence::persistent_state_dir(&user).join(FILE_NAME))
}

/// The signed-in account's choices (defaults when signed out).
pub(crate) fn load_current() -> Prefs {
    current_path().map(|path| Prefs::load(&path)).unwrap_or_default()
}

/// Changes and saves the signed-in account's choices.
pub(crate) fn update_current<T>(change: impl FnOnce(&mut Prefs) -> Result<T, String>) -> Result<T, String> {
    let path = current_path().ok_or("sign_in_required")?;
    let _guard = WRITE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut prefs = Prefs::load(&path);
    let out = change(&mut prefs)?;
    prefs.save(&path)?;
    Ok(out)
}
