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

/// The source time per room, in its own table so the `room_autonomy` table
/// keeps the columns an earlier build writes positionally (GPT review P2: a
/// seventh column broke rollback).
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room_autonomy_as_of (room TEXT PRIMARY KEY, as_of INTEGER NOT NULL) STRICT;";

/// Creates the side table; a ledger that got the round-2 `as_of` column has
/// its times moved there and the column dropped, so an earlier build's
/// six-value insert works again.
pub(crate) fn ensure_schema(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch(SCHEMA)?;
    let has: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('room_autonomy') WHERE name='as_of')", [], |r| r.get(0))?;
    if has {
        db.execute_batch("INSERT OR REPLACE INTO room_autonomy_as_of SELECT room, as_of FROM room_autonomy WHERE as_of IS NOT NULL; ALTER TABLE room_autonomy DROP COLUMN as_of;")?;
    }
    Ok(())
}

/// Record `room`'s source time (`None`: leave the stored one, never clear it).
pub(crate) fn record(db: &rusqlite::Connection, room: &str, as_of: Option<i64>) -> Result<()> {
    if let Some(as_of) = as_of {
        db.execute("INSERT OR REPLACE INTO room_autonomy_as_of VALUES (?,?)", (room, as_of))?;
    }
    Ok(())
}

/// How far a snapshot lets agents go: fail-closed states first, then by level, then the maintainer bit.
fn reach(level: Option<&str>, paused: bool, unread: bool, maintainer: bool) -> (usize, bool) {
    let level = level.and_then(Level::from_wire).unwrap_or_default();
    let rank = if paused || unread { 0 } else { Level::ALL.iter().position(|l| *l == level).unwrap_or_default() };
    (rank, maintainer && rank > 0)
}

/// Whether `incoming`, arriving at `now`, replaces the room's recorded
/// snapshot, and the source time kept with it. One rule: a delayed snapshot
/// never loosens the gate (GPT review rounds 2–3).
/// - Both timed: a newer one replaces; an equal one only when not looser.
/// - Untimed over timed: only when not looser, and the stored time stays.
/// - Untimed over untimed: never looser within [`UNTIMED_GUARD_SECS`].
pub(crate) fn replaces(db: &rusqlite::Connection, incoming: &AutonomySnapshot, now: i64) -> Result<Option<Option<i64>>> {
    let stored: Option<(String, i64, i64, i64, i64, Option<i64>)> = db
        .query_row("SELECT level, paused, unread, maintainer, read_at, (SELECT as_of FROM room_autonomy_as_of t WHERE t.room=a.room) FROM room_autonomy a WHERE room=?", [&incoming.room_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .optional()?;
    let Some((level, paused, unread, maintainer, read_at, stored_as_of)) = stored else { return Ok(Some(incoming.as_of)) };
    let wire = incoming.level.unwrap_or_default().wire();
    let looser = reach(Some(wire), incoming.paused, incoming.unread, incoming.owner_maintainer) > reach(Some(&level), paused == 1, unread == 1, maintainer == 1);
    Ok(match (incoming.as_of, stored_as_of) {
        (Some(new), Some(old)) => (new > old || (new == old && !looser)).then_some(Some(new)),
        (Some(new), None) => Some(Some(new)),
        (None, Some(old)) => (!looser).then_some(Some(old)),
        (None, None) => (!looser || now.saturating_sub(read_at) >= UNTIMED_GUARD_SECS).then_some(None),
    })
}

/// A source time newer than every earlier one, for tests that record a room's
/// snapshots in sequence (each a fresh read).
#[cfg(test)]
pub(crate) fn next_for_test() -> Option<i64> {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    Some(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}
