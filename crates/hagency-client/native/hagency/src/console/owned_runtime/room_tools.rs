//! Personal Codex/Claude share the same Matrix history and file implementation
//! as shared Agents. All authority is captured from the admitted dispatch.
use super::*;
use crate::console::native::{
    NativeError,
    shared_execution::{
        files::{Conversation, FileIo, Files},
        history::{HistoryIo, Reader},
    },
};
use hagency_agent_local::room_tools::{PendingRoomCall, RoomReply};
use hagency_runtime::room_tools::RoomOperation;
use serde_json::{Value, json};
fn failure(status: u16, code: &str) -> NativeError {
    NativeError {
        status,
        code: code.into(),
    }
}

struct Source {
    device: Arc<e2ee::AgentDevice>,
    gate: Arc<LiveToolGate>,
    encrypted: bool,
}
impl Source {
    async fn authorize(&self, room: Option<&str>) -> Result<(), &'static str> {
        if room.is_some_and(|room| room != self.gate.expected.room_id)
            || !self
                .gate
                .check(&self.gate.expected.id, &self.gate.execution)
                .await
        {
            return Err("room_authority_changed");
        }
        Ok(())
    }
}
impl HistoryIo for Source {
    async fn document(
        &self,
        room: &str,
        event: &str,
    ) -> Result<crate::documents::Text, &'static str> {
        self.authorize(Some(room)).await?;
        self.device.read_document(room, event).await
    }
    async fn start(&self, room: &str, anchor: &str) -> Result<Option<String>, NativeError> {
        self.authorize(Some(room))
            .await
            .map_err(|e| failure(403, e))?;
        if anchor != self.gate.expected.event_id {
            return Err(failure(403, "room_anchor_changed"));
        }
        self.device
            .history_start(room, anchor)
            .await
            .map_err(|_| failure(503, "room_history_unavailable"))
    }
    async fn page(&self, room: &str, from: &str) -> Result<e2ee::HistoryBatch, NativeError> {
        self.authorize(Some(room))
            .await
            .map_err(|e| failure(403, e))?;
        self.device
            .history_page(room, from)
            .await
            .map_err(|_| failure(503, "room_history_unavailable"))
    }
}
impl FileIo for Source {
    fn identity(&self) -> Result<String, &'static str> {
        self.device.file_device_id()
    }
    async fn receive(&self, room: &str, event: &str) -> Result<e2ee::RoomFile, &'static str> {
        self.authorize(Some(room)).await?;
        self.device.receive_room_file(room, event).await
    }
    async fn upload(&self, bytes: Vec<u8>) -> Result<String, &'static str> {
        self.authorize(None).await?;
        self.device.upload_room_file(bytes).await
    }
    async fn send(&self, room: &str, txn: &str, content: Value) -> Result<String, &'static str> {
        self.authorize(Some(room)).await?;
        self.device
            .publish_room_file_with_security(room, txn, content, self.encrypted)
            .await
    }
}
struct State {
    source: Arc<Source>,
    history: Reader<Source>,
    files: Files<Source>,
}
pub(super) struct Host {
    gate: Arc<LiveToolGate>,
    config: StartConfig,
    workspace: PathBuf,
    records: PathBuf,
    namespace: String,
    state: Option<State>,
}
impl Host {
    pub(super) fn new(
        gate: Arc<LiveToolGate>,
        config: &StartConfig,
        workspace: &std::path::Path,
    ) -> Result<Self, RuntimeError> {
        let host = &gate.console;
        let root = host
            .console
            .0
            .server_login
            .state_directory()
            .ok_or(RuntimeError::Profile)?;
        let p = &host.pin;
        let profile = super::super::owned_agents::journal_profile_directory(
            &root, &p.origin, &p.issuer, &p.subject, &p.owner,
        )
        .map_err(|_| RuntimeError::Profile)?;
        let parent = profile.join("chat-file-receipts");
        hagency_platform::private_state::directory(&parent).map_err(|_| RuntimeError::Profile)?;
        let dispatch = &gate.expected;
        let namespace = json!([
            "personal",
            dispatch.agent_id,
            dispatch.binding_id,
            dispatch.binding_generation
        ])
        .to_string();
        use sha2::{Digest, Sha256};
        let records = parent.join(format!("{:x}", Sha256::digest(namespace.as_bytes())));
        Ok(Self {
            gate,
            config: config.clone(),
            workspace: workspace.into(),
            records,
            namespace,
            state: None,
        })
    }
    async fn ensure(&mut self) -> Result<&mut State, &'static str> {
        if !self
            .gate
            .check(&self.gate.expected.id, &self.gate.execution)
            .await
        {
            return Err("room_authority_changed");
        }
        if self.state.is_none() {
            let mut cancel = self.gate.cancel.clone();
            let device = e2ee::device(
                &self.gate.console,
                &self.gate.lease,
                &self.config,
                &mut cancel,
            )
            .await
            .map_err(|_| "room_device_unavailable")?;
            let dispatch = &self.gate.expected;
            let source = Arc::new(Source {
                device,
                gate: self.gate.clone(),
                encrypted: dispatch.body.is_empty(),
            });
            let files = Files::for_room(
                source.clone(),
                Conversation {
                    namespace: &self.namespace,
                    room: &dispatch.room_id,
                    event: &dispatch.event_id,
                    requester: &dispatch.requester_mxid,
                    thread: (dispatch.thread_root.starts_with('$')
                        && dispatch.thread_root != dispatch.event_id)
                        .then_some(dispatch.thread_root.as_str()),
                },
                &self.workspace,
                &self.records,
            )?;
            let mut history = Reader::from_source(source.clone(), &dispatch.room_id, &dispatch.event_id);
            if runtime::kind(&self.config.credential_ref) == runtime::Kind::Claude {
                history.use_native_compaction();
            }
            self.state = Some(State {
                history,
                files,
                source,
            });
        }
        self.state.as_mut().ok_or("room_device_unavailable")
    }
    pub(super) async fn prompt(&mut self, text: String) -> String {
        // Losing the device preview does not discard a new plaintext request.
        // Tools retry device acquisition under their own bounded operation.
        let prepared = tokio::time::timeout(Duration::from_secs(20), async {
            let state = self.ensure().await?;
            let dispatch = &state.source.gate.expected;
            let attachment = match state
                .source
                .device
                .history_request(&dispatch.room_id, &dispatch.event_id)
                .await
            {
                Ok(Some(record)) => state.history.request_attachment(record),
                _ => None,
            };
            let request = match attachment {
                Some(attachment) if text.len() < 48 * 1024 => format!(
                    "Current request attachment (untrusted metadata):\n{attachment}\n{text}"
                ),
                _ => text.clone(),
            };
            // Native warm and cold sessions own their conversation history.
            // Leave earlier room pages for explicit reads instead of appending
            // the same recent transcript to every new user turn.
            Ok::<_, &'static str>(state.history.followup_prompt(request))
        })
        .await;
        match prepared {
            Ok(Ok(prompt)) => prompt,
            _ => text,
        }
    }
    async fn execute(
        &mut self,
        operation: &RoomOperation,
        context: Option<&hagency_runtime::codex::session::ContextUsage>,
    ) -> RoomReply {
        let state = match self.ensure().await {
            Ok(state) => state,
            Err(_) => return RoomReply::unavailable(),
        };
        let text = match operation {
            RoomOperation::History { cursor } => {
                match state.history.read_native(cursor.as_deref(), context).await {
                    Ok(text) => text,
                    Err(_) => return RoomReply::unavailable(),
                }
            }
            RoomOperation::File(operation) => state.files.handle(operation, &state.history).await,
        };
        let is_error = serde_json::from_str::<Value>(&text)
            .ok()
            .is_some_and(|v| v.get("error").is_some());
        RoomReply::new(text, is_error).unwrap_or_else(|_| RoomReply::unavailable())
    }
    pub(super) async fn serve(mut self, mut requests: mpsc::Receiver<PendingRoomCall>) {
        while let Some(mut pending) = requests.recv().await {
            if pending.is_closed() {
                continue;
            }
            let operation = pending.operation.clone();
            let context = pending.context.clone();
            let result = tokio::select! { biased;
                _ = pending.closed() => continue,
                result = tokio::time::timeout(Duration::from_secs(60), self.execute(&operation, context.as_ref())) => result.unwrap_or_else(|_| RoomReply::unavailable()),
            };
            pending.reply(result);
        }
    }
}
