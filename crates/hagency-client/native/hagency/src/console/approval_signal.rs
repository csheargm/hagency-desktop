//! Process-wide "personal approvals changed" signal (ported from the original
//! demo, snapshot 1949906dd).
//!
//! The owned runtime holds pending tool approvals in memory only. An in-process
//! host (the desktop) subscribes here so it can refresh the moment a request is
//! queued, decided or dropped, instead of waiting for its next poll. The value
//! is a change counter; it carries no approval content.
use std::{collections::BTreeMap, sync::LazyLock};
use tokio::sync::watch;

static CHANGED: LazyLock<watch::Sender<u64>> = LazyLock::new(|| watch::channel(0).0);

/// Wakes on every change to this process's pending personal approvals.
pub fn subscribe() -> watch::Receiver<u64> {
    CHANGED.subscribe()
}

/// Record that the pending set changed. Never fails, even with no subscriber.
pub(super) fn bump() {
    CHANGED.send_modify(|count| *count = count.wrapping_add(1));
}

/// `BTreeMap::retain` that signals only when something was actually dropped.
pub(super) fn retain<K: Ord, V>(map: &mut BTreeMap<K, V>, keep: impl FnMut(&K, &mut V) -> bool) {
    let before = map.len();
    map.retain(keep);
    if map.len() != before {
        bump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_signal_bumps_only_when_pending_set_changes() {
        let mut receiver = subscribe();
        let mut map = BTreeMap::from([(1, "a"), (2, "b")]);
        retain(&mut map, |_, _| true);
        // Other tests may bump the process-wide counter concurrently, so only
        // the positive direction is asserted against the shared signal.
        let start = *receiver.borrow_and_update();
        retain(&mut map, |key, _| *key != 1);
        assert_eq!(map.len(), 1);
        assert!(receiver.has_changed().unwrap());
        assert!(*receiver.borrow_and_update() > start);
    }
}
