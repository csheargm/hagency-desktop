//! Reviewer rotation (`reviewer_pass`): a reviewer that never answers a
//! head's review request is passed over for the next one.
use super::*;

/// GPT batch review: a paused remote Codex reviewer (its profile publishes a
/// runtime, nothing says it is active) never answered, and every re-send of the
/// head's review went to it. Once its rounds are spent it is passed over, and
/// the next reviewer gets a round of its own.
#[test]
fn a_reviewer_that_never_answers_is_passed_over_for_the_next_one() {
    const CLAUDE: &str = "@claude:test";
    let (_temp, mut l) = ledger();
    let review = ("review", PR, HEAD);
    let ask = |l: &mut Ledger, to: &str, now: i64| l.step_due(AGENT, review, (ME, to, ROOM), now).unwrap();
    l.claim_dispatched_step(AGENT, "review", PR, HEAD).unwrap();
    assert!(ask(&mut l, CODEX, T0).taken, "no dispatch here yet: it waits");
    assert_eq!(ask(&mut l, CODEX, T0 + UNSEEN_AFTER_SECS), StepDue { taken: false, again: 1, say: say(Why::Unseen, false, 0) });
    let t1 = T0 + UNSEEN_AFTER_SECS + 10;
    l.claim_dispatched_step(AGENT, "review", PR, HEAD).unwrap();
    assert!(ask(&mut l, CODEX, t1).taken);
    assert_eq!(ask(&mut l, CODEX, t1 + UNSEEN_AFTER_SECS), StepDue { taken: true, again: 1, say: say(Why::Unseen, true, 1) });
    assert_eq!(l.passed_reviewers(AGENT, PR, HEAD).unwrap(), [CODEX]);
    assert_eq!(ask(&mut l, CLAUDE, t1 + UNSEEN_AFTER_SECS + 1), StepDue { taken: false, again: 2, say: None }, "the next reviewer is asked, as a new message");
    let t2 = t1 + UNSEEN_AFTER_SECS + 5;
    l.claim_dispatched_step(AGENT, "review", PR, HEAD).unwrap();
    assert!(ask(&mut l, CLAUDE, t2).taken);
    assert_eq!(ask(&mut l, CLAUDE, t2 + UNSEEN_AFTER_SECS).again, 3, "its own re-send, not the one Codex spent");
    assert!(l.passed_reviewers(AGENT, PR, &"b".repeat(40)).unwrap().is_empty(), "a new head starts over");
}

/// GPT review of the fix: a pick that changes mid-round never passes the new
/// reviewer on the old one's timer, and a used-up budget never passes one.
#[test]
fn only_a_reviewer_that_was_asked_and_never_ran_is_passed_over() {
    const CLAUDE: &str = "@claude:test";
    let review = ("review", PR, HEAD);
    let (_temp, mut l) = ledger();
    assert!(!l.step_due(AGENT, review, (ME, CODEX, ROOM), T0).unwrap().taken, "first asked: Codex");
    l.claim_dispatched_step(AGENT, "review", PR, HEAD).unwrap();
    let late = T0 + 2 * UNSEEN_AFTER_SECS;
    assert_eq!(l.step_due(AGENT, review, (ME, CLAUDE, ROOM), late).unwrap(), StepDue { taken: false, again: 1, say: None }, "a new pick gets its own round");
    assert!(l.passed_reviewers(AGENT, PR, HEAD).unwrap().is_empty());
    let (_temp, mut l) = ledger();
    for round in 0..MAX_RESENDS as i64 {
        let at = T0 + round * 10 * RESEND_AFTER_SECS;
        receive(&mut l, &format!("review-{round}"), &review_body(), ME, at, NO_RUN);
        l.claim_dispatched_step(AGENT, "review", PR, HEAD).unwrap();
        assert!(!l.step_due(AGENT, review, (ME, CODEX, ROOM), at + RESEND_AFTER_SECS).unwrap().taken);
    }
    let at = T0 + 100 * RESEND_AFTER_SECS;
    receive(&mut l, "review-last", &review_body(), ME, at, NO_RUN);
    l.claim_dispatched_step(AGENT, "review", PR, HEAD).unwrap();
    use_up_budget(&l, at + 1);
    assert_eq!(l.step_due(AGENT, review, (ME, CODEX, ROOM), at + RESEND_AFTER_SECS).unwrap().say.map(|s| s.why), Some(Why::Budget));
    assert!(l.passed_reviewers(AGENT, PR, HEAD).unwrap().is_empty(), "a budget hold is no pass");
}
