//! Board #89: the startup exchange the REAL Codex app-server returns.
//!
//! The offline fakes replay a hand-written subset; the live app-server sends
//! more than they do, and native's strict notification allow-lists turned that
//! into a startup kill. These frames were captured from a local
//! `codex app-server` (0.157.0) driven with exactly the initialize +
//! `thread/start` params native sends (`Settings::thread_request`), with no
//! model turn executed and no network beyond the local process.
use super::*;

/// Captured `initialize` result (codex 0.157.0).
fn real_initialize_result() -> Value {
    json!({
        "userAgent": "hagency/0.167.0 (Mac OS 26.5.0; arm64) Apple_Terminal/470.2 (hagency; 0.1.0)",
        "codexHome": "/fixture/.codex",
        "platformFamily": "unix",
        "platformOs": "macos"
    })
}

/// Captured global notices the app-server emits between the initialize result
/// and the `initialized` acknowledgement — in this order.
fn real_startup_notices() -> Vec<Value> {
    vec![
        json!({"method":"remoteControl/status/changed","params":{
            "status":"disabled","serverName":"Mac",
            "installationId":"2cb8a48d-db71-4949-8dd9-a63f586cbaca","environmentId":null},
            "emittedAtMs":1790389103756i64}),
        json!({"method":"account/updated","params":{"authMode":"chatgpt","planType":"pro"},
            "emittedAtMs":1790389104416i64}),
    ]
}

/// Captured `thread/start` result (codex 0.157.0), trimmed to the fields native
/// reads plus the extra ones the real server adds.
fn real_thread_result(id: &str) -> Value {
    let cwd = cwd();
    json!({
        "thread": {
            "id": id,
            "environments": [{"environmentId":"local","cwd":cwd,
                "runtimeWorkspaceRoots":[cwd]}],
            "extra": null,
            "sessionId": id,
            "forkedFromId": null,
            "parentThreadId": null,
            "preview": "",
            "ephemeral": true,
            "section": null,
            "sectionEnteredAt": null,
            "projectId": null,
            "historyMode": "legacy",
            "modelProvider": "openai",
            "model": "fixture-model",
            "reasoningEffort": "xhigh",
            "createdAt": 1790389163,
            "updatedAt": 1790389163,
            "recencyAt": 1790389163,
            "status": {"type":"idle"},
            "path": null,
            "cwd": cwd,
            "cliVersion": "0.157.0",
            "originator": "hagency",
            "source": "vscode",
            "canAcceptDirectInput": true,
            "threadSource": null,
            "agentNickname": null,
            "agentRole": null,
            "gitInfo": null,
            "name": null,
            "daybreakEnabled": null,
            "turns": []
        },
        "model": "fixture-model",
        "modelProvider": "openai",
        "serviceTier": null,
        "disabledPluginIds": [],
        "cwd": cwd,
        "runtimeWorkspaceRoots": [cwd],
        "instructionSources": [],
        "approvalPolicy": "on-request",
        "approvalsReviewer": "user",
        "sandbox": {"type":"workspaceWrite","writableRoots":[],
            "networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false},
        "activePermissionProfile": null,
        "reasoningEffort": "xhigh",
        "multiAgentMode": "explicitRequestOnly"
    })
}

/// Captured notices the app-server emits after the `thread/start` result.
fn real_thread_notices(id: &str) -> Vec<Value> {
    vec![
        json!({"method":"thread/started","params":{"thread":{"id":id,
            "environments":[{"environmentId":"local","cwd":cwd(),
                "runtimeWorkspaceRoots":[cwd()]}],
            "ephemeral":true,"model":"fixture-model","modelProvider":"openai",
            "reasoningEffort":"xhigh","status":{"type":"idle"},"turns":[]}}}),
        json!({"method":"mcpServer/startupStatus/updated","params":{"threadId":id,
            "name":"codex_apps","status":"starting","error":null,"failureReason":null},
            "emittedAtMs":1790389106689i64}),
        json!({"method":"mcpServer/startupStatus/updated","params":{"threadId":id,
            "name":"codex_apps","status":"ready","error":null,"failureReason":null},
            "emittedAtMs":1790389106690i64}),
    ]
}

/// The whole captured startup exchange must reach `Ready` + a thread id; the
/// live rig never got past `thread_start`.
#[tokio::test]
async fn native_codex_session_real_app_server_startup_exchange() {
    let thread = "01a0db82-942d-7162-8461-b7d028c3a0b8";
    let (mut session, mut peer) = fixture(false);
    let (result, ()) = tokio::join!(session.initialize(), async {
        let request = read(&mut peer.stdin).await;
        assert_eq!(request["method"], "initialize");
        // The real server answers, then emits its global notices, THEN reads
        // our `initialized` acknowledgement.
        write(
            &mut peer,
            json!({"id": request["id"], "result": real_initialize_result()}),
        )
        .await;
        for notice in real_startup_notices() {
            write(&mut peer, notice).await;
        }
        assert_eq!(read(&mut peer.stdin).await["method"], "initialized");
    });
    if let Err(error) = result {
        panic!("the real initialize exchange must reach Ready: {error:?}");
    }
    assert_eq!(session.phase(), Phase::Ready);

    let (result, ()) = tokio::join!(session.start_thread(), async {
        let request = read(&mut peer.stdin).await;
        assert_eq!(request["method"], "thread/start");
        write(
            &mut peer,
            json!({"id": request["id"], "result": real_thread_result(thread)}),
        )
        .await;
        for notice in real_thread_notices(thread) {
            write(&mut peer, notice).await;
        }
    });
    let opened = match result {
        Ok(id) => id,
        Err(error) => panic!("the real thread/start exchange must open the thread: {error:?}"),
    };
    assert_eq!(opened, thread);
    assert_eq!(session.phase(), Phase::ThreadReady);
}

/// Board #105: Codex withholds the `thread/start` response until every REQUIRED
/// MCP server in the thread config has completed its handshake, and bounds that
/// on its own side by `startup_timeout_sec` (`task_mcp.rs:51`, 5 s). Native's
/// generic per-request bound is the product `response_ms` ceiling (2 s,
/// `hagency-execution/src/host.rs:58`) — UNDER Codex's own ceiling. A cold
/// helper that loses that race was reported as `Transport(Timeout)` at
/// `stage="thread_start"`, settling the attempt `outcome_unknown` (the live
/// regression). A startup RPC takes the startup floor instead, so a
/// slow-but-valid handshake still opens the thread; past Codex's own ceiling we
/// receive Codex's refusal, never a timeout.
///
/// Fails before the fix: the 300 ms generic bound fires while the handshake is
/// still in flight, so `start_thread()` returns `Transport(Timeout)`.
#[tokio::test]
async fn native_codex_session_thread_start_waits_out_a_slow_required_mcp_handshake() {
    use hagency_runtime::codex::session::TaskMcp;
    let helper = TaskMcp::new(
        std::env::temp_dir().join("native-task-helper"),
        "original_task".into(),
        None,
    )
    .unwrap();
    let (stdin, peer_in) = tokio::io::duplex(131072);
    let (stdout, peer_out) = tokio::io::duplex(131072);
    let (stderr, peer_err) = tokio::io::duplex(1024);
    // 300 ms is the generic per-request bound; the required-MCP handshake this
    // test simulates takes ~600 ms — longer than the bound, under the floor.
    let mut session = Session::new(
        stdout,
        stdin,
        stderr,
        settings(false).with_task_mcp(helper),
        Limits {
            write_timeout_ms: 500,
            event_wait_ms: 1000,
            lifetime_ms: 30_000,
        },
        300,
    )
    .unwrap();
    let mut peer = Peer {
        stdin: peer_in,
        stdout: peer_out,
        _stderr: peer_err,
    };
    initialize(&mut session, &mut peer).await;

    let (result, ()) = tokio::join!(session.start_thread(), async {
        let request = read(&mut peer.stdin).await;
        assert_eq!(request["method"], "thread/start");
        // The response is withheld while the required MCP server handshakes.
        sleep(Duration::from_millis(600)).await;
        // Best effort: before the fix the session has already timed out and
        // closed our pipe, so delivering the answer is impossible. Swallowing
        // that lets the assertion below name the real fault (the startup
        // timeout) instead of a derived `BrokenPipe`.
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            let mut bytes =
                serde_json::to_vec(&json!({"id": request["id"], "result": thread_result(false)}))
                    .unwrap();
            bytes.push(b'\n');
            peer.stdout.write_all(&bytes).await
        })
        .await;
    });
    assert_eq!(
        result.unwrap_or_else(|error| panic!(
            "a valid slow required-MCP handshake must not fail thread/start: {error:?}"
        )),
        "thread-one"
    );
    assert_eq!(session.phase(), Phase::ThreadReady);
}
