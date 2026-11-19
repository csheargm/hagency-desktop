//! PR #94 (Nov 18): the merge notice reached bird-claude while its token
//! budget was used up, was answered "budget reached, model not called", and
//! the claimed `merge` step was never sent again.
use super::*;
use crate::inbox::{Dispatch, Limits};

const OWNER: &str = "@alice:test";
/// The owner's account, which posts the loop's notices.
const ME: &str = "@alice:test";
const AGENT: &str = "agt_bird";
const PR: &str = "https://github.com/csheargm/hagency-demo-sandbox/pull/94";
const REPO: &str = "csheargm/hagency-demo-sandbox";
const HEAD: &str = "0aed2bbbefa935ed6b91ed2a657b60776c57e107";
/// 08:19:16 PST, Nov 18: the refused merge dispatch.
const T0: i64 = 1_795_019_956;
const MERGE: (&str, &str, &str) = ("merge", PR, HEAD);
/// The merging Agent's Matrix ID, which the merge notice addresses.
const BIRD: &str = "@bird:test";
const CODEX: &str = "@codex:test";
/// The room the loop's notices go to.
const ROOM: &str = "!room:test";

fn merge_body() -> String {
    format!("✅ Approved at {} — merging under the repo rules: @bird:test run exactly `{}` as its own command.", &HEAD[..12], merge_command(94, HEAD, REPO))
}
fn review_body() -> String {
    format!("🔎 Review requested — @codex:test {}: {PR}\nTo review: read it", review_ask(94, REPO, HEAD))
}

/// The desktop's "still a draft" notice, as `loop_notice::MarkReady` writes it.
fn ready_body() -> String {
    format!("{READY_NOTICE_LEAD}bird-claude (@bird:test) {}: if the fix is done, run exactly `gh pr ready 94 --repo {REPO}`. {PR}", ready_ask(94, REPO, HEAD))
}

fn ledger() -> (tempfile::TempDir, Ledger) {
    let temp = tempfile::tempdir().unwrap();
    let mut l = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    let mut policy = l.agent_policy(OWNER, AGENT).unwrap().policy;
    policy.budget = crate::Budget { limit: crate::Limit::Tokens(10_000_000), period: crate::Period::UtcDay };
    let scope = dispatch("x", "", ME).scope();
    l.register_binding(OWNER, &scope).unwrap();
    l.set_policy(OWNER, &scope, crate::Layer::Agent, 0, &policy).unwrap();
    (temp, l)
}
fn use_up_budget(l: &Ledger, now: i64) {
    let window = crate::Period::UtcDay.window(now).unwrap();
    l.db.execute("INSERT INTO accounts VALUES (?,?,10277116,0)", (crate::json(&("agent", AGENT)).unwrap(), &window)).unwrap();
}
fn dispatch(id: &str, body: &str, requester: &str) -> Dispatch {
    Dispatch {
        id: id.into(), binding_id: "bind".into(), agent_id: AGENT.into(), event_id: format!("${id}:test"), room_id: "!room:test".into(),
        requester_mxid: requester.into(), thread_root: format!("$root-{id}:test"), body: body.into(), state: "offered".into(),
        binding_generation: 1, dispatch_epoch: Some(1), dispatch_device_id: Some("device-a".into()), execution_id: None, outcome: None,
    }
}
/// Receive `body` at `at` and end it as `state`; `version` is the settled call's accounting.
fn receive(l: &mut Ledger, id: &str, body: &str, requester: &str, at: i64, end: Option<(&str, &str)>) {
    receive_dispatch(l, dispatch(id, body, requester), at, end);
}
fn receive_dispatch(l: &mut Ledger, d: Dispatch, at: i64, end: Option<(&str, &str)>) {
    let id = d.id.clone();
    l.receive_dispatch(OWNER, &d, Limits::default(), at).unwrap();
    let Some((state, version)) = end else { return };
    let execution = format!("exec-{id}");
    l.db.execute("UPDATE local_inbox SET state=?,execution_id=? WHERE id=?", (state, &execution, &id)).unwrap();
    let usage = format!(r#"{{"input":0,"output":0,"cached_input":0,"reasoning_output":0,"accounting_version":"{version}"}}"#);
    l.db.execute("INSERT INTO calls VALUES (?,?,?,'d',0,'[]','settled',?)", (&execution, &d.binding_id, crate::json(&d.scope()).unwrap(), usage)).unwrap();
}
const NO_RUN: Option<(&str, &str)> = Some(("replied", crate::inbox::NO_PROVIDER_CALL));
const RAN: Option<(&str, &str)> = Some(("replied", "claude-reported-v1"));
const LOST: Option<(&str, &str)> = Some(("failed", "claude-reported-v1"));

fn say(why: Why, gave_up: bool, round: u32) -> Option<Say> {
    Some(Say { why, gave_up, round })
}
fn due(l: &mut Ledger, now: i64) -> StepDue {
    l.step_due(AGENT, MERGE, (ME, BIRD, ROOM), now).unwrap()
}
fn said(l: &mut Ledger, round: u32) {
    l.step_line_said(AGENT, MERGE, round).unwrap();
}
fn sent_and_claimed(l: &mut Ledger, id: &str, at: i64, end: Option<(&str, &str)>) {
    receive(l, id, &merge_body(), ME, at, end);
    l.claim_dispatched_step(AGENT, "merge", PR, HEAD).unwrap();
}

#[test]
fn loop_notices_name_their_step_pull_request_and_head_exactly() {
    assert_eq!(loop_step_of(&merge_body()), Some(("merge", PR.to_owned(), HEAD.to_owned(), BIRD.to_owned())));
    assert_eq!(loop_step_of(&review_body()), Some(("review", PR.to_owned(), HEAD.to_owned(), CODEX.to_owned())));
    // The desktop's merge line names the Agent first: "bird-claude (@bird:test) run exactly …" (GPT review).
    let named = merge_body().replace("repo rules: @bird:test run", "room rules: bird-claude (@bird:test) run");
    assert_eq!(loop_step_of(&named), Some(("merge", PR.to_owned(), HEAD.to_owned(), BIRD.to_owned())), "{named}");
    for body in [
        "🔔 New GitHub issue #93 in csheargm/hagency-demo-sandbox".to_owned(),
        format!("run `gh pr merge 94 --squash --match-head-commit {} --repo {REPO}`", &HEAD[..12]),
        format!("run `gh pr merge 94 --squash --match-head-commit {HEAD} --repo not/a/repo`"),
        format!("gh pr merge 94 --rebase --match-head-commit {HEAD} --repo {REPO}"),
        format!("please review PR #94 in {REPO} at commit {HEAD}"),
        format!("> quoting: {}", merge_body()),
        format!("> quoting: {}", review_body()),
    ] {
        assert_eq!(loop_step_of(&body), None, "{body}");
    }
}

#[test]
fn pr_94_a_merge_refused_by_the_budget_is_held_then_sent_again_once_and_then_runs() {
    let (_temp, mut l) = ledger();
    use_up_budget(&l, T0);
    sent_and_claimed(&mut l, "merge-1", T0, NO_RUN);
    let held = due(&mut l, T0 + 120);
    assert_eq!(held, StepDue { taken: true, again: 0, say: say(Why::Budget, false, 0) }, "no re-send while the budget is used up; one line");
    assert_eq!(due(&mut l, T0 + 300).say, held.say, "offered again until the room accepted it");
    said(&mut l, 0);
    assert_eq!(due(&mut l, T0 + 600), StepDue { taken: true, again: 0, say: None }, "the line is said once");
    // The next UTC day: the budget allows again.
    let next_day = T0 + 86_400;
    assert_eq!(due(&mut l, next_day), StepDue { taken: false, again: 1, say: None }, "given back once");
    assert!(!l.loop_step_taken(AGENT, "merge", PR, HEAD).unwrap());
    assert_eq!(due(&mut l, next_day + 5), StepDue { taken: false, again: 1, say: None }, "never given back twice before the re-post");
    // The loop posts the merge again (as `again` 1) and claims it; this time the Agent runs.
    sent_and_claimed(&mut l, "merge-2", next_day + 6, None);
    assert_eq!(due(&mut l, next_day + 200), StepDue { taken: true, again: 1, say: None }, "running: wait");
    l.db.execute("UPDATE local_inbox SET state='replied' WHERE id='merge-2'", []).unwrap();
    assert!(l.claim_auto_merge(PR, HEAD, AGENT).unwrap(), "the gate takes the one automatic merge");
    assert_eq!(due(&mut l, next_day + 900), StepDue { taken: true, again: 1, say: None }, "a merge that ran is never sent again");
}

#[test]
fn a_step_whose_run_executed_is_never_sent_again() {
    let (_temp, mut l) = ledger();
    sent_and_claimed(&mut l, "merge-1", T0, RAN);
    for later in [T0 + 61, T0 + 86_400 * 3] {
        assert_eq!(due(&mut l, later), StepDue { taken: true, again: 0, say: None });
    }
    // A merge claimed through the gate stays done even if its dispatch reads as lost.
    let (_temp, mut l) = ledger();
    sent_and_claimed(&mut l, "merge-1", T0, LOST);
    l.claim_auto_merge(PR, HEAD, AGENT).unwrap();
    assert_eq!(due(&mut l, T0 + 600), StepDue { taken: true, again: 0, say: None });
}

#[test]
fn a_no_run_or_lost_run_is_sent_again_after_the_back_off_and_gives_up_after_the_cap() {
    let (_temp, mut l) = ledger();
    sent_and_claimed(&mut l, "merge-0", T0, LOST);
    assert_eq!(due(&mut l, T0 + 30), StepDue { taken: true, again: 0, say: say(Why::Lost, false, 0) }, "the line at once, the re-send after the back-off");
    said(&mut l, 0);
    assert_eq!(due(&mut l, T0 + RESEND_AFTER_SECS), StepDue { taken: false, again: 1, say: None });
    let mut at = T0 + RESEND_AFTER_SECS;
    for round in 1..=MAX_RESENDS {
        at += 10;
        sent_and_claimed(&mut l, &format!("merge-{round}"), at, NO_RUN);
        at += RESEND_AFTER_SECS;
        let last = round == MAX_RESENDS;
        let answer = due(&mut l, at);
        assert_eq!(answer.say, say(Why::NoRun, last, round), "round {round}");
        assert_eq!((answer.taken, answer.again), if last { (true, round) } else { (false, round + 1) }, "round {round}");
    }
    said(&mut l, MAX_RESENDS);
    assert_eq!(due(&mut l, at + 3_600), StepDue { taken: true, again: MAX_RESENDS, say: None }, "gave up: one line, no more re-sends");
}

#[test]
fn only_this_owners_own_notice_counts_and_unclaimed_steps_are_free() {
    let (_temp, mut l) = ledger();
    assert_eq!(due(&mut l, T0), StepDue { taken: false, again: 0, say: None });
    sent_and_claimed(&mut l, "merge-1", T0, RAN);
    // A room member's look-alike notice that was refused does not release ours.
    receive(&mut l, "fake", &merge_body(), "@mallory:test", T0 + 10, NO_RUN);
    assert_eq!(due(&mut l, T0 + 600), StepDue { taken: true, again: 0, say: None });
    // Our own notice of the same step to another Agent never stands in for this one's.
    receive(&mut l, "other", &merge_body().replace(BIRD, "@wren:test"), ME, T0 + 20, NO_RUN);
    assert_eq!(due(&mut l, T0 + 600), StepDue { taken: true, again: 0, say: None });
}

#[test]
fn a_review_is_asked_again_only_after_the_zero_call_proof() {
    let (_temp, mut l) = ledger();
    let review = ("review", PR, HEAD);
    receive(&mut l, "review-1", &review_body(), ME, T0, LOST);
    l.claim_dispatched_step(AGENT, "review", PR, HEAD).unwrap();
    assert!(l.step_due(AGENT, review, (ME, CODEX, ROOM), T0 + 600).unwrap().taken, "a lost review may have posted its verdict");
    receive(&mut l, "review-2", &review_body(), ME, T0 + 700, NO_RUN);
    let again = l.step_due(AGENT, review, (ME, CODEX, ROOM), T0 + 900).unwrap();
    assert_eq!((again.taken, again.again), (false, 1));
}

#[test]
fn a_refused_dispatch_is_found_however_many_requests_came_after_it() {
    let (_temp, mut l) = ledger();
    sent_and_claimed(&mut l, "merge-1", T0, NO_RUN);
    for i in 0..(crate::activity::MAX_RECENT + 5) {
        receive(&mut l, &format!("chat-{i}"), "hello", "@bob:test", T0 + 1 + i as i64, RAN);
    }
    assert_eq!(due(&mut l, T0 + 600).taken, false);
}

#[test]
fn a_merge_claimed_before_steps_were_tagged_is_sent_again_once_unless_it_merged() {
    // PR #94 after the update: its merge row predates the tags, no merge was taken.
    let (_temp, mut l) = ledger();
    assert!(l.claim_loop_step(AGENT, "merge", PR, HEAD).unwrap());
    assert!(l.claim_loop_step(AGENT, "review", PR, HEAD).unwrap());
    assert_eq!(due(&mut l, T0), StepDue { taken: false, again: 1, say: say(Why::Unrecorded, false, 0) });
    assert_eq!(l.step_due(AGENT, ("review", PR, HEAD), (ME, CODEX, ROOM), T0).unwrap(), StepDue { taken: true, again: 0, say: None }, "an old review is never asked again");
    l.claim_dispatched_step(AGENT, "merge", PR, HEAD).unwrap();
    assert_eq!(due(&mut l, T0 + 3_600), StepDue { taken: true, again: 1, say: None }, "once: the re-post is tagged from now on");
    let (_temp, mut l) = ledger();
    l.claim_loop_step(AGENT, "merge", PR, HEAD).unwrap();
    l.claim_auto_merge(PR, HEAD, AGENT).unwrap();
    assert_eq!(due(&mut l, T0).taken, true, "it merged: done");
}

#[test]
fn a_given_back_step_not_re_posted_yet_waits_while_the_budget_is_used_up_again() {
    let (_temp, mut l) = ledger();
    sent_and_claimed(&mut l, "merge-1", T0, NO_RUN);
    assert_eq!(due(&mut l, T0 + 120).taken, false, "given back; the re-post then failed to send");
    use_up_budget(&l, T0 + 200);
    assert_eq!(due(&mut l, T0 + 300), StepDue { taken: true, again: 1, say: None }, "no re-post into a used-up budget");
    assert_eq!(due(&mut l, T0 + 86_400).taken, false, "the next day it goes out");
}

#[path = "step_resend_scope_tests.rs"]
mod scope;

/// Round-2 GPT review (P2): the ready request reached an Agent that never
/// ran (a used-up budget, a signed-out provider), and its once-per-head step
/// stayed taken, so the draft was never readied nor reviewed. It is tagged
/// like the other dispatched steps and given back the same way.
#[test]
fn a_ready_request_the_agent_never_ran_is_given_back_like_a_merge() {
    assert_eq!(loop_step_of(&ready_body()), Some((READY_STEP, PR.to_owned(), HEAD.to_owned(), BIRD.to_owned())));
    assert_eq!(loop_step_of(&format!("> quoting: {}", ready_body())), None, "a quote is no notice");
    assert_eq!(loop_step_of(&ready_body().replace(HEAD, &HEAD[..12])), None, "only a full head pins the step");
    let (_temp, mut l) = ledger();
    let ready = (READY_STEP, PR, HEAD);
    receive(&mut l, "ready-1", &ready_body(), ME, T0, NO_RUN);
    l.claim_dispatched_step(AGENT, READY_STEP, PR, HEAD).unwrap();
    assert_eq!(l.step_due(AGENT, ready, (ME, BIRD, ROOM), T0 + 1).unwrap().taken, true, "after the back-off only");
    let due = l.step_due(AGENT, ready, (ME, BIRD, ROOM), T0 + RESEND_AFTER_SECS).unwrap();
    assert_eq!(due, StepDue { taken: false, again: 1, say: say(Why::NoRun, false, 0) }, "given back once, with one line");
    receive(&mut l, "ready-2", &ready_body(), ME, T0 + RESEND_AFTER_SECS + 5, RAN);
    l.claim_dispatched_step(AGENT, READY_STEP, PR, HEAD).unwrap();
    assert_eq!(l.step_due(AGENT, ready, (ME, BIRD, ROOM), T0 + 10 * RESEND_AFTER_SECS).unwrap().taken, true, "a request that ran stays taken");
}

/// Round-2 GPT review: a ready request claimed before the upgrade has no
/// round row nor tagged dispatch; it is given back once, never stuck.
#[test]
fn a_ready_request_claimed_before_the_upgrade_is_given_back_once() {
    let (_temp, mut l) = ledger();
    let ready = (READY_STEP, PR, HEAD);
    l.claim_loop_step(AGENT, READY_STEP, PR, HEAD).unwrap();
    let due = l.step_due(AGENT, ready, (ME, BIRD, ROOM), T0).unwrap();
    assert_eq!((due.taken, due.again), (false, 1), "{due:?}");
    assert_eq!(due.say.map(|s| s.why), Some(Why::Unrecorded));
}
