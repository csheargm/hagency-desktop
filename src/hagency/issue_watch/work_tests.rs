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

#[test]
fn a_pull_request_names_the_issues_it_closes_in_order() {
    use super::closing_issues;
    let r = "acme/app";
    assert_eq!(closing_issues("Closes #134", r), vec![134]);
    assert_eq!(closing_issues("Fixes #3, see #9.\nResolves https://github.com/acme/app/issues/5 and closes #3", r), vec![3, 5]);
    assert_eq!(closing_issues("Fixes other/app#12, see #4", r), Vec::<u64>::new(), "another repository, a mention");
    assert_eq!(closing_issues("closes #1 closes #2 closes #3 closes #4", r), vec![1, 2, 3], "at most three");
    assert_eq!(closing_issues("", r), Vec::<u64>::new());
}

#[test]
fn a_truncated_issue_listing_fails_the_cap_closed() {
    assert!(super::listing_truncated(1000));
    assert!(!super::listing_truncated(999));
}

/// GPT integration review: a failed room-rules save left the gate on the old
/// rules while the check still merged. That room's merges wait; others run.
#[test]
fn a_room_whose_rules_were_not_recorded_runs_no_merge_this_check() {
    let held: std::collections::HashSet<String> = ["!held:x".to_owned()].into();
    assert!(!super::super::work::merges_run(&held, "!held:x"));
    assert!(super::super::work::merges_run(&held, "!fresh:x"));
    assert!(super::super::work::merges_run(&std::collections::HashSet::new(), "!held:x"));
}
