use super::*;
use matrix_sdk::ruma::room_id;

const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
const SERVER: &str = "localhost:8450";

/// The live dev room's events (Nov 19 01:36-01:39, read from palpo_fa):
/// bird-codex's card + final edit, then anna-claude's card + final edit.
fn fixture() -> Vec<Value> {
    serde_json::from_str(include_str!("loaded_run_cards_fixture.json")).unwrap()
}

/// (card, final edit, the request it answers) for each run in the fixture.
fn runs() -> Vec<(Value, Value, String)> {
    let events = fixture();
    events.chunks(2).map(|pair| {
        let request = pair[0]["content"]["im.hagency.agent_run"]["eventId"].as_str().unwrap().to_owned();
        (pair[0].clone(), pair[1].clone(), request)
    }).collect()
}

fn admit(generation: u64, events: Vec<Admit>) {
    for (value, _) in events {
        // bird-codex is on the trusted peer (beta); anna-claude on the room's server.
        let remote = !value["sender"].as_str().unwrap().ends_with(SERVER);
        super::super::pending_questions::test_accept(generation, ROOM, SERVER, &value, remote);
    }
}

/// Regression (Nov 19 02:06): after a restart, replies posted earlier were
/// missing from the dev room. Their cards came from the local cache, never
/// through the sync handler, so no card was drawn on the request while the
/// card's own event stayed hidden. Loaded cards are admitted with their edit.
#[test]
fn replies_loaded_from_the_cache_are_drawn_on_their_request_after_a_restart() {
    let _guard = super::super::pending_questions::test_serial();
    let generation = super::super::pending_questions::test_login("@csheargm:localhost:8450");
    let room = room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450");
    let runs = runs();
    assert_eq!(runs.len(), 2);
    for (card, _, request) in &runs {
        assert!(super::super::run_controls::for_event(room, request, true).is_empty(), "fresh start: nothing admitted");
        assert!(!super::super::run_controls::is_card_event(room, card["event_id"].as_str().unwrap()));
    }
    let loaded: Vec<_> = runs.iter().map(|(card, edit, _)| cards_of(card.clone(), Some(edit.clone()), None).unwrap()).collect();
    admit(generation, unseen(generation, loaded));
    for (card, edit, request) in &runs {
        let shown = super::super::run_controls::for_event(room, request, true);
        assert_eq!(shown.len(), 1, "one card on request {request}");
        let answer = edit["content"]["m.new_content"]["im.hagency.agent_run"]["answer"].as_str().unwrap();
        assert_eq!(shown[0].card.answer, answer, "the final edit's answer is drawn");
        assert!(!shown[0].card.state.active(), "completed, not stuck on Working");
        assert!(super::super::run_controls::is_card_event(room, card["event_id"].as_str().unwrap()));
    }
    assert!(runs[0].1["content"]["m.new_content"]["body"].as_str().unwrap().starts_with("Checked [PR #135]"));
    assert!(runs[1].1["content"]["m.new_content"]["body"].as_str().unwrap().starts_with("The docs index was proposed"));
}

#[test]
fn each_card_is_handed_over_once_per_edit_and_login() {
    let _guard = super::super::pending_questions::test_serial();
    let (card, edit, _) = runs().remove(1);
    let first = || vec![cards_of(card.clone(), None, None).unwrap()];
    assert_eq!(unseen(7001, first()).len(), 1, "the card alone");
    assert!(unseen(7001, first()).is_empty(), "redraws do not hand it over again");
    assert_eq!(unseen(7001, vec![cards_of(card.clone(), Some(edit.clone()), None).unwrap()]).len(), 2, "a new edit: card, then edit");
    assert_eq!(unseen(7002, first()).len(), 1, "a new login starts over");
}

#[test]
fn only_agent_run_status_events_are_cards() {
    let mut message = fixture().remove(0);
    message["content"]["msgtype"] = "m.text".into();
    assert!(cards_of(message, None, None).is_none());
    let (card, edit, _) = runs().remove(0);
    let loaded = cards_of(card, Some(edit), None).unwrap();
    assert_eq!(loaded.events[0].0["content"]["m.relates_to"]["rel_type"], Value::Null, "the card first");
    assert_eq!(loaded.events[1].0["content"]["m.relates_to"]["rel_type"], "m.replace");
}
