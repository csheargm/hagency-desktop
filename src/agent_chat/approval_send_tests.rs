//! The actual SDK must stop inside encryption preparation when a claim changes.
use super::*;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    ruma::{api::MatrixVersion, device_id, event_id, room_id, user_id},
    SessionMeta, SessionTokens,
};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Semaphore},
};

struct Server {
    origin: String,
    members_started: mpsc::UnboundedReceiver<()>,
    release_members: Arc<Semaphore>,
    sends: Arc<AtomicUsize>,
    trace: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        // The listener owns its JoinSet, so abort also drops blocked handlers.
        self.task.abort();
    }
}
fn member(user: &str, event: &str) -> Value {
    json!({"type":"m.room.member","state_key":user,"sender":user,
        "event_id":event,"origin_server_ts":1,"content":{"membership":"join"}})
}
impl Server {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (started, members_started) = mpsc::unbounded_channel();
        let release_members = Arc::new(Semaphore::new(0));
        let release = release_members.clone();
        let sends = Arc::new(AtomicUsize::new(0));
        let observed = sends.clone();
        let trace = Arc::new(Mutex::new(Vec::new()));
        let requests = trace.clone();
        let task = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let started = started.clone();
                let release = release.clone();
                let observed = observed.clone();
                let requests = requests.clone();
                handlers.spawn(async move {
                    let mut bytes = Vec::new();
                    let mut chunk = [0; 4096];
                    let head_end = loop {
                        let n = stream.read(&mut chunk).await.unwrap();
                        if n == 0 { return; }
                        bytes.extend_from_slice(&chunk[..n]);
                        assert!(bytes.len() < 1_048_576);
                        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            break at + 4;
                        }
                    };
                    let head = String::from_utf8_lossy(&bytes[..head_end]).to_string();
                    let length = head.lines().find_map(|line| {
                        line.to_ascii_lowercase().strip_prefix("content-length:")
                            .and_then(|n| n.trim().parse::<usize>().ok())
                    }).unwrap_or(0);
                    assert!(length < 1_048_576);
                    while bytes.len() < head_end + length {
                        let n = stream.read(&mut chunk).await.unwrap();
                        if n == 0 { return; }
                        bytes.extend_from_slice(&chunk[..n]);
                    }
                    assert!(head.to_ascii_lowercase().contains("authorization: bearer approval-fixture"));
                    let route = head.lines().next().unwrap();
                    requests.lock().unwrap().push(route.to_owned());
                    let owner = member("@owner:example.test", "$owner");
                    let publisher = member("@approval:example.test", "$publisher");
                    let encryption = json!({"type":"m.room.encryption","state_key":"",
                        "sender":"@owner:example.test","event_id":"$encryption","origin_server_ts":1,
                        "content":{"algorithm":"m.megolm.v1.aes-sha2"}});
                    let (status, response) = if route.contains("/sync?") {
                        (200, json!({"next_batch":"batch1", "rooms":{"join":{
                            "!private:example.test":{"state":{"events":[owner,publisher,encryption]},
                            "timeline":{"events":[],"limited":false,"prev_batch":"prev"},
                            "ephemeral":{"events":[]},"account_data":{"events":[]}}}},
                            "device_lists":{"changed":[],"left":[]},
                            "device_one_time_keys_count":{"signed_curve25519":50},
                            "to_device":{"events":[]},"account_data":{"events":[]}}))
                    } else if route.contains("/members") {
                        started.send(()).unwrap();
                        release.acquire().await.unwrap().forget();
                        (200, json!({"chunk":[owner,publisher]}))
                    } else if route.contains("/state/m.room.encryption") {
                        (200, encryption["content"].clone())
                    } else if route.contains("/keys/query") {
                        let query: Value = serde_json::from_slice(
                            &bytes[head_end..head_end + length],
                        ).unwrap();
                        // An omitted user remains dirty in the real SDK, which
                        // then waits for another query during session sharing.
                        // Explicit empty maps are a completed no-devices result.
                        let devices: serde_json::Map<String, Value> = query["device_keys"]
                            .as_object().unwrap().keys().map(|user| {
                                assert!(matches!(user.as_str(),
                                    "@owner:example.test" | "@approval:example.test"));
                                (user.clone(), json!({}))
                            }).collect();
                        (200, json!({"device_keys":devices,"failures":{}}))
                    } else if route.contains("/keys/upload") {
                        (200, json!({"one_time_key_counts":{"signed_curve25519":50}}))
                    } else if route.contains("/sendToDevice/") {
                        (200, json!({}))
                    } else if route.contains("/send/") {
                        assert!(route.contains("m.room.encrypted"), "verdict must be encrypted: {route}");
                        observed.fetch_add(1, Ordering::AcqRel);
                        (200, json!({"event_id":"$verdict"}))
                    } else {
                        (404, json!({"errcode":"M_NOT_FOUND","error":"fixture route unavailable"}))
                    };
                    let body = response.to_string();
                    requests.lock().unwrap().push(format!("{status} {route}"));
                    let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    // A correctly cancelled SDK request can close its socket.
                    let _ = stream.write_all(response.as_bytes()).await;
                });
                while let Some(result) = handlers.try_join_next() {
                    result.unwrap();
                }
            }
        });
        Self {
            origin,
            members_started,
            release_members,
            sends,
            trace,
            task,
        }
    }
    async fn room(&self) -> matrix_sdk::Room {
        let client = Client::builder()
            .homeserver_url(&self.origin)
            .server_versions([MatrixVersion::V1_11])
            .build()
            .await
            .unwrap();
        client
            .restore_session(MatrixSession {
                meta: SessionMeta {
                    user_id: user_id!("@owner:example.test").to_owned(),
                    device_id: device_id!("OWNER").to_owned(),
                },
                tokens: SessionTokens {
                    access_token: "approval-fixture".into(),
                    refresh_token: None,
                },
            })
            .await
            .unwrap();
        client
            .encryption()
            .wait_for_e2ee_initialization_tasks()
            .await;
        client.sync_once(SyncSettings::default()).await.unwrap();
        let room = client.get_room(room_id!("!private:example.test")).unwrap();
        assert!(
            !room.are_members_synced(),
            "send must enter the controlled SDK await"
        );
        assert!(room.encryption_state().is_encrypted());
        room
    }
}

#[tokio::test]
async fn sdk_encryption_await_cancels_revoked_claims_and_current_claim_sends() {
    for interruption in [
        "terminal", "conflict", "expiry", "relogin", "account", "none",
    ] {
        let mut server = Server::new().await;
        let room = server.room().await;
        let now = current_unix_time_millis();
        let fixture: Value = serde_json::from_str(include_str!("../../crates/hagency-client/native/hagency-store/tests/fixtures/native-approval-wire.json")).unwrap();
        let mut content = fixture["cards"][0]["content"].clone();
        content["com.agentchat.approval"]["expires_at"] = json!(
            now + if interruption == "expiry" {
                2000
            } else {
                30_000
            }
        );
        let request = super::super::approval::parse_approval_request(&content).unwrap();
        let mut state = Runtime {
            owner: Some(user_id!("@owner:example.test").to_owned()),
            ..Default::default()
        };
        assert!(ingest_original(
            &mut state,
            room.room_id(),
            event_id!("$request"),
            user_id!("@approval:example.test"),
            &content,
            now
        ));
        let claim = claim_locked(
            &mut state,
            user_id!("@owner:example.test"),
            room.room_id(),
            event_id!("$request"),
            user_id!("@approval:example.test"),
            &request,
            request.action("approve_once").unwrap(),
            now,
        )
        .unwrap();
        let state = Arc::new(Mutex::new(state));
        let task_state = state.clone();
        let verdict = request.verdict_content(
            request.action("approve_once").unwrap(),
            event_id!("$request"),
        );
        let task_claim = claim.clone();
        let sending = tokio::spawn(async move {
            let transaction: matrix_sdk::ruma::OwnedTransactionId =
                task_claim.transaction_id.clone().into();
            while_current(
                room.send_raw("m.room.message", verdict)
                    .with_transaction_id(&transaction),
                task_claim.changed.clone(),
                task_claim.expires_at_millis,
                || {
                    claim_current_locked(
                        &task_state.lock().unwrap(),
                        &task_claim,
                        current_unix_time_millis(),
                    ) && room.state() == matrix_sdk::RoomState::Joined
                },
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), server.members_started.recv())
            .await
            .expect("SDK must reach encrypted member preparation")
            .unwrap();
        {
            let mut state = state.lock().unwrap();
            match interruption {
                "terminal" => {
                    let mut terminal = content.clone();
                    terminal["msgtype"] = json!("com.agentchat.approval.status.v1");
                    terminal["com.agentchat.approval"]["kind"] = json!("status");
                    terminal["com.agentchat.approval"]["state"] = json!("denied");
                    terminal["com.agentchat.approval"]["decision"] = json!("deny");
                    terminal["com.agentchat.approval"]["revision"] = json!(2);
                    terminal["com.agentchat.approval"]
                        .as_object_mut()
                        .unwrap()
                        .remove("actions");
                    assert!(ingest_original(
                        &mut state,
                        room_id!("!private:example.test"),
                        event_id!("$terminal"),
                        user_id!("@approval:example.test"),
                        &terminal,
                        current_unix_time_millis()
                    ));
                }
                "conflict" => {
                    let mut conflict = content.clone();
                    conflict["com.agentchat.approval"]["input_digest"] = json!("f".repeat(64));
                    assert!(ingest_original(
                        &mut state,
                        room_id!("!private:example.test"),
                        event_id!("$conflict"),
                        user_id!("@approval:example.test"),
                        &conflict,
                        current_unix_time_millis()
                    ));
                }
                "relogin" | "account" => {
                    // Same replacement as reset(): old channel closes even
                    // when the replacement has the same account and device.
                    *state = Runtime {
                        epoch: state.epoch + 1,
                        owner: Some(
                            if interruption == "account" {
                                user_id!("@other:example.test")
                            } else {
                                user_id!("@owner:example.test")
                            }
                            .to_owned(),
                        ),
                        ..Default::default()
                    };
                }
                "expiry" => {} // No event wake-up: the real expiry timer must cancel.
                "none" => server.release_members.add_permits(1),
                _ => unreachable!(),
            }
        }
        let outcome = tokio::time::timeout(Duration::from_secs(5), sending)
            .await
            .unwrap_or_else(|_| panic!("send did not settle after {interruption}: {:?}", server.trace.lock().unwrap()))
            .unwrap();
        if interruption == "none" {
            outcome.unwrap().unwrap();
            assert_eq!(server.sends.load(Ordering::Acquire), 1);
        } else {
            assert!(
                outcome.is_err(),
                "stale SDK send completed after {interruption}"
            );
            // Completion occurs while /members is still blocked. Releasing
            // it must not let any continuation issue the encrypted verdict.
            server.release_members.add_permits(1);
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(
                server.sends.load(Ordering::Acquire),
                0,
                "sent after {interruption}"
            );
            complete_locked(
                &mut state.lock().unwrap(),
                &claim,
                model::SendResult::OutcomeUnknown,
                current_unix_time_millis(),
            );
        }
        eprintln!("approval SDK case={interruption} sends={} trace={:?}",
            server.sends.load(Ordering::Acquire), server.trace.lock().unwrap());
    }
}
