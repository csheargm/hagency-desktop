//! Keeps a room's rooms-list preview in step with run card admission.
//!
//! The rooms list computes a room's latest-event preview only when the latest
//! event's timestamp changes (`sliding_sync::update_room`). A run card's
//! preview shows its answer only once the card is admitted, which can happen
//! later (loaded-card admission, deferred membership checks), so without this
//! the list kept "Agent run" for a card whose reply was drawn in the timeline.

use std::collections::HashSet;
use std::sync::{LazyLock, Mutex};

use matrix_sdk::Room;
use matrix_sdk::ruma::{OwnedRoomId, RoomId};

/// Rooms whose preview refresh is queued and has not read the store yet.
static QUEUED: LazyLock<Mutex<HashSet<OwnedRoomId>>> = LazyLock::new(Default::default);

/// Queues `room`'s refresh; false when one is already queued (a batch of
/// admitted cards in one room recomputes its preview once, not per card).
fn queue(room: &RoomId) -> bool {
    QUEUED.lock().unwrap_or_else(|e| e.into_inner()).insert(room.to_owned())
}

/// Unqueues `room` before its preview is read, so a card admitted meanwhile
/// queues a fresh refresh rather than being missed.
fn unqueue(room: &RoomId) {
    QUEUED.lock().unwrap_or_else(|e| e.into_inner()).remove(room);
}

/// Cards were admitted in `room`: recompute only that room's preview.
pub(super) fn after_admission(room: &Room) {
    if !queue(room.room_id()) {
        return;
    }
    let room = room.clone();
    crate::sliding_sync::spawn_async_task(async move {
        unqueue(room.room_id());
        crate::sliding_sync::update_latest_event(&room).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::pending_questions as pq;
    use matrix_sdk::ruma::room_id;
    use serde_json::Value;

    const ROOM: &str = "!a3cQXHR8GZ4RP5n71s:localhost:8450";
    const SERVER: &str = "localhost:8450";

    /// The rooms-list text of a run card's content, as the list computes it.
    fn list_preview(content: &Value, sender: &str) -> String {
        let msgtype = serde_json::from_value::<matrix_sdk::ruma::events::room::message::RoomMessageEventContent>(content.clone())
            .unwrap().msgtype;
        let sender = matrix_sdk::ruma::UserId::parse(sender).unwrap();
        crate::event_preview::text_preview_of_message(&msgtype, &sender, "anna-claude", crate::event_preview::CardScope { room: matrix_sdk::ruma::room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450"), event: None }).format_with("anna-claude", true)
    }

    /// Live, Nov 19 03:23: "anna-claude: Agent run" stayed in the rooms list
    /// for a card whose reply was drawn in the timeline. The latest event is
    /// a run card: "Agent run" before admission; admission queues one refresh
    /// of that room only, and the recomputed preview shows the answer, escaped.
    #[test]
    fn an_admitted_latest_run_card_refreshes_its_rooms_list_preview_to_the_answer() {
        let _guard = pq::test_serial();
        let generation = pq::test_login("@csheargm:localhost:8450");
        let events: Vec<Value> = serde_json::from_str(include_str!("loaded_run_cards_fixture.json")).unwrap();
        let (card, mut edit) = (events[2].clone(), events[3].clone());
        edit["content"]["m.new_content"]["im.hagency.agent_run"]["answer"] = "Done <b>ok</b>".into();
        let sender = card["sender"].as_str().unwrap().to_owned();
        let latest = edit["content"]["m.new_content"].clone();
        let room = room_id!("!a3cQXHR8GZ4RP5n71s:localhost:8450");
        unqueue(room);

        let before = list_preview(&latest, &sender);
        assert!(before.contains(crate::i18n::tr("Agent run")) && !before.contains("Done"), "{before}");

        for value in [&card, &edit] {
            assert!(pq::test_accept(generation, ROOM, SERVER, value, None, true, false));
        }
        assert!(queue(room), "the admission queues this room's refresh");
        assert!(!queue(room), "a second card in the same room does not queue another");
        assert!(queue(room_id!("!other:localhost:8450")), "other rooms are independent");
        unqueue(room);
        unqueue(room_id!("!other:localhost:8450"));

        let after = list_preview(&latest, &sender);
        assert!(after.contains("Done &lt;b&gt;ok&lt;/b&gt;") && !after.contains(crate::i18n::tr("Agent run")), "{after}");
    }
}
