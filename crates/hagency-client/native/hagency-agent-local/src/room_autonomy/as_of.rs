//! Which of two snapshots of a room the gate keeps (full-integration GPT
//! review P1): the newest by the room state it was read from (`as_of`, the
//! newest server time among the state events read), never by when it arrived.
//! A poll that read Autonomous before a Pause was saved can arrive after the
//! desktop pushed that Pause; it must not put Autonomous back. A snapshot
//! without a source time (an older desktop) never loosens a timed one
//! recorded within the desktop's poll interval.
use super::{AutonomySnapshot, Level};
use crate::Result;
use rusqlite::OptionalExtension;

/// The desktop's poll interval: how long an untimed snapshot may not loosen a timed one.
pub const UNTIMED_GUARD_SECS: i64 = 60;

/// Adds the source time to a ledger created before it existed.
pub(crate) fn ensure_schema(db: &rusqlite::Connection) -> Result<()> {
    let has: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('room_autonomy') WHERE name='as_of')", [], |r| r.get(0))?;
    if !has {
        db.execute_batch("ALTER TABLE room_autonomy ADD COLUMN as_of INTEGER")?;
    }
    Ok(())
}

/// How far a snapshot lets agents go: fail-closed states first, then by level, then the maintainer bit.
fn reach(level: Option<&str>, paused: bool, unread: bool, maintainer: bool) -> (usize, bool) {
    let level = level.and_then(Level::from_wire).unwrap_or_default();
    let rank = if paused || unread { 0 } else { Level::ALL.iter().position(|l| *l == level).unwrap_or_default() };
    (rank, maintainer && rank > 0)
}

/// Whether `incoming`, arriving at `now`, replaces the room's recorded snapshot.
pub(crate) fn replaces(db: &rusqlite::Connection, incoming: &AutonomySnapshot, now: i64) -> Result<bool> {
    let stored: Option<(String, i64, i64, i64, i64, Option<i64>)> = db
        .query_row("SELECT level, paused, unread, maintainer, read_at, as_of FROM room_autonomy WHERE room=?", [&incoming.room_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .optional()?;
    let Some((level, paused, unread, maintainer, read_at, stored_as_of)) = stored else { return Ok(true) };
    Ok(match (incoming.as_of, stored_as_of) {
        (Some(new), Some(old)) => new >= old,
        (Some(_), None) => true,
        (None, None) => true,
        (None, Some(_)) => {
            let wire = incoming.level.unwrap_or_default().wire();
            let looser = reach(Some(wire), incoming.paused, incoming.unread, incoming.owner_maintainer) > reach(Some(&level), paused == 1, unread == 1, maintainer == 1);
            !looser || now.saturating_sub(read_at) >= UNTIMED_GUARD_SECS
        }
    })
}
