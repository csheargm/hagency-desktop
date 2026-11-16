//! When a follow attempt counts as done, and how long to wait before the next
//! one. A (room, Agent) pair is recorded as handled only once the bind
//! succeeded or was definitively refused; a room that is not loaded yet or a
//! transient bind error is retried a bounded number of times, and a pair that
//! still failed stays open so a later join or restart tries again.
use std::{collections::BTreeSet, sync::Mutex, time::Duration};

/// Waits between attempts; its length bounds the attempts (one more than delays).
pub(crate) const RETRY_DELAYS_MS: [u64; 4] = [1_000, 2_000, 4_000, 8_000];

/// Bind errors that retrying cannot fix: the Agent or its binding is gone,
/// suspended or not allowed. Anything else (network, server busy) is transient.
const DEFINITIVE_BIND_ERRORS: [&str; 4] = ["agent_retired", "agent_binding_suspended", "agent_owner_binding_required", "agent_creation_denied"];

/// Lookup errors that mean "the joined room is not loaded yet": sliding sync
/// has not delivered it, or its power levels are not readable yet.
pub(crate) const ROOM_NOT_LOADED: &str = "room_join_required";
pub(crate) const POWER_LEVELS_UNREADABLE: &str = "power_levels_unavailable";

/// Pairs with a bind in flight, so a second trigger does not start a second bind.
static PENDING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// Whether this bind outcome settles the pair (record it as handled).
pub(crate) fn bind_settles(result: &Result<bool, String>) -> bool {
    match result {
        Ok(_) => true,
        Err(code) => DEFINITIVE_BIND_ERRORS.contains(&code.as_str()),
    }
}

/// Whether a failed room lookup is worth another try once the room loads.
pub(crate) fn lookup_retries(code: &str) -> bool {
    code == ROOM_NOT_LOADED || code == POWER_LEVELS_UNREADABLE
}

/// The wait before attempt `attempt + 1`, or None once attempts are used up.
pub(crate) fn delay_after(attempt: usize) -> Option<Duration> {
    RETRY_DELAYS_MS.get(attempt).map(|ms| Duration::from_millis(*ms))
}

/// A pair's in-flight mark; dropping it clears the mark, also when the bind
/// task is aborted (sign-out), so a pair is never blocked for the process.
#[derive(Debug)]
pub(crate) struct InFlight(String);

impl Drop for InFlight {
    fn drop(&mut self) {
        PENDING.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.0);
    }
}

/// Marks a pair in flight; None when a bind for it is already running.
pub(crate) fn begin(key: &str) -> Option<InFlight> {
    PENDING.lock().unwrap_or_else(|p| p.into_inner()).insert(key.to_owned()).then(|| InFlight(key.to_owned()))
}
