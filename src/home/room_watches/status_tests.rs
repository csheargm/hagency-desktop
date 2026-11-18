//! The Status tab (ADR 0023): tasks for every room, repository sections only
//! when the room has repositories, grouped per repository with a filter, and
//! Agents' activity lines from published state.
use super::super::lines::{Line, filter_labels, lines};
use super::super::status::{RoomStatus, activity};
use super::super::tasks::{self, Task, TaskState};
use super::super::Watches;
use ::hagency::repo_status::{Published, Seen, TrackState, table::{self, GithubItem, GithubState}};
use serde_json::json;

const BIRD: &str = "@_hagency_agt_bird:hs.test";
const BETTY: &str = "@betty:hs.test";
const NOW: i64 = 1_800_000_000;

fn seen(repo: &str, number: i64, state: TrackState, pr: Option<u64>) -> Seen {
    Seen { publisher: BETTY.into(), issue: Published { agent: BIRD.into(), repo: repo.into(), number, title: format!("Issue {number}"), state, hold: None, attempts: 1, pr, updated_at: NOW - 30 } }
}
fn status_with(tables: Vec<table::Table>, seen: Vec<Seen>, tasks: Vec<Task>) -> RoomStatus {
    let names = [(BIRD.to_owned(), "bird-claude".to_owned()), (BETTY.to_owned(), "Betty".to_owned()), ("@anna:hs.test".to_owned(), "Anna".to_owned())].into();
    RoomStatus { tables, seen, tasks, names, ..Default::default() }
}
fn text(lines: &[Line]) -> String {
    lines.iter().map(|l| match l { Line::Section(s) => format!("[{s}]"), Line::Item(t, d) => format!("{t} | {d}") }).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_room_without_repositories_shows_tasks_but_no_issue_or_pr_lines() {
    let task = Task { agent: BIRD.into(), requester: "@anna:hs.test".into(), state: TaskState::WaitingForApproval, step: String::new(), age_secs: 120 };
    let w = Watches { rows: Vec::new(), status: Ok(status_with(Vec::new(), Vec::new(), vec![task])) };
    let shown = text(&lines(&w, None));
    assert!(shown.contains("[Agent tasks]") && shown.contains("bird-claude · waiting for approval | asked by Anna · 2 min ago"), "{shown}");
    assert!(!shown.contains("in progress across") && !shown.contains("GitHub") && !shown.contains('#'), "{shown}");
    assert_eq!(filter_labels(&w).len(), 1, "no repository filter");
}

#[test]
fn several_repositories_group_into_sections_with_a_summary_and_a_filter() {
    let gh = |n: i64| vec![GithubItem { number: n, title: format!("Bug {n}"), is_pr: false, state: GithubState::Open, closed_at: None }];
    let all_seen = vec![seen("h/desktop", 37, TrackState::Reviewed, Some(38)), seen("h/server", 5, TrackState::HandedOff, None)];
    let tables: Vec<_> = ["h/client", "h/desktop", "h/server"].iter().enumerate()
        .map(|(i, r)| table::build(r, Some(&gh(if *r == "h/desktop" { 37 } else { i as i64 + 1 })), &all_seen, |id| id.to_owned(), NOW)).collect();
    let w = Watches { rows: Vec::new(), status: Ok(status_with(tables, all_seen, Vec::new())) };
    let shown = text(&lines(&w, None));
    assert!(shown.contains("[2 in progress across 3 repos]"), "{shown}");
    for repo in ["[h/client]", "[h/desktop]", "[h/server]"] {
        assert!(shown.contains(repo), "{shown}");
    }
    assert!(shown.contains("#37 Bug 37 · issue | GitHub: open · in review (attempt 1) · PR #38"), "{shown}");
    let only = text(&lines(&w, Some("h/server")));
    assert!(only.contains("[h/server]") && !only.contains("[h/desktop]"));
    assert_eq!(filter_labels(&w), vec!["All repositories", "h/client", "h/desktop", "h/server"]);
}

#[test]
fn activity_comes_from_published_state_then_tasks_else_idle() {
    let s = status_with(Vec::new(), vec![seen("h/desktop", 37, TrackState::Reviewed, Some(38)), seen("h/desktop", 30, TrackState::Merged, Some(31))], Vec::new());
    assert_eq!(activity(&s, BIRD, Some(BETTY)), "working on #37 (PR #38 in review)");
    assert_eq!(activity(&s, BIRD, Some("@mallory:hs.test")), "idle", "a claim from someone who is not the Agent's owner is not shown");
    assert_eq!(activity(&s, BIRD, None), "idle");
    let working = Task { agent: "@_hagency_agt_x:hs.test".into(), requester: "@anna:hs.test".into(), state: TaskState::Working, step: String::new(), age_secs: 5 };
    let s = status_with(Vec::new(), Vec::new(), vec![working]);
    assert_eq!(activity(&s, "@_hagency_agt_x:hs.test", None), "working");
    assert_eq!(activity(&s, BIRD, Some(BETTY)), "idle");
}

fn card(id: &str, state: &str, revision: u64, step: &str) -> serde_json::Value {
    json!({"msgtype": "im.hagency.agent_run", "body": "run", "im.hagency.agent_run": {"version": 1, "id": id, "dispatchId": "d", "executionId": "e",
        "roomId": "!r:hs.test", "eventId": "$req", "threadRoot": "$req", "requester": "@anna:hs.test", "owner": BETTY, "runtime": "claude",
        "state": state, "revision": revision, "progress": {"step": step, "steps": 1}}})
}

#[test]
fn run_cards_become_tasks_with_the_newest_edit_and_only_from_joined_agents() {
    let ms = NOW * 1000;
    let events = vec![
        json!({"type": "m.room.message", "event_id": "$c1", "sender": BIRD, "origin_server_ts": ms - 300_000, "content": card("run_c1", "running", 1, "")}),
        json!({"type": "m.room.message", "event_id": "$e1", "sender": BIRD, "origin_server_ts": ms - 10_000,
            "content": {"m.relates_to": {"rel_type": "m.replace", "event_id": "$c1"}, "m.new_content": card("run_c1", "running", 2, "running gh pr create")}}),
        json!({"type": "m.room.message", "event_id": "$e2", "sender": "@_hagency_agt_evil:hs.test", "origin_server_ts": ms,
            "content": {"m.relates_to": {"rel_type": "m.replace", "event_id": "$c1"}, "m.new_content": card("run_c1", "completed", 9, "")}}),
        json!({"type": "m.room.message", "event_id": "$c2", "sender": "@anna:hs.test", "origin_server_ts": ms, "content": card("run_c2", "running", 1, "")}),
    ];
    // The roster names bird and the "evil" puppet as Agents; Anna is a person.
    let got = tasks::tasks(&events, |id| id == BIRD || id == "@_hagency_agt_evil:hs.test", ms);
    assert_eq!(got.len(), 1, "a person's forged card is not a task: {got:?}");
    assert_eq!((got[0].state, got[0].step.as_str(), got[0].age_secs), (TaskState::Working, "running gh pr create", 300));
    let newest_first: Vec<_> = events.iter().rev().cloned().collect();
    assert_eq!(tasks::tasks(&newest_first, |id| id == BIRD || id == "@_hagency_agt_evil:hs.test", ms), got, "history order does not let another Agent's edit win");
    assert!(tasks::tasks(&events, |_| false, ms).is_empty(), "a lookalike puppet the roster does not name posts no tasks");
    let readable = json!({"type": "m.room.message", "event_id": "$c9", "sender": "@_hagency_csheargm_bird-claude:hs.test", "origin_server_ts": ms, "content": card("run_c9", "running", 1, "")});
    assert_eq!(tasks::tasks(&[readable], |_| true, ms).len(), 1, "readable puppet IDs count");
}
