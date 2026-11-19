//! Which loaded run cards were handed over for admission this login: each
//! card and edit is claimed once, settled when stored, and retried a bounded
//! number of times (moved from loaded_run_cards.rs, which reached its budget).
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use super::{Key, Loaded};

/// Where a loaded card stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Handed {
    /// Being admitted, after this many failed tries.
    Admitting(u8),
    /// Stored with its latest edit: not handed over again.
    Stored,
    /// Not stored after this many tries (e.g. an untrusted sender, a full store).
    Failed(u8),
}
pub(super) static HANDED: LazyLock<Mutex<HashMap<Key, Handed>>> = LazyLock::new(Default::default);
/// Forgotten past this size; handing a card over again is harmless (admission is idempotent).
pub(super) const MAX_HANDED: usize = 2048;
/// Tries per card and edit before it waits for the next login.
pub(super) const MAX_TRIES: u8 = 3;

/// The cards not handed over yet in `generation`, now marked as being admitted.
pub(super) fn claim(generation: u64, cards: Vec<Loaded>) -> Vec<Loaded> {
    let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
    // An older login's claims never apply again (its tasks end on sign-out).
    handed.retain(|key, _| key.0 == generation);
    prune(&mut handed);
    cards.into_iter().filter(|c| claim_key(&mut handed, c.key(generation))).collect()
}

/// Over `MAX_HANDED`, forgets only what is safe to hand over again: stored
/// cards (`ingest` skips a card the store holds) and failures with tries left.
/// Out-of-tries failures are kept, so they are not fetched again this login
/// (GPT review P2: all were forgotten); past the cap even those go, to stay bounded.
fn prune(handed: &mut HashMap<Key, Handed>) {
    if handed.len() <= MAX_HANDED {
        return;
    }
    handed.retain(|_, state| matches!(state, Handed::Admitting(_) | Handed::Failed(MAX_TRIES..)));
    let mut over = handed.len().saturating_sub(MAX_HANDED);
    handed.retain(|_, state| {
        let evict = over > 0 && !matches!(state, Handed::Admitting(_));
        over -= usize::from(evict);
        !evict
    });
}

/// Marks `key` as being admitted unless it is stored, being admitted, or out of tries.
pub(super) fn claim_key(handed: &mut HashMap<Key, Handed>, key: Key) -> bool {
    let state = handed.entry(key).or_insert(Handed::Failed(0));
    match *state {
        Handed::Failed(n) if n < MAX_TRIES => { *state = Handed::Admitting(n); true }
        _ => false,
    }
}

pub(super) fn settle_key(key: Key, stored: bool) {
    let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
    let state = handed.entry(key).or_insert(Handed::Failed(0));
    *state = match *state {
        _ if stored => Handed::Stored,
        Handed::Admitting(n) | Handed::Failed(n) => Handed::Failed(n.saturating_add(1)),
        Handed::Stored => Handed::Failed(1),
    };
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod tests;
