//! Run cards this device received before a restart. The sync event handler
//! (`pending_questions::attach`) only sees what the server sends after
//! start-up, and the sliding sync position survives a restart, so a card and
//! its final edit that come back from the local cache (or back-pagination)
//! never pass through it. The card's own event is still hidden as "drawn by
//! the card", so the Agent's reply vanished after a restart (Nov 19). The
//! room timeline hands its loaded cards here; they go through the same
//! admission boundary as live sync, the card before its edit, and as history
//! they never evict a live run (`run_controls::as_history`).
use matrix_sdk::{Room, deserialized_responses::EncryptionInfo, ruma::{OwnedEventId, events::AnySyncTimelineEvent, serde::Raw}};
use matrix_sdk_ui::timeline::{EventTimelineItem, TimelineItem};
use makepad_widgets::Cx;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// One event to admit, with what the SDK knows about its encryption.
type Admit = (Value, Option<EncryptionInfo>);
/// (login generation, card event, latest edit event).
type Key = (u64, String, String);

/// A loaded run card: its own event, and its latest edit as the timeline has it.
struct Loaded {
    card: String,
    original: Admit,
    edit: Option<Value>,
}
impl Loaded {
    fn edit_id(&self) -> &str {
        self.edit.as_ref().and_then(|e| e["event_id"].as_str()).unwrap_or_default()
    }
    fn key(&self, generation: u64) -> Key {
        (generation, self.card.clone(), self.edit_id().to_owned())
    }
    /// The request the run answers (the card is drawn on it).
    fn request(&self) -> &str {
        self.original.0["content"]["im.hagency.agent_run"]["eventId"].as_str().unwrap_or_default()
    }
    /// The revision the card has once its latest edit is applied.
    fn revision(&self) -> u64 {
        let card = |v: &Value| v["im.hagency.agent_run"]["revision"].as_u64();
        self.edit.as_ref().and_then(|e| card(&e["content"]["m.new_content"]))
            .or_else(|| card(&self.original.0["content"])).unwrap_or_default()
    }
}

/// Where a loaded card stands.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Handed {
    /// Being admitted, after this many failed tries.
    Admitting(u8),
    /// Stored with its latest edit: not handed over again.
    Stored,
    /// Not stored after this many tries (e.g. an untrusted sender, a full store).
    Failed(u8),
}
static HANDED: LazyLock<Mutex<HashMap<Key, Handed>>> = LazyLock::new(Default::default);
/// Forgotten past this size; handing a card over again is harmless (admission is idempotent).
const MAX_HANDED: usize = 2048;
/// Tries per card and edit before it waits for the next login.
const MAX_TRIES: u8 = 3;

fn json(raw: &Raw<AnySyncTimelineEvent>) -> Option<Value> {
    raw.deserialize_as::<Value>().ok()
}

/// The card and its latest edit, if `event` is an Agent run status event.
fn loaded(event: &EventTimelineItem) -> Option<Loaded> {
    let original = json(event.original_json()?)?;
    cards_of(original, event.latest_edit_json().and_then(json), event.encryption_info().cloned())
}

fn cards_of(original: Value, edit: Option<Value>, info: Option<EncryptionInfo>) -> Option<Loaded> {
    if original["content"]["msgtype"] != ::hagency::agent_run_controls::MSGTYPE {
        return None;
    }
    let card = original["event_id"].as_str()?.to_owned();
    Some(Loaded { card, original: (original, info), edit })
}

/// The cards not handed over yet in `generation`, now marked as being admitted.
fn claim(generation: u64, cards: Vec<Loaded>) -> Vec<Loaded> {
    let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
    if handed.len() > MAX_HANDED {
        handed.retain(|_, state| matches!(state, Handed::Admitting(_)));
    }
    cards.into_iter().filter(|c| {
        let state = handed.entry(c.key(generation)).or_insert(Handed::Failed(0));
        match *state {
            Handed::Failed(n) if n < MAX_TRIES => { *state = Handed::Admitting(n); true }
            _ => false,
        }
    }).collect()
}

/// Marks `card` handed over only once the store holds it with its latest
/// edit; otherwise a later timeline update tries it again (`MAX_TRIES`).
fn settle(generation: u64, room: &str, card: &Loaded) {
    let stored = stored(room, card);
    let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
    let state = handed.entry(card.key(generation)).or_insert(Handed::Failed(0));
    *state = match *state {
        _ if stored => Handed::Stored,
        Handed::Admitting(n) | Handed::Failed(n) => Handed::Failed(n.saturating_add(1)),
        Handed::Stored => Handed::Failed(1),
    };
}

/// The edit to admit: as the event cache has it, with its own encryption
/// info (the timeline item keeps none for its edit), else the timeline's
/// JSON without it, which only a known-unencrypted room admits.
fn edit_to_admit(card: &Loaded, fetched: Option<Admit>) -> Option<Admit> {
    let id = card.edit_id();
    fetched.filter(|(v, _)| !id.is_empty() && v["event_id"] == id)
        .or_else(|| card.edit.clone().map(|e| (e, None)))
}

async fn fetch_edit(room: &Room, card: &Loaded) -> Option<Admit> {
    let id = OwnedEventId::try_from(card.edit_id()).ok()?;
    // A failed lookup falls back to the timeline's JSON; a card left unstored
    // is retried on the next timeline update (`settle`).
    let event = match room.load_or_fetch_event(&id, None).await {
        Ok(event) => event,
        Err(e) => {
            makepad_widgets::log!("Loaded run cards: edit {id} not in the event cache ({e}); using the timeline's copy");
            return None;
        }
    };
    Some((json(event.raw())?, event.encryption_info().map(|info| (**info).clone())))
}

/// Admits the run cards among `items` (a room timeline's loaded items).
pub fn ingest(room: &Room, items: &imbl::Vector<Arc<TimelineItem>>) {
    let cards: Vec<_> = items.iter().filter_map(|i| i.as_event()).filter_map(loaded).collect();
    let generation = super::pending_questions::generation();
    let found = cards.len();
    let cards = claim(generation, cards);
    makepad_widgets::log!("Loaded run cards: ingest {} with {} items: {found} run cards, {} to admit (generation {generation})",
        room.room_id(), items.len(), cards.len());
    if cards.is_empty() {
        return;
    }
    let room = room.clone();
    crate::sliding_sync::spawn_async_task(super::run_controls::as_history(admit_with_retries(room, generation, cards)));
}

/// Pauses before each try: a card can fail at start-up while the room's
/// state or the login is still settling, with no later timeline update.
const TRIES_AFTER: [std::time::Duration; 3] = [
    std::time::Duration::ZERO, std::time::Duration::from_secs(3), std::time::Duration::from_secs(15),
];

fn stored(room: &str, card: &Loaded) -> bool {
    super::run_controls::stored_revision(room, &card.card).is_some_and(|r| r >= card.revision())
}

/// Admits `cards`, trying the ones not stored again after a pause.
async fn admit_with_retries(room: Room, generation: u64, mut cards: Vec<Loaded>) {
    let room_id = room.room_id().to_owned();
    for (attempt, pause) in TRIES_AFTER.iter().enumerate() {
        tokio::time::sleep(*pause).await;
        if super::pending_questions::generation() != generation {
            break;
        }
        let mut changed = false;
        for card in &cards {
            let mut events = vec![card.original.clone()];
            if card.edit.is_some() {
                events.extend(edit_to_admit(card, fetch_edit(&room, card).await));
            }
            changed |= super::pending_questions::admit_loaded(generation, &room, events).await;
            makepad_widgets::log!("Loaded run cards: try {} {} (edit {}) stored_revision={:?}, wanted {}, shown on request {}: {} card(s)",
                attempt + 1, card.card, card.edit_id(), super::run_controls::stored_revision(room_id.as_str(), &card.card),
                card.revision(), card.request(), super::run_controls::for_event(&room_id, card.request(), true).len());
        }
        makepad_widgets::log!("Loaded run cards: {room_id} try {} done, changed={changed}{}", attempt + 1,
            if changed { ", PendingQuestionsChanged posted" } else { "" });
        if changed {
            Cx::post_action(super::pending_questions::PendingQuestionsChanged);
        }
        let (done, left): (Vec<_>, Vec<_>) = cards.into_iter().partition(|c| stored(room_id.as_str(), c));
        done.iter().for_each(|c| settle(generation, room_id.as_str(), c));
        cards = left;
        if cards.is_empty() {
            return;
        }
    }
    cards.iter().for_each(|c| settle(generation, room_id.as_str(), c));
}

#[cfg(test)]
#[path = "loaded_run_cards_tests.rs"]
mod tests;
