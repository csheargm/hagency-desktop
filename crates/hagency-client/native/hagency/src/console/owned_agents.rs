//! Owner-scoped agent administration and local resource policy. OAuth/session
//! credentials never leave the host, and local policy writes never ask an admin.
use super::server_login::{OwnerError, OwnerOperation, OwnerReply};
use super::{Error, body, console, cookie, current, recheck};
use crate::native_owner::RuntimeStartRequest as StartRuntime;
use hagency_agent_local::{
    Budget, Layer, Ledger, Limit, ModelProfile, Period, Policy, RequestPolicy, Scope, ToolPolicy,
};
use salvo::{http::Method, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
mod commands;
mod direct;
#[cfg(test)]
mod profile_directory_tests;

pub(super) fn router() -> Router {
    Router::with_path("owned-agents")
        .goal(dispatch)
        .push(Router::with_path("{**path}").goal(dispatch))
}
fn error(status: u16, code: &str) -> OwnerError {
    OwnerError {
        status,
        code: code.into(),
    }
}
fn local_error(error: hagency_agent_local::Error) -> OwnerError {
    use hagency_agent_local::Error::*;
    match error {
        Conflict => self::error(409, "local_policy_conflict"),
        Invalid(_) => self::error(400, "invalid_local_policy"),
        Unauthorized => self::error(403, "owner_scope_required"),
        ForeignDatabase => self::error(409, "foreign_local_database"),
        _ => self::error(503, "local_state_unavailable"),
    }
}
fn key(value: &str) -> Result<(), OwnerError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        Err(error(400, "invalid_arguments"))
    } else {
        Ok(())
    }
}
fn matrix(value: &str, prefix: char) -> Result<(), OwnerError> {
    if value.len() <= 255
        && value.starts_with(prefix)
        && value[1..]
            .split_once(':')
            .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        Ok(())
    } else {
        Err(error(400, "invalid_arguments"))
    }
}
async fn parse<T: serde::de::DeserializeOwned>(req: &mut Request) -> Result<T, OwnerError> {
    serde_json::from_slice(
        &body(req, 16384)
            .await
            .map_err(|_| error(400, "invalid_arguments"))?,
    )
    .map_err(|_| error(400, "invalid_arguments"))
}
async fn api(req: &Request, depot: &Depot, op: OwnerOperation) -> Result<OwnerReply, OwnerError> {
    current(req, depot).map_err(|_| error(401, "sign_in_required"))?;
    console(depot)
        .map_err(|_| error(503, "local_state_unavailable"))?
        .owner_api(cookie(req).map_err(|_| error(401, "sign_in_required"))?, op)
        .await
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Create {
    display_name: String,
    idempotency_key: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Bind {
    project_id: String,
    room_id: String,
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selection {
    binding_id: String,
    requester: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolDecision {
    args_digest: String,
    approved: bool,
}
fn default_effort() -> String {
    "medium".into()
}
fn runtime_error(e: super::owned_runtime::RuntimeError) -> OwnerError {
    use super::owned_runtime::RuntimeError::*;
    match e {
        Approval => error(409, "invalid_tool_approval"),
        Authorization => error(401, "owner_authorization_required"),
        AlreadyActive => error(409, "agent_runtime_already_active"),
        LeaseBusy => error(409, "agent_lease_busy"),
        ExecutionDevice => error(409, "agent_execution_device_required"),
        LedgerRecovery => error(409, "ledger_recovery_required"),
        Profile => error(409, "runtime_profile_unavailable"),
        Provider => error(401, "provider_authorization_lost"),
        Execution(code) => error(503, code),
        Transport => error(503, "device_transport_unavailable"),
        Request => error(409, "execution_not_running"),
        Stopped => error(409, "runtime_stopped"),
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditPolicy {
    binding_id: String,
    requester: String,
    layer: String,
    expected_revision: i64,
    policy: Policy,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetPolicy {
    binding_id: String,
    requester: String,
    layer: String,
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EditAgentPolicy {
    expected_revision: i64,
    policy: Policy,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResetAgentPolicy {
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditAgentModel {
    profile: ModelProfile,
    #[serde(rename="expectedProfileDigest")]
    expected_profile_digest:String,
}
fn layer(value: &str) -> Result<Layer, OwnerError> {
    match value {
        "agent" => Ok(Layer::Agent),
        "room" => Ok(Layer::Room),
        "requester" => Ok(Layer::Requester),
        _ => Err(error(400, "invalid_arguments")),
    }
}
fn selection(req: &Request) -> Result<Selection, OwnerError> {
    if req.uri().query().is_some_and(|q| q.len() > 1024) {
        return Err(error(400, "invalid_arguments"));
    }
    let queries = req.queries();
    for (k, _) in queries.iter() {
        if !["bindingId", "requester"].contains(&k.as_str())
            || queries.get_vec(k).is_none_or(|v| v.len() != 1)
        {
            return Err(error(400, "invalid_arguments"));
        }
    }
    Ok(Selection {
        binding_id: req
            .query::<String>("bindingId")
            .ok_or_else(|| error(400, "invalid_arguments"))?,
        requester: req
            .query::<String>("requester")
            .ok_or_else(|| error(400, "invalid_arguments"))?,
    })
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerBinding {
    id: String,
    agent_id: String,
    project_id: Option<String>,
    scope_kind: String,
    room_id: String,
    state: String,
    generation: i64,
    #[serde(default)]
    owner_service_paused: bool,
    #[serde(default)]
    thread_auto_reply: bool,
}
fn required_nullable_device<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ServerAgent {
    #[serde(deserialize_with = "required_nullable_device")]
    pub execution_device_id: Option<String>,
    owner_direct_room_id: Option<String>,
    pub id: String,
    pub owner_user_id: String,
    puppet_mxid: String,
    display_name: String,
    pub state: String,
    pub generation: i64,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerProject {
    id: String,
    space_id: String,
    active: bool,
    revision: i64,
}
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerDevice {
    id: String,
    name: String,
    generation: i64,
    revoked: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AssignExecutionDevice {
    expected_generation: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OwnerDirect {
    room_id: String,
}
fn public(value: &Value, kind: &str) -> Result<Value, OwnerError> {
    let invalid = || error(502, "invalid_server_response");
    match kind {
        "devices" => {
            let devices: Vec<ServerDevice> =
                serde_json::from_value(value["devices"].clone()).map_err(|_| invalid())?;
            Ok(json!({"devices":devices}))
        }
        "agents" => {
            let agents: Vec<ServerAgent> =
                serde_json::from_value(value["agents"].clone()).map_err(|_| invalid())?;
            Ok(json!({"agents":agents}))
        }
        "projects" => {
            let projects: Vec<ServerProject> =
                serde_json::from_value(value["projects"].clone()).map_err(|_| invalid())?;
            Ok(json!({"projects":projects}))
        }
        "bindings" => {
            let bindings: Vec<ServerBinding> =
                serde_json::from_value(value["bindings"].clone()).map_err(|_| invalid())?;
            Ok(json!({"bindings":bindings}))
        }
        "creation" => {
            let agent: ServerAgent = serde_json::from_value(value["creation"]["agent"].clone())
                .map_err(|_| invalid())?;
            let binding: Option<ServerBinding> =
                serde_json::from_value(value["creation"]["binding"].clone())
                    .map_err(|_| invalid())?;
            let state = binding
                .as_ref()
                .map(|b| b.state.as_str())
                .unwrap_or(&agent.state);
            let command_state = if matches!(state, "joining" | "provisioning" | "creating") {
                "pending"
            } else {
                state
            }
            .to_owned();
            // Provisioning exposes only audited static protocol error codes.
            let pending_reason = value["pendingReason"].as_str().filter(|code| {
                matches!(
                    *code,
                    "matrix_identity_mismatch"
                        | "matrix_permission_missing"
                        | "invalid_matrix_origin"
                        | "invalid_puppet_identity"
                        | "invalid_matrix_response"
                        | "matrix_outcome_unknown"
                        | "matrix_request_failed"
                        | "matrix_rate_limited"
                        | "matrix_response_too_large"
                        | "matrix_unavailable"
                        | "storage_unavailable"
                )
            });
            let mut creation = json!({"agent":agent});
            if let Some(binding) = binding {
                creation["binding"] = serde_json::to_value(binding).map_err(|_| invalid())?;
            }
            Ok(
                json!({"creation":creation,"commandState":command_state,"pendingReason":pending_reason}),
            )
        }
        "binding" => {
            let binding: ServerBinding =
                serde_json::from_value(value["binding"].clone()).map_err(|_| invalid())?;
            Ok(json!({"binding":binding}))
        }
        "agent" => {
            let agent: ServerAgent =
                serde_json::from_value(value["agent"].clone()).map_err(|_| invalid())?;
            Ok(json!({"agent":agent}))
        }
        _ => Err(invalid()),
    }
}
async fn require_execution_device(
    req: &Request,
    depot: &Depot,
    agent: &str,
) -> Result<(), OwnerError> {
    let reply = api(
        req,
        depot,
        OwnerOperation::Agent {
            agent: agent.into(),
        },
    )
    .await?;
    let device = console(depot)
        .map_err(|_| error(503, "local_state_unavailable"))?
        .authorized_device()
        .await
        .map_err(|_| error(401, "owner_authorization_required"))?;
    if reply.owner != device.owner_mxid()
        || reply.origin != device.origin()
        || reply.issuer != device.issuer()
        || reply.subject != device.subject()
    {
        return Err(error(401, "owner_authorization_required"));
    }
    let record: ServerAgent = serde_json::from_value(reply.value["agent"].clone())
        .map_err(|_| error(502, "invalid_server_response"))?;
    if record.id != agent || record.owner_user_id != device.user_id() {
        return Err(error(403, "owner_scope_required"));
    }
    if record.execution_device_id.as_deref() != Some(device.device_id()) || record.generation <= 0 {
        return Err(error(409, "agent_execution_device_required"));
    }
    recheck(depot).map_err(|_| error(401, "sign_in_required"))
}
async fn verified(
    req: &Request,
    depot: &Depot,
    agent: &str,
    selected: &Selection,
) -> Result<(OwnerReply, Scope, PathBuf), OwnerError> {
    key(agent)?;
    key(&selected.binding_id)?;
    matrix(&selected.requester, '@')?;
    let reply = api(
        req,
        depot,
        OwnerOperation::Bindings {
            agent: agent.into(),
        },
    )
    .await?;
    let bindings: Vec<ServerBinding> = serde_json::from_value(reply.value["bindings"].clone())
        .map_err(|_| error(502, "invalid_server_response"))?;
    let binding = bindings
        .into_iter()
        .find(|b| b.id == selected.binding_id && b.agent_id == agent)
        .ok_or_else(|| error(403, "owner_scope_required"))?;
    matrix(&binding.room_id, '!')?;
    let root = console(depot)
        .map_err(|_| error(503, "local_state_unavailable"))?
        .0
        .server_login
        .state_directory()
        .ok_or_else(|| error(503, "local_state_unavailable"))?;
    Ok((
        reply,
        Scope {
            agent: agent.into(),
            binding: binding.id,
            room: binding.room_id,
            requester: selected.requester.clone(),
            thread: "policy-editor".into(),
        },
        root,
    ))
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct LocalProfileIdentity {
    version: u32,
    origin: String,
    issuer: String,
    subject: String,
    owner: String,
}
pub(crate) fn profile_identity(
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
) -> Result<String, Error> {
    if origin.len() > 2048
        || issuer != format!("{origin}auth/")
        || subject.is_empty()
        || subject.len() > 4096
        || subject.chars().any(char::is_control)
        || owner.len() > 255
        || !owner.starts_with('@')
        || !owner.contains(':')
        || owner.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(Error::Unauthorized);
    }
    let parsed = reqwest::Url::parse(origin).map_err(|_| Error::Unauthorized)?;
    if parsed.as_str() != origin
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return Err(Error::Unauthorized);
    }
    let digest = Sha256::digest(
        serde_json::to_vec(&(
            origin,
            crate::server_admission::profile_storage_namespace(origin),
            subject,
            owner,
        ))
        .map_err(|_| Error::Unavailable)?,
    );
    Ok(format!("{digest:x}"))
}
struct CheckedProfileDirectory {
    path: PathBuf,
    digest: String,
}

/// Validate the authenticated profile's private journal namespace without
/// opening, creating, migrating or authorizing a personal SQLite ledger. Each
/// journal must still check its own full identity and command custody. A copied
/// database in this directory never gains authority from this path check.
pub(crate) fn journal_profile_directory(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
) -> Result<PathBuf, Error> {
    Ok(checked_profile_directory(root, origin, issuer, subject, owner)?.path)
}

fn checked_profile_directory(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
) -> Result<CheckedProfileDirectory, Error> {
    let digest = profile_identity(origin, issuer, subject, owner)?;
    hagency_store::private::directory(root).map_err(|_| Error::Unavailable)?;
    let owners = root.join("owned-agent-owners");
    hagency_store::private::directory(&owners).map_err(|_| Error::Unavailable)?;
    let directory = owners.join(format!("owner_{digest}"));
    hagency_store::private::directory(&directory).map_err(|_| Error::Unavailable)?;
    let expected = LocalProfileIdentity {
        version: 1,
        origin: origin.into(),
        issuer: issuer.into(),
        subject: subject.into(),
        owner: owner.into(),
    };
    let marker = directory.join("profile-identity.json");
    if !marker.exists() {
        if std::fs::read_dir(&directory)
            .map_err(|_| Error::Unavailable)?
            .next()
            .is_some()
        {
            return Err(Error::Unauthorized);
        }
        hagency_store::private::replace(
            &marker,
            &serde_json::to_vec(&expected).map_err(|_| Error::Unavailable)?,
        )
        .map_err(|_| Error::Unavailable)?;
    }
    let stored: LocalProfileIdentity =
        serde_json::from_slice(&read_identity_marker(&marker)?).map_err(|_| Error::Unavailable)?;
    if stored != expected { return Err(Error::Unauthorized); }
    Ok(CheckedProfileDirectory {
        path: directory,
        digest,
    })
}

/// Personal operations additionally validate the actual database's immutable
/// owner/profile pin. Journal namespace validation alone cannot replace this.
pub(crate) fn ledger_path(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
) -> Result<PathBuf, Error> {
    let profile = checked_profile_directory(root, origin, issuer, subject, owner)?;
    let path = profile.path.join("agent-local.sqlite");
    hagency_store::private::open(&path, true)
        .or_else(|_| hagency_store::private::open(&path, false))
        .map_err(|_| Error::Unavailable)?;
    // Pin the database too: directory markers alone cannot make copied data valid.
    Ledger::open_scoped(&path, owner, &profile.digest).map_err(|_| Error::Unauthorized)?;
    Ok(path)
}

fn read_identity_marker(path: &Path) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    // This is structured identity metadata, not a single short secret. Bound
    // the actual private handle and the read, including concurrent file growth.
    const LIMIT: u64 = 32 * 1024;
    let file = hagency_store::private::open(path, false).map_err(|_| Error::Unavailable)?;
    if file.metadata().map_err(|_| Error::Unavailable)?.len() > LIMIT {
        return Err(Error::Unavailable);
    }
    let mut raw = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut raw)
        .map_err(|_| Error::Unavailable)?;
    if raw.len() as u64 > LIMIT {
        return Err(Error::Unavailable);
    }
    Ok(raw)
}

fn ledger(root: &Path, reply: &OwnerReply, scope: &Scope) -> Result<Ledger, OwnerError> {
    let path = ledger_path(
        root,
        &reply.origin,
        &reply.issuer,
        &reply.subject,
        &reply.owner,
    )
    .map_err(|_| error(503, "local_state_unavailable"))?;
    let mut ledger = Ledger::open_scoped(
        &path,
        &reply.owner,
        &profile_identity(&reply.origin, &reply.issuer, &reply.subject, &reply.owner)
            .map_err(|_| error(403, "owner_scope_required"))?,
    )
    .map_err(local_error)?;
    hagency_store::private::open(&path, false)
        .map_err(|_| error(503, "local_state_unavailable"))?;
    ledger
        .register_binding(&reply.owner, scope)
        .map_err(local_error)?;
    Ok(ledger)
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}
fn view(ledger: &Ledger, scope: &Scope) -> Result<Value, OwnerError> {
    let policies = ledger.policy_snapshot(scope).map_err(local_error)?;
    let mut usage = Vec::new();
    for (index, l) in [Layer::Agent, Layer::Room, Layer::Requester]
        .into_iter()
        .enumerate()
    {
        let (spent, held) = ledger
            .account(scope, l, policies[index].policy.budget.period, now())
            .map_err(local_error)?;
        usage.push(json!({"spent":spent,"held":held}));
    }
    Ok(
        json!({"agentId":scope.agent,"bindingId":scope.binding,"roomId":scope.room,"requester":scope.requester,"policies":policies,"usage":usage,"modelProfile":ledger.model_profile(scope).map_err(local_error)?,"runtime":"codex","transportOnline":false}),
    )
}
fn validate_policy(policy: &mut Policy) -> Result<(), OwnerError> {
    if let ToolPolicy::AllowWithRules { tools, directories } = &mut policy.high_risk {
        if tools.is_empty()
            || tools.len() > 64
            || directories.is_empty()
            || directories.len() > 64
            || tools
                .iter()
                .any(|v| v.is_empty() || v.len() > 128 || v.chars().any(char::is_control))
        {
            return Err(error(400, "invalid_local_policy"));
        }
        for directory in directories {
            if !Path::new(directory).is_absolute() {
                return Err(error(400, "invalid_local_policy"));
            }
            let canonical = std::fs::canonicalize(&directory)
                .map_err(|_| error(400, "invalid_local_policy"))?;
            if !canonical.is_dir() {
                return Err(error(400, "invalid_local_policy"));
            }
            *directory = canonical
                .to_str()
                .ok_or_else(|| error(400, "invalid_local_policy"))?
                .to_owned();
        }
    }
    Ok(())
}
async fn call(req: &mut Request, depot: &Depot) -> Result<Value, OwnerError> {
    // Authenticate before parsing bodies or opening files, even if accidentally
    // mounted outside the parent console's authenticate hoop.
    current(req, depot).map_err(|_| error(401, "sign_in_required"))?;
    let tail = req.param::<String>("path").unwrap_or_default();
    let parts: Vec<_> = tail
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let method = req.method().clone();
    if parts.as_slice()==["room-candidates"] {
        if method!=Method::GET || req.uri().query().is_some(){return Err(error(400,"invalid_arguments"));}
        let reply=api(req,depot,OwnerOperation::RoomAgents).await?;
        recheck(depot).map_err(|_|error(401,"sign_in_required"))?;
        return public(&reply.value,"agents");
    }
    if parts.as_slice() == ["devices"] {
        if method != Method::GET || req.uri().query().is_some() {
            return Err(error(400, "invalid_arguments"));
        }
        let reply = api(req, depot, OwnerOperation::Devices).await?;
        let host = console(depot).map_err(|_| error(503, "local_state_unavailable"))?;
        let device = host
            .authorized_device()
            .await
            .map_err(|_| error(401, "owner_authorization_required"))?;
        if reply.owner != device.owner_mxid()
            || reply.origin != device.origin()
            || reply.subject != device.subject()
            || reply.issuer != device.issuer()
        {
            return Err(error(401, "owner_authorization_required"));
        }
        let mut value = public(&reply.value, "devices")?;
        value["currentDeviceId"] = device.device_id().into();
        return Ok(value);
    }
    if parts.first() == Some(&"commands") {
        if req.uri().query().is_some() {
            return Err(error(400, "invalid_arguments"));
        }
        if parts.len() == 1 && method == Method::GET {
            return commands::list(req, depot).await;
        }
        if parts.len() == 3 && parts[2] == "resume" && method == Method::POST {
            if !body(req, 1)
                .await
                .map_err(|_| error(400, "invalid_arguments"))?
                .is_empty()
            {
                return Err(error(400, "invalid_arguments"));
            }
            return commands::resume(req, depot, parts[1]).await;
        }
        return Err(error(405, "unsupported_operation"));
    }
    if parts.is_empty() {
        if req.uri().query().is_some() {
            return Err(error(400, "invalid_arguments"));
        }
        if method == Method::GET {
            let agents = api(req, depot, OwnerOperation::Agents).await?;
            let projects = api(req, depot, OwnerOperation::Projects).await?;
            let devices = api(req, depot, OwnerOperation::Devices).await?;
            let device = console(depot)
                .map_err(|_| error(503, "local_state_unavailable"))?
                .authorized_device()
                .await
                .map_err(|_| error(401, "owner_authorization_required"))?;
            for reply in [&agents, &projects, &devices] {
                if reply.owner != device.owner_mxid()
                    || reply.origin != device.origin()
                    || reply.issuer != device.issuer()
                    || reply.subject != device.subject()
                {
                    return Err(error(401, "owner_authorization_required"));
                }
            }
            recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
            return Ok(
                json!({"ownerMxid":agents.owner,"agents":public(&agents.value,"agents")?["agents"],"projects":public(&projects.value,"projects")?["projects"],"devices":public(&devices.value,"devices")?["devices"],"currentDeviceId":device.device_id(),"transportOnline":false}),
            );
        }
        if method == Method::POST {
            let input: Create = parse(req).await?;
            if input.display_name.trim().is_empty()
                || input.display_name.chars().count() > 64
                || input.display_name.chars().any(char::is_control)
            {
                return Err(error(400, "invalid_arguments"));
            }
            key(&input.idempotency_key)?;
            return commands::submit(
                req,
                depot,
                "agent.create",
                None,
                serde_json::to_value(input).map_err(|_| error(400, "invalid_arguments"))?,
            )
            .await;
        }
    } else {
        key(parts[0])?;
        let agent = parts[0].to_owned();
        if parts.len() == 3
            && parts[1] == "owner-direct"
            && parts[2] == "ensure"
            && method == Method::POST
        {
            return direct::ensure(req, depot, &agent).await;
        }
        if (parts.len() == 1 && method == Method::GET)
            || (parts.len() == 2 && parts[1] == "execution-device")
        {
            if req.uri().query().is_some() {
                return Err(error(400, "invalid_arguments"));
            }
            let op = if parts.len() == 1 {
                OwnerOperation::Agent {
                    agent: agent.clone(),
                }
            } else if method == Method::PUT {
                let input: AssignExecutionDevice = parse(req).await?;
                OwnerOperation::AssignExecutionDevice {
                    agent: agent.clone(),
                    expected: input.expected_generation,
                }
            } else {
                return Err(error(405, "unsupported_operation"));
            };
            let reply = api(req, depot, op).await?;
            recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
            let value = public(&reply.value, "agent")?;
            if value["agent"]["id"] != agent {
                return Err(error(502, "invalid_server_response"));
            }
            return Ok(value);
        }
        if parts.len() == 2 && parts[1] == "owner-direct" && method == Method::GET {
            if req.uri().query().is_some() {
                return Err(error(400, "invalid_arguments"));
            }
            let reply = api(
                req,
                depot,
                OwnerOperation::AgentOwnerDirect {
                    agent: agent.clone(),
                },
            )
            .await?;
            let room: Option<String> = serde_json::from_value(
                reply
                    .value
                    .get("ownerDirectRoomId")
                    .ok_or_else(|| error(502, "invalid_server_response"))?
                    .clone(),
            )
            .map_err(|_| error(502, "invalid_server_response"))?;
            let binding: Option<ServerBinding> = serde_json::from_value(
                reply
                    .value
                    .get("binding")
                    .ok_or_else(|| error(502, "invalid_server_response"))?
                    .clone(),
            )
            .map_err(|_| error(502, "invalid_server_response"))?;
            if let Some(room) = &room {
                matrix(room, '!').map_err(|_| error(502, "invalid_server_response"))?;
            }
            if binding.as_ref().is_some_and(|b| {
                b.agent_id != agent
                    || b.project_id.is_some()
                    || b.scope_kind != "owner_direct"
                    || Some(&b.room_id) != room.as_ref()
            }) {
                return Err(error(502, "invalid_server_response"));
            }
            return Ok(json!({"ownerDirectRoomId":room,"binding":binding}));
        }
        if parts.len() == 2 && parts[1] == "owner-direct" && method == Method::POST {
            if req.uri().query().is_some() {
                return Err(error(400, "invalid_arguments"));
            }
            let input: OwnerDirect = parse(req).await?;
            matrix(&input.room_id, '!')?;
            let reply = api(
                req,
                depot,
                OwnerOperation::OwnerDirect {
                    agent,
                    room: input.room_id.clone(),
                },
            )
            .await?;
            if reply.value["ownerDirectRoomId"] != input.room_id {
                return Err(error(502, "invalid_server_response"));
            }
            let mut value = public(&reply.value, "creation")?;
            value["ownerDirectRoomId"] = input.room_id.into();
            return Ok(value);
        }
        if parts.len() == 2 && ["agent-policy", "agent-model-profile"].contains(&parts[1]) {
            if method != Method::GET {
                require_execution_device(req, depot, &agent).await?;
            }
            if req.uri().query().is_some() {
                return Err(error(400, "invalid_arguments"));
            }
            let mut model_change = None;
            let mut expected_model_digest = None;
            let change = if method == Method::PUT && parts[1] == "agent-model-profile" {
                let mut input: EditAgentModel = parse(req).await?;
                if !input.profile.workspace_root.is_empty()
                    && !Path::new(&input.profile.workspace_root).is_absolute()
                {
                    return Err(error(400, "invalid_local_policy"));
                }
                if !input.profile.workspace_root.is_empty() {
                    let workspace = std::fs::canonicalize(&input.profile.workspace_root)
                        .map_err(|_| error(400, "invalid_local_policy"))?;
                    if !workspace.is_dir() {
                        return Err(error(400, "invalid_local_policy"));
                    }
                    input.profile.workspace_root = workspace
                        .to_str()
                        .ok_or_else(|| error(400, "invalid_local_policy"))?
                        .into();
                }
                expected_model_digest=Some(input.expected_profile_digest);
                model_change = Some(input.profile);
                None
            } else if method == Method::GET {
                None
            } else if method == Method::PUT && parts[1] == "agent-policy" {
                let mut input: EditAgentPolicy = parse(req).await?;
                validate_policy(&mut input.policy)?;
                Some((input.expected_revision, input.policy))
            } else if method == Method::DELETE && parts[1] == "agent-policy" {
                let input: ResetAgentPolicy = parse(req).await?;
                Some((
                    input.expected_revision,
                    Policy {
                        budget: Budget {
                            limit: Limit::Unlimited,
                            period: Period::Lifetime,
                        },
                        requests: RequestPolicy::Allow,
                        high_risk: ToolPolicy::Deny,
                    },
                ))
            } else {
                return Err(error(405, "unsupported_operation"));
            };
            let reply = api(req, depot, OwnerOperation::Agents).await?;
            let agents: Vec<ServerAgent> = serde_json::from_value(reply.value["agents"].clone())
                .map_err(|_| error(502, "invalid_server_response"))?;
            // This endpoint returns only the authenticated principal's agents;
            // ownerUserId is the server domain ID, not a Matrix ID.
            if !agents.iter().any(|record| record.id == agent) {
                return Err(error(403, "owner_scope_required"));
            }
            let root = console(depot)
                .map_err(|_| error(503, "local_state_unavailable"))?
                .0
                .server_login
                .state_directory()
                .ok_or_else(|| error(503, "local_state_unavailable"))?;
            if let Some(profile) = model_change.as_mut()
                && profile.workspace_root.is_empty()
            {
                let paths = super::owner_provider::paths(
                    &root,
                    &reply.origin,
                    &reply.issuer,
                    &reply.subject,
                    &reply.owner,
                )
                .map_err(|e| error(e.status, e.code))?;
                profile.workspace_root = super::owner_provider::prepare_workspace(&paths, &agent)
                    .map_err(|e| error(e.status, e.code))?
                    .to_string_lossy()
                    .into_owned();
            }
            recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
            return tokio::task::spawn_blocking(move || {
                let path = ledger_path(&root, &reply.origin, &reply.issuer, &reply.subject, &reply.owner)
                    .map_err(|_| error(503, "local_state_unavailable"))?;
                let identity = profile_identity(&reply.origin, &reply.issuer, &reply.subject, &reply.owner)
                    .map_err(|_| error(403, "owner_scope_required"))?;
                let mut ledger = Ledger::open_scoped(&path, &reply.owner, &identity).map_err(local_error)?;
                hagency_store::private::open(&path, false).map_err(|_| error(503, "local_state_unavailable"))?;
                if let Some((revision, policy)) = change {
                    ledger.set_agent_policy(&reply.owner, &agent, revision, &policy).map_err(local_error)?;
                }
                if let Some(profile)=model_change {
                    let expected = expected_model_digest.ok_or_else(|| error(400, "invalid_arguments"))?;
                    ledger.set_agent_model_profile_checked(&reply.owner,&agent,&expected,&profile).map_err(local_error)?;
                }
                let policy = ledger.agent_policy(&reply.owner, &agent).map_err(local_error)?;
                let (spent, held) = ledger.agent_account(&reply.owner, &agent, policy.policy.budget.period, now()).map_err(local_error)?;
                let model=ledger.agent_model_profile(&reply.owner,&agent).map_err(local_error)?;
                Ok(json!({"agentId":agent,"policy":policy,"usage":{"spent":spent,"held":held},"modelProfile":model}))
            }).await.map_err(|_| error(503, "local_state_unavailable"))?;
        }
        if parts.len() >= 2 && parts[1] == "runtime" {
            if req.uri().query().is_some() {
                return Err(error(400, "invalid_arguments"));
            }
            let host = console(depot).map_err(|_| error(503, "local_state_unavailable"))?;
            if parts.len() == 3 && parts[2] == "service" && method == Method::GET {
                return host
                    .0
                    .owned_runtime
                    .agent_service_status(host, &agent)
                    .await
                    .map_err(runtime_error);
            }
            if parts.len() == 4
                && parts[2] == "service"
                && parts[3] == "stop"
                && method == Method::POST
            {
                if !body(req, 1)
                    .await
                    .map_err(|_| error(400, "invalid_arguments"))?
                    .is_empty()
                {
                    return Err(error(400, "invalid_arguments"));
                }
                let warning = host
                    .0
                    .owned_runtime
                    .stop_agent_service(host, &agent)
                    .await
                    .map_err(runtime_error)?;
                return Ok(json!({"stopped":true,"warning":warning}));
            }
            if parts.len() == 4
                && parts[2] == "service"
                && parts[3] == "start"
                && method == Method::POST
            {
                let input: StartRuntime = parse(req).await?;
                if !input.binding_id.is_empty() || input.host_files {
                    return Err(error(400, "invalid_runtime_start"));
                }
                let reply = api(
                    req,
                    depot,
                    OwnerOperation::Agent {
                        agent: agent.clone(),
                    },
                )
                .await?;
                let root = host
                    .0
                    .server_login
                    .state_directory()
                    .ok_or_else(|| error(503, "local_state_unavailable"))?;
                let path = ledger_path(
                    &root,
                    &reply.origin,
                    &reply.issuer,
                    &reply.subject,
                    &reply.owner,
                )
                .map_err(|_| error(503, "local_state_unavailable"))?;
                let profile_id =
                    profile_identity(&reply.origin, &reply.issuer, &reply.subject, &reply.owner)
                        .map_err(|_| error(403, "owner_scope_required"))?;
                let ledger =
                    Ledger::open_scoped(&path, &reply.owner, &profile_id).map_err(local_error)?;
                let profile = ledger
                    .agent_model_profile(&reply.owner, &agent)
                    .map_err(local_error)?
                    .ok_or_else(|| error(409, "runtime_profile_unavailable"))?;
                let provider = super::owner_provider::external::paths_for_reference(
                    &root,
                    &reply.origin,
                    &reply.issuer,
                    &reply.subject,
                    &reply.owner,
                    &profile.credential_ref,
                )
                .map_err(|e| error(e.status, e.code))?;
                if profile.credential_ref != provider.credential_ref {
                    return Err(error(409, "provider_profile_mismatch"));
                }
                let saved_options = host
                    .0
                    .owned_runtime
                    .saved_service_options(host, &agent)
                    .await
                    .map_err(runtime_error)?;
                let effort = input
                    .effort
                    .clone()
                    .or_else(|| saved_options.as_ref().map(|value| value.effort.clone()))
                    .unwrap_or_else(default_effort);
                let config = super::owned_runtime::StartConfig {
                    agent_id: agent,
                    binding_id: "*".into(),
                    host_files: false,
                    owner_direct: false,
                    recovery_device_id: input.recovery_device_id,
                    recovery_consent_id: input.recovery_consent_id,
                    profile: hagency_agent_local::codex::Profile {
                        executable: super::owner_provider::external::executable(
                            hagency_agent_local::runtime::kind(&profile.credential_ref),
                        )
                        .map_err(|e| error(e.status, e.code))?,
                        home: provider.home,
                        codex_home: provider.codex_home,
                        cwd: PathBuf::from(profile.workspace_root),
                        model: profile.model,
                        effort: if profile.reasoning_effort.is_empty() {
                            effort
                        } else {
                            profile.reasoning_effort
                        },
                        shared_auth: provider.shared,
                        web_search: profile.web_search,
                    },
                    credential_ref: profile.credential_ref,
                    takeover: if input.takeover {
                        super::device_execution::Takeover::OwnerRequested
                    } else {
                        super::device_execution::Takeover::Never
                    },
                };
                return host
                    .0
                    .owned_runtime
                    .start_agent_service(host.clone(), config)
                    .await
                    .map_err(runtime_error);
            }
            if parts.len() == 2 && method == Method::GET {
                let state = host
                    .0
                    .owned_runtime
                    .status(host, &agent)
                    .await
                    .map_err(runtime_error)?;
                let native_tools = state.as_ref().is_some_and(|status| status.native_tools);
                return Ok(json!({"runtime":state,"nativeTools":native_tools,"strictTokenCap":false}));
            }
            if parts.len() == 3 && parts[2] == "approvals" && method == Method::GET {
                let approvals = host
                    .0
                    .owned_runtime
                    .pending(host, &agent)
                    .await
                    .map_err(runtime_error)?;
                return Ok(json!({"approvals":approvals}));
            }
            if parts.len() == 5
                && parts[2] == "approvals"
                && parts[4] == "decision"
                && method == Method::POST
            {
                key(parts[3])?;
                let input: ToolDecision = parse(req).await?;
                if input.args_digest.len() != 64
                    || !input.args_digest.bytes().all(|c| c.is_ascii_hexdigit())
                {
                    return Err(error(400, "invalid_arguments"));
                }
                host.0
                    .owned_runtime
                    .decide(
                        host,
                        cookie(req).map_err(|_| error(401, "sign_in_required"))?,
                        &agent,
                        parts[3],
                        &input.args_digest,
                        input.approved,
                    )
                    .await
                    .map_err(runtime_error)?;
                return Ok(json!({"decided":true}));
            }
            if parts.len() == 3 && parts[2] == "stop" && method == Method::POST {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct StopBinding {
                    binding_id: String,
                }
                let input: StopBinding = parse(req).await?;
                key(&input.binding_id)?;
                host.0
                    .owned_runtime
                    .stop(host, &agent, &input.binding_id)
                    .await
                    .map_err(runtime_error)?;
                return Ok(json!({"stopped":true}));
            }
            if parts.len() == 3 && parts[2] == "start" && method == Method::POST {
                let input: StartRuntime = parse(req).await?;
                let effort = input.effort.clone().unwrap_or_else(default_effort);
                if ![
                        "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
                    ]
                    .contains(&effort.as_str())
                {
                    return Err(error(400, "invalid_runtime_start"));
                }
                let device = host
                    .authorized_device()
                    .await
                    .map_err(|_| error(401, "owner_authorization_required"))?;
                let selected = Selection {
                    binding_id: input.binding_id.clone(),
                    requester: device.owner_mxid().into(),
                };
                let (reply, scope, root) = verified(req, depot, &agent, &selected).await?;
                let profile = ledger(&root, &reply, &scope)?
                    .model_profile(&scope)
                    .map_err(local_error)?
                    .ok_or(error(409, "runtime_profile_unavailable"))?;
                let provider = super::owner_provider::external::paths_for_reference(
                    &root,
                    &reply.origin,
                    &reply.issuer,
                    &reply.subject,
                    &reply.owner,
                    &profile.credential_ref,
                )
                .map_err(|e| error(e.status, e.code))?;
                if profile.credential_ref != provider.credential_ref {
                    return Err(error(409, "provider_profile_mismatch"));
                }
                let config = super::owned_runtime::StartConfig {
                    agent_id: agent,
                    binding_id: input.binding_id,
                    host_files: input.host_files,
                    owner_direct: false,
                    recovery_device_id: input.recovery_device_id,
                    recovery_consent_id: input.recovery_consent_id,
                    profile: hagency_agent_local::codex::Profile {
                        executable: super::owner_provider::external::executable(
                            hagency_agent_local::runtime::kind(&profile.credential_ref),
                        )
                        .map_err(|e| error(e.status, e.code))?,
                        home: provider.home,
                        codex_home: provider.codex_home,
                        cwd: PathBuf::from(profile.workspace_root),
                        model: profile.model,
                        effort: if profile.reasoning_effort.is_empty() {
                            effort
                        } else {
                            profile.reasoning_effort
                        },
                        shared_auth: provider.shared,
                        web_search: profile.web_search,
                    },
                    credential_ref: profile.credential_ref,
                    takeover: if input.takeover {
                        super::device_execution::Takeover::OwnerRequested
                    } else {
                        super::device_execution::Takeover::Never
                    },
                };
                let state = host
                    .0
                    .owned_runtime
                    .start(
                        host.clone(),
                        cookie(req).map_err(|_| error(401, "sign_in_required"))?,
                        config,
                    )
                    .await
                    .map_err(runtime_error)?;
                let native_tools = state.native_tools;
                return Ok(json!({"runtime":state,"nativeTools":native_tools,"strictTokenCap":false}));
            }
            return Err(error(404, "not_found"));
        }
        if parts.len() == 1 && method == Method::DELETE && req.uri().query().is_none() {
            if !body(req, 1)
                .await
                .map_err(|_| error(400, "invalid_arguments"))?
                .is_empty()
            {
                return Err(error(400, "invalid_arguments"));
            }
            return public(
                &api(req, depot, OwnerOperation::Retire { agent })
                    .await?
                    .value,
                "agent",
            );
        }
        if (parts.len() == 3 || parts.len() == 4)
            && parts[1] == "bindings"
            && req.uri().query().is_none()
        {
            let binding = parts[2].to_owned();
            key(&binding)?;
            // Check both authenticated ownership and the URL's agent/binding pair.
            let reply = api(
                req,
                depot,
                OwnerOperation::Binding {
                    binding: binding.clone(),
                },
            )
            .await?;
            let verified: ServerBinding = serde_json::from_value(reply.value["binding"].clone())
                .map_err(|_| error(502, "invalid_server_response"))?;
            if verified.agent_id != agent {
                return Err(error(403, "owner_scope_required"));
            }
            if parts.len() == 4 && method == Method::PUT && parts[3] == "reply-policy" {
                #[derive(Deserialize)]
                #[serde(rename_all = "camelCase", deny_unknown_fields)]
                struct ReplyPolicy {
                    thread_auto_reply: bool,
                }
                let policy: ReplyPolicy = serde_json::from_slice(
                    &body(req, 1024)
                        .await
                        .map_err(|_| error(400, "invalid_arguments"))?,
                )
                .map_err(|_| error(400, "invalid_arguments"))?;
                return public(
                    &api(
                        req,
                        depot,
                        OwnerOperation::SetThreadAutoReply {
                            binding,
                            enabled: policy.thread_auto_reply,
                        },
                    )
                    .await?
                    .value,
                    "binding",
                );
            }
            if !body(req, 1)
                .await
                .map_err(|_| error(400, "invalid_arguments"))?
                .is_empty()
            {
                return Err(error(400, "invalid_arguments"));
            }
            let op = match (parts.len(), method, parts.get(3).copied()) {
                (3, Method::GET, _) => return public(&reply.value, "binding"),
                (3, Method::DELETE, _) => OwnerOperation::LeaveBinding { binding },
                (4, Method::POST, Some("pause")) => OwnerOperation::PauseBinding { binding },
                (4, Method::POST, Some("resume")) => OwnerOperation::ResumeBinding { binding },
                _ => return Err(error(404, "not_found")),
            };
            return public(&api(req, depot, op).await?.value, "binding");
        }
        if parts.len() == 2 && parts[1] == "bindings" && req.uri().query().is_none() {
            if method == Method::GET {
                return public(
                    &api(req, depot, OwnerOperation::Bindings { agent })
                        .await?
                        .value,
                    "bindings",
                );
            }
            if method == Method::POST {
                let input: Bind = parse(req).await?;
                matrix(&input.room_id, '!')?;
                key(&input.project_id)?;
                key(&input.idempotency_key)?;
                return commands::submit(
                    req,
                    depot,
                    "agent.bind",
                    Some(agent),
                    serde_json::to_value(input).map_err(|_| error(400, "invalid_arguments"))?,
                )
                .await;
            }
        }
        if parts.len() == 2
            && ["pause", "resume"].contains(&parts[1])
            && method == Method::POST
            && req.uri().query().is_none()
        {
            if !body(req, 1)
                .await
                .map_err(|_| error(400, "invalid_arguments"))?
                .is_empty()
            {
                return Err(error(400, "invalid_arguments"));
            }
            let op = if parts[1] == "pause" {
                OwnerOperation::Pause { agent }
            } else {
                OwnerOperation::Resume { agent }
            };
            return public(&api(req, depot, op).await?.value, "agent");
        }
        if parts.len() == 2 && parts[1] == "local-policy" {
            if method != Method::GET {
                require_execution_device(req, depot, &agent).await?;
            }
            let route = parts[1].to_owned();
            enum Change {
                Read,
                Policy(EditPolicy),
                Reset(ResetPolicy),
            }
            let (selected, change) = if method == Method::GET {
                (selection(req)?, Change::Read)
            } else if req.uri().query().is_some() {
                return Err(error(400, "invalid_arguments"));
            } else if method == Method::PUT && route == "local-policy" {
                let input: EditPolicy = parse(req).await?;
                (
                    Selection {
                        binding_id: input.binding_id.clone(),
                        requester: input.requester.clone(),
                    },
                    Change::Policy(input),
                )
            } else if method == Method::DELETE && route == "local-policy" {
                let input: ResetPolicy = parse(req).await?;
                (
                    Selection {
                        binding_id: input.binding_id.clone(),
                        requester: input.requester.clone(),
                    },
                    Change::Reset(input),
                )
            } else {
                return Err(error(405, "unsupported_operation"));
            };
            let (reply, scope, root) = verified(req, depot, &agent, &selected).await?;
            recheck(depot).map_err(|_| error(401, "sign_in_required"))?;
            return tokio::task::spawn_blocking(move || {
                let mut ledger = ledger(&root, &reply, &scope)?;
                match change {
                    Change::Read => {}
                    Change::Policy(mut input) => {
                        validate_policy(&mut input.policy)?;
                        ledger
                            .set_policy(
                                &reply.owner,
                                &scope,
                                layer(&input.layer)?,
                                input.expected_revision,
                                &input.policy,
                            )
                            .map_err(local_error)?;
                    }
                    Change::Reset(input) => {
                        let l = layer(&input.layer)?;
                        let policy = Policy {
                            budget: Budget {
                                limit: Limit::Unlimited,
                                period: Period::Lifetime,
                            },
                            requests: RequestPolicy::Allow,
                            high_risk: ToolPolicy::Deny,
                        };
                        ledger
                            .set_policy(&reply.owner, &scope, l, input.expected_revision, &policy)
                            .map_err(local_error)?;
                    }
                }
                view(&ledger, &scope)
            })
            .await
            .map_err(|_| error(503, "local_state_unavailable"))?;
        }
    }
    Err(error(405, "unsupported_operation"))
}
#[handler]
async fn dispatch(req: &mut Request, depot: &Depot, res: &mut Response) {
    res.add_header("cache-control", "no-store", true).unwrap();
    match call(req, depot).await {
        Ok(value) => res.render(Json(value)),
        Err(error) => {
            res.status_code(
                StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );
            res.render(Json(json!({"code":error.code})));
        }
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn runtime_start_rejects_obsolete_budget_configuration() {
        assert!(serde_json::from_value::<StartRuntime>(json!({"effort":"low"})).is_ok());
        for value in [json!({"estimatedOptIn":true}), json!({"reservation":0}),
            json!({"reservation":1234}), json!({"mode":"strict"}), json!({"mode":"estimated"}),
            json!({"executable":"untrusted"})] {
            assert!(serde_json::from_value::<StartRuntime>(value).is_err());
        }
    }
    #[test]
    fn public_binding_keeps_reply_policy_without_leaking_unknown_fields() {
        let result=public(&json!({"binding":{"id":"b","agentId":"a","projectId":"p","scopeKind":"project","roomId":"!r:test","state":"active","generation":1,"threadAutoReply":true,"token":"secret"}}),"binding").unwrap();
        assert_eq!(result["binding"]["threadAutoReply"], true);
        assert!(result["binding"].get("token").is_none());
    }
    #[test]
    fn profile_marker_accepts_long_identity_and_rejects_oversized_metadata() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().join("state");
        let origin = "https://server.example/";
        let issuer = "https://server.example/auth/";
        let owner = format!("@{}:example", "a".repeat(200));
        let subject = "s".repeat(255);
        let first = ledger_path(&root, origin, issuer, &subject, &owner).unwrap();
        let marker = first.parent().unwrap().join("profile-identity.json");
        assert!(std::fs::metadata(&marker).unwrap().len() > 512);
        assert_eq!(
            ledger_path(&root, origin, issuer, &subject, &owner).unwrap(),
            first
        );
        hagency_store::private::replace(&marker, &vec![b' '; 32769]).unwrap();
        assert!(ledger_path(&root, origin, issuer, &subject, &owner).is_err());
    }

    #[test]
    fn obsolete_auth_marker_is_rejected_without_rewriting_identity() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().join("state");
        let origin = "https://server.example/";
        let issuer = "https://server.example/auth/";
        let owner = "@alice:example";
        let legacy_issuer = crate::server_admission::profile_storage_namespace(origin);
        let legacy_digest = format!(
            "{:x}",
            Sha256::digest(
                serde_json::to_vec(&(origin, &legacy_issuer, "subject", owner)).unwrap()
            )
        );
        assert_eq!(
            profile_identity(origin, issuer, "subject", owner).unwrap(),
            legacy_digest
        );
        assert!(profile_identity(origin, &legacy_issuer, "subject", owner).is_err());
        let path = ledger_path(&root, origin, issuer, "subject", owner).unwrap();
        let marker = path.parent().unwrap().join("profile-identity.json");
        let legacy = LocalProfileIdentity {
            version: 1,
            origin: origin.into(),
            issuer: legacy_issuer,
            subject: "subject".into(),
            owner: owner.into(),
        };
        hagency_store::private::replace(&marker, &serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(ledger_path(&root, origin, issuer, "subject", owner).is_err());
        let retained: LocalProfileIdentity = serde_json::from_slice(&read_identity_marker(&marker).unwrap()).unwrap();
        assert!(retained == legacy);
        let wrong = LocalProfileIdentity {
            subject: "different".into(),
            ..legacy
        };
        hagency_store::private::replace(&marker, &serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(ledger_path(&root, origin, issuer, "subject", owner).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn profile_marker_rejects_symlink_and_nonprivate_file() {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let file = temp.path().join("metadata.json");
        hagency_store::private::write_new(&file, b"{}").unwrap();
        let link = temp.path().join("link.json");
        symlink(&file, &link).unwrap();
        assert!(read_identity_marker(&link).is_err());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_identity_marker(&file).is_err());
    }

    #[test]
    fn multi_profile_directories_preserve_local_budget_and_reject_foreign_copy() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().join("state");
        let origin = "https://server.example/";
        let issuer = "https://server.example/auth/";
        let owner = "@alice:example";
        let a = ledger_path(&root, origin, issuer, "subject-a", owner).unwrap();
        let b = ledger_path(&root, origin, issuer, "subject-b", owner).unwrap();
        assert_ne!(a, b);
        let other = ledger_path(
            &root,
            "https://other.example/",
            "https://other.example/auth/",
            "subject-a",
            owner,
        )
        .unwrap();
        assert_ne!(a, other);
        let scope = Scope {
            agent: "agent".into(),
            binding: "binding".into(),
            room: "!room:example".into(),
            requester: owner.into(),
            thread: "thread".into(),
        };
        let mut ledger = Ledger::open_scoped(
            &a,
            owner,
            &profile_identity(origin, issuer, "subject-a", owner).unwrap(),
        )
        .unwrap();
        ledger.register_binding(owner, &scope).unwrap();
        for layer in [Layer::Agent, Layer::Room, Layer::Requester] {
            ledger
                .set_policy(
                    owner,
                    &scope,
                    layer,
                    0,
                    &Policy {
                        budget: Budget {
                            limit: Limit::Tokens(100),
                            period: Period::Lifetime,
                        },
                        requests: RequestPolicy::Allow,
                        high_risk: ToolPolicy::Deny,
                    },
                )
                .unwrap();
        }
        ledger.reserve(&scope, "call", "dispatch", 10, 1).unwrap();
        ledger
            .settle(
                &scope,
                "call",
                &hagency_agent_local::Usage {
                    input: 7,
                    output: 0,
                    cached_input: 0,
                    cache_write_input: None,
                    reasoning_output: 0,
                    accounting_version: "test".into(),
                },
            )
            .unwrap();
        drop(ledger);
        let again = ledger_path(&root, origin, issuer, "subject-a", owner).unwrap();
        assert_eq!(a, again);
        let ledger = Ledger::open_scoped(
            &again,
            owner,
            &profile_identity(origin, issuer, "subject-a", owner).unwrap(),
        )
        .unwrap();
        assert_eq!(
            ledger
                .account(&scope, Layer::Room, Period::Lifetime, 2)
                .unwrap(),
            (7, 0)
        );
        drop(ledger);
        std::fs::copy(&a, &b).unwrap();
        assert!(ledger_path(&root, origin, issuer, "subject-b", owner).is_err());
        assert!(
            ledger_path(
                &root,
                origin,
                "https://bad.example/auth/",
                "subject-a",
                owner
            )
            .is_err()
        );
        let marker = a.parent().unwrap().join("profile-identity.json");
        hagency_store::private::replace(&marker, b"{}").unwrap();
        assert!(ledger_path(&root, origin, issuer, "subject-a", owner).is_err());
    }
}

#[cfg(test)]
mod execution_projection_tests {
    use super::*;
    #[test]
    fn global_creation_has_no_room_and_device_projections_never_expose_tokens() {
        let agent = json!({"id":"a","ownerUserId":"o","puppetMxid":"@a:test","displayName":"Agent","state":"creating","generation":1,"executionDeviceId":"d","token":"secret"});
        let result = public(
            &json!({"creation":{"agent":agent},"commandState":"pending"}),
            "creation",
        )
        .unwrap();
        assert_eq!(result["commandState"], "pending");
        assert!(result["creation"].get("binding").is_none());
        assert!(!result.to_string().contains("secret"));
        assert!(
            serde_json::from_value::<Create>(
                json!({"displayName":"A","idempotencyKey":"key","roomId":"!r:test"})
            )
            .is_err()
        );
        let devices=public(&json!({"devices":[{"id":"d","name":"Device","generation":1,"revoked":false,"token":"secret"}]}),"devices").unwrap();
        assert!(!devices.to_string().contains("secret"));
        assert!(
            serde_json::from_value::<AssignExecutionDevice>(
                json!({"expectedGeneration":1,"deviceId":"foreign"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<AssignExecutionDevice>(
                json!({"expectedGeneration":1,"name":"old instance"})
            )
            .is_err()
        );
    }
    #[test]
    fn direct_binding_remains_a_real_non_project_scope() {
        let result=public(&json!({"bindings":[{"id":"b","agentId":"a","projectId":null,"scopeKind":"owner_direct","roomId":"!direct:test","state":"active","generation":1,"token":"secret"}]}),"bindings").unwrap();
        assert!(result["bindings"][0]["projectId"].is_null());
        assert_eq!(result["bindings"][0]["scopeKind"], "owner_direct");
        assert!(!result.to_string().contains("secret"));
    }
}
