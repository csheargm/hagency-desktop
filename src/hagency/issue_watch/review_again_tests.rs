//! Live PRs #104 and #105 (Nov 18) as user flows on fakes (R6): a verdict
//! GitHub shows but the ledger never recorded is asked for again (bounded,
//! spaced) before any hold; a draft whose fixer's run ended is asked to be
//! readied once per head; the merge line names the Agent and the room rules.
use super::super::loop_notice::{DRAFT_STEP, LoopNotice, Target};
use super::{Again, DRAFT_WAIT_SECS, REREVIEW_STEPS, REREVIEW_WAIT_SECS, ask_again, ask_ready, asks_again, noted_other_owner, ready_draft, ready_moved, unrecorded_verdict};
use super::super::review_loop::LoopIo;
use super::super::run_end::RunEnd;
use super::super::tracker::Followed;
use super::super::tracker_tests::{Fake as Tracker, REPO, T0, check, first_hand_off, issue};
use ::hagency::issue_watch::{APPROVE_VERDICT, HOLD_STEP, PullRequest};
use matrix_sdk::ruma::{OwnedUserId, owned_user_id};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;

const HEAD: &str = "0bc7fc9780b3d1d75238e281f15e5a31a8172001";

#[derive(Default)]
struct Room {
    posted: RefCell<Vec<(LoopNotice, u32)>>,
    steps: RefCell<HashSet<String>>,
    /// `stepDue` answers to give, oldest first (none left: the step's own `taken`).
    due: RefCell<std::collections::VecDeque<Value>>,
}
impl LoopIo for Room {
    async fn post(&self, notice: &LoopNotice, again: u32) -> Result<(), String> {
        self.posted.borrow_mut().push((notice.clone(), again));
        Ok(())
    }
    async fn review(&self, change: Value) -> Result<Value, String> {
        let key = |step: &Value| format!("{}@{}", step["step"].as_str().unwrap(), step["head"].as_str().unwrap());
        if let Some(step) = change.get("stepTaken") {
            return Ok(json!({"taken": self.steps.borrow().contains(&key(step))}));
        }
        if let Some(step) = change.get("stepDue") {
            let taken = self.steps.borrow().contains(&key(step));
            return Ok(json!({"due": self.due.borrow_mut().pop_front().unwrap_or_else(|| json!({"taken": taken, "again": 0}))}));
        }
        if change.get("stepSaid").is_some() {
            return Ok(json!({}));
        }
        let step = change.get("claimStep").expect("only steps are asked here");
        Ok(json!({"claimed": self.steps.borrow_mut().insert(key(step))}))
    }
    fn paused(&self) -> bool {
        false
    }
    fn requester(&self) -> String {
        "@bird:beta".into()
    }
    fn room(&self) -> String {
        "!watch:alpha".into()
    }
}

fn target(number: u64) -> Target {
    Target { repo: "csheargm/hagency-demo-sandbox".into(), number, url: format!("https://github.com/csheargm/hagency-demo-sandbox/pull/{number}"), head: HEAD.into() }
}
fn codex() -> OwnedUserId {
    owned_user_id!("@_hagency_agt_bird_codex:beta")
}
fn claude() -> OwnedUserId {
    owned_user_id!("@_hagency_agt_bird_claude:beta")
}
fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(future)
}
fn no_approval() -> Value {
    json!({"decision": "human", "head": HEAD, "kind": "approvals", "approvers": 0, "reason": "needs 1 approval(s) from another Agent, has 0"})
}

#[test]
fn pr_105_an_unrecorded_github_verdict_is_what_asks_again() {
    let verdict = vec![("csheargm".to_owned(), APPROVE_VERDICT.to_owned(), HEAD.to_owned())];
    assert!(unrecorded_verdict(&no_approval(), (&verdict, "CSHEARGM"), HEAD));
    assert!(!unrecorded_verdict(&no_approval(), (&verdict, "mallory"), HEAD), "only the Agents' own GitHub account's verdict");
    let comment = vec![("csheargm".to_owned(), "COMMENTED".to_owned(), HEAD.to_owned())];
    assert!(!unrecorded_verdict(&no_approval(), (&comment, "csheargm"), HEAD), "a plain comment is no verdict");
    let older = vec![("csheargm".to_owned(), APPROVE_VERDICT.to_owned(), "1".repeat(40))];
    assert!(!unrecorded_verdict(&no_approval(), (&older, "csheargm"), HEAD), "a verdict of another head");
    let recorded = json!({"decision": "human", "head": HEAD, "kind": "approvals", "approvers": 1});
    assert!(!unrecorded_verdict(&recorded, (&verdict, "csheargm"), HEAD), "a recorded approval needs no second review");
    let rules = json!({"decision": "human", "head": HEAD, "kind": "rules", "approvers": 0});
    assert!(!unrecorded_verdict(&rules, (&verdict, "csheargm"), HEAD), "only a missing approval is the reviewer's to fix");
}

#[test]
fn pr_105_the_reviewer_is_asked_again_twice_spaced_out_then_the_hold_may_follow() {
    let room = Room::default();
    let t = target(105);
    assert_eq!(run(ask_again(&room, &t, &codex(), None)), Ok(Again::Asked), "asked at once: its run is over");
    assert_eq!(run(ask_again(&room, &t, &codex(), Some(60))), Ok(Again::Waiting), "never twice within the wait");
    assert_eq!(run(ask_again(&room, &t, &codex(), Some(REREVIEW_WAIT_SECS))), Ok(Again::Asked));
    assert_eq!(run(ask_again(&room, &t, &codex(), Some(60))), Ok(Again::Waiting), "the last request gets its wait too");
    assert_eq!(run(ask_again(&room, &t, &codex(), Some(REREVIEW_WAIT_SECS))), Ok(Again::Exhausted));
    let posted = room.posted.borrow();
    assert_eq!(posted.len(), REREVIEW_STEPS.len(), "bounded: {posted:?}");
    for (notice, again) in posted.iter() {
        assert!(matches!(notice, LoopNotice::ReviewRequested { reviewer, .. } if *reviewer == codex()), "the same reviewer: {notice:?}");
        assert!(*again > ::hagency::issue_watch::MAX_RESENDS, "a new message, apart from step re-sends: {again}");
    }
    assert_ne!(posted[0].0.txn_id_again("agt", posted[0].1), posted[1].0.txn_id_again("agt", posted[1].1));
    drop(posted);
    // A new head starts over.
    let moved = Target { head: "2".repeat(40), ..t };
    assert_eq!(run(ask_again(&room, &moved, &codex(), Some(60))), Ok(Again::Waiting));
    assert_eq!(run(ask_again(&room, &moved, &codex(), None)), Ok(Again::Asked));
}

/// GPT review: both requests went out before a restart; the reviewer still
/// gets its wait from the first check after it before the hold.
#[test]
fn after_a_restart_with_every_request_sent_the_hold_still_waits_once() {
    let room = Room::default();
    let t = target(905);
    for step in REREVIEW_STEPS {
        room.steps.borrow_mut().insert(format!("{step}@{HEAD}"));
    }
    let verdict = vec![("csheargm".to_owned(), APPROVE_VERDICT.to_owned(), HEAD.to_owned())];
    let at = |now| run(asks_again(&room, &t, (&no_approval(), &verdict, "csheargm"), &codex(), now));
    assert_eq!(at(T0), Ok(true), "waits: this process never saw the last request");
    assert_eq!(at(T0 + 60), Ok(true));
    assert_eq!(at(T0 + REREVIEW_WAIT_SECS), Ok(false), "then the hold asks a person");
    assert!(room.posted.borrow().is_empty(), "never more requests than the steps allow");
}

#[test]
fn the_loops_own_steps_are_once_per_head_hold_steps() {
    for step in REREVIEW_STEPS.iter().chain([&DRAFT_STEP]) {
        let words = step.strip_prefix(HOLD_STEP).unwrap();
        assert!(words.split('-').all(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase())), "{step}");
    }
}

fn draft(number: u64, created_at: i64) -> PullRequest {
    PullRequest { number, url: format!("https://github.com/{REPO}/pull/{number}"), draft: true, head_sha: "1".repeat(40), author: "anna".into(), created_at, body: "Closes #103".into(), head_ref: "fix/issue-103-status-doc".into() }
}

#[test]
fn pr_104_a_draft_whose_run_ended_asks_the_fixer_to_mark_it_ready() {
    let mut tracker = Tracker::default();
    tracker.peer = true;
    first_hand_off(&tracker, 103);
    *tracker.outcome.borrow_mut() = Some(Followed::Draft);
    let pr = draft(104, T0 + 30);
    tracker.run.set(Some(RunEnd::Running));
    assert_eq!(check(&tracker, T0 + 60, &[issue(103)], std::slice::from_ref(&pr)), ["#103 PR #104 is a draft"]);
    assert!(tracker.asked_ready.borrow().is_empty(), "the fixer may still be working on it");
    tracker.run.set(Some(RunEnd::Ended));
    check(&tracker, T0 + 120, &[issue(103)], std::slice::from_ref(&pr));
    assert_eq!(*tracker.asked_ready.borrow(), [104], "its run ended without gh pr ready");
    // A run that never reads as ended still gets its draft readied after the wait.
    let mut stuck = Tracker::default();
    stuck.peer = true;
    first_hand_off(&stuck, 103);
    *stuck.outcome.borrow_mut() = Some(Followed::Draft);
    stuck.run.set(Some(RunEnd::Running));
    check(&stuck, T0 + 30 + DRAFT_WAIT_SECS, &[issue(103)], std::slice::from_ref(&pr));
    assert_eq!(*stuck.asked_ready.borrow(), [104]);
}

#[test]
fn pr_104_the_ready_request_is_posted_once_per_head_with_the_exact_command() {
    let room = Room::default();
    for _ in 0..3 {
        run(ask_ready(&room, &target(104), &claude(), "bird-claude")).unwrap();
    }
    let posted = room.posted.borrow();
    assert_eq!(posted.len(), 1, "{posted:?}");
    let body = posted[0].0.body();
    assert!(body.starts_with(&format!("📝 Still a draft — bird-claude (@_hagency_agt_bird_claude:beta) mark ready PR #104 in csheargm/hagency-demo-sandbox at head commit {HEAD}:")), "{body}");
    assert!(body.contains("head is still 0bc7fc9780b3, run exactly `gh pr ready 104 --repo csheargm/hagency-demo-sandbox`"), "{body}");
    assert!(body.contains("if its head moved since, do not mark it ready"), "{body}");
    assert_eq!(posted[0].0.mention(), Some(&claude()), "the fixer is dispatched");
}

#[test]
fn the_merge_line_names_the_agent_and_the_room_rules() {
    let approved = LoopNotice::Approved { target: target(105), fixer: claude(), name: "bird-claude".into() };
    let body = approved.body();
    assert!(body.contains("merging under the room rules: bird-claude (@_hagency_agt_bird_claude:beta) run exactly `gh pr merge 105 --squash --match-head-commit"), "{body}");
    assert!(!body.contains("repo rules"), "{body}");
    assert_eq!(approved.mention(), Some(&claude()), "the mention the Agent needs stays");
    let unnamed = LoopNotice::Approved { target: target(105), fixer: claude(), name: String::new() }.body();
    assert!(unnamed.contains("room rules: @_hagency_agt_bird_claude:beta run exactly"), "{unnamed}");
}

/// Round-2 GPT review (P2): the ready request reached a fixer that never ran
/// (its budget used up): the owner API gives the step back, and the request
/// is posted again as a new message, never consumed for good.
#[test]
fn a_ready_request_that_never_ran_is_posted_again_when_the_step_is_given_back() {
    let room = Room::default();
    room.due.borrow_mut().extend([json!({"taken": true, "again": 0, "say": {"why": "budget", "gaveUp": false, "round": 0}}), json!({"taken": false, "again": 1})]);
    run(ask_ready(&room, &target(204), &claude(), "bird-claude")).unwrap();
    run(ask_ready(&room, &target(204), &claude(), "bird-claude")).unwrap();
    let posted = room.posted.borrow();
    assert_eq!(posted.len(), 2, "{posted:?}");
    assert!(posted[0].0.body().starts_with("⏸️ The ready request of PR #204"), "one line for the round: {}", posted[0].0.body());
    assert!(matches!(&posted[1], (LoopNotice::MarkReady { .. }, 1)), "re-posted as a new message: {posted:?}");
}

/// Round-2 GPT review (P2): a ready request run late readies whatever head
/// is current. A pull request readied at a head other than the one asked
/// waits like a draft until the fixer's run ended; ready at the asked head, it goes on.
#[test]
fn a_pull_request_readied_at_a_moved_head_waits_until_the_fixers_run_ended() {
    let tracker = Tracker::default();
    first_hand_off(&tracker, 103);
    let track = tracker.tracks()[0].clone();
    tracker.run.set(Some(RunEnd::Ended));
    let mut pr = draft(304, T0);
    run(ready_draft(&tracker, &track, &pr, T0 + 60)).unwrap();
    assert_eq!(*tracker.asked_ready.borrow(), [304]);
    pr.draft = false;
    pr.head_sha = "2".repeat(40);
    assert!(ready_moved(&pr.url, &pr.head_sha, false), "readied at a head nobody asked for");
    tracker.run.set(Some(RunEnd::Running));
    run(ready_draft(&tracker, &track, &pr, T0 + 120)).unwrap();
    assert!(ready_moved(&pr.url, &pr.head_sha, false), "the fixer's run may still be pushing");
    tracker.run.set(Some(RunEnd::Ended));
    run(ready_draft(&tracker, &track, &pr, T0 + 180)).unwrap();
    assert!(!ready_moved(&pr.url, &pr.head_sha, false), "its run ended: what it left is reviewed");
    // Readied at the asked head: no wait, and the request is done.
    let mut asked = draft(305, T0);
    run(ready_draft(&tracker, &track, &asked, T0 + 60)).unwrap();
    asked.draft = false;
    assert!(!ready_moved(&asked.url, &asked.head_sha, false));
    assert!(!ready_moved(&asked.url, &"3".repeat(40), false), "forgotten once done");
}

/// Round-2 GPT review (P2): another owner's reviewer posts its verdict as
/// that owner's GitHub login, which this device does not know; the hold line says so.
#[test]
fn the_hold_line_says_when_the_reviewers_github_login_is_unknown() {
    let noted = noted_other_owner(no_approval(), true);
    let reason = noted["reason"].as_str().unwrap();
    assert!(reason.starts_with("needs 1 approval(s) from another Agent, has 0; the reviewer is another owner's Agent"), "{reason}");
    assert_eq!(noted_other_owner(no_approval(), false), no_approval(), "this owner's own reviewer: its login is this owner's");
    let ci = json!({"kind": "ci", "approvers": 0, "reason": "CI is red"});
    assert_eq!(noted_other_owner(ci.clone(), true), ci, "only a missing approval");
}
