//! Where a merge may happen at all (round-2 GPT review): a fresh snapshot
//! saying the owner is a maintainer, or the Agent's own private chat.
use super::*;
use crate::room_autonomy::{AutonomySnapshot, Level};
use crate::merge_policy::room_rules::unix_now;

const OWNER: &str = "@owner:example.org";

fn ledger(dir: &std::path::Path) -> Ledger {
    Ledger::open(dir.join("local.db"), OWNER).unwrap()
}
fn scope(binding: &str, room: &str) -> Scope {
    Scope { agent: "anna".into(), binding: binding.into(), room: room.into(), requester: OWNER.into(), thread: room.into() }
}
fn snapshot(room: &str, maintainer: bool) -> AutonomySnapshot {
    AutonomySnapshot { room_id: room.into(), level: Some(Level::Autonomous), paused: false, unread: false, owner_maintainer: maintainer, as_of: None }
}

/// A room this device knows nothing about (its state not yet synced, no
/// watch, rules or work entry) is never treated as a private chat: refused.
#[test]
fn an_unknown_room_refuses_merges() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger(temp.path());
    let unknown = scope("b-unknown", "!unknown:x");
    ledger.register_binding(OWNER, &unknown).unwrap();
    assert_eq!(ledger.merge_refusal(&unknown, unix_now()).unwrap(), Some(NOT_READ_MERGE_REASON));
}

/// The Agent's private chat with its owner, by its binding, leaves the merge to the card.
#[test]
fn the_owner_direct_chat_leaves_merges_to_the_card() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger(temp.path());
    let direct = scope("b-direct", "!dm:x");
    ledger.register_binding(OWNER, &direct).unwrap();
    ledger.set_owner_direct(OWNER, &direct, true).unwrap();
    assert_eq!(ledger.merge_refusal(&direct, unix_now()).unwrap(), None);
    // Another binding of the same room is not the private chat's binding.
    let other = Scope { agent: "bob".into(), ..scope("b-other", "!dm:x") };
    ledger.register_binding(OWNER, &other).unwrap();
    assert_eq!(ledger.merge_refusal(&other, unix_now()).unwrap(), Some(NOT_READ_MERGE_REASON));
    ledger.set_owner_direct(OWNER, &direct, false).unwrap();
    assert_eq!(ledger.merge_refusal(&direct, unix_now()).unwrap(), Some(NOT_READ_MERGE_REASON), "no longer the private chat");
    assert!(ledger.set_owner_direct(OWNER, &scope("b-never", "!dm:x"), true).is_err(), "an unregistered binding");
}

/// A fresh snapshot decides by the owner's role; a stale one counts for nothing.
#[test]
fn a_fresh_snapshot_decides_by_the_owners_role() {
    let temp = tempfile::tempdir().unwrap();
    let mut ledger = ledger(temp.path());
    let room = scope("b-room", "!r:x");
    ledger.register_binding(OWNER, &room).unwrap();
    ledger.set_room_autonomy(OWNER, &snapshot("!r:x", true), unix_now()).unwrap();
    assert_eq!(ledger.merge_refusal(&room, unix_now()).unwrap(), None);
    ledger.set_room_autonomy(OWNER, &snapshot("!r:x", false), unix_now()).unwrap();
    assert_eq!(ledger.merge_refusal(&room, unix_now()).unwrap(), Some(NOT_MAINTAINER_REASON));
    ledger.set_room_autonomy(OWNER, &snapshot("!r:x", true), unix_now() - ROOM_RULES_TTL_SECS - 1).unwrap();
    assert_eq!(ledger.merge_refusal(&room, unix_now()).unwrap(), Some(NOT_READ_MERGE_REASON), "stale");
}
