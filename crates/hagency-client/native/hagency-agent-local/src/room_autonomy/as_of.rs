//! Which of two snapshots of a room the gate keeps (full-integration GPT
//! review P1): the newest by the room state it was read from (`as_of`, the
//! newest server time among the state events read), never by when it arrived.
//! A poll that read Autonomous before a Pause was saved can arrive after the
//! desktop pushed that Pause; it must not put Autonomous back. A snapshot
//! without a source time (an older desktop) never loosens a timed one
//! recorded within the desktop's poll interval.
//!
//! The sturdy part (GPT review round 4): a restriction the desktop pushes the
//! moment it is saved (Pause, Off, a lowered level) is sticky, stamped with
//! this ledger's own clock. While it holds, a looser snapshot counts only once
//! [`STICKY_HOLD_SECS`] passed on this clock since the push and, when the
//! saved event's server time is known, only if it was read after that event.
//! Homeserver timestamps never shorten the hold.
use super::{AutonomySnapshot, Level};
use crate::Result;
use rusqlite::OptionalExtension;

/// The desktop's poll interval: how long an untimed snapshot may not loosen a timed one.
pub const UNTIMED_GUARD_SECS: i64 = 60;
/// How long (this ledger's clock) a sticky restriction refuses every looser snapshot: two poll intervals.
pub const STICKY_HOLD_SECS: i64 = 2 * UNTIMED_GUARD_SECS;

/// The source time per room, in its own table so the `room_autonomy` table
/// keeps the columns an earlier build writes positionally (GPT review P2: a
/// seventh column broke rollback).
/// A sticky restriction per room: when this ledger received it, and the
/// saved event's server time when the desktop knew it.
const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room_autonomy_as_of (room TEXT PRIMARY KEY, as_of INTEGER NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS room_autonomy_sticky (room TEXT PRIMARY KEY, at INTEGER NOT NULL, saved_ts INTEGER) STRICT;";

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

/// What the ledger keeps with an accepted snapshot: its source time (`None`:
/// leave the stored one, never clear it) and the sticky restriction (`None`: none).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Kept {
    pub as_of: Option<i64>,
    pub sticky: Option<(i64, Option<i64>)>,
}

/// Record what [`replaces`] kept for `room`.
pub(crate) fn record(db: &rusqlite::Connection, room: &str, kept: &Kept) -> Result<()> {
    if let Some(as_of) = kept.as_of {
        db.execute("INSERT OR REPLACE INTO room_autonomy_as_of VALUES (?,?)", (room, as_of))?;
    }
    match kept.sticky {
        Some((at, saved_ts)) => db.execute("INSERT OR REPLACE INTO room_autonomy_sticky VALUES (?,?,?)", (room, at, saved_ts))?,
        None => db.execute("DELETE FROM room_autonomy_sticky WHERE room=?", [room])?,
    };
    Ok(())
}

/// How far a snapshot lets agents go: fail-closed states first, then by level, then the maintainer bit.
fn reach(level: Option<&str>, paused: bool, unread: bool, maintainer: bool) -> (usize, bool) {
    let level = level.and_then(Level::from_wire).unwrap_or_default();
    let rank = if paused || unread { 0 } else { Level::ALL.iter().position(|l| *l == level).unwrap_or_default() };
    (rank, maintainer && rank > 0)
}

/// Whether `incoming`, arriving at `now` (this ledger's clock), replaces the
/// room's recorded snapshot, and what is kept with it. A stricter or equal
/// snapshot always counts (a sticky push sets or renews the restriction).
/// A looser one, while a sticky restriction holds: only as described in the
/// module doc. Otherwise (GPT review rounds 2–3): both timed, only a newer
/// one; untimed over timed, never; untimed over untimed, not within
/// [`UNTIMED_GUARD_SECS`].
pub(crate) fn replaces(db: &rusqlite::Connection, incoming: &AutonomySnapshot, now: i64) -> Result<Option<Kept>> {
    let stored: Option<(String, i64, i64, i64, i64, Option<i64>)> = db
        .query_row("SELECT level, paused, unread, maintainer, read_at, (SELECT as_of FROM room_autonomy_as_of t WHERE t.room=a.room) FROM room_autonomy a WHERE room=?", [&incoming.room_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?))
        })
        .optional()?;
    let pushed = incoming.sticky.then_some((now, incoming.as_of));
    let Some((level, paused, unread, maintainer, read_at, stored_as_of)) = stored else { return Ok(Some(Kept { as_of: incoming.as_of, sticky: pushed })) };
    let sticky: Option<(i64, Option<i64>)> = db.query_row("SELECT at, saved_ts FROM room_autonomy_sticky WHERE room=?", [&incoming.room_id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?;
    let wire = incoming.level.unwrap_or_default().wire();
    let looser = reach(Some(wire), incoming.paused, incoming.unread, incoming.owner_maintainer) > reach(Some(&level), paused == 1, unread == 1, maintainer == 1);
    let as_of = incoming.as_of.max(stored_as_of);
    if !looser {
        return Ok(Some(Kept { as_of, sticky: pushed.or(sticky) }));
    }
    if let Some((at, saved_ts)) = sticky {
        let held = now.saturating_sub(at) < STICKY_HOLD_SECS;
        let read_after = saved_ts.is_none_or(|ts| incoming.as_of.is_some_and(|t| t >= ts));
        return Ok((!held && read_after).then_some(Kept { as_of, sticky: None }));
    }
    let newer = match (incoming.as_of, stored_as_of) {
        (Some(new), Some(old)) => new > old,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => now.saturating_sub(read_at) >= UNTIMED_GUARD_SECS,
    };
    Ok(newer.then_some(Kept { as_of, sticky: None }))
}

/// A source time newer than every earlier one, for tests that record a room's
/// snapshots in sequence (each a fresh read).
#[cfg(test)]
pub(crate) fn next_for_test() -> Option<i64> {
    static NEXT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1);
    Some(NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}
