use super::table::{self, GithubItem, GithubState};
use super::*;
use crate::room_tools::{RoomOperation, RoomTools};

const BIRD: &str = "@_hagency_agt_bird:hs.test";
const ANNA_AGENT: &str = "@_hagency_agt_anna:hs.test";
const BETTY: &str = "@betty:hs.test";
const MALLORY: &str = "@mallory:hs.test";
const REPO: &str = "csheargm/hagency-demo-sandbox";
const NOW: i64 = 1_800_000_000;

fn issue(agent: &str, repo: &str, number: i64, state: TrackState, updated_at: i64) -> Published {
    Published { agent: agent.into(), repo: repo.into(), number, title: format!("Issue {number}"), state, hold: None, attempts: 1, pr: None, updated_at }
}
fn event(sender: &str, key: &str, content: Value) -> Value {
    json!({"type": ISSUE_STATUS_STATE_TYPE, "sender": sender, "state_key": key, "content": content})
}
fn joined(ids: &'static [&'static str]) -> impl Fn(&str) -> Member {
    move |id| Member { joined: ids.contains(&id), name: None }
}

#[test]
fn offered_with_the_watch_tools_and_arguments_are_bounded() {
    let on = RoomTools::history_and_files().with_watches();
    assert!(on.definitions().iter().any(|d| d["name"] == REPO_STATUS_TOOL));
    assert!(matches!(on.parse(REPO_STATUS_TOOL, &json!({})), Ok(RoomOperation::RepoStatus { repo: None })));
    assert!(matches!(on.parse(REPO_STATUS_TOOL, &json!({"repo": REPO})), Ok(RoomOperation::RepoStatus { repo: Some(r) }) if r == REPO));
    for bad in [json!({"repo": "x"}), json!({"repo": "a/b/c"}), json!({"repo": "../x"}), json!({"room": "!r:x"}), json!([REPO])] {
        assert_eq!(on.parse(REPO_STATUS_TOOL, &bad).err(), Some(Error::Malformed), "{bad}");
    }
    assert_eq!(RoomTools::history_and_files().parse(REPO_STATUS_TOOL, &json!({})).err(), Some(Error::Unsupported));
}

#[test]
fn a_transition_replaces_only_that_watchs_issues_and_unchanged_is_no_write() {
    let other = issue(ANNA_AGENT, "acme/web", 4, TrackState::Reviewed, NOW - 60);
    let first = publish_content(&Value::Null, BIRD, REPO, &[issue(BIRD, REPO, 31, TrackState::HandedOff, NOW)], NOW).unwrap();
    let both = publish_content(&first, ANNA_AGENT, "acme/web", &[other.clone()], NOW).unwrap();
    let moved = issue(BIRD, REPO, 31, TrackState::PrOpened, NOW + 5);
    let next = publish_content(&both, BIRD, REPO, &[Published { pr: Some(32), ..moved }], NOW + 5).unwrap();
    let issues: Vec<Published> = serde_json::from_value(next["issues"].clone()).unwrap();
    assert_eq!(issues.len(), 2);
    assert!(issues.iter().any(|i| i.number == 31 && i.state == TrackState::PrOpened && i.pr == Some(32)));
    assert!(issues.contains(&other), "another watch's issues are kept");
    assert_eq!(publish_content(&next, BIRD, REPO, &[issues.iter().find(|i| i.number == 31).unwrap().clone()], NOW + 9), None);
}

#[test]
fn finished_issues_age_out_and_size_is_capped() {
    let old = issue(BIRD, REPO, 1, TrackState::Merged, NOW - RECENT_SECS - 1);
    let fresh = issue(BIRD, REPO, 2, TrackState::Merged, NOW - 10);
    let out = publish_content(&Value::Null, BIRD, REPO, &[old, fresh], NOW).unwrap();
    assert_eq!(out["issues"].as_array().unwrap().len(), 1);
    let many: Vec<Published> = (1..=200).map(|n| Published { title: "t".repeat(500), hold: Some("h".repeat(500)), ..issue(BIRD, REPO, n, TrackState::Reviewed, NOW) }).collect();
    let out = publish_content(&Value::Null, BIRD, REPO, &many, NOW).unwrap();
    assert!(out.to_string().len() <= MAX_CONTENT_BYTES);
    let kept = out["issues"].as_array().unwrap();
    assert!(kept.len() <= MAX_PUBLISHED_ISSUES && !kept.is_empty());
    assert!(kept.iter().all(|i| i["title"].as_str().unwrap().len() <= TITLE_BYTES && i["hold"].as_str().unwrap().len() <= HOLD_BYTES));
}

#[test]
fn only_shareable_fields_are_published_even_from_tampered_content() {
    let tampered = json!({"v": 1, "issues": [{"agent": BIRD, "repo": "acme/web", "number": 5, "state": "reviewed", "attempts": 1, "updatedAt": NOW,
        "token": "ghp_SECRET", "issuesPerDay": 9, "otherRoom": "!x:y"}]});
    let out = publish_content(&tampered, BIRD, REPO, &[issue(BIRD, REPO, 31, TrackState::HandedOff, NOW)], NOW).unwrap();
    let allowed = ["agent", "repo", "number", "title", "state", "hold", "attempts", "pr", "updatedAt"];
    for i in out["issues"].as_array().unwrap() {
        assert!(i.as_object().unwrap().keys().all(|k| allowed.contains(&k.as_str())), "{i}");
    }
    assert!(!out.to_string().contains("SECRET"));
}

#[test]
fn spoofed_state_keys_strangers_and_non_agents_are_ignored() {
    let content = |agent: &str| json!({"v": 1, "issues": [issue(agent, REPO, 31, TrackState::Merged, NOW)]});
    let events = vec![
        event(BETTY, BETTY, content(BIRD)),
        event(MALLORY, BETTY, content(BIRD)),          // someone else's key
        event("@gone:hs.test", "@gone:hs.test", content(BIRD)), // not joined
        event(BETTY, BETTY, content("@betty-alt:hs.test")),     // not an Agent puppet
        json!({"type": "m.room.name", "sender": BETTY, "state_key": BETTY, "content": content(BIRD)}),
    ];
    let got = accepted(&events, joined(&[BETTY, MALLORY, BIRD, "@betty-alt:hs.test"]));
    assert_eq!(got.seen.len(), 1);
    assert_eq!((got.seen[0].publisher.as_str(), got.seen[0].issue.agent.as_str()), (BETTY, BIRD));
    assert_eq!(got.ignored_events, 2);
    let absent_agent = accepted(&events[..1], joined(&[BETTY]));
    assert!(absent_agent.seen.is_empty(), "an Agent that is not in the room is never shown");
}

#[test]
fn an_agent_claimed_by_two_publishers_is_shown_by_neither() {
    let content = json!({"v": 1, "issues": [issue(BIRD, REPO, 31, TrackState::Reviewed, NOW)]});
    let events = vec![event(BETTY, BETTY, content.clone()), event(MALLORY, MALLORY, content)];
    let got = accepted(&events, joined(&[BETTY, MALLORY, BIRD]));
    assert!(got.seen.is_empty());
    assert_eq!(got.conflicts, vec![BIRD.to_owned()]);
    assert!(people_in(&events).contains(BIRD));
}

#[test]
fn the_table_merges_github_with_trackers_and_recent_closes() {
    let github = vec![
        GithubItem { number: 40, title: "Open | untracked".into(), is_pr: false, state: GithubState::Open, closed_at: None },
        GithubItem { number: 32, title: "Fix typo".into(), is_pr: true, state: GithubState::Open, closed_at: None },
        GithubItem { number: 31, title: "Typo".into(), is_pr: false, state: GithubState::Open, closed_at: None },
        GithubItem { number: 20, title: "Done".into(), is_pr: true, state: GithubState::Merged, closed_at: Some(NOW - 3600) },
        GithubItem { number: 10, title: "Ancient".into(), is_pr: false, state: GithubState::Closed, closed_at: Some(NOW - RECENT_SECS - 5) },
    ];
    let seen = vec![
        Seen { issue: Published { pr: Some(32), hold: Some("green CI".into()), ..issue(BIRD, REPO, 31, TrackState::Reviewed, NOW) }, publisher: BETTY.into() },
        Seen { issue: issue(BIRD, REPO, 33, TrackState::Failed, NOW - 60), publisher: BETTY.into() },
        Seen { issue: issue(BIRD, "acme/other", 99, TrackState::HandedOff, NOW), publisher: BETTY.into() },
    ];
    let name = |id: &str| match id { BIRD => "bird-claude".into(), BETTY => "Betty".into(), other => other.into() };
    let t = table::build(REPO, Some(&github), &seen, name, NOW);
    let numbers: Vec<i64> = t.rows.iter().map(|r| r.number).collect();
    assert_eq!(numbers, vec![31, 33, 40, 32, 20], "tracked first, the 8-day-old close left out, other repos ignored");
    assert_eq!(t.rows.iter().find(|r| r.number == 32).unwrap().fixes, Some(31));
    let out = table::render(&[t], &[]);
    assert!(out.contains("in review (attempt 1) · PR #32 · held: green CI · bird-claude (Betty, @betty:hs.test)"), "{out}");
    assert!(out.contains("Open / untracked"), "a title cannot add table columns");
    assert!(out.contains("failed") && out.contains("not listed"));
    assert!(out.contains("\"summary\":\"1 in progress across 1 repo\""), "{out}");
}

#[test]
fn github_unavailable_still_shows_trackers_and_the_result_is_capped() {
    let seen: Vec<Seen> = (1..=40).map(|n| Seen { issue: Published { title: "x".repeat(120), ..issue(BIRD, REPO, n, TrackState::HandedOff, NOW) }, publisher: BETTY.into() }).collect();
    let t = table::build(REPO, None, &seen, str::to_owned, NOW);
    assert!(!t.github_read && t.rows.len() == 40);
    let out = table::render(&[t.clone(), Table { repo: "acme/web".into(), ..t }], &["note".into()]);
    assert!(out.len() <= table::RESULT_BYTES, "{}", out.len());
    let v: Value = serde_json::from_str(&out).unwrap();
    assert!(v["repos"][0]["rowsLeftOut"].as_u64().unwrap() > 0);
}

use table::Table;

#[test]
fn junk_events_do_not_use_up_the_publisher_cap() {
    let mut events: Vec<Value> = (0..MAX_PUBLISHERS * 2).map(|n| event(MALLORY, &format!("@other{n}:hs.test"), json!({}))).collect();
    events.push(event(BETTY, BETTY, json!({"v": 1, "issues": [issue(BIRD, REPO, 31, TrackState::Reviewed, NOW)]})));
    let got = accepted(&events, joined(&[BETTY, MALLORY, BIRD]));
    assert_eq!(got.seen.len(), 1, "Betty's event is still read");
}

#[test]
fn expired_trackers_never_attach_counts_precede_truncation_and_closes_win() {
    let github = vec![
        GithubItem { number: 5, title: "Was open".into(), is_pr: false, state: GithubState::Open, closed_at: None },
        GithubItem { number: 5, title: "Was open".into(), is_pr: false, state: GithubState::Closed, closed_at: Some(NOW - 5) },
    ];
    let old = Seen { issue: Published { pr: Some(6), ..issue(BIRD, REPO, 5, TrackState::Merged, NOW - RECENT_SECS - 1) }, publisher: BETTY.into() };
    let t = table::build(REPO, Some(&github), &[old], str::to_owned, NOW);
    assert_eq!(t.rows.len(), 1);
    assert_eq!(t.rows[0].github, Some(GithubState::Closed));
    assert!(t.rows[0].trackers.is_empty(), "an expired state is not shown as current");
    let many: Vec<Seen> = (1..=60).map(|n| Seen { issue: issue(BIRD, REPO, n, TrackState::HandedOff, NOW), publisher: BETTY.into() }).collect();
    let t = table::build(REPO, None, &many, str::to_owned, NOW);
    assert_eq!((t.rows.len(), table::in_progress(&t)), (table::MAX_ROWS, 60));
}
