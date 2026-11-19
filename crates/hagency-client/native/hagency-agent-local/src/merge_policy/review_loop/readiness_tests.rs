//! Readiness counts approvals as the judge does (round-2 GPT review).
use super::*;
use super::tests::{anna, rules, Src, A};
use crate::merge_policy::gate;
use crate::merge_policy::tests::{file, HEAD};
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
