//! What an owner's Agent does for the other members of a group room it was
//! added to (ADR 0024): a shared team member with per-room defaults kept in
//! the OWNER's ledger (never room state, so only the owner changes them,
//! through the owner API's approval settings).
//!
//! Defaults, when the owner saved nothing for the room:
//! - `answer`: room members' questions, summaries and status requests run
//!   without asking the owner (off: each one waits for the owner's OK);
//! - `review`: a room member's PR review of a room repository posts its
//!   verdict without a card (`watch_trust::review_post`); reading the room's
//!   repositories is card-free either way (ADR 0019);
//! - `fix`: OFF. A room member's fix steps (edit, commit, push, PR) follow
//!   the owner's rules; on, they get the owner's own push-freely trust on
//!   the repositories where the owner turned push freely on (ADR 0022).
//!
//! Merges keep the repository's merge rules only, and anything outside the
//! room's repositories asks the owner. The kill switch and budget apply as
//! before: nothing here is read before a run is admitted.
use crate::{key, Ledger, Result};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room_share (agent TEXT NOT NULL, room TEXT NOT NULL, answer INTEGER NOT NULL CHECK(answer IN (0,1)), review INTEGER NOT NULL CHECK(review IN (0,1)), fix INTEGER NOT NULL CHECK(fix IN (0,1)), PRIMARY KEY(agent,room)) STRICT;";

/// The owner's choices for one Agent in one room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoomShare {
    pub answer: bool,
    pub review: bool,
    pub fix: bool,
}

impl Default for RoomShare {
    /// The shared-team-member defaults (Chris, 2026-11-17).
    fn default() -> Self {
        Self { answer: true, review: true, fix: false }
    }
}

/// One saved room, as the owner API lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedRoomShare {
    pub room: String,
    pub answer: bool,
    pub review: bool,
    pub fix: bool,
}

impl SavedRoomShare {
    pub fn new(room: &str, share: RoomShare) -> Self {
        Self { room: room.into(), answer: share.answer, review: share.review, fix: share.fix }
    }
    pub fn share(&self) -> RoomShare {
        RoomShare { answer: self.answer, review: self.review, fix: self.fix }
    }
}

impl Ledger {
    /// Save the owner's choices for `agent` in `room`.
    pub fn set_room_share(&mut self, owner: &str, agent: &str, room: &str, share: RoomShare) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        key(room)?;
        // A Matrix room ID, never a free-form key (GPT review).
        if !room.starts_with('!') || !room.contains(':') {
            return Err(crate::Error::Invalid("room"));
        }
        self.db.execute(
            "INSERT INTO room_share VALUES (?,?,?,?,?) ON CONFLICT(agent,room) DO UPDATE SET answer=excluded.answer, review=excluded.review, fix=excluded.fix",
            (agent, room, share.answer as i64, share.review as i64, share.fix as i64),
        )?;
        Ok(())
    }
    /// The owner's choices for `agent` in `room`; the defaults when none were saved.
    pub fn room_share(&self, agent: &str, room: &str) -> Result<RoomShare> {
        read(&self.db, agent, room)
    }
    /// Every room the owner saved choices for, for the settings panel.
    pub fn room_shares(&self, agent: &str) -> Result<Vec<SavedRoomShare>> {
        let mut rows = self.db.prepare("SELECT room,answer,review,fix FROM room_share WHERE agent=? ORDER BY room")?;
        let saved = rows.query_map([agent], |r| {
            Ok(SavedRoomShare { room: r.get(0)?, answer: r.get::<_, i64>(1)? == 1, review: r.get::<_, i64>(2)? == 1, fix: r.get::<_, i64>(3)? == 1 })
        })?;
        Ok(saved.collect::<rusqlite::Result<_>>()?)
    }
}

/// [`Ledger::room_share`] on a bare connection (the trust checks hold one).
pub(crate) fn read(db: &rusqlite::Connection, agent: &str, room: &str) -> Result<RoomShare> {
    let saved = db
        .query_row("SELECT answer,review,fix FROM room_share WHERE agent=? AND room=?", [agent, room], |r| {
            Ok(RoomShare { answer: r.get::<_, i64>(0)? == 1, review: r.get::<_, i64>(1)? == 1, fix: r.get::<_, i64>(2)? == 1 })
        })
        .optional()?;
    Ok(saved.unwrap_or_default())
}

#[cfg(test)]
mod tests;
