use super::*;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

pub(in crate::console::native) struct Endpoint {
    pub records: PathBuf,
    pub room: String,
    pub original: Vec<u8>,
    pub receives: AtomicUsize,
    pub uploads: Mutex<Vec<Vec<u8>>>,
    pub events: Mutex<HashMap<String, Value>>,
    pub lose_send: AtomicBool,
    pub lose_upload: AtomicBool,
    pub changed_device: AtomicBool,
    pub stall_receive: AtomicBool,
    pub stall_upload: AtomicBool,
    pub stall_send: AtomicBool,
}
impl Endpoint {
    pub fn new(records: PathBuf, room: String) -> Self {
        Self {
            records,
            room,
            original: b"original file\0\xff".to_vec(),
            receives: 0.into(),
            uploads: Default::default(),
            events: Default::default(),
            lose_send: false.into(),
            lose_upload: false.into(),
            changed_device: false.into(),
            stall_receive: false.into(),
            stall_upload: false.into(),
            stall_send: false.into(),
        }
    }
}
impl FileIo for Endpoint {
    fn identity(&self) -> Result<String> {
        Ok(if self.changed_device.load(Ordering::SeqCst) {
            "other-device"
        } else {
            "original-device"
        }
        .into())
    }
    async fn receive(&self, room: &str, event: &str) -> Result<RoomFile> {
        assert_eq!(room, self.room);
        assert_eq!(event, "$attachment:test");
        self.receives.fetch_add(1, Ordering::SeqCst);
        if self.stall_receive.swap(false, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        Ok(RoomFile {
            bytes: self.original.clone(),
            filename: "上传文档.pdf".into(),
            mime: Some("application/pdf".into()),
        })
    }
    async fn upload(&self, bytes: Vec<u8>) -> Result<String> {
        self.uploads.lock().unwrap().push(bytes);
        if self.stall_upload.swap(false, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.lose_upload.swap(false, Ordering::SeqCst) {
            return Err("lost_upload_response");
        }
        Ok("mxc://example.test/encrypted-file".into())
    }
    async fn send(&self, room: &str, transaction: &str, content: Value) -> Result<String> {
        assert_eq!(room, self.room);
        {
            let mut events = self.events.lock().unwrap();
            if let Some(existing) = events.get(transaction) {
                assert_eq!(*existing, content);
            }
            events.insert(transaction.into(), content);
        }
        if self.stall_send.swap(false, Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        if self.lose_send.swap(false, Ordering::SeqCst) {
            return Err("lost_send_response");
        }
        Ok("$delivered:test".into())
    }
}
fn work() -> SharedWorkItem {
    SharedWorkItem {
        room_binding: None,
        encrypted: true,
        id: "work_files".into(),
        assignment_id: "agent_files".into(),
        runtime_command_digest: "a".repeat(64),
        event_id: "$request:test".into(),
        room_id: "!room:test".into(),
        requester_mxid: "@user:test".into(),
        thread_root: "$request:test".into(),
        body: "$request:test".into(),
        body_digest: "b".repeat(64),
        execution_id: "execution_files".into(),
        created_at_ms: 1,
    }
}
fn fixture() -> (tempfile::TempDir, Arc<Endpoint>, Files<Endpoint>, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let workspace = root.join("workspace");
    hagency_platform::private_state::directory(&workspace).unwrap();
    let endpoint = Arc::new(Endpoint::new(root.join("receipts"), work().room_id));
    let files = Files::open(endpoint.clone(), &work(), &workspace, &endpoint.records).unwrap();
    (temp, endpoint, files, workspace)
}

#[tokio::test]
async fn chat_files_common_constructor_preserves_receipts_and_separates_personal_bindings() {
    let (_temp, endpoint, mut original, workspace) = fixture();
    std::fs::write(workspace.join("report.pdf"), b"original content").unwrap();
    let delivered = original.send(&send("report.pdf")).await.unwrap();
    drop(original);
    let work = work();
    let from_scope = |namespace| Files::for_room(endpoint.clone(), Conversation {
        namespace, room: &work.room_id, event: &work.event_id, requester: &work.requester_mxid, thread: None,
    }, &workspace, &endpoint.records).unwrap();
    let mut same = from_scope(&work.assignment_id);
    assert_eq!(same.send(&send("report.pdf")).await.unwrap(), delivered);
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 1);
    let new_binding = from_scope("personal-agent-binding-generation-2");
    assert_eq!(new_binding.delivery(delivered["delivery_id"].as_str().unwrap()).unwrap_err(), "file_delivery_not_found");
}
fn send(path: &str) -> RoomFileOperation {
    RoomFileOperation::SendFile {
        call_id: "report-1".into(),
        path: path.into(),
        filename: Some("结果.pdf".into()),
        caption: None,
    }
}

fn send_with_id(path: &str, id: &str) -> RoomFileOperation {
    let mut operation = send(path);
    if let RoomFileOperation::SendFile { call_id, .. } = &mut operation {
        *call_id = id.into();
    }
    operation
}

#[tokio::test]
async fn chat_files_shared_send_count_survives_restart_and_replays_at_the_limit() {
    let (_temp, endpoint, mut files, workspace) = fixture();
    files.send_allowance = Some(SendAllowance { count: 2, bytes: 100 });
    std::fs::write(workspace.join("report.pdf"), b"abc").unwrap();
    let original = files.send(&send_with_id("report.pdf", "first")).await.unwrap();
    endpoint.lose_upload.store(true, Ordering::SeqCst);
    let uncertain = files.send(&send_with_id("report.pdf", "second")).await.unwrap();
    assert_eq!(uncertain["status"], "outcome_unknown");
    drop(files);
    let mut files = Files::open(endpoint.clone(), &work(), &workspace, &endpoint.records).unwrap();
    files.send_allowance = Some(SendAllowance { count: 2, bytes: 100 });
    assert_eq!(files.send(&send_with_id("report.pdf", "first")).await.unwrap(), original);
    assert_eq!(files.send(&send_with_id("report.pdf", "second")).await.unwrap(), uncertain);
    assert_eq!(files.send(&send_with_id("report.pdf", "third")).await.unwrap_err(), "file_send_turn_limit");
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 2);
    assert_eq!(endpoint.events.lock().unwrap().len(), 1);
    let mut next = work();
    next.event_id = "$next-request:test".into();
    next.thread_root = next.event_id.clone();
    let mut files = Files::open(endpoint.clone(), &next, &workspace, &endpoint.records).unwrap();
    files.send_allowance = Some(SendAllowance { count: 2, bytes: 100 });
    assert_eq!(files.send(&send_with_id("report.pdf", "first")).await.unwrap()["status"], "delivered");
}

#[tokio::test]
async fn chat_files_shared_send_bytes_refuse_before_upload_without_stopping_other_operations() {
    let (_temp, endpoint, mut files, workspace) = fixture();
    files.send_allowance = Some(SendAllowance { count: 8, bytes: 5 });
    std::fs::write(workspace.join("report.pdf"), b"abc").unwrap();
    let original = files.send(&send_with_id("report.pdf", "first")).await.unwrap();
    assert_eq!(files.send(&send_with_id("report.pdf", "second")).await.unwrap_err(), "file_send_turn_limit");
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 1);
    assert_eq!(files.delivery(original["delivery_id"].as_str().unwrap()).unwrap(), original);
    assert_eq!(files.receive("$attachment:test").await.unwrap()["status"], "ready");
    std::fs::write(workspace.join("small.txt"), b"de").unwrap();
    assert_eq!(files.send(&send_with_id("small.txt", "third")).await.unwrap()["status"], "delivered");
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn chat_files_round_trip_preserves_binary_and_replay_never_overwrites() {
    let (_temp, endpoint, mut files, workspace) = fixture();
    let received = files.receive("$attachment:test").await.unwrap();
    let path = received["path"].as_str().unwrap();
    assert_eq!(
        std::fs::read(workspace.join(path)).unwrap(),
        endpoint.original
    );
    let replay = files.receive("$attachment:test").await.unwrap();
    assert_eq!(received, replay);
    assert_eq!(endpoint.receives.load(Ordering::SeqCst), 1);
    let sent = files.send(&send(path)).await.unwrap();
    assert_eq!(sent["status"], "delivered");
    let events = endpoint.events.lock().unwrap();
    let content = events.values().next().unwrap();
    assert_eq!(content["msgtype"], "m.file");
    assert_eq!(content["filename"], "结果.pdf");
    assert_eq!(
        content["m.relates_to"]["m.in_reply_to"]["event_id"],
        "$request:test"
    );
    let mut descriptor = content["file"].clone();
    descriptor.as_object_mut().unwrap().remove("url");
    let descriptor = hagency_media::Descriptor::from_private_event_json(
        &serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    let uploads = endpoint.uploads.lock().unwrap();
    assert_ne!(uploads[0], endpoint.original);
    let codec = hagency_media::Codec::new(hagency_media::Limits::new(MAX_BYTES, 1).unwrap());
    assert_eq!(
        codec.decrypt(&descriptor, &uploads[0]).unwrap().bytes(),
        endpoint.original
    );
    assert!(!sent.to_string().contains("mxc://"));
    drop(uploads);
    drop(events);
    std::fs::write(workspace.join(path), "user changed the received copy").unwrap();
    assert_eq!(
        files.receive("$attachment:test").await.unwrap_err(),
        "received_file_changed"
    );
    assert_eq!(
        files.send(&send(path)).await.unwrap(),
        sent,
        "delivered replay does not recapture modified bytes"
    );
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn chat_files_lost_ack_survives_restart_and_reuses_prepared_encrypted_event() {
    let (_temp, endpoint, mut files, workspace) = fixture();
    std::fs::write(workspace.join("report.pdf"), b"first immutable content").unwrap();
    endpoint.lose_send.store(true, Ordering::SeqCst);
    let unknown = files.send(&send("report.pdf")).await.unwrap();
    assert_eq!(unknown["status"], "outcome_unknown");
    assert_eq!(unknown["retry_same_call"], true);
    drop(files);
    let mut files = Files::open(endpoint.clone(), &work(), &workspace, &endpoint.records).unwrap();
    assert_eq!(
        files
            .delivery(unknown["delivery_id"].as_str().unwrap())
            .unwrap(),
        unknown
    );
    std::fs::write(
        workspace.join("report.pdf"),
        b"changed content after restart",
    )
    .unwrap();
    assert_eq!(
        files.send(&send("different.pdf")).await.unwrap_err(),
        "file_call_id_conflict"
    );
    assert_eq!(
        files.send(&send("report.pdf")).await.unwrap()["status"],
        "delivered"
    );
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 1);
    assert_eq!(endpoint.events.lock().unwrap().len(), 1);
    let mut other = work();
    other.room_id = "!other:test".into();
    let other = Files::open(endpoint.clone(), &other, &workspace, &endpoint.records).unwrap();
    assert_eq!(
        other
            .delivery(unknown["delivery_id"].as_str().unwrap())
            .unwrap_err(),
        "file_delivery_not_found"
    );
}

#[tokio::test]
async fn chat_files_uncertain_upload_and_replaced_device_never_duplicate_transfer() {
    for upload in [true, false] {
        let (_temp, endpoint, mut files, workspace) = fixture();
        std::fs::write(workspace.join("report.pdf"), b"content").unwrap();
        if upload {
            endpoint.lose_upload.store(true, Ordering::SeqCst);
        } else {
            endpoint.lose_send.store(true, Ordering::SeqCst);
        }
        let original = files.send(&send("report.pdf")).await.unwrap();
        assert_eq!(original["status"], "outcome_unknown");
        endpoint.changed_device.store(true, Ordering::SeqCst);
        let replay = files.send(&send("report.pdf")).await;
        if upload {
            assert_eq!(replay.unwrap(), original);
        } else {
            assert_eq!(replay.unwrap_err(), "file_delivery_device_changed");
        }
        assert_eq!(endpoint.uploads.lock().unwrap().len(), 1);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn chat_files_reject_traversal_links_oversize_and_replaced_workspace() {
    let (temp, endpoint, mut files, workspace) = fixture();
    std::fs::write(temp.path().join("secret"), b"private").unwrap();
    std::os::unix::fs::symlink(temp.path().join("secret"), workspace.join("link")).unwrap();
    std::fs::hard_link(temp.path().join("secret"), workspace.join("hardlink")).unwrap();
    for path in ["../secret", "/etc/passwd", "link", "hardlink"] {
        assert!(files.send(&send(path)).await.is_err(), "{path}");
    }
    std::fs::write(workspace.join("large"), vec![0; MAX_BYTES + 1]).unwrap();
    assert!(files.send(&send("large")).await.is_err());
    std::fs::rename(&workspace, temp.path().join("old-workspace")).unwrap();
    hagency_platform::private_state::directory(&workspace).unwrap();
    std::fs::write(workspace.join("new"), b"replaced").unwrap();
    assert_eq!(
        files.send(&send("new")).await.unwrap_err(),
        "file_workspace_changed"
    );
    assert!(endpoint.uploads.lock().unwrap().is_empty());
}

#[tokio::test]
async fn chat_files_receive_restart_never_truncates_existing_destination() {
    let (_temp, endpoint, mut files, workspace) = fixture();
    let id = format!(
        "receive_{}",
        sha(format!("{}|$attachment:test", files.scope))
    );
    std::fs::write(workspace.join(format!("{id}.pdf")), b"existing").unwrap();
    assert_eq!(
        files.receive("$attachment:test").await.unwrap_err(),
        "file_receive_destination_exists"
    );
    drop(files);
    let mut files = Files::open(endpoint.clone(), &work(), &workspace, &endpoint.records).unwrap();
    assert_eq!(
        files.receive("$attachment:test").await.unwrap_err(),
        "file_receive_outcome_unknown"
    );
    assert_eq!(
        std::fs::read(workspace.join(format!("{id}.pdf"))).unwrap(),
        b"existing"
    );
    assert_eq!(endpoint.receives.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn chat_files_cancelled_send_keeps_original_transaction_after_restart() {
    let (_temp, endpoint, mut files, workspace) = fixture();
    std::fs::write(workspace.join("report.pdf"), b"original").unwrap();
    endpoint.stall_send.store(true, Ordering::SeqCst);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), files.send(&send("report.pdf")))
            .await
            .is_err()
    );
    drop(files);
    let mut files = Files::open(endpoint.clone(), &work(), &workspace, &endpoint.records).unwrap();
    assert_eq!(
        files.send(&send("report.pdf")).await.unwrap()["status"],
        "delivered"
    );
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 1);
    assert_eq!(endpoint.events.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn chat_files_network_timeouts_preserve_unknown_outcomes_and_allow_safe_replay() {
    let (_temp, endpoint, mut files, _workspace) = fixture();
    files.transfer_timeout = Duration::from_millis(20);
    endpoint.stall_receive.store(true, Ordering::SeqCst);
    assert_eq!(
        files.receive("$attachment:test").await.unwrap_err(),
        "file_receive_timed_out"
    );
    assert_eq!(
        files.receive("$attachment:test").await.unwrap()["status"],
        "ready"
    );
    for upload in [true, false] {
        let (_temp, endpoint, mut files, workspace) = fixture();
        files.transfer_timeout = Duration::from_millis(20);
        std::fs::write(workspace.join("report.pdf"), b"original").unwrap();
        if upload {
            endpoint.stall_upload.store(true, Ordering::SeqCst);
        } else {
            endpoint.stall_send.store(true, Ordering::SeqCst);
        }
        let unknown = files.send(&send("report.pdf")).await.unwrap();
        assert_eq!(unknown["status"], "outcome_unknown");
        assert_eq!(unknown["retry_same_call"], !upload);
        drop(files);
        let mut files =
            Files::open(endpoint.clone(), &work(), &workspace, &endpoint.records).unwrap();
        let replay = files.send(&send("report.pdf")).await.unwrap();
        assert_eq!(
            replay["status"],
            if upload {
                "outcome_unknown"
            } else {
                "delivered"
            }
        );
        assert_eq!(endpoint.uploads.lock().unwrap().len(), 1);
        assert_eq!(endpoint.events.lock().unwrap().len(), usize::from(!upload));
    }
}

#[tokio::test]
async fn chat_files_long_turn_uses_durable_receipts_without_a_cumulative_transfer_cap() {
    let (_temp, endpoint, mut files, workspace) = fixture();
    std::fs::write(workspace.join("report.pdf"), b"bounded").unwrap();
    let mut receipt = Value::Null;
    for n in 0..32 {
        let mut op = send("report.pdf");
        if let RoomFileOperation::SendFile { call_id, .. } = &mut op {
            *call_id = format!("report-{n}");
        }
        receipt = files.send(&op).await.unwrap();
    }
    assert_eq!(files.receive("$attachment:test").await.unwrap()["status"], "ready");
    assert_eq!(endpoint.receives.load(Ordering::SeqCst), 1);
    assert_eq!(
        files
            .delivery(receipt["delivery_id"].as_str().unwrap())
            .unwrap(),
        receipt
    );
    drop(files);
    let mut files = Files::open(endpoint.clone(), &work(), &workspace, &endpoint.records).unwrap();
    for n in 0..32 {
        let mut op = send("report.pdf");
        if let RoomFileOperation::SendFile { call_id, .. } = &mut op {
            *call_id = format!("report-{n}");
        }
        assert_eq!(files.send(&op).await.unwrap()["status"], "delivered");
    }
    assert_eq!(files.receive("$attachment:test").await.unwrap()["status"], "ready");
    assert_eq!(endpoint.receives.load(Ordering::SeqCst), 1);
    assert_eq!(endpoint.uploads.lock().unwrap().len(), 32);
    assert_eq!(endpoint.events.lock().unwrap().len(), 32);
}
