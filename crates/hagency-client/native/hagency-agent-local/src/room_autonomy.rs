//! The room's work loop controls (ADR 0030): one autonomy level per project
//! room (`Off`, `Assist`, `Autonomous`) and Pause / Resume.
//!
//! - The level is room state [`ROOM_AUTONOMY_STATE_TYPE`] (state key ""),
//!   for the room's owner and maintainers; its content also holds `cleared`,
//!   the event IDs of the pauses the last Resume saw and cleared.
//! - A pause is room state [`ROOM_PAUSE_STATE_TYPE`] keyed by the pausing
//!   member's own Matrix ID (the homeserver lets nobody write another
//!   member's key), open to every member (power level 0). A pause event
//!   counts unless its event ID is among the level event's `cleared`, whatever
//!   its content or timestamp: a member who edits or redacts their own pause
//!   event only sends a new event, so a member can pause but never resume,
//!   and no clock (a server's or a sender's) can hide a pause (GPT review).
//!
//! The gate never reads Matrix. The owner's desktop reads both from the
//! homeserver and records a snapshot per room through the owner API
//! ([`Ledger::set_room_autonomy`]); a snapshot counts for
//! [`ROOM_RULES_TTL_SECS`]. Missing, stale or unreadable: nothing new starts
//! and nothing is delivered (fail closed), with the reason said.
//!
//! Per Agent, the owner only decides whether it may work in a room
//! ([`Ledger::set_may_work`]): its loop, push steps and reviews there.
use crate::merge_policy::room_rules::{unix_now, ROOM_RULES_TTL_SECS};
use crate::{key, Error, Ledger, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The room state event type of a room's autonomy level.
pub const ROOM_AUTONOMY_STATE_TYPE: &str = "io.hagency.room_autonomy";
/// The room state event type of one member's pause (state key: their Matrix ID).
pub const ROOM_PAUSE_STATE_TYPE: &str = "io.hagency.room_autonomy_pause";
const LEVEL_FIELD: &str = "level";
const CLEARED_FIELD: &str = "cleared";
/// The most pause event IDs one Resume records (one current pause per member).
pub const MAX_CLEARED: usize = 1000;

/// How far a room's agents go on their own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// No loop: agents only answer when asked.
    #[default]
    Off,
    /// Agents claim, work and review; every delivery waits for a person.
    Assist,
    /// Deliveries happen on their own where the room rules allow them.
    Autonomous,
}

impl Level {
    pub const ALL: [Level; 3] = [Level::Off, Level::Assist, Level::Autonomous];
    pub fn wire(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Assist => "assist",
            Self::Autonomous => "autonomous",
        }
    }
    pub fn from_wire(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.wire() == text)
    }
}

/// The level in a level event's content (`None`: not a level this reads).
pub fn level_of(content: &Value) -> Option<Level> {
    Level::from_wire(content[LEVEL_FIELD].as_str()?)
}
/// The pause event IDs the last Resume cleared (none: never resumed).
pub fn cleared(content: &Value) -> Vec<String> {
    content[CLEARED_FIELD].as_array().into_iter().flatten().filter_map(Value::as_str).take(MAX_CLEARED).map(str::to_owned).collect()
}
/// The level event content for `level`, keeping `cleared`.
pub fn level_content(level: Level, cleared: &[String]) -> Value {
    json!({ LEVEL_FIELD: level.wire(), CLEARED_FIELD: cleared.iter().take(MAX_CLEARED).collect::<Vec<_>>() })
}
/// The pause event content (informative only: its timestamp is what counts).
pub fn pause_content(now_ms: i64) -> Value {
    json!({ "pausedAt": now_ms })
}

/// The pause in force: who paused last and when (server ms).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pause {
    pub by: String,
    pub at_ms: i64,
}

/// The room's pause events: keyed by their own sender, with an event ID.
fn pauses(events: &[Value]) -> impl Iterator<Item = (&str, &str, i64)> {
    events
        .iter()
        .filter(|e| e["type"] == ROOM_PAUSE_STATE_TYPE && e["state_key"].as_str().is_some_and(|k| k.starts_with('@') && e["sender"] == k))
        .filter_map(|e| Some((e["event_id"].as_str().filter(|id| id.starts_with('$'))?, e["sender"].as_str()?, e["origin_server_ts"].as_i64().unwrap_or(0))))
}

/// The newest counting pause among the server's state `events`: a pause
/// event keyed by its own sender that the last Resume did not clear. Content
/// and timestamps never decide, so an edited or redacted pause still counts.
pub fn pause_in(events: &[Value], cleared: &[String]) -> Option<Pause> {
    pauses(events).filter(|(id, _, _)| !cleared.iter().any(|c| c == id)).map(|(_, by, at_ms)| Pause { by: by.to_owned(), at_ms }).max_by_key(|p| p.at_ms)
}
/// What a Resume clears: exactly the pause events it read now, so any pause
/// sent after the read still counts.
pub fn resume_clears(events: &[Value]) -> Vec<String> {
    pauses(events).map(|(id, _, _)| id.to_owned()).take(MAX_CLEARED).collect()
}

/// Why the loop does not do something in a room.
pub const OFF_REASON: &str = "the room's agent work is Off — a room admin can choose Assist or Autonomous in Room info → Status";
pub const ASSIST_REASON: &str = "room is in Assist — a person delivers";
pub const PAUSED_REASON: &str = "the room is paused — nothing new starts and nothing is delivered until a room admin resumes it";
pub const UNREAD_REASON: &str = "the room's autonomy level could not be read, so agents treat the room as paused until it is";
pub const STALE_REASON: &str = "the room's autonomy level was not re-read in the last 5 minutes (the owner's Hagency desktop re-reads it while it runs), so agents treat the room as paused";
pub const NOT_READ_REASON: &str = "the room's autonomy level has not been read yet, so agents treat the room as paused";
pub const NOT_MAINTAINER_REASON: &str = "only agents of the room owner or maintainers merge here";
pub const NOT_MAY_WORK_REASON: &str = "this agent may not work in this room (its owner can turn on \"May work in this room\")";

/// What a room's recorded state allows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Autonomy {
    Level(Level),
    Paused,
    /// Missing, stale or unreadable: treated as paused, and why.
    Unknown(&'static str),
}

impl Autonomy {
    /// Whether the loop may start work here (claims, hand-offs, push steps, reviews).
    pub fn may_work(&self) -> std::result::Result<(), &'static str> {
        match self {
            Self::Level(Level::Assist | Level::Autonomous) => Ok(()),
            Self::Level(Level::Off) => Err(OFF_REASON),
            Self::Paused => Err(PAUSED_REASON),
            Self::Unknown(reason) => Err(reason),
        }
    }
    /// Whether a delivery (a merge) may happen without a person.
    pub fn may_deliver(&self) -> std::result::Result<(), &'static str> {
        self.may_work()?;
        match self {
            Self::Level(Level::Autonomous) => Ok(()),
            _ => Err(ASSIST_REASON),
        }
    }
}

/// Whether `reason` is one of the loop's autonomy reasons (the hold notice's kind).
pub fn is_autonomy_reason(reason: &str) -> bool {
    [OFF_REASON, ASSIST_REASON, PAUSED_REASON, UNREAD_REASON, STALE_REASON, NOT_READ_REASON, NOT_MAY_WORK_REASON, NOT_MAINTAINER_REASON].contains(&reason)
}

/// What the owner's desktop read for one room: `level: None` is no
/// (accepted) level event, which is `Off`; `unread`: the state could not be
/// read or the level event did not count (then the room counts as paused).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AutonomySnapshot {
    pub room_id: String,
    #[serde(default)]
    pub level: Option<Level>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub unread: bool,
    /// The owner of this device's Agents is the room's owner or a maintainer
    /// (power level at least [`MAINTAINER_LEVEL`]) by the server's power
    /// levels: only their Agents deliver here.
    #[serde(default)]
    pub owner_maintainer: bool,
}

/// The power level of a room maintainer (the Matrix presets' moderator).
pub const MAINTAINER_LEVEL: i64 = 50;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS room_autonomy (room TEXT PRIMARY KEY, level TEXT NOT NULL, paused INTEGER NOT NULL CHECK(paused IN (0,1)), unread INTEGER NOT NULL CHECK(unread IN (0,1)), read_at INTEGER NOT NULL, maintainer INTEGER NOT NULL DEFAULT 0 CHECK(maintainer IN (0,1))) STRICT;
CREATE TABLE IF NOT EXISTS room_work (agent TEXT NOT NULL, room TEXT NOT NULL, PRIMARY KEY(agent,room)) STRICT;";

impl Ledger {
    /// Record what the owner's desktop read for `snapshot.room_id` at `now` (owner API only).
    pub fn set_room_autonomy(&mut self, owner: &str, snapshot: &AutonomySnapshot, now: i64) -> Result<()> {
        self.owner(owner)?;
        if !crate::issue_watch::valid_room_id(&snapshot.room_id) {
            return Err(Error::Invalid("room autonomy needs a room ID"));
        }
        let level = snapshot.level.unwrap_or_default().wire();
        self.db.execute(
            "INSERT OR REPLACE INTO room_autonomy VALUES (?,?,?,?,?,?)",
            (&snapshot.room_id, level, snapshot.paused as i64, snapshot.unread as i64, now, snapshot.owner_maintainer as i64),
        )?;
        Ok(())
    }
    /// What `room`'s recorded state allows at `now`.
    pub fn room_autonomy_at(&self, room: &str, now: i64) -> Result<Autonomy> {
        autonomy_at(&self.db, room, now)
    }
    /// What `room`'s recorded state allows now.
    pub fn room_autonomy(&self, room: &str) -> Result<Autonomy> {
        autonomy_at(&self.db, room, unix_now())
    }
    /// The owner lets `agent` work in `room` (`on`), or stops it.
    pub fn set_may_work(&mut self, owner: &str, agent: &str, room: &str, on: bool) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        if !crate::issue_watch::valid_room_id(room) {
            return Err(Error::Invalid("may-work needs a room ID"));
        }
        if on {
            self.db.execute("INSERT OR IGNORE INTO room_work VALUES (?,?)", (agent, room))?;
        } else {
            self.db.execute("DELETE FROM room_work WHERE agent=? AND room=?", (agent, room))?;
        }
        Ok(())
    }
    /// Whether the owner lets `agent` work in `room`.
    pub fn may_work(&self, agent: &str, room: &str) -> Result<bool> {
        may_work_in(&self.db, agent, room)
    }
    /// The rooms `agent` may work in, sorted.
    pub fn may_work_rooms(&self, agent: &str) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT room FROM room_work WHERE agent=? ORDER BY room")?;
        let rows = statement.query_map([agent], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// Whether `agent`'s loop may start work in `room` now ([`loop_may_work_in`]).
    pub fn loop_may_work(&self, agent: &str, room: &str) -> std::result::Result<(), &'static str> {
        loop_may_work_in(&self.db, agent, room)
    }
    /// Whether `agent` may deliver (merge) in `room` now without a person:
    /// as [`Ledger::loop_may_work`], and the room is Autonomous.
    pub fn loop_may_deliver(&self, agent: &str, room: &str) -> std::result::Result<(), &'static str> {
        loop_may_work_in(&self.db, agent, room)?;
        autonomy_at(&self.db, room, unix_now()).map_err(|_| UNREAD_REASON)?.may_deliver()?;
        match self.owner_maintainer_at(room, unix_now()) {
            Ok(Some(true)) => Ok(()),
            Ok(Some(false)) => Err(NOT_MAINTAINER_REASON),
            _ => Err(UNREAD_REASON),
        }
    }
    /// Whether a merge from `room` is refused outright: the room is a project
    /// room ([`Ledger::project_room`]) and no fresh, readable snapshot says
    /// this owner is its owner or a maintainer (fail closed, GPT review). A
    /// room this device knows nothing project-like about (a private chat)
    /// leaves it to the card.
    pub fn merge_refused(&self, room: &str, now: i64) -> Result<bool> {
        Ok(self.project_room(room)? && self.owner_maintainer_at(room, now)? != Some(true))
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

/// What `room`'s recorded state in `db` allows at `now`.
pub(crate) fn autonomy_at(db: &rusqlite::Connection, room: &str, now: i64) -> Result<Autonomy> {
    use rusqlite::OptionalExtension;
    let row: Option<(String, i64, i64, i64)> = db
        .query_row("SELECT level, paused, unread, read_at FROM room_autonomy WHERE room=?", [room], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .optional()?;
    let Some((level, paused, unread, read_at)) = row else { return Ok(Autonomy::Unknown(NOT_READ_REASON)) };
    if now.saturating_sub(read_at) > ROOM_RULES_TTL_SECS || read_at > now.saturating_add(ROOM_RULES_TTL_SECS) {
        return Ok(Autonomy::Unknown(STALE_REASON));
    }
    if unread == 1 {
        return Ok(Autonomy::Unknown(UNREAD_REASON));
    }
    if paused == 1 {
        return Ok(Autonomy::Paused);
    }
    Ok(Level::from_wire(&level).map_or(Autonomy::Unknown(UNREAD_REASON), Autonomy::Level))
}

/// Whether the owner lets `agent` work in `room`, by `db`.
pub(crate) fn may_work_in(db: &rusqlite::Connection, agent: &str, room: &str) -> Result<bool> {
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM room_work WHERE agent=? AND room=?)", (agent, room), |r| r.get(0))?)
}

/// Whether `agent`'s loop may start work in `room` now: it may work there
/// and the room is Assist or Autonomous, not paused, freshly read. Any read
/// failure is a no (fail closed).
pub(crate) fn loop_may_work_in(db: &rusqlite::Connection, agent: &str, room: &str) -> std::result::Result<(), &'static str> {
    if !may_work_in(db, agent, room).unwrap_or(false) {
        return Err(NOT_MAY_WORK_REASON);
    }
    autonomy_at(db, room, unix_now()).map_err(|_| UNREAD_REASON)?.may_work()
}

#[cfg(test)]
#[path = "room_autonomy_tests.rs"]
mod tests;
