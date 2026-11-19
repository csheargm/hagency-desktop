//! Live PR #121 (Nov 18): bird-codex, another owner's Agent, approved
//! through bird's own gate, which only bird's device records. Readiness
//! says what one more approval would leave, so the desktop can tell "a
//! person merges it" (Assist) from a hold no approval could lift. The
//! decision itself never changes.
use super::*;
use super::tests::{anna, rules, Src, A};
use crate::merge_policy::gate;
use crate::merge_policy::tests::{file, set_room_rules};
use crate::merge_policy::work::Role;
use crate::room_autonomy::{AutonomySnapshot, Level, ASSIST_REASON};

const ROOM: &str = "!r:x";

fn pr_7(dir: &std::path::Path) -> GhPr {
    GhPr { dir: dir.into(), number: 7, repo: Some(A.into()), head: None, kind: gate::Kind::Merge, body: String::new() }
}

fn approvals_missing(decision: &Decision) -> bool {
    matches!(decision, Decision::NeedsHuman(reason) if super::super::reason_kind(reason) == "approvals")
}

#[tokio::test]
async fn one_more_approval_at_assist_leaves_only_the_person_and_never_merges() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let snapshot = AutonomySnapshot { room_id: ROOM.into(), level: Some(Level::Assist), paused: false, unread: false, owner_maintainer: true, as_of: None };
    ledger.set_room_autonomy("@owner:example.org", &snapshot, super::super::room_rules::unix_now()).unwrap();
    let src = Src::new(vec![file("docs/SUPPORT.md", 3)]);
    let ready = merge_readiness_full(&mut ledger, &pr_7(temp.path()), ("anna", ROOM), &src).await.unwrap();
    assert!(approvals_missing(&ready.decision), "{:?}", ready.decision);
    assert_eq!(ready.counted, 0);
    assert_eq!(ready.one_more, Decision::NeedsHuman(ASSIST_REASON.into()), "only Assist would hold");
}

#[tokio::test]
async fn one_more_tells_a_room_that_needs_two_and_an_autonomous_room_apart() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = anna(temp.path(), vec![rules(A, Role::Maintainer)]);
    let src = Src::new(vec![file("docs/SUPPORT.md", 3)]);
    let ready = merge_readiness_full(&mut ledger, &pr_7(temp.path()), ("anna", ROOM), &src).await.unwrap();
    assert!(approvals_missing(&ready.decision));
    assert_eq!(ready.one_more, Decision::Merge, "Autonomous: one approval would merge, so the desktop still holds and names it");
    set_room_rules(&mut ledger, A, Some("low_risk_paths: [\"**\"]\nmax_changed_lines: 100000\nrequire_ci: false\nrequired_agent_approvals: 2\n"));
    let two = merge_readiness_full(&mut ledger, &pr_7(temp.path()), ("anna", ROOM), &src).await.unwrap();
    assert!(approvals_missing(&two.one_more), "one more of two is still short: {:?}", two.one_more);
}
