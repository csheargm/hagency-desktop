//! Native SDK MCP adapter. Room access and current dispatch authorization stay
//! with the host; this module owns only the original callback and wire reply.
use super::{ControlOutcome, Error, Message, Operation, Phase, PreparedUpdate, SessionDriver, io};
use crate::{
    deadline::Deadline,
    room_tools::{RoomOperation, RoomTools, ROOM_HISTORY_RESULT_BYTES},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::Instant,
};

pub(super) const SERVER: &str = "hagency_matrix";
const MAX_PENDING: usize = 16;

/// Obtained only from an observed callback on the original native stream. No
/// room, URL, credential, requester or arbitrary file root can be supplied here.
#[derive(Clone)]
pub struct RoomToolRequest {
    source: Arc<()>,
    id: String,
    session: String,
    tool_use_id: Option<String>,
    operation: Option<RoomOperation>,
    deadline: Instant,
}
impl RoomToolRequest {
    pub fn request_id(&self) -> &str {
        &self.id
    }
    pub fn session_id(&self) -> &str {
        &self.session
    }
    pub fn tool_use_id(&self) -> Option<&str> {
        self.tool_use_id.as_deref()
    }
    /// None means pure MCP metadata, never a request for room data or tools.
    pub fn operation(&self) -> Option<&RoomOperation> {
        self.operation.as_ref()
    }
    /// The host must leave time to write the reply within this original bound.
    pub fn response_deadline(&self) -> Instant {
        self.deadline
    }
}
pub struct PreparedRoomReply {
    source: Arc<()>,
    id: String,
    sequence: u64,
    frame: io::PreparedFrame,
}
enum Stage {
    Waiting,
    Prepared(u64),
    Sent,
    Cancelled,
}
struct Pending {
    request: RoomToolRequest,
    mcp_id: Option<Value>,
    metadata: Option<Value>,
    deadline: Instant,
    stage: Stage,
}
pub(super) struct State {
    tools: RoomTools,
    bound: bool,
    initialized: bool,
    ready: bool,
    active: bool,
    seen: BTreeSet<String>,
    pending: BTreeMap<String, Pending>,
}
impl State {
    fn new(tools: RoomTools) -> Self {
        Self {
            tools,
            bound: false,
            initialized: false,
            ready: false,
            active: false,
            seen: BTreeSet::new(),
            pending: BTreeMap::new(),
        }
    }
    pub(super) fn seen(&self, id: &str) -> bool {
        self.seen.contains(id)
    }
    pub(super) fn quiescent(&self) -> bool {
        self.pending
            .values()
            .all(|p| matches!(p.stage, Stage::Sent | Stage::Cancelled))
    }
    pub(super) fn untouched(&self) -> bool {
        self.pending.is_empty()
    }
    pub(super) fn begin_turn(&mut self) {
        self.active = false;
        self.pending.clear();
        self.seen.clear();
    }
    pub(super) fn cancel(&mut self, id: &str) -> Result<(), Error> {
        let pending = self.pending.get_mut(id).ok_or(Error::Identity)?;
        if matches!(pending.stage, Stage::Cancelled) {
            return Err(Error::Identity);
        }
        pending.stage = Stage::Cancelled;
        pending.metadata = None;
        pending.request.operation = None;
        Ok(())
    }
    fn remember(&mut self, id: &str) -> Result<(), Error> {
        if self.seen.contains(id) {
            return Err(Error::Identity);
        }
        if self.seen.len() >= super::MAX_CALLBACK_IDS {
            return Err(Error::Capacity);
        }
        self.seen.insert(id.into());
        Ok(())
    }
    fn metadata(&mut self, message: &Value) -> Result<Option<Value>, Error> {
        let method = envelope(message)?;
        let result = match method {
            "initialize" => {
                let params = &message["params"];
                let version = params["protocolVersion"].as_str().ok_or(Error::Malformed)?;
                if !params["capabilities"].is_object() || !params["clientInfo"].is_object() {
                    return Err(Error::Malformed);
                }
                self.initialized = true;
                self.ready = false;
                let version = match version {
                    "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25" => version,
                    _ => "2025-11-25",
                };
                json!({"protocolVersion":version,"capabilities":{"tools":{}},
                    "serverInfo":{"name":SERVER,"version":"1"}})
            }
            "notifications/initialized" if self.initialized => {
                self.ready = true;
                json!({})
            }
            "tools/list" if self.ready => {
                if message["params"]
                    .get("cursor")
                    .is_some_and(|v| !v.is_null())
                {
                    return Err(Error::Malformed);
                }
                json!({"tools":self.tools.definitions()})
            }
            "ping" if self.initialized => json!({}),
            "tools/call" if self.ready => return Ok(None),
            _ => return Err(Error::Malformed),
        };
        Ok(Some(result))
    }
}
fn envelope(message: &Value) -> Result<&str, Error> {
    if message["jsonrpc"] != "2.0" {
        return Err(Error::Malformed);
    }
    let method = message["method"].as_str().ok_or(Error::Malformed)?;
    if let Some(id) = message.get("id") {
        if !(id.is_u64() || id.as_str().is_some_and(super::super::identifier)) {
            return Err(Error::Malformed);
        }
    } else if method != "notifications/initialized" {
        return Err(Error::Malformed);
    }
    if message.get("params").is_some_and(|v| !v.is_object()) {
        return Err(Error::Malformed);
    }
    Ok(method)
}
pub(super) fn server_config() -> Value {
    json!({"type":"sdk","name":SERVER,"alwaysLoad":true})
}
pub(super) fn check_foreign_sdk_servers(status: &Value, owned: &[&str]) -> Result<(), Error> {
    // CLI --mcp-config stdio servers also use scope/source "dynamic" and are
    // retained by mcp_set_servers. Only an unowned in-process SDK server needs
    // rejection: its callback handler cannot be supplied by this room adapter.
    if super::task_mcp::servers(status)?.iter().any(|s| {
        (s["transport"] == "sdk" || s["source"] == "sdk")
            && !s["name"].as_str().is_some_and(|name| owned.contains(&name))
    }) {
        return Err(Error::RoomToolStartup);
    }
    Ok(())
}
fn response(id: &str, mcp_id: Option<&Value>, result: Value) -> Result<Vec<u8>, Error> {
    let mut mcp = json!({"jsonrpc":"2.0","result":result});
    if let Some(id) = mcp_id {
        mcp["id"] = id.clone();
    }
    Ok(crate::claude::encode(
        json!({"type":"control_response","response":{
        "subtype":"success","request_id":id,"response":{"mcp_response":mcp}}}),
    )?)
}

impl<R, W, E> SessionDriver<R, W, E> {
    /// Each admitted turn needs an explicit host attachment after system/init.
    /// Reusing definitions or a live process never reuses room authorization.
    pub fn enable_room_tool_callbacks(&mut self) -> Result<(), Error> {
        if self.phase != Phase::Running || self.awaiting_init || self.observation_source().is_err()
        {
            return Err(Error::State);
        }
        let state = self
            .room_mcp
            .as_mut()
            .filter(|s| s.bound && !s.active)
            .ok_or(Error::State)?;
        state.active = true;
        Ok(())
    }
    pub(super) fn callback_deadline(&mut self) -> Result<Deadline, Error> {
        let mut until = self.control.deadline(&self.wire)?;
        if let Some(deadline) = self.interrupt.deadline() {
            until = until.limit(deadline);
        }
        if let Some(deadline) = self.completion.steers.deadline() {
            until = until.limit(deadline);
        }
        if let Some(state) = &self.room_mcp {
            for pending in state.pending.values() {
                if matches!(pending.stage, Stage::Waiting | Stage::Prepared(_)) {
                    until = until.limit(pending.deadline);
                }
            }
        }
        if until.expired() {
            return Err(Error::Timeout);
        }
        Ok(until)
    }
    pub(super) fn admit_room_request(
        &mut self,
        message: &Message,
        received: Instant,
    ) -> Result<(), Error> {
        let Message::Mcp {
            request_id,
            server_name,
            message,
        } = message
        else {
            return Err(Error::State);
        };
        if server_name != SERVER || self.awaiting_init || self.control.seen(request_id) {
            return Err(Error::Identity);
        }
        let session = self.session_id.clone().ok_or(Error::Identity)?;
        let state = self
            .room_mcp
            .as_mut()
            .filter(|s| s.bound)
            .ok_or(Error::State)?;
        state
            .pending
            .retain(|_, p| matches!(p.stage, Stage::Waiting | Stage::Prepared(_)));
        if state.pending.len() >= MAX_PENDING {
            return Err(Error::Capacity);
        }
        state.remember(request_id)?;
        let metadata = state.metadata(message)?;
        let (operation, tool_use_id) = if metadata.is_none() {
            if !state.active {
                return Err(Error::State);
            }
            let params = &message["params"];
            if params.as_object().is_none_or(|p| {
                p.keys()
                    .any(|k| !["name", "arguments", "_meta"].contains(&k.as_str()))
            }) {
                return Err(Error::Malformed);
            }
            let id = params["_meta"]["claudecode/toolUseId"]
                .as_str()
                .filter(|id| super::super::identifier(id))
                .ok_or(Error::Identity)?;
            let operation = state
                .tools
                .parse(
                    params["name"].as_str().ok_or(Error::Malformed)?,
                    &params["arguments"],
                )
                .map_err(|_| Error::Malformed)?;
            (Some(operation), Some(id.into()))
        } else {
            (None, None)
        };
        let request = RoomToolRequest {
            source: self.source.clone(),
            id: request_id.clone(),
            session,
            tool_use_id,
            operation,
            deadline: self.wire.callback_deadline(received),
        };
        state.pending.insert(
            request_id.clone(),
            Pending {
                request,
                mcp_id: message.get("id").cloned(),
                metadata,
                deadline: self.wire.callback_deadline(received),
                stage: Stage::Waiting,
            },
        );
        Ok(())
    }
    pub fn room_tool_request(&self, id: &str) -> Result<RoomToolRequest, Error> {
        if self.phase != Phase::Running {
            return Err(Error::State);
        }
        self.room_mcp
            .as_ref()
            .and_then(|s| s.pending.get(id))
            .filter(|p| matches!(p.stage, Stage::Waiting))
            .map(|p| p.request.clone())
            .ok_or(Error::RoomToolUnavailable)
    }
    /// Host must authorize this operation against the current dispatch before
    /// executing it. A tool result is not an owner approval or a delivery receipt.
    pub fn prepare_room_tool_reply(
        &mut self,
        request: &RoomToolRequest,
        text: &str,
        is_error: bool,
    ) -> Result<PreparedRoomReply, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = if text.len() > ROOM_HISTORY_RESULT_BYTES {
            Err(Error::Capacity)
        } else {
            operation.driver.prepare_room_reply(
                request,
                Some(json!({"content":[{"type":"text","text":text}],"isError":is_error})),
            )
        };
        operation.finish(result)
    }
    pub fn prepare_room_metadata_reply(
        &mut self,
        request: &RoomToolRequest,
    ) -> Result<PreparedRoomReply, Error> {
        let operation = Operation::new(self, Phase::Running)?;
        let result = operation.driver.prepare_room_reply(request, None);
        operation.finish(result)
    }
    fn prepare_room_reply(
        &mut self,
        request: &RoomToolRequest,
        result: Option<Value>,
    ) -> Result<PreparedRoomReply, Error> {
        if self.interrupt.blocks_replies() {
            return Err(Error::RoomToolUnavailable);
        }
        if !Arc::ptr_eq(&self.source, &request.source) {
            return Err(Error::Identity);
        }
        let until = self.callback_deadline()?;
        self.wire.check(until)?;
        let pending = self
            .room_mcp
            .as_mut()
            .and_then(|s| s.pending.get_mut(&request.id))
            .ok_or(Error::Identity)?;
        if !matches!(pending.stage, Stage::Waiting) {
            return Err(Error::RoomToolUnavailable);
        }
        if result.is_some() != pending.request.operation.is_some() {
            return Err(Error::State);
        }
        let result = result
            .or_else(|| pending.metadata.take())
            .ok_or(Error::State)?;
        let bytes = response(&request.id, pending.mcp_id.as_ref(), result)?;
        let sequence = self.control.next_sequence()?;
        pending.stage = Stage::Prepared(sequence);
        Ok(PreparedRoomReply {
            source: self.source.clone(),
            id: request.id.clone(),
            sequence,
            frame: io::PreparedFrame::new(sequence, bytes, pending.deadline),
        })
    }
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin, E: AsyncRead + Unpin> SessionDriver<R, W, E> {
    /// Adds a fixed SDK server without replacing owner/project/plugin settings.
    /// The host still supplies a fresh room capability for each admitted turn.
    pub async fn bind_room_tools(&mut self, tools: RoomTools) -> Result<(), Error> {
        let operation = Operation::new(self, Phase::Ready)?;
        let result = operation.driver.bind_room_inner(tools).await;
        operation.finish(result)
    }
    async fn bind_room_inner(&mut self, tools: RoomTools) -> Result<(), Error> {
        if self.room_mcp.is_some() || self.wire.has_buffered_input() {
            return Err(Error::State);
        }
        let until = self.wire.event_deadline();
        let before = self
            .mcp_control(
                "hagency-room-before",
                json!({"subtype":"mcp_status"}),
                until,
            )
            .await?;
        let original = super::task_mcp::server_names(&before)?;
        if original.contains(SERVER) {
            return Err(Error::RoomToolStartup);
        }
        let owned = if self.task_mcp.is_some() {
            &[crate::claude::task_mcp::SERVER][..]
        } else {
            &[]
        };
        check_foreign_sdk_servers(&before, owned)?;
        self.room_mcp = Some(State::new(tools));
        let mut servers = json!({SERVER: server_config()});
        if let Some(helper) = &self.task_mcp {
            servers[crate::claude::task_mcp::SERVER] = helper.server();
        }
        let added = self
            .mcp_control(
                "hagency-room-bind",
                json!({"subtype":"mcp_set_servers","servers":servers}),
                until,
            )
            .await?;
        if added != json!({"added":[SERVER],"removed":[],"errors":{}}) {
            return Err(Error::RoomToolStartup);
        }
        self.wait_room_connected(&original, until).await?;
        self.room_mcp.as_mut().ok_or(Error::State)?.bound = true;
        Ok(())
    }
    async fn wait_room_connected(
        &mut self,
        original: &BTreeSet<&str>,
        until: Instant,
    ) -> Result<(), Error> {
        let mut poll = 0_u64;
        loop {
            self.wire.check(until)?;
            let id = format!("hagency-room-status-{poll}");
            let status = self
                .mcp_control(&id, json!({"subtype":"mcp_status"}), until)
                .await?;
            if !original.is_subset(&super::task_mcp::server_names(&status)?) {
                return Err(Error::RoomToolStartup);
            }
            let server = super::task_mcp::servers(&status)?
                .iter()
                .find(|s| s["name"] == SERVER)
                .ok_or(Error::RoomToolStartup)?;
            match server["status"].as_str() {
                Some("connected") => {
                    self.verify_room_server(server)?;
                    return Ok(());
                }
                Some("pending") => {}
                _ => return Err(Error::RoomToolStartup),
            }
            // Same original deadline, even if the native connection never settles.
            tokio::time::sleep_until((Instant::now() + Duration::from_millis(20)).min(until)).await;
            poll = poll.checked_add(1).ok_or(Error::Capacity)?;
        }
    }
    fn verify_room_server(&self, server: &Value) -> Result<(), Error> {
        let state = self.room_mcp.as_ref().ok_or(Error::State)?;
        if !state.ready
            || server["transport"] != "sdk"
            || server["scope"] != "dynamic"
            || server["source"] != "sdk"
        {
            return Err(Error::RoomToolStartup);
        }
        let expected: BTreeSet<String> = state
            .tools
            .definitions()
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect();
        let tools = server["tools"].as_array().ok_or(Error::RoomToolStartup)?;
        let actual = tools
            .iter()
            .map(|t| t["name"].as_str().ok_or(Error::RoomToolStartup))
            .collect::<Result<BTreeSet<_>, _>>()?;
        if tools.len() != expected.len() || actual != expected.iter().map(String::as_str).collect()
        {
            return Err(Error::RoomToolStartup);
        }
        Ok(())
    }
    pub(super) fn verify_retained_room_server(&self, status: &Value) -> Result<(), Error> {
        if self.room_mcp.is_none() {
            return Ok(());
        }
        let server = super::task_mcp::servers(status)?
            .iter()
            .find(|s| s["name"] == SERVER)
            .filter(|s| s["status"] == "connected")
            .ok_or(Error::RoomToolStartup)?;
        self.verify_room_server(server)
    }
    pub(super) async fn mcp_control(
        &mut self,
        id: &str,
        request: Value,
        until: Instant,
    ) -> Result<Value, Error> {
        self.wire
            .send(crate::claude::control(id, request)?, until)
            .await?;
        for _ in 0..64 {
            match self.wire.next(until).await?.message {
                Message::ControlResponse {
                    request_id,
                    outcome,
                } if request_id == id => {
                    return match outcome {
                        ControlOutcome::Success(value) => Ok(value),
                        ControlOutcome::Refused => Err(Error::Refused),
                    };
                }
                Message::Mcp {
                    request_id,
                    server_name,
                    message,
                } => {
                    self.reply_room_metadata(&request_id, &server_name, &message, until)
                        .await?;
                }
                _ => return Err(Error::Identity),
            }
        }
        Err(Error::Capacity)
    }
    pub(super) async fn reply_room_metadata(
        &mut self,
        id: &str,
        server: &str,
        message: &Value,
        until: Instant,
    ) -> Result<(), Error> {
        if server != SERVER {
            return Err(Error::Identity);
        }
        let state = self.room_mcp.as_mut().ok_or(Error::State)?;
        state.remember(id)?;
        // Never execute a tool during startup, probing or idle reuse.
        let result = state.metadata(message)?.ok_or(Error::State)?;
        self.wire
            .send(response(id, message.get("id"), result)?, until)
            .await?;
        Ok(())
    }
    /// As with approval writes, keep this same prepared frame across Message
    /// returns and recheck the host's current authority before resuming it.
    pub async fn send_prepared_room_reply(
        &mut self,
        prepared: &mut PreparedRoomReply,
    ) -> Result<PreparedUpdate, Error> {
        let expected = if self.phase == Phase::ResultObserved
            && self.wire.awaits_flush_only(&prepared.frame)
        {
            Phase::ResultObserved
        } else {
            Phase::Running
        };
        let operation = Operation::new(self, expected)?;
        let result = operation.driver.send_room_inner(prepared).await;
        operation.finish(result)
    }
    async fn send_room_inner(
        &mut self,
        prepared: &mut PreparedRoomReply,
    ) -> Result<PreparedUpdate, Error> {
        if self.interrupt.blocks_replies() {
            return Err(Error::RoomToolUnavailable);
        }
        if !Arc::ptr_eq(&self.source, &prepared.source) {
            return Err(Error::Identity);
        }
        let until = self.callback_deadline()?;
        self.wire.check(until)?;
        let pending = self
            .room_mcp
            .as_ref()
            .and_then(|s| s.pending.get(&prepared.id))
            .ok_or(Error::Identity)?;
        if !matches!(pending.stage, Stage::Prepared(sequence) if sequence == prepared.sequence) {
            return Err(Error::RoomToolUnavailable);
        }
        match self.wire.send_prepared(&mut prepared.frame, until).await? {
            io::Controlled::Message(received) => {
                self.observe(&received.message, received.at)?;
                Ok(PreparedUpdate::Message(received.message))
            }
            io::Controlled::Control(receipt) => {
                let pending = self
                    .room_mcp
                    .as_mut()
                    .and_then(|s| s.pending.get_mut(&prepared.id))
                    .ok_or(Error::Identity)?;
                pending.stage = Stage::Sent;
                pending.metadata = None;
                pending.request.operation = None;
                Ok(PreparedUpdate::WriteAccepted(receipt))
            }
        }
    }
}
