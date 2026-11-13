//! Ordinary Matrix devices on the enrolled execution computer. Wire records
//! contain event references; decrypted prompts and answers stay on this host.
use super::*;
use crate::console::{
    device_execution::E2eeDeviceGrant,
    owned_runtime::e2ee::{AgentDevice, forget_grant, load_grant, save_grant},
};
use hagency_agent_local::inbox::Dispatch;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::LazyLock,
};
use tokio::sync::Mutex;

/// One slot per Agent device. The map's own lock is held only to find a
/// slot; a slot's lock serializes opening that one device, so a slow first
/// sync for one Agent never holds up another.
type DeviceSlot = Arc<Mutex<Option<Arc<AgentDevice>>>>;
static DEVICES: LazyLock<std::sync::Mutex<HashMap<String, DeviceSlot>>> =
    LazyLock::new(Default::default);
static SENDS: Mutex<()> = Mutex::const_new(());
// A process-local hint, never execution authority. Every subscriber still
// checks its own account, device, assignment and budget before doing work.
static WORK_CHANGED: LazyLock<tokio::sync::watch::Sender<()>> =
    LazyLock::new(|| tokio::sync::watch::channel(()).0);
pub(super) enum CryptoAction {
    Poll {
        after: Option<String>,
    },
    Device {
        assignment: String,
    },
    Confirm {
        assignment: String,
        event: String,
        addressed: bool,
        mentioned: bool,
        thread: Option<String>,
    },
    Send {
        work: String,
    },
}
impl CryptoAction {
    pub(super) fn body(self) -> (String, Value) {
        match self {
            Self::Poll { after } => ("crypto/poll".into(), json!({"after":after})),
            Self::Device { assignment } => {
                ("crypto/device".into(), json!({"assignmentId":assignment}))
            }
            Self::Confirm {
                assignment,
                event,
                addressed,
                mentioned,
                thread,
            } => (
                "crypto/confirm".into(),
                json!({"assignmentId":assignment,"eventId":event,"addressed":addressed,"mentioned":mentioned,"threadRoot":thread}),
            ),
            Self::Send { work } => ("crypto/send".into(), json!({"workId":work})),
        }
    }
}
impl From<CryptoAction> for Action {
    fn from(v: CryptoAction) -> Self {
        Self::Crypto(v)
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Poll {
    scopes: Vec<CryptoScope>,
    next_after: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CryptoScope {
    assignment_id: String,
    puppet_mxid: String,
    room_id: String,
    manager_mxid: String,
    direct: bool,
    events: Vec<Candidate>,
}
#[derive(Deserialize)]
struct Candidate {
    event_id: String,
    room_id: String,
    sender_mxid: String,
}
fn sha(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn root(io: &Io, b: &Bound) -> Result<PathBuf, NativeError> {
    let root = io.crypto_directory()?.join(sha(&format!(
        "{}:{}:{}",
        b.scope.device.origin(),
        b.scope.device.owner_mxid(),
        b.scope.device.device_id()
    )));
    hagency_store::private::directory(&root).map_err(store)?;
    Ok(root)
}
pub(super) fn message_root(io: &Io, b: &Bound, assignment: &str) -> Result<PathBuf, NativeError> {
    crate::console::owned_runtime::e2ee::agent_root(&root(io, b)?, assignment)
        .map_err(|_| failure(503, "shared_message_storage_unavailable"))
}
pub(super) async fn send_message(
    io: &Io, b: &Bound, work: &SharedWorkItem,
    outbox: &crate::console::owned_runtime::messages::Outbox,
) -> Result<bool, NativeError> {
    let Some(pending) = outbox.next().map_err(|_| invalid())? else { return Ok(false); };
    super::files::authorize(io, &work.id).await
        .map_err(|_| failure(503, "shared_message_delivery_pending"))?;
    // Only the Matrix boundary is substituted in supervised-process tests.
    #[cfg(test)]
    if let Some(fixture) = &io.crypto_fixture {
        let event = format!("$progress_{}:test", sha(&pending.txn));
        let mut txns = fixture.message_transactions.lock().unwrap();
        if let Some((previous_work, previous_text)) = txns.get(&pending.txn) {
            if previous_work != &work.id || previous_text != &pending.text { return Err(invalid()); }
        } else {
            txns.insert(pending.txn.clone(), (work.id.clone(), pending.text.clone()));
            fixture.sent.lock().unwrap().push((work.id.clone(), pending.text));
        }
        drop(txns);
        if fixture.lose_message_response.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(failure(503, "shared_message_delivery_pending"));
        }
        outbox.sent(pending.sequence, &event)
            .map_err(|_| invalid())?;
        return Ok(true);
    }
    let _ = pending;
    let device = device(io, b, &work.assignment_id).await?;
    outbox.send_next(&device, work.encrypted).await
        .map_err(|_| failure(503, "shared_message_delivery_pending"))
}
pub(super) fn file_records(io: &Io, b: &Bound, assignment: &str) -> Result<PathBuf, NativeError> {
    let parent = root(io,b)?.join("chat-file-receipts");
    hagency_store::private::directory(&parent).map_err(store)?;
    Ok(parent.join(sha(assignment)))
}
fn device_key(root: &Path, b: &Bound, assignment: &str) -> String {
    format!(
        "{}:{assignment}:{}",
        root.display(),
        b.scope.device.generation()
    )
}
fn slot(key: &str) -> DeviceSlot {
    let mut devices = DEVICES.lock().unwrap_or_else(|e| e.into_inner());
    devices.entry(key.to_owned()).or_default().clone()
}
/// Where this Agent's posted question cards are recorded, so a restart can
/// still close them.
pub(super) fn question_records(
    io: &Io,
    b: &Bound,
    assignment: &str,
) -> Result<PathBuf, NativeError> {
    Ok(root(io, b)?
        .join("agent-question-cards")
        .join(format!("{}.json", sha(assignment))))
}
/// The Agent's device when it is open already, without opening one: never a
/// device login or a first sync. `None` while it is being opened.
pub(super) fn opened(
    io: &Io,
    b: &Bound,
    assignment: &str,
) -> Result<Option<Arc<AgentDevice>>, NativeError> {
    let key = device_key(&root(io, b)?, b, assignment);
    let slot = DEVICES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned();
    Ok(slot
        .and_then(|slot| slot.try_lock().ok().and_then(|d| d.clone()))
        .filter(|d| !d.is_revoked()))
}
pub(super) async fn device(
    io: &Io,
    b: &Bound,
    assignment: &str,
) -> Result<Arc<AgentDevice>, NativeError> {
    let device = open_device(io, b, assignment).await?;
    crate::console::owned_runtime::run_controls::device_ready(device.clone());
    // Card outcomes this Agent still owes its rooms go out while it is in use.
    if let Ok(records) = question_records(io, b, assignment) {
        super::questions::device_ready(
            Arc::new(super::questions::Source::Device(device.clone())),
            Some(records),
        );
    }
    Ok(device)
}
async fn open_device(
    io: &Io,
    b: &Bound,
    assignment: &str,
) -> Result<Arc<AgentDevice>, NativeError> {
    let root = root(io, b)?;
    let slot = slot(&device_key(&root, b, assignment));
    let mut slot = slot.lock().await;
    if let Some(d) = slot.as_ref()
        && !d.is_revoked()
    {
        return Ok(d.clone());
    }
    let generation = b.scope.device.generation();
    // First open in this process: reuse the grant an earlier daemon saved, so a
    // restart costs no Matrix login. A revoked device always takes a new grant.
    if slot.is_none() {
        if let Some(saved) = load_grant(&root, assignment, b.scope.device.origin(), generation) {
            if let Ok(d) =
                AgentDevice::open_notifying(saved, &root, assignment, Some(WORK_CHANGED.clone()))
                    .await
            {
                let d = Arc::new(d);
                *slot = Some(d.clone());
                return Ok(d);
            }
            forget_grant(&root, assignment);
        }
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Grant {
        user_id: String,
        device_id: String,
        access_token: String,
    }
    let grant: Timed<Grant> = b
        .request(
            CryptoAction::Device {
                assignment: assignment.into(),
            }
            .into(),
            false,
        )
        .await?;
    if !grant.value.device_id.starts_with("HAGE2EE")
        || !grant
            .value
            .device_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric())
        || grant.value.device_id.len() > 64
    {
        return Err(invalid());
    }
    let grant = E2eeDeviceGrant {
        user_id: grant.value.user_id,
        device_id: grant.value.device_id,
        access_token: grant.value.access_token,
        homeserver: b.scope.device.origin().into(),
    };
    let saved = grant.clone();
    let d = Arc::new(
        AgentDevice::open_notifying(grant, &root, assignment, Some(WORK_CHANGED.clone()))
            .await
            .map_err(|_| failure(503, "shared_crypto_device_unavailable"))?,
    );
    save_grant(&root, assignment, &saved, generation);
    *slot = Some(d.clone());
    Ok(d)
}
impl SharedExecutionTransport {
    pub fn work_changes(&self) -> tokio::sync::watch::Receiver<()> {
        WORK_CHANGED.subscribe()
    }

    /// Poll before demand admission, even while the provider is stopped. Device
    /// keys must be advertised before another client can share room keys.
    pub(super) async fn pump_crypto(&self) -> Result<(), NativeError> {
        let b = self.io.bound().await?;
        if !b
            .demand_discovery()
            .await?
            .capabilities
            .iter()
            .any(|v| v == "shared-agent-e2ee-v1")
        {
            return Ok(());
        }
        let mut after = None;
        loop {
            let reply: Timed<Poll> = b
                .request(
                    CryptoAction::Poll {
                        after: after.clone(),
                    }
                    .into(),
                    false,
                )
                .await?;
            if reply.value.scopes.len() > 256
                || reply
                    .value
                    .next_after
                    .as_ref()
                    .is_some_and(|n| after.as_ref().is_some_and(|a| n <= a))
            {
                return Err(invalid());
            }
            for scope in reply.value.scopes {
                super::super::key(&scope.assignment_id)?;
                let d = device(&self.io, &b, &scope.assignment_id).await?;
                for event in scope.events.into_iter().take(16) {
                    if event.room_id != scope.room_id || event.sender_mxid == scope.puppet_mxid {
                        continue;
                    }
                    let message = match d
                        .read_candidate(&event.room_id, &event.event_id, &event.sender_mxid)
                        .await
                    {
                        Ok(message) => message,
                        Err(_) => continue, // Missing keys retain the original event for retry.
                    };
                    let mentioned = message
                        .as_ref()
                        .is_some_and(|m| m.mentions.contains(&scope.puppet_mxid));
                    // The server uses fresh membership to decide whether this
                    // owner's original conversation is still one-to-one.
                    let addressed = message.is_some()
                        && (mentioned || scope.direct && event.sender_mxid == scope.manager_mxid);
                    let thread = message.as_ref().and_then(|m| m.thread_root.clone());
                    let _: Timed<Value> = b
                        .request(
                            CryptoAction::Confirm {
                                assignment: scope.assignment_id.clone(),
                                event: event.event_id,
                                addressed,
                                mentioned,
                                thread,
                            }
                            .into(),
                            false,
                        )
                        .await?;
                }
            }
            let Some(next) = reply.value.next_after else {
                break;
            };
            after = Some(next);
        }
        Ok(())
    }
}
pub(super) async fn input(io: &Io, b: &Bound, w: &SharedWorkItem) -> Result<String, NativeError> {
    if !w.encrypted {
        return Err(invalid());
    }
    #[cfg(test)]
    if let Some(fixture) = &io.crypto_fixture {
        tokio::time::sleep(fixture.input_delay).await;
        return fixture
            .input
            .clone()
            .ok_or_else(|| failure(503, "shared_room_keys_pending"));
    }
    let d = device(io, b, &w.assignment_id).await?;
    let m = d
        .read_candidate(&w.room_id, &w.event_id, &w.requester_mxid)
        .await
        .map_err(|_| failure(503, "shared_room_keys_pending"))?
        .ok_or_else(invalid)?;
    if m.thread_root.as_deref().unwrap_or(&w.event_id) != w.thread_root {
        return Err(invalid());
    }
    Ok(m.body)
}
fn dispatch(w: &SharedWorkItem) -> Dispatch {
    Dispatch {
        id: w.id.clone(),
        binding_id: w.assignment_id.clone(),
        agent_id: w.assignment_id.clone(),
        event_id: w.event_id.clone(),
        room_id: w.room_id.clone(),
        requester_mxid: w.requester_mxid.clone(),
        thread_root: w.thread_root.clone(),
        body: String::new(),
        state: "started".into(),
        binding_generation: 1,
        dispatch_epoch: None,
        dispatch_device_id: None,
        execution_id: Some(w.execution_id.clone()),
        outcome: None,
    }
}

async fn send_reply(
    io: &Io,
    b: &Bound,
    w: &SharedWorkItem,
    body: &str,
) -> Result<String, NativeError> {
    #[cfg(test)]
    if let Some(fixture) = &io.crypto_fixture {
        fixture
            .sent
            .lock()
            .unwrap()
            .push((w.id.clone(), body.into()));
        return Ok(format!("$encrypted_reply_{}:test", w.id));
    }
    device(io, b, &w.assignment_id)
        .await?
        .send_shared_reply(&dispatch(w), body)
        .await
        .map_err(|_| failure(503, "shared_encrypted_send_pending"))
}

// Only the Matrix I/O boundary is substituted in supervised-process tests.
// Input pins, authority checks, durable delivery projection and replay stay real.
#[cfg(test)]
pub(super) struct CryptoFixture {
    input: Option<String>,
    input_delay: Duration,
    sent: Arc<std::sync::Mutex<Vec<(String, String)>>>,
    message_transactions: std::sync::Mutex<std::collections::BTreeMap<String, (String, String)>>,
    lose_message_response: std::sync::atomic::AtomicBool,
    pub(super) history: Option<Arc<super::history::Fixture>>,
    pub(super) files: Option<Arc<super::files::tests::Endpoint>>,
    pub(super) questions: Option<Arc<super::questions::Fixture>>,
    pub(super) approvals: Option<Arc<ApprovalFixture>>,
}
#[cfg(test)]
pub(in crate::console::native) struct ApprovalFixture {
    pub(super) allow: bool,
    pub(super) requests: std::sync::Mutex<Vec<Value>>,
}
#[cfg(test)]
impl ApprovalFixture {
    pub(in crate::console::native) fn requests(&self) -> Vec<Value> { self.requests.lock().unwrap().clone() }
}
#[cfg(test)]
impl SharedExecutionTransport {
    pub(in crate::console::native) fn with_crypto_fixture(
        mut self,
        input: Option<String>,
    ) -> (Self, Arc<std::sync::Mutex<Vec<(String, String)>>>) {
        let sent = Arc::new(std::sync::Mutex::new(Vec::new()));
        self.io.crypto_fixture = Some(Arc::new(CryptoFixture {
            input,
            input_delay: Duration::ZERO,
            sent: sent.clone(),
            message_transactions: Default::default(),
            lose_message_response: std::sync::atomic::AtomicBool::new(false),
            history: None,
            files: None,
            questions: None,
            approvals: None,
        }));
        (self, sent)
    }
    /// The fake homeserver accepts one event but loses its HTTP response.
    /// A retry of that transaction must return the same event, not post twice.
    pub(in crate::console::native) fn with_lost_message_response(self) -> Self {
        self.io.crypto_fixture.as_ref().unwrap().lose_message_response
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self
    }
    /// A local decision fixture; no live Matrix device or owner is contacted.
    pub(in crate::console::native) fn with_approval_fixture(mut self, allow: bool) -> (Self, Arc<ApprovalFixture>) {
        let fixture = Arc::new(ApprovalFixture { allow, requests: std::sync::Mutex::new(Vec::new()) });
        Arc::get_mut(self.io.crypto_fixture.as_mut().unwrap()).unwrap().approvals = Some(fixture.clone());
        (self, fixture)
    }
    pub(in crate::console::native) fn with_crypto_input_delay(mut self, delay: Duration) -> Self {
        Arc::get_mut(self.io.crypto_fixture.as_mut().unwrap())
            .unwrap()
            .input_delay = delay;
        self
    }
    /// Question cards go to this fixture room; only `members` can answer, and
    /// each card waits `wait` before the model decides on its own.
    pub(in crate::console::native) fn with_question_fixture(
        self,
        members: &[&str],
        wait: Duration,
    ) -> (Self, Arc<super::questions::Fixture>) {
        self.with_question_fixture_records(members, wait, None)
    }
    /// The same, with the Agent's posted cards recorded at `records`.
    pub(in crate::console::native) fn with_question_fixture_records(
        mut self,
        members: &[&str],
        wait: Duration,
        records: Option<std::path::PathBuf>,
    ) -> (Self, Arc<super::questions::Fixture>) {
        let questions = Arc::new(super::questions::Fixture::new(members, wait, records));
        Arc::get_mut(self.io.crypto_fixture.as_mut().unwrap())
            .unwrap()
            .questions = Some(questions.clone());
        (self, questions)
    }
    pub(in crate::console::native) fn with_file_fixture(mut self, records: PathBuf, room: String) -> (Self, Arc<super::files::tests::Endpoint>) {
        let files = Arc::new(super::files::tests::Endpoint::new(records,room));
        Arc::get_mut(self.io.crypto_fixture.as_mut().unwrap()).unwrap().files = Some(files.clone());
        (self,files)
    }
    pub(in crate::console::native) fn with_history_fixture(
        mut self,
        room: String,
        anchor: String,
        pages: Vec<Vec<Value>>,
    ) -> (Self, Arc<std::sync::Mutex<Vec<String>>>) {
        let reads = Arc::new(std::sync::Mutex::new(Vec::new()));
        let fixture = super::history::Fixture {
            room,
            anchor,
            pages,
            reads: reads.clone(),
        };
        Arc::get_mut(self.io.crypto_fixture.as_mut().unwrap())
            .unwrap()
            .history = Some(Arc::new(fixture));
        (self, reads)
    }
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Sent {
    source_digest: String,
    body: String,
    sent_at_ms: u64,
    content: Value,
}
// A delivery record contains the Matrix content, not merely a short secret.
// Allow the 32 KiB answer + 64 KiB formatted answer with worst-case JSON
// escaping, plus bounded Matrix identifiers and the delivery projection.
const MAX_SENT_BYTES: u64 = 640 * 1024;

fn read_sent(path: &Path) -> Result<Option<Sent>, NativeError> {
    let file = match hagency_store::private::open(path, false) {
        Ok(file) => file,
        Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(store(error)),
    };
    if file
        .metadata()
        .map_err(hagency_store::Error::from)
        .map_err(store)?
        .len()
        > MAX_SENT_BYTES
    {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(MAX_SENT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(hagency_store::Error::from)
        .map_err(store)?;
    if bytes.len() as u64 > MAX_SENT_BYTES {
        return Err(invalid());
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| invalid())
}

fn encode_sent(sent: &Sent) -> Result<Vec<u8>, NativeError> {
    let bytes = serde_json::to_vec(sent).map_err(|_| invalid())?;
    if bytes.len() as u64 > MAX_SENT_BYTES {
        return Err(invalid());
    }
    Ok(bytes)
}
/// The ledger retains the original full receipt locally. Its wire projection
/// replaces only the answer with a closed Matrix delivery receipt and hashes
/// that projection separately; retries never confuse the two digests.
pub(super) async fn wire_receipt(
    io: &Io,
    b: &Bound,
    registry: &OwnerResourceRegistry,
    command: &SharedRuntimeCommand,
    w: &SharedWorkItem,
    r: &SharedWorkReceipt,
) -> Result<SharedWorkReceipt, NativeError> {
    // Recovery reuses the original native identities and Matrix transactions;
    // the provider process and its already-accounted call are never repeated.
    let card = crate::agent_run_controls::Card::for_shared(w, &r.call_id).map_err(|_| invalid())?;
    let message_directory = message_root(io, b, &w.assignment_id)?;
    if crate::console::owned_runtime::messages::Outbox::exists(&message_directory, &card) {
        let messages = crate::console::owned_runtime::messages::Outbox::open(&message_directory, card)
            .map_err(|_| failure(503, "shared_message_storage_unavailable"))?;
        if messages.next().map_err(|_| invalid())?.is_some() {
            while messages.next().map_err(|_| invalid())?.is_some() {
                send_message(io, b, w, &messages).await?;
            }
        }
    }
    if !w.encrypted {
        return Ok(r.clone());
    }
    let mut wire = r.clone();
    let Some(body) = &r.body else { return Ok(wire) };
    let dir = root(io, b)?.join("sent");
    hagency_store::private::directory(&dir).map_err(store)?;
    let path = dir.join(sha(&w.id));
    let _send = SENDS.lock().await;
    let saved = match read_sent(&path)? {
        Some(saved) => saved,
        None => {
            let authorized: Result<Timed<Value>, NativeError> = b
                .request(CryptoAction::Send { work: w.id.clone() }.into(), false)
                .await;
            let event = match authorized {
                Ok(_) => Some(send_reply(io, b, w, body).await?),
                // The model already ran. Preserve usage even when permission to
                // publish was revoked; do not claim an uncertain send never ran.
                Err(e) if matches!(e.status, 401 | 403 | 409) => None,
                Err(e) => return Err(e),
            };
            let content = crate::console::owned_runtime::e2ee::reply_content(&dispatch(w), body)
                .map_err(|_| invalid())?;
            let saved=Sent {
                source_digest:sha(body),
                body:json!({"algorithm":"m.megolm.v1.aes-sha2","roomId":w.room_id,"eventId":event,"delivery":if event.is_some(){"sent"}else{"unknown"}}).to_string(),
                content:serde_json::to_value(content).map_err(|_|invalid())?,
                sent_at_ms:std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|_|invalid())?.as_millis() as u64,
            };
            let bytes = encode_sent(&saved)?;
            // Atomic replacement makes a crash leave either no receipt or the complete receipt.
            hagency_store::private::replace(&path, &bytes).map_err(store)?;
            saved
        }
    };
    if saved.source_digest != sha(body) {
        return Err(invalid());
    }
    let metadata: Value = serde_json::from_str(&saved.body).map_err(|_| invalid())?;
    if let Some(event) = metadata["eventId"].as_str() {
        let content = &saved.content;
        let receipt = SharedReplyDelivery {
            version: 1,
            work_id: w.id.clone(),
            work_digest: digest(w)?,
            assignment_id: w.assignment_id.clone(),
            runtime_command_digest: w.runtime_command_digest.clone(),
            room_id: w.room_id.clone(),
            thread_root: w.thread_root.clone(),
            source_event_id: w.event_id.clone(),
            matrix_txn_id: format!("hagency_shared_{}", w.id),
            matrix_event_id: event.into(),
            matrix_content_digest: digest(&content)?,
            body: body.clone(),
            formatted_body: content["formatted_body"].as_str().map(str::to_owned),
            body_digest: sha(body),
            sent_at_ms: saved.sent_at_ms,
        };
        let record = SharedDeliveryRecord {
            receipt_digest: digest(&receipt)?,
            receipt,
        };
        registry
            .record_shared_reply_delivery(b.cleanup(registry, command).await?, w.clone(), record)
            .await
            .map_err(store)?;
    }
    wire.body_digest = Some(sha(&saved.body));
    wire.body = Some(saved.body);
    Ok(wire)
}

#[cfg(test)]
mod delivery_file_tests {
    use super::*;

    fn saved(content: Value) -> Sent {
        Sent {
            source_digest: "a".repeat(64),
            body: json!({"algorithm":"m.megolm.v1.aes-sha2","roomId":format!("!{}:example.test", "r".repeat(230)),"eventId":format!("${}", "e".repeat(254)),"delivery":"sent"}).to_string(),
            sent_at_ms: 1,
            content,
        }
    }

    #[test]
    fn encrypted_delivery_record_replays_above_short_secret_limit() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sent");
        let original = saved(json!({"msgtype":"m.text","body":"offline encrypted answer"}));
        let bytes = encode_sent(&original).unwrap();
        assert!(bytes.len() > 512);
        hagency_store::private::write_new(&path, &bytes).unwrap();
        assert!(hagency_store::private::read_secret(&path).is_err());
        let replay = read_sent(&path).unwrap().unwrap();
        assert_eq!(encode_sent(&replay).unwrap(), bytes);
    }

    #[test]
    fn encrypted_delivery_record_accepts_maximum_escaped_content_and_rejects_oversize() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sent");
        let original = saved(
            json!({"msgtype":"m.text","body":"\u{0000}".repeat(32*1024),"formatted_body":"\u{0000}".repeat(64*1024),"format":"org.matrix.custom.html"}),
        );
        let bytes = encode_sent(&original).unwrap();
        assert!(bytes.len() > 576 * 1024);
        hagency_store::private::write_new(&path, &bytes).unwrap();
        assert_eq!(
            encode_sent(&read_sent(&path).unwrap().unwrap()).unwrap(),
            bytes
        );
        let oversized = saved(json!({"body":"x".repeat(MAX_SENT_BYTES as usize)}));
        assert!(encode_sent(&oversized).is_err());
        hagency_store::private::replace(&path, &vec![b' '; MAX_SENT_BYTES as usize + 1]).unwrap();
        assert!(read_sent(&path).is_err());
        assert!(read_sent(&root.path().join("missing")).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn encrypted_delivery_record_rejects_public_files_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sent");
        hagency_store::private::write_new(
            &path,
            &encode_sent(&saved(json!({"body":"done"}))).unwrap(),
        )
        .unwrap();
        let link = root.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(read_sent(&link).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_sent(&path).is_err());
    }
}

#[cfg(test)]
mod device_slot_tests {
    use super::*;

    #[tokio::test]
    async fn one_agent_device_opening_never_holds_up_another() {
        // A first sync can retry for minutes while its own slot is held.
        let first = slot("slot-test:first");
        let _opening = first.lock().await;
        let second = slot("slot-test:second");
        assert!(
            tokio::time::timeout(Duration::from_millis(200), second.lock())
                .await
                .is_ok(),
            "another Agent's device opens meanwhile"
        );
        assert!(
            DEVICES.try_lock().is_ok(),
            "the device map is never held across an open"
        );
        // The same Agent's callers wait for that one opening.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), slot("slot-test:first").lock())
                .await
                .is_err()
        );
    }
}
