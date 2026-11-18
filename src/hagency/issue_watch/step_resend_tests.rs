//! PR #94 (Nov 18) on the desktop side: the merge step's notice ended
//! without the Agent running; once the owner API gives the step back, the
//! merge is posted again ONCE as a new message, with one room line per
//! round, and a step that stays taken is never posted again.
use super::loop_notice::{LoopNotice, Target};
use super::merge_hold::{Gate, follow_gate};
use super::review_loop::{LoopCtx, LoopIo};
use matrix_sdk::ruma::owned_user_id;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::VecDeque;

const HEAD: &str = "0aed2bbbefa935ed6b91ed2a657b60776c57e107";

/// The owner API as a queue of `stepDue` answers; every other call is recorded.
#[derive(Default)]
struct Fake {
    due: RefCell<VecDeque<Value>>,
    posted: RefCell<Vec<(LoopNotice, u32)>>,
    asked: RefCell<Vec<Value>>,
}
impl LoopIo for Fake {
    async fn post(&self, notice: &LoopNotice, again: u32) -> Result<(), String> {
        self.posted.borrow_mut().push((notice.clone(), again));
        Ok(())
    }
    async fn review(&self, change: Value) -> Result<Value, String> {
        self.asked.borrow_mut().push(change.clone());
        if change.get("stepDue").is_some() {
            return Ok(json!({"due": self.due.borrow_mut().pop_front().expect("one answer per check")}));
        }
        Ok(json!({"taken": false, "claimed": true}))
    }
    fn paused(&self) -> bool {
        false
    }
    fn requester(&self) -> String {
        "@owner:alpha".into()
    }
    fn room(&self) -> String {
        "!watch:alpha".into()
    }
}

fn target() -> Target {
    Target { repo: "csheargm/hagency-demo-sandbox".into(), number: 94, url: "https://github.com/csheargm/hagency-demo-sandbox/pull/94".into(), head: HEAD.into() }
}
fn check(fake: &Fake) -> Gate {
    let agent = owned_user_id!("@_hagency_agt_bird_claude:alpha");
    let ctx = LoopCtx { agent: &agent, name: "bird-claude", maintainer: true, permission: Some("admin") };
    let ready = json!({"decision": "merge", "head": HEAD, "approvers": 1, "autoMerge": true});
    tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(follow_gate(fake, &target(), &ready, &ctx, true)).unwrap()
}

#[test]
fn pr_94_the_refused_merge_is_held_on_the_budget_then_posted_again_once() {
    let fake = Fake::default();
    fake.due.borrow_mut().extend([
        json!({"taken": true, "again": 0, "say": {"why": "budget", "gaveUp": false, "round": 0}}),
        json!({"taken": true, "again": 0}),
        json!({"taken": false, "again": 1}),
        json!({"taken": true, "again": 1}),
        json!({"taken": true, "again": 1}),
    ]);
    for _ in 0..5 {
        assert_eq!(check(&fake), Gate::Merge);
    }
    let posted = fake.posted.borrow();
    assert_eq!(posted.len(), 2, "{posted:?}");
    let (line, _) = &posted[0];
    assert!(matches!(line, LoopNotice::NotStarted { .. }));
    assert_eq!(line.body(), "⏸️ Merge of PR #94 in csheargm/hagency-demo-sandbox could not start — the Agent's token budget for this period is used up; it will retry when the budget allows. https://github.com/csheargm/hagency-demo-sandbox/pull/94");
    assert!(matches!(&posted[1], (LoopNotice::Approved { .. }, 1)), "re-sent once, as a new message: {posted:?}");
    let asked = fake.asked.borrow();
    assert_eq!(asked[0]["stepDue"], json!({"step": "merge", "pr": target().url, "head": HEAD, "requester": "@owner:alpha", "to": "@_hagency_agt_bird_claude:alpha", "room": "!watch:alpha"}), "only this room's dispatches count");
    assert_eq!(asked.iter().filter(|c| c.get("claimStep").is_some()).count(), 1, "claimed again after the re-post");
    assert_eq!(asked[1]["stepSaid"]["round"], 0, "the line is acknowledged once posted");
}

#[test]
fn a_re_send_is_a_new_message_and_each_round_has_its_own_line() {
    let agent = owned_user_id!("@_hagency_agt_bird_claude:alpha");
    let approved = LoopNotice::Approved { target: target(), fixer: agent, name: "bird-claude".into() };
    assert_eq!(approved.txn_id_again("agt", 0), approved.txn_id("agt"), "the first post keeps its id");
    assert_ne!(approved.txn_id_again("agt", 1), approved.txn_id("agt"));
    assert_ne!(approved.txn_id_again("agt", 1), approved.txn_id_again("agt", 2));
    let line = |round: u32, why: &str, gave_up: bool| LoopNotice::NotStarted { target: target(), what: "merge".into(), why: why.into(), gave_up, step: format!("not-started-merge-{round}") };
    assert_ne!(line(0, "noRun", false).txn_id("agt"), line(1, "noRun", false).txn_id("agt"));
    assert!(line(1, "noRun", false).body().starts_with("🔁 Merge of PR #94 in csheargm/hagency-demo-sandbox could not start — an owner limit"));
    assert!(line(3, "lost", true).body().contains("will not be retried"), "the give-up line");
    assert!(line(0, "unseen", false).body().contains("no run of it reached this device") && line(0, "unseen", false).body().ends_with("it will retry. https://github.com/csheargm/hagency-demo-sandbox/pull/94"));
    assert!(line(1, "unseen", true).body().contains("it was sent again and will not be retried"), "never claims three re-sends");
    assert!(approved.body().contains(&::hagency::issue_watch::merge_command(94, HEAD, "csheargm/hagency-demo-sandbox")), "the ledger reads back this exact command");
}
