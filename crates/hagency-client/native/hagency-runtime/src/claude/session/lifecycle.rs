//! Native completion evidence. A main-loop result does not stop background work.
use super::{Error, EventKind, Operation, Phase, SessionDriver};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Default)]
pub(super) struct Completion {
    enabled: bool,
    idle: bool,
    result: bool,
    background: BTreeSet<String>,
    changed_after_result: bool,
    result_had_background: bool,
    saw_child_work: bool,
    snapshot: Option<String>,
}
impl Completion {
    pub(super) fn observe(&mut self, kind: EventKind, payload: &Value) -> Result<(), Error> {
        if !self.enabled {
            return Ok(());
        }
        match kind {
            EventKind::System => match payload["subtype"].as_str() {
                Some("session_state_changed") => {
                    self.idle = match payload["state"].as_str() {
                        Some("idle") => true,
                        Some("running" | "requires_action") => false,
                        _ => return Err(Error::Protocol(super::super::Error::Envelope)),
                    };
                }
                Some("background_tasks_changed") => {
                    let tasks = payload["tasks"].as_array().ok_or(Error::Identity)?;
                    if tasks.len() > 1024 {
                        return Err(Error::Capacity);
                    }
                    let mut background = BTreeSet::new();
                    for task in tasks {
                        let id = task["task_id"]
                            .as_str()
                            .filter(|id| super::super::identifier(id))
                            .ok_or(Error::Identity)?;
                        if task.get("ambient").is_some_and(|v| !v.is_boolean()) {
                            return Err(Error::Identity);
                        }
                        if task["ambient"] != true && !background.insert(id.to_owned()) {
                            return Err(Error::Identity);
                        }
                    }
                    if background != self.background && self.result {
                        self.changed_after_result = true;
                    }
                    // This is the authoritative full live set, not a delta.
                    self.saw_child_work |= !background.is_empty();
                    self.background = background;
                }
                Some("task_started" | "task_progress" | "task_notification" | "task_updated")
                    if payload["ambient"] != true =>
                {
                    self.saw_child_work = true;
                    if self.result {
                        self.changed_after_result = true;
                    }
                }
                _ => {}
            },
            EventKind::Result => {
                if let Some(id) = &self.snapshot
                    && !correlated(payload, id)
                {
                    return Err(Error::Identity);
                }
                self.result = true;
                self.changed_after_result = false;
                self.result_had_background = !self.background.is_empty()
                    || (self.saw_child_work
                        && !payload["modelUsage"]
                            .as_object()
                            .is_some_and(|models| !models.is_empty()));
            }
            EventKind::Assistant
            | EventKind::Stream
            | EventKind::ToolProgress
            | EventKind::ToolSummary
                if self.result =>
            {
                self.changed_after_result = true;
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn complete(&self) -> bool {
        self.enabled && self.result && self.idle && self.background.is_empty()
    }
    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }
}

impl<R, W, E> SessionDriver<R, W, E> {
    /// Use only while the host continuously supervises the admitted turn and
    /// owns process cleanup. Native reasoning/tool silence has no wall-clock
    /// ceiling; protocol frames, writes and individual callbacks remain bounded.
    /// This must precede any callback and cannot renew an existing grant.
    pub fn supervise_turn(&mut self) -> Result<(), Error> {
        if self.phase != Phase::Running
            || self.session_id.is_none()
            || self.awaiting_init
            || !self.control.untouched()
            || self.room_mcp.as_ref().is_some_and(|state| !state.untouched())
        {
            return Err(Error::State);
        }
        self.wire.supervise()
    }
    /// Host opt-in before initialization. Every input/result is bound to its
    /// native UUID, while the native session and compaction remain unchanged.
    pub fn enable_continuous_session(&mut self) -> Result<(), Error> {
        if self.phase != Phase::New || !self.completion.enabled || self.continuous {
            return Err(Error::State);
        }
        self.continuous = true;
        Ok(())
    }
    pub fn can_followup(&self) -> bool {
        self.continuous
            && self.phase == Phase::ResultObserved
            && self.completion.complete()
            && !self.needs_usage_snapshot()
            && self.control.quiescent()
            && self.room_mcp.as_ref().is_none_or(|state| state.quiescent())
            && self.task_mcp.is_none()
            && self.wire.termination().is_none()
    }
    /// A new host-admitted turn gets fresh IO bounds and private receipt and
    /// approval identities. This never extends an active turn or its grants.
    pub fn begin_followup(&mut self, limits: super::Limits) -> Result<(), Error> {
        if self.wire.has_buffered_input() {
            return Err(Error::State);
        }
        self.reset_followup(limits)
    }
    fn reset_followup(&mut self, limits: super::Limits) -> Result<(), Error> {
        if !self.can_followup() {
            return Err(Error::State);
        }
        self.wire.begin_turn(limits)?;
        self.observations.retire();
        self.observations = Default::default();
        self.source = std::sync::Arc::new(());
        self.control = Default::default();
        if let Some(state) = &mut self.room_mcp {
            state.begin_turn();
        }
        self.completion = Completion {
            enabled: true,
            ..Default::default()
        };
        self.prelude_session_id = None;
        self.prompt_id = None;
        self.phase = Phase::Ready;
        Ok(())
    }
    /// Pair with CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS=1 and
    /// CLAUDE_CODE_BG_TASKS_REPORT_RUNNING=1 on the owned CLI process.
    pub fn enable_native_completion(&mut self) -> Result<(), Error> {
        if !matches!(self.phase, Phase::New | Phase::Ready) || self.session_id.is_some() {
            return Err(Error::State);
        }
        self.completion.enabled = true;
        Ok(())
    }
    /// A fresh native total is needed if background work outlived the result.
    pub fn needs_usage_snapshot(&self) -> bool {
        self.completion.complete()
            && (self.completion.changed_after_result || self.completion.result_had_background)
    }
    pub fn usage_snapshot_requested(&self) -> bool {
        self.completion.snapshot.is_some()
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// Confirm the retained process still answers before the host reserves or
    /// transmits a new model input. This metadata request performs no inference.
    pub async fn prepare_followup(&mut self, limits: super::Limits) -> Result<(), Error> {
        self.reset_followup(limits)?;
        let operation = Operation::new(self, Phase::Ready)?;
        let result = operation.driver.followup_probe().await;
        operation.finish(result)
    }
    async fn followup_probe(&mut self) -> Result<(), Error> {
        let id = prompt_id();
        let until = self
            .wire
            .event_deadline()
            .min(tokio::time::Instant::now() + std::time::Duration::from_secs(10));
        self.wire
            .send(
                super::super::encode(json!({"type":"control_request","request_id":id,
            "request":{"subtype":"mcp_status"}}))?,
                until,
            )
            .await?;
        for _ in 0..64 {
            match self.wire.next(until).await?.message {
                super::Message::ControlResponse {
                    request_id,
                    outcome: super::ControlOutcome::Success(status),
                } if request_id == id => {
                    self.verify_retained_room_server(&status)?;
                    return Ok(());
                }
                super::Message::Mcp {
                    request_id,
                    server_name,
                    message,
                } => {
                    self.reply_room_metadata(&request_id, &server_name, &message, until)
                        .await?;
                }
                super::Message::Event {
                    session_id,
                    kind,
                    payload,
                } if Some(session_id.as_str()) == self.session_id.as_deref()
                    && (matches!(kind, EventKind::AuthStatus | EventKind::RateLimit)
                        || (kind == EventKind::System
                            && payload["subtype"] == "session_state_changed"
                            && payload["state"] == "idle")
                        || (kind == EventKind::CommandLifecycle
                            && payload["state"] == "completed")) => {}
                _ => return Err(Error::Identity),
            }
        }
        Err(Error::Capacity)
    }
    /// Query the CLI's local /usage command after the whole turn is idle. This
    /// keeps later child usage in the original call and does not run the model.
    /// The host retains the original answer and accepts only the correlated
    /// native result for accounting. At most one snapshot is sent per call.
    pub async fn request_usage_snapshot(&mut self) -> Result<(), Error> {
        if !self.needs_usage_snapshot()
            || self.completion.snapshot.is_some()
            || self.room_mcp.as_ref().is_some_and(|s| !s.quiescent())
        {
            return Err(Error::State);
        }
        let operation = Operation::new(self, Phase::ResultObserved)?;
        operation.driver.wire.finish_activity();
        let id = prompt_id();
        let bytes = super::super::encode(json!({
            "type":"user", "uuid":id,
            "session_id":operation.driver.session_id,
            "parent_tool_use_id":null,
            "message":{"role":"user","content":"/usage"}
        }))?;
        operation.driver.completion.snapshot = Some(id.clone());
        operation.driver.awaiting_init = true;
        operation.driver.prompt_id = Some(id);
        operation.driver.completion.idle = false;
        operation.driver.completion.result = false;
        operation.driver.completion.changed_after_result = false;
        operation.driver.phase = Phase::Running;
        let result = operation
            .driver
            .wire
            .send(bytes, operation.driver.wire.event_deadline())
            .await
            .map(|_| ());
        operation.finish(result)
    }
}

pub(super) fn correlated(payload: &Value, id: &str) -> bool {
    let single = payload
        .get("user_message_uuid")
        .filter(|value| !value.is_null());
    let multiple = payload
        .get("user_message_uuids")
        .filter(|value| !value.is_null());
    (single.is_some() || multiple.is_some())
        && single.is_none_or(|value| value == id)
        && multiple.is_none_or(|value| {
            value
                .as_array()
                .is_some_and(|ids| ids.len() == 1 && ids[0] == id)
        })
}
pub(super) fn prompt_id() -> String {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let mut hash = Sha256::new();
    hash.update(std::process::id().to_be_bytes());
    hash.update(SEQUENCE.fetch_add(1, Ordering::Relaxed).to_be_bytes());
    hash.update(format!("{:?}", std::time::SystemTime::now()).as_bytes());
    let hex = format!("{:x}", hash.finalize());
    format!(
        "{}-{}-4{}-a{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[13..16],
        &hex[17..20],
        &hex[20..32]
    )
}
