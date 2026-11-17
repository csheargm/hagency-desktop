//! Host-only device transport. This module performs no model/tool execution and
//! never ACKs automatically: callers must durably persist events before ACK.
use super::{AuthorizedDevice, Console};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_WIRE_BYTES: usize = 131_072;
const MAX_TEXT_BYTES: usize = 65_536;
// Up to 100 events with 64KiB text each, worst-case six-byte JSON escaping.
const MAX_RESPONSE_BYTES: usize = 40 * 1024 * 1024;

/// Immutable host-only profile/device pin. No bearer or browser token is stored.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct DeviceIdentity {
    pub(super) origin: String,
    pub(super) issuer: String,
    pub(super) subject: String,
    pub(super) owner: String,
    pub(super) user: String,
    pub(super) id: String,
    pub(super) generation: u64,
}
impl From<&AuthorizedDevice> for DeviceIdentity {
    fn from(device: &AuthorizedDevice) -> Self {
        Self {
            origin: device.origin().into(),
            issuer: device.issuer().into(),
            subject: device.subject().into(),
            owner: device.owner_mxid().into(),
            user: device.user_id().into(),
            id: device.device_id().into(),
            generation: device.generation(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceError {
    pub status: u16,
    pub code: String,
}
impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.code)
    }
}
impl std::error::Error for DeviceError {}
fn error(status: u16, code: &str) -> DeviceError {
    DeviceError {
        status,
        code: code.into(),
    }
}
fn id(value: &str) -> Result<(), DeviceError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Err(error(400, "invalid_execution_arguments"))
    } else {
        Ok(())
    }
}
/// Room version 3+ event IDs are opaque `$` identifiers without a server part.
fn event_id(value: &str) -> Result<(), DeviceError> {
    if value.starts_with('$')
        && (2..=512).contains(&value.len())
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        Ok(())
    } else {
        Err(error(400, "invalid_execution_arguments"))
    }
}
fn matrix(value: &str, prefix: char) -> Result<(), DeviceError> {
    if value.starts_with(prefix)
        && value.len() <= 255
        && value[1..]
            .split_once(':')
            .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
        && !value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        Ok(())
    } else {
        Err(error(502, "invalid_execution_response"))
    }
}
fn now_ms() -> Result<i64, DeviceError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64)
        .map_err(|_| error(503, "invalid_local_clock"))
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LeaseRef {
    pub agent_id: String,
    pub epoch: i64,
}
impl LeaseRef {
    fn validate(&self) -> Result<(), DeviceError> {
        id(&self.agent_id)?;
        if self.epoch < 1 {
            Err(error(400, "invalid_lease_epoch"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub agent_id: String,
    pub owner_user_id: String,
    pub device_id: String,
    pub device_generation: i64,
    pub epoch: i64,
    pub expires_at_ms: i64,
}
impl Lease {
    pub fn reference(&self) -> LeaseRef {
        LeaseRef {
            agent_id: self.agent_id.clone(),
            epoch: self.epoch,
        }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dispatch {
    pub id: String,
    pub binding_id: String,
    pub agent_id: String,
    pub event_id: String,
    pub room_id: String,
    pub requester_mxid: String,
    pub thread_root: String,
    pub body: String,
    pub state: String,
    pub binding_generation: i64,
    pub dispatch_epoch: Option<i64>,
    pub dispatch_device_id: Option<String>,
    pub execution_id: Option<String>,
    pub outcome: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionStart {
    pub dispatch: Dispatch,
    pub newly_started: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplyReceipt {
    pub id: String,
    pub owner_event_id: String,
    pub agent_id: String,
    pub binding_id: String,
    pub owner_user_id: String,
    pub room_id: String,
    pub puppet_mxid: String,
    pub binding_generation: i64,
    pub dispatch_epoch: i64,
    pub delivery_epoch: i64,
    pub requester_mxid: String,
    pub thread_root: String,
    pub body: String,
    pub payload_digest: String,
    pub matrix_txn_id: String,
    pub state: String,
    pub matrix_event_id: Option<String>,
}
/// Token for the Agent's dedicated Matrix device, issued only to the lease holder
/// so it can decrypt and encrypt locally in an encrypted owner-direct Room.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct E2eeDeviceGrant {
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
    pub homeserver: String,
}
impl std::fmt::Debug for E2eeDeviceGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2eeDeviceGrant")
            .field("user_id", &self.user_id)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}
/// Taking over another live device lease requires a deliberate owner action.
#[derive(Clone, Copy)]
pub enum Takeover {
    Never,
    OwnerRequested,
}
#[derive(Clone, Copy)]
pub enum Outcome {
    Rejected,
    Failed,
    Unknown,
}
impl Outcome {
    fn wire(self) -> &'static str {
        match self {
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}
/// Text already durably persisted for an original execution. These local
/// expectations never become client-supplied membership facts on the wire.
pub struct KnownReply {
    pub dispatch_id: String,
    pub execution_id: String,
    pub body: String,
    pub binding_id: String,
    pub room_id: String,
    pub requester_mxid: String,
    pub thread_root: String,
    pub binding_generation: i64,
    pub original_epoch: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HistorySnapshot {
    pub count: u64,
    pub digest: String,
}
impl HistorySnapshot {
    fn validate(&self) -> Result<(), DeviceError> {
        if self.digest.len() != 64
            || !self
                .digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(error(502, "invalid_execution_history"));
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionHistory {
    pub agent_id: String,
    pub snapshot: HistorySnapshot,
    pub executions: Vec<hagency_agent_local::inbox::HistoryExecution>,
    pub next_cursor: Option<String>,
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessingReceipt {
    pub id: String,
    pub dispatch_id: String,
    pub execution_id: String,
    pub state: String,
    pub matrix_event_id: Option<String>,
}
impl ProcessingReceipt {
    fn validate(&self, dispatch_id: &str, execution_id: &str) -> Result<(), DeviceError> {
        id(&self.id).map_err(|_| error(502, "invalid_execution_response"))?;
        if self.dispatch_id != dispatch_id
            || self.execution_id != execution_id
            || !["pending", "sending", "sent", "unknown", "cancelled"]
                .contains(&self.state.as_str())
            || (self.state == "sent") != self.matrix_event_id.is_some()
            || self.matrix_event_id.as_ref().is_some_and(|event| {
                !event.starts_with('$') || event.len() > 1024 || event.chars().any(char::is_control)
            })
        {
            return Err(error(502, "execution_scope_mismatch"));
        }
        Ok(())
    }
}
pub enum DeviceOperation {
    History {
        agent_id: String,
        cursor: Option<String>,
        snapshot: Option<HistorySnapshot>,
    },
    Acquire {
        agent_id: String,
        ttl_ms: i64,
        takeover: Takeover,
        history_snapshot: HistorySnapshot,
    },
    Renew {
        lease: LeaseRef,
        ttl_ms: i64,
    },
    Release {
        lease: LeaseRef,
    },
    Poll {
        lease: LeaseRef,
        binding_id: String,
        limit: usize,
    },
    Ack {
        lease: LeaseRef,
        dispatch_id: String,
    },
    Start {
        lease: LeaseRef,
        dispatch_id: String,
        execution_id: String,
    },
    Processing {
        lease: LeaseRef,
        dispatch_id: String,
        execution_id: String,
    },
    AuthorizeTool {
        lease: LeaseRef,
        dispatch_id: String,
        execution_id: String,
    },
    Finish {
        lease: LeaseRef,
        dispatch_id: String,
        execution_id: String,
        outcome: Outcome,
    },
    ReconcileKnownReply {
        lease: LeaseRef,
        known: KnownReply,
    },
    Reply {
        lease: LeaseRef,
        dispatch_id: String,
        execution_id: String,
        body: String,
    },
    E2eeCandidates { lease: LeaseRef, binding_id: String },
    E2eeConfirm { lease: LeaseRef, binding_id: String, event_id: String, mentions: Vec<String>, thread_root: Option<String> },
    E2eeDevice {
        lease: LeaseRef,
    },
    /// Records the run card this device already edited into the plaintext
    /// answer: one message per answer, so the server posts no second reply.
    CardReply {
        lease: LeaseRef,
        dispatch_id: String,
        execution_id: String,
        body: String,
        matrix_event_id: String,
    },
    /// Records an encrypted reply this device already sent with the Agent device.
    EncryptedReply {
        lease: LeaseRef,
        dispatch_id: String,
        execution_id: String,
        matrix_event_id: String,
    },
}
pub enum DeviceResponse {
    History(ExecutionHistory),
    Lease(Lease),
    Released,
    Events(Vec<Dispatch>),
    Acknowledged,
    Started(ExecutionStart),
    Processing(ProcessingReceipt),
    ToolAuthorized(Dispatch),
    Finished,
    ReplyQueued(ReplyReceipt),
    E2eeDevice(E2eeDeviceGrant),
    E2eeCandidates(serde_json::Value),
    E2eeConfirmed,
}
impl DeviceOperation {
    fn lease(&self) -> Option<&LeaseRef> {
        match self {
            Self::Acquire { .. } | Self::History { .. } => None,
            Self::Renew { lease, .. }
            | Self::Release { lease }
            | Self::Poll { lease, .. }
            | Self::Ack { lease, .. }
            | Self::Start { lease, .. }
            | Self::Processing { lease, .. }
            | Self::AuthorizeTool { lease, .. }
            | Self::Finish { lease, .. }
            | Self::Reply { lease, .. }
            | Self::E2eeCandidates { lease, .. }
            | Self::E2eeConfirm { lease, .. }
            | Self::E2eeDevice { lease }
            | Self::EncryptedReply { lease, .. }
            | Self::CardReply { lease, .. }
            | Self::ReconcileKnownReply { lease, .. } => Some(lease),
        }
    }
    fn request(&self) -> Result<(&'static str, Vec<u8>), DeviceError> {
        if let Some(lease) = self.lease() {
            lease.validate()?;
        }
        let (path, body) = match self {
            Self::E2eeCandidates {lease,binding_id} => { id(binding_id)?; ("e2ee-candidates",json!({"lease":lease,"bindingId":binding_id})) },
            Self::E2eeConfirm {lease,binding_id,event_id,mentions,thread_root} => {
                id(binding_id)?;
                // Room version 3+ event IDs are opaque hashes, not Matrix user
                // or room IDs with a server suffix. Rejecting them here leaves
                // encrypted group candidates permanently unconfirmed.
                self::event_id(event_id)?;
                if let Some(root) = thread_root { self::event_id(root)?; }
                ("e2ee-confirm",json!({"lease":lease,"bindingId":binding_id,"eventId":event_id,"mentions":mentions,"threadRoot":thread_root}))
            },
            Self::History {
                agent_id,
                cursor,
                snapshot,
            } => {
                id(agent_id)?;
                if let Some(cursor) = cursor {
                    id(cursor)?;
                }
                if cursor.is_some() != snapshot.is_some() {
                    return Err(error(400, "invalid_execution_history"));
                }
                if let Some(snapshot) = snapshot {
                    snapshot.validate()?;
                }
                (
                    "history",
                    json!({"agentId":agent_id,"cursor":cursor,"snapshot":snapshot}),
                )
            }
            Self::Acquire {
                agent_id,
                ttl_ms,
                takeover,
                history_snapshot,
            } => {
                history_snapshot.validate()?;
                id(agent_id)?;
                ttl(*ttl_ms)?;
                (
                    "leases/acquire",
                    json!({"agentId":agent_id,"ttlMs":ttl_ms,"takeover":matches!(takeover,Takeover::OwnerRequested),"historySnapshot":history_snapshot}),
                )
            }
            Self::Renew { lease, ttl_ms } => {
                ttl(*ttl_ms)?;
                ("leases/renew", json!({"lease":lease,"ttlMs":ttl_ms}))
            }
            Self::Release { lease } => ("leases/release", json!({"lease":lease})),
            Self::Poll {
                lease,
                binding_id,
                limit,
            } => {
                id(binding_id)?;
                if !(1..=100).contains(limit) {
                    return Err(error(400, "invalid_poll_limit"));
                }
                (
                    "events/poll",
                    json!({"lease":lease,"bindingId":binding_id,"limit":limit}),
                )
            }
            Self::Ack { lease, dispatch_id } => {
                id(dispatch_id)?;
                (
                    "events/ack",
                    json!({"lease":lease,"dispatchId":dispatch_id}),
                )
            }
            Self::Start {
                lease,
                dispatch_id,
                execution_id,
            } => {
                id(dispatch_id)?;
                id(execution_id)?;
                (
                    "events/start",
                    json!({"lease":lease,"dispatchId":dispatch_id,"executionId":execution_id}),
                )
            }
            Self::Processing {
                lease,
                dispatch_id,
                execution_id,
            } => {
                id(dispatch_id)?;
                id(execution_id)?;
                (
                    "events/processing",
                    json!({"lease":lease,"dispatchId":dispatch_id,"executionId":execution_id}),
                )
            }
            Self::AuthorizeTool {
                lease,
                dispatch_id,
                execution_id,
            } => {
                id(dispatch_id)?;
                id(execution_id)?;
                (
                    "events/authorize-tool",
                    json!({"lease":lease,"dispatchId":dispatch_id,"executionId":execution_id}),
                )
            }
            Self::Finish {
                lease,
                dispatch_id,
                execution_id,
                outcome,
            } => {
                id(dispatch_id)?;
                id(execution_id)?;
                (
                    "events/finish",
                    json!({"lease":lease,"dispatchId":dispatch_id,"executionId":execution_id,"outcome":outcome.wire()}),
                )
            }
            Self::ReconcileKnownReply { lease, known } => {
                for value in [&known.dispatch_id, &known.execution_id, &known.binding_id] {
                    id(value)?;
                }
                matrix(&known.room_id, '!')
                    .map_err(|_| error(400, "invalid_execution_arguments"))?;
                matrix(&known.requester_mxid, '@')
                    .map_err(|_| error(400, "invalid_execution_arguments"))?;
                if known.original_epoch < 1
                    || known.original_epoch > lease.epoch
                    || known.binding_generation < 1
                    || !(known.thread_root.starts_with('$') || known.thread_root == known.room_id)
                    || known.thread_root.is_empty()
                    || known.thread_root.len() > 1024
                    || known.thread_root.chars().any(char::is_control)
                {
                    return Err(error(400, "invalid_execution_arguments"));
                }
                if known.body.is_empty() || known.body.len() > MAX_TEXT_BYTES {
                    return Err(error(400, "reply_too_large"));
                }
                (
                    "replies/reconcile-known",
                    json!({"lease":lease,"reply":{"dispatchId":known.dispatch_id,"executionId":known.execution_id,"body":known.body}}),
                )
            }
            Self::Reply {
                lease,
                dispatch_id,
                execution_id,
                body,
            } => {
                id(dispatch_id)?;
                id(execution_id)?;
                if body.is_empty() || body.len() > MAX_TEXT_BYTES {
                    return Err(error(400, "reply_too_large"));
                }
                (
                    "replies",
                    json!({"lease":lease,"reply":{"dispatchId":dispatch_id,"executionId":execution_id,"body":body}}),
                )
            }
            Self::E2eeDevice { lease } => ("e2ee-device", json!({"lease":lease})),
            Self::CardReply { lease, dispatch_id, execution_id, body, matrix_event_id } => {
                id(dispatch_id)?;
                id(execution_id)?;
                event_id(matrix_event_id)?;
                if body.is_empty() || body.len() > MAX_TEXT_BYTES {
                    return Err(error(400, "reply_too_large"));
                }
                (
                    "replies/card",
                    json!({"lease":lease,"reply":{"dispatchId":dispatch_id,"executionId":execution_id,"body":body,"matrixEventId":matrix_event_id}}),
                )
            }
            Self::EncryptedReply {
                lease,
                dispatch_id,
                execution_id,
                matrix_event_id,
            } => {
                id(dispatch_id)?;
                id(execution_id)?;
                event_id(matrix_event_id)?;
                (
                    "replies/encrypted",
                    json!({"lease":lease,"reply":{"dispatchId":dispatch_id,"executionId":execution_id,"matrixEventId":matrix_event_id}}),
                )
            }
        };
        let bytes =
            serde_json::to_vec(&body).map_err(|_| error(400, "invalid_execution_arguments"))?;
        if bytes.len() > MAX_WIRE_BYTES {
            return Err(error(400, "execution_wire_body_too_large"));
        }
        Ok((path, bytes))
    }
}
fn ttl(ttl: i64) -> Result<(), DeviceError> {
    if (1000..=60_000).contains(&ttl) {
        Ok(())
    } else {
        Err(error(400, "invalid_lease_ttl"))
    }
}
fn decode<T: serde::de::DeserializeOwned>(value: &Value, key: &str) -> Result<T, DeviceError> {
    serde_json::from_value(value[key].clone()).map_err(|_| error(502, "invalid_execution_response"))
}
fn check_dispatch(
    dispatch: &Dispatch,
    reference: &LeaseRef,
    device: &AuthorizedDevice,
) -> Result<(), DeviceError> {
    for value in [&dispatch.id, &dispatch.binding_id, &dispatch.agent_id] {
        id(value).map_err(|_| error(502, "invalid_execution_response"))?;
    }
    matrix(&dispatch.room_id, '!')?;
    matrix(&dispatch.requester_mxid, '@')?;
    if dispatch.agent_id != reference.agent_id
        || dispatch.binding_generation < 1
        || dispatch.dispatch_epoch != Some(reference.epoch)
        || dispatch.dispatch_device_id.as_deref() != Some(device.device_id())
        || dispatch.body.len() > MAX_TEXT_BYTES
        || dispatch.event_id.is_empty()
        || dispatch.event_id.len() > 1024
        || dispatch.event_id.chars().any(char::is_control)
        || !(dispatch.thread_root.starts_with('$') || dispatch.thread_root == dispatch.room_id)
        || dispatch.thread_root.is_empty()
        || dispatch.thread_root.len() > 1024
        || dispatch.thread_root.chars().any(char::is_control)
    {
        return Err(error(502, "execution_scope_mismatch"));
    }
    Ok(())
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitDigest<'a> {
    dispatch_id: &'a str,
    execution_id: &'a str,
    body: &'a str,
}
fn payload_digest(
    dispatch_id: &str,
    execution_id: &str,
    body: &str,
) -> Result<String, DeviceError> {
    let encoded = serde_json::to_vec(&SubmitDigest {
        dispatch_id,
        execution_id,
        body,
    })
    .map_err(|_| error(400, "invalid_execution_arguments"))?;
    Ok(format!("{:x}", Sha256::digest(encoded)))
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedDigest<'a> {
    dispatch_id: &'a str,
    execution_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
    matrix_event_id: &'a str,
}
/// This device already sent the encrypted reply: the server keeps no body and
/// records exactly that Matrix event as sent under the current lease.
fn check_encrypted_reply(
    reply: &ReplyReceipt,
    lease: &LeaseRef,
    device: &AuthorizedDevice,
    dispatch_id: &str,
    execution_id: &str,
    matrix_event_id: &str,
) -> Result<(), DeviceError> {
    check_recorded_reply(reply, lease, device, dispatch_id, execution_id, None, matrix_event_id)
}
/// A reply this device already put in the Room (encrypted: no body kept; a
/// plaintext run card: its answer kept), recorded as sent under this lease.
fn check_recorded_reply(
    reply: &ReplyReceipt,
    lease: &LeaseRef,
    device: &AuthorizedDevice,
    dispatch_id: &str,
    execution_id: &str,
    body: Option<&str>,
    matrix_event_id: &str,
) -> Result<(), DeviceError> {
    let digest = serde_json::to_vec(&EncryptedDigest {
        dispatch_id,
        execution_id,
        body,
        matrix_event_id,
    })
    .map_err(|_| error(400, "invalid_execution_arguments"))?;
    if reply.owner_event_id != dispatch_id
        || reply.agent_id != lease.agent_id
        || reply.owner_user_id != device.user_id()
        || reply.binding_generation < 1
        || reply.dispatch_epoch != lease.epoch
        || reply.delivery_epoch != lease.epoch
        || reply.body != body.unwrap_or_default()
        || reply.payload_digest != format!("{:x}", Sha256::digest(digest))
        || reply.matrix_txn_id != format!("hagency_{:x}", Sha256::digest(dispatch_id.as_bytes()))
        || reply.state != "sent"
        || reply.matrix_event_id.as_deref() != Some(matrix_event_id)
        || !(reply.thread_root.starts_with('$') || reply.thread_root == reply.room_id)
    {
        return Err(error(502, "execution_scope_mismatch"));
    }
    matrix(&reply.room_id, '!')?;
    matrix(&reply.puppet_mxid, '@')?;
    matrix(&reply.requester_mxid, '@')?;
    id(&reply.id).map_err(|_| error(502, "invalid_execution_response"))?;
    id(&reply.binding_id).map_err(|_| error(502, "invalid_execution_response"))
}
fn check_reply(
    reply: &ReplyReceipt,
    lease: &LeaseRef,
    device: &AuthorizedDevice,
    dispatch_id: &str,
    execution_id: &str,
    body: &str,
) -> Result<(), DeviceError> {
    if reply.owner_event_id != dispatch_id
        || reply.agent_id != lease.agent_id
        || reply.owner_user_id != device.user_id()
        || reply.binding_generation < 1
        || reply.delivery_epoch < 1
        || reply.body != body
        || reply.payload_digest != payload_digest(dispatch_id, execution_id, body)?
        || reply.matrix_txn_id != format!("hagency_{:x}", Sha256::digest(dispatch_id.as_bytes()))
        || !["pending", "unknown", "sending", "sent", "cancelled"].contains(&reply.state.as_str())
        || !(reply.thread_root.starts_with('$') || reply.thread_root == reply.room_id)
        || reply.thread_root.is_empty()
        || reply.thread_root.len() > 1024
        || reply.thread_root.chars().any(char::is_control)
    {
        return Err(error(502, "execution_scope_mismatch"));
    }
    matrix(&reply.room_id, '!')?;
    matrix(&reply.puppet_mxid, '@')?;
    matrix(&reply.requester_mxid, '@')?;
    id(&reply.id).map_err(|_| error(502, "invalid_execution_response"))?;
    id(&reply.binding_id).map_err(|_| error(502, "invalid_execution_response"))?;
    if reply
        .matrix_event_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 1024 || id.chars().any(char::is_control))
    {
        return Err(error(502, "execution_scope_mismatch"));
    }
    // Durable acceptance and historical sent receipts do not permit model replay.
    Ok(())
}
fn parse_response(
    operation: &DeviceOperation,
    value: Value,
    device: &AuthorizedDevice,
) -> Result<DeviceResponse, DeviceError> {
    let invalid = || error(502, "invalid_execution_response");
    match operation {
        DeviceOperation::History {
            agent_id,
            cursor,
            snapshot,
        } => {
            let history: ExecutionHistory = decode(&value, "history")?;
            history.snapshot.validate()?;
            if history.agent_id != *agent_id
                || history.executions.len() > 128
                || snapshot.as_ref().is_some_and(|s| s != &history.snapshot)
            {
                return Err(invalid());
            }
            let mut previous = cursor.as_deref();
            for h in &history.executions {
                for v in [
                    &h.dispatch_id,
                    &h.execution_id,
                    &h.binding_id,
                    &h.dispatch_device_id,
                ] {
                    id(v).map_err(|_| invalid())?;
                }
                matrix(&h.room_id, '!')?;
                matrix(&h.requester_mxid, '@')?;
                if h.agent_id != *agent_id
                    || h.dispatch_epoch < 1
                    || h.binding_generation < 1
                    || h.immutable_digest.len() != 64
                    || !h
                        .immutable_digest
                        .bytes()
                        .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                    || previous.is_some_and(|p| p >= h.dispatch_id.as_str())
                    || !h.event_id.starts_with('$')
                    || h.event_id.len() > 1024
                    || !(h.thread_root.starts_with('$') || h.thread_root == h.room_id)
                    || h.thread_root.is_empty()
                    || h.thread_root.len() > 1024
                    || h.thread_root.chars().any(char::is_control)
                {
                    return Err(invalid());
                }
                previous = Some(&h.dispatch_id);
            }
            if let Some(next) = &history.next_cursor
                && (history.executions.is_empty() || Some(next.as_str()) != previous)
            {
                return Err(invalid());
            }
            Ok(DeviceResponse::History(history))
        }
        DeviceOperation::Acquire { agent_id, .. }
        | DeviceOperation::Renew {
            lease: LeaseRef { agent_id, .. },
            ..
        } => {
            let lease: Lease = decode(&value, "lease")?;
            let remaining = lease
                .expires_at_ms
                .checked_sub(now_ms()?)
                .filter(|n| *n > 0 && *n <= 31_000)
                .ok_or_else(invalid)?;
            if &lease.agent_id != agent_id
                || lease.owner_user_id != device.user_id()
                || lease.device_id != device.device_id()
                || lease.device_generation
                    != i64::try_from(device.generation()).map_err(|_| invalid())?
                || lease.epoch < 1
                || std::time::Instant::now() + Duration::from_millis(remaining as u64)
                    > device.valid_until() + Duration::from_secs(1)
                || operation
                    .lease()
                    .is_some_and(|reference| reference.epoch != lease.epoch)
            {
                return Err(error(502, "execution_scope_mismatch"));
            }
            Ok(DeviceResponse::Lease(lease))
        }
        DeviceOperation::Poll {
            lease,
            binding_id,
            limit,
        } => {
            let events: Vec<Dispatch> = decode(&value, "events")?;
            if events.len() > *limit {
                return Err(invalid());
            }
            let mut ids = std::collections::BTreeSet::new();
            for event in &events {
                check_dispatch(event, lease, device)?;
                if event.binding_id != *binding_id
                    || !["offered", "acknowledged"].contains(&event.state.as_str())
                    || !ids.insert(&event.id)
                {
                    return Err(invalid());
                }
            }
            Ok(DeviceResponse::Events(events))
        }
        DeviceOperation::Start {
            lease,
            dispatch_id,
            execution_id,
        } => {
            let start: ExecutionStart = decode(&value, "execution")?;
            check_dispatch(&start.dispatch, lease, device)?;
            if &start.dispatch.id != dispatch_id
                || start.dispatch.execution_id.as_deref() != Some(execution_id.as_str())
                || start.dispatch.state != "running"
            {
                return Err(error(502, "execution_scope_mismatch"));
            }
            Ok(DeviceResponse::Started(start))
        }
        DeviceOperation::Processing {
            dispatch_id,
            execution_id,
            ..
        } => {
            let receipt: ProcessingReceipt = decode(&value, "processing")?;
            receipt.validate(dispatch_id, execution_id)?;
            Ok(DeviceResponse::Processing(receipt))
        }
        DeviceOperation::AuthorizeTool {
            lease,
            dispatch_id,
            execution_id,
        } => {
            let dispatch: Dispatch = decode(&value, "dispatch")?;
            check_dispatch(&dispatch, lease, device)?;
            if dispatch.id != *dispatch_id
                || dispatch.execution_id.as_deref() != Some(execution_id.as_str())
                || dispatch.state != "running"
            {
                return Err(error(502, "execution_scope_mismatch"));
            }
            Ok(DeviceResponse::ToolAuthorized(dispatch))
        }
        DeviceOperation::Reply {
            lease,
            dispatch_id,
            execution_id,
            body,
        } => {
            let reply: ReplyReceipt = decode(&value, "reply")?;
            check_reply(&reply, lease, device, dispatch_id, execution_id, body)?;
            if reply.dispatch_epoch != lease.epoch || reply.delivery_epoch != lease.epoch {
                return Err(error(502, "execution_scope_mismatch"));
            }
            Ok(DeviceResponse::ReplyQueued(reply))
        }
        DeviceOperation::ReconcileKnownReply { lease, known } => {
            let reply: ReplyReceipt = decode(&value, "reply")?;
            check_reply(
                &reply,
                lease,
                device,
                &known.dispatch_id,
                &known.execution_id,
                &known.body,
            )?;
            if reply.dispatch_epoch != known.original_epoch
                || reply.binding_id != known.binding_id
                || reply.room_id != known.room_id
                || reply.requester_mxid != known.requester_mxid
                || reply.thread_root != known.thread_root
                || reply.binding_generation != known.binding_generation
                || (reply.state != "sent" && reply.delivery_epoch != lease.epoch)
                || (reply.state == "sent"
                    && (reply.delivery_epoch < 1
                        || reply.delivery_epoch > lease.epoch
                        || reply.matrix_event_id.as_deref().is_none_or(str::is_empty)))
            {
                return Err(error(502, "execution_scope_mismatch"));
            }
            Ok(DeviceResponse::ReplyQueued(reply))
        }
        DeviceOperation::Release { .. } if value["released"] == true => {
            Ok(DeviceResponse::Released)
        }
        DeviceOperation::Ack { .. } if value["acknowledged"] == true => {
            Ok(DeviceResponse::Acknowledged)
        }
        DeviceOperation::Finish { .. } if value["finished"] == true => Ok(DeviceResponse::Finished),
        DeviceOperation::CardReply { lease, dispatch_id, execution_id, body, matrix_event_id } => {
            let reply: ReplyReceipt = decode(&value, "reply")?;
            check_recorded_reply(&reply, lease, device, dispatch_id, execution_id, Some(body), matrix_event_id)?;
            Ok(DeviceResponse::ReplyQueued(reply))
        }
        DeviceOperation::EncryptedReply {
            lease,
            dispatch_id,
            execution_id,
            matrix_event_id,
        } => {
            let reply: ReplyReceipt = decode(&value, "reply")?;
            check_encrypted_reply(
                &reply,
                lease,
                device,
                dispatch_id,
                execution_id,
                matrix_event_id,
            )?;
            Ok(DeviceResponse::ReplyQueued(reply))
        }
        DeviceOperation::E2eeCandidates { .. } => Ok(DeviceResponse::E2eeCandidates(value.clone())),
        DeviceOperation::E2eeConfirm { .. } if value["confirmed"].is_boolean() => Ok(DeviceResponse::E2eeConfirmed),
        DeviceOperation::E2eeDevice { .. } => {
            let grant: E2eeDeviceGrant =
                serde_json::from_value(value.clone()).map_err(|_| invalid())?;
            matrix(&grant.user_id, '@')?;
            if !(1..=255).contains(&grant.device_id.len())
                || !grant.device_id.bytes().all(|b| b.is_ascii_alphanumeric())
                || !(1..=1024).contains(&grant.access_token.len())
                || !(grant.homeserver.starts_with("https://")
                    || grant.homeserver.starts_with("http://127.0.0.1"))
            {
                return Err(invalid());
            }
            Ok(DeviceResponse::E2eeDevice(grant))
        }
        _ => Err(invalid()),
    }
}
impl Console {
    /// Obtain a fresh owner/device snapshot for each operation. No credential,
    /// arbitrary URL or arbitrary remote path is accepted from a caller.
    pub async fn execution_api(
        &self,
        operation: DeviceOperation,
    ) -> Result<DeviceResponse, DeviceError> {
        self.execution_api_inner(operation, None).await
    }
    pub(super) async fn execution_api_pinned(
        &self,
        operation: DeviceOperation,
        expected: &DeviceIdentity,
    ) -> Result<DeviceResponse, DeviceError> {
        self.execution_api_inner(operation, Some(expected)).await
    }
    async fn execution_api_inner(
        &self,
        operation: DeviceOperation,
        expected: Option<&DeviceIdentity>,
    ) -> Result<DeviceResponse, DeviceError> {
        let (path, body) = operation.request()?;
        let device = self.authorized_device().await.map_err(|e| {
            if matches!(e, super::Error::Unavailable) {
                error(503, "server_unavailable")
            } else {
                error(401, "device_authorization_required")
            }
        })?;
        if expected.is_some_and(|pin| pin != &DeviceIdentity::from(&device)) {
            return Err(error(401, "device_authorization_required"));
        }
        let server =
            Url::parse(device.origin()).map_err(|_| error(503, "invalid_server_binding"))?;
        let client = super::server_login::http().map_err(|_| error(503, "server_unavailable"))?;
        let bearer = device
            .bearer()
            .map_err(|_| error(401, "device_authorization_required"))?;
        let mut response = client
            .post(
                server
                    .join(&format!("/hagency/api/v1/execution/{path}"))
                    .unwrap(),
            )
            .bearer_auth(bearer)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| error(503, "server_unavailable"))?;
        let status = response.status().as_u16();
        let mut raw = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| error(503, "server_unavailable"))?
        {
            if raw.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err(error(502, "execution_response_too_large"));
            }
            raw.extend_from_slice(&chunk);
        }
        // A response from an already-started request cannot resurrect local
        // authorization revoked while that request was in flight.
        device
            .bearer()
            .map_err(|_| error(401, "device_authorization_required"))?;
        let current = self.authorized_device().await.map_err(|e| {
            if matches!(e, super::Error::Unavailable) {
                error(503, "server_unavailable")
            } else {
                error(401, "device_authorization_required")
            }
        })?;
        current
            .bearer()
            .map_err(|_| error(401, "device_authorization_required"))?;
        if DeviceIdentity::from(&current) != DeviceIdentity::from(&device) {
            return Err(error(401, "device_authorization_required"));
        }
        let value: Value =
            serde_json::from_slice(&raw).map_err(|_| error(502, "invalid_execution_response"))?;
        if !(200..300).contains(&status) {
            let code = value["code"]
                .as_str()
                .filter(|s| {
                    !s.is_empty()
                        && s.len() <= 80
                        && s.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                })
                .unwrap_or("execution_request_failed");
            return Err(error(status, code));
        }
        parse_response(&operation, value, &device)
    }
}

#[cfg(test)]
mod processing_tests {
    use super::*;
    /// The run card reply is recorded with the server's own digest of the
    /// request (field order dispatchId, executionId, body, matrixEventId);
    /// the encrypted record keeps its body-less digest.
    #[test]
    fn card_reply_request_and_digest_match_the_server_record() {
        let lease = LeaseRef { agent_id: "agent-a".into(), epoch: 1 };
        let op = DeviceOperation::CardReply { lease: lease.clone(), dispatch_id: "d".into(),
            execution_id: "e".into(), body: "42".into(), matrix_event_id: "$card".into() };
        let (path, body) = op.request().unwrap();
        assert_eq!(path, "replies/card");
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["reply"], json!({"dispatchId":"d","executionId":"e","body":"42","matrixEventId":"$card"}));
        let card = serde_json::to_string(&EncryptedDigest { dispatch_id: "d", execution_id: "e", body: Some("42"), matrix_event_id: "$card" }).unwrap();
        assert_eq!(card, r#"{"dispatchId":"d","executionId":"e","body":"42","matrixEventId":"$card"}"#);
        let encrypted = serde_json::to_string(&EncryptedDigest { dispatch_id: "d", execution_id: "e", body: None, matrix_event_id: "$card" }).unwrap();
        assert_eq!(encrypted, r#"{"dispatchId":"d","executionId":"e","matrixEventId":"$card"}"#);
        let bad = DeviceOperation::CardReply { lease, dispatch_id: "d".into(), execution_id: "e".into(), body: String::new(), matrix_event_id: "$card".into() };
        assert!(bad.request().is_err());
    }
    #[test]
    fn encrypted_candidate_confirmation_accepts_real_matrix_event_ids() {
        let operation = |event: &str, root: Option<&str>| DeviceOperation::E2eeConfirm {
            lease: LeaseRef { agent_id: "agent-a".into(), epoch: 1 },
            binding_id: "binding-a".into(), event_id: event.into(),
            mentions: vec!["@agent:example.test".into()], thread_root: root.map(str::to_owned),
        };
        for event in ["$sFSDgP24phmdQkrcSXwQWTZM7-QArRju-59jc9-uNIY", "$legacy:example.test"] {
            let (route, bytes) = operation(event, Some("$opaque_thread_root")).request().unwrap();
            assert_eq!(route, "e2ee-confirm");
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["eventId"], event);
            assert_eq!(body["threadRoot"], "$opaque_thread_root");
        }
        for invalid in ["", "$", "!room:example.test", "$has space", "$line\nfeed"] {
            assert!(operation(invalid, None).request().is_err());
            assert!(operation("$valid", Some(invalid)).request().is_err());
        }
    }
    #[test]
    fn processing_route_is_closed_and_receipt_pins_execution() {
        let op = DeviceOperation::Processing {
            lease: LeaseRef {
                agent_id: "agent-a".into(),
                epoch: 1,
            },
            dispatch_id: "dispatch-a".into(),
            execution_id: "execution-a".into(),
        };
        let (route, bytes) = op.request().unwrap();
        assert_eq!(route, "events/processing");
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({"lease":{"agentId":"agent-a","epoch":1},"dispatchId":"dispatch-a","executionId":"execution-a"})
        );
        let mut receipt: ProcessingReceipt=serde_json::from_value(json!({"id":"processing-a","dispatchId":"dispatch-a","executionId":"execution-a","state":"pending","matrixEventId":null})).unwrap();
        receipt.validate("dispatch-a", "execution-a").unwrap();
        assert!(receipt.validate("dispatch-other", "execution-a").is_err());
        assert!(receipt.validate("dispatch-a", "execution-other").is_err());
        receipt.state = "sent".into();
        assert!(receipt.validate("dispatch-a", "execution-a").is_err());
        receipt.matrix_event_id = Some("$eyes:test".into());
        receipt.validate("dispatch-a", "execution-a").unwrap();
        receipt.matrix_event_id = Some("!room:test".into());
        assert!(receipt.validate("dispatch-a", "execution-a").is_err());
        receipt.state = "arbitrary".into();
        assert!(receipt.validate("dispatch-a", "execution-a").is_err());
    }
}
