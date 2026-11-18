//! A hand-off whose run ended without a pull request (e2e, Nov 17: #45/#46
//! pushed branches but opened none; #43/#44 lost their runs to a 502 on
//! lease renewal), on the tracker's fakes: retried at once with one room
//! line saying why, never waiting out the deadline silently.
use super::run_end::{RunEnd, branch_names_issue, issue_branch, run_end_of};
use super::tracker_tests::{Fake, T0, check, first_hand_off, issue};
use ::hagency::issue_watch::issue_track::{HANDOFF_TIMEOUT_SECS, MAX_ATTEMPTS, State};
use ::hagency::issue_watch::parse_branches;
use serde_json::json;

const ROOM: &str = "!watch:x";

#[test]
fn a_pushed_branch_without_a_pull_request_is_named_and_asked_for_at_once() {
    let fake = Fake::default();
    first_hand_off(&fake, 46);
    fake.run.set(Some(RunEnd::Ended));
    fake.branches.borrow_mut().insert(46, "fix/issue-46-e2e-doc".into());
    assert_eq!(check(&fake, T0 + 120, &[issue(46)], &[]), ["#46 retrying (2/3)"], "two minutes in, far before the deadline");
    assert_eq!(*fake.handed.borrow(), [(46, 2)]);
    assert_eq!(*fake.asked_branch.borrow(), [Some("fix/issue-46-e2e-doc".to_owned())], "the retry asks for the pull request from that branch");
    let said = fake.said.borrow();
    assert_eq!(said.len(), 1);
    assert_eq!(said[0].2, "🔁 #46: bird-claude pushed \"fix/issue-46-e2e-doc\" but opened no pull request — retrying (attempt 2 of 3)");
}

#[test]
fn a_run_that_ended_or_was_lost_without_a_branch_retries_now_with_its_reason() {
    for (run, text) in [(RunEnd::Ended, "🔁 #45: the run ended without a pull request — retrying (attempt 2 of 3)"), (RunEnd::Lost, "🔁 #45: the run was lost before it finished — retrying (attempt 2 of 3)")] {
        let fake = Fake::default();
        first_hand_off(&fake, 45);
        fake.run.set(Some(run));
        check(&fake, T0 + 60, &[issue(45)], &[]);
        assert_eq!(*fake.handed.borrow(), [(45, 2)], "{run:?}");
        assert_eq!(*fake.asked_branch.borrow(), [None]);
        assert_eq!(fake.said.borrow()[0].2, text);
    }
}

#[test]
fn ended_runs_use_up_the_attempts_then_give_up_once() {
    let fake = Fake::default();
    first_hand_off(&fake, 43);
    fake.run.set(Some(RunEnd::Lost));
    for minute in 1..6 {
        check(&fake, T0 + minute * 60, &[issue(43)], &[]);
    }
    assert_eq!(*fake.handed.borrow(), [(43, 2), (43, 3)], "never more than MAX_ATTEMPTS hand-offs");
    assert_eq!(fake.keys(), ["43:retry-2", "43:retry-3", "43:failed"]);
    assert_eq!((fake.tracks()[0].state, fake.tracks()[0].attempts), (State::Failed, MAX_ATTEMPTS));
}

#[test]
fn a_running_hand_off_still_waits_for_its_deadline() {
    let fake = Fake::default();
    first_hand_off(&fake, 44);
    fake.run.set(Some(RunEnd::Running));
    assert_eq!(check(&fake, T0 + HANDOFF_TIMEOUT_SECS - 1, &[issue(44)], &[]), ["#44 waiting for a pull request"]);
    assert!(fake.handed.borrow().is_empty() && fake.said.borrow().is_empty());
    assert_eq!(check(&fake, T0 + HANDOFF_TIMEOUT_SECS, &[issue(44)], &[]), ["#44 retrying (2/3)"]);
    assert!(fake.said.borrow()[0].2.starts_with("🔁 Retrying #44 (attempt 2 of 3): no pull request"));
}

#[test]
fn the_hand_offs_run_is_its_activity_record_in_the_watch_room() {
    let handed = 1_000;
    let req = |room: &str, at: i64, outcome: &str| json!({"roomId": room, "receivedAt": at, "outcome": outcome});
    let requests = json!([req(ROOM, handed + 400, "running"), req(ROOM, handed - 5, "answered"), req("!other:x", handed + 1, "failed"), req(ROOM, handed - 900, "failed")]);
    assert_eq!(run_end_of(&requests, ROOM, handed), RunEnd::Ended, "the first request at the hand-off, not a later one or an older run");
    assert_eq!(run_end_of(&json!([req(ROOM, handed + 2, "failed")]), ROOM, handed), RunEnd::Lost);
    assert_eq!(run_end_of(&json!([req(ROOM, handed + 2, "running")]), ROOM, handed), RunEnd::Running);
    assert_eq!(run_end_of(&json!([req(ROOM, handed - 900, "answered")]), ROOM, handed), RunEnd::Running, "the last attempt's run does not end this one");
    assert_eq!(run_end_of(&json!(null), ROOM, handed), RunEnd::Running, "unreadable: wait for the deadline");
}

#[test]
fn only_one_branch_naming_the_issue_counts() {
    let branches = parse_branches(r#"[{"name":"main"},{"name":"fix/issue-46-e2e-doc"},{"name":"agent/issue-45-e2e-doc"},{"name":"fix-460"}]"#).unwrap();
    assert_eq!(issue_branch(&branches, 46).as_deref(), Some("fix/issue-46-e2e-doc"));
    assert_eq!(issue_branch(&branches, 45).as_deref(), Some("agent/issue-45-e2e-doc"));
    assert_eq!(issue_branch(&branches, 4), None);
    let two = vec!["issue-7-a".to_owned(), "agent/7-b".to_owned()];
    assert_eq!(issue_branch(&two, 7), None, "two candidates: cannot tell which, retry plainly");
    assert!(branch_names_issue("agent/33-readme", 33) && !branch_names_issue("issue-333", 33));
    assert!(parse_branches(r#"[{"sha":"x"}]"#).is_err());
}
