//! The room's repository set for card-free read-only GitHub queries
//! (ADR 0023): repositories linked to the room, and those other owners'
//! watchers publish with a power level that could have linked them. The
//! host records them per (room, requester) before each turn, for a requester
//! who is a joined member; a record expires, so an unlink or a departure
//! ends the shortcut by the next turn at the latest.
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// How long a recorded set is believed without the host reading it again.
/// Each turn records it afresh; a longer turn then asks again (fails closed).
pub const SCOPE_TTL: Duration = Duration::from_secs(5 * 60);
/// Repositories one room's set may hold.
const MAX_REPOS: usize = 64;

type Key = (String, String);
static SCOPES: RwLock<Option<HashMap<Key, (Instant, Vec<String>)>>> = RwLock::new(None);

/// Replace `room`'s set for `requester` (empty: none).
pub fn record(room: &str, requester: &str, mut repos: Vec<String>) {
    repos.truncate(MAX_REPOS);
    // A poisoned lock still holds a whole map: inserts are single operations.
    let mut scopes = SCOPES.write().unwrap_or_else(|p| p.into_inner());
    let map = scopes.get_or_insert_with(HashMap::new);
    map.retain(|_, (at, _)| at.elapsed() < SCOPE_TTL);
    map.insert((room.to_owned(), requester.to_owned()), (Instant::now(), repos));
}

/// The recorded set of `room` for `requester`, if still fresh.
pub fn repos(room: &str, requester: &str) -> Vec<String> {
    let scopes = SCOPES.read().unwrap_or_else(|p| p.into_inner());
    let Some((at, repos)) = scopes.as_ref().and_then(|m| m.get(&(room.to_owned(), requester.to_owned()))) else { return Vec::new() };
    if at.elapsed() < SCOPE_TTL { repos.clone() } else { Vec::new() }
}

/// The repositories `room` itself links (`io.hagency.room_repos`, from a
/// sender the room's power levels still let link them), recorded per turn only
/// for a requester who is the room's owner or a maintainer: the only ones a
/// card-free issue may be filed on
/// (ADR 0030, [`super::issue_create`]). Same lifetime as [`repos`].
static LINKED: RwLock<Option<HashMap<Key, (Instant, Vec<String>)>>> = RwLock::new(None);

/// Replace `room`'s linked set for `requester` (empty: none).
pub fn record_linked(room: &str, requester: &str, mut repos: Vec<String>) {
    repos.truncate(MAX_REPOS);
    // A poisoned lock still holds a whole map: inserts are single operations.
    let mut linked = LINKED.write().unwrap_or_else(|p| p.into_inner());
    let map = linked.get_or_insert_with(HashMap::new);
    map.retain(|_, (at, _)| at.elapsed() < SCOPE_TTL);
    map.insert((room.to_owned(), requester.to_owned()), (Instant::now(), repos));
}

/// The recorded linked set of `room` for `requester`, if still fresh.
pub fn linked(room: &str, requester: &str) -> Vec<String> {
    let linked = LINKED.read().unwrap_or_else(|p| p.into_inner());
    let Some((at, repos)) = linked.as_ref().and_then(|m| m.get(&(room.to_owned(), requester.to_owned()))) else { return Vec::new() };
    if at.elapsed() < SCOPE_TTL { repos.clone() } else { Vec::new() }
}
