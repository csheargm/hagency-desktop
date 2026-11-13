//! Octos UI Protocol (OUP, `octos-ui/v1alpha1`) frames over `octos serve
//! --stdio` (ADR-193): bounded, typed observations, never dispatch or
//! approval authority. No process, credential or canonical-completion API
//! lives here.
use serde_json::{Value, json};

pub mod session;
pub mod task_tools;
pub mod profile_binding;

pub const PROTOCOL: &str = "octos-ui/v1alpha1";
/// OUP's own frame bound (`MAX_TEXT_FRAME_BYTES`).
pub const MAX_FRAME_BYTES: usize = 1_048_576;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const PARTIAL_FRAME_MS: u64 = 10_000;
/// Features understood by this connection. Older kernels may ignore an
/// optional feature; they then use their non-blocking question fallback.
pub const FEATURES: [&str; 13] = [
    "projection.envelope.v2",
    "approval.typed.v1",
    "session.workspace_cwd.v1",
    "user_question.v1",
    "turn.steer_active_only.v1",
    "event.turn_steer_dropped.v1",
    "runtime.host_scoped_profile.v1",
    "runtime.host_scoped_workspace_io.v1",
    "runtime.host_scoped_search.v1",
    "runtime.host_scoped_vision.v1",
    "runtime.host_patch_worker.v1",
    "runtime.host_scoped_web.v1",
    "plan.todos.v1",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("Octos stream is closed")]
    Closed,
    #[error("invalid Octos frame")]
    Envelope,
    #[error("unsupported Octos frame")]
    Unsupported,
    #[error("Octos frame capacity exceeded")]
    Capacity,
    #[error("Octos partial frame expired")]
    Timeout,
    #[error("Octos stream clock moved backwards")]
    Clock,
    #[error("Octos stream ended with an incomplete frame")]
    UnexpectedEof,
    #[error("invalid Octos host input")]
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
/// An Octos profile ID: the first segment of a session key, never a path.
pub fn profile_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
/// The session key Hagency names for one dispatch: `<profile>:local:<id>`.
pub fn session_key(session: &str, profile: &str) -> bool {
    profile_id(profile)
        && session
            .strip_prefix(profile)
            .and_then(|rest| rest.strip_prefix(":local:"))
            .is_some_and(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
}
/// A canonical lowercase UUID, the only turn ID shape OUP accepts.
pub fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_digit() || (b'a'..=b'f').contains(&b),
        })
}

/// A JSON-RPC error answer. Its message is upstream text, never kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub kind: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Errored,
    Interrupted,
    RateLimited,
}
/// Exact per-turn counters (`EnvelopeTokenUsage`). Omitted counters are zero;
/// an absent object is unknown, which the caller keeps as `None`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
}
impl Usage {
    fn parse(value: &Value) -> Result<Self, Error> {
        let object = value.as_object().ok_or(Error::Envelope)?;
        let counter = |key: &str| -> Result<u64, Error> {
            match object.get(key) {
                None | Some(Value::Null) => Ok(0),
                Some(value) => value
                    .as_u64()
                    .filter(|n| *n <= 9_007_199_254_740_991)
                    .ok_or(Error::Envelope),
            }
        };
        Ok(Self {
            input: counter("input_tokens")?,
            output: counter("output_tokens")?,
            reasoning: counter("reasoning_tokens")?,
            cache_read: counter("cache_read_tokens")?,
            cache_write: counter("cache_write_tokens")?,
        })
    }
    pub fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            input: self.input.checked_add(other.input)?,
            output: self.output.checked_add(other.output)?,
            reasoning: self.reasoning.checked_add(other.reasoning)?,
            cache_read: self.cache_read.checked_add(other.cache_read)?,
            cache_write: self.cache_write.checked_add(other.cache_write)?,
        })
    }
}

/// One canonical projection payload (spec §14). Only what the host reads.
pub enum Payload {
    AssistantDelta {
        text: String,
        segment_id: String,
    },
    AssistantPersisted {
        text: String,
        segment_id: Option<String>,
        message_id: Option<String>,
    },
    /// A separately persisted, user-facing background result on its own
    /// child stream. Its parent must belong to the admitted session.
    BackgroundComplete {
        parent_turn_id: String,
        task_id: String,
        message_id: String,
        text: String,
    },
    /// A tool started: its call ID and name when Octos gives them.
    ToolStart {
        tool_call_id: Option<String>,
        name: Option<String>,
    },
    /// That tool ended.
    ToolEnd {
        tool_call_id: Option<String>,
    },
    Terminal {
        outcome: Outcome,
        error_code: Option<String>,
        usage: Option<Usage>,
    },
    /// Every other tag, current or future: ignored, as OUP's additive rule asks.
    Other,
}
pub struct Envelope {
    pub session_id: String,
    /// The projection stream and its position: a redelivered envelope repeats
    /// both, and OUP's own client drops it by that pair.
    pub thread_id: String,
    pub seq: u64,
    /// The turn, or for a background child stream the child's own identity.
    pub turn_id: String,
    pub payload: Payload,
}
/// Payloads may carry private text and tool input; deliberately no Debug.
pub enum Notification {
    TurnStarted {
        session_id: String,
        turn_id: String,
    },
    Envelope(Envelope),
    ApprovalRequested {
        session_id: String,
        approval_id: String,
        turn_id: String,
        params: Value,
    },
    /// `approval/cancelled`, `approval/decided` or `approval/auto_resolved`.
    ApprovalSettled {
        session_id: String,
        approval_id: String,
    },
    QuestionRequested {
        session_id: String,
        question_id: String,
        turn_id: String,
        params: Value,
    },
    QuestionCancelled {
        session_id: String,
        question_id: String,
        turn_id: String,
    },
    SteerDropped {
        session_id: String,
        turn_id: String,
        inputs: Vec<String>,
    },
    Orchestration {
        session_id: String,
        active: bool,
    },
    PlanUpdated {
        session_id: String,
        turn_id: Option<String>,
        plan: Value,
    },
    /// `peer/tool/call` for a host tool (ADR-193 decision 5).
    ToolCall {
        params: Value,
    },
    ToolCancelled {
        call_id: String,
    },
    /// Every other notification: ignored (OUP's additive rule).
    Other,
}
pub enum Frame {
    Response {
        id: String,
        outcome: Result<Value, RpcError>,
    },
    Notification(Notification),
}
/// An optional bounded text field of a projection payload.
fn text_field(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(str::to_owned)
}
/// The session key a projection names: `session_id`, with its `topic`
/// suffix rebuilt when the envelope splits it out.
fn routed_session(params: &Value) -> Result<String, Error> {
    let session = field(params, "session_id")?;
    match params.get("topic").filter(|v| !v.is_null()) {
        None => Ok(session.to_owned()),
        Some(topic) => {
            let topic = topic
                .as_str()
                .filter(|t| identifier(t))
                .ok_or(Error::Envelope)?;
            if session.ends_with(&format!("#{topic}")) {
                Ok(session.to_owned())
            } else {
                Ok(format!("{session}#{topic}"))
            }
        }
    }
}
impl Frame {
    fn parse(value: Value) -> Result<Self, Error> {
        let object = value.as_object().ok_or(Error::Envelope)?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(Error::Envelope);
        }
        match (object.get("id"), object.get("method")) {
            (Some(id), None) => {
                // Hagency sends string IDs only; any other ID answers nothing it sent.
                let id = id
                    .as_str()
                    .filter(|v| identifier(v))
                    .ok_or(Error::Envelope)?
                    .to_owned();
                let outcome = match (object.get("result"), object.get("error")) {
                    (Some(result), None) => Ok(result.clone()),
                    (None, Some(error)) => Err(RpcError {
                        code: error
                            .get("code")
                            .and_then(Value::as_i64)
                            .ok_or(Error::Envelope)?,
                        kind: error
                            .get("data")
                            .and_then(|data| data.get("kind"))
                            .and_then(Value::as_str)
                            .filter(|kind| identifier(kind))
                            .map(str::to_owned),
                    }),
                    _ => return Err(Error::Envelope),
                };
                Ok(Self::Response { id, outcome })
            }
            (None, Some(method)) => {
                let method = method.as_str().ok_or(Error::Envelope)?;
                let params = object.get("params").cloned().unwrap_or(Value::Null);
                Ok(Self::Notification(Notification::parse(method, params)?))
            }
            // The server never sends a request (spec §3); one would ask for an
            // answer this host has no handler for.
            _ => Err(Error::Unsupported),
        }
    }
}
impl Notification {
    fn parse(method: &str, params: Value) -> Result<Self, Error> {
        Ok(match method {
            "turn/started" => Self::TurnStarted {
                session_id: routed_session(&params)?,
                turn_id: field(&params, "turn_id")?.to_owned(),
            },
            "turn/steer_dropped" => {
                let inputs = params.get("inputs").and_then(Value::as_array).ok_or(Error::Envelope)?;
                if inputs.len() > 16 { return Err(Error::Capacity); }
                let inputs = inputs.iter().map(|input| input.as_str()
                    .filter(|text| !text.is_empty() && text.len() <= MAX_TEXT_BYTES)
                    .map(str::to_owned).ok_or(Error::Envelope)).collect::<Result<_, _>>()?;
                Self::SteerDropped {
                    session_id: routed_session(&params)?,
                    turn_id: field(&params, "turn_id")?.to_owned(),
                    inputs,
                }
            }
            "projection/envelope" => {
                let session_id = routed_session(&params)?;
                let thread_id = field(&params, "thread_id")?.to_owned();
                let seq = params
                    .get("seq")
                    .and_then(Value::as_u64)
                    .ok_or(Error::Envelope)?;
                let turn_id = field(&params, "turn_id")?.to_owned();
                let payload = params.get("payload").ok_or(Error::Envelope)?;
                let data = payload.get("data").unwrap_or(&Value::Null);
                let payload = match payload.get("type").and_then(Value::as_str) {
                    Some("assistant_delta") => Payload::AssistantDelta {
                        text: data.get("text").and_then(Value::as_str)
                            .filter(|text| text.len() <= MAX_FRAME_BYTES)
                            .ok_or(Error::Envelope)?.to_owned(),
                        segment_id: field(data, "assistant_segment_id")?.to_owned(),
                    },
                    Some("assistant_persisted") => Payload::AssistantPersisted {
                        text: data
                            .get("text")
                            .and_then(Value::as_str)
                            .ok_or(Error::Envelope)?
                            .to_owned(),
                        segment_id: text_field(data, "assistant_segment_id"),
                        message_id: text_field(&data["meta"], "message_id"),
                    },
                    Some("background/spawn_complete") => Payload::BackgroundComplete {
                        parent_turn_id: field(data, "parent_turn_id")?.to_owned(),
                        task_id: field(data, "task_id")?.to_owned(),
                        message_id: field(data, "message_id")?.to_owned(),
                        text: data.get("content").and_then(Value::as_str)
                            .filter(|text| text.len() <= MAX_FRAME_BYTES)
                            .ok_or(Error::Envelope)?.to_owned(),
                    },
                    Some("tool_start") => Payload::ToolStart {
                        tool_call_id: text_field(data, "tool_call_id"),
                        name: text_field(data, "name"),
                    },
                    Some("tool_end") => Payload::ToolEnd {
                        tool_call_id: text_field(data, "tool_call_id"),
                    },
                    Some("turn_terminal") => Payload::Terminal {
                        outcome: match data.get("outcome").and_then(Value::as_str) {
                            Some("completed") => Outcome::Completed,
                            Some("errored") => Outcome::Errored,
                            Some("interrupted") => Outcome::Interrupted,
                            Some("rate_limited") => Outcome::RateLimited,
                            // An end this host cannot classify refuses the turn.
                            _ => return Err(Error::Unsupported),
                        },
                        error_code: data
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(Value::as_str)
                            .map(|code| code.chars().take(128).collect()),
                        usage: match data.get("token_usage").filter(|v| !v.is_null()) {
                            Some(usage) => Some(Usage::parse(usage)?),
                            None => None,
                        },
                    },
                    Some(_) => Payload::Other,
                    None => return Err(Error::Envelope),
                };
                Self::Envelope(Envelope {
                    session_id,
                    thread_id,
                    seq,
                    turn_id,
                    payload,
                })
            }
            "approval/requested" => Self::ApprovalRequested {
                session_id: routed_session(&params)?,
                approval_id: field(&params, "approval_id")?.to_owned(),
                turn_id: field(&params, "turn_id")?.to_owned(),
                params,
            },
            "approval/cancelled" | "approval/decided" | "approval/auto_resolved" => {
                Self::ApprovalSettled {
                    session_id: routed_session(&params)?,
                    approval_id: field(&params, "approval_id")?.to_owned(),
                }
            }
            "user_question/requested" => Self::QuestionRequested {
                session_id: routed_session(&params)?,
                question_id: field(&params, "question_id")?.to_owned(),
                turn_id: field(&params, "turn_id")?.to_owned(),
                params,
            },
            "user_question/cancelled" => Self::QuestionCancelled {
                session_id: routed_session(&params)?,
                question_id: field(&params, "question_id")?.to_owned(),
                turn_id: field(&params, "turn_id")?.to_owned(),
            },
            "session/orchestration" => Self::Orchestration {
                session_id: routed_session(&params)?,
                active: params
                    .get("active")
                    .and_then(Value::as_bool)
                    .ok_or(Error::Envelope)?,
            },
            "plan/updated" => Self::PlanUpdated {
                session_id: routed_session(&params)?,
                turn_id: params.get("turn_id").and_then(Value::as_str).filter(|id| identifier(id)).map(str::to_owned),
                plan: params.get("plan").cloned().unwrap_or(Value::Null),
            },
            "peer/tool/call" => Self::ToolCall { params },
            "peer/tool/cancel" => Self::ToolCancelled {
                call_id: field(&params, "call_id")?.to_owned(),
            },
            _ => Self::Other,
        })
    }
}

/// Pull-style, one bounded frame per call. EOF says nothing about child cleanup.
#[derive(Default)]
pub struct Decoder {
    bytes: Vec<u8>,
    since: Option<u64>,
    clock: u64,
    closed: bool,
}
impl Decoder {
    pub fn buffered_bytes(&self) -> usize {
        self.bytes.len()
    }
    pub(crate) fn deadline_ms(&self) -> Option<u64> {
        self.since.map(|at| at.saturating_add(PARTIAL_FRAME_MS))
    }
    fn fail<T>(&mut self, error: Error) -> Result<T, Error> {
        self.closed = true;
        self.bytes.clear();
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
    pub fn feed(&mut self, input: &[u8], now_ms: u64) -> Result<(usize, Option<Frame>), Error> {
        self.check_deadline(now_ms)?;
        let remaining = MAX_FRAME_BYTES.saturating_sub(self.bytes.len());
        let end = input
            .iter()
            .take(remaining + 1)
            .position(|&byte| byte == b'\n');
        let length = end.unwrap_or(input.len());
        if length > remaining {
            return self.fail(Error::Capacity);
        }
        if length > 0 && self.since.is_none() {
            self.since = Some(now_ms);
        }
        self.bytes.extend_from_slice(&input[..length]);
        let Some(end) = end else {
            return Ok((length, None));
        };
        let frame = crate::json::parse(&self.bytes)
            .map_err(Error::from)
            .and_then(Frame::parse);
        self.bytes.clear();
        self.since = None;
        match frame {
            Ok(frame) => Ok((end + 1, Some(frame))),
            Err(error) => self.fail(error),
        }
    }
    pub fn eof(&mut self) -> Result<(), Error> {
        if self.closed {
            return Err(Error::Closed);
        }
        let partial = !self.bytes.is_empty();
        self.closed = true;
        self.bytes.clear();
        if partial {
            Err(Error::UnexpectedEof)
        } else {
            Ok(())
        }
    }
}

pub(crate) fn encode(value: Value) -> Result<Vec<u8>, Error> {
    crate::json::depth(&value, 0)?;
    let mut bytes = serde_json::to_vec(&value).map_err(|_| Error::Input)?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(Error::Capacity);
    }
    bytes.push(b'\n');
    Ok(bytes)
}
/// One JSON-RPC request with a host-chosen string ID. Prepared bytes only,
/// never a delivery or execution receipt.
pub fn request(id: &str, method: &str, params: Value) -> Result<Vec<u8>, Error> {
    if !identifier(id) || !identifier(method) || !params.is_object() {
        return Err(Error::Input);
    }
    encode(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
}
/// Hagency's own Octos settings (ADR-193 decision 3), passed with `--config`
/// so Octos reads neither a project's `.octos/config.json` nor the user's
/// config: no hooks, no MCP servers, and sessions kept out of the project.
/// The model and keys still come from the user's profile.
pub const CONFIG: &[u8] =
    br#"{"version":1,"hooks":[],"mcp_servers":[],"appui":{"sessions_in_cwd":false}}"#;
/// The one file Octos writes into a writable workspace that has none: its
/// workspace policy, kept out of Git by the host.
pub const WORKSPACE_POLICY: &str = ".octos-workspace.toml";
/// The argv after the executable: one private stdio server bound to the
/// dispatch's workspace, network denied, runtime state in its own directory
/// and Hagency's own settings file.
pub fn serve_arguments(
    workspace: &str,
    instance_dir: &str,
    config: &str,
) -> Result<Vec<String>, Error> {
    if [workspace, instance_dir, config]
        .iter()
        .any(|path| !path.starts_with('/') || path.len() > 1024 || path.contains('\0'))
    {
        return Err(Error::Input);
    }
    Ok([
        "serve",
        "--stdio",
        "--cwd",
        workspace,
        "--no-network",
        "--instance-data-dir",
        instance_dir,
        "--config",
        config,
    ]
    .map(str::to_owned)
    .to_vec())
}

/// Require an Octos binary that accepts the scoped-host startup flag. Older
/// binaries reject the flag before they can start unrelated profile extensions.
pub fn serve_arguments_scoped(
    workspace: &str,
    instance_dir: &str,
    config: &str,
    profile: &str,
) -> Result<Vec<String>, Error> {
    if !profile_id(profile) { return Err(Error::Input); }
    let mut args = serve_arguments(workspace, instance_dir, config)?;
    args.extend(["--host-scoped-profile".to_owned(), profile.to_owned()]);
    Ok(args)
}

/// The most of one Octos profile file Hagency reads (ADR-193 decision 7).
pub const MAX_PROFILE_BYTES: u64 = 1024 * 1024;
/// A configured model and its provider family. The original profile retains
/// the route and credentials; these values are safe for the owner's inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileModel {
    pub family: String,
    pub model: String,
}
/// Read the profile's configured primary and unambiguous alternatives, not
/// the global catalog or provider credentials. A model ID repeated in two
/// fallback routes is omitted because the enrollment contract names a model
/// but does not yet carry a route identifier. The primary remains selectable.
pub fn profile_models(id: &str, bytes: &[u8]) -> Option<Vec<ProfileModel>> {
    #[derive(serde::Deserialize)]
    struct Profile {
        id: String,
        #[serde(default)]
        parent_id: Option<serde::de::IgnoredAny>,
        config: Config,
    }
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(default)]
        llm: Option<Llm>,
    }
    #[derive(serde::Deserialize)]
    struct Llm {
        #[serde(default)]
        primary: Option<Primary>,
        #[serde(default)]
        fallbacks: Vec<Primary>,
    }
    #[derive(serde::Deserialize)]
    struct Primary {
        #[serde(default)]
        family_id: Option<String>,
        #[serde(default)]
        model_id: Option<String>,
    }
    if !profile_id(id) || bytes.len() as u64 > MAX_PROFILE_BYTES {
        return None;
    }
    let profile: Profile = serde_json::from_slice(bytes).ok()?;
    if profile.id != id || profile.parent_id.is_some() { return None; }
    let llm = profile.config.llm?;
    if llm.fallbacks.len() > 64 { return None; }
    let primary = llm.primary?;
    let named = |value: &str, max: usize| {
        !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
    };
    let parse = |value: Primary| match (value.family_id, value.model_id) {
        (Some(family), Some(model)) if named(&family, 128) && named(&model, 256) => Some(ProfileModel { family, model }),
        _ => None,
    };
    let primary = parse(primary)?;
    let alternatives: Vec<_> = llm.fallbacks.into_iter().filter_map(parse).collect();
    let mut models = vec![primary.clone()];
    for candidate in &alternatives {
        if candidate.model != primary.model
            && alternatives.iter().filter(|other| other.model == candidate.model).count() == 1 {
            models.push(candidate.clone());
        }
    }
    // Enrollment and its read projection are bounded to 64 models per
    // profile. Never expose a choice which cannot be inventoried later.
    if models.len() > 64 { return None; }
    Some(models)
}

/// Backward-compatible primary model projection for personal Octos sessions.
pub fn profile_model(id: &str, bytes: &[u8]) -> Option<ProfileModel> {
    profile_models(id, bytes)?.into_iter().next()
}
