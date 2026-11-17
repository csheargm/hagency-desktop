//! The open-pull-request cap across one pass (GPT review P1).
use super::PrSlots;

/// Mirrors `develop`: stop when full, take one slot per hand-off.
fn dispatch(mut slots: PrSlots, eligible: usize) -> usize {
    let mut handed = 0;
    for _ in 0..eligible {
        if slots.is_full() {
            break;
        }
        handed += 1;
        slots.take(1);
    }
    handed
}

#[test]
fn a_cap_of_one_with_none_open_hands_off_one_of_five() {
    assert_eq!(dispatch(PrSlots::left(1, 0), 5), 1);
}

#[test]
fn the_cap_counts_pull_requests_already_open() {
    assert_eq!(dispatch(PrSlots::left(3, 1), 5), 2);
    assert!(PrSlots::left(2, 5).is_full(), "more open than the cap is full, not an underflow");
}

#[test]
fn no_cap_hands_off_every_eligible_issue() {
    assert_eq!(dispatch(PrSlots::Unlimited, 5), 5);
}

#[test]
fn a_failed_hand_off_keeps_its_slot() {
    let mut slots = PrSlots::left(1, 0);
    slots.take(0);
    assert!(!slots.is_full());
}

#[test]
fn a_pending_hand_off_and_an_unrelated_open_pr_both_take_a_slot() {
    // open_slots: used = open PRs + live claims that no open PR closes yet.
    assert!(PrSlots::left(2, 1 + 1).is_full(), "cap 2: one unrelated PR plus one hand-off awaiting its PR");
    assert!(PrSlots::left(1, 1).is_full(), "poll 2: no PR yet, one pending hand-off");
}

#[test]
fn only_a_closing_keyword_links_a_pull_request_to_its_issue() {
    use super::closes_issue;
    let r = "acme/app";
    assert!(closes_issue("Adds a glossary.\n\nCloses #12", r, 12));
    assert!(closes_issue("fixes #12.", r, 12));
    assert!(closes_issue("Fixes Acme/App#12", r, 12));
    assert!(closes_issue("Resolves https://github.com/acme/app/issues/12", r, 12));
    assert!(!closes_issue("Fixes other/app#12", r, 12), "another repository");
    assert!(!closes_issue("Closes #123", r, 12), "another issue");
    assert!(!closes_issue("see #12", r, 12), "a mention is no link");
    assert!(!closes_issue("encloses #12", r, 12));
}
