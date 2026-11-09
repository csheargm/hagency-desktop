//! Claude Code streaming JSONL observations, not dispatch or approval authority.
//! No process, credential, owner-authorization or canonical-completion API lives here.
pub mod account;
use serde_json::{Value, json};

pub mod session;
mod task_mcp;
pub use task_mcp::{TaskMcp, task_arguments};

pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const PARTIAL_FRAME_MS: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Claude stream is closed")]
    Closed,
    #[error("invalid Claude stream envelope")]
    Envelope,
    #[error("unsupported Claude stream envelope")]
    Unsupported,
    #[error("Claude stream capacity exceeded")]
    Capacity,
    #[error("Claude partial frame expired")]
    Timeout,
    #[error("Claude stream clock moved backwards")]
    Clock,
    #[error("Claude stream ended with an incomplete frame")]
    UnexpectedEof,
    #[error("invalid Claude host input")]
    Input,
}
impl From<crate::json::Error> for Error {
    fn from(error: crate::json::Error) -> Self {
        match error {
            crate::json::Error::Envelope => Self::Envelope,
            crate::json::Error::Capacity => Self::Capacity,
        }
    }
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, Error> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| identifier(v))
        .ok_or(Error::Envelope)
}
fn object(value: &Value) -> Result<(), Error> {
    value.as_object().map(|_| ()).ok_or(Error::Envelope)
}
fn keys(value: &Value, allowed: &[&str]) -> Result<(), Error> {
    if value
        .as_object()
        .ok_or(Error::Envelope)?
        .keys()
        .any(|key| !allowed.contains(&key.as_str()))
    {
        return Err(Error::Envelope);
    }
    Ok(())
}

fn known_type(kind: &str) -> bool {
    matches!(
        kind,
        "control_request"
            | "control_response"
            | "control_cancel_request"
            | "system"
            | "assistant"
            | "user"
            | "result"
            | "stream_event"
            | "tool_progress"
            | "tool_use_summary"
            | "auth_status"
            | "rate_limit_event"
            | "command_lifecycle"
    )
}
fn ignorable_type(kind: &str) -> bool {
    identifier(kind) && !known_type(kind) && !kind.starts_with("control_")
}
// Read only a bounded prefix to classify an oversized non-authority event.
// This never returns an actionable message; the entire line is discarded.
fn oversized_metadata(prefix: &[u8]) -> bool {
    use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
    struct Type<'a>(&'a mut Option<String>);
    impl<'de> Visitor<'de> for Type<'_> {
        type Value = ();
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a native event envelope")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            while let Some(key) = map.next_key::<String>()? {
                if key == "type" {
                    *self.0 = Some(map.next_value::<String>()?);
                    return Err(serde::de::Error::custom("classified native prefix"));
                }
                map.next_value::<IgnoredAny>()?;
            }
            Err(serde::de::Error::custom("missing native type"))
        }
    }
    let mut kind = None;
    let _ = serde_json::Deserializer::from_slice(prefix).deserialize_map(Type(&mut kind));
    kind.is_some_and(|kind| ignorable_type(&kind))
}

/// Payloads may contain private tool arguments/text; deliberately no Debug.
pub enum Message {
    /// Private native SDK transport. Decoding alone supplies no room authority.
    Mcp {
        request_id: String,
        server_name: String,
        message: Value,
    },
    Permission {
        request_id: String,
        tool_name: String,
        tool_use_id: Option<String>,
        input: Value,
    },
    ControlResponse {
        request_id: String,
        outcome: ControlOutcome,
    },
    ControlCancel {
        request_id: String,
    },
    Event {
        session_id: String,
        kind: EventKind,
        payload: Value,
    },
}
pub enum ControlOutcome {
    Success(Value),
    Refused,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    System,
    Assistant,
    User,
    Result,
    Stream,
    ToolProgress,
    ToolSummary,
    AuthStatus,
    RateLimit,
    CommandLifecycle,
}
impl Message {
    fn parse(value: Value) -> Result<Self, Error> {
        object(&value)?;
        match field(&value, "type")? {
            "control_request" => {
                keys(&value, &["type", "request_id", "request"])?;
                let request_id = field(&value, "request_id")?.to_owned();
                let request = value.get("request").ok_or(Error::Envelope)?;
                object(request)?;
                if field(request, "subtype")? == "mcp_message" {
                    keys(request, &["subtype", "server_name", "message"])?;
                    let message = request.get("message").ok_or(Error::Envelope)?;
                    object(message)?;
                    return Ok(Self::Mcp {
                        request_id,
                        server_name: field(request, "server_name")?.to_owned(),
                        message: message.clone(),
                    });
                }
                if field(request, "subtype")? != "can_use_tool" {
                    return Err(Error::Unsupported);
                }
                let tool_name = field(request, "tool_name")?.to_owned();
                let input = request.get("input").ok_or(Error::Envelope)?;
                object(input)?;
                let tool_use_id = request
                    .get("tool_use_id")
                    .map(|value| {
                        value
                            .as_str()
                            .filter(|v| identifier(v))
                            .map(str::to_owned)
                            .ok_or(Error::Envelope)
                    })
                    .transpose()?;
                Ok(Self::Permission {
                    request_id,
                    tool_name,
                    tool_use_id,
                    input: input.clone(),
                })
            }
            "control_response" => {
                keys(&value, &["type", "response"])?;
                let response = value.get("response").ok_or(Error::Envelope)?;
                let request_id = field(response, "request_id")?.to_owned();
                let outcome = match field(response, "subtype")? {
                    "success" => {
                        keys(
                            response,
                            &[
                                "subtype",
                                "request_id",
                                "response",
                                "pending_permission_requests",
                                "pending_user_dialog_requests",
                            ],
                        )?;
                        // Installed CLI2.1.270 carries these on initialize. A
                        // fresh one-prompt session cannot adopt preexisting work.
                        for name in [
                            "pending_permission_requests",
                            "pending_user_dialog_requests",
                        ] {
                            if let Some(pending) = response.get(name) {
                                let pending = pending.as_array().ok_or(Error::Envelope)?;
                                if !pending.is_empty() {
                                    return Err(Error::Unsupported);
                                }
                            }
                        }
                        let body = response.get("response").ok_or(Error::Envelope)?;
                        object(body)?;
                        ControlOutcome::Success(body.clone())
                    }
                    "error" => {
                        keys(response, &["subtype", "request_id", "error"])?;
                        response
                            .get("error")
                            .and_then(Value::as_str)
                            .ok_or(Error::Envelope)?;
                        // The upstream error text is not a safe diagnostic.
                        ControlOutcome::Refused
                    }
                    _ => return Err(Error::Unsupported),
                };
                Ok(Self::ControlResponse {
                    request_id,
                    outcome,
                })
            }
            "control_cancel_request" => {
                keys(&value, &["type", "request_id"])?;
                Ok(Self::ControlCancel {
                    request_id: field(&value, "request_id")?.to_owned(),
                })
            }
            kind => {
                let kind = match kind {
                    "system" => {
                        field(&value, "subtype")?;
                        EventKind::System
                    }
                    "assistant" => {
                        object(value.get("message").ok_or(Error::Envelope)?)?;
                        EventKind::Assistant
                    }
                    "user" => {
                        object(value.get("message").ok_or(Error::Envelope)?)?;
                        EventKind::User
                    }
                    "result" => {
                        let subtype = field(&value, "subtype")?;
                        let is_error = value
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .ok_or(Error::Envelope)?;
                        if subtype == "success" && !is_error {
                            value
                                .get("result")
                                .and_then(Value::as_str)
                                .ok_or(Error::Envelope)?;
                        }
                        if let Some(usage) = value.get("usage") {
                            object(usage)?;
                        }
                        EventKind::Result
                    }
                    "stream_event" => {
                        object(value.get("event").ok_or(Error::Envelope)?)?;
                        EventKind::Stream
                    }
                    "tool_progress" => EventKind::ToolProgress,
                    "tool_use_summary" => EventKind::ToolSummary,
                    "auth_status" => EventKind::AuthStatus,
                    "rate_limit_event" => EventKind::RateLimit,
                    "command_lifecycle" => {
                        field(&value, "command_uuid")?;
                        field(&value, "state")?;
                        EventKind::CommandLifecycle
                    }
                    _ => return Err(Error::Unsupported),
                };
                // Binding this observation to an owned turn is the future Host's
                // responsibility. A syntactically valid ID is not that binding.
                let session_id = field(&value, "session_id")?.to_owned();
                if let Some(parent) = value.get("parent_tool_use_id")
                    && !parent.is_null()
                    && !parent.as_str().is_some_and(identifier)
                {
                    return Err(Error::Envelope);
                }
                Ok(Self::Event {
                    session_id,
                    kind,
                    payload: value,
                })
            }
        }
    }
}

/// Pull-style, one bounded frame per call. EOF says nothing about child cleanup.
#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
    since: Option<u64>,
    clock: u64,
    closed: bool,
    discarding_metadata: bool,
    ignored_frames: u64,
}
impl Decoder {
    pub fn buffered_bytes(&self) -> usize {
        self.bytes.len() + usize::from(self.discarding_metadata)
    }
    pub fn ignored_frames(&self) -> u64 {
        self.ignored_frames
    }
    pub(crate) fn ignored_metadata(&mut self) {
        self.ignored_frames = self.ignored_frames.saturating_add(1);
    }
    pub(crate) fn deadline_ms(&self) -> Option<u64> {
        self.since.map(|at| at.saturating_add(PARTIAL_FRAME_MS))
    }
    fn fail<T>(&mut self, error: Error) -> Result<T, Error> {
        self.closed = true;
        self.bytes.clear();
        self.discarding_metadata = false;
        Err(error)
    }
    pub fn check_deadline(&mut self, now_ms: u64) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        if now_ms < self.clock {
            return self.fail(Error::Clock);
        }
        self.clock = now_ms;
        if self.since.is_some_and(|at| now_ms - at >= PARTIAL_FRAME_MS) {
            return self.fail(Error::Timeout);
        }
        Ok(())
    }
    pub fn feed(&mut self, input: &[u8], now_ms: u64) -> Result<(usize, Option<Message>), Error> {
        self.check_deadline(now_ms)?;
        if self.discarding_metadata {
            let end = input.iter().position(|&byte| byte == b'\n');
            if let Some(end) = end {
                self.discarding_metadata = false;
                self.since = None;
                self.ignored_frames = self.ignored_frames.saturating_add(1);
                return Ok((end + 1, None));
            }
            return Ok((input.len(), None));
        }
        let remaining = MAX_FRAME_BYTES.saturating_sub(self.bytes.len());
        let end = input
            .iter()
            .take(remaining + 1)
            .position(|&byte| byte == b'\n');
        let length = end.unwrap_or(input.len());
        if length > remaining {
            self.bytes.extend_from_slice(&input[..remaining]);
            if oversized_metadata(&self.bytes) {
                self.bytes.clear();
                self.discarding_metadata = true;
                self.since.get_or_insert(now_ms);
                let (used, message) = self.feed(&input[remaining..], now_ms)?;
                return Ok((remaining + used, message));
            }
            return self.fail(Error::Capacity);
        }
        if length > 0 && self.since.is_none() {
            self.since = Some(now_ms);
        }
        self.bytes.extend_from_slice(&input[..length]);
        let Some(end) = end else {
            return Ok((length, None));
        };
        let message = crate::json::parse(&self.bytes)
            .map_err(Error::from)
            .and_then(|value| {
                if ignorable_type(field(&value, "type")?) {
                    self.ignored_frames = self.ignored_frames.saturating_add(1);
                    Ok(None)
                } else {
                    Message::parse(value).map(Some)
                }
            });
        self.bytes.clear();
        self.since = None;
        match message {
            Ok(message) => Ok((end + 1, message)),
            Err(error) => self.fail(error),
        }
    }
    pub fn eof(&mut self) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        let partial = !self.bytes.is_empty() || self.discarding_metadata;
        self.closed = true;
        self.bytes.clear();
        if partial {
            Err(Error::UnexpectedEof)
        } else {
            Ok(())
        }
    }
}

fn encode(value: Value) -> Result<Vec<u8>, Error> {
    crate::json::depth(&value, 0)?;
    // Callers below accept only bounded text/IDs and fixed shape, not arbitrary
    // JSON, so allocation itself is bounded before the serialized size check.
    let mut bytes = serde_json::to_vec(&value).map_err(|_| Error::Input)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Capacity);
    }
    bytes.push(b'\n');
    Ok(bytes)
}
fn control(request_id: &str, request: Value) -> Result<Vec<u8>, Error> {
    if !identifier(request_id) {
        return Err(Error::Input);
    }
    encode(json!({"type":"control_request","request_id":request_id,"request":request}))
}
/// Prepared bytes only, not an acknowledged initialization or write receipt.
pub fn initialize(request_id: &str) -> Result<Vec<u8>, Error> {
    control(request_id, json!({"subtype":"initialize","hooks":null}))
}
pub fn interrupt(request_id: &str) -> Result<Vec<u8>, Error> {
    control(request_id, json!({"subtype":"interrupt"}))
}
pub fn prompt(text: &str, session_id: Option<&str>) -> Result<Vec<u8>, Error> {
    if text.is_empty() || text.len() > MAX_TEXT_BYTES || session_id.is_some_and(|v| !identifier(v))
    {
        return Err(Error::Input);
    }
    encode(
        json!({"type":"user","message":{"role":"user","content":text},
        "parent_tool_use_id":null,"session_id":session_id.unwrap_or("")}),
    )
}
/// Base stdio transport arguments. The owned launch adds its scoped approval
/// configuration before admitting work.
pub fn arguments(model: &str) -> Result<Vec<String>, Error> {
    arguments_for_workspace(model, true)
}

/// Native Claude Code capabilities with Hagency's private approval transport.
/// Preserve the owner's settings, skills, hooks, MCP servers and complete tool
/// inventory. An ask rule routes native tools (including newly added tools and
/// MCP tools) through the host; it does not grant permission to execute them.
pub fn interactive_arguments(model: &str, effort: &str) -> Result<Vec<String>, Error> {
    if !matches!(effort, "low" | "medium" | "high" | "xhigh" | "max") {
        return Err(Error::Input);
    }
    let mut args = arguments(model)?;
    args.extend([
        "--include-partial-messages".into(),
        "--effort".into(),
        effort.into(),
        "--settings".into(),
        json!({"permissions":{"ask":["*"]},"sandbox":{"autoAllowBashIfSandboxed":false}})
            .to_string(),
    ]);
    Ok(args)
}
fn arguments_for_workspace(model: &str, may_write: bool) -> Result<Vec<String>, Error> {
    // Native aliases include context suffixes (sonnet[1m]); Bedrock accepts
    // full inference-profile ARNs. Keep these as one argv value, never a shell
    // fragment or an extra option. Availability is checked by the native CLI.
    if model.is_empty()
        || model.len() > 1024
        || !model.as_bytes()[0].is_ascii_alphanumeric()
        || !model.bytes().all(|b| {
            b.is_ascii_alphanumeric()
                || matches!(b, b'.' | b'_' | b'-' | b':' | b'/' | b'@' | b'[' | b']')
        })
    {
        return Err(Error::Input);
    }
    let mut args = [
        "--print",
        "--verbose",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--permission-mode",
        if may_write { "auto" } else { "plan" },
        "--permission-prompt-tool",
        "stdio",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(format!("--model={model}"));
    Ok(args)
}
