//! Integration GPT review (Nov 18) of the step re-send: a dispatch in
//! another room never stands in for this room's, a used-up room budget
//! holds the re-send like the Agent's own, and a claimed step whose notice
//! never reached an Agent here is given back once, after a while.
use super::*;

const ELSEWHERE: &str = "!elsewhere:test";

/// `body` received in `room` on its own binding, ended as `end`.
fn receive_in(l: &mut Ledger, id: &str, room: &str, at: i64, end: Option<(&str, &str)>) {
    let mut d = dispatch(id, &merge_body(), ME);
    (d.binding_id, d.room_id) = (format!("bind-{id}"), room.into());
    l.register_binding(OWNER, &d.scope()).unwrap();
    receive_dispatch(l, d, at, end);
}
fn claim(l: &mut Ledger, step: &str) {
    l.claim_dispatched_step(AGENT, step, PR, HEAD).unwrap();
}

#[test]
fn a_refused_dispatch_in_another_room_never_gives_this_rooms_step_back() {
    let (_temp, mut l) = ledger();
    claim(&mut l, "merge");
    receive_in(&mut l, "merge-b", ELSEWHERE, T0, NO_RUN);
    assert_eq!(due(&mut l, T0 + 600), StepDue { taken: true, again: 0, say: None }, "room B's refusal is not room A's");
    let theirs = l.step_due(AGENT, MERGE, (ME, BIRD, ELSEWHERE), T0 + 600).unwrap();
    assert_eq!((theirs.taken, theirs.again), (false, 1), "in its own room it is given back");
}

#[test]
fn a_used_up_room_budget_holds_the_re_send_like_the_agents_own() {
    let (_temp, mut l) = ledger();
    let scope = dispatch("x", "", ME).scope();
    let mut policy = l.agent_policy(OWNER, AGENT).unwrap().policy;
    policy.budget = crate::Budget { limit: crate::Limit::Tokens(100), period: crate::Period::UtcDay };
    l.set_policy(OWNER, &scope, crate::Layer::Room, 0, &policy).unwrap();
    let window = crate::Period::UtcDay.window(T0).unwrap();
    l.db.execute("INSERT INTO accounts VALUES (?,?,100,0)", (&scope.layers().unwrap()[1], &window)).unwrap();
    sent_and_claimed(&mut l, "merge-1", T0, NO_RUN);
    assert_eq!(due(&mut l, T0 + 120), StepDue { taken: true, again: 0, say: say(Why::Budget, false, 0) }, "no re-send into a used-up room budget");
    assert_eq!(due(&mut l, T0 + 86_400).again, 1, "the next day it goes out");
}

#[test]
fn a_step_with_no_dispatch_here_is_given_back_once_after_the_wait() {
    let (_temp, mut l) = ledger();
    claim(&mut l, "merge");
    assert_eq!(due(&mut l, T0), StepDue { taken: true, again: 0, say: None }, "the wait starts when it is first found");
    assert_eq!(due(&mut l, T0 + UNSEEN_AFTER_SECS - 1).taken, true);
    assert_eq!(due(&mut l, T0 + UNSEEN_AFTER_SECS), StepDue { taken: false, again: 1, say: say(Why::Unseen, false, 0) }, "given back once");
    claim(&mut l, "merge");
    let later = T0 + UNSEEN_AFTER_SECS + 10;
    assert_eq!(due(&mut l, later).say, None, "a new wait for the re-post");
    let gave_up = due(&mut l, later + UNSEEN_AFTER_SECS);
    assert_eq!(gave_up, StepDue { taken: true, again: 1, say: say(Why::Unseen, true, 1) }, "then one line and no more re-sends");
    l.step_line_said(AGENT, MERGE, 1).unwrap();
    assert_eq!(due(&mut l, later + 10 * UNSEEN_AFTER_SECS), StepDue { taken: true, again: 1, say: None });
}

#[test]
fn a_seen_dispatch_or_a_done_review_never_reads_as_unseen() {
    let (_temp, mut l) = ledger();
    sent_and_claimed(&mut l, "merge-1", T0, RAN);
    assert!(due(&mut l, T0 + 60).taken);
    // The inbox dropped the request and its tag later: still seen.
    l.db.execute("DELETE FROM loop_step_dispatches", []).unwrap();
    assert_eq!(due(&mut l, T0 + 10 * UNSEEN_AFTER_SECS), StepDue { taken: true, again: 0, say: None });
    // A review whose merge step was taken is done, wherever it ran.
    let (_temp, mut l) = ledger();
    claim(&mut l, "review");
    l.claim_loop_step(AGENT, "merge", PR, "f".repeat(40).as_str()).unwrap();
    let review = |l: &mut Ledger, now| l.step_due(AGENT, ("review", PR, HEAD), (ME, CODEX, ROOM), now).unwrap();
    review(&mut l, T0);
    assert_eq!(review(&mut l, T0 + UNSEEN_AFTER_SECS).again, 1, "another head's merge says nothing about this one");
    claim(&mut l, "review");
    l.claim_loop_step(AGENT, "merge", PR, HEAD).unwrap();
    review(&mut l, T0 + UNSEEN_AFTER_SECS + 1);
    assert_eq!(review(&mut l, T0 + 3 * UNSEEN_AFTER_SECS), StepDue { taken: true, again: 1, say: None }, "its merge step was taken: never asked again");
}

#[test]
fn another_rooms_approval_never_stands_in_for_this_rooms_review() {
    // GPT review: an approval of the head given in room B kept room A's
    // never-delivered review claimed, though A's merge needs its own.
    let (_temp, mut l) = ledger();
    l.record_agent_review(PR, HEAD, "wren", ELSEWHERE).unwrap();
    claim(&mut l, "review");
    let review = |l: &mut Ledger, now| l.step_due(AGENT, ("review", PR, HEAD), (ME, CODEX, ROOM), now).unwrap();
    review(&mut l, T0);
    assert_eq!(review(&mut l, T0 + UNSEEN_AFTER_SECS).again, 1, "asked again in its own room");
    claim(&mut l, "review");
    l.record_agent_review(PR, HEAD, "wren", ROOM).unwrap();
    review(&mut l, T0 + UNSEEN_AFTER_SECS + 1);
    assert!(review(&mut l, T0 + 3 * UNSEEN_AFTER_SECS).taken, "approved in this room: done");
}
