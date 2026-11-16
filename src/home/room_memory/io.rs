//! Reads and writes a room's memory through the signed-in Matrix client, with
//! the same storage rules as the Agent device (ADR 0013): state events in a
//! plaintext room, encrypted timeline events in an encrypted one. State is
//! never read in an encrypted room. Every failure comes back as text to show.
use super::model::{Delivery, Write};
use hagency::room_memory::{self, Entry, MEMORY_STATE_TYPE, PowerLevels, Storage};
use hagency::room_skills::SKILL_STATE_TYPE;
use hagency::room_memory_timeline::{ScanKind, ScanPage, ScanState, ScanStep, TimelineScan, SCAN_PAGE_EVENTS, reduce_timeline};
use matrix_sdk::{
    Client,
    deserialized_responses::{TimelineEvent, TimelineEventKind},
    room::{MessagesOptions, Room},
    ruma::{OwnedRoomId, RoomId, UserId, events::room::power_levels::UserPowerLevel},
};
use serde_json::Value;

/// A room's memory as the signed-in person sees it.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub room_id: OwnedRoomId,
    pub me: String,
    pub storage: Storage,
    pub entries: Vec<Entry>,
    pub levels: PowerLevels,
    /// Encrypted rooms only: events this device could not read, a scan that
    /// stopped early, and revisions ignored for lack of power level.
    pub unreadable: usize,
    pub truncated: bool,
    pub ignored: usize,
    /// Names for authors and acceptors (display, never raw ids).
    pub people: super::people::People,
}
impl Loaded {
    pub fn can_write(&self) -> bool {
        self.levels.can_write(&self.me)
    }
}

fn unavailable() -> String {
    crate::i18n::tr("Room memory is unavailable right now. Try again in a moment.").to_owned()
}

/// An unknown encryption state is an error, never a plaintext read or write.
pub(crate) async fn open(client: &Client, room_id: &RoomId) -> Result<(Room, Storage), String> {
    let room = client.get_room(room_id).ok_or_else(unavailable)?;
    let security = room.latest_encryption_state().await.map_err(|_| unavailable())?;
    if security.is_unknown() {
        return Err(unavailable());
    }
    Ok((room, room_memory::storage_for(security.is_encrypted())))
}

pub async fn load(client: &Client, room_id: &RoomId) -> Result<Loaded, String> {
    let me = client.user_id().ok_or_else(unavailable)?.to_string();
    let (room, storage) = open(client, room_id).await?;
    let mut loaded = Loaded {
        room_id: room_id.to_owned(), me, storage, entries: Vec::new(), levels: PowerLevels::default(),
        unreadable: 0, truncated: false, ignored: 0, people: Default::default(),
    };
    match storage {
        Storage::State => {
            // From the server: the sync store does not hold custom state types.
            let events = super::server_state::events_of_type(client, room_id, MEMORY_STATE_TYPE).await?;
            loaded.entries = room_memory::reduce(&events);
            loaded.people.accepted_by = super::people::acceptors(&events);
            loaded.levels = levels(&room, std::iter::once(loaded.me.clone())).await?;
        }
        Storage::Timeline => {
            let scan = scan(&room, MEMORY_STATE_TYPE).await?;
            let senders = scan.events.iter().filter_map(|e| e["sender"].as_str().map(String::from)).chain([loaded.me.clone()]);
            loaded.levels = levels(&room, senders).await?;
            let view = reduce_timeline(&scan, &loaded.levels);
            (loaded.entries, loaded.unreadable, loaded.truncated, loaded.ignored) = (view.entries, view.unreadable, view.truncated, view.ignored);
        }
    }
    loaded.people.names = names(&room, &loaded).await;
    Ok(loaded)
}

/// Display names of everyone the entries name; an unknown member keeps no name.
async fn names(room: &Room, loaded: &Loaded) -> std::collections::BTreeMap<String, String> {
    let users: std::collections::BTreeSet<String> = loaded.entries.iter().map(|e| e.author.clone())
        .chain(loaded.people.accepted_by.values().cloned()).collect();
    let mut names = std::collections::BTreeMap::new();
    for user in users {
        let Ok(id) = UserId::parse(user.as_str()) else { continue };
        // A member that cannot be read is shown by its fallback name, not an error.
        if let Ok(Some(member)) = room.get_member_no_sync(&id).await
            && let Some(name) = member.display_name()
        {
            names.insert(user, name.to_owned());
        }
    }
    names
}

/// Sends one write under the signed-in person's own user.
pub async fn send(client: &Client, room_id: &RoomId, storage: Storage, write: &Write) -> Result<(), String> {
    let (room, current) = open(client, room_id).await?;
    if current != storage {
        return Err(unavailable());
    }
    let sent = match &write.delivery {
        Delivery::State { state_key } => room.send_state_event_raw(MEMORY_STATE_TYPE, state_key, write.content.clone()).await.map(|_| ()),
        Delivery::Timeline => room.send_raw(MEMORY_STATE_TYPE, write.content.clone()).await.map(|_| ()),
    };
    sent.map_err(|error| crate::i18n::format("Could not save to room memory: {reason}", &[("reason", error.to_string())]))
}

/// A bounded backward scan for one record type (memory or team skills).
pub(crate) async fn scan(room: &Room, kind: &str) -> Result<TimelineScan, String> {
    let (mut state, mut from) = (ScanState::default(), None::<String>);
    loop {
        let mut options = MessagesOptions::backward();
        options.from = from.clone();
        options.limit = SCAN_PAGE_EVENTS.into();
        let page = room.messages(options).await.map_err(|_| unavailable())?;
        let step = state.push(ScanPage { events: page.chunk.iter().map(|e| scan_event_of(e, kind)).collect(), next: page.end });
        match step {
            ScanStep::Next(token) => from = Some(token),
            ScanStep::Done => return Ok(state.finish()),
        }
    }
}

#[cfg(test)]
pub(super) fn scan_event(event: &TimelineEvent) -> (String, ScanKind) {
    scan_event_of(event, MEMORY_STATE_TYPE)
}
pub(crate) fn scan_event_of(event: &TimelineEvent, wanted: &str) -> (String, ScanKind) {
    let Some(raw) = serde_json::from_str::<Value>(event.kind.raw().json().get()).ok() else {
        return (String::new(), ScanKind::Unreadable);
    };
    let id = raw["event_id"].as_str().unwrap_or_default().to_string();
    let untrusted = untrusted(event, &raw);
    let is_wanted = raw["type"].as_str() == Some(wanted);
    let kind = match (is_wanted, untrusted) {
        (_, true) if matches!(event.kind, TimelineEventKind::UnableToDecrypt { .. }) => ScanKind::Unreadable,
        (true, true) => ScanKind::Unreadable,
        // Only a decrypted event is authenticated: a plaintext record event in
        // an encrypted room could have been written by the homeserver.
        (true, false) if matches!(event.kind, TimelineEventKind::PlainText { .. }) => ScanKind::Unreadable,
        (true, false) => ScanKind::Memory(raw),
        _ => ScanKind::Other,
    };
    (id, kind)
}

/// The Agent device's authenticity rule: undecryptable, a sender that differs
/// from the device that encrypted it, or a sender whose identity changed.
fn untrusted(event: &TimelineEvent, raw: &Value) -> bool {
    match &event.kind {
        TimelineEventKind::UnableToDecrypt { .. } => true,
        TimelineEventKind::PlainText { .. } => false,
        TimelineEventKind::Decrypted(d) => super::authentic::identity_untrusted(
            d.encryption_info.sender.as_str(),
            &d.encryption_info.verification_state,
            raw["sender"].as_str().unwrap_or_default(),
        ),
    }
}

/// Power levels for exactly the given users, so a creator with infinite power
/// and an unlisted member are both judged by the room's rules.
pub(crate) async fn levels(room: &Room, users: impl Iterator<Item = String>) -> Result<PowerLevels, String> {
    let sdk = room.power_levels().await.map_err(|_| unavailable())?;
    let mut levels = PowerLevels { users_default: i64::from(sdk.users_default), state_default: Some(i64::from(sdk.state_default)), ..Default::default() };
    for kind in [MEMORY_STATE_TYPE, SKILL_STATE_TYPE] {
        if let Some(level) = sdk.events.get(&kind.into()) {
            levels.events.insert(kind.into(), i64::from(*level));
        }
    }
    for user in users {
        let Ok(id) = <&UserId>::try_from(user.as_str()) else { continue };
        match sdk.for_user(id) {
            UserPowerLevel::Infinite => levels.users.insert(user, i64::MAX),
            UserPowerLevel::Int(int) => levels.users.insert(user, i64::from(int)),
            _ => continue,
        };
    }
    Ok(levels)
}
