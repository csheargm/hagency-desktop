//! The daily digest's integration-GPT-review regressions as user flows (R6):
//! a move just before the cutoff, a hold that cleared, and a card that went
//! unanswered after the cutoff or behind heavy traffic.
use super::digest::{self, Inputs, SEND_HOUR, Window, summarize, window_at};
use super::digest_live::send_due;
use super::digest_tests::{Fake, ME, REPO, at, card, run, saved, today, track};
use super::status_publish::entries;
use ::hagency::issue_watch::issue_track::digest::{DigestLog, TrackStep};
use ::hagency::issue_watch::issue_track::{Track, TrackChange, apply_change};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;

/// One record moved through `moves` with the ledger's rule, and the step the
/// ledger logs for each move (`issue_track::digest::record_step`).
fn logged(number: i64, moves: &[(i64, Value)]) -> (Track, Vec<TrackStep>) {
    let (mut current, mut steps): (Option<Track>, Vec<TrackStep>) = (None, Vec::new());
    for (now, extra) in moves {
        let change: TrackChange = serde_json::from_value(super::tracker::change_of(REPO, number, extra.clone())).unwrap();
        let next = apply_change(current.clone(), &change, *now).unwrap();
        if next.changed_at == *now && current.as_ref() != Some(&next) {
            steps.push(TrackStep { repo: next.repo.clone(), number, state: next.state, pr: next.pr, notified: next.notified, at: *now });
        }
        current = Some(next);
    }
    (current.unwrap(), steps)
}
fn tomorrow() -> Window {
    window_at(&at(19, SEND_HOUR, 5)).unwrap()
}

/// Reviewed 17:50, merged 18:02, digest at 18:05: today's digest still shows
/// the review, and tomorrow's the merge — each move in its own day.
#[test]
fn a_move_before_the_cutoff_counts_today_after_a_later_move() {
    let w = today();
    let (merged, steps) = logged(33, &[(w.end - 3600, json!({"handed": 1})), (w.end - 1800, json!({"state": "pr_opened", "pr": 41})), (w.end - 600, json!({"state": "reviewed"})), (w.end + 120, json!({"state": "merged", "notified": true}))]);
    let tracks = [merged];
    let day = summarize(REPO, &Inputs { tracks: &tracks, steps: &steps, ..Inputs::default() }, &w).expect("the review moved today");
    assert_eq!((day.fixed, day.merged, day.items.clone()), (0, 0, vec!["#33 → PR #41 in review".to_owned()]));
    let next = summarize(REPO, &Inputs { tracks: &tracks, steps: &steps, ..Inputs::default() }, &tomorrow()).unwrap();
    assert_eq!((next.fixed, next.merged, next.items), (1, 1, vec!["#33 → PR #41 merged".to_owned()]));
    // A record without logged steps (an older ledger) still counts by its own last move.
    assert_eq!(summarize(REPO, &Inputs { tracks: &tracks, ..Inputs::default() }, &tomorrow()).map(|d| d.fixed), Some(1));
}

/// Held 17:00 (the hold notice set `notified`), then another Agent approved
/// and the merge is underway: the ledger record is unchanged, the gate is not.
#[test]
fn a_pull_request_whose_hold_cleared_does_not_need_the_owner_while_it_merges() {
    let w = today();
    let held = track(36, &[(w.start + 60, json!({"handed": 1})), (w.start + 90, json!({"state": "pr_opened", "pr": 43})), (w.end - 3600, json!({"state": "reviewed", "notified": true}))]);
    assert!(held.notified, "the sticky flag the old digest read");
    let tracks = [held];
    let merging = summarize(REPO, &Inputs { tracks: &tracks, ..Inputs::default() }, &w).unwrap();
    assert_eq!((merging.need, merging.items.clone()), (0, vec!["#36 → PR #43 in review".to_owned()]));
    let still = summarize(REPO, &Inputs { tracks: &tracks, needs_owner: &[36], ..Inputs::default() }, &w).unwrap();
    assert_eq!((still.need, still.items), (1, vec!["#36 → PR #43 needs you: https://github.com/acme/app/pull/43".to_owned()]));
}

/// The gate's holds come from the published entries: running CI needs nobody.
#[test]
fn the_gate_counts_holds_for_the_owner_but_not_running_ci() {
    let w = today();
    let tracks: Vec<Track> = [41, 42, 43].iter().map(|n| track(*n, &[(w.start, json!({"handed": 1})), (w.start + 1, json!({"state": "reviewed", "pr": *n as u64 + 100}))])).collect();
    let holds = [(41, "green CI".to_owned()), (42, super::tracker::hold_of(&super::tracker::Followed::WaitingCi).unwrap())];
    let published = entries("@_hagency_agt_b:x", REPO, &tracks, &HashMap::new(), &holds, w.end).unwrap();
    assert_eq!(super::digest_gate::owner_holds(&published), vec![41]);
    assert_eq!(super::digest_gate::needs_owner("@_hagency_agt_b:x", "Acme/App"), None, "unknown before a follow");
    super::digest_gate::note("@_hagency_agt_b:x", REPO, &published);
    assert_eq!(super::digest_gate::needs_owner("@_hagency_agt_b:x", "Acme/App"), Some(vec![41]));
}

/// A card received 17:40 that went unanswered at 18:10 counts tomorrow, not
/// never; another person's hand-off and another repository's never do.
#[test]
fn a_card_counts_in_the_day_it_went_unanswered() {
    let w = today();
    let mut theirs = card(40, w.start + 5);
    theirs.requester = "@anna:example.org".into();
    let mut elsewhere = card(41, w.start + 5);
    elsewhere.repo = "acme/other".into();
    let cards = [card(38, w.end + 600), theirs, elsewhere];
    assert!(digest::unanswered_issues(&cards, ME, REPO, &w).is_empty());
    assert_eq!(digest::unanswered_issues(&cards, ME, REPO, &tomorrow()), vec![38]);
}

/// The flow: a day whose only news is cards behind heavy traffic is posted
/// from the ledger's log; just after a restart it waits for the gate's word.
#[test]
fn cards_behind_heavy_traffic_are_posted_once_the_gate_is_known() {
    let w = today();
    let log = DigestLog { steps: Vec::new(), cards: (1..=30).map(|n| card(n, w.start + n)).collect() };
    let fake = Fake { log, gate_unknown: true, tracks: RefCell::new(Vec::new()), ..Fake::default() };
    assert!(run(send_due(&fake, &saved(&fake, &[]), Some(w.clone()))).is_empty());
    assert!(fake.posts.borrow().is_empty() && fake.sent.borrow().is_empty(), "the day waits for a follow");
    let known = Fake { gate_unknown: false, log: fake.log.clone(), ..Fake::default() };
    assert!(run(send_due(&known, &saved(&known, &[]), Some(w))).is_empty());
    let posts = known.posts.borrow();
    assert!(posts[0].1.contains("Today: 0 merged, 30 need you"), "{:?}", posts);
}
