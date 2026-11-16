//! The Skills list and cards against a fake room: what each click writes, who
//! may write, and which senders get a card.
use super::model::{Change, Proposal, WriteError, accept_plan, is_applied, plan_write, trusted_proposal};
use crate::home::room_memory::model::Delivery;
use hagency::room_memory::{PowerLevels, Storage};
use hagency::room_skills::{self as skills, SKILL_STATE_TYPE, Skill, proposal};
use serde_json::{Value, json};

const ROOM: &str = "!room:example.test";
const AGENT: &str = "@_hagency_agt_01abc:example.test";

fn levels() -> PowerLevels {
    PowerLevels { users: [("@amy:example.test".to_string(), 100)].into(), ..Default::default() }
}
fn save(name: &str, body: &str) -> Change {
    Change::Save { name: name.into(), description: "Cut a release".into(), body: body.into() }
}
/// A fake room: applies each write as the homeserver would store it.
fn apply(events: &mut Vec<Value>, storage: Storage, content: Value, name: &str) -> Vec<Skill> {
    match storage {
        Storage::State => {
            events.push(json!({"state_key":name,"content":content}));
            skills::reduce_state(events)
        }
        Storage::Timeline => {
            events.push(json!({"type":SKILL_STATE_TYPE,"sender":"@amy:example.test","content":content}));
            skills::reduce_timeline(events, &levels()).0
        }
    }
}

#[test]
fn add_edit_delete_round_trip_in_both_storages() {
    for storage in [Storage::State, Storage::Timeline] {
        let (me, mut events) = ("@amy:example.test", Vec::new());
        let added = plan_write(&save("Release", "1. test"), None, me, &levels(), storage).unwrap();
        assert_eq!(added.delivery == Delivery::Timeline, storage == Storage::Timeline);
        let now = apply(&mut events, storage, added.content, "release");
        assert_eq!((now[0].name.as_str(), now[0].rev), ("release", 1), "names are lowercased keys");
        let edited = plan_write(&save("release", "1. test --locked"), Some(&now[0]), me, &levels(), storage).unwrap();
        let now = apply(&mut events, storage, edited.content, "release");
        assert_eq!((now[0].body.as_str(), now[0].rev), ("1. test --locked", 2));
        let deleted = plan_write(&Change::Delete, Some(&now[0]), me, &levels(), storage).unwrap();
        assert!(apply(&mut events, storage, deleted.content, "release").is_empty());
    }
}

#[test]
fn unprivileged_member_cannot_write_and_bad_input_is_refused() {
    let err = plan_write(&save("release", "x"), None, "@bob:example.test", &levels(), Storage::State).unwrap_err();
    assert_eq!(err, WriteError::PowerLevel);
    let me = "@amy:example.test";
    assert_eq!(plan_write(&save("no spaces", "x"), None, me, &levels(), Storage::State).unwrap_err(), WriteError::Name);
    assert_eq!(plan_write(&save("ok", " "), None, me, &levels(), Storage::State).unwrap_err(), WriteError::Body);
    assert_eq!(plan_write(&Change::Delete, None, me, &levels(), Storage::State).unwrap_err(), WriteError::Missing);
}

fn card() -> Value {
    let skill = Skill { name: "release".into(), description: "Cut a release".into(), body: "1. test".into(), author: "@forged:x".into(), rev: 0 };
    proposal::proposal_content("sk_1", &skill, "did it twice", "$req", "@forged:x", false)
}

#[test]
fn only_a_real_agent_gets_a_card_and_accept_bumps_the_revision() {
    assert!(trusted_proposal(&card(), "@member:example.test", ROOM).is_none(), "non-agent sender gets no card");
    assert!(trusted_proposal(&card(), "@_hagency_agt_x:elsewhere.test", ROOM).is_none());
    let p: Proposal = trusted_proposal(&card(), AGENT, ROOM).unwrap();
    assert_eq!(p.skill.author, AGENT, "the payload's author is ignored");
    assert!(accept_plan(&p, None, "@bob:example.test", &levels(), Storage::State).is_err());
    let mut events = Vec::new();
    let first = accept_plan(&p, None, "@amy:example.test", &levels(), Storage::Timeline).unwrap();
    let now = apply(&mut events, Storage::Timeline, first.content, "release");
    assert!(is_applied(&p, &now));
    let mut edit = p.clone();
    edit.skill.body = "1. test --locked".into();
    assert!(!is_applied(&edit, &now), "an edit card stays open until accepted");
    let second = accept_plan(&edit, now.first(), "@amy:example.test", &levels(), Storage::Timeline).unwrap();
    assert_eq!(second.content["rev"], 2);
    assert!(is_applied(&edit, &apply(&mut events, Storage::Timeline, second.content, "release")));
}

#[test]
fn account_switch_loads_the_rooms_again() {
    use matrix_sdk::ruma::room_id;
    let room = room_id!("!r:example.test");
    let mut store = crate::home::room_skills::ops::Store::default();
    assert!(store.first_request(room, Some("@a:example.test")));
    assert!(!store.first_request(room, Some("@a:example.test")));
    assert!(store.first_request(room, Some("@b:example.test")), "account B must load its own skills");
    assert!(store.was_requested(room, Some("@b:example.test")));
}
