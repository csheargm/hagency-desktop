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
/// time becomes the saved event's own server time (`saved_ts`), or just past
/// the read's when that is unknown, so a poll that read the room before the
/// write never replaces it at the gate (GPT review P1). Neither known: untimed,
/// which the gate takes only as stricter, keeping its stored time.
pub fn pushed_snapshot(room_id: &str, read: &Result<RoomState, String>, saved: Saved, saved_ts: Option<i64>) -> AutonomySnapshot {
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
        snapshot.as_of = saved_ts.max(snapshot.as_of.map(|t| t.saturating_add(1)));
    }
    snapshot
}

/// The event ID of the state event a change just saved (set by its write).
#[derive(Default)]
pub struct Sent(std::sync::OnceLock<matrix_sdk::ruma::OwnedEventId>);

impl Sent {
    /// Records the saved event (one write per change, so the first is the one).
    pub fn record(&self, id: matrix_sdk::ruma::OwnedEventId) {
        // Deliberately ignored: a change writes once; a second ID cannot occur.
        let _ = self.0.set(id);
    }
}

/// The server time of the event `sent` names, when it can be read.
async fn sent_ts(room: &Room, sent: &Sent) -> Option<i64> {
    let event = room.load_or_fetch_event(sent.0.get()?, None).await.ok()?;
    serde_json::from_str::<serde_json::Value>(event.raw().json().get()).ok()?["origin_server_ts"].as_i64()
}

/// Push `room`'s snapshot after `saved` (its event in `sent`) to every Agent
/// of this account this device runs (the gate that merges for them), read
/// from the homeserver now.
pub async fn push(room: &Room, saved: Saved, sent: &Sent) -> Result<(), String> {
    use crate::hagency::{backend::Service, model::Command};
    let read = super::server::read(room).await;
    let change = serde_json::json!({"roomAutonomy": [pushed_snapshot(room.room_id().as_str(), &read, saved, sent_ts(room, sent).await)]});
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
        let pushed = pushed_snapshot("!r:x", &read, Saved::Paused, Some(5_000));
        assert!(pushed.paused, "the gate sees the pause now");
        let polled = read.as_ref().unwrap().snapshot("!r:x");
        assert!(pushed.as_of > polled.as_of, "a poll that read the room before the pause is older at the gate");
        assert_eq!(pushed.as_of, Some(5_000), "the saved pause event's own server time");
        assert_eq!(pushed_snapshot("!r:x", &read, Saved::Paused, None).as_of, Some(1_001), "unknown: just past the read");
        assert_eq!(pushed.level, Some(Level::Autonomous));
    }

    /// A saved Off (or Assist) is never loosened by a read that missed it; a
    /// saved Autonomous never tightens past what the server shows.
    #[test]
    fn a_saved_level_only_ever_makes_the_gate_stricter() {
        let read = autonomous_read();
        assert_eq!(pushed_snapshot("!r:x", &read, Saved::Level(Level::Off), None).level, Some(Level::Off));
        assert_eq!(pushed_snapshot("!r:x", &read, Saved::Level(Level::Assist), None).level, Some(Level::Assist));
        let mut assist = read.clone().unwrap();
        assist.level = Ok(Level::Assist);
        assert_eq!(pushed_snapshot("!r:x", &Ok(assist), Saved::Level(Level::Autonomous), None).level, Some(Level::Assist), "a stale read keeps the stricter level");
        let unread = pushed_snapshot("!r:x", &Err("no".into()), Saved::Resumed, None);
        assert!(unread.unread, "an unreadable room is unread at the gate (fail closed)");
    }
}
