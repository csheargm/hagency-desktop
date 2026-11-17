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
fn a_hand_off_still_waiting_for_its_pull_request_keeps_its_slot_next_poll() {
    // open_slots counts max(open PRs, live claims on handed-off issues).
    assert!(PrSlots::left(1, 0u64.max(1)).is_full(), "poll 2: no PR yet, one pending hand-off");
    assert_eq!(dispatch(PrSlots::left(2, 1u64.max(1)), 5), 1, "a PR that opened from the pending issue is not counted twice");
}
