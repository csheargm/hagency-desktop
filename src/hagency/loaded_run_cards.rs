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
#[derive(Clone)]
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

/// An Agent run card (not its edits) is a timeline item. The SDK's default
/// filter drops unknown msgtypes, so a card never became one: after a restart
/// there was nothing to admit or hide (live, Nov 19 02:57: "0 run cards").
pub fn is_timeline_event(event: &AnySyncTimelineEvent) -> bool {
    use matrix_sdk::ruma::events::{AnySyncMessageLikeEvent, room::message::Relation};
    let AnySyncTimelineEvent::MessageLike(AnySyncMessageLikeEvent::RoomMessage(message)) = event else {
        return false;
    };
    message.as_original().is_some_and(|m| {
        m.content.msgtype.msgtype() == ::hagency::agent_run_controls::MSGTYPE
            && !matches!(m.content.relates_to, Some(Relation::Replacement(_)))
    })
}

fn json(raw: &Raw<AnySyncTimelineEvent>) -> Option<Value> {
    raw.deserialize_as::<Value>().ok()
}

/// `id` for a log line when it is a plain Matrix event or room ID, else
/// `<invalid>`: a card's ids come from its sender and must not inject log text.
fn loggable(id: &str) -> &str {
    let valid = OwnedEventId::try_from(id).is_ok() || matrix_sdk::ruma::RoomId::parse(id).is_ok();
    if valid && id.chars().all(|c| c.is_ascii_graphic()) { id } else { "<invalid>" }
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
    // An older login's claims never apply again (its tasks end on sign-out).
    handed.retain(|key, _| key.0 == generation);
    if handed.len() > MAX_HANDED {
        handed.retain(|_, state| matches!(state, Handed::Admitting(_)));
    }
    cards.into_iter().filter(|c| claim_key(&mut handed, c.key(generation))).collect()
}

/// Marks `key` as being admitted unless it is stored, being admitted, or out of tries.
fn claim_key(handed: &mut HashMap<Key, Handed>, key: Key) -> bool {
    let state = handed.entry(key).or_insert(Handed::Failed(0));
    match *state {
        Handed::Failed(n) if n < MAX_TRIES => { *state = Handed::Admitting(n); true }
        _ => false,
    }
}

/// Marks `card` handed over only once the store holds it with its latest
/// edit; otherwise a later timeline update tries it again (`MAX_TRIES`).
fn settle(generation: u64, room: &str, card: &Loaded) {
    settle_key(card.key(generation), stored(room, card));
}

fn settle_key(key: Key, stored: bool) {
    let mut handed = HANDED.lock().unwrap_or_else(|e| e.into_inner());
    let state = handed.entry(key).or_insert(Handed::Failed(0));
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
    let event = room.load_or_fetch_event(&id, None).await.ok()?;
    Some((json(event.raw())?, event.encryption_info().map(|info| (**info).clone())))
}

/// Admits the run cards among `items` (a room timeline's loaded items).
pub fn ingest(room: &Room, items: &imbl::Vector<Arc<TimelineItem>>) {
    let cards: Vec<_> = items.iter().filter_map(|i| i.as_event()).filter_map(loaded).collect();
    let generation = super::pending_questions::generation();
    let found = cards.len();
    let cards = claim(generation, cards);
    if cards.is_empty() {
        return;
    }
    makepad_widgets::log!("Loaded run cards: {} has {found} run cards, admitting {}", loggable(room.room_id().as_str()), cards.len());
    let unsettled = Unsettled { generation, room: room.room_id().to_string(), cards };
    let room = room.clone();
    crate::sliding_sync::spawn_async_task(super::run_controls::as_history(admit_with_retries(room, unsettled)));
}

/// A room's rooms-list latest event, when it is a run card or a card's edit.
enum Latest {
    Card(Loaded),
    /// The SDK's latest event is the card's latest edit; the card is fetched.
    EditOf { card: String, edit: Value },
}
impl Latest {
    fn key(&self, generation: u64) -> Key {
        match self {
            Latest::Card(card) => card.key(generation),
            Latest::EditOf { card, edit } => (generation, card.clone(), edit["event_id"].as_str().unwrap_or_default().to_owned()),
        }
    }
}

fn latest_of(event: Value, info: Option<EncryptionInfo>) -> Option<Latest> {
    let relates = &event["content"]["m.relates_to"];
    if relates["rel_type"] != "m.replace" {
        return cards_of(event, None, info).map(Latest::Card);
    }
    let card = relates["event_id"].as_str()?.to_owned();
    let is_card = event["content"]["m.new_content"]["msgtype"] == ::hagency::agent_run_controls::MSGTYPE;
    is_card.then_some(Latest::EditOf { card, edit: event })
}

/// The card to admit for `latest`, its own event as the event cache has it.
async fn loaded_latest(room: &Room, latest: &Latest) -> Option<Loaded> {
    let (card, edit) = match latest {
        Latest::Card(card) => return Some(card.clone()),
        Latest::EditOf { card, edit } => (card, edit),
    };
    let event = room.load_or_fetch_event(&OwnedEventId::try_from(card.as_str()).ok()?, None).await.ok()?;
    let info = event.encryption_info().map(|info| (**info).clone());
    cards_of(json(event.raw())?, Some(edit.clone()), info).filter(|loaded| &loaded.card == card)
}

/// On screen Nov 19 03:39: rooms not opened since start-up kept "<agent>:
/// Agent run" in the rooms list, as only an open room's timeline handed its
/// cards to `ingest`. A room whose latest event is a run card (or its edit)
/// has exactly that card admitted, once per card and edit per login
/// (`claim_key`), through the same admission boundary; admission then
/// refreshes the room's preview (`run_card_previews::after_admission`).
pub fn admit_latest(room: &Room) {
    use matrix_sdk::latest_events::LatestEventValue;
    let LatestEventValue::Remote(event) = std::ops::Deref::deref(room).latest_event() else { return };
    let Some(value) = json(event.raw()) else { return };
    let Some(latest) = latest_of(value, event.encryption_info().map(|info| (**info).clone())) else { return };
    let generation = super::pending_questions::generation();
    let key = latest.key(generation);
    if !claim_key(&mut HANDED.lock().unwrap_or_else(|e| e.into_inner()), key.clone()) {
        return;
    }
    let room = room.clone();
    crate::sliding_sync::spawn_async_task(super::run_controls::as_history(async move {
        // Each fetch waits for an admission slot; a failed one is tried again after a pause.
        let Some(card) = super::admission_queue::with_backoff(&TRIES_AFTER, || loaded_latest(&room, &latest)).await else {
            // Not fetched (or not a card): a later preview update tries again (`MAX_TRIES`).
            return settle_key(key, false);
        };
        let unsettled = Unsettled { generation, room: room.room_id().to_string(), cards: vec![card] };
        admit_with_retries(room, unsettled).await;
    }));
}

/// Pauses before each try: a card can fail at start-up while the room's
/// state or the login is still settling, with no later timeline update.
const TRIES_AFTER: [std::time::Duration; 3] = [
    std::time::Duration::ZERO, std::time::Duration::from_secs(3), std::time::Duration::from_secs(15),
];

fn stored(room: &str, card: &Loaded) -> bool {
    super::run_controls::stored_revision(room, &card.card).is_some_and(|r| r >= card.revision())
}

/// Claimed cards not settled yet. Dropping it settles them, so a task ended
/// early (sign-out aborts account tasks) leaves no claim stuck as admitting.
struct Unsettled {
    generation: u64,
    room: String,
    cards: Vec<Loaded>,
}
impl Drop for Unsettled {
    fn drop(&mut self) {
        for card in &self.cards {
            if !stored(&self.room, card) {
                makepad_widgets::log!("Loaded run cards: card {} in {} not admitted", loggable(&card.card), loggable(&self.room));
            }
            settle(self.generation, &self.room, card);
        }
    }
}

/// One try at the unsettled cards, each edit fetched first; whether any changed.
async fn admit_once(room: &Room, unsettled: &Unsettled) -> bool {
    let mut changed = false;
    for card in &unsettled.cards {
        let mut events = vec![card.original.clone()];
        if card.edit.is_some() {
            events.extend(edit_to_admit(card, fetch_edit(room, card).await));
        }
        changed |= super::pending_questions::admit_loaded(unsettled.generation, room, events).await;
    }
    changed
}

/// Admits the unsettled cards (one admission slot per try, `admission_queue`),
/// trying the ones not stored again after a pause.
async fn admit_with_retries(room: Room, mut unsettled: Unsettled) {
    for pause in TRIES_AFTER {
        tokio::time::sleep(pause).await;
        if super::pending_questions::generation() != unsettled.generation {
            return;
        }
        if super::admission_queue::limited(admit_once(&room, &unsettled)).await {
            Cx::post_action(super::pending_questions::PendingQuestionsChanged);
        }
        let (done, left): (Vec<_>, Vec<_>) = std::mem::take(&mut unsettled.cards).into_iter()
            .partition(|c| stored(&unsettled.room, c));
        done.iter().for_each(|c| settle(unsettled.generation, &unsettled.room, c));
        unsettled.cards = left;
        if unsettled.cards.is_empty() {
            return;
        }
    }
}

#[cfg(test)]
#[path = "loaded_run_cards_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "loaded_run_cards_latest_tests.rs"]
mod latest_tests;
