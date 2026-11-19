//! User flow (Chris, 2026-11-17): Betty added bird to the dev room, which
//! reports csheargm/hagency-demo-sandbox. Anna, a member, asks bird to review
//! PR #7: the verdict posts with no card. Anna's request to fix an issue
//! follows Betty's rules (a card) until Betty lets bird work in the room (ADR 0030);
//! a review on a repository outside the room asks; Betty turning "review
//! PRs" off for the room makes the review ask again.
use super::fix_flow_tests::{self, git, run_under};
use super::read_only_flow_tests::{OWNER, REPO, ROOM, ask_owner_ledger, proposal};
use super::{TrustedStep, pinned};
use crate::merge_policy::work::OwnerRules;
use crate::room_autonomy::{AutonomySnapshot, Level};
use crate::room_share::RoomShare;
use crate::{Ledger, ToolPolicy};
use serde_json::json;

const REVIEW: &str = "gh pr review 7 --repo csheargm/hagency-demo-sandbox --comment -b 'Hagency verdict: CHANGES - the test is missing'";

fn dev_room(dir: &std::path::Path) -> Ledger {
    let mut ledger = ask_owner_ledger(dir);
    ledger.set_work(OWNER, "bird", vec![OwnerRules { repo: REPO.into(), room_id: ROOM.into(), ..Default::default() }]).unwrap();
    room_at(&mut ledger, Some(Level::Assist), false);
    ledger
}

fn room_at(ledger: &mut Ledger, level: Option<Level>, paused: bool) {
    let snapshot = AutonomySnapshot { room_id: ROOM.into(), level, paused, unread: false, owner_maintainer: true, as_of: crate::room_autonomy::as_of::next_for_test(), sticky: false };
    ledger.set_room_autonomy(OWNER, &snapshot, crate::merge_policy::room_rules::unix_now()).unwrap();
}

/// Batch GPT review: a card-free verdict follows the room's level like every
/// other loop step: Off, paused or never read, it asks the owner.
#[test]
fn an_off_or_paused_room_posts_no_verdict_without_a_card() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = dev_room(db.path());
    for (level, paused, why) in [(None, false, "no level: Off"), (Some(Level::Off), false, "Off"), (Some(Level::Autonomous), true, "paused")] {
        room_at(&mut ledger, level, paused);
        assert_eq!(ledger.trusted_publish(OWNER, &proposal(ROOM, REVIEW), 11).unwrap(), None, "{why}");
    }
    room_at(&mut ledger, Some(Level::Autonomous), false);
    assert!(ledger.trusted_publish(OWNER, &proposal(ROOM, REVIEW), 11).unwrap().is_some(), "running again");
    let fresh = tempfile::tempdir().unwrap();
    let mut unread = ask_owner_ledger(fresh.path());
    unread.set_work(OWNER, "bird", vec![OwnerRules { repo: REPO.into(), room_id: ROOM.into(), ..Default::default() }]).unwrap();
    assert_eq!(unread.trusted_publish(OWNER, &proposal(ROOM, REVIEW), 11).unwrap(), None, "never read: asks");
}

#[test]
fn a_room_members_review_posts_its_verdict_without_a_card() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = dev_room(db.path());
    let review = proposal(ROOM, REVIEW);
    let Some(TrustedStep::Command(command)) = ledger.trusted_publish(OWNER, &review, 11).unwrap() else { panic!("Anna's review posts") };
    assert!(command.ends_with("GH_HOST='github.com' gh api -X POST 'repos/csheargm/hagency-demo-sandbox/pulls/7/reviews' -f 'event=COMMENT' -f 'body=Hagency verdict: CHANGES - the test is missing'"), "{command}");
    // Reading the PR first is card-free too (ADR 0019).
    for read in ["gh pr view 7 -R csheargm/hagency-demo-sandbox --json title,files", "gh pr diff 7 -R csheargm/hagency-demo-sandbox", "gh pr checks 7 -R csheargm/hagency-demo-sandbox"] {
        assert!(ledger.trusted_publish(OWNER, &proposal(ROOM, read), 11).unwrap().is_some(), "{read}");
    }
}

#[test]
fn a_review_outside_the_rooms_repos_or_in_another_shape_asks() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = dev_room(db.path());
    for line in [
        "gh pr review 7 --repo csheargm/other --comment -b 'Hagency verdict: APPROVE'",
        "gh pr review 7 --repo csheargm/hagency-demo-sandbox --comment -b 'LGTM'",
        "gh pr review 7 --repo csheargm/hagency-demo-sandbox --comment -b 'Hagency verdict: x' && gh pr merge 7",
        "gh pr merge 7 --repo csheargm/hagency-demo-sandbox --squash",
        "gh pr comment 7 --repo csheargm/hagency-demo-sandbox -b 'Hagency verdict: x'",
    ] {
        assert_eq!(ledger.trusted_publish(OWNER, &proposal(ROOM, line), 11).unwrap(), None, "{line}");
        assert_eq!(ledger.tool_disposition(&proposal(ROOM, line), 11).unwrap(), ToolPolicy::AskOwner, "{line}: a card to Betty");
    }
    assert_eq!(ledger.trusted_publish(OWNER, &proposal("!other:example.org", REVIEW), 11).unwrap(), None, "another room's repos are not this room's");
}

#[test]
fn the_owner_turning_reviews_off_makes_them_ask() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = dev_room(db.path());
    ledger.set_room_share(OWNER, "bird", ROOM, RoomShare { review: false, ..RoomShare::default() }).unwrap();
    assert_eq!(ledger.trusted_publish(OWNER, &proposal(ROOM, REVIEW), 11).unwrap(), None);
    assert_eq!(ledger.tool_disposition(&proposal(ROOM, REVIEW), 11).unwrap(), ToolPolicy::AskOwner);
    // Reading the room's repositories stays card-free.
    assert!(ledger.trusted_publish(OWNER, &proposal(ROOM, "gh pr view 7 -R csheargm/hagency-demo-sandbox"), 11).unwrap().is_some());
    ledger.set_room_share(OWNER, "bird", ROOM, RoomShare::default()).unwrap();
    assert!(ledger.trusted_publish(OWNER, &proposal(ROOM, REVIEW), 11).unwrap().is_some(), "back on");
}

/// ADR 0030: a member's fix step in the room follows "May work in this room"
/// and the room's level (it replaced "fix issues without asking").
#[test]
fn a_room_members_fix_follows_may_work_and_the_room_level() {
    let r = run_under(ToolPolicy::AskOwner);
    git(&r.root, &["clone", "-q", r.target.to_str().unwrap(), "app"]);
    git(&r.root.join("app"), &["remote", "set-url", "origin", &format!("https://github.com/{}.git", fix_flow_tests::REPO)]);
    let mut status = fix_flow_tests::proposal(&r.root, fix_flow_tests::ROOM, "claude.Bash", json!({"command": "git -C app status"}));
    status.scope.requester = "@anna:example.org".into();
    let owner = fix_flow_tests::OWNER;
    let mut ledger = r.ledger;
    ledger.set_may_work(owner, "bird", fix_flow_tests::ROOM, false).unwrap();
    assert_eq!(pinned(&ledger.db, &status, owner).unwrap(), None, "may not work here: Anna's fix step asks Betty");
    ledger.set_may_work(owner, "bird", fix_flow_tests::ROOM, true).unwrap();
    assert!(matches!(pinned(&ledger.db, &status, owner).unwrap(), Some(TrustedStep::Command(_))), "Betty let bird work in this room");
    let mut elsewhere = status.clone();
    elsewhere.scope.room = "!other:example.org".into();
    assert_eq!(pinned(&ledger.db, &elsewhere, owner).unwrap(), None, "only in that room");
    let off = crate::room_autonomy::AutonomySnapshot { room_id: fix_flow_tests::ROOM.into(), ..Default::default() };
    ledger.set_room_autonomy(owner, &off, crate::merge_policy::room_rules::unix_now()).unwrap();
    assert_eq!(pinned(&ledger.db, &status, owner).unwrap(), None, "the room is Off: asks");
}

/// GPT review: the host's check right before a push rereads the room, so
/// Pause (or "may work" off) stops a push already authorized, for a member
/// and for the owner.
#[test]
fn pausing_the_room_stops_a_push_at_the_last_check() {
    let r = run_under(ToolPolicy::AskOwner);
    let owner = fix_flow_tests::OWNER;
    let mut ledger = r.ledger;
    let mut push = fix_flow_tests::proposal(&r.root, fix_flow_tests::ROOM, "claude.Bash", json!({"command": "git -C app push origin fix/x"}));
    push.scope.requester = "@anna:example.org".into();
    // The recheck follows an authorization: the member's run must be admitted (its own dispatch).
    push.dispatch = "d-member".into();
    let push = super::tests::admitted_by(&mut ledger, owner, push);
    let still = ledger.push_trust_recheck(&push, fix_flow_tests::REPO);
    assert!(still(), "running: the push goes ahead");
    let paused = crate::room_autonomy::AutonomySnapshot { room_id: fix_flow_tests::ROOM.into(), level: Some(crate::room_autonomy::Level::Autonomous), paused: true, unread: false, owner_maintainer: true, as_of: crate::room_autonomy::as_of::next_for_test(), sticky: false };
    ledger.set_room_autonomy(owner, &paused, crate::merge_policy::room_rules::unix_now()).unwrap();
    assert!(!still(), "paused: refused right before it publishes");
    let mut own = push.clone();
    own.scope.requester = owner.into();
    own.dispatch = "d-own".into();
    let own = super::tests::admitted_by(&mut ledger, owner, own);
    assert!(!ledger.push_trust_recheck(&own, fix_flow_tests::REPO)(), "the owner's own push stops too");
    ledger.allow_loop_in(owner, "bird", fix_flow_tests::ROOM);
    ledger.set_may_work(owner, "bird", fix_flow_tests::ROOM, false).unwrap();
    assert!(!still(), "may work off: refused");
}

#[test]
fn a_room_share_names_a_matrix_room() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = dev_room(db.path());
    for bad in ["dev", "!nodomain", "#alias:example.org"] {
        assert!(ledger.set_room_share(OWNER, "bird", bad, RoomShare::default()).is_err(), "{bad}");
    }
}
