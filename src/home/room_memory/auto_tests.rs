//! Automatic Accept of the owner's own Agents' memory proposals: on and own
//! Agent accepts through the normal Accept; off, someone else's Agent, or a
//! skill proposal leaves the card for a person.
use super::auto::{self, AutoRooms, should_accept};
use super::model::{CardState, Proposal, trusted_proposal};
use super::store::{self, TEST_ME};
use super::tests::{build_card, loaded, room};
use hagency::room_memory;
use hagency::room_skills::{Skill, proposal as skill_proposal};
use matrix_sdk::ruma::OwnedUserId;
use serde_json::json;

/// The tests' admin (power 100) owns `MY_AGENT`; `OTHER_AGENT` belongs to someone else.
const ME: &str = "@admin:s";
const MY_AGENT: &str = "@_hagency_agt_mine:s";
const OTHER_AGENT: &str = "@_hagency_agt_theirs:s";

fn agent_proposal(room_id: &str, sender: &str, id: &str) -> Proposal {
    let content = room_memory::proposal_content(id, "We deploy on Fridays", "Said twice", "$e", "Helper");
    trusted_proposal(&content, sender, room_id).expect("an Agent's card parses")
}
fn sign_in() {
    TEST_ME.with(|t| *t.borrow_mut() = Some(ME.into()));
    let owner = OwnedUserId::try_from(ME).unwrap();
    crate::home::agent_ask_badge::set_test_puppets(&owner, &json!([{"id": "agt_mine", "puppetMxid": MY_AGENT, "ownerMxid": ME}]));
}
/// Syncs a card for `sender`'s proposal; returns whether Accept ran on its own.
fn sync_card(room_id: &str, sender: &str, id: &str, on: bool) -> bool {
    sign_in();
    let room = room(room_id);
    auto::set_for_test(&room, ME, on);
    store::lock().set_loaded(loaded(&room, ME, vec![]));
    let (mut cx, mut card) = build_card();
    card.context = Some((room.clone(), agent_proposal(room_id, sender, id)));
    card.sync(&mut cx);
    let started = store::lock().progress(&room, id).is_some();
    assert_eq!(started, auto::was_auto(&room, id), "every automatic Accept is marked as automatic");
    started
}

#[test]
fn my_agents_proposal_is_accepted_when_the_setting_is_on() {
    assert!(sync_card("!auto-on:s", MY_AGENT, "p-on", true), "the normal Accept ran without a click");
}

#[test]
fn the_card_waits_for_a_person_when_the_setting_is_off() {
    assert!(!sync_card("!auto-off:s", MY_AGENT, "p-off", false));
}

#[test]
fn someone_elses_agent_is_never_accepted_automatically() {
    assert!(!sync_card("!auto-other:s", OTHER_AGENT, "p-other", true));
}

#[test]
fn skill_proposals_are_not_memory_proposals_and_never_auto_accept() {
    let skill = Skill { name: "release".into(), description: "Cut a release".into(), body: "1. test".into(), author: MY_AGENT.into(), rev: 0 };
    let content = skill_proposal::proposal_content("sk_1", &skill, "did it twice", "$req", MY_AGENT, false);
    assert!(trusted_proposal(&content, MY_AGENT, "!auto-skill:s").is_none(), "no memory card, so nothing to auto-accept");
}

#[test]
fn the_rule_needs_every_condition() {
    assert!(should_accept(true, Some(true), &CardState::Pending, true, false));
    assert!(!should_accept(false, Some(true), &CardState::Pending, true, false), "off");
    assert!(!should_accept(true, None, &CardState::Pending, true, false), "ownership unknown");
    assert!(!should_accept(true, Some(true), &CardState::Pending, false, false), "no power level");
    assert!(!should_accept(true, Some(true), &CardState::Pending, true, true), "already tried once");
    for state in [CardState::Added, CardState::Dismissed, CardState::Adding, CardState::Failed("x".into())] {
        assert!(!should_accept(true, Some(true), &state, true, false), "{state:?}: a dismissed or deleted card stays a person's call");
    }
}

#[test]
fn the_setting_is_off_by_default_and_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("auto-rooms-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("auto.json");
    let room = room("!auto-disk:s");
    assert!(!AutoRooms::load(&path).contains(&room), "off when nothing was saved");
    AutoRooms::load(&path).set(&path, &room, true).unwrap();
    assert!(AutoRooms::load(&path).contains(&room));
    AutoRooms::load(&path).set(&path, &room, false).unwrap();
    assert!(!AutoRooms::load(&path).contains(&room));
    std::fs::remove_dir_all(dir).unwrap();
}
