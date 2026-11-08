//! File exchange for an admitted encrypted conversation, separate from task MCP.
//! The host fixes the room/workspace; the model supplies only event IDs and paths.
use super::*;
use crate::console::owned_runtime::e2ee::{AgentDevice, RoomFile};
use hagency_runtime::{room_tools::RoomFileOperation, owned::OwnedSession};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

mod storage;
#[cfg(test)]
pub(super) mod tests;
use storage::{ReceiveRecord, SendRecord, Store, Workspace};
const MAX_BYTES: usize = crate::documents::MAX_FILE_BYTES;
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(60);
type Result<T> = std::result::Result<T, &'static str>;
fn sha(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

pub(super) trait FileIo: Send + Sync {
    fn identity(&self) -> Result<String>;
    fn receive(
        &self,
        room: &str,
        event: &str,
    ) -> impl std::future::Future<Output = Result<RoomFile>> + Send;
    fn upload(&self, bytes: Vec<u8>) -> impl std::future::Future<Output = Result<String>> + Send;
    fn send(
        &self,
        room: &str,
        txn: &str,
        content: Value,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
}
pub(super) enum Source {
    Device {
        device: Arc<AgentDevice>,
        io: Io,
        work: String,
    },
    #[cfg(test)]
    Fixture(Arc<tests::Endpoint>),
}
impl FileIo for Source {
    fn identity(&self) -> Result<String> {
        match self {
            Self::Device { device, .. } => device.file_device_id(),
            #[cfg(test)]
            Self::Fixture(f) => f.identity(),
        }
    }
    async fn receive(&self, room: &str, event: &str) -> Result<RoomFile> {
        match self {
            Self::Device { device, .. } => device.receive_room_file(room, event).await,
            #[cfg(test)]
            Self::Fixture(f) => f.receive(room, event).await,
        }
    }
    async fn upload(&self, bytes: Vec<u8>) -> Result<String> {
        match self {
            Self::Device { device, io, work } => {
                authorize(io, work).await?;
                device.upload_room_file(bytes).await
            }
            #[cfg(test)]
            Self::Fixture(f) => f.upload(bytes).await,
        }
    }
    async fn send(&self, room: &str, txn: &str, content: Value) -> Result<String> {
        match self {
            Self::Device { device, io, work } => {
                authorize(io, work).await?;
                device.publish_room_file(room, txn, content).await
            }
            #[cfg(test)]
            Self::Fixture(f) => f.send(room, txn, content).await,
        }
    }
}

async fn authorize(io: &Io, work: &str) -> Result<()> {
    let b = io
        .bound()
        .await
        .map_err(|_| "file_send_authority_changed")?;
    let _: Timed<Value> = b
        .request(
            super::e2ee::CryptoAction::Send { work: work.into() }.into(),
            false,
        )
        .await
        .map_err(|_| "file_send_authority_changed")?;
    Ok(())
}

pub(super) struct Files<S = Source> {
    source: Arc<S>,
    room: String,
    reply_to: String,
    thread: Option<String>,
    scope: String,
    workspace: Workspace,
    store: Store,
    transfer_timeout: Duration,
}
impl Files {
    pub(super) async fn new(
        io: &Io,
        b: &Bound,
        work: &SharedWorkItem,
        process: &OwnedSession,
    ) -> std::result::Result<Option<Self>, NativeError> {
        if !work.encrypted || !process.shared_tools_enabled() {
            return Ok(None);
        }
        #[cfg(test)]
        if let Some(fixture) = &io.crypto_fixture {
            return match &fixture.files {
                Some(f) => Self::open(
                    Arc::new(Source::Fixture(f.clone())),
                    work,
                    process.shared_workspace(),
                    &f.records,
                )
                .map(Some)
                .map_err(|e| failure(503, e)),
                None => Ok(None),
            };
        }
        let device = super::e2ee::device(io, b, &work.assignment_id).await?;
        let root = super::e2ee::file_records(io, b, &work.assignment_id)?;
        Self::open(
            Arc::new(Source::Device {
                device,
                io: io.clone(),
                work: work.id.clone(),
            }),
            work,
            process.shared_workspace(),
            &root,
        )
        .map(Some)
        .map_err(|e| failure(503, e))
    }
}
impl<S: FileIo> Files<S> {
    fn open(
        source: Arc<S>,
        work: &SharedWorkItem,
        workspace: &Path,
        records: &Path,
    ) -> Result<Self> {
        let scope = sha(serde_json::to_vec(&json!([
            work.assignment_id,
            work.room_id,
            work.event_id,
            work.requester_mxid
        ]))
        .map_err(|_| "file_scope_invalid")?);
        Ok(Self {
            source,
            room: work.room_id.clone(),
            reply_to: work.event_id.clone(),
            thread: (work.thread_root != work.event_id).then(|| work.thread_root.clone()),
            scope,
            workspace: Workspace::open(workspace)?,
            store: Store::open(records)?,
            transfer_timeout: TRANSFER_TIMEOUT,
        })
    }
    pub(super) async fn handle<H: super::history::HistoryIo>(
        &mut self,
        operation: &RoomFileOperation,
        history: &super::history::Reader<H>,
    ) -> String {
        let result = async {
            operation.validate().map_err(|_| "invalid_file_arguments")?;
            match operation {
                RoomFileOperation::ListReceivedFiles { after, limit } => {
                    Ok(history.list_attachments(*after, *limit))
                }
                RoomFileOperation::ReceiveFile { event_id } => {
                    if !history.contains_attachment(event_id) {
                        return Err("attachment_not_observed_in_this_room");
                    }
                    self.receive(event_id).await
                }
                RoomFileOperation::SendFile { .. } => self.send(operation).await,
                RoomFileOperation::GetFileDelivery { delivery_id } => self.delivery(delivery_id),
            }
        }
        .await;
        match result {
            Ok(value) => value.to_string(),
            Err(error) => json!({"error":error}).to_string(),
        }
    }
    async fn receive(&mut self, event: &str) -> Result<Value> {
        tokio::time::timeout(self.transfer_timeout, self.receive_inner(event))
            .await
            .map_err(|_| "file_receive_timed_out")?
    }
    async fn receive_inner(&mut self, event: &str) -> Result<Value> {
        let id = format!("receive_{}", sha(format!("{}|{event}", self.scope)));
        if let Some(saved) = self.store.read::<ReceiveRecord>(&id)? {
            return self.workspace.receive_receipt(&saved);
        }
        let file = self.source.receive(&self.room, event).await?;
        if file.bytes.len() > MAX_BYTES {
            return Err("file_size_limit");
        }
        let extension = file.filename.rsplit_once('.').map(|(_, v)| v).filter(|v| {
            !v.is_empty() && v.len() <= 12 && v.bytes().all(|c| c.is_ascii_alphanumeric())
        });
        let path = format!(
            "{id}{}",
            extension.map(|v| format!(".{v}")).unwrap_or_default()
        );
        let mut record = ReceiveRecord {
            event: event.into(),
            path,
            filename: file.filename,
            mime: file.mime,
            sha256: sha(&file.bytes),
            size: file.bytes.len(),
            ready: false,
        };
        // Create-only admission before the first workspace write. A crash or
        // duplicate callback never opens an existing path for truncation.
        if !self.store.insert(&id, &record)? {
            return Err("file_receive_outcome_unknown");
        }
        self.workspace.materialize(&record.path, &file.bytes)?;
        record.ready = true;
        self.store.update(&id, &record)?;
        self.workspace.receive_receipt(&record)
    }
    fn delivery(&self, id: &str) -> Result<Value> {
        let saved = self
            .store
            .read::<SendRecord>(id)?
            .ok_or("file_delivery_not_found")?;
        if saved.scope != self.scope {
            return Err("file_delivery_not_found");
        }
        Ok(saved.receipt())
    }
    async fn send(&mut self, operation: &RoomFileOperation) -> Result<Value> {
        // SDK encryption/key discovery and event publication may retry forever.
        // Bound the entire operation, including fresh server authorization.
        match tokio::time::timeout(self.transfer_timeout, self.send_inner(operation)).await {
            Ok(result) => result,
            Err(_) => {
                let RoomFileOperation::SendFile { call_id, .. } = operation else {
                    return Err("invalid_file_arguments");
                };
                // Every network write has a durable receipt first. Timing out
                // preserves the same unknown outcome and original transaction.
                self.delivery(&self.send_id(call_id))
            }
        }
    }
    fn send_id(&self, call_id: &str) -> String {
        format!("chatfile_{}", sha(format!("{}|{call_id}", self.scope)))
    }
    async fn send_inner(&mut self, operation: &RoomFileOperation) -> Result<Value> {
        let RoomFileOperation::SendFile {
            call_id,
            path,
            filename,
            caption,
        } = operation
        else {
            return Err("invalid_file_arguments");
        };
        let selection = hagency_files::RelativeFile::new(path).map_err(|_| "file_path_refused")?;
        let filename = filename
            .as_deref()
            .unwrap_or_else(|| path.rsplit('/').next().unwrap_or("file"));
        // Display filename is one portable component, never a destination.
        hagency_files::RelativeFile::new(filename).map_err(|_| "file_filename_refused")?;
        if filename.contains('/') {
            return Err("file_filename_refused");
        }
        let id = self.send_id(call_id);
        let selection_digest =
            sha(serde_json::to_vec(operation).map_err(|_| "invalid_file_arguments")?);
        let mut saved = match self.store.read::<SendRecord>(&id)? {
            Some(saved) => {
                if saved.scope != self.scope || saved.selection != selection_digest {
                    return Err("file_call_id_conflict");
                }
                if saved.event.is_some() || saved.content.is_none() {
                    return Ok(saved.receipt());
                }
                if saved.device != self.source.identity()? {
                    return Err("file_delivery_device_changed");
                }
                saved
            }
            None => {
                let snapshot = self.workspace.snapshot(&selection)?;
                let size = snapshot.len();
                let hash = sha(snapshot.bytes());
                let codec = hagency_media::Codec::new(
                    hagency_media::Limits::new(MAX_BYTES, 1).map_err(|_| "file_size_limit")?,
                );
                let encrypted = codec
                    .encrypt(snapshot)
                    .map_err(|_| "file_encryption_failed")?;
                let mut saved = SendRecord {
                    id: id.clone(),
                    scope: self.scope.clone(),
                    selection: selection_digest,
                    device: self.source.identity()?,
                    filename: filename.into(),
                    size,
                    sha256: hash,
                    content: None,
                    event: None,
                };
                // Persist before any media POST: uncertain uploads are never
                // repeated or replaced with a new capture/encryption key.
                if !self.store.insert(&id, &saved)? {
                    return Err("file_send_outcome_unknown");
                }
                let media = match self.source.upload(encrypted.ciphertext().to_vec()).await {
                    Ok(media) => media,
                    Err(_) => return Ok(saved.receipt()),
                };
                hagency_matrix::MediaId::new(&media).map_err(|_| "file_upload_outcome_unknown")?;
                let mut descriptor: Value =
                    serde_json::from_slice(encrypted.descriptor().private_event_json())
                        .map_err(|_| "file_encryption_failed")?;
                descriptor["url"] = media.into();
                let mut content = json!({"msgtype":"m.file","body":caption.as_deref().unwrap_or(filename),"filename":filename,
                    "info":{"size":size,"mimetype":"application/octet-stream"},"file":descriptor,
                    "m.relates_to":{"m.in_reply_to":{"event_id":self.reply_to}}});
                if let Some(root) = &self.thread {
                    content["m.relates_to"]["rel_type"] = "m.thread".into();
                    content["m.relates_to"]["event_id"] = root.clone().into();
                    content["m.relates_to"]["is_falling_back"] = true.into();
                }
                saved.content = Some(content);
                self.store.update(&id, &saved)?;
                saved
            }
        };
        if saved.device != self.source.identity()? {
            return Err("file_delivery_device_changed");
        }
        let content = saved.content.clone().ok_or("file_send_outcome_unknown")?;
        // The prepared descriptor and transaction survive restart. An exact
        // replay may retry this same event, but never re-upload or recapture.
        match self.source.send(&self.room, &id, content).await {
            Ok(event) => {
                if !event.starts_with('$') || event.len() > 255 {
                    return Err("file_send_outcome_unknown");
                }
                saved.event = Some(event);
                self.store.update(&id, &saved)?;
            }
            Err(_) => {}
        }
        Ok(saved.receipt())
    }
}
