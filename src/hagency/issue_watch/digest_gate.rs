//! The merge gate's holds as they are now, for the daily digest's "needs
//! you" (ADR 0028, integration GPT review): per Agent and repository, the
//! issues whose pull request this process's latest follow found held for
//! its owner (a hold notice, or no other Agent to review). The ledger's
//! `notified` flag cannot say it: it stays set while a held pull request
//! goes on to merge. Fed from every status publish, whether or not the room
//! takes it; unknown until the repository's first follow since start.
use super::tracker::{Followed, hold_of};
use ::hagency::repo_status::Published;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

type Holds = HashMap<(String, String), Vec<i64>>;
static HOLDS: LazyLock<Mutex<Holds>> = LazyLock::new(Default::default);

/// The issues of `published` that wait for their owner: held for anything
/// but running CI, which needs nobody.
pub(crate) fn owner_holds(published: &[Published]) -> Vec<i64> {
    let ci = hold_of(&Followed::WaitingCi);
    published.iter().filter(|p| p.hold.is_some() && p.hold != ci).map(|p| p.number).collect()
}

/// Record one follow's published entries for `agent_mxid`'s `repo`.
pub(crate) fn note(agent_mxid: &str, repo: &str, published: &[Published]) {
    // A poisoned lock still holds a whole map: inserts are single operations.
    HOLDS.lock().unwrap_or_else(|p| p.into_inner()).insert((agent_mxid.to_owned(), repo.to_ascii_lowercase()), owner_holds(published));
}

/// The issues held for the owner now; `None` before this process followed the repository.
pub(crate) fn needs_owner(agent_mxid: &str, repo: &str) -> Option<Vec<i64>> {
    HOLDS.lock().unwrap_or_else(|p| p.into_inner()).get(&(agent_mxid.to_owned(), repo.to_ascii_lowercase())).cloned()
}
