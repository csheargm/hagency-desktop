//! The merge gate's copy of a room's level and Pause, pushed the moment a
//! change is saved (full-integration GPT review P1): the gate otherwise kept
//! the poller's last Autonomous snapshot until its next check, so a merge
//! notice sent just before Pause could still merge after the homeserver took
//! the Pause. The snapshot is read from the homeserver now; what this desktop
//! just saved only ever makes it stricter, so a read that missed the write
//! never loosens the gate.
use super::state::RoomState;
use ::hagency::issue_watch::safeguards::room_autonomy::{AutonomySnapshot, Level};
use matrix_sdk::Room;

/// What this desktop just saved in a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Saved {
    Level(Level),
    Paused,
    Resumed,
}

/// How far `level` lets agents go (Off < Assist < Autonomous).
fn reach(level: Level) -> usize {
    Level::ALL.iter().position(|l| *l == level).unwrap_or_default()
}

/// The snapshot the gate gets for `room_id` after `saved`: the fresh `read`
/// (unreadable: unread, fail closed), never looser than what was saved. When
/// the read missed the write (the saved change made it stricter), its source
/// time moves past the read's and to `now_ms`, so a poll that read the room
/// before the write never replaces it at the gate (GPT review P1).
pub fn pushed_snapshot(room_id: &str, read: &Result<RoomState, String>, saved: Saved, now_ms: i64) -> AutonomySnapshot {
    let mut snapshot = match read {
        Ok(state) => state.snapshot(room_id),
        Err(_) => AutonomySnapshot { room_id: room_id.to_owned(), unread: true, ..Default::default() },
    };
    let as_read = snapshot.clone();
    match saved {
        Saved::Paused => snapshot.paused = true,
        Saved::Level(level) => snapshot.level = Some(snapshot.level.map_or(level, |read| if reach(read) < reach(level) { read } else { level })),
        Saved::Resumed => {}
    }
    if snapshot != as_read {
        snapshot.as_of = Some(snapshot.as_of.map_or(now_ms, |t| t.saturating_add(1).max(now_ms)));
    }
    snapshot
}

/// Push `room`'s snapshot after `saved` to every Agent of this account this
/// device runs (the gate that merges for them), read from the homeserver now.
pub async fn push(room: &Room, saved: Saved) -> Result<(), String> {
    use crate::hagency::{backend::Service, model::Command};
    let read = super::server::read(room).await;
    let change = serde_json::json!({"roomAutonomy": [pushed_snapshot(room.room_id().as_str(), &read, saved, chrono::Utc::now().timestamp_millis())]});
    let service = Service::for_current()?;
    let listed = service.execute(Command::Agents).await?;
    for id in crate::hagency::kill_switch::here(&listed, &service.execute(Command::Devices).await?) {
        crate::hagency::issue_watch::live::saved(&service, &id, Some(change.clone())).await?;
    }
    Ok(())
}

/// The note when the gate could not be told now (the poller tells it on its next check).
pub fn not_pushed(error: String) -> String {
    crate::i18n::format("The change is saved, but your agents learn of it only on their next check: {error}", &[("error", error)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::room_autonomy::state::{Rights, state_of};
    use serde_json::json;

    fn autonomous_read() -> Result<RoomState, String> {
        use ::hagency::issue_watch::safeguards::room_autonomy::{ROOM_AUTONOMY_STATE_TYPE, level_content};
        let level = json!({"type": ROOM_AUTONOMY_STATE_TYPE, "state_key": "", "sender": "@own:x", "event_id": "$l", "origin_server_ts": 1000, "content": level_content(Level::Autonomous, &[])});
        let member = json!({"type": "m.room.member", "state_key": "@own:x", "sender": "@own:x", "event_id": "$m", "content": {"membership": "join"}});
        Ok(state_of(&[level, member], "!r:x", |_, _| true, Rights { change_level: true, pause: true, link: true, maintainer: true }))
    }

    /// GPT review (P1): a saved Pause reaches the gate as paused even when the
    /// read right after it still shows the room running Autonomous.
    #[test]
    fn a_saved_pause_makes_the_gate_refuse_a_queued_merge() {
        let read = autonomous_read();
        assert_eq!(read.as_ref().unwrap().level, Ok(Level::Autonomous), "the stale read still says Autonomous");
        let pushed = pushed_snapshot("!r:x", &read, Saved::Paused, 0);
        assert!(pushed.paused, "the gate sees the pause now");
        let polled = read.as_ref().unwrap().snapshot("!r:x");
        assert!(pushed.as_of > polled.as_of, "a poll that read the room before the pause is older at the gate");
        assert_eq!(pushed.level, Some(Level::Autonomous));
    }

    /// A saved Off (or Assist) is never loosened by a read that missed it; a
    /// saved Autonomous never tightens past what the server shows.
    #[test]
    fn a_saved_level_only_ever_makes_the_gate_stricter() {
        let read = autonomous_read();
        assert_eq!(pushed_snapshot("!r:x", &read, Saved::Level(Level::Off), 0).level, Some(Level::Off));
        assert_eq!(pushed_snapshot("!r:x", &read, Saved::Level(Level::Assist), 0).level, Some(Level::Assist));
        let mut assist = read.clone().unwrap();
        assist.level = Ok(Level::Assist);
        assert_eq!(pushed_snapshot("!r:x", &Ok(assist), Saved::Level(Level::Autonomous), 0).level, Some(Level::Assist), "a stale read keeps the stricter level");
        let unread = pushed_snapshot("!r:x", &Err("no".into()), Saved::Resumed, 0);
        assert!(unread.unread, "an unreadable room is unread at the gate (fail closed)");
    }
}
