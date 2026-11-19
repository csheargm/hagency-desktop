//! GPT batch-3 review: a finished record of an issue opened before the
//! work-list entry stays hidden after it falls off the capped open list.
//! Issue numbers are this file's own: the remembered ages are process-wide.
use super::super::status_publish::shown;
use super::super::tracker::{Inputs, follow};
use super::super::tracker_tests::{Fake, REPO, T0, issue};
use super::{hidden, unknown};
use ::hagency::issue_watch::Issue;
use ::hagency::issue_watch::issue_track::{IssueFact, State, Track};
use std::collections::HashSet;

const OLD: i64 = T0 - 2 * 86_400;

fn finished(number: i64, state: State, handed_at: i64) -> Track {
    Track { repo: REPO.into(), number, state, attempts: 1, handed_at, pr: None, notified: true, free_retries: 0, changed_at: handed_at }
}

fn pass(fake: &Fake, tracks: &[Track], open: &[Issue]) -> Vec<i64> {
    let seen = HashSet::new();
    let inputs = Inputs { repo: REPO, now: T0 + 60, tracks, seen: &seen, since: T0, open_issues: open, own_pulls: &[] };
    let out = tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(follow(fake, &inputs));
    assert!(out.errors.is_empty(), "{:?}", out.errors);
    out.predating
}

fn numbers(tracks: Vec<Track>) -> Vec<i64> {
    tracks.iter().map(|t| t.number).collect()
}

#[test]
fn a_needs_human_record_of_an_old_issue_stays_hidden_once_off_the_list() {
    let fake = Fake::default();
    // Handed off after the entry by a single-repository watch: only the issue's age tells.
    let tracks = [finished(9031, State::NeedsHuman, T0 + 10)];
    let listed = [Issue { created_at: OLD, ..issue(9031) }];
    assert_eq!(pass(&fake, &tracks, &listed), [9031], "listed: hidden as before");
    // Off the list, GitHub would now say "new": the remembered age answers, no read.
    fake.closed.borrow_mut().insert(9031, IssueFact::Open);
    fake.created.borrow_mut().insert(9031, Some(T0 + 1));
    assert_eq!(pass(&fake, &tracks, &[]), [9031]);
    assert!(numbers(shown(tracks.to_vec(), &[], T0, &[])).is_empty(), "never published again");
}

#[test]
fn a_finished_record_off_the_list_is_judged_by_its_hand_off_or_one_lookup() {
    let fake = Fake::default();
    for (n, created) in [(9032, Some(OLD)), (9033, Some(T0 + 5)), (9035, Some(T0 + 5))] {
        fake.closed.borrow_mut().insert(n, IssueFact::Done);
        fake.created.borrow_mut().insert(n, created);
    }
    // #9034's lookup fails: unknown, so hidden. #9035 was handed off before
    // the entry: older without a read (GitHub's "new" is never asked).
    let tracks = [9032, 9033, 9034].map(|n| finished(n, State::Merged, T0 + 10)).into_iter().chain([finished(9035, State::Failed, T0 - 1_000)]).collect::<Vec<_>>();
    assert!(unknown(&tracks[2], &[], T0), "a restart's first check looks it up");
    assert!(hidden(&tracks[2], &[], T0) && hidden(&tracks[3], &[], T0), "before any read: hidden");
    assert!(!unknown(&tracks[3], &[], T0), "the hand-off time answers");
    assert_eq!(pass(&fake, &tracks, &[]), [9032, 9034, 9035]);
    assert_eq!(numbers(shown(tracks.clone(), &[], T0, &[])), [9033], "a newer finished issue is still published");
    assert!(!hidden(&tracks[1], &[], i64::MIN), "a single-repository watch has no cutoff");
}
