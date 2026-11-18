//! Trial mode (ADR 0028), end to end through the gate: a new watch's first
//! merges go to the owner, the owner's merges end the trial, and nothing an
//! Agent or room member does can end it.
use super::*;
use crate::issue_track::TrackChange;
use crate::issue_watch::{WatchMode, WatchSetting};
use crate::merge_policy::gate::{self, GhPr, Kind};
use crate::merge_policy::review_loop::merge_readiness;
use crate::merge_policy::review_loop::tests::Src;
use crate::merge_policy::tests::{bash, file, ledger_with, HEAD};
use crate::merge_policy::work::{OwnerRules, Role};
use crate::{Error, Ledger};
use std::path::Path;

const A: &str = "csheargm/hagency-demo-sandbox";
const OWNER: &str = "@owner:example.org";
const MEMBER: &str = "@member:example.org";

fn rules() -> OwnerRules {
    OwnerRules { repo: A.into(), role: Role::Maintainer, room_id: "!r:x".into(), trusted_authors: vec!["betty".into()], any_path: true, ..Default::default() }
}
fn anna(dir: &Path) -> Ledger {
    let mut ledger = ledger_with(dir, &["anna"]);
    ledger.set_auto_merge(OWNER, "anna", true).unwrap();
    ledger.set_work(OWNER, "anna", vec![rules()]).unwrap();
    ledger
}
/// Anna's Agent reviews (settled), then asks to merge PR 7: the gate's answer.
async fn review_then_merge(ledger: &mut Ledger, dir: &Path, src: &Src, call: &str) -> Option<bool> {
    let review = format!("gh pr review 7 --repo {A} --approve -b \"Hagency verdict: APPROVE {HEAD} - ok\"");
    let ok = gate::auto_answer(ledger, OWNER, &bash(&review, dir, "anna"), &format!("rv-{call}"), 11, src).await;
    ledger.settle_agent_review(&format!("rv-{call}"), ok == Some(true)).unwrap();
    let merge = format!("gh pr merge 7 --squash --match-head-commit {HEAD} --repo {A}");
    gate::auto_answer(ledger, OWNER, &bash(&merge, dir, "anna"), call, 11, src).await
}
/// Issue `issue` was fixed by pull request `pr`, which GitHub reports merged
/// (by the owner's own login when `by_owner`): the owner API's merged change.
fn merged_by(ledger: &mut Ledger, issue: i64, pr: u64, by_owner: bool) -> TrackChange {
    let change = |handed, state, pr| TrackChange { repo: A.into(), number: issue, handed, state, pr, notified: None, rehanded: None, owner_merged: Some(by_owner) };
    ledger.track_issue("anna", &change(Some(1), None, None), 1).unwrap();
    ledger.track_issue("anna", &change(None, Some(State::PrOpened), Some(pr)), 2).unwrap();
    let done = change(None, Some(State::Merged), None);
    ledger.track_issue("anna", &done, 3).unwrap();
    done
}
fn merged(ledger: &mut Ledger, issue: i64, pr: u64) -> TrackChange {
    merged_by(ledger, issue, pr, true)
}
fn readiness_command(dir: &Path) -> GhPr {
    GhPr { dir: dir.into(), number: 7, repo: Some(A.into()), head: None, kind: Kind::Merge, body: String::new() }
}

#[test]
fn the_trial_reason_round_trips_and_nothing_else_passes_for_it() {
    assert_eq!(trial_counts(&trial_reason(1, TRIAL_PRS)), Some((1, TRIAL_PRS)));
    assert_eq!(crate::merge_policy::reason_kind(&trial_reason(2, 3)), "trial");
    for other in ["trial: pull request x of 3 needs the owner to merge", "src/trial: pull request 1 of 3 needs the owner to merge", "trial: pull request 1 of 3 needs the owner to merge now", ""] {
        assert_eq!(trial_counts(other), None, "{other}");
    }
}

#[tokio::test]
async fn a_new_watch_holds_its_first_merges_for_the_owner_with_the_trial_reason() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path());
    assert_eq!(ledger.trial_status("anna", A).unwrap(), Some((0, TRIAL_PRS)));
    let src = Src::new(vec![file("src/a.rs", 3)]);
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &src, "mg").await, None, "the owner's card, not the gate");
    let (_, decision) = merge_readiness(&mut ledger, &readiness_command(temp.path()), "anna", &src).await.unwrap();
    assert_eq!(decision, Decision::NeedsHuman(trial_reason(1, TRIAL_PRS)), "the room notice says PR 1 of 3");
    // Reviewing and readying still run without a card: only the merge waits.
    let ready = format!("gh pr ready 7 --repo {A}");
    assert_eq!(gate::auto_answer(&mut ledger, OWNER, &bash(&ready, temp.path(), "anna"), "rd", 11, &src).await, Some(true));
}

#[tokio::test]
async fn owner_merges_advance_the_count_and_after_the_trial_low_risk_prs_merge_by_themselves() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path());
    for (i, pr) in [11u64, 12, 13].into_iter().enumerate() {
        let done = merged(&mut ledger, i as i64 + 1, pr);
        assert!(ledger.count_trial_merge(OWNER, "anna", &done).unwrap());
        assert!(!ledger.count_trial_merge(OWNER, "anna", &done).unwrap(), "a replay counts once");
        if i + 1 < TRIAL_PRS as usize {
            assert_eq!(ledger.trial_status("anna", A).unwrap(), Some((i as u32 + 1, TRIAL_PRS)));
        }
    }
    assert_eq!(ledger.trial_status("anna", A).unwrap(), None, "the trial is over");
    let src = Src::new(vec![file("src/a.rs", 3)]);
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &src, "mg").await, Some(true), "auto-merge resumes");
    // Low risk only: a protected path still needs a person.
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path());
    ledger.end_watch_trial(OWNER, "anna", A).unwrap();
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &Src::new(vec![file(".github/workflows/ci.yml", 1)]), "mg").await, None);
}

#[tokio::test]
async fn a_watch_from_before_trials_is_unaffected_even_when_edited() {
    let temp = tempfile::tempdir().unwrap();
    let ledger = anna(temp.path());
    // A ledger from before this change: the watch exists, no trial row.
    ledger.db.execute_batch("DELETE FROM watch_trial").unwrap();
    drop(ledger);
    let mut ledger = Ledger::open(temp.path().join("local.db"), OWNER).unwrap();
    ledger.set_work(OWNER, "anna", vec![OwnerRules { max_lines: Some(80), ..rules() }]).unwrap();
    assert_eq!(ledger.trial_status("anna", A).unwrap(), None, "an edit is not a new watch");
    let src = Src::new(vec![file("src/a.rs", 3)]);
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &src, "mg").await, Some(true));
}

#[test]
fn a_legacy_watch_starts_a_trial_only_for_a_new_repository() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger_with(temp.path(), &["anna"]);
    let setting = |repo: &str, mode| WatchSetting { mode, label: String::new(), repo: repo.into(), room_id: "!r:x".into(), push_trusted: false };
    ledger.set_issue_watch(OWNER, "anna", setting(A, WatchMode::Off), 1).unwrap();
    assert_eq!(ledger.trial_status("anna", A).unwrap(), Some((0, TRIAL_PRS)), "saved Off first: switching it on is no way round the trial");
    ledger.db.execute_batch("DELETE FROM watch_trial").unwrap();
    for mode in [WatchMode::Off, WatchMode::Trusted, WatchMode::All] {
        ledger.set_issue_watch(OWNER, "anna", setting(A, mode), 2).unwrap();
    }
    assert_eq!(ledger.trial_status("anna", A).unwrap(), None, "pausing, resuming or editing an old watch");
}

#[tokio::test]
async fn agents_and_room_members_cannot_end_or_count_the_trial_and_the_owner_can_end_it() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path());
    assert!(matches!(ledger.end_watch_trial(MEMBER, "anna", A), Err(Error::Unauthorized)));
    assert!(matches!(ledger.end_watch_trial("anna", "anna", A), Err(Error::Unauthorized)));
    let done = merged(&mut ledger, 1, 11);
    assert!(matches!(ledger.count_trial_merge(MEMBER, "anna", &done), Err(Error::Unauthorized)));
    // Another maintainer's merge on GitHub is no owner merge (GPT review).
    let other = merged_by(&mut ledger, 4, 14, false);
    assert!(!ledger.count_trial_merge(OWNER, "anna", &other).unwrap());
    // A merge the gate answered itself is the Agent's own: it never counts.
    ledger.claim_auto_merge("https://github.com/csheargm/hagency-demo-sandbox/pull/12", HEAD, "anna").unwrap();
    let own = merged(&mut ledger, 2, 12);
    assert!(!ledger.count_trial_merge(OWNER, "anna", &own).unwrap());
    // An issue that closed without a merged pull request never counts.
    let handed = TrackChange { repo: A.into(), number: 3, handed: Some(1), state: None, pr: None, notified: None, rehanded: None, owner_merged: None };
    ledger.track_issue("anna", &handed, 1).unwrap();
    let unmerged = TrackChange { state: Some(State::Merged), owner_merged: Some(true), ..handed };
    assert!(!ledger.count_trial_merge(OWNER, "anna", &unmerged).unwrap(), "the stored record is not merged");
    assert_eq!(ledger.trial_status("anna", A).unwrap(), Some((0, TRIAL_PRS)));
    ledger.end_watch_trial(OWNER, "anna", A).unwrap();
    assert_eq!(ledger.trial_status("anna", A).unwrap(), None);
    let src = Src::new(vec![file("src/a.rs", 3)]);
    assert_eq!(review_then_merge(&mut ledger, temp.path(), &src, "mg").await, Some(true), "ended by the owner");
}
