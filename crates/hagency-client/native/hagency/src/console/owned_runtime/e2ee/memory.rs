//! Room memory reads and proposal cards on the Agent's own device. The device
//! never writes memory: a person's client does, on Accept. A plaintext room
//! keeps entries as `io.hagency.memory` state; an encrypted room keeps them as
//! encrypted `io.hagency.memory` timeline events, rebuilt here by a bounded
//! backward scan (ADR 0013). Nothing is ever read from state in an encrypted room.
use super::{history::unavailable_reason, *};
use hagency_runtime::room_memory::{MEMORY_STATE_TYPE, PowerLevels};
use hagency_runtime::room_memory_timeline::{ScanKind, ScanPage, ScanState, ScanStep, TimelineScan, SCAN_PAGE_EVENTS};
use matrix_sdk::{
    room::{MessagesOptions, Room},
    ruma::events::StateEventType,
};
use serde_json::Value;

pub(in crate::console) struct MemoryRoom {
    pub encrypted: bool,
    /// Raw state events; always empty for an encrypted room.
    pub events: Vec<Value>,
    /// Timeline scan; empty for a plaintext room.
    pub scan: TimelineScan,
    /// The room's power levels, read only for an encrypted room.
    pub levels: PowerLevels,
}
impl AgentDevice {
    /// An unknown encryption state is an error, never a plaintext read.
    pub(in crate::console) async fn memory_room(&self, room: &str) -> Result<MemoryRoom, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let joined = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let security = joined.latest_encryption_state().await.map_err(|_| RuntimeError::Transport)?;
        if security.is_unknown() {
            return Err(RuntimeError::Transport);
        }
        let mut memory = MemoryRoom {
            encrypted: security.is_encrypted(),
            events: Vec::new(),
            scan: TimelineScan::default(),
            levels: PowerLevels::default(),
        };
        if memory.encrypted {
            memory.scan = self.memory_scan(&joined, room).await?;
            memory.levels = memory_levels(&joined, &memory.scan).await?;
            return Ok(memory);
        }
        let raw = joined
            .get_state_events(StateEventType::from(MEMORY_STATE_TYPE))
            .await
            .map_err(|_| RuntimeError::Transport)?;
        memory.events = raw
            .iter()
            .map(|event| serde_json::to_value(event).map_err(|_| RuntimeError::Transport))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(memory)
    }

    async fn memory_scan(&self, room: &Room, key: &str) -> Result<TimelineScan, RuntimeError> {
        let cached = self.memory_scans.lock().map_err(|_| RuntimeError::Transport)?.get(key).cloned();
        let (mut state, mut from) = (ScanState::default(), None::<String>);
        loop {
            let mut options = MessagesOptions::backward();
            options.from = from.clone();
            options.limit = SCAN_PAGE_EVENTS.into();
            let page = room.messages(options).await.map_err(|_| RuntimeError::Transport)?;
            let first = state.newest().is_none();
            let step = state.push(ScanPage {
                events: page.chunk.iter().map(scan_event).collect(),
                next: page.end,
            });
            if first
                && let Some(hit) = cached.as_ref().filter(|c| c.newest.is_some() && c.newest.as_deref() == state.newest())
            {
                return Ok(hit.clone());
            }
            match step {
                ScanStep::Next(token) => from = Some(token),
                ScanStep::Done => break,
            }
        }
        let scan = state.finish();
        // An undecryptable event may become readable when its key arrives with
        // no new event id, so such a scan is never reused.
        let mut cache = self.memory_scans.lock().map_err(|_| RuntimeError::Transport)?;
        if scan.unreadable == 0 {
            cache.insert(key.into(), scan.clone());
        } else {
            cache.remove(key);
        }
        Ok(scan)
    }

    /// Posts the proposal card under a transaction fixed by the caller, so a
    /// replay returns the original event. In an encrypted room the card is an
    /// ordinary encrypted message; it never changes memory.
    pub(in crate::console) async fn post_memory_proposal(
        &self,
        room: &str,
        txn: &str,
        content: Value,
    ) -> Result<String, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let joined = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let security = joined.latest_encryption_state().await.map_err(|_| RuntimeError::Transport)?;
        if security.is_unknown() {
            return Err(RuntimeError::Transport);
        }
        self.send_room_message(room, txn, content).await
    }
}

fn scan_event(event: &matrix_sdk::deserialized_responses::TimelineEvent) -> (String, ScanKind) {
    let Some(raw) = serde_json::from_str::<Value>(event.kind.raw().json().get()).ok() else {
        return (String::new(), ScanKind::Unreadable);
    };
    let id = raw["event_id"].as_str().unwrap_or_default().to_string();
    let undecrypted = matches!(&event.kind, TimelineEventKind::UnableToDecrypt { .. });
    let kind = match (raw["type"].as_str(), unavailable_reason(event, &raw)) {
        (_, Some(_)) if undecrypted => ScanKind::Unreadable,
        (Some(MEMORY_STATE_TYPE), Some(_)) => ScanKind::Unreadable,
        (Some(MEMORY_STATE_TYPE), None) => ScanKind::Memory(raw),
        _ => ScanKind::Other,
    };
    (id, kind)
}

/// Power levels for exactly the senders in the scan, so a creator with
/// infinite power and an unlisted member are both judged by the room's rules.
async fn memory_levels(room: &Room, scan: &TimelineScan) -> Result<PowerLevels, RuntimeError> {
    use matrix_sdk::ruma::{UserId, events::room::power_levels::UserPowerLevel};
    let sdk = room.power_levels().await.map_err(|_| RuntimeError::Transport)?;
    let mut levels = PowerLevels { users_default: i64::from(sdk.users_default), ..Default::default() };
    levels.state_default = Some(i64::from(sdk.state_default));
    if let Some(level) = sdk.events.get(&MEMORY_STATE_TYPE.into()) {
        levels.events.insert(MEMORY_STATE_TYPE.into(), i64::from(*level));
    }
    for event in &scan.events {
        let Some(sender) = event["sender"].as_str().and_then(|s| <&UserId>::try_from(s).ok()) else { continue };
        let level = match sdk.for_user(sender) {
            UserPowerLevel::Infinite => i64::MAX,
            UserPowerLevel::Int(int) => i64::from(int),
            _ => continue,
        };
        levels.users.insert(sender.to_string(), level);
    }
    Ok(levels)
}
