//! Hand-offs whose runs never started the model (Nov 18 08:16-08:19: #93's
//! run used up bird-claude's daily token budget; #90-#92 were each handed
//! off three times in three minutes, every run answered "budget reached,
//! model not called", and all three were marked failed), on the tracker's
//! fakes: held while the budget is used up, then re-handed without an
//! attempt, never twice within the back-off.
use super::run_end::{RunEnd, run_end_in};
use super::tracker_tests::{Fake, T0, check, first_hand_off, issue};
use ::hagency::issue_watch::issue_track::{MAX_FREE_RETRIES, REHAND_BACKOFF_SECS, State};
use serde_json::json;

const ME: &str = "@betty:x";

#[test]
fn three_hand_offs_refused_for_the_budget_are_held_not_failed() {
    let fake = Fake::default();
    for n in [90, 91, 92] {
        first_hand_off(&fake, n);
    }
    let open = [issue(90), issue(91), issue(92)];
    fake.run.set(Some(RunEnd::NoRun { budget_out: true }));
    for minute in 1..=10 {
        check(&fake, T0 + minute * 60, &open, &[]);
    }
    assert!(fake.handed.borrow().is_empty(), "nothing re-posted while every run would be refused");
    assert_eq!(fake.keys(), ["90:budget-1-0", "91:budget-1-0", "92:budget-1-0"], "one line per issue, not one per check");
    assert!(fake.said.borrow()[0].2.starts_with("⏸️ #90: bird-claude's token budget for this period is used up"));
    for track in fake.tracks() {
        assert_eq!((track.state, track.attempts, track.free_retries), (State::HandedOff, 1, 0), "#{}", track.number);
    }
    // The UTC day turns: each issue gets its own run again, not counted.
    fake.run.set(Some(RunEnd::NoRun { budget_out: false }));
    assert_eq!(check(&fake, T0 + 11 * 60, &open, &[]), ["#90 retrying (1/3)", "#91 retrying (1/3)", "#92 retrying (1/3)"]);
    assert_eq!(*fake.handed.borrow(), [(90, 1), (91, 1), (92, 1)], "the same attempt");
    assert!(fake.said.borrow()[3].2.starts_with("🔁 #90: the run ended before the model started"));
    assert!(fake.tracks().iter().all(|t| t.attempts == 1 && t.free_retries == 1));
}

#[test]
fn a_refused_run_is_re_handed_only_after_the_back_off_then_counts_at_the_cap() {
    let fake = Fake::default();
    first_hand_off(&fake, 90);
    fake.run.set(Some(RunEnd::NoRun { budget_out: false }));
    // 50 s after the hand-off, as on Nov 18: too soon.
    assert_eq!(check(&fake, T0 + 50, &[issue(90)], &[]), ["#90 waiting for a pull request"]);
    assert!(fake.handed.borrow().is_empty());
    let mut at = T0;
    for _ in 0..MAX_FREE_RETRIES {
        at += REHAND_BACKOFF_SECS;
        check(&fake, at, &[issue(90)], &[]);
    }
    assert_eq!(*fake.handed.borrow(), [(90, 1), (90, 1)], "free re-hand-offs, each a back-off apart");
    check(&fake, at + 1, &[issue(90)], &[]);
    assert_eq!(fake.handed.borrow().len(), 2, "never within the back-off");
    check(&fake, at + REHAND_BACKOFF_SECS, &[issue(90)], &[]);
    assert_eq!(fake.handed.borrow().last(), Some(&(90, 2)), "a refusal loop still ends: then it counts");
}

#[test]
fn the_activity_view_names_a_no_run_and_whether_the_budget_is_used_up() {
    let fake = Fake::default();
    first_hand_off(&fake, 92);
    let track = &fake.tracks()[0];
    let req = |extra: serde_json::Value| {
        let mut r = json!({"requester": ME, "receivedAt": T0 + 1, "outcome": "answered", "issue": {"repo": "acme/app", "number": 92}});
        r.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        r
    };
    let view = |extra, used_up| json!({"requests": [req(extra)], "budgetUsedUp": used_up});
    assert_eq!(run_end_in(&view(json!({"noRun": true}), true), ME, track), RunEnd::NoRun { budget_out: true });
    assert_eq!(run_end_in(&view(json!({"noRun": true}), false), ME, track), RunEnd::NoRun { budget_out: false });
    assert_eq!(run_end_in(&view(json!({}), true), ME, track), RunEnd::Ended, "a model reply is a real end, budget or not");
    assert_eq!(run_end_in(&view(json!({"noRun": true}), true), "@other:x", track), RunEnd::Running, "only this account's hand-offs");
}
