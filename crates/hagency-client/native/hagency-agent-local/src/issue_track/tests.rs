use super::*;

const T0: i64 = 1_000_000;

fn handed(attempts: u32, at: i64) -> Track {
    Track { repo: "acme/app".into(), number: 33, state: State::HandedOff, attempts, handed_at: at, pr: None, notified: false }
}
fn open(now: i64) -> Facts {
    Facts { now, issue: IssueFact::Open, open_pr: None, cannot_fix: false }
}
fn change(handed: Option<u32>, state: Option<State>, pr: Option<u64>) -> TrackChange {
    TrackChange { repo: "acme/app".into(), number: 33, handed, state, pr, notified: None }
}
fn ledger() -> (tempfile::TempDir, Ledger) {
    let tmp = tempfile::tempdir().unwrap();
    let l = Ledger::open(tmp.path().join("local.db"), "@alice:example.org").unwrap();
    (tmp, l)
}

#[test]
fn a_hand_off_without_a_pull_request_is_retried_with_backoff_then_fails() {
    assert_eq!(decide(&handed(1, T0), &open(T0 + HANDOFF_TIMEOUT_SECS - 1)), Action::Wait);
    assert_eq!(decide(&handed(1, T0), &open(T0 + HANDOFF_TIMEOUT_SECS)), Action::Retry { attempt: 2 });
    // The second attempt waits twice as long, the third four times.
    assert_eq!(decide(&handed(2, T0), &open(T0 + HANDOFF_TIMEOUT_SECS)), Action::Wait);
    assert_eq!(decide(&handed(2, T0), &open(T0 + 2 * HANDOFF_TIMEOUT_SECS)), Action::Retry { attempt: 3 });
    assert_eq!(decide(&handed(3, T0), &open(T0 + 4 * HANDOFF_TIMEOUT_SECS)), Action::GiveUp);
    assert_eq!(decide(&handed(MAX_ATTEMPTS + 5, T0), &open(i64::MAX)), Action::GiveUp, "never more than MAX_ATTEMPTS");
}

#[test]
fn an_open_pull_request_stops_retries_and_is_followed() {
    let facts = Facts { open_pr: Some(32), ..open(T0 + 99 * HANDOFF_TIMEOUT_SECS) };
    assert_eq!(decide(&handed(1, T0), &facts), Action::FollowPr { pr: 32 });
    // A pull request seen once and closed unmerged waits for a person, never a retry.
    let reviewed = Track { state: State::Reviewed, pr: Some(32), ..handed(1, T0) };
    assert_eq!(decide(&reviewed, &open(i64::MAX)), Action::Wait);
}

#[test]
fn closing_cannot_fix_and_terminal_states_win() {
    let t = Track { state: State::PrOpened, pr: Some(32), ..handed(1, T0) };
    assert_eq!(decide(&t, &Facts { issue: IssueFact::Done, ..open(T0) }), Action::AnnounceFixed);
    assert_eq!(decide(&t, &Facts { issue: IssueFact::ClosedOther, ..open(T0) }), Action::Close);
    assert_eq!(decide(&handed(1, T0), &Facts { cannot_fix: true, ..open(i64::MAX) }), Action::NeedsHuman);
    for state in [State::Merged, State::Closed, State::Failed, State::NeedsHuman] {
        let done = Track { state, ..handed(1, T0) };
        assert_eq!(decide(&done, &Facts { issue: IssueFact::Done, cannot_fix: true, ..open(i64::MAX) }), Action::Wait);
    }
}

#[test]
fn the_ledger_counts_each_attempt_once_and_keeps_terminal_states() {
    let (_tmp, mut l) = ledger();
    assert!(l.track_issue("a1", &change(Some(2), None, None), T0).is_err(), "tracking starts at the first hand-off");
    l.track_issue("a1", &change(Some(1), None, None), T0).unwrap();
    l.track_issue("a1", &change(Some(1), None, None), T0 + 50).unwrap(); // replay: no-op
    assert_eq!(l.issue_tracks("a1").unwrap(), vec![handed(1, T0)]);
    l.track_issue("a1", &change(Some(3), None, None), T0 + 60).unwrap(); // skipped attempt: no-op
    l.track_issue("a1", &change(Some(2), None, None), T0 + 70).unwrap();
    assert_eq!(l.issue_tracks("a1").unwrap(), vec![handed(2, T0 + 70)]);
    l.track_issue("a1", &change(None, Some(State::PrOpened), Some(32)), T0 + 80).unwrap();
    l.track_issue("a1", &change(None, Some(State::Merged), None), T0 + 90).unwrap();
    l.track_issue("a1", &TrackChange { notified: Some(true), ..change(None, None, None) }, T0 + 91).unwrap();
    // A late retry or state write after the merge changes nothing.
    l.track_issue("a1", &change(Some(3), Some(State::HandedOff), Some(40)), T0 + 99).unwrap();
    let merged = Track { state: State::Merged, pr: Some(32), notified: true, ..handed(2, T0 + 70) };
    assert_eq!(l.issue_tracks("a1").unwrap(), vec![merged]);
    assert!(l.issue_tracks("other").unwrap().is_empty());
}

#[test]
fn records_survive_reopening_the_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("local.db");
    {
        let mut l = Ledger::open(&path, "@alice:example.org").unwrap();
        l.track_issue("a1", &change(Some(1), None, None), T0).unwrap();
    }
    let l = Ledger::open(&path, "@alice:example.org").unwrap();
    assert_eq!(l.issue_tracks("a1").unwrap(), vec![handed(1, T0)]);
}

#[test]
fn changes_parse_strictly() {
    let ok: TrackChange = serde_json::from_value(serde_json::json!({"repo": "a/b", "number": 3, "state": "needs_human"})).unwrap();
    assert_eq!(ok.state, Some(State::NeedsHuman));
    assert!(serde_json::from_value::<TrackChange>(serde_json::json!({"repo": "a/b", "number": 3, "attempts": 0})).is_err());
    assert!(serde_json::from_value::<TrackChange>(serde_json::json!({"repo": "a/b", "number": 3, "state": "done"})).is_err());
}

#[test]
fn only_the_agents_own_cannot_fix_comment_of_this_attempt_counts() {
    use crate::issue_watch::wire::Comment;
    let c = |author: &str, body: &str, at: i64| Comment { author: author.into(), body: body.into(), created_at: at };
    let reason = format!("{CANNOT_FIX_MARKER}: needs a design decision");
    assert!(says_cannot_fix(&[c("Anna", &reason, T0)], "anna", T0));
    assert!(!says_cannot_fix(&[c("mallory", &reason, T0)], "anna", T0), "someone else cannot stop the loop");
    assert!(!says_cannot_fix(&[c("anna", &reason, T0 - 1)], "anna", T0), "an earlier attempt's comment does not count");
    assert!(!says_cannot_fix(&[c("anna", "I think it cannot fix", T0)], "anna", T0));
}
