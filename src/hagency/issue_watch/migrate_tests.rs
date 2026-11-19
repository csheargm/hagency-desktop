//! The one-time move to the room work loop (ADR 0030): the mapping.
use super::*;
use crate::hagency::room_autonomy::state::RoomState;
use serde_json::json;

fn linked(entries: &[(&str, &[&str])]) -> Rooms {
    Rooms(entries.iter().map(|(room, repos)| ((*room).to_owned(), Ok(RoomState {
        level: Ok(Level::Assist), level_by: String::new(), cleared: vec![], pause: None, linked: repos.iter().map(|r| (*r).to_owned()).collect(),
        can_change: false, can_pause: false, can_link: false, maintainer: false,
    }))).collect())
}
fn none() -> Rooms {
    Rooms::default()
}

#[test]
fn a_watch_with_push_freely_and_merges_on_becomes_autonomous_else_assist() {
    let watch = |push: bool| json!({"issueWatch": {"mode": "trusted", "repo": "acme/app", "roomId": "!r:x", "pushTrusted": push}});
    assert_eq!(planned(&watch(true), &none(), true, false).get("!r:x"), Some(&Move { repos: vec!["acme/app".into()], level: Level::Autonomous }));
    assert_eq!(planned(&watch(true), &none(), false, false)["!r:x"].level, Level::Assist, "merges were off");
    assert_eq!(planned(&watch(false), &none(), true, false)["!r:x"].level, Level::Assist, "push freely was off");
    assert!(planned(&json!({"issueWatch": {"mode": "off", "repo": "acme/app", "roomId": "!r:x"}}), &none(), true, false).is_empty(), "an off watch moves nothing");
}

#[test]
fn the_work_list_moves_once_and_trials_are_ignored() {
    let saved = json!({"issueWatch": {"mode": "off"}, "trials": [{"repo": "acme/app", "merged": 1, "required": 3}],
        "work": [{"repo": "acme/app", "roomId": "!r:x", "pushFreely": true}, {"repo": "acme/docs", "roomId": "!r:x"}, {"repo": "acme/site", "roomId": "!s:x"}]});
    let plan = planned(&saved, &none(), true, false);
    assert_eq!(plan["!r:x"], Move { repos: vec!["acme/app".into(), "acme/docs".into()], level: Level::Autonomous });
    assert_eq!(plan["!s:x"].level, Level::Assist);
    let mut moved = saved.clone();
    moved["mayWork"] = json!(["!r:x", "!s:x"]);
    let all_linked = linked(&[("!r:x", &["acme/app", "acme/docs"]), ("!s:x", &["acme/site"])]);
    assert!(planned(&moved, &all_linked, true, false).is_empty(), "moved already: idempotent");
    assert!(planned(&saved, &none(), true, true).is_empty(), "GPT review: once moved on this device, unticking every room never moves it back");
}

#[test]
fn the_room_line_says_what_moved_and_what_is_left() {
    let line = moved_line("bird", &["acme/app".into()], Some(Level::Assist));
    assert_eq!(line, "bird's issue watching moved to this room's settings: it may work on acme/app here; agent work here is Assist.");
    assert!(moved_line("bird", &["acme/app".into()], None).contains("still needs to choose Assist or Autonomous"));
}

#[test]
fn a_partial_move_plans_every_entry_not_yet_moved_whatever_may_work_holds() {
    // Batch GPT review: room A's grant saved, B's failed; or one-click setup ticked B first.
    let saved = json!({"issueWatch": {"mode": "off"}, "mayWork": ["!a:x"],
        "work": [{"repo": "acme/app", "roomId": "!a:x"}, {"repo": "acme/site", "roomId": "!b:x"}]});
    let rooms = linked(&[("!a:x", &["acme/app"]), ("!b:x", &[])]);
    let plan = planned(&saved, &rooms, false, false);
    assert_eq!(plan.keys().collect::<Vec<_>>(), ["!b:x"], "A is done, B still moves");
    let mut set_up_first = saved.clone();
    set_up_first["mayWork"] = json!(["!b:x"]);
    let plan = planned(&set_up_first, &rooms, false, false);
    assert_eq!(plan.keys().collect::<Vec<_>>(), ["!a:x", "!b:x"], "B may work but does not link acme/site yet");
    let unread = Rooms([("!a:x".to_owned(), Err("down".to_owned()))].into_iter().collect());
    assert!(planned(&saved, &unread, false, false).contains_key("!a:x"), "an unread room is not done: fail closed");
}

#[test]
fn the_old_watch_keeps_its_filter_and_start_time_in_the_work_list() {
    let watch = |mode: &str| json!({"issueWatch": {"mode": mode, "label": "agent-ok", "repo": "acme/app", "roomId": "!r:x", "enabledAt": 42}, "work": [{"repo": "acme/docs", "roomId": "!d:x"}]});
    let work = work_with_watch(&watch("all")).unwrap().expect("carried");
    assert_eq!(work.len(), 2);
    let carried: OwnerRules = serde_json::from_value(work[1].clone()).unwrap();
    assert_eq!((carried.repo.as_str(), carried.room_id.as_str(), carried.issues, carried.since), ("acme/app", "!r:x", IssueFilter::All, 42), "public issues and the backlog since switch-on stay eligible");
    let trusted: OwnerRules = serde_json::from_value(work_with_watch(&watch("trusted")).unwrap().expect("carried")[1].clone()).unwrap();
    assert_eq!(trusted.issues, IssueFilter::Trusted);
    assert_eq!(work_with_watch(&watch("labelled")).unwrap(), None, "a label watch is not carried");
    assert_eq!(work_with_watch(&json!({"issueWatch": {"mode": "off"}, "work": [{"repo": "acme/docs", "roomId": "!d:x"}]})).unwrap(), None, "GPT review: a work-list-only move rewrites no work list");
    assert!(planned(&watch("labelled"), &none(), true, true).is_empty(), "a label watch stays as it is: the work list has no label filter");
    let mut listed = watch("all");
    listed["work"] = json!([{"repo": "Acme/App", "roomId": "!r:x", "since": 7}]);
    assert_eq!(work_with_watch(&listed).unwrap(), None, "a listed repository keeps its own entry (it already governed the watch)");
}
