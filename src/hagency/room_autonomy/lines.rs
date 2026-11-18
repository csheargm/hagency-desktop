//! The words the room's work loop controls show (ADR 0030): the level
//! choices, the Status line, the header badge and the room lines Pause and
//! Resume post. English keys, translated on use; room lines are room content
//! and stay English, like the loop's other notices.
use super::state::RoomState;
use ::hagency::issue_watch::safeguards::room_autonomy::Level;

/// The level choices in [`Level::ALL`] order.
pub const LEVEL_LABELS: [&str; 3] = [
    "Off — agents only answer when asked",
    "Assist — agents work, a person merges",
    "Autonomous — agents merge where the room rules allow",
];
/// Under the level, for everyone.
pub const WHO_CAN_CHANGE: &str = "Owner and maintainers can change this. Anyone in the room can pause.";

/// The short name of `level`.
pub fn level_name(level: Level) -> &'static str {
    match level {
        Level::Off => "Off",
        Level::Assist => "Assist",
        Level::Autonomous => "Autonomous",
    }
}

/// The level choice's position.
pub fn level_index(level: Level) -> usize {
    Level::ALL.iter().position(|l| *l == level).unwrap_or(0)
}

/// The Status tab's line: "Autonomous · 2 agents working", "Paused by Anna
/// at 13:40", or why the loop treats the room as paused. `name` and `time`
/// render a Matrix ID and a server time (ms).
pub fn status_line(state: &RoomState, working: usize, name: impl Fn(&str) -> String, time: impl Fn(i64) -> String) -> String {
    use crate::i18n::{format, tr};
    if let Some(pause) = &state.pause {
        return format("Paused by {name} at {time}", &[("name", name(&pause.by)), ("time", time(pause.at_ms))]);
    }
    let level = match &state.level {
        Ok(level) => *level,
        Err(reason) => return format("Agent work is on hold: {reason}", &[("reason", reason.clone())]),
    };
    let working = match working {
        0 => tr("no agent working now").to_owned(),
        1 => tr("1 agent working").to_owned(),
        n => format("{count} agents working", &[("count", n.to_string())]),
    };
    match level {
        Level::Off => tr("Off · agents only answer when asked").to_owned(),
        _ => format("{level} · {working}", &[("level", tr(level_name(level)).to_owned()), ("working", working)]),
    }
}

/// The header badge: shown while the room runs a loop or is paused.
pub fn badge(state: &RoomState) -> Option<&'static str> {
    if state.pause.is_some() || state.level.is_err() {
        return Some("Paused");
    }
    match state.level {
        Ok(Level::Assist) => Some("Assist"),
        Ok(Level::Autonomous) => Some("Autonomous"),
        _ => None,
    }
}

/// The room line Pause posts.
pub fn paused_line(name: &str, time: &str) -> String {
    format!("⏸ Paused by {name} at {time}")
}
/// The room line Resume posts.
pub fn resumed_line(name: &str, time: &str) -> String {
    format!("▶ Resumed by {name} at {time}")
}
/// The room line a level change posts.
pub fn level_line(name: &str, level: Level) -> String {
    let what = match level {
        Level::Off => "Off: agents only answer when asked",
        Level::Assist => "Assist: agents claim, build and review; a person merges",
        Level::Autonomous => "Autonomous: agents merge on their own where the room rules allow",
    };
    format!("{name} set agent work in this room to {what}.")
}
