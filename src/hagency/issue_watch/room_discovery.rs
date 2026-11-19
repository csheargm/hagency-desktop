//! Which joined rooms are project rooms (round-2 GPT review, both reviewers):
//! sliding sync does not carry the custom state types, so a room whose linked
//! repositories or level live only on the server never showed in the local
//! store, was never read, and the gate took it for unknown. Every joined room
//! is asked the server itself once per [`DISCOVERY_TTL_SECS`] (at most
//! [`DISCOVERY_READS_PER_CHECK`] rooms per check, so a desktop in many rooms
//! spreads the reads over a few checks); a room found to be a project room is
//! then read in full every check (`room_loop::read`).
use ::hagency::issue_watch::safeguards::room_autonomy::ROOM_AUTONOMY_STATE_TYPE;
use ::hagency::repo_status::linked::ROOM_REPOS_STATE_TYPE;
use std::collections::{BTreeMap, BTreeSet};

/// How long a room's discovery answer stands before the server is asked again.
pub const DISCOVERY_TTL_SECS: i64 = 15 * 60;
/// Server discovery reads per check; the other due rooms wait for the next check.
pub const DISCOVERY_READS_PER_CHECK: usize = 32;
/// The state types that make a room a project room.
const PROJECT_TYPES: [&str; 2] = [ROOM_REPOS_STATE_TYPE, ROOM_AUTONOMY_STATE_TYPE];

/// Each room's last discovery answer (when, and whether it is a project room), per session.
#[derive(Debug, Default)]
pub(crate) struct Discovery {
    account: String,
    seen: BTreeMap<String, (i64, bool)>,
}

impl Discovery {
    /// Forget what another session found (a sign-out, or a sign-in as anyone).
    pub fn for_account(&mut self, account: &str) {
        if self.account != account {
            *self = Self { account: account.to_owned(), ..Default::default() };
        }
    }
    /// The rooms of `joined` to ask the server now: never asked, or asked
    /// [`DISCOVERY_TTL_SECS`] ago or longer; at most [`DISCOVERY_READS_PER_CHECK`].
    pub fn due(&self, joined: &[String], now: i64) -> Vec<String> {
        let stale = |room: &&String| self.seen.get(*room).is_none_or(|(at, _)| now - at >= DISCOVERY_TTL_SECS);
        joined.iter().filter(stale).take(DISCOVERY_READS_PER_CHECK).cloned().collect()
    }
    /// The server's answer for `room` at `now`.
    pub fn note(&mut self, room: &str, now: i64, project: bool) {
        self.seen.insert(room.to_owned(), (now, project));
    }
    /// The rooms of `joined` last found to be project rooms.
    pub fn project_rooms(&self, joined: &[String]) -> BTreeSet<String> {
        joined.iter().filter(|room| self.seen.get(*room).is_some_and(|(_, project)| *project)).cloned().collect()
    }
}

fn lock() -> std::sync::MutexGuard<'static, Discovery> {
    static DISCOVERY: std::sync::LazyLock<std::sync::Mutex<Discovery>> = std::sync::LazyLock::new(Default::default);
    // Plain timestamps and flags: still usable after a panic elsewhere held the lock.
    DISCOVERY.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether the room's server state holds linked repositories or a level.
async fn on_server(client: &matrix_sdk::Client, room: &matrix_sdk::ruma::RoomId) -> Result<bool, String> {
    crate::home::room_memory::server_state::events_of_types(client, room, &PROJECT_TYPES).await.map(|events| !events.is_empty())
}

/// How long one discovery read may take; a check's reads run together.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Every joined room the server (or the synced store) shows as a project room.
/// A room whose read fails counts as one until its next read (fail closed:
/// it is read and recorded unread), so failing rooms never hold the others back.
pub(crate) async fn project_rooms() -> BTreeSet<String> {
    let Some(client) = crate::sliding_sync::get_client() else { return BTreeSet::new() };
    let session = client.session_meta().map(|m| format!("{}/{}", m.user_id, m.device_id)).unwrap_or_default();
    let rooms = client.joined_rooms();
    let joined: Vec<String> = rooms.iter().map(|r| r.room_id().to_string()).collect();
    let now = chrono::Utc::now().timestamp();
    let due = {
        let mut discovery = lock();
        discovery.for_account(&session);
        discovery.due(&joined, now)
    };
    let reads = rooms.iter().filter(|r| due.iter().any(|d| d == r.room_id().as_str())).map(|room| async {
        let read = tokio::time::timeout(READ_TIMEOUT, on_server(&client, room.room_id())).await.unwrap_or_else(|_| Err("timed out".to_owned()));
        (room.room_id().to_string(), read)
    });
    for (room, read) in futures::future::join_all(reads).await {
        if let Err(error) = &read {
            makepad_widgets::log!("WARN room_discovery_unreadable room={room}: {error}");
        }
        lock().note(&room, now, read.unwrap_or(true));
    }
    let mut out = lock().project_rooms(&joined);
    out.extend(in_store(&rooms).await);
    out
}

/// The joined rooms the synced store already shows as project rooms: a room
/// linked since its last discovery read counts at once, not after the TTL.
async fn in_store(rooms: &[matrix_sdk::Room]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for room in rooms {
        for kind in PROJECT_TYPES {
            if room.get_state_events(kind.into()).await.is_ok_and(|events| !events.is_empty()) {
                out.insert(room.room_id().to_string());
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rooms(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("!r{i:03}:x")).collect()
    }

    /// Round-2 GPT review: a room whose project state only the server holds
    /// is found by asking the server, each room once per TTL, a bounded number per check.
    #[test]
    fn every_joined_room_is_asked_the_server_once_per_ttl_a_bounded_number_per_check() {
        let mut d = Discovery::default();
        d.for_account("@me:x");
        let joined = rooms(DISCOVERY_READS_PER_CHECK + 3);
        let first = d.due(&joined, 0);
        assert_eq!(first.len(), DISCOVERY_READS_PER_CHECK, "bounded per check");
        for room in &first {
            d.note(room, 0, room == "!r001:x");
        }
        assert_eq!(d.due(&joined, 60), joined[DISCOVERY_READS_PER_CHECK..].to_vec(), "the rest next check");
        assert_eq!(d.project_rooms(&joined).into_iter().collect::<Vec<_>>(), ["!r001:x"], "the server's answer, not the sync store's");
        for room in &joined[DISCOVERY_READS_PER_CHECK..] {
            d.note(room, 60, false);
        }
        assert!(d.due(&joined, DISCOVERY_TTL_SECS - 1).is_empty(), "nothing is asked twice within the TTL");
        assert_eq!(d.due(&joined, DISCOVERY_TTL_SECS).len(), DISCOVERY_READS_PER_CHECK, "asked again after it");
        assert!(d.project_rooms(&["!left:x".to_owned()]).is_empty(), "a room left is no longer listed");
        d.for_account("@other:x");
        assert!(d.project_rooms(&joined).is_empty(), "another account starts over");
    }
}
