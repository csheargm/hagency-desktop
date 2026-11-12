//! Native MCP for both thread/start and thread/resume. A dynamic tool cannot be
//! added on resume in Codex 0.167. This endpoint preserves the original context.
//! Only turns observed on this process's app-server stream can invoke host IO.
use super::{Error, Result};
use crate::room_tools::{RoomQueue, RoomReply};
use hagency_runtime::{
    codex::tree::{Scope, TurnTree},
    room_tools::RoomTools,
};
use http_body_util::{BodyExt, Full, Limited};
use hyper::{
    body::{Bytes, Incoming},
    Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::{JoinHandle, JoinSet},
    time::Instant,
};

pub(super) const SERVER: &str = "hagency_matrix";
const MAX_BODY: usize = 16 * 1024;
const CALL_WAIT: Duration = Duration::from_secs(65);
type Key = (String, String);
type Shared = Arc<Mutex<Option<Active>>>;
struct Active {
    queue: RoomQueue,
    turns: BTreeMap<Key, watch::Sender<bool>>,
    calls: BTreeSet<(Key, String)>,
    context: BTreeMap<Key, hagency_runtime::codex::session::ContextUsage>,
}
pub(super) struct Bridge {
    config: Value,
    tools: RoomTools,
    state: Shared,
    server: JoinHandle<()>,
}
/// Dropping a turn (including cancellation) revokes every in-flight operation.
pub(super) struct Turn(Shared);
impl Drop for Turn {
    fn drop(&mut self) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}
impl Turn {
    pub(super) fn context(&self, evidence: hagency_runtime::codex::session::ContextUsage) {
        if let Some(active) = self.0.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            active.context.insert(
                (evidence.thread_id().into(), evidence.turn_id().into()),
                evidence,
            );
        }
    }
    pub(super) fn observe(&self, scope: &Scope, tree: &TurnTree) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(active) = state.as_mut() {
            active
                .turns
                .retain(|(thread, turn), _| tree.is_active(thread, turn));
            active
                .context
                .retain(|(thread, turn), _| tree.is_active(thread, turn));
            if tree.is_active(&scope.thread, &scope.turn) {
                active
                    .turns
                    .entry((scope.thread.clone(), scope.turn.clone()))
                    .or_insert_with(|| watch::channel(false).0);
            }
        }
    }
}
impl Bridge {
    pub(super) async fn new(tools: RoomTools) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let authority = listener.local_addr()?.to_string();
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| Error::Protocol("room capability unavailable"))?;
        let token = random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let mut config = json!({"url":format!("http://{authority}/matrix"),
            "http_headers":{"Authorization":format!("Bearer {token}")},
            "enabled":true,"required":true,"startup_timeout_sec":10,"tool_timeout_sec":75});
        for definition in tools.definitions() {
            config["tools"][definition["name"].as_str().expect("host tool name")] =
                json!({"approval_mode":"approve"});
        }
        let state: Shared = Arc::new(Mutex::new(None));
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            let connections = Arc::new(Semaphore::new(16));
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! { biased;
                    _ = tasks.join_next(), if !tasks.is_empty() => {},
                    accepted = listener.accept() => {
                        let Ok((socket, peer)) = accepted else { break; };
                        if !peer.ip().is_loopback() { continue; }
                        let Ok(permit) = connections.clone().try_acquire_owned() else { continue; };
                        let (state, token, authority) = (server_state.clone(), token.clone(), authority.clone());
                        tasks.spawn(async move {
                            let _permit = permit;
                            let service = hyper::service::service_fn(move |req| {
                                serve(req, state.clone(), token.clone(), authority.clone(), tools)
                            });
                            // No idle client can retain a connection indefinitely. The
                            // MCP client may reconnect; this endpoint is stateless.
                            let _ = tokio::time::timeout(Duration::from_secs(90),
                                hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(socket), service)).await;
                        });
                    }
                }
            }
        });
        Ok(Self {
            config,
            tools,
            state,
            server,
        })
    }
    pub(super) fn tools(&self) -> RoomTools {
        self.tools
    }
    pub(super) fn config(&self) -> Value {
        self.config.clone()
    }
    pub(super) fn begin(&self, queue: &RoomQueue, thread: &str, turn: &str) -> Result<Turn> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.is_some() {
            return Err(Error::Protocol("room turn already active"));
        }
        *state = Some(Active {
            queue: queue.clone(),
            turns: BTreeMap::from([((thread.into(), turn.into()), watch::channel(false).0)]),
            calls: BTreeSet::new(),
            context: BTreeMap::new(),
        });
        Ok(Turn(self.state.clone()))
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.server.abort();
    }
}
fn response(status: StatusCode, value: Option<Value>) -> Response<Full<Bytes>> {
    let bytes = value.map(|v| v.to_string()).unwrap_or_default();
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .header("cache-control", "no-store")
        .body(Full::new(Bytes::from(bytes)))
        .expect("static headers")
}
async fn serve(
    req: Request<Incoming>,
    state: Shared,
    token: String,
    authority: String,
    tools: RoomTools,
) -> std::result::Result<Response<Full<Bytes>>, Infallible> {
    if req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        != Some(format!("Bearer {token}").as_str())
        || req.headers().contains_key("origin")
        || req.headers().get("host").and_then(|v| v.to_str().ok()) != Some(authority.as_str())
    {
        return Ok(response(StatusCode::FORBIDDEN, None));
    }
    if req.uri().path() != "/matrix" || req.uri().query().is_some() {
        return Ok(response(StatusCode::NOT_FOUND, None));
    }
    if req.method() != hyper::Method::POST {
        return Ok(response(StatusCode::METHOD_NOT_ALLOWED, None));
    }
    let body = tokio::time::timeout(
        Duration::from_secs(5),
        Limited::new(req.into_body(), MAX_BODY).collect(),
    )
    .await;
    let Ok(Ok(body)) = body else {
        return Ok(response(StatusCode::PAYLOAD_TOO_LARGE, None));
    };
    let Ok(value) = serde_json::from_slice::<Value>(&body.to_bytes()) else {
        return Ok(response(StatusCode::BAD_REQUEST, None));
    };
    let id = value.get("id").cloned();
    if value["jsonrpc"] != "2.0"
        || !value["method"].is_string()
        || id.as_ref().is_some_and(|id| {
            !(id.is_i64() || id.is_u64() || id.as_str().is_some_and(|s| s.len() <= 256))
        })
    {
        return Ok(response(StatusCode::BAD_REQUEST, None));
    }
    if id.is_none() {
        return Ok(response(
            if value["method"] == "notifications/initialized" {
                StatusCode::ACCEPTED
            } else {
                StatusCode::BAD_REQUEST
            },
            None,
        ));
    }
    let result = match value["method"].as_str().expect("validated method") {
        "initialize" => Ok(
            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":SERVER,"version":"1"}}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":tools.definitions()})),
        "resources/list" => Ok(json!({"resources":[]})),
        "resources/templates/list" => Ok(json!({"resourceTemplates":[]})),
        "tools/call" => Ok(call(&state, tools, &value["params"]).await),
        _ => Err(json!({"code":-32601,"message":"Method not found"})),
    };
    Ok(response(
        StatusCode::OK,
        Some(match result {
            Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
        }),
    ))
}
fn text_result(reply: RoomReply) -> Value {
    json!({"isError":reply.is_error(),"content":[{"type":"text","text":reply.text()}]})
}
fn identity(params: &Value) -> Option<(Key, String)> {
    let meta = &params["_meta"];
    let source = &meta["x-codex-turn-metadata"];
    let valid = |v: &Value| {
        v.as_str()
            .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
            .map(str::to_owned)
    };
    let thread = valid(&source["thread_id"])?;
    let turn = valid(&source["turn_id"])?;
    let call = valid(&meta["callId"])?;
    if meta["threadId"] != thread || meta["itemId"] != call {
        return None;
    }
    Some(((thread, turn), call))
}
async fn call(state: &Shared, tools: RoomTools, params: &Value) -> Value {
    let denied = || text_result(RoomReply::unavailable());
    let Some((key, call)) = identity(params) else {
        return denied();
    };
    let Some(name) = params["name"].as_str() else {
        return denied();
    };
    let Ok(operation) = tools.parse(name, &params["arguments"]) else {
        return denied();
    };
    let admission = {
        let mut locked = state.lock().unwrap_or_else(|e| e.into_inner());
        locked.as_mut().and_then(|active| {
            let cancel = active.turns.get(&key)?.subscribe();
            let context = active.context.get(&key).cloned();
            if active.calls.len() >= hagency_runtime::codex::session::MAX_EVENTS
                || !active.calls.insert((key, call.clone()))
            {
                return None;
            }
            Some((active.queue.clone(), cancel, context))
        })
    };
    let Some((queue, mut cancel, context)) = admission else {
        return denied();
    };
    // The cancellation sender belongs to an observed native turn, not to the
    // HTTP connection or to a subsequent dispatch's room queue.
    tokio::select! { biased;
        _ = cancel.changed() => denied(),
        reply = queue.call_with_context(operation, call, Instant::now() + CALL_WAIT, context) => text_result(reply),
    }
}

#[cfg(test)]
mod tests;
