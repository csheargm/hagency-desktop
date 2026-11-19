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

/// The card event a raw latest event (rooms list, thread latest reply) shows.
fn card_event(value: &Value) -> Option<String> {
    let raw = matrix_sdk::ruma::serde::Raw::from_json(serde_json::value::to_raw_value(value).unwrap());
    super::run_card_previews::card_event_of(&raw).map(|id| id.to_string())
}

#[test]
fn a_rooms_list_preview_shows_the_answer_only_in_the_room_the_card_was_admitted_in() {
    let _guard = pq::test_serial();
    let (card, edit) = admitted_card_and_edit();
    let sender = card["sender"].as_str().unwrap();
    let shown = &edit["content"]["m.new_content"];
    let target = card_event(&edit);
    assert_eq!(target.as_deref(), card["event_id"].as_str(), "an edit shows the card it replaces");
    assert!(preview(shown, sender, ROOM, target.as_deref()).contains(ANSWER), "the admitted card's own edit: the answer");
    let replay = preview(shown, sender, OTHER_ROOM, target.as_deref());
    assert!(replay.contains(crate::i18n::tr("Agent run")) && !replay.contains(ANSWER), "{replay}");
}

/// GPT review P2 (round 2): the rooms list and thread latest reply bound to the
/// room only. Same room and sender, another event reusing the run id: placeholder.
#[test]
fn a_latest_event_reusing_an_admitted_run_id_from_another_event_shows_the_placeholder() {
    let _guard = pq::test_serial();
    let (card, _) = admitted_card_and_edit();
    let sender = card["sender"].as_str().unwrap();
    let mut replay = card.clone();
    replay["event_id"] = "$neverAdmittedReplayOfTheRunId".into();
    let target = card_event(&replay);
    assert_eq!(target.as_deref(), Some("$neverAdmittedReplayOfTheRunId"), "an unedited event shows itself");
    let shown = preview(&replay["content"], sender, ROOM, target.as_deref());
    assert!(shown.contains(crate::i18n::tr("Agent run")) && !shown.contains(ANSWER), "{shown}");
    let unknown = preview(&replay["content"], sender, ROOM, None);
    assert!(unknown.contains(crate::i18n::tr("Agent run")) && !unknown.contains(ANSWER), "no event known: {unknown}");
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
