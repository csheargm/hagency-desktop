//! Team skills for one admitted dispatch: the injected index and `skill_read`.
//! This module has no way to write a skill: `skill_propose` only posts a card. Encrypted rooms read
//! encrypted timeline revisions, kept only from senders with the power level.
use super::{Source, e2ee::MemoryRoom};
use hagency_runtime::room_memory::{Storage, storage_for};
use hagency_runtime::room_skills::{self as skills, Skill, SkillOperation};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(super) trait SkillIo {
    async fn room(&self) -> Result<MemoryRoom, &'static str>;
    async fn post(&self, txn: &str, content: Value) -> Result<String, &'static str>;
    /// (dispatch id, request event id, agent id)
    fn identity(&self) -> (&str, &str, &str);
}
impl SkillIo for Source {
    async fn room(&self) -> Result<MemoryRoom, &'static str> {
        let room = &self.gate.expected.room_id;
        self.authorize(Some(room)).await?;
        self.device.skill_room(room).await.map_err(|_| "skills_unavailable")
    }
    async fn post(&self, txn: &str, content: Value) -> Result<String, &'static str> {
        super::memory::MemoryIo::post(self, txn, content).await.map_err(|_| "skill_propose_failed")
    }
    fn identity(&self) -> (&str, &str, &str) {
        super::memory::MemoryIo::identity(self)
    }
}

/// Current skills. State is never consulted in an encrypted room.
fn current(room: &MemoryRoom) -> Vec<Skill> {
    match storage_for(room.encrypted) {
        Storage::State => skills::reduce_state(&room.events),
        Storage::Timeline => skills::reduce_timeline(&room.scan.events, &room.levels).0,
    }
}

/// The index for the next prompt. Anything unexpected means no index, never a
/// failed turn.
pub(super) async fn index<I: SkillIo>(io: &I) -> Option<String> {
    let room = io.room().await.ok()?;
    skills::render_index(&current(&room))
}

pub(super) async fn handle<I: SkillIo>(io: &I, operation: &SkillOperation) -> String {
    let room = match io.room().await {
        Ok(room) => room,
        Err(code) => return json!({"error": code}).to_string(),
    };
    match operation {
        SkillOperation::Read { name, offset } => skills::render_read(&current(&room), name, *offset),
        SkillOperation::Propose { skill, reason } => {
            let edit = current(&room).iter().any(|s| s.name == skill.name);
            propose(io, skill, reason, edit).await
        }
    }
}

async fn propose<I: SkillIo>(io: &I, skill: &Skill, reason: &str, edit: bool) -> String {
    let (dispatch, event, agent) = io.identity();
    let digest = format!("{:x}", Sha256::digest(format!("{dispatch}\0{}\0{}", skill.name, skill.body)));
    let id = format!("sk_{}", &digest[..16]);
    let content = skills::proposal::proposal_content(&id, skill, reason, event, agent, edit);
    match io.post(&format!("hagency_skill_{}", &digest[..24]), content).await {
        Ok(event_id) => json!({"status":"proposed","proposal_id":id,"event_id":event_id,"edit":edit,
            "note":"Not saved yet: a person with enough power level must accept the card."})
        .to_string(),
        Err(code) => json!({"error": code}).to_string(),
    }
}

#[cfg(test)]
mod tests;
