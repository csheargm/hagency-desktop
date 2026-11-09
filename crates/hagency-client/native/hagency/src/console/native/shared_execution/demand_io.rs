//! Closed schema17 B routes. Offers and bindings describe history, not launch authority.
use super::*;
use serde_json::{Value, json};
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Status {
    pub offer: SharedDemandOffer,
    pub demand_digest: String,
    pub state: String,
    pub runtime_command_id: Option<String>,
    pub binding: Option<SharedDemandBinding>,
    /// The latest retry of this event (ADR-193 amendment), once this
    /// executor asked for one.
    #[serde(default)]
    pub retry: Option<u64>,
}
impl Status {
    pub fn validate(&self) -> Result<(), NativeError> {
        if self.offer.digest().map_err(store)? != self.demand_digest
            || self
                .retry
                .is_some_and(|n| !(1..=MAX_DEMAND_RETRIES).contains(&n))
            || self.state == "retry_offered" && self.retry.is_none()
            || !matches!(
                self.state.as_str(),
                "offered"
                    | "start_committed"
                    | "retry_offered"
                    | "bound"
                    | "cancelled"
                    | "expired"
                    | "head_changed"
                    | "budget_blocked"
                    | "usage_unrepresentable"
                    | "blocked"
            )
        {
            return Err(invalid());
        }
        if let Some(id) = &self.runtime_command_id {
            super::super::key(id)?;
        }
        if let Some(b) = &self.binding
            && (b.version != 1
                || b.demand_id != self.offer.demand_id
                || b.demand_digest != self.demand_digest
                || b.assignment_id != self.offer.assignment_id
                || b.event_id != self.offer.event_id
                || b.received_at_ms != self.offer.received_at_ms
                || b.control_revision != self.offer.control_revision
                || self.runtime_command_id.as_ref() != Some(&b.runtime_command_id))
        {
            return Err(invalid());
        }
        if self.state == "bound" {
            self.binding.as_ref().ok_or_else(invalid)?.validate_offer(&self.offer).map_err(store)?;
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PollReply {
    pub demand: Option<Status>,
    pub next_after: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RecoverReply {
    pub demand: Option<Status>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WorkBinding {
    pub offer: SharedDemandOffer,
    pub demand_digest: String,
    pub binding: SharedDemandBinding,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BindingReply {
    pub demand: Option<WorkBinding>,
}
/// The executor's not_started evidence for a demand's latest attempt, with its
/// request to run the same event again in place (ADR-193 amendment).
#[derive(Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RetryInput {
    pub demand_digest: String,
    pub expected_control_revision: u64,
    pub retry: u64,
    pub attempt_id: String,
    pub result: super::attempt_io::ResultInput,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RetryReply {
    pub acknowledgement: super::attempt_io::Acknowledgement,
    pub demand: Status,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct BlockInput {
    pub demand_digest: String,
    pub expected_control_revision: u64,
    pub reason: String,
    pub budget: Option<SharedBudgetObservation>,
    /// Sent only to a server that advertises retries (ADR-193 amendment).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub retries: bool,
}
pub(super) enum DemandAction {
    Discovery,
    Control {
        assignment: String,
    },
    Stop {
        assignment: String,
        input: SharedDemandStopInput,
    },
    /// `retries`: this executor runs retries and the server advertises them;
    /// only then may a status carry them (ADR-193 amendment).
    Poll {
        after: Option<String>,
        retries: bool,
    },
    Recover {
        id: String,
        digest: String,
        retries: bool,
    },
    Binding {
        work: String,
        work_digest: String,
        runtime_digest: String,
    },
    Block {
        id: String,
        input: BlockInput,
    },
    Retry {
        id: String,
        input: RetryInput,
    },
}
impl DemandAction {
    pub(super) fn validate(&self) -> Result<(), NativeError> {
        // Native context and engine negotiation share this read-only endpoint.
        // Enabling execution must not also enable intake mutations.
        if !cfg!(feature = "shared-intake")
            && !(cfg!(feature = "shared-execution") && matches!(self, Self::Discovery))
        {
            return Err(failure(409, "shared_intake_disabled"));
        }
        match self {
            Self::Discovery => {}
            Self::Control { assignment } => super::super::key(assignment)?,
            Self::Stop { assignment, input } => {
                super::super::key(assignment)?;
                super::super::key(&input.command_id)?;
                if input.expected_revision == 0
                    || input.expected_revision >= 9_007_199_254_740_991
                    || input.mode != "paused"
                {
                    return Err(invalid());
                }
            }
            Self::Poll { after, .. } => {
                if let Some(id) = after {
                    super::super::key(id)?;
                }
            }
            Self::Recover { id, digest, .. } => {
                super::super::key(id)?;
                hash(digest)?;
            }
            Self::Binding {
                work,
                work_digest,
                runtime_digest,
            } => {
                super::super::key(work)?;
                hash(work_digest)?;
                hash(runtime_digest)?;
            }
            Self::Block { id, input } => {
                super::super::key(id)?;
                hash(&input.demand_digest)?;
                if input.expected_control_revision == 0
                    || input.expected_control_revision > 9_007_199_254_740_991
                    || !matches!(
                        input.reason.as_str(),
                        "budget_blocked" | "usage_unrepresentable"
                    )
                {
                    return Err(invalid());
                }
            }
            Self::Retry { id, input } => {
                super::super::key(id)?;
                super::super::key(&input.attempt_id)?;
                hash(&input.demand_digest)?;
                hash(&input.result.command_digest)?;
                hash(&input.result.token)?;
                if input.expected_control_revision == 0
                    || input.expected_control_revision > 9_007_199_254_740_991
                    || !(1..=MAX_DEMAND_RETRIES + 1).contains(&input.retry)
                    || input.result.receipt.state != "not_started"
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
    pub(super) fn body(self) -> (String, Value) {
        match self {
            Self::Discovery => (String::new(), Value::Null),
            Self::Control { assignment } => {
                (format!("assignments/{assignment}/control"), Value::Null)
            }
            Self::Stop { assignment, input } => {
                (format!("assignments/{assignment}/control"), json!(input))
            }
            Self::Poll { after, retries } => (
                "demands/poll".into(),
                if retries { json!({"after":after,"retries":true}) } else { json!({"after":after}) },
            ),
            Self::Recover { id, digest, retries } => (
                format!("demands/{id}/recover"),
                if retries { json!({"demandDigest":digest,"retries":true}) } else { json!({"demandDigest":digest}) },
            ),
            Self::Binding {
                work,
                work_digest,
                runtime_digest,
            } => (
                format!("work/{work}/demand"),
                json!({"workDigest":work_digest,"runtimeCommandDigest":runtime_digest}),
            ),
            Self::Block { id, input } => (format!("demands/{id}/block"), json!(input)),
            Self::Retry { id, input } => (format!("demands/{id}/retry"), json!(input)),
        }
    }
}
impl From<DemandAction> for Action {
    fn from(v: DemandAction) -> Self {
        Self::Demand(Box::new(v))
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RunControl {
    pub assignment_id: String,
    pub revision: u64,
    pub mode: String,
    pub running_since_ms: Option<u64>,
    pub updated_at_ms: u64,
    pub origin: String,
}
impl RunControl {
    pub fn validate(&self, id: &str) -> Result<(), NativeError> {
        if self.assignment_id != id
            || self.revision == 0
            || self.revision >= 9_007_199_254_740_991
            || !matches!(self.mode.as_str(), "manual" | "running" | "paused")
            || self.updated_at_ms == 0
            || self.updated_at_ms > 9_007_199_254_740_991
            || self
                .running_since_ms
                .is_some_and(|v| v == 0 || v > self.updated_at_ms)
            || self.origin.len() > 128
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ControlReply {
    pub control: RunControl,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StopReply {
    pub command_id: String,
    pub applied_revision: u64,
    pub control: RunControl,
}

// Discovery is extensible; only this bounded protocol-capability projection is
// consumed. It never substitutes for authenticated B route policy checks.
#[derive(Deserialize)]
pub(super) struct Discovery {
    pub capabilities: Vec<String>,
}
pub(super) async fn paired(b: &Bound) -> Result<bool, NativeError> {
    let d = b.demand_discovery().await?;
    if d.capabilities.len() > 64 || d.capabilities.iter().any(|c| c.len() > 128) {
        return Err(invalid());
    }
    Ok(d.capabilities
        .iter()
        .any(|c| c == "shared-durable-demand-v1"))
}
/// Whether the server retries an event whose attempt never started.
pub(super) async fn retries(b: &Bound) -> Result<bool, NativeError> {
    let d = b.demand_discovery().await?;
    if d.capabilities.len() > 64 || d.capabilities.iter().any(|c| c.len() > 128) {
        return Err(invalid());
    }
    Ok(d.capabilities.iter().any(|c| c == "shared-demand-retry-v1"))
}
pub(super) async fn require_pair(b: &Bound) -> Result<(), NativeError> {
    if !paired(b).await? {
        return Err(failure(409, "shared_intake_protocol_unavailable"));
    }
    Ok(())
}
