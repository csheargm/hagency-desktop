//! Recent requests for one Agent, newest first, for the owner's Activity view.
//! Read-only: room, requester, receipt time, a coarse outcome and the tokens
//! the settled call charged. Request and reply text never leave this module.
use crate::inbox::Dispatch;
use crate::{Error, Ledger, Result, Usage, key};
use serde::Serialize;

/// Largest page the Activity view may ask for.
pub const MAX_RECENT: usize = 100;

/// What became of one request, as the owner reads it.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Received, prepared or running; no result yet.
    Running,
    /// A reply was produced (delivered or waiting to be delivered).
    Answered,
    /// Turned away without a model call.
    Declined,
    /// Failed, or ended with an unknown result.
    Failed,
}
impl Outcome {
    /// Inbox `state` (schema.sql `local_inbox.state`) to outcome.
    pub fn of_state(state: &str) -> Self {
        match state {
            "reply_ready" | "replied" => Self::Answered,
            "rejected" => Self::Declined,
            "failed" | "unknown" => Self::Failed,
            _ => Self::Running,
        }
    }
}

/// One recent request.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub dispatch_id: String,
    pub binding_id: String,
    pub room_id: String,
    pub requester: String,
    /// Unix seconds when this device durably received the request.
    pub received_at: i64,
    pub outcome: Outcome,
    /// Input plus output tokens of the settled call; `None` when unknown.
    pub tokens: Option<u64>,
}

/// Tokens a settled call charged; unreported or unsettled usage is unknown.
fn charged(call_state: Option<&str>, usage: Option<&str>) -> Option<u64> {
    if call_state != Some("settled") {
        return None;
    }
    let usage: Usage = serde_json::from_str(usage?).ok()?;
    usage.category_counts().3?;
    Some(usage.input.saturating_add(usage.output))
}

const RECENT: &str = "SELECT i.envelope,i.state,i.received_at,c.state,c.usage FROM local_inbox i \
JOIN bindings b ON b.id=i.binding LEFT JOIN calls c ON c.id=i.execution_id \
WHERE b.agent=? ORDER BY i.received_at DESC,i.id DESC LIMIT ?";

impl Ledger {
    /// The last `limit` requests for `agent`, newest first.
    pub fn recent_activity(&self, owner: &str, agent: &str, limit: usize) -> Result<Vec<Activity>> {
        self.owner(owner)?;
        key(agent)?;
        if !(1..=MAX_RECENT).contains(&limit) {
            return Err(Error::Invalid("activity query limit"));
        }
        let mut stmt = self.db.prepare(RECENT)?;
        let rows = stmt.query_map((agent, limit), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (envelope, state, received_at, call_state, usage) = row?;
            let dispatch: Dispatch = serde_json::from_str(&envelope)?;
            out.push(Activity {
                dispatch_id: dispatch.id,
                binding_id: dispatch.binding_id,
                room_id: dispatch.room_id,
                requester: dispatch.requester_mxid,
                received_at,
                outcome: Outcome::of_state(&state),
                tokens: charged(call_state.as_deref(), usage.as_deref()),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
