//! The room work loop on the poller (ADR 0030): what runs, what is recorded,
//! and the work list "may work" implies.
use super::*;
use crate::hagency::room_autonomy::state::RoomState;
use ::hagency::issue_watch::safeguards::room_autonomy::{Level, Pause};
use serde_json::json;

fn state(level: Level, paused: bool, linked: &[&str]) -> Result<RoomState, String> {
    Ok(RoomState {
        level: Ok(level), level_by: "@mo:x".into(), cleared: vec![], pause: paused.then(|| Pause { by: "@m:x".into(), at_ms: 1 }),
        linked: linked.iter().map(|r| (*r).to_owned()).collect(), can_change: false, can_pause: true, can_link: false, maintainer: true,
    })
}
fn rooms(entries: Vec<(&str, Result<RoomState, String>)>) -> Rooms {
    Rooms(entries.into_iter().map(|(r, s)| (r.to_owned(), s)).collect())
}

#[test]
fn only_running_rooms_work_and_each_held_room_says_why() {
    let saved = json!({"work": [{"repo": "a/off", "roomId": "!off:x"}, {"repo": "a/assist", "roomId": "!a:x"}, {"repo": "a/paused", "roomId": "!p:x"}, {"repo": "a/unread", "roomId": "!u:x"}]});
    let rooms = rooms(vec![("!off:x", state(Level::Off, false, &[])), ("!a:x", state(Level::Assist, false, &[])), ("!p:x", state(Level::Autonomous, true, &[])), ("!u:x", Err("no answer".into()))]);
    let (kept, held) = running_only(&saved, &rooms);
    assert_eq!(kept["work"].as_array().unwrap().len(), 1);
    assert_eq!(kept["work"][0]["repo"], "a/assist");
    assert_eq!(held.len(), 3, "{held:?}");
    assert!(held.iter().any(|h| h.starts_with("a/paused: the room is paused")));
    assert!(held.iter().any(|h| h == "a/unread: no answer"), "an unreadable room holds, fail closed");
    assert_eq!(rooms.paused(), vec!["!p:x".to_owned()]);
    let snapshots = rooms.snapshots();
    assert!(snapshots.iter().find(|s| s.room_id == "!u:x").is_some_and(|s| s.unread), "an unread room is recorded unread");
    assert!(snapshots.iter().find(|s| s.room_id == "!p:x").is_some_and(|s| s.paused));
}

#[test]
fn may_work_derives_the_work_list_from_linked_repos_and_keeps_settings() {
    let saved = json!({"mayWork": ["!a:x", "!b:x"], "work": [{"repo": "acme/app", "roomId": "!old:x", "since": 7, "trustedAuthors": ["betty"]}]});
    let rooms = rooms(vec![("!a:x", state(Level::Assist, false, &["Acme/App", "acme/docs"])), ("!b:x", state(Level::Assist, false, &["acme/docs"]))]);
    let work = derived_work(&saved, &rooms, 100);
    let repos: Vec<(&str, &str, bool, i64)> = work.iter().map(|w| (w.repo.as_str(), w.room_id.as_str(), w.push_freely, w.since)).collect();
    assert_eq!(repos, vec![("acme/app", "!a:x", true, 7), ("acme/docs", "!a:x", true, 100)], "kept settings, push steps on, a repo in two rooms once");
    assert_eq!(work[0].trusted_authors, vec!["betty".to_owned()]);
    assert!(same_work(&json!({"work": serde_json::to_value(&work).unwrap()}), &work));
    assert!(!same_work(&saved, &work));
    // A room that could not be read keeps what it had: one failed read drops nothing.
    let saved = json!({"mayWork": ["!a:x"], "work": [{"repo": "acme/app", "roomId": "!a:x"}]});
    let unread = derived_work(&saved, &rooms_of_err(), 100);
    assert_eq!(unread.iter().map(|w| w.repo.as_str()).collect::<Vec<_>>(), ["acme/app"]);
    assert!(derived_work(&json!({"work": [{"repo": "acme/app", "roomId": "!a:x"}]}), &rooms_of_err(), 1).is_empty(), "no may-work room: no work");
}

fn rooms_of_err() -> Rooms {
    rooms(vec![("!a:x", Err("down".into()))])
}

#[test]
fn the_loop_rooms_are_may_work_work_and_an_active_legacy_watch() {
    let saved = json!({"mayWork": ["!a:x"], "work": [{"repo": "r/w", "roomId": "!w:x"}], "issueWatch": {"mode": "trusted", "roomId": "!l:x"}});
    assert_eq!(loop_rooms(&saved).into_iter().collect::<Vec<_>>(), ["!a:x", "!l:x", "!w:x"]);
    let off = json!({"issueWatch": {"mode": "off", "roomId": "!l:x"}});
    assert!(loop_rooms(&off).is_empty());
}

#[test]
fn a_paused_room_never_takes_a_repository_from_a_running_room() {
    // Batch GPT review: A (paused) first in mayWork, B (Assist) links the same repo.
    let saved = json!({"mayWork": ["!a:x", "!b:x", "!c:x"], "work": []});
    let rooms = rooms(vec![("!a:x", state(Level::Autonomous, true, &["acme/app", "acme/only-a"])), ("!b:x", state(Level::Assist, false, &["acme/app"])), ("!c:x", state(Level::Off, false, &["acme/app"]))]);
    let work = derived_work(&saved, &rooms, 1);
    let pairs: Vec<(&str, &str)> = work.iter().map(|w| (w.repo.as_str(), w.room_id.as_str())).collect();
    assert_eq!(pairs, vec![("acme/app", "!b:x"), ("acme/only-a", "!a:x")], "the running room gets it; the paused room keeps its own");
    let (kept, _) = running_only(&json!({"work": serde_json::to_value(&work).unwrap()}), &rooms);
    assert_eq!(kept["work"][0]["repo"], "acme/app", "so it runs");
}
