//! Reads and writes a room's memory through the signed-in Matrix client, with
//! the same storage rules as the Agent device (ADR 0013): state events in a
//! plaintext room, encrypted timeline events in an encrypted one. State is
//! never read in an encrypted room. Every failure comes back as text to show.
use super::model::{Delivery, Write};
use hagency::room_memory::{self, Entry, MEMORY_STATE_TYPE, PowerLevels, Storage};
use hagency::room_memory_timeline::{ScanKind, ScanPage, ScanState, ScanStep, TimelineScan, SCAN_PAGE_EVENTS, reduce_timeline};
use matrix_sdk::{
    Client,
    deserialized_responses::{DeviceLinkProblem, TimelineEvent, TimelineEventKind, VerificationLevel, VerificationState},
    room::{MessagesOptions, Room},
    ruma::{OwnedRoomId, RoomId, UserId, events::{StateEventType, room::power_levels::UserPowerLevel}},
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
async fn open(client: &Client, room_id: &RoomId) -> Result<(Room, Storage), String> {
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
        unreadable: 0, truncated: false, ignored: 0,
    };
    match storage {
        Storage::State => {
            let raw = room.get_state_events(StateEventType::from(MEMORY_STATE_TYPE)).await.map_err(|_| unavailable())?;
            let events: Vec<Value> = raw.iter().filter_map(|e| serde_json::to_value(e).ok()).collect();
            loaded.entries = room_memory::reduce(&events);
            loaded.levels = levels(&room, std::iter::once(loaded.me.clone())).await?;
        }
        Storage::Timeline => {
            let scan = scan(&room).await?;
            let senders = scan.events.iter().filter_map(|e| e["sender"].as_str().map(String::from)).chain([loaded.me.clone()]);
            loaded.levels = levels(&room, senders).await?;
            let view = reduce_timeline(&scan, &loaded.levels);
            (loaded.entries, loaded.unreadable, loaded.truncated, loaded.ignored) = (view.entries, view.unreadable, view.truncated, view.ignored);
        }
    }
    Ok(loaded)
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

async fn scan(room: &Room) -> Result<TimelineScan, String> {
    let (mut state, mut from) = (ScanState::default(), None::<String>);
    loop {
        let mut options = MessagesOptions::backward();
        options.from = from.clone();
        options.limit = SCAN_PAGE_EVENTS.into();
        let page = room.messages(options).await.map_err(|_| unavailable())?;
        let step = state.push(ScanPage { events: page.chunk.iter().map(scan_event).collect(), next: page.end });
        match step {
            ScanStep::Next(token) => from = Some(token),
            ScanStep::Done => return Ok(state.finish()),
        }
    }
}

fn scan_event(event: &TimelineEvent) -> (String, ScanKind) {
    let Some(raw) = serde_json::from_str::<Value>(event.kind.raw().json().get()).ok() else {
        return (String::new(), ScanKind::Unreadable);
    };
    let id = raw["event_id"].as_str().unwrap_or_default().to_string();
    let untrusted = untrusted(event, &raw);
    let kind = match (raw["type"].as_str(), untrusted) {
        (_, true) if matches!(event.kind, TimelineEventKind::UnableToDecrypt { .. }) => ScanKind::Unreadable,
        (Some(MEMORY_STATE_TYPE), true) => ScanKind::Unreadable,
        (Some(MEMORY_STATE_TYPE), false) => ScanKind::Memory(raw),
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
        TimelineEventKind::Decrypted(d) => {
            d.encryption_info.sender.as_str() != raw["sender"].as_str().unwrap_or_default()
                || match &d.encryption_info.verification_state {
                    VerificationState::Verified => false,
                    VerificationState::Unverified(level) => matches!(
                        level,
                        VerificationLevel::None(DeviceLinkProblem::MissingDevice | DeviceLinkProblem::InsecureSource)
                            | VerificationLevel::MismatchedSender
                            | VerificationLevel::VerificationViolation
                    ),
                }
        }
    }
}

/// Power levels for exactly the given users, so a creator with infinite power
/// and an unlisted member are both judged by the room's rules.
async fn levels(room: &Room, users: impl Iterator<Item = String>) -> Result<PowerLevels, String> {
    let sdk = room.power_levels().await.map_err(|_| unavailable())?;
    let mut levels = PowerLevels { users_default: i64::from(sdk.users_default), state_default: Some(i64::from(sdk.state_default)), ..Default::default() };
    if let Some(level) = sdk.events.get(&MEMORY_STATE_TYPE.into()) {
        levels.events.insert(MEMORY_STATE_TYPE.into(), i64::from(*level));
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
