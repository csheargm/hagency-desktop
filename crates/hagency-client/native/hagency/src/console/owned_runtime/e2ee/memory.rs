//! Room memory reads and proposal cards on the Agent's own device. The device
//! never writes the `io.hagency.memory` state: a person's client does, on Accept.
use super::*;
use matrix_sdk::ruma::events::StateEventType;
use serde_json::Value;

pub(in crate::console) struct MemoryRoom {
    pub encrypted: bool,
    /// Raw state events; always empty for an encrypted room.
    pub events: Vec<Value>,
}

impl AgentDevice {
    /// Reads memory state only from a room the device knows to be unencrypted.
    /// An unknown encryption state is an error, never a plaintext read.
    pub(in crate::console) async fn memory_room(&self, room: &str) -> Result<MemoryRoom, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let room = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let security = room
            .latest_encryption_state()
            .await
            .map_err(|_| RuntimeError::Transport)?;
        if security.is_unknown() {
            return Err(RuntimeError::Transport);
        }
        if security.is_encrypted() {
            return Ok(MemoryRoom { encrypted: true, events: Vec::new() });
        }
        let raw = room
            .get_state_events(StateEventType::from(hagency_runtime::room_memory::MEMORY_STATE_TYPE))
            .await
            .map_err(|_| RuntimeError::Transport)?;
        let events = raw
            .iter()
            .map(|event| serde_json::to_value(event).map_err(|_| RuntimeError::Transport))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(MemoryRoom { encrypted: false, events })
    }

    /// Posts the proposal card under a transaction fixed by the caller, so a
    /// replay returns the original event. Plaintext rooms only.
    pub(in crate::console) async fn post_memory_proposal(
        &self,
        room: &str,
        txn: &str,
        content: Value,
    ) -> Result<String, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let joined = self.client.get_room(&room_id).ok_or(RuntimeError::Transport)?;
        let encrypted = joined
            .latest_encryption_state()
            .await
            .map_err(|_| RuntimeError::Transport)?;
        if encrypted.is_unknown() || encrypted.is_encrypted() {
            return Err(RuntimeError::Transport);
        }
        self.send_room_message(room, txn, content).await
    }
}
