//! User flow (Chris, 2026-11-17): Betty adds her Agent to the dev room. Anna,
//! a member, asks it questions without Betty's OK by default; Betty turns
//! "answer questions" off for that room and Anna's next request waits for her.
use super::{RoomShare, SavedRoomShare};
use crate::{Layer, Ledger, RequestPolicy, Scope, ToolPolicy};

const OWNER: &str = "@betty:example.org";
const ROOM: &str = "!dev:example.org";

fn scope(requester: &str) -> Scope {
    Scope { agent: "bird".into(), binding: "b".into(), room: ROOM.into(), requester: requester.into(), thread: "t".into() }
}

fn ledger(dir: &std::path::Path) -> Ledger {
    let mut ledger = Ledger::open(dir.join("local.db"), OWNER).unwrap();
    for who in [OWNER, "@anna:example.org"] {
        let s = scope(who);
        ledger.register_binding(OWNER, &s).unwrap();
        let allow = crate::Policy {
            budget: crate::Budget { limit: crate::Limit::Tokens(1000), period: crate::Period::Lifetime },
            requests: RequestPolicy::Allow,
            high_risk: ToolPolicy::AskOwner,
        };
        for (index, layer) in [Layer::Agent, Layer::Room, Layer::Requester].into_iter().enumerate() {
            let revision = ledger.policy_snapshot(&s).unwrap()[index].revision;
            ledger.set_policy(OWNER, &s, layer, revision, &allow).unwrap();
        }
    }
    ledger
}

#[test]
fn unsaved_rooms_get_the_shared_team_member_defaults() {
    let db = tempfile::tempdir().unwrap();
    let ledger = ledger(db.path());
    assert_eq!(ledger.room_share("bird", ROOM).unwrap(), RoomShare { answer: true, review: true, fix: false });
    assert!(ledger.room_shares("bird").unwrap().is_empty());
}

#[test]
fn the_owner_saves_each_room_and_only_the_owner() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = ledger(db.path());
    let quiet = RoomShare { answer: true, review: false, fix: true };
    ledger.set_room_share(OWNER, "bird", ROOM, quiet).unwrap();
    assert_eq!(ledger.room_share("bird", ROOM).unwrap(), quiet);
    assert_eq!(ledger.room_share("bird", "!other:example.org").unwrap(), RoomShare::default(), "per room");
    assert_eq!(ledger.room_share("other-agent", ROOM).unwrap(), RoomShare::default(), "per Agent");
    assert_eq!(ledger.room_shares("bird").unwrap(), vec![SavedRoomShare::new(ROOM, quiet)]);
    assert!(ledger.set_room_share("@anna:example.org", "bird", ROOM, RoomShare::default()).is_err(), "a room member cannot change it");
}

#[test]
fn answer_off_makes_a_room_members_request_wait_for_the_owner() {
    let db = tempfile::tempdir().unwrap();
    let mut ledger = ledger(db.path());
    let (anna, betty) = (scope("@anna:example.org"), scope(OWNER));
    assert_eq!(ledger.request_disposition(&anna, "d1").unwrap(), RequestPolicy::Allow, "default: members may ask");
    ledger.set_room_share(OWNER, "bird", ROOM, RoomShare { answer: false, ..RoomShare::default() }).unwrap();
    assert_eq!(ledger.request_disposition(&anna, "d2").unwrap(), RequestPolicy::AskOwner);
    assert_eq!(ledger.request_disposition(&betty, "d3").unwrap(), RequestPolicy::Allow, "the owner's own requests never wait");
}

#[test]
fn the_api_shape_is_closed() {
    let saved: SavedRoomShare = serde_json::from_value(serde_json::json!({"room": ROOM, "answer": true, "review": false, "fix": false})).unwrap();
    assert!(!saved.share().review);
    assert!(serde_json::from_value::<SavedRoomShare>(serde_json::json!({"room": ROOM, "answer": true, "review": true})).is_err(), "every choice is explicit");
    assert!(serde_json::from_value::<SavedRoomShare>(serde_json::json!({"room": ROOM, "answer": true, "review": true, "fix": false, "merge": true})).is_err());
}
