//! Room memory for one admitted dispatch: the injected block and the two Agent
//! tools. Reads and proposals only. This module has no way to write memory.
use super::{Source, e2ee::MemoryRoom};
use hagency_runtime::room_memory::{self as memory, MemoryOperation, Storage};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) trait MemoryIo {
    async fn room(&self) -> Result<MemoryRoom, &'static str>;
    async fn post(&self, txn: &str, content: Value) -> Result<String, &'static str>;
    /// (dispatch id, request event id, agent id)
    fn identity(&self) -> (&str, &str, &str);
}
impl MemoryIo for Source {
    async fn room(&self) -> Result<MemoryRoom, &'static str> {
        let room = &self.gate.expected.room_id;
        self.authorize(Some(room)).await?;
        self.device.memory_room(room).await.map_err(|_| "memory_unavailable")
    }
    async fn post(&self, txn: &str, content: Value) -> Result<String, &'static str> {
        let room = &self.gate.expected.room_id;
        self.authorize(Some(room)).await?;
        self.device
            .post_memory_proposal(room, txn, content)
            .await
            .map_err(|_| "memory_propose_failed")
    }
    fn identity(&self) -> (&str, &str, &str) {
        let d = &self.gate.expected;
        (&d.id, &d.event_id, &d.agent_id)
    }
}

/// The block for the next prompt. Anything unexpected means no block, never a
/// failed turn. Encrypted rooms inject nothing.
pub(super) async fn block<I: MemoryIo>(io: &I) -> Option<String> {
    let room = io.room().await.ok()?;
    if memory::storage_for(room.encrypted) == Storage::Unsupported {
        return None;
    }
    memory::render_block(&memory::reduce(&room.events))
}

pub(super) async fn handle<I: MemoryIo>(io: &I, operation: &MemoryOperation) -> String {
    let room = match io.room().await {
        Ok(room) => room,
        Err(code) => return json!({"error": code}).to_string(),
    };
    if memory::storage_for(room.encrypted) == Storage::Unsupported {
        return memory::encrypted_error();
    }
    match operation {
        MemoryOperation::List => memory::render_list(&memory::reduce(&room.events)),
        MemoryOperation::Propose { text, reason } => propose(io, text, reason).await,
    }
}

async fn propose<I: MemoryIo>(io: &I, text: &str, reason: &str) -> String {
    let (dispatch, event, agent) = io.identity();
    let digest = format!("{:x}", Sha256::digest(format!("{dispatch}\0{text}")));
    let id = format!("mem_{}", &digest[..16]);
    let content = memory::proposal_content(&id, text, reason, event, agent);
    match io.post(&format!("hagency_memory_{}", &digest[..24]), content).await {
        Ok(event_id) => json!({"status":"proposed","entry_id":id,"event_id":event_id,
            "note":"Not saved yet: a person with enough power level must accept the card."})
        .to_string(),
        Err(code) => json!({"error": code}).to_string(),
    }
}

#[cfg(test)]
mod tests;
