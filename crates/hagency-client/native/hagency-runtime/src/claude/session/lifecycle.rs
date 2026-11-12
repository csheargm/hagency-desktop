//! Native completion evidence. A main-loop result does not stop background work.
use super::{Error, EventKind, Operation, Phase, SessionDriver};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
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
    answer: Option<([u8; 32], bool)>,
    answer_text: sha2::Sha256,
    answer_joined: bool,
    pub(super) steers: super::steer::State,
    task_runs: BTreeMap<String, String>,
    /// Runs the native CLI listed in its live background set or flagged as
    /// backgrounded. Only such a run gets a notification turn once it ends.
    background_runs: BTreeSet<String>,
    notified_runs: BTreeSet<String>,
    consumed_runs: BTreeSet<String>,
    background_response: bool,
    stop_result: Option<String>,
    answer_stop: Option<String>,
    /// The open turn was logged once; see `report_open`.
    reported_open: bool,
}
impl Completion {
    /// A turn that has its result and native idle and still stays open waits
    /// for background work. Say once what it waits for: a turn that waits
    /// for something that never comes is otherwise silent.
    fn report_open(&mut self) {
        if !self.enabled || !self.result || !self.idle || self.reported_open || self.complete() {
            return;
        }
        self.reported_open = true;
        tracing::info!(
            code = "claude_turn_open_after_result",
            live_background_tasks = self.background.len(),
            awaited_notification_turns = self
                .notified_runs
                .iter()
                .filter(|run| self.background_runs.contains(*run))
                .count(),
            notification_turn_running = self.background_response,
            steers_settled = self.steers.settled(),
            "Claude turn has its result and is idle; it stays open for background work"
        );
    }
    fn remember_run(&mut self, task: &Value) -> Result<(), Error> {
        let Some(run) = task.get("run_id") else {
            return Ok(());
        };
        let run = run
            .as_str()
            .filter(|id| super::super::identifier(id))
            .ok_or(Error::Identity)?;
        let task = task["task_id"]
            .as_str()
            .filter(|id| super::super::identifier(id))
            .ok_or(Error::Identity)?;
        if let Some(original) = self.task_runs.get(run) {
            if original != task {
                return Err(Error::Identity);
            }
        } else {
            if self.task_runs.len() >= 1024 {
                return Err(Error::Capacity);
            }
            self.task_runs.insert(run.into(), task.into());
        }
        Ok(())
    }
    /// Call after `remember_run`: an unknown or mismatched run is not marked.
    fn mark_background(&mut self, task: &Value) {
        if let Some(run) = task["run_id"].as_str()
            && self.task_runs.get(run).map(String::as_str) == task["task_id"].as_str()
        {
            self.background_runs.insert(run.to_owned());
        }
    }
    pub(super) fn begin_background_response(&mut self, payload: &Value) -> Result<(), Error> {
        if !self.enabled
            || !self.result
            || self.snapshot.is_some()
            || self.background_response
            || self.steers.cycle_open()
            || self.notified_runs.is_empty()
            || payload
                .get("user_message_uuid")
                .is_some_and(|v| !v.is_null())
            || payload
                .get("user_message_uuids")
                .is_some_and(|v| !v.is_null())
        {
            return Err(Error::Identity);
        }
        self.background_response = true;
        self.idle = false;
        Ok(())
    }
    /// A second init that names user messages opens a steer cycle or is
    /// refused. False: it names none, and the background rules decide.
    pub(super) fn begin_steer_cycle(&mut self, payload: &Value) -> Result<bool, Error> {
        if ["user_message_uuid", "user_message_uuids"]
            .iter()
            .all(|key| payload.get(key).is_none_or(Value::is_null))
        {
            return Ok(false);
        }
        if !self.result || self.snapshot.is_some() || self.background_response {
            return Err(Error::Identity);
        }
        self.steers.begin_cycle(payload)?;
        self.idle = false;
        Ok(true)
    }
    /// Whether this result belongs to the prompt or to a steer of this turn.
    /// A turn without steers, and the accounting query, keep the exact rule.
    pub(super) fn correlate(&mut self, payload: &Value, prompt: &str) -> bool {
        if self.snapshot.is_some() || self.steers.is_empty() {
            return correlated(payload, prompt);
        }
        self.steers.result(payload, prompt)
    }
    /// The result just observed closed a steer cycle: its text continues the
    /// turn's answer. Every other result replaces the answer.
    pub(super) fn continues_answer(&self) -> bool {
        self.snapshot.is_none() && self.steers.continues()
    }
    pub(super) fn background_result(&self, payload: &Value) -> bool {
        self.background_response
            && self.snapshot.is_none()
            && payload["origin"]["kind"] == "task-notification"
            && payload["origin"]["producer"] == "session-task"
            && payload["origin"]["runId"]
                .as_str()
                .is_some_and(|id| self.notified_runs.contains(id))
            && payload.get("user_message_uuid").is_none_or(|v| v.is_null())
            && payload
                .get("user_message_uuids")
                .is_none_or(|v| v.is_null())
    }
    pub(super) fn observe(&mut self, kind: EventKind, payload: &Value) -> Result<(), Error> {
        if !self.enabled {
            self.result |= kind == EventKind::Result;
            return Ok(());
        }
        match kind {
            EventKind::System => match payload["subtype"].as_str() {
                Some("session_state_changed") => {
                    // Only native idle can complete a turn. Every other state,
                    // including one this host does not know, is not idle: the
                    // turn keeps waiting for a later idle instead of ending.
                    self.idle = payload["state"] == "idle";
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
                        if task["ambient"] != true {
                            self.remember_run(task)?;
                            self.mark_background(task);
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
                    if payload["subtype"] == "task_started" {
                        self.remember_run(payload)?;
                        if payload["is_backgrounded"] == true {
                            self.mark_background(payload);
                        }
                    } else if payload["subtype"] == "task_updated" {
                        // A foreground command can be moved to the background.
                        if payload["patch"]["is_backgrounded"] == true {
                            self.mark_background(payload);
                        }
                    } else if payload["subtype"] == "task_notification"
                        && let Some(run) = payload.get("run_id")
                    {
                        let run = run.as_str().ok_or(Error::Identity)?;
                        if self.task_runs.get(run).map(String::as_str)
                            != payload["task_id"].as_str()
                            || !matches!(
                                payload["status"].as_str(),
                                Some("completed" | "failed" | "stopped")
                            )
                        {
                            return Err(Error::Identity);
                        }
                        if self.consumed_runs.contains(run) {
                            return Ok(());
                        }
                        self.notified_runs.insert(run.into());
                    }
                    self.saw_child_work = true;
                    if self.result {
                        self.changed_after_result = true;
                    }
                }
                _ => {}
            },
            EventKind::Result => {
                if self.background_response {
                    if !self.background_result(payload) {
                        return Err(Error::Identity);
                    }
                    let run = payload["origin"]["runId"].as_str().ok_or(Error::Identity)?;
                    self.notified_runs.remove(run);
                    self.consumed_runs.insert(run.into());
                    self.background_response = false;
                } else if !payload["origin"].is_null() {
                    // An autonomous result must be tied to the original task's
                    // observed start, completion notification and native init.
                    return Err(Error::Identity);
                }
                if let Some(id) = &self.snapshot
                    && !correlated(payload, id)
                {
                    return Err(Error::Identity);
                }
                if self.snapshot.is_none() {
                    use sha2::{Digest, Sha256};
                    // Exactly `join_answer`: the non-empty texts of the cycles
                    // that continue one answer, separated by a blank line.
                    if !self.steers.continues() {
                        self.answer_text = Sha256::new();
                        self.answer_joined = false;
                    }
                    let text = payload["result"].as_str().unwrap_or("");
                    if !text.is_empty() {
                        if self.answer_joined {
                            self.answer_text.update(b"\n\n");
                        }
                        self.answer_text.update(text.as_bytes());
                        self.answer_joined = true;
                    }
                    self.answer = Some((
                        self.answer_text.clone().finalize().into(),
                        payload["is_error"] == true,
                    ));
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
        self.report_open();
        Ok(())
    }
    pub(super) fn complete(&self) -> bool {
        self.enabled
            && self.result
            && self.idle
            && self.background.is_empty()
            && !self.background_response
            // Claude Code reports a foreground command that runs for a few
            // seconds as a task too, and its end as a notification. Its
            // output went back as the tool result, so no notification turn
            // follows and the turn must not wait for one.
            && !self
                .notified_runs
                .iter()
                .any(|run| self.background_runs.contains(run))
            && self.steers.settled()
    }
    /// `request` is the stop that was in flight or accepted when this result
    /// arrived. An answer read before any such stop keeps no stop identity,
    /// and an accounting snapshot never replaces the answer's own record.
    pub(super) fn observe_result_stop(&mut self, request: Option<&str>) {
        if self.snapshot.is_none() {
            self.answer_stop = request.map(str::to_owned);
        }
        if let Some(request) = request {
            self.stop_result = Some(request.into());
        }
    }
    pub(super) fn accepted_stop(&mut self, request: &str) {
        // An acknowledged interrupt's terminal result ends pending native
        // notification turns as well; the live child set must still drain.
        if self.stop_result.as_deref() == Some(request) {
            self.notified_runs.clear();
        }
    }
    pub(super) fn finished(&self) -> bool {
        if self.enabled {
            self.complete()
        } else {
            self.result
        }
    }
    pub(super) fn snapshot_requested(&self) -> bool {
        self.snapshot.is_some()
    }
}

impl<R, W, E> SessionDriver<R, W, E> {
    pub fn completion_receipt(&self) -> Result<super::CompletionReceipt, Error> {
        if self.phase != Phase::ResultObserved || !self.completion.complete() {
            return Err(Error::State);
        }
        let (digest, is_error) = self.completion.answer.ok_or(Error::State)?;
        Ok(self.observations.completed(
            self.session_id.as_deref().ok_or(Error::State)?,
            digest,
            is_error,
            self.answer_interrupted(),
        ))
    }
    /// An acknowledged stop interrupts the answer only when that answer
    /// arrived after the stop was started. A stop that the native peer accepts
    /// after the final answer was read ends nothing the answer depends on.
    pub fn answer_interrupted(&self) -> bool {
        self.interrupt_receipt().is_some_and(|receipt| {
            receipt.accepted()
                && self.completion.answer_stop.as_deref() == Some(receipt.request_id())
        })
    }
    /// The last original answer of a stream that has since failed or closed.
    /// Unlike `completion_receipt` this proves neither native idle nor that
    /// descendants finished, so it carries no usage authority: a host may
    /// deliver the text and must leave the call's usage unknown. A stop that
    /// was in flight or accepted when the answer arrived, acknowledged or
    /// not, is reported as an interruption.
    pub fn closed_answer_receipt(&self) -> Result<super::CompletionReceipt, Error> {
        if self.phase != Phase::Closed || !self.prompt_started {
            return Err(Error::State);
        }
        let (digest, is_error) = self.completion.answer.ok_or(Error::State)?;
        Ok(self.observations.completed(
            self.session_id.as_deref().ok_or(Error::State)?,
            digest,
            is_error,
            self.completion
                .answer_stop
                .as_deref()
                .is_some_and(|id| self.interrupt.unrefused(id)),
        ))
    }
    /// Use only while the host continuously supervises the admitted turn and
    /// owns process cleanup. Native reasoning/tool silence has no wall-clock
    /// ceiling; protocol frames, writes and individual callbacks remain bounded.
    /// This must precede any callback and cannot renew an existing grant.
    pub fn supervise_turn(&mut self) -> Result<(), Error> {
        if self.phase != Phase::Running
            || self.session_id.is_none()
            || self.awaiting_init
            || !self.control.untouched()
            || self
                .room_mcp
                .as_ref()
                .is_some_and(|state| !state.untouched())
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
            && self.interrupt.quiescent()
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
        self.interrupt = Default::default();
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
                        // A hook that ended after the turn did.
                        || (kind == EventKind::System && super::hook_report(&payload))
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
