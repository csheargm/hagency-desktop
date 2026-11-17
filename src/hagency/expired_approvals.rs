//! Requests that ended without an answer from this owner.
//!
//! A request that expired used to vanish: its card disappeared and the only
//! trace was the Agent's own reply saying the approval timed out. The owner
//! never learned that they had been asked. This store keeps a short record of
//! each one, so the approvals inbox, the room notice and the card on the
//! message can say "Expired, not answered" for a while.
//!
//! Nothing here can be decided: `pending_approvals::decide` refuses an
//! expired row, and the executor refuses an expired digest anyway.
use serde_json::Value;
use std::sync::{LazyLock, Mutex};

/// How long an expired request stays listed.
pub const KEEP_MS: u64 = 60 * 60 * 1000;
/// At most this many expired requests are kept.
const MAX_KEPT: usize = 20;
/// A request that leaves the list this close to its expiry, unanswered by
/// this client, is taken to have expired: the list is read every few seconds,
/// so the last reading before the end is up to one poll period early.
pub const EXPIRY_SLACK_MS: u64 = 15_000;

/// One request that ended unanswered, with the time this client noticed.
#[derive(Clone, Debug, PartialEq)]
pub struct Expired {
    pub row: Value,
    pub noticed_ms: u64,
}
impl Expired {
    pub fn id(&self) -> &str {
        self.row["approval"]["id"].as_str().unwrap_or_default()
    }
    pub fn room_id(&self) -> &str {
        self.row["approval"]["roomId"].as_str().unwrap_or_default()
    }
}

#[derive(Default)]
struct Store {
    kept: Vec<Expired>,
    /// Ids the owner dismissed from the inbox; the record stays on its card.
    dismissed: Vec<String>,
}
static STORE: LazyLock<Mutex<Store>> = LazyLock::new(Default::default);

/// Whether a request that left the list at `now` had run out of time.
pub fn ran_out(row: &Value, now: u64) -> bool {
    row["approval"]["expiresAtMs"].as_u64().is_some_and(|t| t <= now.saturating_add(EXPIRY_SLACK_MS))
}

/// Keep `row` as expired. Returns false when it was already kept.
pub fn record(row: &Value, now: u64) -> bool {
    let Ok(mut store) = STORE.lock() else { return false };
    let id = row["approval"]["id"].as_str().unwrap_or_default();
    if id.is_empty() || store.kept.iter().any(|e| e.id() == id) {
        return false;
    }
    // Kept as ended at `now` at the latest, so every card shows it expired
    // and `pending_approvals::decide` refuses it.
    let mut row = row.clone();
    let ended = row["approval"]["expiresAtMs"].as_u64().map_or(now, |t| t.min(now));
    row["approval"]["expiresAtMs"] = ended.into();
    store.kept.push(Expired { row, noticed_ms: now });
    prune(&mut store, now);
    true
}

fn prune(store: &mut Store, now: u64) {
    store.kept.retain(|e| now.saturating_sub(e.noticed_ms) < KEEP_MS);
    let excess = store.kept.len().saturating_sub(MAX_KEPT);
    store.kept.drain(..excess);
    let Store { kept, dismissed } = store;
    dismissed.retain(|id| kept.iter().any(|e| e.id() == id));
}

/// Expired requests the inbox lists, newest first.
pub fn listed(now: u64) -> Vec<Expired> {
    let Ok(mut store) = STORE.lock() else { return Vec::new() };
    prune(&mut store, now);
    store.kept.iter().rev().filter(|e| !store.dismissed.iter().any(|d| d == e.id())).cloned().collect()
}

/// The expired request raised by `event_id` in `room` (or, when
/// `include_thread_root`, by its thread), for the card on that message.
pub fn for_event(room: &str, event_id: &str, include_thread_root: bool) -> Option<Expired> {
    let store = STORE.lock().ok()?;
    store.kept.iter().rev().find(|e| e.room_id() == room && super::pending_approvals::raised_by(&e.row, event_id, include_thread_root)).cloned()
}

/// The newest expired request of `room` (and `thread`, when not empty).
pub fn latest_in(room: &str, thread: &str) -> Option<Expired> {
    let store = STORE.lock().ok()?;
    store.kept.iter().rev()
        .find(|e| e.room_id() == room && (thread.is_empty() || e.row["approval"]["threadRoot"] == thread))
        .cloned()
}

/// Hide `id` from the inbox.
pub fn dismiss(id: &str) {
    if let Ok(mut store) = STORE.lock()
        && !store.dismissed.iter().any(|d| d == id)
    {
        store.dismissed.push(id.to_owned());
    }
}

pub fn forget() {
    if let Ok(mut store) = STORE.lock() {
        *store = Store::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(id: &str, expires: u64) -> Value {
        json!({"approval":{"id":id,"roomId":"!r:x","eventId":"","threadRoot":"$t","expiresAtMs":expires}})
    }

    #[test]
    fn an_expired_request_is_kept_listed_until_dismissed_and_then_forgotten() {
        let _turn = super::super::pending_approvals::TEST_TURN.lock().unwrap_or_else(|e| e.into_inner());
        forget();
        assert!(ran_out(&row("a", 1_000), 990), "inside the slack it counts as expired");
        assert!(!ran_out(&row("a", 100_000), 1_000), "far from expiry it was answered elsewhere");
        assert!(!ran_out(&json!({"approval":{"id":"a"}}), 1_000), "no expiry, no claim");
        assert!(record(&row("a", 1_000), 2_000));
        assert!(!record(&row("a", 1_000), 2_100), "kept once");
        assert_eq!(listed(3_000).len(), 1);
        assert!(for_event("!r:x", "$t", false).is_some(), "the thread root shows it in its own thread");
        assert!(record(&row("b", 50_000), 2_000));
        let b = listed(3_000).into_iter().find(|e| e.id() == "b").unwrap();
        assert_eq!(b.row["approval"]["expiresAtMs"], 2_000, "kept as already ended");
        assert!(latest_in("!r:x", "$t").is_some() && latest_in("!r:x", "$other").is_none());
        dismiss("a");
        dismiss("b");
        assert!(listed(3_000).is_empty(), "dismissed from the inbox");
        assert!(for_event("!r:x", "$t", true).is_some(), "the card keeps its record");
        assert!(listed(2_000 + KEEP_MS).is_empty());
        assert!(latest_in("!r:x", "").is_none(), "forgotten after KEEP_MS");
        forget();
    }
}
