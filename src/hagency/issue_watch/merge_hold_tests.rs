//! The issue loop's merge step against a fake room and owner API (R6): each
//! thing that keeps a reviewed pull request from merging is named in ONE
//! room notice per head and set, a fixed blocker lets the next one through,
//! and a pull request that passes is carried on to the pinned merge.
use super::loop_notice::{LoopNotice, Target};
use super::merge_hold::{Gate, Missing, REVIEW_WAIT_SECS, Streaks, UNREACHABLE_AFTER, after_failure, follow_gate, review_settled};
use super::review_loop::{LoopCtx, LoopIo};
use matrix_sdk::ruma::{OwnedUserId, owned_user_id};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashSet;

const HEAD: &str = "ad395c6d9cdea90a54566da56cda9cae91ddd540";
const NO_APPROVAL: &str = "needs 1 approval(s) from another Agent, has 0";

struct Fake {
    posted: RefCell<Vec<LoopNotice>>,
    steps: RefCell<HashSet<String>>,
    readiness: RefCell<Value>,
}
impl Fake {
    fn new(readiness: Value) -> Self {
        Self { posted: RefCell::default(), steps: RefCell::default(), readiness: RefCell::new(readiness) }
    }
    fn bodies(&self) -> Vec<String> {
        self.posted.borrow().iter().map(LoopNotice::body).collect()
    }
}
impl LoopIo for Fake {
    async fn post(&self, notice: &LoopNotice) -> Result<(), String> {
        self.posted.borrow_mut().push(notice.clone());
        Ok(())
    }
    async fn review(&self, change: Value) -> Result<Value, String> {
        let key = |step: &Value| format!("{}@{}", step["step"].as_str().unwrap(), step["head"].as_str().unwrap());
        if let Some(step) = change.get("stepTaken") {
            return Ok(json!({"taken": self.steps.borrow().contains(&key(step))}));
        }
        if let Some(step) = change.get("claimStep") {
            return Ok(json!({"claimed": self.steps.borrow_mut().insert(key(step))}));
        }
        Ok(json!({"readiness": self.readiness.borrow().clone()}))
    }
    fn paused(&self) -> bool {
        false
    }
}

fn target(head: &str) -> Target {
    Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 36, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/36".into(), head: head.into() }
}
fn fixer() -> OwnedUserId {
    owned_user_id!("@_hagency_agt_bird_claude:alpha")
}
fn human(kind: &str, reason: &str, auto_merge: bool) -> Value {
    json!({"decision": "human", "head": HEAD, "reason": reason, "kind": kind, "approvers": 0, "autoMerge": auto_merge})
}
fn merge(auto_merge: bool) -> Value {
    json!({"decision": "merge", "head": HEAD, "approvers": 1, "autoMerge": auto_merge})
}
fn check(fake: &Fake, permission: Option<&str>, settled: bool) -> Gate {
    let agent = fixer();
    let ctx = LoopCtx { agent: &agent, maintainer: true, permission };
    let readiness = fake.readiness.borrow().clone();
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(follow_gate(fake, &target(HEAD), &readiness, &ctx, settled)).unwrap()
}

#[test]
fn pr_36_an_unrecorded_approval_is_named_once_instead_of_waiting_silently() {
    // Nov 17: README-only, require_ci false, the reviewer's verdict is on
    // GitHub but no Agent approval is in this device's ledger.
    let fake = Fake::new(human("approvals", NO_APPROVAL, true));
    for _ in 0..3 {
        assert_eq!(check(&fake, Some("admin"), true), Gate::Hold(vec![(Missing::Approval, NO_APPROVAL.into())]));
    }
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 1, "one notice, not one per check: {bodies:?}");
    assert!(bodies[0].contains("PR #36 in csheargm/hagency-demo-sandbox at ad395c6d9cde cannot be merged yet"), "{}", bodies[0]);
    assert!(bodies[0].contains(&format!("an Agent approval of this head (the merge gate says \"{NO_APPROVAL}\")")));
    assert!(bodies[0].contains("Codex Agent's review") && bodies[0].contains("review and merge it yourself"));
    assert!(bodies[0].ends_with("https://github.com/csheargm/hagency-demo-sandbox/pull/36"));
}

#[test]
fn a_missing_approval_waits_while_the_reviewer_has_not_answered() {
    let fake = Fake::new(human("approvals", NO_APPROVAL, true));
    assert_eq!(check(&fake, Some("admin"), false), Gate::WaitReview);
    assert!(fake.posted.borrow().is_empty());
}

#[test]
fn the_review_settles_on_a_verdict_for_this_head_or_after_the_wait_when_never_reviewed() {
    let review = |commit: &str| vec![("csheargm".to_owned(), "COMMENTED".to_owned(), commit.to_owned())];
    let opened = 1_000;
    assert!(review_settled(&review(HEAD), HEAD, opened, opened + 1));
    assert!(!review_settled(&[], HEAD, opened, opened + REVIEW_WAIT_SECS - 1));
    assert!(review_settled(&[], HEAD, opened, opened + REVIEW_WAIT_SECS));
    let old = "1".repeat(40);
    assert!(!review_settled(&review(&old), HEAD, opened, opened + 10 * REVIEW_WAIT_SECS), "after a push, wait for the new head's verdict");
}

#[test]
fn merges_switched_off_is_named_instead_of_telling_the_agent_to_merge() {
    let fake = Fake::new(merge(false));
    assert_eq!(check(&fake, Some("admin"), true), Gate::Hold(vec![(Missing::MergesOff, String::new())]));
    check(&fake, Some("admin"), true);
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert!(bodies[0].contains("Pull request merges switched on for this Agent") && bodies[0].contains("Turn it on in the Agent's settings"));
    assert!(!bodies[0].contains("gh pr merge"), "no merge instruction while the owner keeps merges off");
}

#[test]
fn red_ci_rules_and_a_missing_merge_permission_are_each_named() {
    let cases = [
        (human("ci", "a CI check failed", true), Some("admin"), Missing::Ci, "green CI (the merge gate says \"a CI check failed\")"),
        (human("rules", "src/main.rs is outside the low-risk paths", true), Some("admin"), Missing::Rules, "the repo rules say \"src/main.rs is outside the low-risk paths\""),
        (merge(true), Some("read"), Missing::Permission, "cannot merge csheargm/hagency-demo-sandbox"),
    ];
    for (readiness, permission, missing, text) in cases {
        let fake = Fake::new(readiness);
        let Gate::Hold(held) = check(&fake, permission, false) else { panic!("{missing:?} must hold") };
        assert_eq!(held.iter().map(|(m, _)| *m).collect::<Vec<_>>(), [missing]);
        assert!(fake.bodies()[0].contains(text), "{}", fake.bodies()[0]);
    }
}

#[test]
fn running_ci_and_unreadable_or_moved_facts_wait_without_a_notice() {
    let fake = Fake::new(json!({"decision": "wait", "head": HEAD, "reason": "CI checks are still running", "kind": "ci", "autoMerge": true}));
    assert_eq!(check(&fake, Some("admin"), true), Gate::WaitCi);
    let fake = Fake::new(json!({"decision": "human", "reason": "GitHub or the merge policy could not be read", "kind": "rules"}));
    assert_eq!(check(&fake, Some("admin"), true), Gate::Unreadable("GitHub or the merge policy could not be read".into()), "a failed read is a failure, not a wait");
    let fake = Fake::new(json!({"decision": "merge", "head": "2".repeat(40), "autoMerge": true}));
    assert_eq!(check(&fake, Some("admin"), true), Gate::WaitFacts);
    assert!(fake.posted.borrow().is_empty());
}

#[test]
fn each_fixed_blocker_lets_the_next_through_until_the_agent_is_told_to_merge() {
    // Two things missing at once: one notice lists both.
    let fake = Fake::new(human("approvals", NO_APPROVAL, false));
    assert_eq!(check(&fake, Some("admin"), true), Gate::Hold(vec![(Missing::Approval, NO_APPROVAL.into()), (Missing::MergesOff, String::new())]));
    check(&fake, Some("admin"), true);
    assert_eq!(fake.bodies().len(), 1);
    assert!(fake.bodies()[0].contains("an Agent approval") && fake.bodies()[0].contains("Pull request merges switched on"));
    // An approval is recorded: only the switch is left, said once more.
    *fake.readiness.borrow_mut() = merge(false);
    check(&fake, Some("admin"), true);
    check(&fake, Some("admin"), true);
    assert_eq!(fake.bodies().len(), 2);
    assert!(!fake.bodies()[1].contains("an Agent approval") && fake.bodies()[1].contains("Pull request merges switched on"));
    // The owner turns merges on: the fixer gets the pinned merge, once.
    *fake.readiness.borrow_mut() = merge(true);
    for _ in 0..3 {
        assert_eq!(check(&fake, Some("admin"), true), Gate::Merge);
    }
    let posted = fake.posted.borrow();
    assert_eq!(posted.len(), 3, "{posted:?}");
    assert!(matches!(&posted[2], LoopNotice::Approved { .. }));
    assert!(posted[2].body().contains(&format!("gh pr merge 36 --squash --match-head-commit {HEAD} --repo csheargm/hagency-demo-sandbox")));
}

#[test]
fn held_notices_never_share_a_transaction_id_with_the_merge_or_each_other() {
    let held = |step: &str| LoopNotice::Held { target: target(HEAD), step: step.into(), missing: vec![] };
    let approved = LoopNotice::Approved { target: target(HEAD), fixer: fixer() };
    let ids: HashSet<_> = [held("hold-approvals"), held("hold-merges-off"), approved].iter().map(|n| n.txn_id("agt")).collect();
    assert_eq!(ids.len(), 3);
}

#[test]
fn an_older_owner_api_without_the_switch_never_lets_an_agent_merge() {
    let fake = Fake::new(json!({"decision": "merge", "head": HEAD, "approvers": 1}));
    assert_eq!(check(&fake, Some("admin"), true), Gate::Hold(vec![(Missing::MergesOff, String::new())]));
}

#[test]
fn the_merge_is_told_from_the_same_answer_that_judged_it() {
    // The fake answers every later readiness read with "human": a second
    // read would post NeedsOwner and burn the merge step (GPT review).
    struct Flip(Fake);
    impl LoopIo for Flip {
        async fn post(&self, notice: &LoopNotice) -> Result<(), String> {
            self.0.post(notice).await
        }
        async fn review(&self, change: Value) -> Result<Value, String> {
            if change.get("readiness").is_some() {
                return Ok(json!({"readiness": human("rules", "changed", true)}));
            }
            self.0.review(change).await
        }
        fn paused(&self) -> bool {
            false
        }
    }
    let flip = Flip(Fake::new(merge(true)));
    let agent = fixer();
    let ctx = LoopCtx { agent: &agent, maintainer: true, permission: Some("admin") };
    let gate = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(follow_gate(&flip, &target(HEAD), &merge(true), &ctx, true)).unwrap();
    assert_eq!(gate, Gate::Merge);
    assert!(matches!(&flip.0.posted.borrow()[..], [LoopNotice::Approved { .. }]));
}

#[test]
fn github_failures_in_a_row_are_told_once_then_the_streak_ends_on_success() {
    // Nov 17 morning: DNS failures. Each failed check is retried; after
    // UNREACHABLE_AFTER in a row the room hears it once, not every check.
    let fake = Fake::new(merge(true));
    let mut streaks = Streaks::default();
    let url = target(HEAD).url;
    let run = |failures: u32| tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(after_failure(&fake, &target(HEAD), failures)).unwrap();
    for expected in 1..=UNREACHABLE_AFTER + 3 {
        let failures = streaks.failed(&url);
        assert_eq!(failures, expected);
        run(failures);
        let told = fake.posted.borrow().len();
        assert_eq!(told, usize::from(failures >= UNREACHABLE_AFTER), "after {failures} failures");
    }
    assert!(fake.bodies()[0].starts_with("⚠️ Can't reach GitHub to merge PR #36 in csheargm/hagency-demo-sandbox — retrying"), "{}", fake.bodies()[0]);
    streaks.reached(&url);
    assert_eq!(streaks.failed(&url), 1, "a check that reached GitHub starts the count again");
    // The same head then merges normally: the unreachable step never took the merge step.
    assert_eq!(check(&fake, Some("admin"), true), Gate::Merge);
    assert!(matches!(fake.posted.borrow().last(), Some(LoopNotice::Approved { .. })));
}
