//! The executor's own Matrix device for encrypted personal and shared Rooms. The
//! server routes only ciphertext and records receipts: this device decrypts the
//! owner's request for the local turn and sends the encrypted answer itself.
//! Decrypted text stays in memory, the local ledger and the provider session.
use super::super::device_execution::E2eeDeviceGrant;
use super::*;
use matrix_sdk::{
    Client, SessionMeta, SessionTokens,
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    deserialized_responses::{
        DecryptedRoomEvent, TimelineEventKind, VerificationLevel, VerificationState,
    },
    ruma::{
        OwnedEventId, OwnedRoomId, OwnedTransactionId, OwnedUserId,
        api::error::ErrorKind,
        events::{
            AnyMessageLikeEvent, AnyTimelineEvent, MessageLikeEvent,
            reaction::ReactionEventContent,
            relation::{Annotation, Thread},
            room::message::{MessageType, Relation, RoomMessageEventContent},
        },
    },
    store::RoomLoadSettings,
};
use sha2::{Digest, Sha256};
mod history;
mod documents;
mod files;
pub(in crate::console) use documents::RoomFile;
pub(in crate::console) use history::HistoryBatch;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        LazyLock,
        atomic::{AtomicBool, Ordering},
    },
};

/// Room keys travel as to-device messages and can arrive after the routed event.
const KEY_WAIT: Duration = Duration::from_secs(60);
/// Encryption grows content by about a third; stay inside the 64 KiB event.
const MARKDOWN_BYTES: usize = 16 * 1024;
const PLAIN_BYTES: usize = 42 * 1024;

/// One device per Agent per process. A new grant rotates the device's token, so
/// a second client for the same Agent would revoke the first.
static DEVICES: LazyLock<Mutex<HashMap<String, Arc<AgentDevice>>>> =
    LazyLock::new(Default::default);

/// Membership lookups can be cancelled when the native turn completes. Until
/// the whole batch returns to its caller, keep every taken event recoverable.
struct ActionCheckBatch<T, F: FnMut(Vec<T>)> {
    events: Vec<T>,
    requeue: F,
}
impl<T, F: FnMut(Vec<T>)> Drop for ActionCheckBatch<T, F> {
    fn drop(&mut self) {
        (self.requeue)(std::mem::take(&mut self.events));
    }
}

pub(in crate::console) struct AgentDevice {
    client: Client,
    pub(in crate::console) approvals: crate::encrypted_approvals::Inbox,
    /// Room members' answers to this Agent's question cards.
    pub(in crate::console) questions: crate::agent_questions::Inbox,
    pub(in crate::console) question_records: std::path::PathBuf,
    run_controls: crate::agent_questions::Inbox,
    pub(in crate::console) run_records: std::path::PathBuf,
    revoked: Arc<AtomicBool>,
    sync: tokio::task::AbortHandle,
}
impl Drop for AgentDevice {
    fn drop(&mut self) {
        self.sync.abort();
    }
}

/// The Agent device must exist before the owner's client encrypts for it, so an
/// worker opens it at start. Best effort: plaintext Rooms never
/// need it, and an encrypted request retries the grant itself.
pub(super) async fn prepare<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    config: &StartConfig,
    cancel: &mut watch::Receiver<bool>,
) {
    match tokio::time::timeout(
        Duration::from_secs(20),
        device(transport, lease, config, cancel),
    )
    .await
    {
        Ok(Ok(_)) => tracing::debug!(stage = "agent_device", "Agent device ready"),
        Ok(Err(error)) => {
            tracing::warn!(stage = "agent_device", %error, "Agent device unavailable")
        }
        Err(_) => tracing::warn!(stage = "agent_device", "Agent device start timed out"),
    }
}

pub(super) async fn device<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    config: &StartConfig,
    cancel: &mut watch::Receiver<bool>,
) -> Result<Arc<AgentDevice>, RuntimeError> {
    // Use the account's private namespace, independent of provider HOME or
    // per-binding lease epochs. All bindings share this agent's one device.
    // The normal provider directory already lived here, preserving its store.
    let (directory, identity) = if let Some(host) = transport.approval_host() {
        let root = host
            .console
            .0
            .server_login
            .state_directory()
            .ok_or(RuntimeError::Profile)?;
        let p = &host.pin;
        let directory = crate::console::owned_agents::journal_profile_directory(
            &root, &p.origin, &p.issuer, &p.subject, &p.owner,
        )
        .map_err(|_| RuntimeError::Profile)?;
        let identity = serde_json::to_string(p).map_err(|_| RuntimeError::Profile)?;
        (directory, identity)
    } else {
        (
            config
                .profile
                .home
                .parent()
                .ok_or(RuntimeError::Profile)?
                .to_path_buf(),
            lease.epoch.to_string(),
        )
    };
    let key = format!("{}:{}:{identity}", directory.display(), config.agent_id);
    let mut devices = DEVICES.lock().await;
    if let Some(device) = devices.get(&key)
        && !device.revoked.load(Ordering::Acquire)
    {
        super::run_controls::device_ready(device.clone());
        return Ok(device.clone());
    }
    let Response::E2eeDevice(grant) = operation(
        transport,
        cancel,
        Op::E2eeDevice {
            lease: lease.clone(),
        },
    )
    .await?
    else {
        return Err(RuntimeError::Transport);
    };
    let device = Arc::new(AgentDevice::open(grant, &directory, &config.agent_id).await?);
    super::questions::device_ready(device.clone());
    super::run_controls::device_ready(device.clone());
    devices.insert(key, device.clone());
    Ok(device)
}

/// Addressing is checked before the server creates a dispatch or a local call
/// hold. Missing room keys leave the original candidate available for retry.
pub(super) async fn pump_candidates<T: Transport>(
    transport: &T,
    lease: &LeaseRef,
    config: &StartConfig,
    cancel: &mut watch::Receiver<bool>,
) -> Result<(), RuntimeError> {
    let d = device(transport, lease, config, cancel).await?;
    let Response::E2eeCandidates(value) = operation(
        transport,
        cancel,
        Op::E2eeCandidates {
            lease: lease.clone(),
            binding_id: config.binding_id.clone(),
        },
    )
    .await?
    else {
        return Err(RuntimeError::Transport);
    };
    #[derive(Deserialize)]
    struct Candidate {
        event_id: String,
        room_id: String,
        sender_mxid: String,
    }
    let events = value["events"]
        .as_array()
        .filter(|v| v.len() <= 16)
        .ok_or(RuntimeError::Transport)?;
    for raw in events {
        let c: Candidate =
            serde_json::from_value(raw.clone()).map_err(|_| RuntimeError::Transport)?;
        let message = match d
            .read_candidate(&c.room_id, &c.event_id, &c.sender_mxid)
            .await
        {
            Ok(m) => m,
            Err(error) => {
                tracing::debug!(stage = "encrypted_candidate", room_id = %c.room_id, event_id = %c.event_id, %error, "Encrypted candidate is not ready; preserving it for retry");
                continue;
            }
        };
        let (mentions, thread_root) = message
            .map(|m| (m.mentions.into_iter().collect(), m.thread_root))
            .unwrap_or_default();
        operation(
            transport,
            cancel,
            Op::E2eeConfirm {
                lease: lease.clone(),
                binding_id: config.binding_id.clone(),
                event_id: c.event_id,
                mentions,
                thread_root,
            },
        )
        .await?;
    }
    Ok(())
}

fn private_directory(path: &Path) -> Result<(), RuntimeError> {
    hagency_store::private::directory(path).map_err(|_| RuntimeError::Profile)
}
/// The SQLite stores hold the device's Olm account and room keys, encrypted
/// with a random passphrase that never leaves this owner's private directory.
fn store_passphrase(root: &Path) -> Result<String, RuntimeError> {
    let path = root.join("store-key");
    match hagency_store::private::read_secret(&path) {
        Ok(bytes) => String::from_utf8(bytes)
            .ok()
            .filter(|key| key.len() == 64)
            .ok_or(RuntimeError::Profile),
        Err(hagency_store::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0u8; 32];
            getrandom::fill(&mut key).map_err(|_| RuntimeError::Profile)?;
            let key = key.iter().map(|b| format!("{b:02x}")).collect::<String>();
            hagency_store::private::write_new(&path, key.as_bytes())
                .map_err(|_| RuntimeError::Profile)?;
            Ok(key)
        }
        Err(_) => Err(RuntimeError::Profile),
    }
}
/// File in an Agent's device directory that keeps its last working grant.
const SAVED_GRANT: &str = "device-grant-v1";

/// Each grant is a Matrix login for the Agent's device, and the homeserver
/// limits logins per address (Palpo's default: 5, then one about every 5.5
/// minutes). Every Agent device logs in from the server's own address, so a
/// daemon that logged in every device on each start ran out after a few
/// restarts and its Agents could not read encrypted messages for many minutes.
/// The grant is saved beside the device's store (private file, like the store
/// key) and reused by the next process for the same device generation; a
/// rejected or stale grant falls back to a fresh one.
fn saved_grant_path(directory: &Path, agent: &str) -> std::path::PathBuf {
    directory
        .join("agent-devices")
        .join(format!("{:x}", Sha256::digest(agent.as_bytes())))
        .join(SAVED_GRANT)
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedGrant {
    version: u8,
    generation: u64,
    homeserver: String,
    user_id: String,
    device_id: String,
    access_token: String,
}

pub(in crate::console) fn load_grant(
    directory: &Path,
    agent: &str,
    homeserver: &str,
    generation: u64,
) -> Option<E2eeDeviceGrant> {
    let bytes = hagency_store::private::read_secret(&saved_grant_path(directory, agent)).ok()?;
    let saved: SavedGrant = serde_json::from_slice(&bytes).ok()?;
    (saved.version == 1
        && saved.generation == generation
        && saved.homeserver == homeserver
        && saved.device_id.starts_with("HAGE2EE")
        && saved.device_id.len() <= 64
        && saved.device_id.bytes().all(|b| b.is_ascii_alphanumeric())
        && !saved.access_token.is_empty())
    .then_some(E2eeDeviceGrant {
        user_id: saved.user_id,
        device_id: saved.device_id,
        access_token: saved.access_token,
        homeserver: saved.homeserver,
    })
}

/// Best effort: a grant that cannot be saved only costs a login next start.
pub(in crate::console) fn save_grant(
    directory: &Path,
    agent: &str,
    grant: &E2eeDeviceGrant,
    generation: u64,
) {
    let saved = SavedGrant {
        version: 1,
        generation,
        homeserver: grant.homeserver.clone(),
        user_id: grant.user_id.clone(),
        device_id: grant.device_id.clone(),
        access_token: grant.access_token.clone(),
    };
    if let Ok(bytes) = serde_json::to_vec(&saved) {
        let _ = hagency_store::private::replace(&saved_grant_path(directory, agent), &bytes);
    }
}

pub(in crate::console) fn forget_grant(directory: &Path, agent: &str) {
    let _ = std::fs::remove_file(saved_grant_path(directory, agent));
}

fn unknown_token(error: &matrix_sdk::Error) -> bool {
    matches!(
        error.client_api_error_kind(),
        Some(ErrorKind::UnknownToken(_))
    )
}
fn txn(kind: &str, dispatch: &str) -> OwnedTransactionId {
    format!("{kind}_{:x}", Sha256::digest(dispatch.as_bytes())).into()
}

pub(in crate::console) struct DecryptedRequest {
    pub body: String,
    pub mentions: std::collections::BTreeSet<String>,
    pub thread_root: Option<String>,
}
impl AgentDevice {
    pub(in crate::console) fn approval_client(&self) -> &Client {
        &self.client
    }
    pub(in crate::console) fn is_revoked(&self) -> bool {
        self.revoked.load(Ordering::Acquire)
    }
    pub(in crate::console) async fn read_candidate(
        &self,
        room: &str,
        event: &str,
        requester: &str,
    ) -> Result<Option<DecryptedRequest>, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let event_id = OwnedEventId::try_from(event).map_err(|_| RuntimeError::Request)?;
        let room = self.client.get_room(&room_id).ok_or_else(|| {
            tracing::debug!(stage = "encrypted_candidate_room", room_id = %room_id, "Agent device has not synchronized the candidate room");
            RuntimeError::Transport
        })?;
        let event = room
            .event(&event_id, None)
            .await
            .map_err(|error| {
                tracing::debug!(stage = "encrypted_candidate_event", room_id = %room_id, event_id = %event_id, %error, "Agent device could not read the encrypted candidate");
                RuntimeError::Transport
            })?;
        match &event.kind {
            TimelineEventKind::Decrypted(d) => Ok(request_message(d, requester)),
            _ => {
                tracing::debug!(stage = "encrypted_candidate_undecrypted", room_id = %room_id, "Agent device has no key for the encrypted candidate yet");
                Err(RuntimeError::Transport) // Keys can arrive later; retain the candidate.
            }
        }
    }

    pub(in crate::console) async fn open(
        grant: E2eeDeviceGrant,
        directory: &Path,
        agent: &str,
    ) -> Result<Self, RuntimeError> {
        Self::open_notifying(grant, directory, agent, None).await
    }

    pub(in crate::console) async fn open_notifying(
        grant: E2eeDeviceGrant,
        directory: &Path,
        agent: &str,
        changed: Option<watch::Sender<()>>,
    ) -> Result<Self, RuntimeError> {
        let user_id =
            OwnedUserId::try_from(grant.user_id.as_str()).map_err(|_| RuntimeError::Transport)?;
        let devices = directory.join("agent-devices");
        private_directory(&devices)?;
        let root = devices.join(format!("{:x}", Sha256::digest(agent.as_bytes())));
        private_directory(&root)?;
        let passphrase = store_passphrase(&root)?;
        // A new execution device generation is a new Matrix device and account.
        let store = root.join(&grant.device_id);
        private_directory(&store)?;
        let client = Client::builder()
            .homeserver_url(&grant.homeserver)
            .sqlite_store(&store, Some(&passphrase))
            .build()
            .await
            .map_err(|_| RuntimeError::Profile)?;
        let session = MatrixSession {
            meta: SessionMeta {
                user_id,
                device_id: grant.device_id.as_str().into(),
            },
            tokens: SessionTokens {
                access_token: grant.access_token,
                refresh_token: None,
            },
        };
        client
            .matrix_auth()
            .restore_session(session, RoomLoadSettings::default())
            .await
            .map_err(|_| RuntimeError::Profile)?;
        // The first sync uploads this device's identity and one-time keys, so
        // the owner's client starts sharing room keys with it.
        let approvals = crate::encrypted_approvals::Inbox::attach(&client);
        let questions = crate::agent_questions::Inbox::attach(&client);
        let run_controls = crate::agent_questions::Inbox::attach_event(
            &client,
            crate::agent_run_controls::ACTION_EVENT,
        );
        client
            .sync_once(SyncSettings::default().timeout(Duration::ZERO))
            .await
            .map_err(|_| RuntimeError::Transport)?;
        let revoked = Arc::new(AtomicBool::new(false));
        let sync = tokio::spawn(sync(client.clone(), revoked.clone(), changed)).abort_handle();
        Ok(Self {
            client,
            approvals,
            questions,
            question_records: root.join("question-cards.json"),
            run_controls,
            run_records: root.join("run-cards.json"),
            revoked,
            sync,
        })
    }

    /// Returns the owner's request text, or None when this device can never
    /// read it: not a text message from the requester, or keys never arrived.
    pub(super) async fn read_request(
        &self,
        dispatch: &inbox::Dispatch,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<Option<String>, RuntimeError> {
        let room_id =
            OwnedRoomId::try_from(dispatch.room_id.as_str()).map_err(|_| RuntimeError::Request)?;
        let event_id = OwnedEventId::try_from(dispatch.event_id.as_str())
            .map_err(|_| RuntimeError::Request)?;
        let deadline = Instant::now() + KEY_WAIT;
        while Instant::now() < deadline {
            if let Some(room) = self.client.get_room(&room_id)
                && let Ok(event) = room.event(&event_id, None).await
            {
                match &event.kind {
                    TimelineEventKind::Decrypted(decrypted) => {
                        return Ok(request_text(decrypted, &dispatch.requester_mxid));
                    }
                    TimelineEventKind::PlainText { event }
                        if event.get_field::<String>("type").ok().flatten().as_deref()
                            != Some("m.room.encrypted") =>
                    {
                        return Ok(None);
                    }
                    _ => {}
                }
            }
            if self.revoked.load(Ordering::Acquire) {
                return Err(RuntimeError::Transport);
            }
            tokio::select! {biased; _=canceled(cancel)=>return Err(RuntimeError::Stopped), _=tokio::time::sleep(Duration::from_secs(2))=>{}}
        }
        Ok(None)
    }

    /// Sends exactly one encrypted event per transaction ID. Retrying after an
    /// interruption reuses it, so the homeserver returns the original event.
    async fn send(
        &self,
        dispatch: &inbox::Dispatch,
        txn: OwnedTransactionId,
        content: impl matrix_sdk::ruma::events::MessageLikeEventContent,
    ) -> Result<String, RuntimeError> {
        let room_id =
            OwnedRoomId::try_from(dispatch.room_id.as_str()).map_err(|_| RuntimeError::Request)?;
        let room = self
            .client
            .get_room(&room_id)
            .ok_or(RuntimeError::Transport)?;
        // Never fall back to plaintext: the server delivers plaintext replies.
        if !room
            .latest_encryption_state()
            .await
            .map_err(|_| RuntimeError::Transport)?
            .is_encrypted()
        {
            return Err(RuntimeError::Request);
        }
        match room.send(content).with_transaction_id(txn).await {
            Ok(sent) => Ok(sent.response.event_id.to_string()),
            Err(error) => {
                if unknown_token(&error) {
                    self.revoked.store(true, Ordering::Release);
                }
                tracing::warn!(stage = "agent_device", %error, "Encrypted send failed");
                Err(RuntimeError::Transport)
            }
        }
    }

    pub(in crate::console) async fn send_reply(
        &self,
        dispatch: &inbox::Dispatch,
        body: &str,
    ) -> Result<String, RuntimeError> {
        let content = reply_content(dispatch, body)?;
        self.send(dispatch, txn("hagency", &dispatch.id), content)
            .await
    }

    /// Posts one question card, or its edit, under a fixed transaction, so a
    /// retry after an interruption returns the original event. Encrypted
    /// whenever the Room is. Unlike a reply, a card has no server-delivered
    /// plaintext twin, so a plaintext Room gets it in plaintext.
    pub(in crate::console) async fn send_room_message(
        &self,
        room: &str,
        txn: &str,
        content: serde_json::Value,
    ) -> Result<String, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let room = self
            .client
            .get_room(&room_id)
            .ok_or(RuntimeError::Transport)?;
        if room
            .latest_encryption_state()
            .await
            .map_err(|_| RuntimeError::Transport)?
            .is_unknown()
        {
            return Err(RuntimeError::Transport);
        }
        let txn: OwnedTransactionId = txn.into();
        match room
            .send_raw("m.room.message", content)
            .with_transaction_id(&txn)
            .await
        {
            Ok(sent) => Ok(sent.response.event_id.to_string()),
            Err(error) => {
                if unknown_token(&error) {
                    self.revoked.store(true, Ordering::Release);
                }
                tracing::warn!(stage = "agent_device", %error, "Question card send failed");
                Err(RuntimeError::Transport)
            }
        }
    }

    /// Answers to one question card from current members of its Room, oldest
    /// first, each handed out once. In an encrypted Room an answer counts only
    /// when its Megolm sender is authenticated as the event's sender. The
    /// Agent's own events never answer. Content is not checked here.
    pub(in crate::console) async fn question_answers(
        &self,
        room: &str,
        card_event_id: &str,
    ) -> Result<Vec<crate::agent_questions::Received>, RuntimeError> {
        self.room_actions(&self.questions, room, card_event_id)
            .await
    }

    pub(in crate::console) async fn run_actions(
        &self,
        room: &str,
        card_event_id: &str,
    ) -> Result<Vec<crate::agent_questions::Received>, RuntimeError> {
        self.room_actions(&self.run_controls, room, card_event_id)
            .await
    }

    pub(in crate::console) fn run_actions_pending(&self, room: &str, card_event_id: &str) -> bool {
        self.run_controls.pending_for(room, card_event_id)
    }

    async fn room_actions(
        &self,
        inbox: &crate::agent_questions::Inbox,
        room: &str,
        card_event_id: &str,
    ) -> Result<Vec<crate::agent_questions::Received>, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let room = self
            .client
            .get_room(&room_id)
            .ok_or(RuntimeError::Transport)?;
        // Room keys can arrive after the answer: retry undecryptable references.
        // Each pass is bounded; anything left waits for the next pass.
        for id in inbox.due_undecrypted(room_id.as_str(), card_event_id, 4)
        {
            let Ok(event_id) = OwnedEventId::try_from(id.as_str()) else {
                continue;
            };
            if let Ok(Ok(event)) =
                tokio::time::timeout(Duration::from_secs(2), room.event(&event_id, None)).await
                && let TimelineEventKind::Decrypted(decrypted) = &event.kind
                && let Ok(value) = decrypted.event.deserialize_as::<serde_json::Value>()
            {
                inbox.decrypted(
                    room_id.as_str(),
                    &value,
                    Some(&decrypted.encryption_info),
                );
            }
        }
        let encryption = room
            .latest_encryption_state()
            .await
            .map_err(|_| RuntimeError::Transport)?;
        if encryption.is_unknown() {
            return Err(RuntimeError::Transport);
        }
        let encrypted = encryption.is_encrypted();
        let own = self.client.user_id().map(|u| u.to_string());
        let mut answers = Vec::new();
        let mut unsure = Vec::new();
        let mut batch = ActionCheckBatch {
            events: inbox.take(room_id.as_str(), card_event_id, 4),
            requeue: |events| inbox.requeue(events),
        };
        for received in &batch.events {
            if !received.admissible(encrypted, own.as_deref()) {
                continue;
            }
            let Ok(user) = OwnedUserId::try_from(received.sender.as_str()) else {
                continue;
            };
            match tokio::time::timeout(Duration::from_secs(2), room.get_member(&user)).await {
                Ok(Ok(Some(member)))
                    if *member.membership()
                        == matrix_sdk::ruma::events::room::member::MembershipState::Join =>
                {
                    answers.push(received.clone())
                }
                // Not a current member: never an answer.
                Ok(Ok(_)) => {}
                // Membership unknown for now: keep the answer for the next pass.
                _ => unsure.push(received.clone()),
            }
        }
        batch.events = unsure;
        Ok(answers)
    }

    /// One event of a room this device can read, as its JSON (`sender`,
    /// `type`, `content`), decrypted where the room is encrypted. A restarted
    /// executor closes a card it posted earlier from the card's own event.
    pub(in crate::console) async fn room_event(
        &self,
        room: &str,
        event: &str,
    ) -> Result<serde_json::Value, RuntimeError> {
        let room_id = OwnedRoomId::try_from(room).map_err(|_| RuntimeError::Request)?;
        let event_id = OwnedEventId::try_from(event).map_err(|_| RuntimeError::Request)?;
        let room = self
            .client
            .get_room(&room_id)
            .ok_or(RuntimeError::Transport)?;
        let event = tokio::time::timeout(Duration::from_secs(10), room.event(&event_id, None))
            .await
            .map_err(|_| RuntimeError::Transport)?
            .map_err(|_| RuntimeError::Transport)?;
        let encrypted = room
            .latest_encryption_state()
            .await
            .map_err(|_| RuntimeError::Transport)?
            .is_encrypted();
        question_recovery_event(&event.kind, encrypted, room_id.as_str(), event_id.as_str())
            .ok_or(RuntimeError::Transport)
    }

    pub(in crate::console) async fn send_shared_reply(
        &self,
        dispatch: &inbox::Dispatch,
        body: &str,
    ) -> Result<String, RuntimeError> {
        self.send(
            dispatch,
            format!("hagency_shared_{}", dispatch.id).into(),
            reply_content(dispatch, body)?,
        )
        .await
    }
    /// Best effort, like the server's processing reaction for plaintext Rooms.
    pub(super) async fn mark_processing(&self, dispatch: &inbox::Dispatch) {
        let Ok(event) = OwnedEventId::try_from(dispatch.event_id.as_str()) else {
            return;
        };
        let reaction = ReactionEventContent::new(Annotation::new(event, "👀".into()));
        let _ = self
            .send(dispatch, txn("hagency_seen", &dispatch.id), reaction)
            .await;
    }

    /// Tells the owner, inside the encrypted Room, that a request was unreadable.
    pub(super) async fn notice_unreadable(&self, dispatch: &inbox::Dispatch) {
        let mut content = RoomMessageEventContent::notice_plain(
            "🔐 这条加密消息在 Agent 设备上无法解密（可能是在 Agent 设备加入前发送的）。请重新发送一次。",
        );
        if dispatch.thread_root != dispatch.room_id
            && let Ok(root) = OwnedEventId::try_from(dispatch.thread_root.as_str())
        {
            content.relates_to = Some(Relation::Thread(Thread::plain(root.clone(), root)));
        }
        let _ = self
            .send(dispatch, txn("hagency_unreadable", &dispatch.id), content)
            .await;
    }
}

/// Recovery has the same sender requirement as live answers. A homeserver
/// cannot replace a saved answer or card with an unauthenticated envelope.
fn question_recovery_event(
    kind: &TimelineEventKind,
    encrypted: bool,
    room_id: &str,
    event_id: &str,
) -> Option<serde_json::Value> {
    let (value, info): (serde_json::Value, _) = match kind {
        TimelineEventKind::Decrypted(decrypted) => (
            decrypted.event.deserialize_as().ok()?,
            Some(&*decrypted.encryption_info),
        ),
        TimelineEventKind::PlainText { event } if !encrypted => {
            (event.deserialize_as().ok()?, None)
        }
        _ => return None,
    };
    let sender = value["sender"].as_str()?;
    if value["event_id"].as_str() != Some(event_id)
        || value
            .get("room_id")
            .is_some_and(|room| room.as_str() != Some(room_id))
        || value["type"] == "m.room.encrypted"
        || info.is_some_and(|info| !crate::agent_questions::authentic(info, sender))
    {
        return None;
    }
    Some(value)
}

/// Same shape as the server's replies: top-level owner DMs stay flat.
pub(in crate::console) fn reply_content(
    dispatch: &inbox::Dispatch,
    body: &str,
) -> Result<RoomMessageEventContent, RuntimeError> {
    let mut content = if body.len() <= MARKDOWN_BYTES {
        RoomMessageEventContent::text_markdown(body)
    } else if body.len() <= PLAIN_BYTES {
        RoomMessageEventContent::text_plain(body)
    } else {
        let mut end = PLAIN_BYTES;
        while !body.is_char_boundary(end) {
            end -= 1;
        }
        RoomMessageEventContent::text_plain(format!("{}\n\n…（回复过长，已截断）", &body[..end]))
    };
    if dispatch.thread_root != dispatch.room_id {
        let root = OwnedEventId::try_from(dispatch.thread_root.as_str())
            .map_err(|_| RuntimeError::Request)?;
        content.relates_to = Some(Relation::Thread(Thread::plain(root.clone(), root)));
    }
    Ok(content)
}

fn request_text(decrypted: &DecryptedRoomEvent, requester: &str) -> Option<String> {
    request_message(decrypted, requester).map(|m| m.body)
}
fn request_message(decrypted: &DecryptedRoomEvent, requester: &str) -> Option<DecryptedRequest> {
    // The Megolm session's Olm-authenticated sender, not just the envelope.
    if decrypted.encryption_info.sender.as_str() != requester
        || matches!(
            decrypted.encryption_info.verification_state,
            VerificationState::Unverified(
                VerificationLevel::None(_)
                    | VerificationLevel::MismatchedSender
                    | VerificationLevel::VerificationViolation
            )
        )
    {
        return None;
    }
    let Ok(AnyTimelineEvent::MessageLike(AnyMessageLikeEvent::RoomMessage(
        MessageLikeEvent::Original(message),
    ))) = decrypted.event.deserialize()
    else {
        return None;
    };
    if message.sender.as_str() != requester
        || matches!(message.content.relates_to, Some(Relation::Replacement(_)))
    {
        return None;
    }
    let mentions = message
        .content
        .mentions
        .as_ref()
        .map(|m| m.user_ids.iter().map(ToString::to_string).collect())
        .unwrap_or_default();
    let thread_root = match &message.content.relates_to {
        Some(Relation::Thread(t)) => Some(t.event_id.to_string()),
        _ => None,
    };
    let MessageType::Text(text) = message.content.msgtype else {
        return None;
    };
    (!text.body.trim().is_empty() && text.body.len() <= 65_536).then_some(DecryptedRequest {
        body: text.body,
        mentions,
        thread_root,
    })
}

async fn sync(client: Client, revoked: Arc<AtomicBool>, changed: Option<watch::Sender<()>>) {
    loop {
        match client
            .sync_once(SyncSettings::default().timeout(Duration::from_secs(30)))
            .await
        {
            Ok(response) => {
                // SDK crypto processing has finished here. Wake the shared
                // consumer for both new messages and late room keys, rather
                // than waiting for its next housekeeping pass. Empty syncs,
                // receipts and typing updates must not cause a polling storm.
                if sync_has_work(&response) {
                    if let Some(changed) = &changed {
                        changed.send_replace(());
                    }
                }
            }
            Err(error) if unknown_token(&error) => {
                // Another grant replaced this device's token; the next request
                // obtains a fresh one under its current lease.
                revoked.store(true, Ordering::Release);
                return;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(5)).await,
        }
    }
}

fn sync_has_work(response: &matrix_sdk::sync::SyncResponse) -> bool {
    !response.to_device.is_empty()
        || response
            .rooms
            .joined
            .values()
            .any(|room| !room.timeline.events.is_empty())
        || !response.rooms.left.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn canceled_action_membership_lookup_requeues_the_entire_taken_batch() {
        let returned = std::sync::Mutex::new(Vec::new());
        let lookup = async {
            let _batch = ActionCheckBatch {
                events: vec!["first-verified", "second-awaiting-membership", "third-unchecked"],
                requeue: |events| returned.lock().unwrap().extend(events),
            };
            std::future::pending::<()>().await;
        };
        assert!(tokio::time::timeout(Duration::from_millis(1), lookup).await.is_err());
        assert_eq!(*returned.lock().unwrap(),
            ["first-verified", "second-awaiting-membership", "third-unchecked"]);
        let batch = ActionCheckBatch {
            events: vec!["membership-still-unknown"],
            requeue: |events| returned.lock().unwrap().extend(events),
        };
        drop(batch);
        assert_eq!(returned.lock().unwrap().last(), Some(&"membership-still-unknown"));
    }

    #[test]
    fn shared_work_sync_wakes_for_late_keys_but_not_empty_polls() {
        let mut response = matrix_sdk::sync::SyncResponse::default();
        assert!(!sync_has_work(&response));
        let raw = matrix_sdk::ruma::serde::Raw::from_json_string(
            serde_json::json!({"type":"m.room_key","sender":"@member:example.test","content":{}})
                .to_string(),
        )
        .unwrap();
        response
            .to_device
            .push(matrix_sdk::deserialized_responses::ProcessedToDeviceEvent::PlainText(raw));
        assert!(sync_has_work(&response));
        response.to_device.clear();
        assert!(!sync_has_work(&response));
    }

    fn dispatch(thread_root: &str) -> inbox::Dispatch {
        inbox::Dispatch {
            id: "evt_1".into(),
            binding_id: "bind_1".into(),
            agent_id: "agt_1".into(),
            event_id: "$request".into(),
            room_id: "!dm:example.test".into(),
            requester_mxid: "@owner:example.test".into(),
            thread_root: thread_root.into(),
            body: String::new(),
            state: "running".into(),
            binding_generation: 1,
            dispatch_epoch: Some(1),
            dispatch_device_id: Some("dev_1".into()),
            execution_id: Some("exe_1".into()),
            outcome: None,
        }
    }
    fn json(content: &RoomMessageEventContent) -> serde_json::Value {
        serde_json::to_value(content).unwrap()
    }

    #[test]
    fn encrypted_addressing_requires_authenticated_sender_and_original_text() {
        use matrix_sdk::deserialized_responses::{
            AlgorithmInfo, DeviceLinkProblem, EncryptionInfo,
        };
        let info = EncryptionInfo {
            sender: "@owner:example.test".try_into().unwrap(),
            sender_device: Some("OWNER".into()),
            forwarder: None,
            algorithm_info: AlgorithmInfo::MegolmV1AesSha2 {
                curve25519_key: "key".into(),
                sender_claimed_keys: Default::default(),
                session_id: Some("session".into()),
            },
            verification_state: VerificationState::Unverified(VerificationLevel::UnsignedDevice),
        };
        let raw = serde_json::json!({"type":"m.room.message","event_id":"$event","room_id":"!room:example.test","sender":"@owner:example.test","origin_server_ts":1,"content":{"msgtype":"m.text","body":"Secret request","m.mentions":{"user_ids":["@agent:example.test"]},"m.relates_to":{"rel_type":"m.thread","event_id":"$root","m.in_reply_to":{"event_id":"$root"}}}});
        let mut event = DecryptedRoomEvent {
            event: serde_json::from_value(raw.clone()).unwrap(),
            encryption_info: Arc::new(info),
            unsigned_encryption_info: None,
        };
        let request = request_message(&event, "@owner:example.test").unwrap();
        assert_eq!(request.thread_root.as_deref(), Some("$root"));
        assert!(request.mentions.contains("@agent:example.test"));
        assert!(request_message(&event, "@intruder:example.test").is_none());
        Arc::make_mut(&mut event.encryption_info).verification_state =
            VerificationState::Unverified(VerificationLevel::None(
                DeviceLinkProblem::InsecureSource,
            ));
        assert!(request_message(&event, "@owner:example.test").is_none());
        Arc::make_mut(&mut event.encryption_info).verification_state = VerificationState::Verified;
        let mut edit = raw;
        edit["content"]["m.relates_to"] =
            serde_json::json!({"rel_type":"m.replace","event_id":"$original"});
        edit["content"]["m.new_content"] =
            serde_json::json!({"msgtype":"m.text","body":"replacement"});
        event.event = serde_json::from_value(edit).unwrap();
        assert!(request_message(&event, "@owner:example.test").is_none());
    }

    #[test]
    fn question_recovery_requires_authenticated_events_in_encrypted_rooms() {
        use matrix_sdk::deserialized_responses::{
            AlgorithmInfo, DeviceLinkProblem, EncryptionInfo,
        };
        let raw = serde_json::json!({"type":"m.room.message","event_id":"$card","room_id":"!room:example.test",
            "sender":"@_hagency_agt_one:example.test","origin_server_ts":1,"content":{"msgtype":"m.text","body":"Question"}});
        let plain = TimelineEventKind::PlainText {
            event: serde_json::from_value(raw.clone()).unwrap(),
        };
        assert!(question_recovery_event(&plain, false, "!room:example.test", "$card").is_some());
        assert!(
            question_recovery_event(&plain, true, "!room:example.test", "$card").is_none(),
            "no plaintext injection during recovery"
        );
        let mut decrypted = DecryptedRoomEvent {
            event: serde_json::from_value(raw.clone()).unwrap(),
            encryption_info: Arc::new(EncryptionInfo {
                sender: "@_hagency_agt_one:example.test".try_into().unwrap(),
                sender_device: Some("AGENT".into()),
                forwarder: None,
                algorithm_info: AlgorithmInfo::MegolmV1AesSha2 {
                    curve25519_key: "key".into(),
                    sender_claimed_keys: Default::default(),
                    session_id: Some("session".into()),
                },
                verification_state: VerificationState::Verified,
            }),
            unsigned_encryption_info: None,
        };
        assert!(
            question_recovery_event(
                &TimelineEventKind::Decrypted(decrypted.clone()),
                true,
                "!room:example.test",
                "$card"
            )
            .is_some()
        );
        // Even an authentic event from this sender is not a replacement for
        // the exact answer/card recorded before restart.
        let authenticated = TimelineEventKind::Decrypted(decrypted.clone());
        assert!(
            question_recovery_event(&authenticated, true, "!room:example.test", "$different")
                .is_none(),
            "another authentic event ID is rejected"
        );
        assert!(
            question_recovery_event(&authenticated, true, "!different:example.test", "$card")
                .is_none(),
            "another room is rejected"
        );
        let mut without_room = raw.clone();
        without_room.as_object_mut().unwrap().remove("room_id");
        let mut roomless = decrypted.clone();
        roomless.event = serde_json::from_value(without_room).unwrap();
        assert!(
            question_recovery_event(
                &TimelineEventKind::Decrypted(roomless),
                true,
                "!room:example.test",
                "$card"
            )
            .is_some(),
            "sync events may omit their room ID"
        );
        Arc::make_mut(&mut decrypted.encryption_info).verification_state =
            VerificationState::Unverified(VerificationLevel::None(
                DeviceLinkProblem::InsecureSource,
            ));
        assert!(
            question_recovery_event(
                &TimelineEventKind::Decrypted(decrypted.clone()),
                true,
                "!room:example.test",
                "$card"
            )
            .is_none()
        );
        Arc::make_mut(&mut decrypted.encryption_info).verification_state =
            VerificationState::Verified;
        Arc::make_mut(&mut decrypted.encryption_info).sender =
            "@intruder:example.test".try_into().unwrap();
        assert!(
            question_recovery_event(
                &TimelineEventKind::Decrypted(decrypted),
                true,
                "!room:example.test",
                "$card"
            )
            .is_none(),
            "the envelope sender is not proof"
        );
    }

    #[test]
    fn agent_question_cards_and_answers_never_become_encrypted_requests() {
        use matrix_sdk::deserialized_responses::{AlgorithmInfo, EncryptionInfo};
        let event = |sender: &str, raw: serde_json::Value| DecryptedRoomEvent {
            event: serde_json::from_value(raw).unwrap(),
            encryption_info: Arc::new(EncryptionInfo {
                sender: sender.try_into().unwrap(),
                sender_device: Some("DEVICE".into()),
                forwarder: None,
                algorithm_info: AlgorithmInfo::MegolmV1AesSha2 {
                    curve25519_key: "key".into(),
                    sender_claimed_keys: Default::default(),
                    session_id: Some("session".into()),
                },
                verification_state: VerificationState::Verified,
            }),
            unsigned_encryption_info: None,
        };
        // A member's answer, even one that mentions the Agent, is not a text
        // message: the addressing check never confirms it as a new request.
        let answer = serde_json::json!({"type":crate::agent_questions::ANSWER_EVENT,"event_id":"$answer","room_id":"!room:example.test",
            "sender":"@member:example.test","origin_server_ts":1,"content":{"version":1,"cardId":"aq_x",
            "answers":{"db":{"option":"SQLite"}},"body":"@agent SQLite","m.mentions":{"user_ids":["@agent:example.test"]},
            "m.relates_to":{"rel_type":"m.reference","event_id":"$card"}}});
        assert!(
            request_message(
                &event("@member:example.test", answer),
                "@member:example.test"
            )
            .is_none()
        );
        // Nor is another Agent's card, whatever its fallback text says.
        let card = serde_json::json!({"type":"m.room.message","event_id":"$card","room_id":"!room:example.test",
            "sender":"@_hagency_agt_other:example.test","origin_server_ts":1,"content":{"msgtype":crate::agent_questions::MSGTYPE,
            "body":"@agent please answer","m.mentions":{"user_ids":["@agent:example.test"]}}});
        assert!(
            request_message(
                &event("@_hagency_agt_other:example.test", card),
                "@_hagency_agt_other:example.test"
            )
            .is_none()
        );
    }

    #[test]
    fn encrypted_replies_keep_the_server_reply_shape() {
        let flat = json(&reply_content(&dispatch("!dm:example.test"), "**done**").unwrap());
        assert_eq!(flat["msgtype"], "m.text");
        assert_eq!(flat["body"], "**done**");
        assert_eq!(flat["format"], "org.matrix.custom.html");
        assert!(flat.get("m.relates_to").is_none());

        let threaded = json(&reply_content(&dispatch("$root"), "done").unwrap());
        assert_eq!(
            threaded["m.relates_to"],
            serde_json::json!({"rel_type":"m.thread","event_id":"$root",
                "is_falling_back":true,"m.in_reply_to":{"event_id":"$root"}})
        );
    }

    #[test]
    fn encrypted_reply_content_validates_as_local_thread_evidence() {
        use hagency_store::owner_resources::shared_execution::SharedReplyDelivery;
        for body in [
            "**done**".to_owned(),
            "答复".to_owned(),
            "a".repeat(32 * 1024),
        ] {
            let content = json(&reply_content(&dispatch("$root"), &body).unwrap());
            let receipt = SharedReplyDelivery {
                version: 1,
                work_id: "work_1".into(),
                work_digest: "a".repeat(64),
                assignment_id: "assignment_1".into(),
                runtime_command_digest: "b".repeat(64),
                room_id: "!room:example.test".into(),
                thread_root: "$root".into(),
                source_event_id: "$request".into(),
                matrix_txn_id: "hagency_shared_work_1".into(),
                matrix_event_id: "$reply".into(),
                matrix_content_digest: hagency_core::canonical::digest(&content).unwrap(),
                body_digest: format!("{:x}", Sha256::digest(body.as_bytes())),
                body,
                formatted_body: content["formatted_body"].as_str().map(str::to_owned),
                sent_at_ms: 1,
            };
            receipt.validate().unwrap();
        }
    }

    #[test]
    fn long_encrypted_replies_fit_one_matrix_event() {
        let plain =
            json(&reply_content(&dispatch("$root"), &"a".repeat(MARKDOWN_BYTES + 1)).unwrap());
        assert!(plain.get("formatted_body").is_none());
        let body = "答".repeat(PLAIN_BYTES);
        let cut = json(&reply_content(&dispatch("$root"), &body).unwrap());
        let text = cut["body"].as_str().unwrap();
        assert!(text.len() <= PLAIN_BYTES + 64 && text.ends_with("已截断）"));
        // Base64 Megolm ciphertext stays inside the 64 KiB event envelope.
        assert!(serde_json::to_vec(&cut).unwrap().len() * 4 / 3 < 62 * 1024);
    }

    #[test]
    fn reply_transaction_matches_the_server_receipt() {
        assert_eq!(
            txn("hagency", "evt_1").as_str(),
            format!("hagency_{:x}", Sha256::digest(b"evt_1"))
        );
        assert_ne!(txn("hagency_seen", "evt_1"), txn("hagency", "evt_1"));
    }

    #[test]
    fn saved_grant_is_private_and_reused_only_for_its_generation_and_homeserver() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path();
        private_directory(&directory.join("agent-devices")).unwrap();
        private_directory(
            &directory
                .join("agent-devices")
                .join(format!("{:x}", Sha256::digest(b"agent"))),
        )
        .unwrap();
        let grant = E2eeDeviceGrant {
            user_id: "@agent:example.test".into(),
            device_id: "HAGE2EEABC123".into(),
            access_token: "fixture-token".into(),
            homeserver: "https://example.test/".into(),
        };
        save_grant(directory, "agent", &grant, 3);
        let loaded = load_grant(directory, "agent", "https://example.test/", 3).unwrap();
        assert_eq!(
            (loaded.user_id, loaded.device_id, loaded.access_token),
            (
                grant.user_id.clone(),
                grant.device_id.clone(),
                grant.access_token.clone()
            )
        );
        // A new device generation or another homeserver never reuses it.
        assert!(load_grant(directory, "agent", "https://example.test/", 4).is_none());
        assert!(load_grant(directory, "agent", "https://other.test/", 3).is_none());
        assert!(load_grant(directory, "other", "https://example.test/", 3).is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(saved_grant_path(directory, "agent"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
        forget_grant(directory, "agent");
        assert!(load_grant(directory, "agent", "https://example.test/", 3).is_none());
    }

    #[test]
    fn store_passphrase_is_private_and_stable() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("agent");
        private_directory(&root).unwrap();
        let first = store_passphrase(&root).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(store_passphrase(&root).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(root.join("store-key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }
}
