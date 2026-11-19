//! What a room's server state says about its work loop (ADR 0030), without
//! I/O: the accepted level event (from a joined person who may send it now,
//! never an Agent or Hagency service account), the pause in force, the
//! linked repositories, and what this account may do. Absent level: Off.
//! A level event that does not count makes the loop treat the room as
//! paused (fail closed); a pause never needs to be accepted to count.
use crate::hagency::issue_watch::room_rules::{MEMBER_EVENT, RoomRead, accept_of};
use ::hagency::issue_watch::safeguards::room_autonomy::{
    AutonomySnapshot, Level, Pause, ROOM_AUTONOMY_STATE_TYPE, UNREAD_REASON, cleared, level_of, pause_in,
};
use ::hagency::repo_status::linked::{ROOM_REPOS_STATE_TYPE, linked};
use serde_json::Value;

/// One room's loop state as this account read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomState {
    /// The level, or why the level event does not count (then: paused).
    pub level: Result<Level, String>,
    /// Who set the level (empty: never set).
    pub level_by: String,
    /// The pause event IDs the accepted level event's last Resume cleared.
    pub cleared: Vec<String>,
    pub pause: Option<Pause>,
    /// The display name of whoever paused, from their room membership (none set: `None`).
    pub pause_name: Option<String>,
    /// The room's linked repositories, from a sender who may still link them.
    pub linked: Vec<String>,
    /// This account may change the level and resume (owner or maintainer).
    pub can_change: bool,
    /// This account may pause (any member, once the room allows it).
    pub can_pause: bool,
    /// This account may link repositories.
    pub can_link: bool,
    /// This account is the room's owner or a maintainer (its Agents deliver).
    pub maintainer: bool,
    /// The newest server time (ms) among the state events read: the gate
    /// keeps the newest snapshot by it, never by arrival (GPT review P1).
    pub as_of: Option<i64>,
}

/// What this account may do in a room, by the server's power levels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rights {
    pub change_level: bool,
    pub pause: bool,
    pub link: bool,
    /// The room's owner or a maintainer (power level at least `MAINTAINER_LEVEL`).
    pub maintainer: bool,
}

impl RoomState {
    /// The state as the owner API records it.
    pub fn snapshot(&self, room_id: &str) -> AutonomySnapshot {
        AutonomySnapshot { room_id: room_id.to_owned(), level: self.level.as_ref().ok().copied(), paused: self.pause.is_some(), unread: self.level.is_err(), owner_maintainer: self.maintainer, as_of: self.as_of, sticky: false }
    }
    /// Whether the loop may start work here now (Assist/Autonomous, not paused).
    pub fn runs(&self) -> Result<(), String> {
        match &self.level {
            Err(reason) => Err(reason.clone()),
            Ok(_) if self.pause.is_some() => Err(::hagency::issue_watch::safeguards::room_autonomy::PAUSED_REASON.to_owned()),
            Ok(Level::Off) => Err(::hagency::issue_watch::safeguards::room_autonomy::OFF_REASON.to_owned()),
            Ok(_) => Ok(()),
        }
    }
}

/// The room state among the server's state `events` of `room_id`.
/// `may_send(sender, type)`: whether the room's power levels let that
/// person send that state type now (for the level: and they are the room's
/// owner or a maintainer); `me`'s rights come from the same levels.
pub fn state_of(events: &[Value], room_id: &str, may_send: impl Fn(&str, &str) -> bool, rights: Rights) -> RoomState {
    let read = accept_of(events, room_id, ROOM_AUTONOMY_STATE_TYPE, |sender| may_send(sender, ROOM_AUTONOMY_STATE_TYPE));
    let (level, level_by, resumed) = match &read {
        RoomRead::None => (Ok(Level::Off), String::new(), Vec::new()),
        RoomRead::Refused(why) => (Err(format!("{UNREAD_REASON} (the level was {why})")), String::new(), Vec::new()),
        RoomRead::Rules { sender, content, .. } => (level_of(content).ok_or_else(|| UNREAD_REASON.to_owned()), sender.clone(), cleared(content)),
    };
    let linked = match accept_of(events, room_id, ROOM_REPOS_STATE_TYPE, |sender| may_send(sender, ROOM_REPOS_STATE_TYPE)) {
        RoomRead::Rules { content, .. } => linked(&content),
        RoomRead::None | RoomRead::Refused(_) => Vec::new(),
    };
    let pause = pause_in(events, &resumed);
    RoomState {
        level,
        level_by,
        pause_name: pause.as_ref().and_then(|p| member_name(events, &p.by)),
        pause,
        cleared: resumed,
        linked,
        // The event permission alone is not the role (GPT review: a permissive
        // room could let a plain member send it): owner or maintainer too.
        can_change: rights.change_level && rights.maintainer,
        can_pause: rights.pause,
        can_link: rights.link,
        maintainer: rights.maintainer,
        as_of: as_of(events, &read),
    }
}

/// A snapshot's source time: the newest server time among the events the
/// gate honours — the accepted level event (`level`), the pauses keyed by
/// their own sender, and the room's power levels. Never membership, and never
/// an event the gate ignores (GPT review rounds 3–4: a future-dated one must
/// not stamp a snapshot).
pub fn as_of(events: &[Value], level: &RoomRead) -> Option<i64> {
    let ts_of = |e: &Value| e["origin_server_ts"].as_i64();
    let level_ts = match level {
        RoomRead::Rules { event_id, .. } => events.iter().find(|e| e["event_id"] == event_id.as_str()).and_then(ts_of),
        RoomRead::None | RoomRead::Refused(_) => None,
    };
    let power_ts = events.iter().find(|e| e["type"] == crate::hagency::issue_watch::room_rules::POWER_LEVELS_EVENT && e["state_key"] == "").and_then(ts_of);
    let pause_ts = ::hagency::issue_watch::safeguards::room_autonomy::pauses(events).map(|(_, _, ts)| ts).max();
    [level_ts, power_ts, pause_ts].into_iter().flatten().max()
}

/// `user`'s display name in the room's member `events`, when they set one.
pub fn member_name(events: &[Value], user: &str) -> Option<String> {
    let member = events.iter().find(|e| e["type"] == MEMBER_EVENT && e["state_key"] == user)?;
    member["content"]["displayname"].as_str().map(str::trim).filter(|n| !n.is_empty()).map(str::to_owned)
}
