//! The room's loop state read from server events (ADR 0030): fail closed,
//! members pause, only owner and maintainers set the level.
use super::state::{Rights, state_of};
use ::hagency::issue_watch::safeguards::room_autonomy::{Level, ROOM_AUTONOMY_STATE_TYPE, ROOM_PAUSE_STATE_TYPE, level_content};
use ::hagency::repo_status::linked::ROOM_REPOS_STATE_TYPE;
use serde_json::{Value, json};

const ROOM: &str = "!r:x";
const MAINTAINER: &str = "@mo:x";
const MEMBER: &str = "@me:x";

fn joined(user: &str) -> Value {
    json!({"type": "m.room.member", "state_key": user, "sender": user, "content": {"membership": "join"}})
}
fn level_by(sender: &str, level: Level, resumed: i64) -> Value {
    json!({"type": ROOM_AUTONOMY_STATE_TYPE, "state_key": "", "sender": sender, "event_id": "$l", "room_id": ROOM, "origin_server_ts": 5, "content": level_content(level, resumed)})
}
fn pause_by(sender: &str, ts: i64) -> Value {
    json!({"type": ROOM_PAUSE_STATE_TYPE, "state_key": sender, "sender": sender, "event_id": "$p", "origin_server_ts": ts, "content": {}})
}
/// Maintainers (and the owner) send the level and links; members only pause.
fn may_send(sender: &str, kind: &str) -> bool {
    sender == MAINTAINER || kind == ROOM_PAUSE_STATE_TYPE
}

#[test]
fn a_maintainers_level_counts_and_a_members_does_not() {
    let events = [joined(MAINTAINER), joined(MEMBER), level_by(MAINTAINER, Level::Autonomous, 0)];
    let state = state_of(&events, ROOM, may_send, Rights::default());
    assert_eq!((state.level.clone(), state.runs()), (Ok(Level::Autonomous), Ok(())));
    let forged = [joined(MAINTAINER), joined(MEMBER), level_by(MEMBER, Level::Autonomous, 0)];
    let state = state_of(&forged, ROOM, may_send, Rights::default());
    assert!(state.level.is_err() && state.runs().is_err(), "a level the sender may not set: treated as paused");
    let agent = [joined("@_hagency_agt_x:x"), level_by("@_hagency_agt_x:x", Level::Autonomous, 0)];
    assert!(state_of(&agent, ROOM, |_, _| true, Rights::default()).level.is_err(), "an Agent never sets the level");
    let gone = [level_by(MAINTAINER, Level::Autonomous, 0)];
    assert!(state_of(&gone, ROOM, may_send, Rights::default()).level.is_err(), "the setter left the room");
    let none = state_of(&[joined(MEMBER)], ROOM, may_send, Rights::default());
    assert_eq!((none.level.clone(), none.runs().is_err()), (Ok(Level::Off), true), "no level yet: Off");
}

#[test]
fn a_members_pause_holds_until_a_maintainer_resumes() {
    let paused = [joined(MAINTAINER), joined(MEMBER), level_by(MAINTAINER, Level::Assist, 0), pause_by(MEMBER, 100)];
    let state = state_of(&paused, ROOM, may_send, Rights::default());
    assert_eq!(state.pause.as_ref().map(|p| p.by.as_str()), Some(MEMBER));
    assert!(state.runs().is_err() && state.snapshot(ROOM).paused);
    let resumed = [joined(MAINTAINER), joined(MEMBER), level_by(MAINTAINER, Level::Assist, 100), pause_by(MEMBER, 100)];
    assert_eq!(state_of(&resumed, ROOM, may_send, Rights::default()).runs(), Ok(()), "the Resume cleared that pause");
    // A member's own "resume" is a level event they may not send: the room stays paused.
    let member_resume = [joined(MAINTAINER), joined(MEMBER), level_by(MEMBER, Level::Assist, 200), pause_by(MEMBER, 150)];
    assert!(state_of(&member_resume, ROOM, may_send, Rights::default()).runs().is_err());
}

#[test]
fn linked_repos_count_only_from_someone_who_may_link_and_rights_carry_through() {
    let link = |sender: &str| json!({"type": ROOM_REPOS_STATE_TYPE, "state_key": "", "sender": sender, "event_id": "$r", "room_id": ROOM, "content": {"repos": ["acme/app"]}});
    let ok = state_of(&[joined(MAINTAINER), link(MAINTAINER)], ROOM, may_send, Rights { maintainer: true, ..Rights::default() });
    assert_eq!(ok.linked, vec!["acme/app".to_owned()]);
    assert!(ok.maintainer && ok.snapshot(ROOM).owner_maintainer);
    let bad = state_of(&[joined(MEMBER), link(MEMBER)], ROOM, may_send, Rights::default());
    assert!(bad.linked.is_empty() && !bad.snapshot(ROOM).owner_maintainer);
}
