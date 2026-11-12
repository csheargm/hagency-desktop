use super::*;
use hagency_runtime::{
    room_tools::{RoomTools, RoomOperation, RoomFileOperation},
    claude::session::PreparedUpdate,
};

fn owner() -> Value {
    json!({"name":"owner-plugin","status":"connected","scope":"dynamic","source":"dynamic","transport":"stdio"})
}
fn server(tools: RoomTools, connected: bool) -> Value {
    json!({"name":"hagency_matrix","status":if connected { "connected" } else { "pending" },
        "transport":"sdk","scope":"dynamic","source":"sdk",
        "tools":tools.definitions().iter().map(|t| json!({"name":t["name"]})).collect::<Vec<_>>()})
}
fn ack(id: &Value, body: Value) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":body}})
}
fn mcp(id: &str, message: Value) -> Value {
    json!({"type":"control_request","request_id":id,"request":{"subtype":"mcp_message",
        "server_name":"hagency_matrix","message":message}})
}
fn call(id: &str, name: &str, args: Value) -> Value {
    mcp(
        id,
        json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{
        "name":name,"arguments":args,"_meta":{"claudecode/toolUseId":format!("tool-{id}"),"progressToken":2}}}),
    )
}
async fn emit(peer: &mut Peer, value: Value) {
    peer.output.write_all(&encoded(value)).await.unwrap();
}
async fn bind_peer(peer: &mut Peer, tools: RoomTools) {
    bind_peer_with_task(peer, tools, false).await;
}
async fn bind_peer_with_task(peer: &mut Peer, tools: RoomTools, with_task: bool) {
    let before = read(peer).await;
    assert_eq!(before["request"]["subtype"], "mcp_status");
    emit(
        peer,
        ack(
            &before["request_id"],
            json!({"mcpServers": if with_task {vec![owner(),task_server()]} else {vec![owner()]}}),
        ),
    )
    .await;
    let bind = read(peer).await;
    assert_eq!(
        bind["request"]["servers"].as_object().unwrap().len(),
        if with_task { 2 } else { 1 }
    );
    assert_eq!(
        bind["request"]["servers"]["hagency_matrix"],
        json!({"type":"sdk","name":"hagency_matrix","alwaysLoad":true})
    );
    if with_task {
        assert_eq!(
            bind["request"]["servers"]["hagency_task_writer"]["type"],
            "stdio"
        );
    }
    // Metadata callbacks can precede the native control acknowledgement.
    emit(peer, mcp("mcp-init",json!({"jsonrpc":"2.0","id":0,"method":"initialize",
        "params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}))).await;
    let reply = read(peer).await;
    assert_eq!(reply["response"]["response"]["mcp_response"]["id"], 0);
    emit(
        peer,
        mcp(
            "mcp-ready",
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        ),
    )
    .await;
    assert!(
        read(peer).await["response"]["response"]["mcp_response"]
            .get("id")
            .is_none()
    );
    emit(
        peer,
        mcp(
            "mcp-list",
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
        ),
    )
    .await;
    let reply = read(peer).await;
    assert_eq!(
        reply["response"]["response"]["mcp_response"]["result"]["tools"],
        json!(tools.definitions())
    );
    emit(
        peer,
        ack(
            &bind["request_id"],
            json!({"added":["hagency_matrix"],"removed":[],"errors":{}}),
        ),
    )
    .await;
    for connected in [false, true] {
        let after = read(peer).await;
        assert_eq!(after["request"]["subtype"], "mcp_status");
        emit(
            peer,
            ack(
                &after["request_id"],
                json!({"mcpServers": if with_task {vec![owner(),task_server(),server(tools,connected)]} else {vec![owner(),server(tools,connected)]}}),
            ),
        )
        .await;
    }
}
async fn bound(continuous: bool, tools: RoomTools) -> (Driver, Peer) {
    let (mut driver, mut peer) = pair(32768, limits());
    if continuous {
        driver.enable_native_completion().unwrap();
        driver.enable_continuous_session().unwrap();
    }
    initialize(&mut driver, &mut peer).await;
    let (r, ()) = tokio::join!(driver.bind_room_tools(tools), bind_peer(&mut peer, tools));
    r.unwrap();
    assert_eq!(driver.phase(), Phase::Ready);
    (driver, peer)
}
#[tokio::test]
async fn claude_room_mcp_binding_passes_reports_of_the_accounts_own_hooks() {
    // A hook that outlasts the CLI's start-up reports while the tools are
    // bound. The reports are metadata and do not fail the binding.
    let tools = RoomTools::history();
    let (mut driver, mut peer) = pair(32768, limits());
    initialize(&mut driver, &mut peer).await;
    for report in native_hook_reports() {
        emit(&mut peer, report).await;
    }
    let (bound, ()) = tokio::join!(driver.bind_room_tools(tools), bind_peer(&mut peer, tools));
    bound.unwrap();
    assert_eq!(driver.phase(), Phase::Ready);

    // Any other frame in that exchange still fails it.
    let (mut driver, mut peer) = pair(32768, limits());
    initialize(&mut driver, &mut peer).await;
    emit(&mut peer, init()).await;
    let (bound, _) = tokio::join!(driver.bind_room_tools(tools), read(&mut peer));
    assert!(bound.is_err());
}
async fn active() -> (Driver, Peer) {
    let (mut driver, mut peer) = bound(false, RoomTools::history_and_files()).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    driver.enable_room_tool_callbacks().unwrap();
    (driver, peer)
}

#[tokio::test]
async fn claude_room_mcp_interrupt_withdraws_waiting_and_prepared_replies() {
    use hagency_runtime::claude::session::InterruptUpdate;
    for prepare_first in [false, true] {
        let (mut driver, mut peer) = active().await;
        event(&mut driver, &mut peer, call("history", "read_room_history", json!({"cursor":null}))).await.unwrap();
        let request = driver.room_tool_request("history").unwrap();
        let reply = if prepare_first {
            Some(driver.prepare_room_tool_reply(&request, "old history", false).unwrap())
        } else { None };
        let mut interrupt = driver.prepare_interrupt().unwrap();
        let (sent, frame) = tokio::join!(driver.send_prepared_interrupt(&mut interrupt), read(&mut peer));
        assert!(matches!(sent.unwrap(), InterruptUpdate::WriteAccepted(_)));
        assert_eq!(frame["request"]["subtype"], "interrupt");
        match reply {
            Some(mut reply) => assert!(matches!(driver.send_prepared_room_reply(&mut reply).await, Err(Error::RoomToolUnavailable))),
            None => assert!(matches!(driver.prepare_room_tool_reply(&request, "late history", false), Err(Error::RoomToolUnavailable))),
        }
        assert_eq!(driver.phase(), Phase::Closed);
        assert_eq!(read_eof(&mut peer).await, 0);
    }
}
async fn answer(driver: &mut Driver, peer: &mut Peer, id: &str) -> Value {
    let request = driver.room_tool_request(id).unwrap();
    assert_eq!(request.session_id(), "session-one");
    let mut reply = driver
        .prepare_room_tool_reply(&request, "{\"fixture\":true}", false)
        .unwrap();
    let (sent, value) = tokio::join!(driver.send_prepared_room_reply(&mut reply), read(peer));
    assert!(matches!(sent, Ok(PreparedUpdate::WriteAccepted(_))));
    assert_eq!(value["response"]["request_id"], id);
    assert_eq!(value["response"]["response"]["mcp_response"]["id"], id);
    assert_eq!(
        value["response"]["response"]["mcp_response"]["result"]["content"][0]["text"],
        "{\"fixture\":true}"
    );
    value
}

#[tokio::test]
async fn claude_room_mcp_uses_common_operations_and_keeps_owner_configuration() {
    let (mut driver, mut peer) = active().await;
    for n in 0..40 {
        let id = format!("history-{n}");
        event(
            &mut driver,
            &mut peer,
            call(&id, "read_room_history", json!({"cursor":null})),
        )
        .await
        .unwrap();
        let request = driver.room_tool_request(&id).unwrap();
        assert_eq!(request.tool_use_id(), Some(format!("tool-{id}").as_str()));
        assert!(matches!(
            request.operation(),
            Some(RoomOperation::History { cursor: None })
        ));
        answer(&mut driver, &mut peer, &id).await;
        assert!(driver.room_tool_request(&id).is_err());
    }
    event(
        &mut driver,
        &mut peer,
        call(
            "file",
            "send_file",
            json!({"call_id":"delivery-1","path":"report.pdf"}),
        ),
    )
    .await
    .unwrap();
    assert!(
        matches!(driver.room_tool_request("file").unwrap().operation(),
        Some(RoomOperation::File(RoomFileOperation::SendFile{path,..})) if path=="report.pdf")
    );
    answer(&mut driver, &mut peer, "file").await;
    assert!(matches!(
        event(
            &mut driver,
            &mut peer,
            call(
                "file",
                "send_file",
                json!({"call_id":"delivery-1","path":"report.pdf"})
            )
        )
        .await,
        Err(Error::Identity)
    ));
}

#[tokio::test]
async fn claude_room_mcp_rejects_wrong_room_method_and_unbound_native_identity() {
    let mut malformed = vec![
        call(
            "bad",
            "read_room_history",
            json!({"cursor":null,"room_id":"!foreign:example.test"}),
        ),
        call("bad", "Bash", json!({"command":"forbidden"})),
        call(
            "bad",
            "receive_file",
            json!({"event_id":"https://private.example.test"}),
        ),
    ];
    let mut foreign = call("bad", "read_room_history", json!({"cursor":null}));
    foreign["request"]["server_name"] = "owner-plugin".into();
    malformed.push(foreign);
    let mut unidentified = call("bad", "read_room_history", json!({"cursor":null}));
    unidentified["request"]["message"]["params"]["_meta"] = json!({});
    malformed.push(unidentified);
    for frame in malformed {
        let (mut driver, mut peer) = active().await;
        assert!(event(&mut driver, &mut peer, frame).await.is_err());
        assert_eq!(driver.phase(), Phase::Closed);
    }
    let (mut driver, mut peer) = bound(false, RoomTools::history()).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    assert!(matches!(
        event(
            &mut driver,
            &mut peer,
            call("inactive", "read_room_history", json!({"cursor":null}))
        )
        .await,
        Err(Error::State)
    ));
}

#[tokio::test(start_paused = true)]
async fn claude_room_mcp_preserves_deadline_cancellation_and_original_connection() {
    let (mut driver, mut peer) = active().await;
    event(
        &mut driver,
        &mut peer,
        call("cancelled", "read_room_history", json!({"cursor":null})),
    )
    .await
    .unwrap();
    let request = driver.room_tool_request("cancelled").unwrap();
    assert!(
        matches!(driver.supervise_turn(), Err(Error::State)),
        "supervision cannot be attached after a room callback"
    );
    let mut prepared = driver
        .prepare_room_tool_reply(&request, "result", false)
        .unwrap();
    event(
        &mut driver,
        &mut peer,
        json!({"type":"control_cancel_request","request_id":"cancelled"}),
    )
    .await
    .unwrap();
    assert!(matches!(
        driver.send_prepared_room_reply(&mut prepared).await,
        Err(Error::RoomToolUnavailable)
    ));
    assert_eq!(read_eof(&mut peer).await, 0);

    let (mut driver, mut peer) = active().await;
    event(
        &mut driver,
        &mut peer,
        call("old", "read_room_history", json!({"cursor":null})),
    )
    .await
    .unwrap();
    let request = driver.room_tool_request("old").unwrap();
    tokio::time::advance(Duration::from_millis(1001)).await;
    assert!(matches!(
        driver.prepare_room_tool_reply(&request, "late", false),
        Err(Error::Timeout)
    ));

    let (mut other, mut other_peer) = active().await;
    event(
        &mut other,
        &mut other_peer,
        call("old", "read_room_history", json!({"cursor":null})),
    )
    .await
    .unwrap();
    assert!(matches!(
        other.prepare_room_tool_reply(&request, "foreign", false),
        Err(Error::Identity)
    ));
    assert_eq!(read_eof(&mut other_peer).await, 0);
}
async fn read_eof(peer: &mut Peer) -> usize {
    let mut line = String::new();
    peer.input.read_line(&mut line).await.unwrap()
}

#[tokio::test]
async fn claude_room_mcp_metadata_cannot_be_used_as_tool_results_or_approvals() {
    let (mut driver, mut peer) = active().await;
    event(
        &mut driver,
        &mut peer,
        mcp(
            "list",
            json!({"jsonrpc":"2.0","id":8,"method":"tools/list","params":{}}),
        ),
    )
    .await
    .unwrap();
    let request = driver.room_tool_request("list").unwrap();
    assert!(request.operation().is_none());
    let mut reply = driver.prepare_room_metadata_reply(&request).unwrap();
    let (sent, value) = tokio::join!(driver.send_prepared_room_reply(&mut reply), read(&mut peer));
    assert!(matches!(sent, Ok(PreparedUpdate::WriteAccepted(_))));
    assert_eq!(
        value["response"]["response"]["mcp_response"]["result"]["tools"],
        json!(RoomTools::history_and_files().definitions())
    );
    event(
        &mut driver,
        &mut peer,
        call("tool", "read_room_history", json!({"cursor":null})),
    )
    .await
    .unwrap();
    assert!(driver.question("tool").is_err());
    let request = driver.room_tool_request("tool").unwrap();
    assert!(matches!(
        driver.prepare_room_metadata_reply(&request),
        Err(Error::State)
    ));
}

#[tokio::test(start_paused = true)]
async fn claude_room_mcp_continuation_keeps_definitions_and_requires_fresh_attachment() {
    for attach in [false, true] {
        let tools = RoomTools::history();
        let (mut driver, mut peer) = bound(true, tools).await;
        let first = continuous::next_prompt(&mut driver, &mut peer).await;
        driver.enable_room_tool_callbacks().unwrap();
        event(
            &mut driver,
            &mut peer,
            call("first", "read_room_history", json!({"cursor":null})),
        )
        .await
        .unwrap();
        let old = driver.room_tool_request("first").unwrap();
        answer(&mut driver, &mut peer, "first").await;
        continuous::completed(&mut driver, &mut peer, &first).await;
        assert!(driver.can_followup());
        let (next, ()) = tokio::join!(driver.prepare_followup(limits()), async {
            let probe = read(&mut peer).await;
            emit(
                &mut peer,
                ack(
                    &probe["request_id"],
                    json!({"mcpServers":[owner(),server(tools,true)]}),
                ),
            )
            .await;
        });
        next.unwrap();
        let second = continuous::next_prompt(&mut driver, &mut peer).await;
        assert_ne!(first["uuid"], second["uuid"]);
        if attach {
            driver.enable_room_tool_callbacks().unwrap();
        }
        let invoked = event(
            &mut driver,
            &mut peer,
            call("first", "read_room_history", json!({"cursor":null})),
        )
        .await;
        if attach {
            invoked.unwrap();
            assert!(matches!(
                driver.prepare_room_tool_reply(&old, "old capability", false),
                Err(Error::Identity)
            ));
        } else {
            assert!(matches!(invoked, Err(Error::State)));
        }
    }
}

#[tokio::test]
async fn claude_room_mcp_refuses_collision_before_changing_any_server() {
    for dynamic in [false, true] {
        let (mut driver, mut peer) = pair(32768, limits());
        initialize(&mut driver, &mut peer).await;
        let (bound, ()) = tokio::join!(driver.bind_room_tools(RoomTools::history()), async {
            let probe = read(&mut peer).await;
            let name = if dynamic {
                "foreign-dynamic"
            } else {
                "hagency_matrix"
            };
            emit(&mut peer,ack(&probe["request_id"],json!({"mcpServers":[{"name":name,"scope":if dynamic {"dynamic"} else {"project"},"transport":if dynamic {"sdk"} else {"stdio"}}]}))).await;
        });
        assert!(matches!(bound, Err(Error::RoomToolStartup)));
        assert_eq!(read_eof(&mut peer).await, 0);
    }
}

#[tokio::test]
async fn claude_room_mcp_callbacks_during_approval_writes_keep_separate_replies() {
    use hagency_runtime::claude::session::{ApprovalControlPolicy, PermissionDecision};
    let (mut driver, mut peer) = active().await;
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 1000,
            response_reserve_ms: 1000,
        })
        .unwrap();
    event(&mut driver, &mut peer, permission("owner-approval"))
        .await
        .unwrap();
    let mut approval = driver
        .prepare_approval("owner-approval", PermissionDecision::Deny)
        .unwrap();
    emit(
        &mut peer,
        call("during-write", "read_room_history", json!({"cursor":null})),
    )
    .await;
    assert!(
        matches!(driver.send_prepared_approval(&mut approval).await.unwrap(),
        PreparedUpdate::Message(Message::Mcp { request_id, .. }) if request_id == "during-write")
    );
    let (written, reply) = tokio::join!(
        driver.send_prepared_approval(&mut approval),
        read(&mut peer)
    );
    assert!(matches!(written, Ok(PreparedUpdate::WriteAccepted(_))));
    assert_eq!(reply["response"]["response"]["behavior"], "deny");
    assert!(driver.question("during-write").is_err());
    answer(&mut driver, &mut peer, "during-write").await;
}

#[tokio::test(start_paused = true)]
async fn claude_room_mcp_pending_startup_cannot_extend_the_original_deadline() {
    let (mut driver, mut peer) = pair(32768, limits());
    initialize(&mut driver, &mut peer).await;
    let started = tokio::time::Instant::now();
    let (bound, ()) = tokio::join!(driver.bind_room_tools(RoomTools::history()), async {
        let before = read(&mut peer).await;
        emit(
            &mut peer,
            ack(&before["request_id"], json!({"mcpServers":[]})),
        )
        .await;
        let bind = read(&mut peer).await;
        emit(
            &mut peer,
            ack(
                &bind["request_id"],
                json!({"added":["hagency_matrix"],"removed":[],"errors":{}}),
            ),
        )
        .await;
        loop {
            let mut line = String::new();
            if peer.input.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            let status: Value = serde_json::from_str(&line).unwrap();
            emit(
                &mut peer,
                ack(
                    &status["request_id"],
                    json!({"mcpServers":[server(RoomTools::history(), false)]}),
                ),
            )
            .await;
        }
    });
    assert!(matches!(bound, Err(Error::Timeout)));
    assert_eq!(started.elapsed(), Duration::from_secs(1));
}

fn task_server() -> Value {
    json!({"name":"hagency_task_writer","status":"connected","transport":"stdio","scope":"dynamic",
        "tools":hagency_runtime::task_mcp::owned_task_tools(false,false).iter().map(|name| json!({"name":name})).collect::<Vec<_>>()})
}
async fn bind_task_peer(peer: &mut Peer, with_room: bool) {
    let before = read(peer).await;
    let mut servers = vec![owner()];
    if with_room {
        servers.push(server(RoomTools::history(), true));
    }
    emit(
        peer,
        ack(&before["request_id"], json!({"mcpServers":servers})),
    )
    .await;
    let bind = read(peer).await;
    assert_eq!(
        bind["request"]["servers"].as_object().unwrap().len(),
        if with_room { 2 } else { 1 }
    );
    assert_eq!(
        bind["request"]["servers"]["hagency_task_writer"]["type"],
        "stdio"
    );
    if with_room {
        assert_eq!(bind["request"]["servers"]["hagency_matrix"]["type"], "sdk");
    }
    emit(
        peer,
        ack(
            &bind["request_id"],
            json!({"added":["hagency_task_writer"],"removed":[],"errors":{}}),
        ),
    )
    .await;
    let after = read(peer).await;
    servers.push(task_server());
    emit(
        peer,
        ack(&after["request_id"], json!({"mcpServers":servers})),
    )
    .await;
}
#[tokio::test]
async fn claude_room_mcp_and_task_helper_preserve_both_dynamic_servers_in_either_order() {
    for room_first in [false, true] {
        let (mut driver, mut peer) = pair(32768, limits());
        initialize(&mut driver, &mut peer).await;
        if room_first {
            let (bound, ()) = tokio::join!(
                driver.bind_room_tools(RoomTools::history()),
                bind_peer(&mut peer, RoomTools::history())
            );
            bound.unwrap();
        }
        let helper = hagency_runtime::claude::TaskMcp::new(
            std::env::temp_dir().join("task-helper"),
            "task-fixture".into(),
        )
        .unwrap();
        let (bound, ()) = tokio::join!(
            driver.bind_task_mcp(helper),
            bind_task_peer(&mut peer, room_first)
        );
        bound.unwrap();
        if !room_first {
            let (bound, ()) = tokio::join!(
                driver.bind_room_tools(RoomTools::history()),
                bind_peer_with_task(&mut peer, RoomTools::history(), true)
            );
            bound.unwrap();
        }
        assert_eq!(driver.phase(), Phase::Ready);
        let (sent, prompt) = tokio::join!(
            driver.prompt("Read this room for the task."),
            read(&mut peer)
        );
        sent.unwrap();
        assert!(
            prompt["message"]["content"]
                .as_str()
                .unwrap()
                .contains("task-fixture")
        );
        event(&mut driver, &mut peer, init()).await.unwrap();
        driver.enable_room_tool_callbacks().unwrap();
        event(
            &mut driver,
            &mut peer,
            call("composed", "read_room_history", json!({"cursor":null})),
        )
        .await
        .unwrap();
        answer(&mut driver, &mut peer, "composed").await;
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_EXECUTABLE; local Anthropic SSE only, no paid model"]
async fn installed_claude_room_tools_roundtrip_through_native_sdk_mcp() {
    use hagency_runtime::claude::session::{ApprovalControlPolicy, PermissionDecision};
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture.py");
    let tool = "mcp__hagency_matrix__read_room_history";
    std::fs::write(
        &fixture,
        include_str!("../claude_question_fixture.py")
            .replace("AskUserQuestion", tool)
            .replace(
                "\"input\": {\"questions\": questions}",
                "\"input\": {\"cursor\": None}",
            ),
    )
    .unwrap();
    let mut server = tokio::process::Command::new("python3")
        .arg(&fixture)
        .arg(root.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut address)
        .await
        .unwrap();
    let executable =
        std::env::var_os("HAGENCY_TEST_CLAUDE_EXECUTABLE").expect("installed Claude executable");
    let mut child = tokio::process::Command::new(executable)
        .args(hagency_runtime::claude::interactive_arguments("sonnet", "medium").unwrap())
        .env_clear()
        .env("HOME", root.path())
        .env("CLAUDE_CONFIG_DIR", root.path().join("config"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("ANTHROPIC_API_KEY", "offline-contract")
        .env(
            "ANTHROPIC_BASE_URL",
            format!("http://127.0.0.1:{}", address.trim()),
        )
        .env("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1")
        .env("CLAUDE_CODE_BG_TASKS_REPORT_RUNNING", "1")
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .current_dir(root.path())
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut driver = SessionDriver::new(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
        child.stderr.take().unwrap(),
        Limits {
            write_timeout_ms: 2000,
            event_wait_ms: 20_000,
            lifetime_ms: 60_000,
        },
    )
    .unwrap();
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    driver.initialize().await.unwrap();
    driver
        .bind_room_tools(RoomTools::history_and_files())
        .await
        .unwrap();
    driver
        .prompt("Read this chat's history with its room tool.")
        .await
        .unwrap();
    driver.next_message().await.unwrap();
    driver.enable_room_tool_callbacks().unwrap();
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 20_000,
            response_reserve_ms: 2000,
        })
        .unwrap();
    let mut calls = 0;
    let mut permissions = 0;
    let mut buffered = std::collections::VecDeque::new();
    for _ in 0..256 {
        let message = if let Some(message) = buffered.pop_front() {
            message
        } else {
            driver.next_message().await.unwrap()
        };
        match message {
            Message::Permission {
                request_id,
                tool_name,
                tool_use_id,
                input,
            } => {
                assert_eq!(tool_name, tool);
                assert_eq!(tool_use_id.as_deref(), Some("question-native"));
                assert_eq!(input, json!({"cursor":null}));
                permissions += 1;
                let mut reply = driver
                    .prepare_approval(&request_id, PermissionDecision::Allow)
                    .unwrap();
                while let PreparedUpdate::Message(message) =
                    driver.send_prepared_approval(&mut reply).await.unwrap()
                {
                    buffered.push_back(message);
                }
            }
            Message::Mcp { request_id, .. } => {
                let request = driver.room_tool_request(&request_id).unwrap();
                let mut reply = if request.operation().is_some() {
                    assert_eq!(request.tool_use_id(), Some("question-native"));
                    assert!(matches!(
                        request.operation(),
                        Some(RoomOperation::History { cursor: None })
                    ));
                    calls += 1;
                    driver
                        .prepare_room_tool_reply(
                            &request,
                            "{\"messages\":[\"MATRIX_HISTORY_FIXTURE\"]}",
                            false,
                        )
                        .unwrap()
                } else {
                    driver.prepare_room_metadata_reply(&request).unwrap()
                };
                while let PreparedUpdate::Message(message) =
                    driver.send_prepared_room_reply(&mut reply).await.unwrap()
                {
                    buffered.push_back(message);
                }
            }
            _ => {}
        }
        if buffered.is_empty() && driver.phase() == Phase::ResultObserved {
            break;
        }
    }
    assert_eq!((calls, permissions), (1, 1));
    assert_eq!(driver.phase(), Phase::ResultObserved);
    let answer = std::fs::read_to_string(root.path().join("native-answer.json")).unwrap();
    assert!(answer.contains("MATRIX_HISTORY_FIXTURE"));
    assert!(driver.can_followup());
    driver
        .prepare_followup(Limits {
            write_timeout_ms: 2000,
            event_wait_ms: 20_000,
            lifetime_ms: 60_000,
        })
        .await
        .unwrap();
    driver.prompt("/usage").await.unwrap();
    driver.next_message().await.unwrap();
    driver.enable_room_tool_callbacks().unwrap();
    for _ in 0..64 {
        driver.next_message().await.unwrap();
        if driver.phase() == Phase::ResultObserved {
            break;
        }
    }
    assert!(driver.can_followup());
    driver.close();
    child.kill().await.unwrap();
    server.kill().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_EXECUTABLE; local metadata-only MCP fixtures"]
async fn installed_claude_room_and_task_mcp_preserve_native_owner_servers() {
    use std::os::unix::fs::PermissionsExt;
    for room_first in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let helper = root.path().join("metadata.py");
        let tools =
            serde_json::to_string(&hagency_runtime::task_mcp::owned_task_tools(false, false))
                .unwrap();
        std::fs::write(
            &helper,
            include_str!("../claude_mcp_metadata_fixture.py")
                .replace("[]  # HAGENCY_FIXTURE_TOOLS", &tools),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let owner_config =
            json!({"mcpServers":{"owner-fixture":{"type":"stdio","command":helper,"args":[]}}});
        let executable = std::env::var_os("HAGENCY_TEST_CLAUDE_EXECUTABLE")
            .expect("installed Claude executable");
        let mut child = tokio::process::Command::new(executable)
            .args(hagency_runtime::claude::interactive_arguments("sonnet", "medium").unwrap())
            .arg("--mcp-config")
            .arg(owner_config.to_string())
            .env_clear()
            .env("HOME", root.path())
            .env("CLAUDE_CONFIG_DIR", root.path().join("config"))
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("ANTHROPIC_API_KEY", "offline-contract")
            .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .current_dir(root.path())
            .kill_on_drop(true)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut driver = SessionDriver::new(
            child.stdout.take().unwrap(),
            child.stdin.take().unwrap(),
            child.stderr.take().unwrap(),
            Limits {
                write_timeout_ms: 2000,
                event_wait_ms: 20_000,
                lifetime_ms: 60_000,
            },
        )
        .unwrap();
        driver.initialize().await.unwrap();
        if room_first {
            driver.bind_room_tools(RoomTools::history()).await.unwrap();
        }
        driver
            .bind_task_mcp(
                hagency_runtime::claude::TaskMcp::new(helper, "fixture-task".into()).unwrap(),
            )
            .await
            .unwrap();
        if !room_first {
            driver.bind_room_tools(RoomTools::history()).await.unwrap();
        }
        assert_eq!(driver.phase(), Phase::Ready);
        driver.close();
        child.kill().await.unwrap();
    }
}
