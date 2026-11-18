//! The one-time move to the room work loop (ADR 0030): the mapping.
use super::*;
use serde_json::json;

#[test]
fn a_watch_with_push_freely_and_merges_on_becomes_autonomous_else_assist() {
    let watch = |push: bool| json!({"issueWatch": {"mode": "trusted", "repo": "acme/app", "roomId": "!r:x", "pushTrusted": push}});
    assert_eq!(planned(&watch(true), true).get("!r:x"), Some(&Move { repos: vec!["acme/app".into()], level: Level::Autonomous }));
    assert_eq!(planned(&watch(true), false)["!r:x"].level, Level::Assist, "merges were off");
    assert_eq!(planned(&watch(false), true)["!r:x"].level, Level::Assist, "push freely was off");
    assert!(planned(&json!({"issueWatch": {"mode": "off", "repo": "acme/app", "roomId": "!r:x"}}), true).is_empty(), "an off watch moves nothing");
}

#[test]
fn the_work_list_moves_once_and_trials_are_ignored() {
    let saved = json!({"issueWatch": {"mode": "off"}, "trials": [{"repo": "acme/app", "merged": 1, "required": 3}],
        "work": [{"repo": "acme/app", "roomId": "!r:x", "pushFreely": true}, {"repo": "acme/docs", "roomId": "!r:x"}, {"repo": "acme/site", "roomId": "!s:x"}]});
    let plan = planned(&saved, true);
    assert_eq!(plan["!r:x"], Move { repos: vec!["acme/app".into(), "acme/docs".into()], level: Level::Autonomous });
    assert_eq!(plan["!s:x"].level, Level::Assist);
    let mut moved = saved.clone();
    moved["mayWork"] = json!(["!r:x", "!s:x"]);
    assert!(planned(&moved, true).is_empty(), "moved already: idempotent");
}

#[test]
fn the_room_line_says_what_moved_and_what_is_left() {
    let line = moved_line("bird", &["acme/app".into()], Some(Level::Assist));
    assert_eq!(line, "bird's issue watching moved to this room's settings: it may work on acme/app here; agent work here is Assist.");
    assert!(moved_line("bird", &["acme/app".into()], None).contains("still needs to choose Assist or Autonomous"));
}
