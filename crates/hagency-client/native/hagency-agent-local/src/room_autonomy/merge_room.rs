//! Where a merge may happen at all (ADR 0030, round-2 GPT review): only in
//! a room this device holds a fresh, readable autonomy snapshot of that says
//! its owner is the room's owner or a maintainer, or in the Agent's own
//! private chat with its owner. That chat is known from the server's binding
//! (`scope_kind: owner_direct`, recorded by [`Ledger::set_owner_direct`]),
//! never from membership or from what this device happens to have synced.
//! Any other room refuses the merge outright, card or not (fail closed).
use super::{NOT_MAINTAINER_REASON, ROOM_RULES_TTL_SECS};
use crate::{Ledger, Result, Scope};

/// Why a merge is refused in a room this device has no fresh read of.
pub const NOT_READ_MERGE_REASON: &str = "merges happen only in a project room the owner's Hagency desktop read in the last 5 minutes, or in the agent's private chat with its owner";

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS owner_direct_bindings (binding TEXT PRIMARY KEY REFERENCES bindings(id)) STRICT;";

impl Ledger {
    /// Record whether `scope`'s binding is its Agent's private chat with the
    /// owner, as the server's owner-scoped bindings say (owner API only; the
    /// binding must already be registered).
    pub fn set_owner_direct(&mut self, owner: &str, scope: &Scope, on: bool) -> Result<()> {
        self.owner(owner)?;
        crate::verify(&self.db, scope)?;
        if on {
            self.db.execute("INSERT OR IGNORE INTO owner_direct_bindings VALUES (?)", [&scope.binding])?;
        } else {
            self.db.execute("DELETE FROM owner_direct_bindings WHERE binding=?", [&scope.binding])?;
        }
        Ok(())
    }
    /// Whether `scope` is its Agent's private chat with the owner: its
    /// binding was recorded as one, for this very Agent and room.
    pub fn owner_direct(&self, scope: &Scope) -> Result<bool> {
        Ok(self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_direct_bindings d JOIN bindings b ON b.id=d.binding WHERE d.binding=?1 AND b.agent=?2 AND b.room=?3)",
            (&scope.binding, &scope.agent, &scope.room),
            |r| r.get(0),
        )?)
    }
    /// Why a merge proposed under `scope` is refused outright at `now`, or
    /// `None` when it may go on to the gate (or the owner's card).
    pub fn merge_refusal(&self, scope: &Scope, now: i64) -> Result<Option<&'static str>> {
        Ok(match self.owner_maintainer_at(&scope.room, now)? {
            Some(true) => None,
            Some(false) => Some(NOT_MAINTAINER_REASON),
            None if self.owner_direct(scope)? => None,
            None => Some(NOT_READ_MERGE_REASON),
        })
    }
    /// Whether this device's owner is the room's owner or a maintainer, by
    /// the fresh, readable snapshot of `room` (`None`: none to go by).
    pub fn owner_maintainer_at(&self, room: &str, now: i64) -> Result<Option<bool>> {
        use rusqlite::OptionalExtension;
        let row: Option<(i64, i64, i64)> = self.db.query_row("SELECT maintainer, unread, read_at FROM room_autonomy WHERE room=?", [room], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?;
        let fresh = |read_at: i64| now.saturating_sub(read_at) <= ROOM_RULES_TTL_SECS && read_at <= now.saturating_add(ROOM_RULES_TTL_SECS);
        Ok(row.filter(|(_, unread, read_at)| *unread == 0 && fresh(*read_at)).map(|(m, _, _)| m == 1))
    }
}

#[cfg(test)]
#[path = "merge_room_tests.rs"]
mod tests;
