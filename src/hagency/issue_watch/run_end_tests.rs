//! A hand-off whose run ended without a pull request (e2e, Nov 17: #45/#46
//! pushed branches but opened none; #43/#44 lost their runs to a 502 on
//! lease renewal), on the tracker's fakes: retried at once with one room
//! line saying why, never waiting out the deadline silently.
use super::run_end::{RunEnd, branch_names_issue, issue_branch, requested_by, run_end_of};
use super::tracker::{Inputs, follow};
use super::tracker_tests::{Fake, REPO, T0, check, first_hand_off, issue};
use std::collections::HashSet;
use ::hagency::issue_watch::issue_track::{HANDOFF_TIMEOUT_SECS, MAX_ATTEMPTS, MAX_FREE_RETRIES, State};
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
fn the_hand_offs_run_is_this_issues_newest_request_since_the_attempt() {
    let handed = 1_000;
    let req = |number: i64, at: i64, outcome: &str| json!({"roomId": ROOM, "receivedAt": at, "outcome": outcome, "issue": {"repo": "Acme/App", "number": number}});
    let plain = json!({"roomId": ROOM, "receivedAt": handed + 1, "outcome": "failed"});
    // #45 and #46 handed off together: #45's lost run never ends #46's.
    let requests = json!([req(45, handed + 1, "failed"), plain, req(46, handed + 2, "running")]);
    assert_eq!(run_end_of(&requests, "acme/app", 46, handed), RunEnd::Running);
    assert_eq!(run_end_of(&requests, "acme/app", 45, handed), RunEnd::Lost);
    // The last attempt failed fast; the retry's request is not in yet.
    let previous = json!([req(46, handed - 60, "failed")]);
    assert_eq!(run_end_of(&previous, "acme/app", 46, handed), RunEnd::Running, "a whole check earlier: the last attempt's run");
    assert_eq!(run_end_of(&json!([req(46, handed - 3, "answered")]), "acme/app", 46, handed), RunEnd::Ended, "received just before the record");
    assert_eq!(run_end_of(&json!([req(46, handed - 3, "answered"), req(46, handed + 30, "running")]), "acme/app", 46, handed), RunEnd::Running, "the newest wins");
    assert_eq!(run_end_of(&json!(null), "acme/app", 46, handed), RunEnd::Running, "unreadable: wait for the deadline");
}

#[test]
fn only_one_branch_naming_the_issue_counts() {
    let branches = parse_branches(r#"[{"name":"main"},{"name":"fix/issue-46-e2e-doc"},{"name":"agent/issue-45-e2e-doc"},{"name":"fix-460"}]"#).unwrap();
    assert_eq!(issue_branch(&branches, 46).as_deref(), Some("fix/issue-46-e2e-doc"));
    assert_eq!(issue_branch(&branches, 45).as_deref(), Some("agent/issue-45-e2e-doc"));
    assert_eq!(issue_branch(&branches, 4), None);
    let odd = vec!["fix/issue-8\"`ignore".to_owned(), "-issue-8".to_owned(), format!("issue-8-{}", "x".repeat(100))];
    assert_eq!(issue_branch(&odd, 8), None, "only plain ref names are repeated to the Agent");
    let two = vec!["issue-7-a".to_owned(), "agent/7-b".to_owned()];
    assert_eq!(issue_branch(&two, 7), None, "two candidates: cannot tell which, retry plainly");
    assert!(branch_names_issue("agent/33-readme", 33) && !branch_names_issue("issue-333", 33));
    assert!(parse_branches(r#"[{"sha":"x"}]"#).is_err());
}

#[test]
fn a_record_is_changed_under_its_own_repository_spelling() {
    // The work list may spell the repository differently from the watch that
    // handed the issue off; the ledger keys the record exactly (GPT review).
    let fake = Fake::default();
    first_hand_off(&fake, 45);
    fake.run.set(Some(RunEnd::Lost));
    fake.now.set(T0 + 60);
    let tracks = fake.tracks();
    let seen: HashSet<i64> = HashSet::from([45]);
    let open = [issue(45)];
    let spelled = REPO.to_ascii_uppercase();
    let inputs = Inputs { repo: &spelled, now: T0 + 60, tracks: &tracks, seen: &seen, open_issues: &open, own_pulls: &[] };
    let out = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(follow(&fake, &inputs));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    assert_eq!(fake.tracks()[0].attempts, 2);
}

/// Hands #62 off up to attempt 3, as on Nov 17 (22:30:47).
fn at_attempt_three(fake: &Fake) {
    first_hand_off(fake, 62);
    fake.run.set(Some(RunEnd::Ended));
    check(fake, T0 + 60, &[issue(62)], &[]);
    check(fake, T0 + 120, &[issue(62)], &[]);
    assert_eq!(fake.tracks()[0].attempts, MAX_ATTEMPTS);
    fake.handed.borrow_mut().clear();
    fake.said.borrow_mut().clear();
}

#[test]
fn a_restart_during_attempt_three_hands_it_off_again_without_using_an_attempt() {
    // Nov 17 22:35: a deploy restarted the app mid-run; the issue was marked failed.
    let fake = Fake::default();
    at_attempt_three(&fake);
    fake.run.set(Some(RunEnd::Interrupted));
    assert_eq!(check(&fake, T0 + 300, &[issue(62)], &[]), ["#62 retrying (3/3)"]);
    let track = fake.tracks()[0].clone();
    assert_eq!((track.state, track.attempts, track.free_retries, track.handed_at), (State::HandedOff, MAX_ATTEMPTS, 1, T0 + 300), "attempts stays 3");
    assert_eq!(*fake.handed.borrow(), [(62, 3)]);
    assert_eq!(fake.said.borrow()[0].2, "🔁 #62: the run was interrupted (this app or its Agent host restarted or lost its connection) — handing it off again (attempt 3 of 3, not counted)");
    assert!(!fake.keys().contains(&"62:failed".to_owned()), "never the give-up on a restart");
    // The new run is still going: it waits, it does not give up.
    fake.run.set(Some(RunEnd::Running));
    assert_eq!(check(&fake, T0 + 360, &[issue(62)], &[]), ["#62 retrying (3/3)"]);
    assert_eq!(fake.handed.borrow().len(), 1);
}

#[test]
fn repeated_restarts_stop_after_the_free_retry_cap_and_then_give_up_once() {
    let fake = Fake::default();
    at_attempt_three(&fake);
    fake.run.set(Some(RunEnd::Interrupted));
    for minute in 5..12 {
        check(&fake, T0 + minute * 60, &[issue(62)], &[]);
    }
    assert_eq!(fake.handed.borrow().len(), MAX_FREE_RETRIES as usize, "a crash loop still ends");
    assert_eq!(fake.keys(), ["62:rehand-3-1", "62:rehand-3-2", "62:failed"]);
    let track = fake.tracks()[0].clone();
    assert_eq!((track.state, track.attempts, track.free_retries), (State::Failed, MAX_ATTEMPTS, MAX_FREE_RETRIES));
}

#[test]
fn a_restart_on_an_earlier_attempt_keeps_the_attempt_too() {
    let fake = Fake::default();
    first_hand_off(&fake, 63);
    fake.run.set(Some(RunEnd::Interrupted));
    check(&fake, T0 + 60, &[issue(63)], &[]);
    assert_eq!((fake.tracks()[0].attempts, fake.tracks()[0].free_retries), (1, 1));
    assert_eq!(*fake.handed.borrow(), [(63, 1)]);
}

#[test]
fn an_interrupted_run_reads_from_its_activity_record() {
    let req = |interrupted: bool| json!([{"receivedAt": 1_000, "outcome": "failed", "interrupted": interrupted, "issue": {"repo": "acme/app", "number": 62}}]);
    assert_eq!(run_end_of(&req(true), "acme/app", 62, 1_000), RunEnd::Interrupted);
    assert_eq!(run_end_of(&req(false), "acme/app", 62, 1_000), RunEnd::Lost);
}

#[test]
fn runs_stopped_at_unanswered_approval_cards_do_not_use_up_the_attempts() {
    // e2e Nov 17: #69's runs (23:26:52, 23:37:49, 23:43:51) each ended at a
    // `gh pr create` card nobody answered; the loop spent all three attempts
    // in 22 minutes and gave up with a pushed fix and no pull request.
    let fake = Fake::default();
    first_hand_off(&fake, 69);
    fake.run.set(Some(RunEnd::Unanswered));
    assert_eq!(check(&fake, T0 + 60, &[issue(69)], &[]), ["#69 retrying (1/3)"]);
    let track = fake.tracks()[0].clone();
    assert_eq!((track.state, track.attempts, track.free_retries), (State::HandedOff, 1, 1), "not counted");
    assert_eq!(*fake.handed.borrow(), [(69, 1)]);
    assert_eq!(fake.said.borrow()[0].2, "🔁 #69: the run stopped at an approval card nobody answered — handing it off again (attempt 1 of 3, not counted); approve its card in the owner's Hagency window");
    // An owner who never comes back still ends the loop: the cap, then counted attempts.
    for minute in 2..12 {
        check(&fake, T0 + minute * 60, &[issue(69)], &[]);
    }
    assert_eq!(fake.keys(), ["69:rehand-1-1", "69:rehand-1-2", "69:retry-2", "69:retry-3", "69:failed"]);
    assert_eq!((fake.tracks()[0].state, fake.tracks()[0].free_retries), (State::Failed, MAX_FREE_RETRIES));
}

#[test]
fn an_unanswered_card_reads_from_its_activity_record() {
    let req = |outcome: &str, unanswered: bool| json!([{"receivedAt": 1_000, "outcome": outcome, "unanswered": unanswered, "issue": {"repo": "acme/app", "number": 69}}]);
    assert_eq!(run_end_of(&req("answered", true), "acme/app", 69, 1_000), RunEnd::Unanswered);
    assert_eq!(run_end_of(&req("answered", false), "acme/app", 69, 1_000), RunEnd::Ended);
    assert_eq!(run_end_of(&req("running", true), "acme/app", 69, 1_000), RunEnd::Running, "a card still open: the run goes on");
    assert_eq!(run_end_of(&req("failed", true), "acme/app", 69, 1_000), RunEnd::Lost);
}

#[test]
fn a_room_members_look_alike_request_never_counts_for_the_hand_off() {
    // GPT review: a member can send a notice-shaped request and leave its card unanswered.
    let requests = json!([
        {"requester": "@mallory:x", "receivedAt": 1_010, "outcome": "answered", "unanswered": true, "issue": {"repo": "acme/app", "number": 69}},
        {"requester": "@bird:x", "receivedAt": 1_001, "outcome": "running", "issue": {"repo": "acme/app", "number": 69}},
    ]);
    assert_eq!(run_end_of(&requested_by(&requests, "@bird:x"), "acme/app", 69, 1_000), RunEnd::Running);
    assert_eq!(run_end_of(&requested_by(&json!(null), "@bird:x"), "acme/app", 69, 1_000), RunEnd::Running);
}
