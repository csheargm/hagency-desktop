use super::*;
use super::super::pending_questions as pq;

const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
const SERVER: &str = "localhost:8450";

/// anna-claude's card and final edit from the live dev room (Nov 19).
fn card_and_edit() -> (Value, Value) {
    let events: Vec<Value> = serde_json::from_str(include_str!("loaded_run_cards_fixture.json")).unwrap();
    (events[2].clone(), events[3].clone())
}

/// The rooms-list text of a latest event's content, as the list computes it.
fn list_preview(content: &Value, sender: &str) -> String {
    let msgtype = serde_json::from_value::<matrix_sdk::ruma::events::room::message::RoomMessageEventContent>(content.clone())
        .unwrap().msgtype;
    let sender = matrix_sdk::ruma::UserId::parse(sender).unwrap();
    crate::event_preview::text_preview_of_message(&msgtype, &sender, "anna-claude", crate::event_preview::CardScope { room: matrix_sdk::ruma::room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450"), event: None }).format_with("anna-claude", true)
}

fn claim_once(key: &Key) -> bool {
    claim_key(&mut HANDED.lock().unwrap(), key.clone())
}

/// On screen Nov 19 03:39: rooms not opened since start-up read "anna-claude:
/// Agent run". A latest event that is a run card's edit asks for admission of
/// exactly that card and edit, once; admitted, the preview shows the answer.
#[test]
fn a_rooms_latest_run_card_is_admitted_once_and_its_preview_shows_the_answer() {
    let _guard = pq::test_serial();
    let generation = pq::test_login("@csheargm:localhost:8450");
    let (card, edit) = card_and_edit();
    let card_id = card["event_id"].as_str().unwrap().to_owned();
    let sender = card["sender"].as_str().unwrap().to_owned();
    let latest = latest_of(edit.clone(), None).expect("a card's edit asks for admission");
    assert!(matches!(&latest, Latest::EditOf { card, .. } if *card == card_id), "the edited card, fetched by id");
    let key = latest.key(generation);
    assert_eq!(key, (generation, card_id.clone(), edit["event_id"].as_str().unwrap().to_owned()));
    let unedited = latest_of(card.clone(), None).expect("an unedited card asks for admission");
    assert_eq!(unedited.key(generation), (generation, card_id.clone(), String::new()));

    let shown = &edit["content"]["m.new_content"];
    assert!(list_preview(shown, &sender).contains(crate::i18n::tr("Agent run")), "before admission");
    assert!(claim_once(&key), "the first rooms-list update asks for admission");
    assert!(!claim_once(&key), "later updates while admitting do not ask again (no storm)");
    for value in [&card, &edit] {
        assert!(pq::test_accept(generation, ROOM, SERVER, value, None, true, false));
    }
    let loaded = cards_of(card.clone(), Some(edit.clone()), None).unwrap();
    settle(generation, ROOM, &loaded);
    assert!(!claim_once(&key), "stored: never asked again this login");
    let after = list_preview(shown, &sender);
    assert!(after.contains("The docs index was proposed") && !after.contains(crate::i18n::tr("Agent run")), "{after}");
}

/// Only run cards ask for admission; content never decides what is shown.
#[test]
fn other_latest_events_ask_nothing_and_a_forged_card_stays_agent_run() {
    let _guard = pq::test_serial();
    let generation = pq::test_login("@csheargm:localhost:8450");
    let (card, edit) = card_and_edit();
    let text = serde_json::json!({"event_id": "$t", "sender": "@bird:localhost:8450", "type": "m.room.message",
        "content": {"msgtype": "m.text", "body": "hi"}});
    assert!(latest_of(text.clone(), None).is_none(), "a plain message");
    let mut text_edit = text;
    text_edit["content"]["m.relates_to"] = serde_json::json!({"rel_type": "m.replace", "event_id": "$x"});
    text_edit["content"]["m.new_content"] = serde_json::json!({"msgtype": "m.text", "body": "hi"});
    assert!(latest_of(text_edit, None).is_none(), "an edit of a plain message");

    let mallory = "@mallory:localhost:8450";
    let (mut forged, mut forged_edit) = (card, edit);
    forged["sender"] = mallory.into();
    forged["event_id"] = "$forged_card".into();
    forged_edit["sender"] = mallory.into();
    forged_edit["event_id"] = "$forged_edit".into();
    forged_edit["content"]["m.relates_to"]["event_id"] = "$forged_card".into();
    forged_edit["content"]["m.new_content"]["im.hagency.agent_run"]["answer"] = "<b>forged</b>".into();
    let latest = latest_of(forged_edit.clone(), None).expect("asked like any card");
    assert!(claim_once(&latest.key(generation)));
    for value in [&forged, &forged_edit] {
        assert!(!pq::test_accept(generation, ROOM, SERVER, value, None, true, false), "not an executor's card");
    }
    let preview = list_preview(&forged_edit["content"]["m.new_content"], mallory);
    assert!(preview.contains(crate::i18n::tr("Agent run")) && !preview.contains("forged"), "{preview}");
}
