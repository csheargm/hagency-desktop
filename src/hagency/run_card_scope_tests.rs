//! GPT review P2: `admitted_card` matched on sender + run id only, so a card
//! the same agent never had admitted, reusing an admitted run id (even in
//! another room), previewed the stored answer there. A preview binds to the
//! room (rooms list) and, for a reply quote, to the admitted card's event id.

use matrix_sdk::ruma::{EventId, RoomId, UserId, events::room::message::RoomMessageEventContent};
use serde_json::Value;

use super::pending_questions as pq;
use crate::event_preview::{CardScope, text_preview_of_message};

const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
const OTHER_ROOM: &str = "!otherRoomZZZZZZZZZZZ:localhost:8450";
const SERVER: &str = "localhost:8450";

/// anna-claude's card and final edit from the live dev room, admitted.
fn admitted_card_and_edit() -> (Value, Value) {
    let events: Vec<Value> = serde_json::from_str(include_str!("loaded_run_cards_fixture.json")).unwrap();
    let (card, edit) = (events[2].clone(), events[3].clone());
    let generation = pq::test_login("@csheargm:localhost:8450");
    for value in [&card, &edit] {
        assert!(pq::test_accept(generation, ROOM, SERVER, value, None, true, false));
    }
    (card, edit)
}

/// The preview text of `content` sent by `sender`, drawn in `room` (and as event `event`).
fn preview(content: &Value, sender: &str, room: &str, event: Option<&str>) -> String {
    let msgtype = serde_json::from_value::<RoomMessageEventContent>(content.clone()).unwrap().msgtype;
    let sender = UserId::parse(sender).unwrap();
    let room = RoomId::parse(room).unwrap();
    let event = event.map(|e| EventId::parse(e).unwrap());
    let scope = CardScope { room: &room, event: event.as_deref() };
    text_preview_of_message(&msgtype, &sender, "anna-claude", scope).format_with("anna-claude", true)
}

const ANSWER: &str = "The docs index was proposed";

#[test]
fn a_rooms_list_preview_shows_the_answer_only_in_the_room_the_card_was_admitted_in() {
    let _guard = pq::test_serial();
    let (card, edit) = admitted_card_and_edit();
    let sender = card["sender"].as_str().unwrap();
    let shown = &edit["content"]["m.new_content"];
    assert!(preview(shown, sender, ROOM, None).contains(ANSWER), "the admitted room shows the answer");
    let replay = preview(shown, sender, OTHER_ROOM, None);
    assert!(replay.contains(crate::i18n::tr("Agent run")) && !replay.contains(ANSWER), "{replay}");
}

#[test]
fn a_reply_quote_shows_the_answer_only_for_the_admitted_card_event() {
    let _guard = pq::test_serial();
    let (card, edit) = admitted_card_and_edit();
    let (sender, card_event) = (card["sender"].as_str().unwrap(), card["event_id"].as_str().unwrap());
    let shown = &edit["content"]["m.new_content"];
    assert!(preview(shown, sender, ROOM, Some(card_event)).contains(ANSWER), "a reply to the admitted card");
    // Same sender, same room, same run id, but an event that was never admitted.
    let replay = preview(&card["content"], sender, ROOM, Some("$neverAdmittedReplayOfTheRunId"));
    assert!(replay.contains(crate::i18n::tr("Agent run")) && !replay.contains(ANSWER), "{replay}");
    let elsewhere = preview(shown, sender, OTHER_ROOM, Some(card_event));
    assert!(elsewhere.contains(crate::i18n::tr("Agent run")) && !elsewhere.contains(ANSWER), "{elsewhere}");
}
