//! Run cards this device received before a restart. The sync event handler
//! (`pending_questions::attach`) only sees what the server sends after
//! start-up, and the sliding sync position survives a restart, so a card and
//! its final edit that come back from the local cache (or back-pagination)
//! never pass through it. The card's own event is still hidden as "drawn by
//! the card", so the Agent's reply vanished after a restart (Nov 19). The
//! room timeline hands its loaded cards here; they go through the same
//! admission boundary as live sync, the card before its edit.
use matrix_sdk::{Room, deserialized_responses::EncryptionInfo};
use matrix_sdk_ui::timeline::{EventTimelineItem, TimelineItem};
use makepad_widgets::Cx;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};

/// One event to admit, with what the SDK knows about its encryption.
type Admit = (Value, Option<EncryptionInfo>);

/// A loaded run card: its key (card event, latest edit event) and events.
struct Loaded {
    key: (String, String),
    events: Vec<Admit>,
}

/// (login generation, card event, latest edit event) already handed over.
static SEEN: LazyLock<Mutex<HashSet<(u64, String, String)>>> = LazyLock::new(Default::default);
/// Forgotten past this size; handing a card over again is harmless (admission is idempotent).
const MAX_SEEN: usize = 2048;

fn json(raw: &matrix_sdk::ruma::serde::Raw<matrix_sdk::ruma::events::AnySyncTimelineEvent>) -> Option<Value> {
    raw.deserialize_as::<Value>().ok()
}

/// The card and its latest edit, if `event` is an Agent run status event.
fn loaded(event: &EventTimelineItem) -> Option<Loaded> {
    let original = json(event.original_json()?)?;
    cards_of(original, event.latest_edit_json().and_then(json), event.encryption_info().cloned())
}

/// The edit carries no encryption info of its own here, so it is only
/// admitted where the room is known to be unencrypted (`accept_event`).
fn cards_of(original: Value, edit: Option<Value>, info: Option<EncryptionInfo>) -> Option<Loaded> {
    if original["content"]["msgtype"] != ::hagency::agent_run_controls::MSGTYPE {
        return None;
    }
    let card = original["event_id"].as_str()?.to_owned();
    let edit_id = edit.as_ref().and_then(|e| e["event_id"].as_str()).unwrap_or_default().to_owned();
    let mut events = vec![(original, info)];
    events.extend(edit.map(|e| (e, None)));
    Some(Loaded { key: (card, edit_id), events })
}

/// The events of the cards not yet handed over in `generation`, marked seen.
fn unseen(generation: u64, cards: Vec<Loaded>) -> Vec<Admit> {
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.len() > MAX_SEEN {
        seen.clear();
    }
    cards.into_iter()
        .filter(|c| seen.insert((generation, c.key.0.clone(), c.key.1.clone())))
        .flat_map(|c| c.events)
        .collect()
}

/// Admits the run cards among `items` (a room timeline's loaded items).
pub fn ingest(room: &Room, items: &imbl::Vector<Arc<TimelineItem>>) {
    let cards = items.iter().filter_map(|i| i.as_event()).filter_map(loaded).collect();
    let generation = super::pending_questions::generation();
    let events = unseen(generation, cards);
    if events.is_empty() {
        return;
    }
    let room = room.clone();
    crate::sliding_sync::spawn_async_task(async move {
        if super::pending_questions::admit_loaded(generation, &room, events).await {
            Cx::post_action(super::pending_questions::PendingQuestionsChanged);
        }
    });
}

#[cfg(test)]
#[path = "loaded_run_cards_tests.rs"]
mod tests;
