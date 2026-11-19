//! The loop controls' words (ADR 0030), read for meaning.
use super::lines::*;
use super::state::RoomState;
use ::hagency::issue_watch::safeguards::room_autonomy::{Level, Pause};

fn state(level: Result<Level, String>, pause: Option<Pause>) -> RoomState {
    RoomState { level, level_by: String::new(), cleared: vec![], pause, pause_name: None, linked: vec![], can_change: false, can_pause: true, can_link: false, maintainer: false }
}

#[test]
fn the_status_line_says_level_and_work_or_who_paused() {
    let name = |id: &str| id.trim_start_matches('@').split(':').next().unwrap_or_default().to_owned();
    let time = |_: i64| "13:40".to_owned();
    assert_eq!(status_line(&state(Ok(Level::Autonomous), None), 2, name, time), "Autonomous · 2 agents working");
    assert_eq!(status_line(&state(Ok(Level::Assist), None), 1, name, time), "Assist · 1 agent working");
    assert_eq!(status_line(&state(Ok(Level::Off), None), 0, name, time), "Off · agents only answer when asked");
    let paused = Some(Pause { by: "@anna:x".into(), at_ms: 1 });
    assert_eq!(status_line(&state(Ok(Level::Autonomous), paused.clone()), 2, name, time), "Paused by anna at 13:40", "no display name: the local part");
    let named = RoomState { pause_name: Some("Anna".into()), ..state(Ok(Level::Autonomous), paused.clone()) };
    assert_eq!(status_line(&named, 2, name, time), "Paused by Anna at 13:40", "on-screen bug (Nov 18): the display name, like the room line");
    assert!(status_line(&state(Err("unreadable".into()), None), 0, name, time).starts_with("Agent work is on hold: unreadable"));
    assert_eq!(badge(&state(Ok(Level::Off), None)), None, "Off shows no badge");
    assert_eq!(badge(&state(Ok(Level::Assist), paused)), Some("Paused"));
    assert_eq!(badge(&state(Err("x".into()), None)), Some("Paused"), "an unreadable level is shown as paused");
}

#[test]
fn room_lines_name_who_and_when() {
    assert_eq!(paused_line("Anna", "13:40"), "⏸ Paused by Anna at 13:40");
    assert_eq!(resumed_line("Ben", "14:02"), "▶ Resumed by Ben at 14:02");
    assert!(level_line("Ben", Level::Assist).contains("a person merges"));
    assert_eq!(level_index(Level::Autonomous), 2);
    assert_eq!(LEVEL_LABELS.len(), Level::ALL.len());
}

/// On-screen bug (Nov 18): the Status pane never named the linked repositories.
#[test]
fn the_linked_repositories_are_listed() {
    assert_eq!(linked_line(&["csheargm/hagency-demo-sandbox".to_owned()]), "Linked: csheargm/hagency-demo-sandbox");
    assert_eq!(linked_line(&["a/b".to_owned(), "c/d".to_owned()]), "Linked: a/b, c/d");
    assert_eq!(linked_line(&[]), "", "nothing linked: no line");
}
