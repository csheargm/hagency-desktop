//! Closed user Matrix creation workflow. No model caller, AS token, room deletion
//! or automatic retry of an uncertain POST createRoom. The private command journal
//! is scoped to the native installation's pinned origin/MXID/Pasion subject.
use super::{OwnerError, OwnerOperation, OwnerReply, ServerLogin, http, origin, session_key};
use crate::console::{body, console, cookie, current, same_origin};
use reqwest::{Method, Url};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;
pub(in crate::console) mod service;
pub(super) use service::Scope;
const MARKER: &str = "im.hagency.client.creation";
static COMMANDS: Mutex<()> = Mutex::const_new(());
fn error(status: u16, code: &str) -> OwnerError {
    OwnerError::new(status, code)
}
pub(in crate::console) fn room_id(room: &str) -> Result<(), OwnerError> {
    if room.starts_with('!')
        && room.len() <= 255
        && !room
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || ['/', '?', '#', '\\'].contains(&c))
    {
        Ok(())
    } else {
        Err(error(400, "invalid_room_id"))
    }
}
pub(super) fn segment(room: &str) -> Result<String, OwnerError> {
    room_id(room)?;
    let mut url = Url::parse("https://matrix.invalid/").unwrap();
    url.path_segments_mut().unwrap().push(room);
    Ok(url.path()[1..].into())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    AgentRoom,
    Space,
    Room,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Input {
    pub command_id: String,
    pub kind: Kind,
    pub name: String,
    pub project_id: Option<String>,
    pub space_id: Option<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Command {
    input: Input,
    origin: String,
    owner: String,
    subject: String,
    room_id: Option<String>,
    phase: String,
    project: Option<Value>,
    last_error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Resume {
    pub room_id: Option<String>,
}
// This enum is deliberately private: browser input never becomes arbitrary paths,
// state types, request bodies, authorization headers, or a remote proxy.
pub(in crate::console) enum MatrixOperation {
    Managed(crate::console::native::project_setup::matrix::Operation),
    DirectCreate {
        agent: String,
        owner: String,
        puppet: String,
        name: String,
    },
    DirectAlias {
        agent: String,
        owner: String,
    },
    DirectIndexGet(String),
    DirectIndexPut(String, Value),
    Create(Input, String, String),
    Discovery,
    JoinedRooms,
    State(String),
    InviteService { room: String, owner: String, discovery: Value },
    Child(String, String, Value),
    Parent(String, String, Value),
}
impl MatrixOperation {
    fn request(self) -> Result<(Method, String, Option<Value>), OwnerError> {
        Ok(match self {
            Self::Managed(operation) => return operation.request(),
            Self::DirectCreate {
                agent,
                owner,
                puppet,
                name,
            } => {
                super::operation_id(&agent)?;
                let server = owner
                    .split_once(':')
                    .filter(|(local, server)| local.starts_with('@') && !server.is_empty())
                    .ok_or_else(|| error(400, "invalid_arguments"))?
                    .1;
                if !puppet.starts_with('@')
                    || puppet.split_once(':').map(|(_, s)| s) != Some(server)
                {
                    return Err(error(400, "invalid_arguments"));
                }
                (
                    Method::POST,
                    "/_matrix/client/v3/createRoom".into(),
                    Some(
                        json!({"name":name,"visibility":"private","preset":"private_chat","is_direct":true,"room_alias_name":format!("hagency-agent-{agent}"),"invite":[puppet],"initial_state":[{"type":"m.room.history_visibility","state_key":"","content":{"history_visibility":"joined"}},{"type":"m.room.guest_access","state_key":"","content":{"guest_access":"forbidden"}},{"type":"m.room.join_rules","state_key":"","content":{"join_rule":"invite"}},{"type":"im.hagency.agent.owner_direct","state_key":"","content":{"version":1,"agentId":agent,"ownerMxid":owner}}]}),
                    ),
                )
            }
            Self::DirectAlias { agent, owner } => {
                super::operation_id(&agent)?;
                let server = owner
                    .split_once(':')
                    .filter(|(local, server)| local.starts_with('@') && !server.is_empty())
                    .ok_or_else(|| error(400, "invalid_arguments"))?
                    .1;
                let alias = format!("#hagency-agent-{agent}:{server}");
                let mut url = Url::parse("https://matrix.invalid/").unwrap();
                url.path_segments_mut().unwrap().push(&alias);
                (
                    Method::GET,
                    format!("/_matrix/client/v3/directory/room/{}", &url.path()[1..]),
                    None,
                )
            }
            Self::DirectIndexGet(owner) => {
                let mut url = Url::parse("https://matrix.invalid/").unwrap();
                url.path_segments_mut().unwrap().push(&owner);
                (
                    Method::GET,
                    format!(
                        "/_matrix/client/v3/user/{}/account_data/m.direct",
                        &url.path()[1..]
                    ),
                    None,
                )
            }
            Self::DirectIndexPut(owner, index) => {
                if !index.is_object() {
                    return Err(error(400, "invalid_arguments"));
                }
                let mut url = Url::parse("https://matrix.invalid/").unwrap();
                url.path_segments_mut().unwrap().push(&owner);
                (
                    Method::PUT,
                    format!(
                        "/_matrix/client/v3/user/{}/account_data/m.direct",
                        &url.path()[1..]
                    ),
                    Some(index),
                )
            }
            Self::Create(input, owner, service) => {
                let marker = json!({"commandId":input.command_id,"owner":owner,"kind":input.kind,"projectId":input.project_id,"spaceId":input.space_id});
                let mut body = json!({"name":input.name,"visibility":"private","preset":"private_chat","invite":[service],"initial_state":[{"type":"m.room.join_rules","state_key":"","content":{"join_rule":"invite"}},{"type":MARKER,"state_key":"","content":marker}]});
                if input.kind == Kind::AgentRoom {
                    body["initial_state"].as_array_mut().unwrap().extend([
                        json!({"type":"m.room.encryption","state_key":"","content":{"algorithm":"m.megolm.v1.aes-sha2"}}),
                        json!({"type":"m.room.history_visibility","state_key":"","content":{"history_visibility":"joined"}}),
                        json!({"type":"m.room.guest_access","state_key":"","content":{"guest_access":"forbidden"}}),
                    ]);
                }
                if input.kind == Kind::Space {
                    body["creation_content"] = json!({"type":"m.space"});
                }
                (
                    Method::POST,
                    "/_matrix/client/v3/createRoom".into(),
                    Some(body),
                )
            }
            Self::Discovery => (Method::GET, "/hagency/api/v1/discovery".into(), None),
            Self::InviteService { room, owner, discovery } => (
                Method::POST,
                format!("/_matrix/client/v3/rooms/{}/invite", segment(&room)?),
                Some(json!({"user_id": service_identity(&discovery, &owner)?})),
            ),
            Self::JoinedRooms => (Method::GET, "/_matrix/client/v3/joined_rooms".into(), None),
            Self::State(room) => (
                Method::GET,
                format!("/_matrix/client/v3/rooms/{}/state", segment(&room)?),
                None,
            ),
            Self::Child(space, room, content) => (
                Method::PUT,
                format!(
                    "/_matrix/client/v3/rooms/{}/state/m.space.child/{}",
                    segment(&space)?,
                    segment(&room)?
                ),
                Some(content),
            ),
            Self::Parent(room, space, content) => (
                Method::PUT,
                format!(
                    "/_matrix/client/v3/rooms/{}/state/m.space.parent/{}",
                    segment(&room)?,
                    segment(&space)?
                ),
                Some(content),
            ),
        })
    }
}
impl ServerLogin {
    /// Discover only Rooms returned by this user's OAuth joined_rooms endpoint.
    /// The cursor is the last scanned ID, including failures/non-Space Rooms.
    pub(crate) async fn space_candidates(
        &self,
        cookie: &str,
        cursor: Option<String>,
    ) -> Result<Value, OwnerError> {
        let projects = self.owner_api(cookie, OwnerOperation::Projects).await?;
        let joined = self
            .matrix_api(cookie, MatrixOperation::JoinedRooms)
            .await?;
        if !same_reply_identity(&projects, &joined) {
            return Err(error(401, "sign_in_required"));
        }
        let ids = candidate_ids(&joined.value, cursor.as_deref())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut spaces = Vec::new();
        let mut errors = Vec::new();
        let mut scanned = 0;
        let mut last = None;
        for id in &ids {
            if scanned >= 128 || spaces.len() >= 32 || Instant::now() >= deadline {
                break;
            }
            scanned += 1;
            last = Some(id.clone());
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                self.matrix_api(cookie, MatrixOperation::State(id.clone())),
            )
            .await;
            match result {
                Ok(Ok(reply)) => {
                    if !same_reply_identity(&projects, &reply) {
                        return Err(error(401, "sign_in_required"));
                    }
                    match candidate_space(id, &reply.value, &projects.owner, &projects.value) {
                        Ok(Some(space)) => spaces.push(space),
                        Ok(None) => {}
                        Err(e) => errors.push(json!({"roomId":id,"code":e.code})),
                    }
                }
                Ok(Err(e)) if e.status == 401 => return Err(e),
                Ok(Err(e)) => errors.push(json!({"roomId":id,"code":e.code})),
                Err(_) => errors.push(json!({"roomId":id,"code":"matrix_state_timeout"})),
            }
        }
        let check = self.owner_api(cookie, OwnerOperation::Projects).await?;
        if !same_reply_identity(&projects, &check) {
            return Err(error(401, "sign_in_required"));
        }
        let next = (scanned < ids.len()).then_some(last).flatten();
        Ok(
            json!({"spaces":spaces,"nextCursor":next,"incomplete":!errors.is_empty(),"errors":errors}),
        )
    }
    pub(in crate::console) async fn matrix_api(
        &self,
        cookie: &str,
        op: MatrixOperation,
    ) -> Result<OwnerReply, OwnerError> {
        self.matrix_api_scoped(cookie, op, None).await
    }
    async fn matrix_api_scoped(
        &self,
        cookie: &str,
        op: MatrixOperation,
        scope: Option<&Scope<'_>>,
    ) -> Result<OwnerReply, OwnerError> {
        let (method, path, body) = op.request()?;
        if self.stopped.load(Ordering::Acquire) {
            return Err(error(401, "sign_in_required"));
        }
        let renewal = self.renewal.lock().await;
        let key = session_key(cookie);
        self.renew_session_inner(&key).await.map_err(|failure| {
            let unavailable = matches!(failure, super::Error::Unavailable);
            error(
                if unavailable { 503 } else { 401 },
                if unavailable {
                    "matrix_authorization_unavailable"
                } else {
                    "sign_in_required"
                },
            )
        })?;
        let session = self
            .sessions
            .lock()
            .await
            .get(&key)
            .cloned()
            .filter(|s| !s.invalidated && s.expires > Instant::now())
            .ok_or_else(|| error(401, "sign_in_required"))?;
        drop(renewal);
        if let Some(scope) = scope {
            scope.matches(&session)?;
            scope.check().await?;
        }
        let shared_token = if let Some(source) = &session.matrix_source {
            let snapshot = source.access_token().await.map_err(|failure| {
                let code = super::super::native::sdk_source_failure(failure);
                error(
                    if code.ends_with("unavailable") {
                        503
                    } else {
                        401
                    },
                    code,
                )
            })?;
            if snapshot.client_id != session.binding.client_id {
                return Err(error(401, "matrix_account_changed"));
            }
            Some(snapshot.access_token)
        } else {
            None
        };
        if let Some(scope) = scope {
            scope.check().await?;
        }
        let server = origin(&session.binding.origin).map_err(|_| error(401, "sign_in_required"))?;
        let mut request = http()
            .map_err(|_| error(503, "matrix_unavailable"))?
            .request(method, server.join(&path).unwrap())
            .bearer_auth(shared_token.as_ref().unwrap_or(&session.oauth_token));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut response = request
            .send()
            .await
            .map_err(|_| error(503, "matrix_request_outcome_unknown"))?;
        let status = response.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error(503, "matrix_request_outcome_unknown"))?
        {
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(error(502, "invalid_matrix_response"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| error(502, "invalid_matrix_response"))?;
        if !status.is_success() {
            return Err(error(
                status.as_u16(),
                if status.as_u16() == 403 {
                    "matrix_permission_denied"
                } else {
                    "matrix_request_failed"
                },
            ));
        }
        if let Some(source) = &session.matrix_source {
            let after = source.access_token().await.map_err(|failure| {
                let code = super::super::native::sdk_source_failure(failure);
                error(
                    if code.ends_with("unavailable") {
                        503
                    } else {
                        401
                    },
                    code,
                )
            })?;
            if after.client_id != session.binding.client_id {
                return Err(error(401, "matrix_account_changed"));
            }
        }
        if let Some(scope) = scope {
            scope.check().await?;
        }
        let sessions = self.sessions.lock().await;
        let current = sessions.values().find(|s| s.token == session.token);
        if current.is_none_or(|s| {
            s.invalidated || s.expires <= Instant::now() || s.oauth_expires <= Instant::now()
        }) || self.stopped.load(Ordering::Acquire)
        {
            return Err(error(401, "sign_in_required"));
        }
        Ok(OwnerReply {
            owner: session
                .binding
                .owner
                .clone()
                .ok_or_else(|| error(401, "sign_in_required"))?,
            origin: session.binding.origin.clone(),
            issuer: format!("{}auth/", session.binding.origin),
            subject: session
                .binding
                .subject
                .clone()
                .ok_or_else(|| error(401, "sign_in_required"))?,
            value,
        })
    }
}
fn load(path: &Path) -> Result<Vec<Command>, OwnerError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = super::read_private_json(path, 1024 * 1024)
        .map_err(|_| error(503, "local_creation_journal_unavailable"))?;
    let records: Vec<Command> = serde_json::from_slice(&bytes)
        .map_err(|_| error(503, "local_creation_journal_unavailable"))?;
    if records.len() > 256 {
        return Err(error(503, "local_creation_journal_full"));
    }
    Ok(records)
}
fn save(path: &Path, records: &[Command]) -> Result<(), OwnerError> {
    let bytes = serde_json::to_vec(records)
        .map_err(|_| error(503, "local_creation_journal_unavailable"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(error(503, "local_creation_journal_full"));
    }
    hagency_store::private::replace(path, &bytes)
        .map_err(|_| error(503, "local_creation_journal_unavailable"))
}
pub(super) fn service_identity(discovery: &Value, owner: &str) -> Result<String, OwnerError> {
    let server = owner
        .split_once(':')
        .filter(|(local, server)| local.starts_with('@') && !server.is_empty())
        .ok_or_else(|| error(502, "invalid_owner_identity"))?
        .1;
    let service = format!("@_hagency_service:{server}");
    if discovery["serverName"] != server || discovery["serviceMxid"] != service {
        return Err(error(502, "invalid_service_identity"));
    }
    Ok(service)
}
fn validate_input(input: &Input) -> Result<(), OwnerError> {
    super::operation_id(&input.command_id)?;
    if input.name.trim().is_empty()
        || input.name.chars().count() > 128
        || input.name.chars().any(char::is_control)
    {
        return Err(error(400, "invalid_arguments"));
    }
    match input.kind {
        Kind::Space | Kind::AgentRoom if input.project_id.is_none() && input.space_id.is_none() => Ok(()),
        Kind::Room => {
            super::operation_id(
                input
                    .project_id
                    .as_deref()
                    .ok_or_else(|| error(400, "invalid_arguments"))?,
            )?;
            room_id(
                input
                    .space_id
                    .as_deref()
                    .ok_or_else(|| error(400, "invalid_arguments"))?,
            )
        }
        _ => Err(error(400, "invalid_arguments")),
    }
}
fn content<'a>(state: &'a Value, kind: &str, key: &str) -> Option<&'a Value> {
    state
        .as_array()?
        .iter()
        .find(|e| e["type"] == kind && e["state_key"] == key)
        .map(|e| &e["content"])
}
fn created_by(state: &Value, command: &Command) -> bool {
    let Some(events) = state.as_array() else {
        return false;
    };
    let mut creates = events
        .iter()
        .filter(|e| e["type"] == "m.room.create" && e["state_key"] == "");
    let mut markers = events
        .iter()
        .filter(|e| e["type"] == MARKER && e["state_key"] == "");
    let (Some(create), Some(marker)) = (creates.next(), markers.next()) else {
        return false;
    };
    if creates.next().is_some() || markers.next().is_some() {
        return false;
    }
    let creator = create["content"]
        .get("creator")
        .is_none_or(|v| v == &command.owner);
    create["sender"] == command.owner
        && creator
        && marker["sender"] == command.owner
        && marker["content"]
            == json!({"commandId":command.input.command_id,"owner":command.owner,"kind":command.input.kind,"projectId":command.input.project_id,"spaceId":command.input.space_id})
        && (create["content"]["type"] == "m.space") == (command.input.kind == Kind::Space)
        && content(state, "m.room.join_rules", "").is_some_and(|c| c["join_rule"] == "invite")
}
async fn checked_project(
    login: &ServerLogin,
    cookie: &str,
    input: &Input,
    scope: Option<&Scope<'_>>,
) -> Result<(), OwnerError> {
    if input.kind == Kind::Room {
        let projects = login
            .owner_api_inner(Some(cookie), None, OwnerOperation::Projects, scope)
            .await?;
        if !projects.value["projects"].as_array().is_some_and(|p| {
            p.iter().any(|p| {
                Some(p["id"].as_str().unwrap_or("")) == input.project_id.as_deref()
                    && Some(p["spaceId"].as_str().unwrap_or("")) == input.space_id.as_deref()
            })
        }) {
            return Err(error(403, "project_space_mismatch"));
        }
    }
    Ok(())
}
async fn continue_known(
    login: &ServerLogin,
    cookie: &str,
    command: &mut Command,
    scope: Option<&Scope<'_>>,
) -> Result<(), OwnerError> {
    checked_project(login, cookie, &command.input, scope).await?;
    let room = command
        .room_id
        .clone()
        .ok_or_else(|| error(409, "matrix_creation_outcome_unknown"))?;
    let state = login
        .matrix_api_scoped(cookie, MatrixOperation::State(room.clone()), scope)
        .await?
        .value;
    if !created_by(&state, command) {
        return Err(error(409, "matrix_creation_proof_mismatch"));
    }
    if command.input.kind == Kind::Space {
        command.project = Some(
            login
                .owner_api_inner(
                    Some(cookie),
                    None,
                    OwnerOperation::AdoptProject {
                        space: room,
                        project_folder: None,
                    },
                    scope,
                )
                .await?
                .value["project"]
                .clone(),
        );
    } else if command.input.kind == Kind::Room {
        let space = command.input.space_id.clone().unwrap();
        let space_state = login
            .matrix_api_scoped(cookie, MatrixOperation::State(space.clone()), scope)
            .await?
            .value;
        let host = command
            .owner
            .split_once(':')
            .ok_or_else(|| error(502, "invalid_owner_identity"))?
            .1;
        let child = json!({"via":[host],"suggested":false});
        if let Some(existing) = content(&space_state, "m.space.child", &room) {
            if existing != &child {
                return Err(error(409, "matrix_space_child_conflict"));
            }
        } else {
            login
                .matrix_api_scoped(
                    cookie,
                    MatrixOperation::Child(space.clone(), room.clone(), child),
                    scope,
                )
                .await?;
        }
        let parent = json!({"via":[host],"canonical":true});
        if let Some(existing) = content(&state, "m.space.parent", &space) {
            if existing != &parent {
                return Err(error(409, "matrix_space_parent_conflict"));
            }
        } else {
            login
                .matrix_api_scoped(
                    cookie,
                    MatrixOperation::Parent(room.clone(), space, parent),
                    scope,
                )
                .await?;
        }
        login
            .owner_api_inner(
                Some(cookie),
                None,
                OwnerOperation::AdoptRoom {
                    project: command.input.project_id.clone().unwrap(),
                    room,
                },
                scope,
            )
            .await?;
    }
    command.phase = "complete".into();
    command.last_error = None;
    Ok(())
}
pub(crate) fn router() -> Router {
    Router::with_path("matrix-creations")
        .get(creations)
        .post(start)
        .push(
            Router::with_path("{commandId}")
                .get(creation_status)
                .push(Router::with_path("resume").post(resume)),
        )
}
fn context<'a>(
    req: &Request,
    depot: &'a Depot,
    mutating: bool,
) -> Result<(&'a crate::console::Console, String), OwnerError> {
    if req.uri().query().is_some() || !same_origin(req, depot, mutating) {
        return Err(error(403, "invalid_origin"));
    }
    current(req, depot).map_err(|_| error(401, "sign_in_required"))?;
    let console = console(depot).map_err(|_| error(503, "local_state_unavailable"))?;
    let cookie = cookie(req)
        .map_err(|_| error(401, "sign_in_required"))?
        .to_owned();
    Ok((console, cookie))
}
fn finish<T: Serialize + Send>(res: &mut Response, value: Result<T, OwnerError>) {
    match value {
        Ok(value) => res.render(Json(value)),
        Err(error) => {
            res.status_code(
                StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );
            res.render(Json(json!({"code":error.code})));
        }
    }
}
#[handler]
async fn start(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let result = async {
        let bytes = body(req, 16384)
            .await
            .map_err(|_| error(400, "invalid_arguments"))?;
        let input = serde_json::from_slice(&bytes).map_err(|_| error(400, "invalid_arguments"))?;
        let (console, cookie) = context(req, depot, true)?;
        service::invoke(console, &cookie, service::Action::Start(input), None).await
    }
    .await;
    finish(res, result);
}
#[handler]
async fn creation_status(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let result = async {
        let id = req
            .param::<String>("commandId")
            .ok_or_else(|| error(400, "invalid_arguments"))?;
        let (console, cookie) = context(req, depot, false)?;
        service::invoke(console, &cookie, service::Action::Status(id), None).await
    }
    .await;
    finish(res, result);
}
#[handler]
async fn resume(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let result = async {
        let raw = body(req, 4096)
            .await
            .map_err(|_| error(400, "invalid_arguments"))?;
        let input = if raw.is_empty() {
            Resume::default()
        } else {
            serde_json::from_slice(&raw).map_err(|_| error(400, "invalid_arguments"))?
        };
        let id = req
            .param::<String>("commandId")
            .ok_or_else(|| error(400, "invalid_arguments"))?;
        let (console, cookie) = context(req, depot, true)?;
        service::invoke(
            console,
            &cookie,
            service::Action::Resume { id, input },
            None,
        )
        .await
    }
    .await;
    finish(res, result);
}

#[cfg(test)]
pub(super) mod tests;

pub(super) fn same_reply_identity(a: &OwnerReply, b: &OwnerReply) -> bool {
    a.origin == b.origin && a.owner == b.owner && a.issuer == b.issuer && a.subject == b.subject
}
fn candidate_ids(value: &Value, cursor: Option<&str>) -> Result<Vec<String>, OwnerError> {
    let rooms = value["joined_rooms"]
        .as_array()
        .ok_or_else(|| error(502, "invalid_matrix_response"))?;
    if rooms.len() > 4096 {
        return Err(error(409, "space_discovery_capacity_exceeded"));
    }
    if let Some(cursor) = cursor {
        room_id(cursor)?;
    }
    let mut ids = std::collections::BTreeSet::new();
    for room in rooms {
        let id = room
            .as_str()
            .ok_or_else(|| error(502, "invalid_matrix_response"))?;
        room_id(id)?;
        ids.insert(id.to_owned());
    }
    Ok(ids
        .into_iter()
        .filter(|id| cursor.is_none_or(|c| id.as_str() > c))
        .collect())
}
fn candidate_space(
    id: &str,
    state: &Value,
    owner: &str,
    projects: &Value,
) -> Result<Option<Value>, OwnerError> {
    let state = state
        .as_array()
        .ok_or_else(|| error(502, "invalid_matrix_response"))?;
    let content = |kind: &str, key: &str| {
        state
            .iter()
            .find(|e| e["type"] == kind && e["state_key"] == key)
            .and_then(|e| e.get("content"))
    };
    if !content("m.room.create", "").is_some_and(|c| c["type"] == "m.space")
        || !content("m.room.member", owner).is_some_and(|c| c["membership"] == "join")
    {
        return Ok(None);
    }
    let name = content("m.room.name", "").and_then(|c| c["name"].as_str());
    let topic = content("m.room.topic", "").and_then(|c| c["topic"].as_str());
    if name.is_some_and(|s| s.len() > 1024) || topic.is_some_and(|s| s.len() > 4096) {
        return Err(error(502, "invalid_matrix_response"));
    }
    let project = projects["projects"]
        .as_array()
        .ok_or_else(|| error(502, "invalid_server_response"))?
        .iter()
        .find(|p| p["spaceId"] == id)
        .and_then(|p| p["id"].as_str());
    Ok(Some(
        json!({"spaceId":id,"name":name,"topic":topic,"projectId":project}),
    ))
}

#[handler]
async fn creations(req: &Request, depot: &Depot, res: &mut Response) {
    let result = async {
        let (console, cookie) = context(req, depot, false)?;
        service::invoke(console, &cookie, service::Action::List, None).await
    }
    .await;
    finish(res, result);
}
