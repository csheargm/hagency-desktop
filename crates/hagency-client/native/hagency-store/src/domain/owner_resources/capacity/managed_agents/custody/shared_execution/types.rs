//! Closed first-text-turn wire types. No wire object confers execution custody.
use super::*;
type Result<T> = std::result::Result<T, Error>;
const MAX_EXACT: u64 = JSON_SAFE_MAX;
fn key(value: &str) -> Result<()> {
    Ok(identifier(value, 128)?)
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn allocation_digest(value: &impl Serialize) -> Result<String> {
    Ok(canonical::digest(&serde_json::to_value(value)?)?)
}
fn hash_text(value: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub const SHARED_EXECUTION_CAPABILITY: &str = "hagency.managed_agents.execution.v1";
pub const SHARED_TEXT_POLICY: &str = "codex_text_v1";
pub const SHARED_CLAUDE_CAPABILITY: &str = "hagency.managed_agents.execution.v4";
pub const SHARED_CLAUDE_POLICY: &str = "claude_text_v1";
pub const MAX_SHARED_TEXT: usize = 4096;
pub const MAX_SHARED_REPLY: usize = 32 * 1024;
fn exact(n: u64) -> bool {
    (1..=MAX_EXACT).contains(&n)
}
fn invalid() -> Error {
    Error::LocalAuthority
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedRuntimeCommand {
    pub version: u16,
    pub capability: String,
    pub operation: String,
    pub command_id: String,
    pub assignment_id: String,
    pub assignment_generation: u64,
    pub agent_reservation_id: String,
    pub reserve_command: ManagedAgentReserveCommand,
    pub reserve_command_digest: String,
    pub reserve_receipt_digest: String,
    pub policy: String,
    /// Deadline for original one-shot issuance, never an activity renewal.
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}
impl SharedRuntimeCommand {
    /// V1 always means Codex. Claude has a distinct signed capability/version;
    /// old command bytes, digests and retries retain their exact interpretation.
    pub fn runtime(&self) -> Result<&'static str> {
        match (self.version, self.capability.as_str(), self.policy.as_str()) {
            (1, SHARED_EXECUTION_CAPABILITY, SHARED_TEXT_POLICY) => Ok("codex"),
            (4, SHARED_CLAUDE_CAPABILITY, SHARED_CLAUDE_POLICY) => Ok("claude"),
            _ => Err(invalid()),
        }
    }
    pub fn execution_policy(&self, web: bool, mode: Option<&str>) -> Result<&'static str> {
        Ok(match self.runtime()? {
            "claude" => hagency_runtime::owned::SharedClaudePolicy::from_options(web, mode)
                .map_err(|_| invalid())?
                .name(),
            _ => hagency_runtime::owned::shared_execution_policy(web, mode),
        })
    }
    pub fn validate(&self, server: &str) -> Result<()> {
        for id in [
            &self.command_id,
            &self.assignment_id,
            &self.agent_reservation_id,
        ] {
            key(id)?;
        }
        self.reserve_command.validate(server)?;
        if self.runtime().is_err()
            || self.operation != "agent.execution.provision"
            || self.assignment_generation != 1
            || self.reserve_command.definition.tool_mode != "none"
            || self.reserve_command_digest != allocation_digest(&self.reserve_command)?
            || !valid_digest(&self.reserve_receipt_digest)
            || !exact(self.issued_at_ms)
            || !exact(self.expires_at_ms)
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms - self.issued_at_ms > 30_000
            || (self.reserve_command.pool_grant.is_none()
                && self.expires_at_ms > self.reserve_command.expires_at_ms)
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedProcessReceipt {
    pub version: u16,
    pub observation_id: String,
    pub command_id: String,
    pub command_digest: String,
    pub assignment_id: String,
    pub agent_reservation_id: String,
    pub ledger_revision: u64,
    /// Native original process binding; absent if no process was proved.
    pub process_digest: Option<String>,
    pub state: String,
    pub policy_digest: Option<String>,
}
impl SharedProcessReceipt {
    pub fn validate_for(&self, c: &SharedRuntimeCommand) -> Result<()> {
        key(&self.observation_id)?;
        if self.version != 1
            || self.command_id != c.command_id
            || self.command_digest != allocation_digest(c)?
            || self.assignment_id != c.assignment_id
            || self.agent_reservation_id != c.agent_reservation_id
            || !exact(self.ledger_revision)
            || !matches!(
                self.state.as_str(),
                "qualified" | "stopped" | "unknown" | "not_started"
            )
            || self.state == "not_started"
                && (!c
                    .reserve_command
                    .pool_grant
                    .as_ref()
                    .is_some_and(|g| g.calendar.is_some())
                    || c.reserve_command.renewal_mode.is_none()
                    || self.process_digest.is_some()
                    || self.policy_digest.is_some())
            || self
                .process_digest
                .as_deref()
                .is_some_and(|v| !valid_digest(v))
            || self
                .policy_digest
                .as_deref()
                .is_some_and(|v| !valid_digest(v))
            || (self.state == "qualified"
                && (self.process_digest.is_none() || self.policy_digest.is_none()))
            || (self.state != "qualified" && self.policy_digest.is_some())
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedRuntimeDelivery {
    pub command: SharedRuntimeCommand,
    pub command_digest: String,
    pub token: String,
    pub lease_until_ms: u64,
    pub execution_allowed: bool,
    pub cleanup_allowed: bool,
    pub recorded: Option<SharedProcessReceipt>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedProcessInput {
    pub command_digest: String,
    pub token: String,
    pub receipt: SharedProcessReceipt,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedActivityRef {
    pub runtime_command_id: String,
    pub epoch: u64,
    pub token: String,
}
impl SharedActivityRef {
    pub fn validate(&self) -> Result<()> {
        key(&self.runtime_command_id)?;
        if !exact(self.epoch) || !valid_digest(&self.token) {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedActivityRenewInput {
    pub authority: SharedActivityRef,
    pub renewal_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedActivityLease {
    pub authority: SharedActivityRef,
    pub command_digest: String,
    pub process_digest: String,
    pub expires_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedRuntimeAcknowledgement {
    pub observation_id: String,
    pub receipt_digest: String,
    /// Present only after a fresh qualified result and current authority check.
    /// Historical receipt recovery never renews an expired activity.
    pub activity: Option<SharedActivityLease>,
    pub state: String,
}
/// Authenticated server evidence for an additional owner-bound Project Room.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedRoomBinding {
    pub id: String,
    pub generation: u64,
    pub assignment_id: String,
    pub room_id: String,
    pub space_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_binding: Option<SharedRoomBinding>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub encrypted: bool,
    pub id: String,
    pub assignment_id: String,
    pub runtime_command_digest: String,
    pub event_id: String,
    pub room_id: String,
    pub requester_mxid: String,
    pub thread_root: String,
    pub body: String,
    pub body_digest: String,
    pub execution_id: String,
    pub created_at_ms: u64,
}
impl SharedWorkItem {
    pub fn validate(&self) -> Result<()> {
        if let Some(b) = &self.room_binding {
            key(&b.id)?;
            if b.generation == 0
                || b.assignment_id != self.assignment_id
                || b.room_id != self.room_id
                || !b.space_id.starts_with('!')
                || b.space_id.len() > 255
            {
                return Err(invalid());
            }
        }
        key(&self.id)?;
        key(&self.assignment_id)?;
        key(&self.execution_id)?;
        if !valid_digest(&self.runtime_command_digest)
            || !valid_digest(&self.body_digest)
            || !exact(self.created_at_ms)
            || (self.encrypted && self.body != self.event_id)
            || self.body.is_empty()
            || self.body.len() > MAX_SHARED_TEXT
            || self.body_digest != hash_text(&self.body)
            || !self.event_id.starts_with('$')
            || self.event_id.len() > 255
            || !self.room_id.starts_with('!')
            || self.room_id.len() > 255
            || !self.requester_mxid.starts_with('@')
            || self.requester_mxid.len() > 255
            || !self.thread_root.starts_with('$')
            || self.thread_root.len() > 255
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkInput {
    pub authority: SharedActivityRef,
    pub work_id: String,
    pub work_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkStart {
    pub work: SharedWorkItem,
    pub work_digest: String,
    pub activity_epoch: u64,
    pub newly_started: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkReceipt {
    pub version: u16,
    pub observation_id: String,
    pub work_id: String,
    pub work_digest: String,
    pub execution_id: String,
    pub call_id: String,
    pub runtime_command_digest: String,
    pub process_digest: String,
    pub ledger_revision: u64,
    pub state: String,
    pub known_consumed_tokens: u64,
    pub retained_tokens: Option<u64>,
    pub usage_complete: bool,
    /// Exact durable result. Missing output is never reconstructed from a rerun.
    pub body: Option<String>,
    pub body_digest: Option<String>,
}
impl SharedWorkReceipt {
    pub fn validate_for(&self, w: &SharedWorkItem, process: &str) -> Result<()> {
        key(&self.observation_id)?;
        key(&self.call_id)?;
        if self.version != 1
            || self.work_id != w.id
            || self.work_digest != allocation_digest(w)?
            || self.execution_id != w.execution_id
            || self.runtime_command_digest != w.runtime_command_digest
            || self.process_digest != process
            || !valid_digest(&self.process_digest)
            || !exact(self.ledger_revision)
            || !matches!(self.state.as_str(), "completed" | "failed" | "unknown")
            || !(0..=MAX_EXACT).contains(&self.known_consumed_tokens)
            || self
                .retained_tokens
                .is_some_and(|n| n < self.known_consumed_tokens || n > MAX_EXACT)
            || self.usage_complete && self.retained_tokens != Some(self.known_consumed_tokens)
            || serde_json::to_vec(self).map_or(true, |v| v.len() > 48 * 1024)
            || self.body.is_some() != (self.state == "completed")
            || self.body.is_some() != self.body_digest.is_some()
            || self.body.as_ref().is_some_and(|b| {
                b.is_empty() || b.len() > MAX_SHARED_REPLY || Some(hash_text(b)) != self.body_digest
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkResultInput {
    pub authority: SharedActivityRef,
    pub receipt: SharedWorkReceipt,
}
/// UI observation only: no lease token, input, reply, provider or receipt data.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkSummary {
    pub id: String,
    pub execution_id: String,
    pub state: String,
    pub reply_state: Option<String>,
    pub known_consumed_tokens: Option<u64>,
    pub retained_tokens: Option<u64>,
    pub usage_complete: bool,
    pub created_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedExecutionStatus {
    pub assignment_id: String,
    pub state: String,
    pub policy: String,
    pub activity_expires_at_ms: Option<u64>,
    pub work_available: bool,
    pub capacity_released: bool,
    pub recent_work: Vec<SharedWorkSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkPollInput {
    pub authority: SharedActivityRef,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkRecoverInput {
    pub runtime_command_digest: String,
    pub work_digest: String,
    pub observation_id: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkResultTicket {
    pub work_id: String,
    pub work_digest: String,
    pub token: String,
    pub lease_until_ms: u64,
    pub recorded: Option<SharedWorkReceipt>,
    pub state: String,
    pub reply_state: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkResultUpload {
    pub work_digest: String,
    pub token: String,
    pub receipt: SharedWorkReceipt,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkAcknowledgement {
    pub observation_id: String,
    pub receipt_digest: String,
    pub state: String,
    pub reply_state: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkQuery {
    pub after: Option<String>,
    pub limit: Option<u16>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedObservedUsage {
    pub known_consumed_tokens: u64,
    pub retained_tokens: Option<u64>,
    pub complete: bool,
    pub period_key: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkHistoryItem {
    pub id: String,
    pub execution_id: String,
    pub event_id: String,
    pub requester_mxid: String,
    pub state: String,
    pub reply_state: Option<String>,
    pub created_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub completed_at_ms: Option<u64>,
    pub usage: Option<SharedObservedUsage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedWorkPage {
    pub items: Vec<SharedWorkHistoryItem>,
    pub next_after: Option<String>,
}
