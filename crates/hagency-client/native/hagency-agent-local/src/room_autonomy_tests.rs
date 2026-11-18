//! Room autonomy (ADR 0030): the level and Pause as recorded by the owner,
//! read fail-closed, and the per-Agent "may work in this room".
use super::*;
use serde_json::json;

const OWNER: &str = "@owner:example.org";
const ROOM: &str = "!r:x";

fn ledger(dir: &std::path::Path) -> Ledger {
    Ledger::open(dir.join("local.db"), OWNER).unwrap()
}
fn record(ledger: &mut Ledger, level: Option<Level>, paused: bool, unread: bool, at: i64) {
    ledger.set_room_autonomy(OWNER, &AutonomySnapshot { room_id: ROOM.into(), level, paused, unread, owner_maintainer: true }, at).unwrap();
}

impl Ledger {
    /// Test helper: `agent` may work in `room`, recorded Autonomous now.
    pub(crate) fn allow_loop_in(&mut self, owner: &str, agent: &str, room: &str) {
        self.set_may_work(owner, agent, room, true).unwrap();
        let snapshot = AutonomySnapshot { room_id: room.into(), level: Some(Level::Autonomous), paused: false, unread: false, owner_maintainer: true };
        self.set_room_autonomy(owner, &snapshot, unix_now()).unwrap();
    }
    /// Test helper: `agent` may work in every room it is bound to, each
    /// recorded as Autonomous now (`on`), or may work nowhere.
    pub(crate) fn allow_loop(&mut self, owner: &str, agent: &str, on: bool) {
        let rooms: Vec<String> = {
            let mut statement = self.db.prepare("SELECT DISTINCT room FROM bindings WHERE agent=?").unwrap();
            statement.query_map([agent], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        for room in rooms {
            self.set_may_work(owner, agent, &room, on).unwrap();
            let snapshot = AutonomySnapshot { room_id: room, level: Some(Level::Autonomous), paused: false, unread: false, owner_maintainer: true };
            self.set_room_autonomy(owner, &snapshot, unix_now()).unwrap();
        }
    }
}

#[test]
fn each_level_allows_its_steps_and_pause_allows_none() {
    let cases = [
        (Autonomy::Level(Level::Off), Err(OFF_REASON), Err(OFF_REASON)),
        (Autonomy::Level(Level::Assist), Ok(()), Err(ASSIST_REASON)),
        (Autonomy::Level(Level::Autonomous), Ok(()), Ok(())),
        (Autonomy::Paused, Err(PAUSED_REASON), Err(PAUSED_REASON)),
        (Autonomy::Unknown(UNREAD_REASON), Err(UNREAD_REASON), Err(UNREAD_REASON)),
    ];
    for (autonomy, work, deliver) in cases {
        assert_eq!((autonomy.may_work(), autonomy.may_deliver()), (work, deliver), "{autonomy:?}");
    }
    assert_eq!(ASSIST_REASON, "room is in Assist — a person delivers");
}

#[test]
fn missing_stale_or_unreadable_state_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger(temp.path());
    let now = unix_now();
    assert_eq!(ledger.room_autonomy_at(ROOM, now).unwrap(), Autonomy::Unknown(NOT_READ_REASON));
    record(&mut ledger, Some(Level::Autonomous), false, false, now - ROOM_RULES_TTL_SECS - 1);
    assert_eq!(ledger.room_autonomy_at(ROOM, now).unwrap(), Autonomy::Unknown(STALE_REASON));
    record(&mut ledger, None, false, true, now);
    assert_eq!(ledger.room_autonomy_at(ROOM, now).unwrap(), Autonomy::Unknown(UNREAD_REASON));
    record(&mut ledger, None, false, false, now);
    assert_eq!(ledger.room_autonomy_at(ROOM, now).unwrap(), Autonomy::Level(Level::Off), "no level event: Off");
    record(&mut ledger, Some(Level::Autonomous), true, false, now);
    assert_eq!(ledger.room_autonomy_at(ROOM, now).unwrap(), Autonomy::Paused);
    assert!(ledger.set_room_autonomy("@intruder:x", &AutonomySnapshot { room_id: ROOM.into(), ..Default::default() }, now).is_err(), "only the owner");
    assert!(ledger.set_room_autonomy(OWNER, &AutonomySnapshot { room_id: "room".into(), ..Default::default() }, now).is_err());
}

#[test]
fn may_work_is_per_agent_and_room_and_needs_a_running_level() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger(temp.path());
    record(&mut ledger, Some(Level::Assist), false, false, unix_now());
    assert_eq!(ledger.loop_may_work("anna", ROOM), Err(NOT_MAY_WORK_REASON));
    ledger.set_may_work(OWNER, "anna", ROOM, true).unwrap();
    assert_eq!(ledger.loop_may_work("anna", ROOM), Ok(()));
    assert_eq!(ledger.loop_may_work("bob", ROOM), Err(NOT_MAY_WORK_REASON), "another agent");
    assert_eq!(ledger.may_work_rooms("anna").unwrap(), vec![ROOM.to_owned()]);
    record(&mut ledger, Some(Level::Assist), true, false, unix_now());
    assert_eq!(ledger.loop_may_work("anna", ROOM), Err(PAUSED_REASON));
    ledger.set_may_work(OWNER, "anna", ROOM, false).unwrap();
    assert!(ledger.may_work_rooms("anna").unwrap().is_empty());
    assert!(ledger.set_may_work("@intruder:x", "anna", ROOM, true).is_err(), "only the owner");
}

fn pause(sender: &str, key: &str, ts: i64, content: serde_json::Value) -> serde_json::Value {
    json!({"type": ROOM_PAUSE_STATE_TYPE, "state_key": key, "sender": sender, "origin_server_ts": ts, "content": content})
}

/// A member's edit or redaction of their own pause is a newer event, so it
/// still pauses: only an admin's Resume (a later `resumedAt`) ends it.
#[test]
fn a_member_can_pause_but_not_resume() {
    let paused = vec![pause("@m:x", "@m:x", 100, pause_content(100))];
    assert_eq!(pause_in(&paused, 0), Some(Pause { by: "@m:x".into(), at_ms: 100 }));
    let edited = vec![pause("@m:x", "@m:x", 200, json!({}))];
    assert!(pause_in(&edited, 0).is_some(), "an emptied pause still counts");
    assert!(pause_in(&edited, resume_point(&edited, 0)).is_none(), "the admin's Resume clears what it saw");
    let later = vec![edited[0].clone(), pause("@n:x", "@n:x", 300, json!({}))];
    assert_eq!(pause_in(&later, 200).map(|p| p.by), Some("@n:x".into()), "a pause after the Resume counts");
    assert!(pause_in(&[pause("@m:x", "@other:x", 500, json!({}))], 0).is_none(), "a key not its sender's is ignored");
    assert!(pause_in(&[pause("@m:x", "", 500, json!({}))], 0).is_none());
}

#[test]
fn level_content_round_trips_and_keeps_the_resume_point() {
    let content = level_content(Level::Assist, 42);
    assert_eq!((level_of(&content), resumed_at(&content)), (Some(Level::Assist), 42));
    assert_eq!(level_of(&json!({"level": "yolo"})), None);
    assert_eq!(resumed_at(&json!({})), 0);
    for level in Level::ALL {
        assert_eq!(Level::from_wire(level.wire()), Some(level));
    }
    assert!(is_autonomy_reason(ASSIST_REASON) && !is_autonomy_reason("code"));
}

/// Delivery follows the owner's room role (owner decision, Nov 18): a plain
/// member's Agents work but never merge, and a demotion ends it at the next read.
#[test]
fn only_agents_of_the_room_owner_or_maintainers_deliver() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger(temp.path());
    ledger.set_may_work(OWNER, "anna", ROOM, true).unwrap();
    let snapshot = |maintainer| AutonomySnapshot { room_id: ROOM.into(), level: Some(Level::Autonomous), paused: false, unread: false, owner_maintainer: maintainer };
    ledger.set_room_autonomy(OWNER, &snapshot(true), unix_now()).unwrap();
    assert_eq!(ledger.loop_may_deliver("anna", ROOM), Ok(()));
    ledger.set_room_autonomy(OWNER, &snapshot(false), unix_now()).unwrap();
    assert_eq!(ledger.loop_may_deliver("anna", ROOM), Err(NOT_MAINTAINER_REASON), "demoted: no merges");
    assert_eq!(ledger.loop_may_work("anna", ROOM), Ok(()), "a member's agent still works");
    ledger.set_room_autonomy(OWNER, &snapshot(true), unix_now() - ROOM_RULES_TTL_SECS - 1).unwrap();
    assert_eq!(ledger.owner_maintainer_at(ROOM, unix_now()).unwrap(), None, "a stale role counts for nothing");
}
