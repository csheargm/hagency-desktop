//! Typed upstream observations for one host-owned turn, never domain authority.
mod driver;
mod hooks;
mod observation;
mod qualification;
mod rate_limits;
mod room_history;
mod room_files;
pub use room_files::{RoomFileCall, RoomFileOperation, ROOM_FILE_TOOLS, ROOM_FILE_RESULT_BYTES};
pub use room_history::{RoomHistoryCall, ROOM_HISTORY_TOOL, ROOM_HISTORY_RESULT_BYTES};
mod user_input;
pub use user_input::{
    MAX_USER_INPUT_OPTIONS, MAX_USER_INPUT_QUESTIONS, MAX_USER_INPUT_TEXT_BYTES,
    OTHER_ANSWER_LABEL, USER_INPUT_METHOD, UserInputAnswer, UserInputOption, UserInputQuestion,
    UserInputRequest, UserInputResponse,
};
pub use rate_limits::{quota_account_digest, RateLimitBucket, RateLimitOrigin, RateLimitSnapshot, RateLimitWindow};
pub use qualification::QualificationRead;
mod state;
mod task_mcp;
mod usage;
pub use driver::{
    ApprovalControlPolicy, ControlUpdate, PreparedApproval, PreparedUpdate, SessionDriver,
};
pub use observation::{
    NativeTurnContext, Observation, ObservationKind, ObservationSource, ToolEvidence, ToolKind, ToolResult, TurnResult,
};
pub use task_mcp::{TASK_MCP_ENV, TASK_MCP_TOOLS, TaskMcp};
pub use usage::{ContextUsage, UsageBreakdown, UsageDiagnostics, UsageEvidence};

use super::transport;
use serde_json::{Value, json};
use std::path::{Component, PathBuf};

pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// Items in one turn, as listed by `turn/completed`. Every command, web search,
/// reasoning step and message is an item; research turns exceeded 128.
pub const MAX_ITEMS: usize = 2048;
// Streaming deltas count as events; a valid bounded research answer can exceed
// 4096 notifications. Text, item, queue and lifetime bounds remain independent.
pub const MAX_EVENTS: usize = 65_536;
pub const MAX_DEFERRED: usize = 16;
pub const MAX_DEFERRED_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid native Codex session settings")]
    Settings,
    #[error("invalid native Codex session state")]
    State,
    #[error("native Codex session identity mismatch")]
    Scope,
    #[error("invalid native Codex lifecycle observation")]
    Malformed,
    #[error("native Codex session capacity exceeded")]
    Capacity,
    #[error("native Codex reported a different execution policy")]
    Policy,
    #[error("native Codex RPC was rejected with code {0}")]
    Rejected(i64),
    #[error("native Codex server request has no authority adapter")]
    UnsupportedRequest,
    #[error("native Codex notification is unsupported")]
    UnsupportedEvent,
    #[error("native Codex canonical task writer startup failed")]
    TaskWriterStartup,
    #[error("native Codex session operation was cancelled")]
    Cancelled,
    #[error("native Codex session transport failed: {0}")]
    Transport(transport::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    New,
    Initializing,
    Ready,
    OpeningThread,
    ThreadReady,
    StartingTurn,
    Running,
    Ended,
}

/// Upstream observations only. None establish canonical completion or cleanup.
pub enum Outcome {
    Completed { text: String },
    Failed,
    Interrupted,
    UnsupportedRequest,
    Unknown { reason: Error },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptDisposition {
    Acknowledged,
    Rejected { code: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemPhase {
    Active,
    Complete,
}

pub enum Update {
    RoomHistory(RoomHistoryCall),
    RoomFile(RoomFileCall),
    /// The model asks its user (Codex `request_user_input`). Only a host that
    /// opted in sees this; it must answer exactly once, possibly with nothing.
    UserInput(UserInputRequest),
    /// Codex withdrew a question this host has not answered yet.
    UserInputResolved {
        id: super::RequestId,
    },
    Approval(super::approval::ApprovalRequest),
    /// Upstream callback ended, possibly by cancellation and before applying.
    ApprovalResolved {
        id: super::RequestId,
    },
    Notice,
    TurnStarted,
    ThreadStatus,
    Item {
        id: String,
        kind: String,
        phase: ItemPhase,
    },
    TextDelta {
        item_id: String,
        delta: String,
    },
    Progress,
    Retrying,
    TurnEnded,
}

/// A host-recorded upstream thread, not a Matrix/Hagency session credential.
pub struct ResumeThreadId(String);
impl ResumeThreadId {
    pub fn new(id: String) -> Result<Self, Error> {
        valid_id(&id)?;
        Ok(Self(id))
    }
}

/// Host-only settings. No Deserialize, arbitrary configuration map, network
/// enablement, approval-policy override or YOLO switch exists here.
pub struct Settings {
    cwd: String,
    model: String,
    effort: String,
    read_only: bool,
    unrestricted: bool,
    task_mcp: Option<TaskMcp>,
    room_history: bool,
    room_files: bool,
    user_input_tool: bool,
}
impl Settings {
    pub(crate) fn room_history_enabled(&self) -> bool { self.room_history }
    pub fn new(cwd: PathBuf, model: String, effort: String) -> Result<Self, Error> {
        if !cwd.is_absolute()
            || cwd
                .components()
                .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        {
            return Err(Error::Settings);
        }
        let cwd = cwd
            .to_str()
            .filter(|v| super::text(v, 4096))
            .ok_or(Error::Settings)?
            .to_owned();
        if !super::text(&model, 128)
            || model.trim() != model
            || !super::text(&effort, 32)
            || effort.trim() != effort
        {
            return Err(Error::Settings);
        }
        Ok(Self {
            cwd,
            model,
            effort,
            read_only: false,
            unrestricted: false,
            task_mcp: None,
            room_history: false,
            room_files: false,
            user_input_tool: false,
        })
    }
    pub(crate) fn shared_mode_matches(&self, mode: Option<&str>) -> bool {
        match mode {None=>self.read_only,Some("standard")=>!self.read_only && !self.unrestricted,Some("yolo")=>!self.read_only && self.unrestricted,_=>false}
    }
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
    /// Apply an owner-selected next-dispatch policy. Read-only dispatches stay
    /// read-only regardless of this setting; the host owns dispatch authority.
    pub fn with_execution_policy(mut self, yolo: bool) -> Self {
        self.unrestricted = yolo;
        self
    }
    pub fn is_unrestricted(&self) -> bool {
        self.unrestricted && !self.read_only
    }
    fn approval_policy(&self) -> &'static str {
        if self.is_unrestricted() {
            "never"
        } else {
            "on-request"
        }
    }
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }
    pub fn cwd(&self) -> &str {
        &self.cwd
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn effort(&self) -> &str {
        &self.effort
    }
    fn mode(&self) -> &'static str {
        if self.read_only {
            "read-only"
        } else if self.is_unrestricted() {
            "danger-full-access"
        } else {
            "workspace-write"
        }
    }
    fn policy(&self) -> Value {
        if self.read_only {
            json!({ "type": "readOnly", "networkAccess": false })
        } else if self.is_unrestricted() {
            json!({ "type": "dangerFullAccess" })
        } else {
            json!({ "type": "workspaceWrite", "writableRoots": [self.cwd], "networkAccess": false })
        }
    }
    fn thread_request(&self, resume: Option<&str>) -> Value {
        let mut params = json!({ "cwd": self.cwd, "model": self.model, "sandbox": self.mode(),
            "approvalPolicy": self.approval_policy(), "approvalsReviewer": "user" });
        if let Some(helper) = &self.task_mcp {
            params["config"] = helper.config(&self.cwd);
            params["developerInstructions"] = helper.guidance().into();
        }
        // Thread mode alone inherits operator workspace-write configuration.
        // Pin the same boundaries as turn/start before validating its echo;
        // provider-owned login remains untouched and no arbitrary map is public.
        if params.get("config").is_none() {
            params["config"] = json!({});
        }
        params["config"]["sandbox_workspace_write.network_access"] = false.into();
        params["config"]["sandbox_workspace_write.writable_roots"] = json!([]);
        if self.user_input_tool {
            // Codex lists `request_user_input` for every thread but lets the
            // model call it in Default mode only with this feature; without
            // it the tool says "Plan mode" and refuses each call. A thread's
            // tools are fixed when it starts, so this cannot wait for the turn.
            params["config"]["features.default_mode_request_user_input"] = true.into();
        }
        if self.room_history {
            params["dynamicTools"] = json!([room_history::definition()]);
            // Keep the model's own compaction point. A fixed 24,000-token limit
            // left only a few thousand tokens above the instructions, so every
            // step compacted away the last tool results and the Agent searched
            // again without end. The host already bounds each history page by
            // the observed context window.
        }
        if self.room_files {
            let mut tools = params["dynamicTools"].as_array().cloned().unwrap_or_default();
            tools.extend(room_files::definitions());
            params["dynamicTools"] = tools.into();
        }
        if let Some(id) = resume {
            params["threadId"] = id.into();
            params["excludeTurns"] = true.into();
        } else {
            // Native fork_context subagents need the parent's retained rollout.
            // Keep disposable storage only for the explicit text-only mode.
            params["ephemeral"] = self.read_only.into();
            params["serviceName"] = "hagency".into();
            params["experimentalRawEvents"] = true.into();
        }
        params
    }
    fn turn_request(&self, thread: &str, input: String) -> Value {
        json!({ "threadId": thread, "input": [{ "type": "text", "text": input, "text_elements": [] }],
            "cwd": self.cwd, "model": self.model, "effort": self.effort,
            "approvalPolicy": self.approval_policy(), "approvalsReviewer": "user", "sandboxPolicy": self.policy() })
    }
}

fn valid_id(value: &str) -> Result<(), Error> {
    if super::text(value, 256) {
        Ok(())
    } else {
        Err(Error::Malformed)
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(Error::Malformed)
}
fn id<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    let value = string(value, key)?;
    valid_id(value)?;
    Ok(value)
}
fn object<'a>(value: &'a Value, key: &str) -> Result<&'a Value, Error> {
    value
        .get(key)
        .filter(|v| v.is_object())
        .ok_or(Error::Malformed)
}
