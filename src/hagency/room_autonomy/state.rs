//! What a room's server state says about its work loop (ADR 0030), without
//! I/O: the accepted level event (from a joined person who may send it now,
//! never an Agent or Hagency service account), the pause in force, the
//! linked repositories, and what this account may do. Absent level: Off.
//! A level event that does not count makes the loop treat the room as
//! paused (fail closed); a pause never needs to be accepted to count.
use crate::hagency::issue_watch::room_rules::{RoomRead, accept_of};
use ::hagency::issue_watch::safeguards::room_autonomy::{
    AutonomySnapshot, Level, Pause, ROOM_AUTONOMY_STATE_TYPE, UNREAD_REASON, level_of, pause_in, resumed_at,
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
    /// The Resume point of the accepted level event (server ms).
    pub resumed_at: i64,
    pub pause: Option<Pause>,
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
        AutonomySnapshot { room_id: room_id.to_owned(), level: self.level.as_ref().ok().copied(), paused: self.pause.is_some(), unread: self.level.is_err(), owner_maintainer: self.maintainer }
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
/// person send that state type now; `me`'s rights come from the same levels.
pub fn state_of(events: &[Value], room_id: &str, may_send: impl Fn(&str, &str) -> bool, rights: Rights) -> RoomState {
    let read = accept_of(events, room_id, ROOM_AUTONOMY_STATE_TYPE, |sender| may_send(sender, ROOM_AUTONOMY_STATE_TYPE));
    let (level, level_by, resumed) = match &read {
        RoomRead::None => (Ok(Level::Off), String::new(), 0),
        RoomRead::Refused(why) => (Err(format!("{UNREAD_REASON} (the level was {why})")), String::new(), 0),
        RoomRead::Rules { sender, content, .. } => (level_of(content).ok_or_else(|| UNREAD_REASON.to_owned()), sender.clone(), resumed_at(content)),
    };
    let linked = match accept_of(events, room_id, ROOM_REPOS_STATE_TYPE, |sender| may_send(sender, ROOM_REPOS_STATE_TYPE)) {
        RoomRead::Rules { content, .. } => linked(&content),
        RoomRead::None | RoomRead::Refused(_) => Vec::new(),
    };
    RoomState {
        level,
        level_by,
        resumed_at: resumed,
        pause: pause_in(events, resumed),
        linked,
        can_change: rights.change_level,
        can_pause: rights.pause,
        can_link: rights.link,
        maintainer: rights.maintainer,
    }
}
