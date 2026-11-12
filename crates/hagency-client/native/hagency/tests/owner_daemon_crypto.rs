//! Isolated real Server/Pasion/Palpo + daemon host subprocess + Matrix SDK E2EE.
//! Run only with the smoke harness's private loopback fixture. The provider is
//! an explicit deterministic subprocess; no production login or inference.
#![cfg(unix)]
#![recursion_limit = "512"]

use hagency::daemon::{self, bridge::ControllerClient, enrollment::Enrollment, host};
use hagency::native_owner::{
    Command, LocalAction, MatrixAccessToken, MatrixTokenSource, NativeError, NativeOwner,
    ProviderAction,
};
use hagency_agent_local::{Budget, Limit, Period, Policy, RequestPolicy, ToolPolicy};
use matrix_sdk::ruma::{
    OwnedRoomId, OwnedUserId,
    events::{
        Mentions,
        room::message::{OriginalSyncRoomMessageEvent, RoomMessageEventContent},
    },
};
use matrix_sdk::{
    Client, SessionMeta, SessionTokens, authentication::matrix::MatrixSession,
    config::SyncSettings, store::RoomLoadSettings,
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

const ANSWER: &str = "OFFLINE_ENCRYPTED_DAEMON_ANSWER";
struct Source(Value);
impl MatrixTokenSource for Source {
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async {
            Ok(MatrixAccessToken {
                access_token: self.0["accessToken"].as_str().unwrap().into(),
                client_id: self.0["clientId"].as_str().unwrap().into(),
            })
        })
    }
}
fn local(agent: &str, action: LocalAction, input: Option<Value>) -> Command {
    Command::Local {
        agent_id: agent.into(),
        action,
        input,
        binding_id: None,
        requester: None,
    }
}
async fn http(
    base: &str,
    token: &str,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Value {
    let mut request = reqwest::Client::new()
        .request(method, format!("{base}{path}"))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let reply = request.send().await.unwrap();
    let status = reply.status();
    let value: Value = reply.json().await.unwrap();
    assert!(
        status.is_success(),
        "fixture request {path}: {status}: {value}"
    );
    value
}
fn segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}
async fn wait_running(home: &Path) {
    let limit = tokio::time::Instant::now() + Duration::from_secs(45);
    loop {
        if let host::Presence::Running(v) = host::probe(home).await {
            if v.hosting == hagency_metering::dashboard::HostingState::Running {
                return;
            }
        }
        assert!(
            tokio::time::Instant::now() < limit,
            "daemon did not reach Running"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
struct DaemonProcess(tokio::process::Child);
impl DaemonProcess {
    async fn start(home: &Path, log: &Path) -> Self {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .unwrap();
        let child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "isolated_owner_daemon_process",
                "--ignored",
                "--nocapture",
            ])
            .env("HAGENCY_DAEMON_CRYPTO_HOME", home)
            .env("RUST_LOG", "hagency=debug,matrix_sdk=warn")
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let process = Self(child);
        wait_running(home).await;
        process
    }
    async fn stop(mut self) {
        // Signal only this test's owned child. Production shutdown retains
        // custody through drain; no daemon service or other process is touched.
        let pid = self.0.id().unwrap();
        assert!(
            std::process::Command::new("/bin/kill")
                .args(["-TERM", &pid.to_string()])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(30), self.0.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}
async fn create_agent(native: &NativeOwner, name: &str) -> Value {
    for _ in 0..90 {
        let v = native
            .execute(Command::CreateAgent {
                input: json!({"displayName":name,"idempotencyKey":name}),
            })
            .await
            .unwrap();
        if v["commandState"] == "active" {
            return v["creation"]["agent"].clone();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("agent creation incomplete");
}
async fn bindings(native: &NativeOwner, agent: &str, expected: usize) -> Vec<Value> {
    let mut last = json!({});
    for _ in 0..90 {
        last = native
            .execute(Command::Bindings {
                agent_id: agent.into(),
            })
            .await
            .unwrap();
        let rows: Vec<Value> = last["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|b| b["state"] == "active")
            .cloned()
            .collect();
        if rows.len() >= expected {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    panic!("bindings incomplete: {last}");
}
fn turns(marker: &Path) -> Vec<Value> {
    std::fs::read_to_string(marker)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
async fn reply_count(replies: &Arc<Mutex<Vec<(String, String)>>>, marker: &Path, expected: usize) {
    let limit = tokio::time::Instant::now() + Duration::from_secs(100);
    loop {
        if replies.lock().unwrap().len() >= expected {
            break;
        }
        assert!(
            tokio::time::Instant::now() < limit,
            "encrypted replies timed out; actual model turns={} replies={:?}",
            turns(marker).len(),
            replies.lock().unwrap()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(
        turns(marker).len(),
        expected,
        "one model execution per accepted encrypted request"
    );
}
async fn send_expected(
    client: &Client,
    room: &str,
    text: &str,
    mention: Option<&str>,
    expected: &mut Vec<(String, String)>,
) -> String {
    let event = send(client, room, text, mention).await;
    expected.push((room.to_owned(), event.clone()));
    event
}
fn reply_parent(content: &RoomMessageEventContent) -> String {
    let value = serde_json::to_value(content).unwrap();
    value["m.relates_to"]["m.in_reply_to"]["event_id"]
        .as_str()
        .or_else(|| value["m.relates_to"]["event_id"].as_str())
        .unwrap_or("")
        .to_owned()
}
async fn send(client: &Client, room: &str, text: &str, mention: Option<&str>) -> String {
    let room: OwnedRoomId = room.try_into().unwrap();
    let room = client.get_room(&room).expect("human joined room");
    assert!(room.latest_encryption_state().await.unwrap().is_encrypted());
    let mut content = RoomMessageEventContent::text_plain(text);
    if let Some(agent) = mention {
        content.mentions = Some(Mentions::with_user_ids([
            OwnedUserId::try_from(agent).unwrap()
        ]));
    }
    room.send(content)
        .await
        .unwrap()
        .response
        .event_id
        .to_string()
}

#[test]
#[ignore = "requires private isolated Server/Pasion/Palpo HOLD_CLIENT fixture"]
fn real_daemon_encrypted_dm_group_restart() {
    let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let provider = root.path().join("offline-provider");
    let marker = root.path().join("turns.jsonl");
    let script = include_str!("fixtures/daemon_crypto_provider.py").replace(
        "'__FIXTURE_MARKER__'",
        &serde_json::to_string(&marker).unwrap(),
    );
    std::fs::write(&provider, script).unwrap();
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&marker, "").unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "real_daemon_encrypted_child",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HAGENCY_DAEMON_CRYPTO_ROOT", root.path())
        .env("HAGENCY_DAEMON_CRYPTO_TURNS", &marker)
        .env("HAGENCY_CODEX_BINARY", &provider)
        .env("HAGENCY_CLAUDE_BINARY", root.path().join("no-claude"))
        .env("HAGENCY_OCTOS_BINARY", root.path().join("no-octos"))
        .output()
        .unwrap();
    if !result.status.success() {
        // Keep only this disposable test tree for diagnostics on failure.
        let path = root.keep();
        panic!(
            "isolated crypto fixture failed (diagnostics: {}):\n{}\n{}",
            path.display(),
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }
    print!("{}", String::from_utf8_lossy(&result.stdout));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "isolated child of real_daemon_encrypted_dm_group_restart"]
async fn real_daemon_encrypted_child() {
    hagency_platform::legacy_writer::test_support::initialize_process_root().unwrap();
    let fixture_path =
        PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_FIXTURE").expect("private fixture"));
    let fixture: Value =
        serde_json::from_reader(hagency_store::private::open(&fixture_path, false).unwrap())
            .unwrap();
    let base = fixture["base"].as_str().unwrap();
    let origin = format!("{}/", base.trim_end_matches('/'));
    let url = reqwest::Url::parse(base).unwrap();
    assert_eq!(url.scheme(), "http");
    assert_eq!(url.host_str(), Some("127.0.0.1"));
    let root = PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_ROOT").unwrap())
        .canonicalize()
        .unwrap();
    let marker = PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_TURNS").unwrap());
    let owner = fixture["daemon"]["owner"].as_str().unwrap();
    assert_eq!(fixture["human"]["owner"], owner);
    let daemon_token = fixture["daemon"]["accessToken"].as_str().unwrap();
    let human_token = fixture["human"]["accessToken"].as_str().unwrap();
    let member_owner = fixture["member"]["owner"]
        .as_str()
        .expect("separate real human");
    let member_token = fixture["member"]["accessToken"].as_str().unwrap();
    assert_ne!(member_owner, owner);
    let native_root = root.join("owner");
    let native = NativeOwner::open_with_matrix(
        &native_root,
        &origin,
        owner,
        Arc::new(Source(fixture["daemon"].clone())),
    )
    .await
    .unwrap();
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let agent = create_agent(&native, &format!("DaemonCrypto{unique:x}")).await;
    let id = agent["id"].as_str().unwrap();
    let puppet = agent["puppetMxid"].as_str().unwrap();
    native
        .execute(Command::EnsureAgentOwnerDirect {
            agent_id: id.into(),
        })
        .await
        .unwrap();
    let direct = bindings(&native, id, 1)
        .await
        .into_iter()
        .find(|b| b["scopeKind"] == "owner_direct")
        .unwrap();
    let dm = direct["roomId"].as_str().unwrap();
    http(
        base,
        human_token,
        reqwest::Method::PUT,
        &format!(
            "/_matrix/client/v3/rooms/{}/state/m.room.encryption",
            segment(dm)
        ),
        Some(json!({"algorithm":"m.megolm.v1.aes-sha2"})),
    )
    .await;

    let models = native
        .execute(Command::Provider {
            action: ProviderAction::Models,
        })
        .await
        .unwrap();
    assert_eq!(models["authenticated"], true);
    let workspace = root.join("work");
    std::fs::create_dir(&workspace).unwrap();
    let initial = native.execute(local(id, LocalAction::AgentModel, None)).await.unwrap();
    let initial: Option<hagency_agent_local::ModelProfile> = serde_json::from_value(initial["modelProfile"].clone()).unwrap();
    native.execute(local(id, LocalAction::SaveAgentModel, Some(json!({"expectedProfileDigest":hagency_agent_local::model_profile_digest(initial.as_ref()).unwrap(),"profile":{"model":"fixture","credential_ref":models["credentialRef"],"workspace_root":workspace,"reasoning_effort":"low","web_search":false}})))).await.unwrap();
    let policy = native
        .execute(local(id, LocalAction::AgentPolicy, None))
        .await
        .unwrap();
    native.execute(local(id, LocalAction::SaveAgentPolicy, Some(json!({"expectedRevision":policy["policy"]["revision"],"policy":Policy { budget: Budget { limit: Limit::Tokens(100_000), period: Period::Lifetime }, requests: RequestPolicy::Allow, high_risk: ToolPolicy::Deny }})))).await.unwrap();

    // The second participant has a separate real Pasion/Matrix account.
    let space = http(base, human_token, reqwest::Method::POST, "/_matrix/client/v3/createRoom", Some(json!({"preset":"private_chat","name":"Crypto fixture space","creation_content":{"type":"m.space"}}))).await;
    let group = http(base, human_token, reqwest::Method::POST, "/_matrix/client/v3/createRoom", Some(json!({"preset":"private_chat","name":"Encrypted fixture group","initial_state":[{"type":"m.room.encryption","state_key":"","content":{"algorithm":"m.megolm.v1.aes-sha2"}}]}))).await;
    let group = group["room_id"].as_str().unwrap();
    let server = owner.split_once(':').unwrap().1;
    http(
        base,
        human_token,
        reqwest::Method::PUT,
        &format!(
            "/_matrix/client/v3/rooms/{}/state/m.space.child/{}",
            segment(space["room_id"].as_str().unwrap()),
            segment(group)
        ),
        Some(json!({"via":[server]})),
    )
    .await;
    let project = native
        .execute(Command::AdoptProject {
            space_id: space["room_id"].as_str().unwrap().into(),
        })
        .await
        .unwrap();
    let project = project["project"]["id"].as_str().unwrap();
    native
        .execute(Command::AdoptRoom {
            project_id: project.into(),
            room_id: group.into(),
        })
        .await
        .unwrap();
    native.execute(Command::BindAgent {agent_id:id.into(),input:json!({"projectId":project,"roomId":group,"idempotencyKey":format!("bind-crypto-{unique:x}")})}).await.unwrap();
    bindings(&native, id, 2).await;
    http(
        base,
        human_token,
        reqwest::Method::POST,
        &format!("/_matrix/client/v3/rooms/{}/invite", segment(group)),
        Some(json!({"user_id":member_owner})),
    )
    .await;
    http(
        base,
        member_token,
        reqwest::Method::POST,
        &format!("/_matrix/client/v3/rooms/{}/join", segment(group)),
        Some(json!({})),
    )
    .await;
    let joined = http(
        base,
        human_token,
        reqwest::Method::GET,
        &format!("/_matrix/client/v3/rooms/{}/joined_members", segment(group)),
        None,
    )
    .await;
    assert!(joined["joined"].as_object().unwrap().len() >= 3);
    native.suspend().await.unwrap();
    drop(native);

    let home = root.join("daemon");
    hagency_store::private::directory(&home).unwrap();
    let matrix = home.join(daemon::MATRIX);
    hagency_store::private::directory(&matrix).unwrap();
    let identity = http(
        base,
        daemon_token,
        reqwest::Method::GET,
        "/_matrix/client/v3/account/whoami",
        None,
    )
    .await;
    hagency_store::private::write_new(&matrix.join("session.json"), &serde_json::to_vec(&json!({"version":1,"homeserver":origin,"owner":owner,"oauthClientId":fixture["daemon"]["clientId"],"user":{"user_id":owner,"device_id":identity["device_id"],"access_token":daemon_token,"refresh_token":fixture["daemon"].get("refreshToken")}})).unwrap()).unwrap();
    Enrollment::enroll(&home, &origin, owner, None, &native_root).unwrap();
    let daemon = DaemonProcess::start(&home, &root.join("daemon.log")).await;
    let a = ControllerClient::new(home.clone(), owner.into(), origin.clone(), 1);
    let b = ControllerClient::new(home.clone(), owner.into(), origin.clone(), 2);
    a.execute(local(
        id,
        LocalAction::StartAgentService,
        Some(json!({"reservation":100,"effort":"low"})),
    ))
    .await
    .unwrap();
    b.execute(Command::Agents).await.unwrap();
    a.retire();
    b.retire();
    drop(a);
    drop(b);

    let human_identity = http(
        base,
        human_token,
        reqwest::Method::GET,
        "/_matrix/client/v3/account/whoami",
        None,
    )
    .await;
    let human = Client::builder()
        .homeserver_url(base)
        .sqlite_store(root.join("human-sdk"), Some("isolated-fixture-store-key"))
        .build()
        .await
        .unwrap();
    human
        .matrix_auth()
        .restore_session(
            MatrixSession {
                meta: SessionMeta {
                    user_id: owner.try_into().unwrap(),
                    device_id: human_identity["device_id"].as_str().unwrap().into(),
                },
                tokens: SessionTokens {
                    access_token: human_token.into(),
                    refresh_token: None,
                },
            },
            RoomLoadSettings::default(),
        )
        .await
        .unwrap();
    let replies = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let output = replies.clone();
    let relations = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let related = relations.clone();
    let expected_sender = puppet.to_owned();
    human.add_event_handler(
        move |event: OriginalSyncRoomMessageEvent, room: matrix_sdk::Room| {
            let output = output.clone();
            let related = related.clone();
            let expected_sender = expected_sender.clone();
            async move {
                if event.sender.as_str() == expected_sender && event.content.body().contains(ANSWER)
                {
                    related
                        .lock()
                        .unwrap()
                        .push((room.room_id().to_string(), reply_parent(&event.content)));
                    output
                        .lock()
                        .unwrap()
                        .push((room.room_id().to_string(), event.event_id.to_string()));
                }
            }
        },
    );
    human
        .sync_once(SyncSettings::default().timeout(Duration::ZERO))
        .await
        .unwrap();
    let sync_client = human.clone();
    let sync = tokio::spawn(async move {
        sync_client
            .sync(SyncSettings::default().timeout(Duration::from_secs(1)))
            .await
    });
    let member_identity = http(
        base,
        member_token,
        reqwest::Method::GET,
        "/_matrix/client/v3/account/whoami",
        None,
    )
    .await;
    let member = Client::builder()
        .homeserver_url(base)
        .sqlite_store(root.join("member-sdk"), Some("isolated-member-key"))
        .build()
        .await
        .unwrap();
    member
        .matrix_auth()
        .restore_session(
            MatrixSession {
                meta: SessionMeta {
                    user_id: member_owner.try_into().unwrap(),
                    device_id: member_identity["device_id"].as_str().unwrap().into(),
                },
                tokens: SessionTokens {
                    access_token: member_token.into(),
                    refresh_token: None,
                },
            },
            RoomLoadSettings::default(),
        )
        .await
        .unwrap();
    let member_replies = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let member_output = member_replies.clone();
    let member_sender = puppet.to_owned();
    member.add_event_handler(
        move |event: OriginalSyncRoomMessageEvent, room: matrix_sdk::Room| {
            let output = member_output.clone();
            let sender = member_sender.clone();
            async move {
                if event.sender.as_str() == sender && event.content.body().contains(ANSWER) {
                    output
                        .lock()
                        .unwrap()
                        .push((room.room_id().to_string(), reply_parent(&event.content)));
                }
            }
        },
    );
    member
        .sync_once(SyncSettings::default().timeout(Duration::ZERO))
        .await
        .unwrap();
    let member_sync_client = member.clone();
    let member_sync = tokio::spawn(async move {
        member_sync_client
            .sync(SyncSettings::default().timeout(Duration::from_secs(1)))
            .await
    });
    // Wait for the actual Agent device key upload before sharing a room key.
    let mut keys = json!({});
    for _ in 0..120 {
        keys = http(
            base,
            human_token,
            reqwest::Method::POST,
            "/_matrix/client/v3/keys/query",
            Some(json!({"device_keys":{puppet:[]}})),
        )
        .await;
        if keys["device_keys"][puppet]
            .as_object()
            .is_some_and(|v| !v.is_empty())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(
        !keys["device_keys"][puppet].as_object().unwrap().is_empty(),
        "Agent encryption keys were not uploaded"
    );
    let original_keys = keys["device_keys"][puppet].clone();
    let mut expected_replies = Vec::new();
    let event = send_expected(
        &human,
        dm,
        "DAEMON_DM_AFTER_BOTH_CONTROLLERS_DETACH",
        None,
        &mut expected_replies,
    )
    .await;
    let wire = http(
        base,
        human_token,
        reqwest::Method::GET,
        &format!(
            "/_matrix/client/v3/rooms/{}/event/{}",
            segment(dm),
            segment(&event)
        ),
        None,
    )
    .await;
    assert_eq!(wire["type"], "m.room.encrypted");
    assert!(wire["content"]["body"].is_null());
    reply_count(&replies, &marker, 1).await;
    send(&human, group, "GROUP_WITHOUT_MENTION_MUST_NOT_RUN", None).await;
    send(
        &member,
        group,
        "SECOND_HUMAN_WITHOUT_MENTION_MUST_NOT_RUN",
        None,
    )
    .await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(turns(&marker).len(), 1);
    send_expected(
        &human,
        group,
        "DAEMON_GROUP_MENTION_AFTER_DETACH",
        Some(puppet),
        &mut expected_replies,
    )
    .await;
    reply_count(&replies, &marker, 2).await;
    send_expected(
        &member,
        group,
        "SECOND_HUMAN_EXPLICIT_MENTION",
        Some(puppet),
        &mut expected_replies,
    )
    .await;
    reply_count(&replies, &marker, 3).await;
    daemon.stop().await;
    let daemon = DaemonProcess::start(&home, &root.join("daemon.log")).await;
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(
        turns(&marker).len(),
        3,
        "restart must not reexecute old requests"
    );
    send_expected(
        &human,
        dm,
        "DAEMON_DM_AFTER_PROCESS_RESTART",
        None,
        &mut expected_replies,
    )
    .await;
    reply_count(&replies, &marker, 4).await;
    send_expected(
        &member,
        group,
        "SECOND_HUMAN_MENTION_AFTER_RESTART",
        Some(puppet),
        &mut expected_replies,
    )
    .await;
    reply_count(&replies, &marker, 5).await;
    let executed = turns(&marker);
    assert_ne!(
        executed[0]["cwd"], executed[1]["cwd"],
        "DM and group must use different working directories"
    );
    assert_eq!(
        executed[0]["cwd"], executed[3]["cwd"],
        "the same room keeps its working directory across restart"
    );
    assert_eq!(
        executed[0]["thread"], executed[3]["thread"],
        "restart resumes only the original room context"
    );
    assert_ne!(
        executed[0]["thread"], executed[1]["thread"],
        "group cannot reuse the DM provider context"
    );
    for index in [2, 4] {
        assert_eq!(
            executed[1]["cwd"], executed[index]["cwd"],
            "both humans share only this group's directory across restart"
        );
        assert_ne!(
            executed[1]["thread"], executed[index]["thread"],
            "separate top-level group threads must not reuse another request's provider context"
        );
    }
    let mut observed = relations.lock().unwrap().clone();
    observed.sort();
    // Personal DM replies intentionally stay flat; group replies retain the
    // exact thread root. The exact provider input sequence checks DM routing.
    let mut expected: Vec<_> = expected_replies
        .iter()
        .map(|(room, event)| {
            (
                room.clone(),
                if room == dm {
                    String::new()
                } else {
                    event.clone()
                },
            )
        })
        .collect();
    expected.sort();
    assert_eq!(
        observed, expected,
        "every decrypted owner-visible reply must target the exact accepted request, never a suppressed message"
    );
    let member_expected: Vec<_> = expected_replies
        .iter()
        .filter(|(room, _)| room == group)
        .cloned()
        .collect();
    for _ in 0..50 {
        if member_expected
            .iter()
            .all(|item| member_replies.lock().unwrap().contains(item))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        member_expected
            .iter()
            .all(|item| member_replies.lock().unwrap().contains(item)),
        "second human must decrypt replies to their actual group requests"
    );
    let expected_inputs = [
        "DAEMON_DM_AFTER_BOTH_CONTROLLERS_DETACH",
        "DAEMON_GROUP_MENTION_AFTER_DETACH",
        "SECOND_HUMAN_EXPLICIT_MENTION",
        "DAEMON_DM_AFTER_PROCESS_RESTART",
        "SECOND_HUMAN_MENTION_AFTER_RESTART",
    ];
    for (recorded, expected) in turns(&marker).iter().zip(expected_inputs) {
        let inputs = recorded["input"].as_array().expect("provider inputs");
        assert!(
            inputs.iter().any(|input| input["text"] == expected),
            "exact accepted text must reach provider: {recorded}"
        );
    }
    let current = http(
        base,
        human_token,
        reqwest::Method::POST,
        "/_matrix/client/v3/keys/query",
        Some(json!({"device_keys":{puppet:[]}})),
    )
    .await;
    assert_eq!(
        current["device_keys"][puppet], original_keys,
        "Agent crypto identity survives daemon process restart"
    );
    for (room, event) in replies.lock().unwrap().clone() {
        let wire = http(
            base,
            human_token,
            reqwest::Method::GET,
            &format!(
                "/_matrix/client/v3/rooms/{}/event/{}",
                segment(&room),
                segment(&event)
            ),
            None,
        )
        .await;
        assert_eq!(wire["type"], "m.room.encrypted");
        assert!(wire["content"]["body"].is_null());
    }
    daemon.stop().await;
    sync.abort();
    member_sync.abort();
    let _ = sync.await;
    let _ = member_sync.await;
    println!(
        "PASS: actual daemon process, two controller detaches, real encrypted DM/group, no-mention suppression, crypto identity restart and two real humans and exactly five offline model turns"
    );
    hagency_store::private::write_new(
        &fixture_path.parent().unwrap().join("client-done.json"),
        b"{\"passed\":true}",
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "private subprocess entry; never opens installed legacy authority"]
async fn isolated_owner_daemon_process() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    hagency_platform::legacy_writer::test_support::initialize_process_root().unwrap();
    let home =
        PathBuf::from(std::env::var_os("HAGENCY_DAEMON_CRYPTO_HOME").expect("test-owned home"));
    let cancel = hagency_matrix::CancellationToken::new();
    let signal = cancel.clone();
    tokio::spawn(async move {
        let mut termination =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        termination.recv().await;
        signal.cancel();
    });
    host::run(&home, cancel).await.unwrap();
}
