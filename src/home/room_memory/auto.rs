//! Opt-in automatic Accept: in a room where this person turned it on, memory
//! proposals from their own Agents are accepted by this client exactly as if
//! they clicked Accept (same planner, same power-level and size checks, the
//! write still comes from their own account). Off by default, per room and per
//! account, stored on this device. Skill proposals are never handled here:
//! they are a different card and always wait for a person.
use super::model::{CardState, Proposal};
use super::store::{self, MemoryChanged};
use matrix_sdk::ruma::{OwnedRoomId, RoomId, UserId};
use std::collections::{BTreeSet, HashSet};
use std::path::Path;
use std::sync::{LazyLock, Mutex, MutexGuard, atomic::{AtomicBool, Ordering}};

/// Rooms where this account accepts its own Agents' memory automatically.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct AutoRooms(BTreeSet<String>);
impl AutoRooms {
    pub fn load(path: &Path) -> Self {
        // A missing or unreadable file means the setting was never turned on:
        // off is the safe default.
        let parsed = std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
        Self(parsed.unwrap_or_default())
    }
    pub fn contains(&self, room: &RoomId) -> bool {
        self.0.contains(room.as_str())
    }
    pub fn set(&mut self, path: &Path, room: &RoomId, on: bool) -> std::io::Result<()> {
        if on { self.0.insert(room.to_string()) } else { self.0.remove(room.as_str()) };
        let bytes = serde_json::to_vec(&self.0).map_err(std::io::Error::other)?;
        let temp = path.with_extension("json.tmp");
        store::write_private(&temp, &bytes)?;
        std::fs::rename(temp, path)
    }
}

#[derive(Default)]
struct State {
    rooms: Option<(String, AutoRooms)>,
    /// Proposals this client accepted automatically (or tried to): never twice.
    claimed: HashSet<(OwnedRoomId, String)>,
}
static STATE: LazyLock<Mutex<State>> = LazyLock::new(Mutex::default);
static ROSTER_LOADING: AtomicBool = AtomicBool::new(false);
fn lock() -> MutexGuard<'static, State> {
    // Plain data: a panic elsewhere cannot leave it half-written.
    STATE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}
fn path(me: &str) -> std::path::PathBuf {
    store::account_file("room-memory-auto-accept", me)
}
fn rooms<'a>(state: &'a mut State, me: &str) -> &'a mut AutoRooms {
    if state.rooms.as_ref().is_none_or(|(owner, _)| owner != me) {
        state.rooms = Some((me.to_owned(), AutoRooms::load(&path(me))));
    }
    state.rooms.as_mut().map(|(_, r)| r).expect("set above")
}

/// Whether automatic Accept is on for `room` for the signed-in account.
pub fn is_on(room: &RoomId, me: Option<&str>) -> bool {
    me.is_some_and(|me| rooms(&mut lock(), me).contains(room))
}
/// Turns automatic Accept on or off; an error means it holds for this session only.
pub fn set(room: &RoomId, me: Option<&str>, on: bool) -> std::io::Result<()> {
    let me = me.ok_or_else(|| std::io::Error::other("not signed in"))?;
    rooms(&mut lock(), me).set(&path(me), room, on)
}
/// Sets the switch in memory only (unit tests never touch the data folder).
#[cfg(test)]
pub fn set_for_test(room: &RoomId, me: &str, on: bool) {
    let mut state = lock();
    if state.rooms.as_ref().is_none_or(|(owner, _)| owner != me) {
        state.rooms = Some((me.to_owned(), AutoRooms::default()));
    }
    let rooms = &mut state.rooms.as_mut().expect("set above").1;
    if on { rooms.0.insert(room.to_string()) } else { rooms.0.remove(room.as_str()) };
}
/// Whether this client accepted the proposal on its own.
pub fn was_auto(room: &RoomId, proposal: &str) -> bool {
    lock().claimed.contains(&(room.to_owned(), proposal.to_owned()))
}

/// The rule: on, from one of my own Agents, still pending, I may write, and
/// not tried before. Unknown ownership (`None`) never accepts.
pub fn should_accept(on: bool, own_agent: Option<bool>, state: &CardState, can_write: bool, claimed: bool) -> bool {
    on && own_agent == Some(true) && *state == CardState::Pending && can_write && !claimed
}

/// Decides for one card and claims the proposal when it should be accepted
/// now; the caller then runs the normal Accept.
pub fn claim(room: &RoomId, proposal: &Proposal, state: &CardState, can_write: bool) -> bool {
    let me = store::me();
    if !is_on(room, me.as_deref()) {
        return false;
    }
    let own = own_agent(room, me.as_deref(), &proposal.entry.author);
    let key = (room.to_owned(), proposal.entry.id.clone());
    let mut state_lock = lock();
    if !should_accept(true, own, state, can_write, state_lock.claimed.contains(&key)) {
        return false;
    }
    state_lock.claimed.insert(key)
}

fn own_agent(room: &RoomId, me: Option<&str>, author: &str) -> Option<bool> {
    let me = me.and_then(|me| <&UserId>::try_from(me).ok())?;
    let author = <&UserId>::try_from(author).ok()?;
    let known = crate::home::agent_ask_badge::is_own_agent(me, author);
    if known.is_none() {
        load_roster(room);
    }
    known
}

/// Reads this account's Agent listing once, then re-checks the room's cards.
fn load_roster(room: &RoomId) {
    if ROSTER_LOADING.swap(true, Ordering::SeqCst) {
        return;
    }
    let room = room.to_owned();
    crate::sliding_sync::spawn_async_task(async move {
        use crate::hagency::{backend::Service, model::Command};
        let result = async {
            let roster = Service::for_current()?.execute(Command::Agents).await?;
            let owner = crate::sliding_sync::current_user_id().ok_or("signed_out")?;
            if roster["ownerMxid"].as_str() != Some(owner.as_str()) {
                return Err::<(), String>("matrix_account_changed".into());
            }
            crate::home::agent_ask_badge::refresh(&owner, &roster["agents"]);
            Ok(())
        }.await;
        ROSTER_LOADING.store(false, Ordering::SeqCst);
        match result {
            Ok(()) => makepad_widgets::Cx::post_action(MemoryChanged { room_id: room }),
            // Silent on purpose: without the listing nothing is accepted
            // automatically and the card keeps its manual Accept; the next card
            // sync tries again.
            Err(error) => makepad_widgets::warning!("Agent listing for automatic memory Accept unavailable: {error}"),
        }
    });
}
