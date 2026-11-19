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
fn level_by(sender: &str, level: Level, cleared: &[&str]) -> Value {
    let cleared: Vec<String> = cleared.iter().map(|c| (*c).to_owned()).collect();
    json!({"type": ROOM_AUTONOMY_STATE_TYPE, "state_key": "", "sender": sender, "event_id": "$l", "room_id": ROOM, "origin_server_ts": 5, "content": level_content(level, &cleared)})
}
fn pause_by(sender: &str, id: &str) -> Value {
    json!({"type": ROOM_PAUSE_STATE_TYPE, "state_key": sender, "sender": sender, "event_id": id, "origin_server_ts": 1, "content": {}})
}
/// Maintainers (and the owner) send the level and links; members only pause.
fn may_send(sender: &str, kind: &str) -> bool {
    sender == MAINTAINER || kind == ROOM_PAUSE_STATE_TYPE
}

#[test]
fn a_maintainers_level_counts_and_a_members_does_not() {
    let events = [joined(MAINTAINER), joined(MEMBER), level_by(MAINTAINER, Level::Autonomous, &[])];
    let state = state_of(&events, ROOM, may_send, Rights::default());
    assert_eq!((state.level.clone(), state.runs()), (Ok(Level::Autonomous), Ok(())));
    let forged = [joined(MAINTAINER), joined(MEMBER), level_by(MEMBER, Level::Autonomous, &[])];
    let state = state_of(&forged, ROOM, may_send, Rights::default());
    assert!(state.level.is_err() && state.runs().is_err(), "a level the sender may not set: treated as paused");
    let agent = [joined("@_hagency_agt_x:x"), level_by("@_hagency_agt_x:x", Level::Autonomous, &[])];
    assert!(state_of(&agent, ROOM, |_, _| true, Rights::default()).level.is_err(), "an Agent never sets the level");
    let gone = [level_by(MAINTAINER, Level::Autonomous, &[])];
    assert!(state_of(&gone, ROOM, may_send, Rights::default()).level.is_err(), "the setter left the room");
    let none = state_of(&[joined(MEMBER)], ROOM, may_send, Rights::default());
    assert_eq!((none.level.clone(), none.runs().is_err()), (Ok(Level::Off), true), "no level yet: Off");
}

#[test]
fn a_members_pause_holds_until_a_maintainer_resumes() {
    let paused = [joined(MAINTAINER), joined(MEMBER), level_by(MAINTAINER, Level::Assist, &[]), pause_by(MEMBER, "$p1")];
    let state = state_of(&paused, ROOM, may_send, Rights::default());
    assert_eq!(state.pause.as_ref().map(|p| p.by.as_str()), Some(MEMBER));
    assert!(state.runs().is_err() && state.snapshot(ROOM).paused);
    let resumed = [joined(MAINTAINER), joined(MEMBER), level_by(MAINTAINER, Level::Assist, &["$p1"]), pause_by(MEMBER, "$p1")];
    assert_eq!(state_of(&resumed, ROOM, may_send, Rights::default()).runs(), Ok(()), "the Resume cleared that pause");
    // A member's own "resume" is a level event they may not send: the room stays paused.
    let member_resume = [joined(MAINTAINER), joined(MEMBER), level_by(MEMBER, Level::Assist, &["$p2"]), pause_by(MEMBER, "$p2")];
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

/// On-screen bug (Nov 18): the Status heading named the pauser by local part;
/// it takes their display name from the room's member events.
#[test]
fn the_pauser_is_named_by_their_display_name() {
    let named = json!({"type": "m.room.member", "state_key": MEMBER, "sender": MEMBER, "content": {"membership": "join", "displayname": "Anna"}});
    let state = state_of(&[named, pause_by(MEMBER, "$p")], ROOM, may_send, Rights::default());
    assert_eq!(state.pause_name.as_deref(), Some("Anna"));
    let state = state_of(&[joined(MEMBER), pause_by(MEMBER, "$p")], ROOM, may_send, Rights::default());
    assert!(state.pause.is_some());
    assert_eq!(state.pause_name, None, "no display name: the caller falls back to the local part");
}

/// GPT review round 3 (P1): a snapshot's source time comes from the level,
/// pause and power-level events only; a membership change (or a future-dated
/// federated one) never moves it.
#[test]
fn a_membership_only_change_does_not_move_as_of() {
    let at = |mut e: serde_json::Value, ts: i64| { e["origin_server_ts"] = ts.into(); e };
    let base = [at(joined(MEMBER), 100), at(pause_by(MEMBER, "$p"), 200)];
    let before = state_of(&base, ROOM, may_send, Rights::default());
    assert_eq!(before.as_of, Some(200));
    let later_member = at(joined(MAINTAINER), 9_999_999_999_999);
    let after = state_of(&[base[0].clone(), base[1].clone(), later_member], ROOM, may_send, Rights::default());
    assert_eq!(after.as_of, Some(200), "a membership event never moves as_of");
}
