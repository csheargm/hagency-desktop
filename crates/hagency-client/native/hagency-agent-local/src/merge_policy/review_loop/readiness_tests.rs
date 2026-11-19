//! Readiness counts approvals as the judge does (round-2 GPT review).
use super::*;
use super::tests::{anna, rules, Src, A};
use crate::merge_policy::gate;
use crate::merge_policy::github::HeadRef;
use crate::merge_policy::tests::{bash, file, HEAD};
use crate::merge_policy::work::Role;

/// The pull request's author Agent's own verdict is in the ledger, but the
/// judge never counts it: readiness reports 0 approvals (so the loop asks a
/// reviewer again), never the raw 1 that suppressed the re-request.
#[tokio::test]
async fn readiness_never_counts_the_authors_own_verdict() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    ledger.db.execute("INSERT INTO issue_track (agent,repo,number,state,attempts,handed_at,pr,notified) VALUES ('bird',?1,3,'pr_open',1,0,7,0)", [A]).unwrap();
    let url = format!("https://github.com/{A}/pull/7");
    ledger.hold_agent_review("rv", &url, HEAD, ("bird", "!r:x")).unwrap();
    ledger.settle_agent_review("rv", true).unwrap();
    assert_eq!(ledger.agent_reviewers_in(&url, HEAD, Some("!r:x")).unwrap(), ["bird"], "the author's verdict is recorded");
    let src = Src::new(vec![file("src/a.rs", 3)]);
    let command = GhPr { dir: temp.path().into(), number: 7, repo: Some(A.into()), head: None, kind: gate::Kind::Merge, body: String::new() };
    let (_, decision, counted) = merge_readiness_counted(&mut ledger, &command, ("anna", "!r:x"), &src).await.unwrap();
    assert_eq!(counted, 0, "the author's own verdict never counts");
    assert!(matches!(decision, Decision::NeedsHuman(_)), "{decision:?}");
}

/// Round-2 GPT review: a hold decided before approvals are judged (here, no
/// room rules) keeps the recorded count, so the author's verdict never turns
/// it into a silent "no review yet".
#[tokio::test]
async fn an_early_hold_keeps_the_recorded_count() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    crate::merge_policy::tests::set_room_rules(&mut ledger, A, None);
    let url = format!("https://github.com/{A}/pull/7");
    ledger.db.execute("INSERT INTO issue_track (agent,repo,number,state,attempts,handed_at,pr,notified) VALUES ('bird',?1,3,'pr_open',1,0,7,0)", [A]).unwrap();
    ledger.hold_agent_review("rv", &url, HEAD, ("bird", "!r:x")).unwrap();
    ledger.settle_agent_review("rv", true).unwrap();
    let command = GhPr { dir: temp.path().into(), number: 7, repo: Some(A.into()), head: None, kind: gate::Kind::Merge, body: String::new() };
    let (_, decision, counted) = merge_readiness_counted(&mut ledger, &command, ("anna", "!r:x"), &Src::new(vec![file("src/a.rs", 3)])).await.unwrap();
    assert!(matches!(decision, Decision::NeedsHuman(ref r) if r == crate::merge_policy::room_rules::NO_ROOM_RULES), "{decision:?}");
    assert_eq!(counted, 1);
}

/// The shared login's pull request #7, opened at [`OPENED`] from branch `fix-3`.
const OPENED: i64 = 1_800_000_000;
fn shared_login() -> Src {
    Src { author: "anna", head_ref: Some(HeadRef { branch: "fix-3".into(), created_at: OPENED }), ..Src::new(vec![file("src/a.rs", 3)]) }
}
/// `agent`'s `gh pr create` of `fix-3` ran (succeeded) `before` seconds before GitHub opened #7.
fn created_by(ledger: &mut Ledger, dir: &std::path::Path, agent: &str, before: i64) {
    let create = bash(&format!("gh pr create --repo {A} --head fix-3 -t 'Fix #3' -b 'Closes #3'"), dir, agent);
    ledger.hold_create(&format!("pc-{agent}"), &create, OPENED - before).unwrap();
    ledger.settle_tool_call(&format!("pc-{agent}"), true).unwrap();
}
fn tracked_by(ledger: &Ledger, agent: &str) {
    ledger.db.execute("INSERT INTO issue_track (agent,repo,number,state,attempts,handed_at,pr,notified) VALUES (?1,?2,3,'pr_open',1,0,7,0)", (agent, A)).unwrap();
}
fn merge_7(dir: &std::path::Path) -> GhPr {
    GhPr { dir: dir.into(), number: 7, repo: Some(A.into()), head: None, kind: gate::Kind::Merge, body: String::new() }
}

/// Live PR #109 (Nov 18): every Agent of the owner posts as one GitHub login,
/// so the login wrote the pull request; this ledger recorded anna's own
/// `gh pr create` of its branch. yue's recorded approval counts, so the loop
/// never asks it again.
#[tokio::test]
async fn a_shared_logins_pull_request_counts_the_other_agents_recorded_review() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let url = format!("https://github.com/{A}/pull/7");
    ledger.record_agent_review(&url, HEAD, "yue", "!r:x").unwrap();
    let (shared, command) = (shared_login(), merge_7(temp.path()));
    let (view, _, counted) = merge_readiness_counted(&mut ledger, &command, ("anna", "!r:x"), &shared).await.unwrap();
    assert_eq!(counted, 0, "an unknown author among the login's Agents: no local approval counts");
    assert_eq!(ledger.recorded_room_reviews(&view, 7, ("anna", "!r:x")).unwrap(), 1, "yet the review is recorded");
    created_by(&mut ledger, temp.path(), "anna", 30);
    let (view, decision, counted) = merge_readiness_counted(&mut ledger, &command, ("anna", "!r:x"), &shared).await.unwrap();
    assert_eq!(counted, 1, "the ledger names the author: the other Agent's approval counts");
    assert!(!matches!(decision, Decision::NeedsHuman(ref r) if crate::merge_policy::reason_kind(r) == "approvals"), "{decision:?}");
    assert_eq!(ledger.recorded_room_reviews(&view, 7, ("anna", "!r:other")).unwrap(), 0, "only the merge's room counts");
}

/// GPT batch review P1-1: anna tracks issue #3, yue (same login) opens the
/// pull request that closes it and approves it itself; the tracker links it
/// to anna. Neither the tracker's guess nor yue's own evidence lets yue's
/// approval count for anna's merge.
#[tokio::test]
async fn the_trackers_guess_never_makes_the_real_authors_approval_count() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    ledger.record_agent_review(&format!("https://github.com/{A}/pull/7"), HEAD, "yue", "!r:x").unwrap();
    tracked_by(&ledger, "anna");
    let (shared, command) = (shared_login(), merge_7(temp.path()));
    let (view, decision, counted) = merge_readiness_counted(&mut ledger, &command, ("anna", "!r:x"), &shared).await.unwrap();
    assert_eq!(counted, 0, "the tracker alone never names the author");
    assert!(matches!(decision, Decision::NeedsHuman(_)), "{decision:?}");
    created_by(&mut ledger, temp.path(), "yue", 30);
    let (_, decision, counted) = merge_readiness_counted(&mut ledger, &command, ("anna", "!r:x"), &shared).await.unwrap();
    assert_eq!(counted, 0, "yue created it: its own approval never counts");
    assert!(matches!(decision, Decision::NeedsHuman(_)), "{decision:?}");
    // GPT batch review P2-4: the author's own verdict is no recorded review, so a re-review may be asked.
    assert_eq!(ledger.recorded_room_reviews(&view, 7, ("anna", "!r:x")).unwrap(), 0);
}

/// A create recorded long before GitHub opened this pull request (an old
/// one of a reused branch name) does not name this one's author.
#[tokio::test]
async fn an_old_create_of_the_same_branch_is_no_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    ledger.record_agent_review(&format!("https://github.com/{A}/pull/7"), HEAD, "yue", "!r:x").unwrap();
    created_by(&mut ledger, temp.path(), "anna", 86_400);
    let (_, _, counted) = merge_readiness_counted(&mut ledger, &merge_7(temp.path()), ("anna", "!r:x"), &shared_login()).await.unwrap();
    assert_eq!(counted, 0);
}
