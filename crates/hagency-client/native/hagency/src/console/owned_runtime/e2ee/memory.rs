//! Room memory reads and proposal cards on the Agent's own device. The device
//! never writes memory: a person's client does, on Accept. A plaintext room
//! keeps entries as `io.hagency.memory` state; an encrypted room keeps them as
//! encrypted `io.hagency.memory` timeline events, rebuilt here by a bounded
//! backward scan (ADR 0013). Nothing is ever read from state in an encrypted room.
use super::{history::unavailable_reason, *};
use hagency_runtime::room_memory::{MEMORY_STATE_TYPE, PowerLevels};
use hagency_runtime::room_skills::SKILL_STATE_TYPE;
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
        self.record_room(room, MEMORY_STATE_TYPE).await
    }
    /// Team skills, read by exactly the same rules as memory.
    pub(in crate::console) async fn skill_room(&self, room: &str) -> Result<MemoryRoom, RuntimeError> {
        self.record_room(room, SKILL_STATE_TYPE).await
    }
    async fn record_room(&self, room: &str, kind: &str) -> Result<MemoryRoom, RuntimeError> {
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
            memory.scan = self.memory_scan(&joined, &format!("{kind}\n{room}"), kind).await?;
            memory.levels = memory_levels(&joined, &memory.scan).await?;
            return Ok(memory);
        }
        let raw = joined
            .get_state_events(StateEventType::from(kind))
            .await
            .map_err(|_| RuntimeError::Transport)?;
        memory.events = raw
            .iter()
            .map(|event| serde_json::to_value(event).map_err(|_| RuntimeError::Transport))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(memory)
    }

    async fn memory_scan(&self, room: &Room, key: &str, kind: &str) -> Result<TimelineScan, RuntimeError> {
        let cached = self.memory_scans.lock().map_err(|_| RuntimeError::Transport)?.get(key).cloned();
        let (mut state, mut from) = (ScanState::default(), None::<String>);
        loop {
            let mut options = MessagesOptions::backward();
            options.from = from.clone();
            options.limit = SCAN_PAGE_EVENTS.into();
            let page = room.messages(options).await.map_err(|_| RuntimeError::Transport)?;
            let first = state.newest().is_none();
            let step = state.push(ScanPage {
                events: page.chunk.iter().map(|e| scan_event(e, kind)).collect(),
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
        let content = with_display_name(&joined, &self.client, content).await;
        self.send_room_message(room, txn, content).await
    }
}

/// Names this Agent in a memory proposal's fallback body, for clients that do
/// not draw the card (Element). No profile yet keeps the generic "Agent".
async fn with_display_name(room: &Room, client: &matrix_sdk::Client, mut content: Value) -> Value {
    let Some(me) = client.user_id() else { return content };
    if content.get(hagency_runtime::room_memory::MEMORY_PROPOSAL_KEY).is_none() {
        return content;
    }
    if let Ok(Some(member)) = room.get_member_no_sync(me).await
        && let Some(name) = member.display_name().filter(|n| !n.trim().is_empty())
    {
        hagency_runtime::room_memory::name_proposal(&mut content, name);
    }
    content
}

fn scan_event(event: &matrix_sdk::deserialized_responses::TimelineEvent, kind: &str) -> (String, ScanKind) {
    let Some(raw) = serde_json::from_str::<Value>(event.kind.raw().json().get()).ok() else {
        return (String::new(), ScanKind::Unreadable);
    };
    let id = raw["event_id"].as_str().unwrap_or_default().to_string();
    let undecrypted = matches!(&event.kind, TimelineEventKind::UnableToDecrypt { .. });
    // Only a decrypted event is authenticated: a plaintext `io.hagency.memory`
    // event in an encrypted room was sent in the clear, so the homeserver could
    // have written it. It is counted as unreadable, never trusted.
    let plaintext = matches!(&event.kind, TimelineEventKind::PlainText { .. });
    let wanted = raw["type"].as_str() == Some(kind);
    let found = match unavailable_reason(event, &raw) {
        Some(_) if undecrypted || wanted => ScanKind::Unreadable,
        None if wanted && plaintext => ScanKind::Unreadable,
        None if wanted => ScanKind::Memory(raw),
        _ => ScanKind::Other,
    };
    (id, found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::deserialized_responses::TimelineEvent;
    use matrix_sdk::ruma::serde::Raw;

    fn plaintext(kind: &str) -> TimelineEvent {
        let json = serde_json::json!({"type":kind,"event_id":"$e","sender":"@m:example.test",
            "origin_server_ts":1u64,"content":{"id":"a","text":"injected","rev":1}});
        TimelineEvent::from_plaintext(Raw::from_json_string(json.to_string()).unwrap())
    }

    #[test]
    fn plaintext_memory_event_in_an_encrypted_room_is_unreadable_not_memory() {
        assert!(matches!(scan_event(&plaintext(MEMORY_STATE_TYPE), MEMORY_STATE_TYPE).1, ScanKind::Unreadable));
        assert!(matches!(scan_event(&plaintext("m.room.message"), MEMORY_STATE_TYPE).1, ScanKind::Other));
        // The same rule holds for team skills, and one type never reads as the other.
        assert!(matches!(scan_event(&plaintext(SKILL_STATE_TYPE), SKILL_STATE_TYPE).1, ScanKind::Unreadable));
        assert!(matches!(scan_event(&plaintext(SKILL_STATE_TYPE), MEMORY_STATE_TYPE).1, ScanKind::Other));
    }
}

/// Power levels for exactly the senders in the scan, so a creator with
/// infinite power and an unlisted member are both judged by the room's rules.
async fn memory_levels(room: &Room, scan: &TimelineScan) -> Result<PowerLevels, RuntimeError> {
    use matrix_sdk::ruma::{UserId, events::room::power_levels::UserPowerLevel};
    let sdk = room.power_levels().await.map_err(|_| RuntimeError::Transport)?;
    let mut levels = PowerLevels { users_default: i64::from(sdk.users_default), ..Default::default() };
    levels.state_default = Some(i64::from(sdk.state_default));
    for kind in [MEMORY_STATE_TYPE, SKILL_STATE_TYPE] {
        if let Some(level) = sdk.events.get(&kind.into()) {
            levels.events.insert(kind.into(), i64::from(*level));
        }
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
