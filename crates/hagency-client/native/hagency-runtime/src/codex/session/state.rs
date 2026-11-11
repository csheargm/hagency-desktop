use super::{
    Error, ItemPhase, MAX_ITEMS, MAX_TEXT_BYTES, Outcome, Phase, Settings, Update, id,
    object, string,
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

struct Item {
    kind: String,
    phase: ItemPhase,
    text: String,
    message_phase: Option<String>,
}
pub(super) struct State {
    pub phase: Phase,
    pub thread: Option<String>,
    pub turn: Option<String>,
    pub outcome: Option<Outcome>,
    pub interrupt_sent: bool,
    items: BTreeMap<String, Item>,
    final_ids: Vec<String>,
    text_bytes: usize,
    events: usize,
    pub max_items: usize,
    /// The turn was ended by an `error` notice that will not retry, and the
    /// failed `turn/completed` the app server sends right after it has not been
    /// read yet. Consumed by that one notice and nothing else.
    failed_echo_owed: bool,
    /// The provider's own reason for ending the turn, bounded and printable
    /// (board #110). The real app server sends it in `error.params.error.message`
    /// and repeats it in `turn/completed`'s `turn.error.message`; TS surfaces it
    /// (`router/src/runner.ts:793` rejects with the whole `params`). Native used
    /// to length-check it and throw it away, so a usage-limit refusal reached the
    /// operator as a bare `protocol` "Result uncertain" with no reason at all.
    turn_failure: Option<String>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            phase: Phase::New,
            thread: None,
            turn: None,
            outcome: None,
            interrupt_sent: false,
            items: BTreeMap::new(),
            final_ids: Vec::new(),
            text_bytes: 0,
            events: 0,
            max_items: MAX_ITEMS,
            failed_echo_owed: false,
            turn_failure: None,
        }
    }
}
impl State {
    pub fn item_count(&self) -> usize {
        self.items.len()
    }
    /// The provider's own turn-failure reason, already length-checked at
    /// admission. Diagnostic only: it never decides an outcome.
    pub fn turn_failure(&self) -> Option<&str> {
        self.turn_failure.as_deref()
    }
    pub fn text_bytes(&self) -> usize {
        self.text_bytes
    }
    pub fn event_count(&self) -> usize {
        self.events
    }
    pub fn failed(&mut self, error: Error) {
        self.phase = Phase::Ended;
        self.outcome = Some(match error {
            Error::UnsupportedRequest => Outcome::UnsupportedRequest,
            Error::Rejected(_) => Outcome::Failed,
            _ => Outcome::Unknown { reason: error },
        });
    }
    pub fn observe_thread(
        &mut self,
        result: &Value,
        settings: &Settings,
        expected: Option<&str>,
    ) -> Result<String, Error> {
        let thread = object(result, "thread")?;
        let observed = id(thread, "id")?;
        if expected.is_some_and(|id| id != observed) {
            return Err(Error::Scope);
        }
        if string(result, "cwd")? != settings.cwd
            || string(thread, "cwd")? != settings.cwd
            || string(result, "model")? != settings.model
        {
            return Err(Error::Scope);
        }
        if string(result, "approvalPolicy")? != settings.approval_policy()
            || string(result, "approvalsReviewer")? != "user"
        {
            return Err(Error::Policy);
        }
        let sandbox = object(result, "sandbox")?;
        let unrestricted = settings.is_unrestricted();
        let kind = if settings.read_only {
            "readOnly"
        } else if unrestricted {
            "dangerFullAccess"
        } else {
            "workspaceWrite"
        };
        if string(sandbox, "type")? != kind
            || sandbox
                .get("networkAccess")
                .is_some_and(|v| v.as_bool() != Some(unrestricted))
        {
            return Err(Error::Policy);
        }
        if let Some(roots) = sandbox.get("writableRoots") {
            let roots = roots.as_array().ok_or(Error::Policy)?;
            if ((settings.read_only || unrestricted) && !roots.is_empty())
                || roots
                    .iter()
                    .any(|root| root.as_str() != Some(settings.cwd.as_str()))
            {
                return Err(Error::Policy);
            }
        }
        if string(object(thread, "status")?, "type")? != "idle" {
            return Err(Error::State);
        }
        if !thread
            .get("turns")
            .and_then(Value::as_array)
            .ok_or(Error::Malformed)?
            .is_empty()
        {
            return Err(Error::Scope);
        }
        self.thread = Some(observed.into());
        self.phase = Phase::ThreadReady;
        Ok(observed.into())
    }
    pub fn observe_turn(&mut self, result: &Value) -> Result<String, Error> {
        let turn = object(result, "turn")?;
        let observed = id(turn, "id")?;
        if !matches!(
            string(turn, "status")?,
            "inProgress" | "completed" | "failed" | "interrupted"
        ) {
            return Err(Error::Malformed);
        }
        if !turn
            .get("items")
            .and_then(Value::as_array)
            .ok_or(Error::Malformed)?
            .is_empty()
        {
            return Err(Error::Malformed);
        }
        // Even a terminal status in the start response is not a completion event.
        self.turn = Some(observed.into());
        self.phase = Phase::Running;
        Ok(observed.into())
    }

    pub fn notification(&mut self, method: &str, params: &Value) -> Result<Update, Error> {
        let result = self.notification_inner(method, params);
        if let Err(error) = &result {
            // The method name and phase only, so a refused event can be traced
            // to its cause; never params or content.
            tracing::warn!(method, phase = ?self.phase, ?error, "Codex notification refused by the session state");
        }
        result
    }

    fn notification_inner(&mut self, method: &str, params: &Value) -> Result<Update, Error> {
        if self.phase == Phase::Ended {
            return Err(Error::Scope);
        }
        self.events = self
            .events
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        scope(method, params, self.thread.as_deref(), self.turn.as_deref())?;
        match method {
            "warning"
            | "configWarning"
            | "remoteControl/status/changed"
            | "mcpServer/startupStatus/updated"
            | "account/rateLimits/updated"
            | "account/updated"
            | "hook/started"
            | "hook/completed" => Ok(Update::Notice),
            "thread/started" => Ok(Update::ThreadStatus),
            "thread/status/changed" => {
                // `systemError` is a defined 0.154.0 status, observed live when
                // the provider refuses a turn (usage limit). It carries no
                // cause and decides nothing: the `error` notice or failed
                // `turn/completed` that follows it ends the turn. Outside a
                // running turn it has no such successor and stays refused, as
                // does `notLoaded`, which no qualified flow has produced.
                match string(object(params, "status")?, "type")? {
                    "idle" | "active" => {}
                    "systemError" if self.phase == Phase::Running => {}
                    _ => return Err(Error::UnsupportedEvent),
                }
                Ok(Update::ThreadStatus)
            }
            "serverRequest/resolved" => Ok(Update::Progress),
            "turn/started" => {
                if self.phase != Phase::Running
                    || string(object(params, "turn")?, "status")? != "inProgress"
                {
                    return Err(Error::State);
                }
                Ok(Update::TurnStarted)
            }
            "turn/completed" => self.complete(object(params, "turn")?),
            "item/started" => self.item(params, false),
            "item/completed" => self.item(params, true),
            "item/agentMessage/delta" => self.delta(params),
            "error" => {
                if self.phase != Phase::Running {
                    return Err(Error::State);
                }
                let message = string(object(params, "error")?, "message")?;
                if message.len() > 4096 {
                    return Err(Error::Capacity);
                }
                match params.get("willRetry").and_then(Value::as_bool) {
                    Some(true) => Ok(Update::Retrying),
                    Some(false) => {
                        self.phase = Phase::Ended;
                        self.outcome = Some(Outcome::Failed);
                        // Retain the provider's own words (board #110).
                        self.turn_failure = Some(message.to_owned());
                        self.failed_echo_owed = true;
                        Ok(Update::TurnEnded)
                    }
                    None => Err(Error::Malformed),
                }
            }
            "turn/diff/updated"
            | "turn/plan/updated"
            | "thread/tokenUsage/updated"
            | "thread/compacted"
            | "turn/moderationMetadata" => {
                if self.phase != Phase::Running {
                    return Err(Error::State);
                }
                Ok(Update::Progress)
            }
            "item/plan/delta"
            | "item/commandExecution/outputDelta"
            | "item/commandExecution/terminalInteraction"
            | "item/fileChange/outputDelta"
            | "item/mcpToolCall/progress"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/summaryPartAdded"
            | "item/reasoning/textDelta" => {
                let item_id = id(params, "itemId")?;
                let kind = method.split('/').nth(1).ok_or(Error::Malformed)?;
                // A unified-exec terminal outlives the command item that
                // opened it: Codex keeps reporting its output and stdin
                // interaction after that item completed. That is progress for
                // an earlier command, never permission or completion, so it is
                // ignored instead of ending the run as a scope violation.
                let late_terminal = matches!(
                    method,
                    "item/commandExecution/outputDelta" | "item/commandExecution/terminalInteraction"
                );
                let Some(item) = self.items.get(item_id) else {
                    if late_terminal {
                        tracing::debug!(method, "Codex terminal progress for an unknown command item ignored");
                        return Ok(Update::Progress);
                    }
                    return Err(Error::Scope);
                };
                if item.kind != kind {
                    return Err(Error::Scope);
                }
                if item.phase != ItemPhase::Active {
                    if late_terminal {
                        return Ok(Update::Progress);
                    }
                    return Err(Error::Scope);
                }
                if method == "item/commandExecution/terminalInteraction" {
                    // Pinned terminal polling/input is progress for this exact
                    // active command. It grants no permission or completion;
                    // stdin stays private and is not retained in observations.
                    id(params, "processId")?;
                    if string(params, "stdin")?.len() > MAX_TEXT_BYTES {
                        return Err(Error::Capacity);
                    }
                }
                Ok(Update::Progress)
            }
            // TS parity (`router/src/runner.ts:738-793`): the retained runner's
            // line handler is a flat chain of `if (message.method === …)`
            // checks with no catch-all, so a notification it does not match
            // falls through and is ignored. Codex adds methods across versions
            // (account/updated, board #89); refusing an unknown METHOD is what
            // killed live dispatches. Log the name and ignore it. A KNOWN method
            // with an unrecognized value keeps its strict arm above.
            method => {
                tracing::debug!(method = method, "ignoring unrecognized Codex notification");
                Ok(Update::Notice)
            }
        }
    }

    pub fn terminal_suffix(&mut self, method: &str, params: &Value) -> Result<(), Error> {
        self.events = self
            .events
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        scope(method, params, self.thread.as_deref(), self.turn.as_deref())?;
        match method {
            "warning"
            | "configWarning"
            | "remoteControl/status/changed"
            | "mcpServer/startupStatus/updated"
            | "account/rateLimits/updated"
            | "account/updated"
            | "serverRequest/resolved"
            | "hook/completed" => Ok(()),
            "thread/status/changed" if string(object(params, "status")?, "type")? == "idle" => {
                Ok(())
            }
            // A provider refusal ends a turn twice on the wire: `error` with
            // `willRetry: false`, then `turn/completed` with status `failed`, in
            // the same instant (captured 0.154.0 order, ADR036). The first ended
            // this turn as Failed; the second reaches this drain. Live 2026-09-20
            // it was refused here, so a usage-limit refusal was reported as a
            // `scope` violation instead of a failed turn. Exactly that one echo,
            // already checked above to be this thread and turn, is the ending
            // restated and decides nothing. A turn that ended any other way, a
            // second echo, or a status other than failed stays refused.
            "turn/completed"
                if self.failed_echo_owed
                    && string(object(params, "turn")?, "status")? == "failed" =>
            {
                self.failed_echo_owed = false;
                Ok(())
            }
            // A KNOWN lifecycle notice that contradicts the ending is still a
            // refusal in the drain, whatever #94 tolerates: this is not an
            // unknown method.
            "thread/status/changed" | "turn/completed" => Err(Error::Scope),
            // Any method this build does not model at all: log and carry on
            // (board #94, the shutdown phase).
            other => {
                tracing::debug!(method = other, "ignoring unrecognized Codex notification");
                Ok(())
            }
        }
    }

    fn item(&mut self, params: &Value, complete: bool) -> Result<Update, Error> {
        if self.phase != Phase::Running {
            return Err(Error::State);
        }
        let timestamp = if complete {
            "completedAtMs"
        } else {
            "startedAtMs"
        };
        if params
            .get(timestamp)
            .and_then(Value::as_i64)
            .is_none_or(|n| n < 0)
        {
            return Err(Error::Malformed);
        }
        let value = object(params, "item")?;
        let item_id = id(value, "id")?;
        let kind = string(value, "type")?;
        if passive_item(kind) {
            // Defined by the app-server schema but not modelled here: a hook's
            // prompt, the output of a function call whose call is its own
            // item, and the notice that a sub-agent started (its work arrives
            // under its own thread). None carries answer text or runs a tool,
            // so each is left untracked instead of failing the turn.
            tracing::debug!(kind = kind, "ignoring passive Codex item");
            return Ok(Update::Notice);
        }
        if !modelled_item(kind) {
            return Err(Error::UnsupportedEvent);
        }
        if !complete && (self.items.contains_key(item_id) || self.items.len() >= self.max_items) {
            return Err(if self.items.contains_key(item_id) {
                Error::Scope
            } else {
                Error::Capacity
            });
        }
        let existing = self.items.get(item_id);
        if complete
            && existing.is_none_or(|item| item.phase != ItemPhase::Active || item.kind != kind)
        {
            return Err(Error::Scope);
        }
        let (text, message_phase) = if kind == "agentMessage" {
            let text = string(value, "text")?.to_owned();
            let phase = match value.get("phase") {
                None | Some(Value::Null) => None,
                Some(Value::String(phase))
                    if matches!(phase.as_str(), "commentary" | "final_answer") =>
                {
                    Some(phase.clone())
                }
                _ => return Err(Error::Malformed),
            };
            if let Some(previous) = existing {
                if previous.message_phase.is_some() && previous.message_phase != phase {
                    return Err(Error::Scope);
                }
                if !text.starts_with(&previous.text) {
                    return Err(Error::Malformed);
                }
            }
            (text, phase)
        } else {
            (String::new(), None)
        };
        let old = existing.map_or(0, |item| item.text.len());
        // Some native providers complete an assistant item without streaming
        // output_text deltas, or include a final suffix only in item/completed.
        // Forward exactly the unseen suffix; the prefix was checked above.
        let unseen = (kind == "agentMessage" && text.len() > old)
            .then(|| text[old..].to_owned());
        self.text_bytes = self
            .text_bytes
            .checked_sub(old)
            .and_then(|n| n.checked_add(text.len()))
            .filter(|&n| n <= MAX_TEXT_BYTES)
            .ok_or(Error::Capacity)?;
        if complete && kind == "agentMessage" && message_phase.as_deref() != Some("commentary") {
            self.final_ids.push(item_id.into());
        }
        let phase = if complete {
            ItemPhase::Complete
        } else {
            ItemPhase::Active
        };
        self.items.insert(
            item_id.into(),
            Item {
                kind: kind.into(),
                phase,
                text,
                message_phase,
            },
        );
        Ok(match unseen {
            Some(delta) => Update::TextDelta { item_id: item_id.into(), delta },
            None => Update::Item { id: item_id.into(), kind: kind.into(), phase },
        })
    }

    fn delta(&mut self, params: &Value) -> Result<Update, Error> {
        let item_id = id(params, "itemId")?;
        let delta = string(params, "delta")?;
        let item = self.items.get_mut(item_id).ok_or(Error::Scope)?;
        if self.phase != Phase::Running
            || item.kind != "agentMessage"
            || item.phase != ItemPhase::Active
        {
            return Err(Error::Scope);
        }
        let total = self
            .text_bytes
            .checked_add(delta.len())
            .filter(|&n| n <= MAX_TEXT_BYTES)
            .ok_or(Error::Capacity)?;
        item.text.push_str(delta);
        self.text_bytes = total;
        Ok(Update::TextDelta {
            item_id: item_id.into(),
            delta: delta.into(),
        })
    }

    fn complete(&mut self, turn: &Value) -> Result<Update, Error> {
        if self.phase != Phase::Running {
            return Err(Error::State);
        }
        let items = turn
            .get("items")
            .and_then(Value::as_array)
            .ok_or(Error::Malformed)?;
        if items.len() > MAX_ITEMS {
            tracing::warn!(items = items.len(), limit = MAX_ITEMS, "Codex turn exceeds the item limit");
            return Err(Error::Capacity);
        }
        let mut seen = BTreeSet::new();
        for value in items {
            let observed = id(value, "id")?;
            if !seen.insert(observed) {
                return Err(Error::Scope);
            }
            let kind = string(value, "type")?;
            let Some(item) = self.items.get(observed) else {
                // A passive item was never tracked. Any other item the stream
                // did not announce contradicts the completion.
                if passive_item(kind) {
                    continue;
                }
                return Err(Error::Scope);
            };
            if kind != item.kind {
                return Err(Error::Scope);
            }
            if item.kind == "agentMessage" && string(value, "text")? != item.text {
                return Err(Error::Malformed);
            }
        }
        self.outcome = Some(match string(turn, "status")? {
            "completed" => {
                if turn.get("error").is_some_and(|v| !v.is_null()) {
                    return Err(Error::Malformed);
                }
                // A unified-exec command can outlive its turn, for example a dev
                // server the model started to preview a site: Codex completes
                // the turn without completing that item. It cannot change the
                // answer, and the runner's process tree is stopped after the
                // run. Any other item still active contradicts the completion.
                let running: Vec<&str> = self
                    .items
                    .values()
                    .filter(|item| item.phase == ItemPhase::Active)
                    .map(|item| item.kind.as_str())
                    .collect();
                if running.iter().any(|kind| *kind != "commandExecution") {
                    return Err(Error::State);
                }
                if !running.is_empty() {
                    tracing::info!(running_commands = running.len(), "Codex turn completed with commands still running");
                }
                let mut text = String::new();
                for id in &self.final_ids {
                    let item = self.items.get(id).ok_or(Error::Scope)?;
                    if !text.is_empty() {
                        text.push_str("\n\n");
                    }
                    text.push_str(&item.text);
                    if text.len() > MAX_TEXT_BYTES {
                        return Err(Error::Capacity);
                    }
                }
                Outcome::Completed { text }
            }
            "failed" => {
                let message = string(object(turn, "error")?, "message")?;
                if message.len() > 4096 {
                    return Err(Error::Capacity);
                }
                // `turn/completed` restates the reason the `error` notice just
                // gave (board #110); keep whichever arrived. The echo arm above
                // already consumed the duplicate, so this is the arm that runs
                // when the completion is the FIRST frame to carry it.
                if self.turn_failure.is_none() {
                    self.turn_failure = Some(message.to_owned());
                }
                Outcome::Failed
            }
            "interrupted" => Outcome::Interrupted,
            _ => return Err(Error::Malformed),
        });
        self.phase = Phase::Ended;
        Ok(Update::TurnEnded)
    }
}

/// Item types of the app-server schema that the turn state leaves untracked.
fn passive_item(kind: &str) -> bool {
    matches!(kind, "hookPrompt" | "functionCallOutput" | "subAgentActivity")
}

/// The item types the turn state tracks. A type that is neither tracked nor
/// passive is refused: a known method with an unknown value stays strict.
fn modelled_item(kind: &str) -> bool {
    matches!(
        kind,
        "userMessage"
            | "agentMessage"
            | "plan"
            | "reasoning"
            | "commandExecution"
            | "fileChange"
            | "mcpToolCall"
            | "dynamicToolCall"
            | "collabAgentToolCall"
            | "webSearch"
            | "imageView"
            | "imageGeneration"
            | "enteredReviewMode"
            | "exitedReviewMode"
            | "contextCompaction"
            | "sleep"
    )
}

/// Optional expected identities are absent only during the corresponding RPC;
/// no notification is acted upon until both required identities are known.
pub(super) fn scope(
    method: &str,
    params: &Value,
    thread: Option<&str>,
    turn: Option<&str>,
) -> Result<(), Error> {
    if !params.is_object() {
        return Err(Error::Malformed);
    }
    if matches!(method, "hook/started" | "hook/completed") {
        let observed = id(params, "threadId")?;
        if thread.is_some_and(|expected| expected != observed) {
            return Err(Error::Scope);
        }
        if params.get("turnId").is_some_and(|value| !value.is_null())
            && turn.is_some_and(|expected| params["turnId"].as_str() != Some(expected))
        {
            return Err(Error::Scope);
        }
        if params.get("turnId").is_some_and(|value| !value.is_null()) {
            id(params, "turnId")?;
        }
        return super::hooks::validate(method, params);
    }
    if method == "mcpServer/startupStatus/updated" {
        // Startup is app/thread-scoped, not turn-scoped. Readiness is only a
        // diagnostic; it never substitutes for canonical tool evidence.
        let name = string(params, "name")?;
        if name.is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
            return Err(Error::Malformed);
        }
        let status = string(params, "status")?;
        if !matches!(status, "starting" | "ready" | "failed" | "cancelled")
            || params.get("error").is_some_and(|value| {
                !value.is_null() && value.as_str().is_none_or(|value| value.len() > 4096)
            })
            || params.get("failureReason").is_some_and(|value| {
                !value.is_null() && value.as_str() != Some("reauthenticationRequired")
            })
        {
            return Err(Error::Malformed);
        }
        if params.get("threadId").is_some_and(|value| !value.is_null()) {
            let observed = id(params, "threadId")?;
            if thread.is_some_and(|expected| expected != observed) {
                return Err(Error::Scope);
            }
        }
        if params.get("turnId").is_some_and(|value| !value.is_null())
            && turn != Some(id(params, "turnId")?)
        {
            return Err(Error::Scope);
        }
        if name == "hagency_task_writer" && matches!(status, "failed" | "cancelled") {
            return Err(Error::TaskWriterStartup);
        }
        return Ok(());
    }
    if matches!(
        method,
        "warning"
            | "configWarning"
            | "remoteControl/status/changed"
            | "account/rateLimits/updated"
            | "account/updated"
    ) {
        if method == "account/updated" {
            // The app-server's global account/auth notice (captured board #89).
            // It carries no threadId/turnId and grants no scope.
            for key in ["authMode", "planType"] {
                if let Some(value) = params.get(key).filter(|value| !value.is_null()) {
                    let value = value.as_str().ok_or(Error::Malformed)?;
                    if value.len() > 64 || value.chars().any(char::is_control) {
                        return Err(Error::Malformed);
                    }
                }
            }
        }
        if method == "account/rateLimits/updated" {
            // Account rolling-window metadata is not per-turn usage evidence.
            object(params, "rateLimits")?;
        }
        if method == "remoteControl/status/changed" {
            for key in ["installationId", "serverName"] {
                let value = string(params, key)?;
                if value.len() > 256 || value.chars().any(char::is_control) {
                    return Err(Error::Malformed);
                }
            }
            if !matches!(
                string(params, "status")?,
                "disabled" | "connecting" | "connected" | "errored"
            ) {
                return Err(Error::Malformed);
            }
            if params.get("environmentId").is_some_and(|value| {
                !value.is_null()
                    && value.as_str().is_none_or(|value| {
                        value.len() > 256 || value.chars().any(char::is_control)
                    })
            }) {
                return Err(Error::Malformed);
            }
        }
        for (key, expected) in [("threadId", thread), ("turnId", turn)] {
            if let Some(value) = params.get(key).filter(|v| !v.is_null()) {
                let observed = value.as_str().ok_or(Error::Malformed)?;
                if expected != Some(observed) {
                    return Err(Error::Scope);
                }
            }
        }
        return Ok(());
    }
    // Thread binding. Enforced whenever the notice DECLARES a thread and this
    // session holds one: a declared foreign thread is always refused. A notice
    // that declares no thread is global — there is nothing to bind, and Codex
    // adds such notices across versions; the retained runner ignores them. This
    // is what lets an unknown method be tolerated by name instead of killed
    // (boards #89/#94), without relaxing the check for a substituted thread.
    let observed_thread = if method == "thread/started" {
        Some(id(object(params, "thread")?, "id")?)
    } else {
        params
            .get("threadId")
            .filter(|value| !value.is_null())
            .map(|value| value.as_str().ok_or(Error::Malformed))
            .transpose()?
    };
    if let (Some(expected), Some(observed)) = (thread, observed_thread)
        && expected != observed
    {
        return Err(Error::Scope);
    }
    if matches!(
        method,
        "thread/started" | "thread/status/changed" | "serverRequest/resolved"
    ) {
        return Ok(());
    }
    // Turn binding. Same rule: enforced whenever the notice declares a turn —
    // by top-level `turnId`, or by the nested `turn` object that the turn
    // lifecycle notices carry.
    let observed_turn = if matches!(method, "turn/started" | "turn/completed") {
        Some(id(object(params, "turn")?, "id")?)
    } else {
        params
            .get("turnId")
            .filter(|value| !value.is_null())
            .map(|value| value.as_str().ok_or(Error::Malformed))
            .transpose()?
    };
    if let (Some(expected), Some(observed)) = (turn, observed_turn)
        && expected != observed
    {
        return Err(Error::Scope);
    }
    Ok(())
}
