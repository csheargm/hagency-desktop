use hagency_runtime::claude::{
    EventKind, Message,
    session::{Error, Limits, Phase, SessionDriver},
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
#[path = "claude_session/continuous.rs"]
mod continuous;
#[path = "claude_session/room_mcp.rs"]
mod room_mcp;
#[path = "claude_session/interrupt.rs"]
mod interrupt;

struct FlushProbe {
    fail: bool,
}
impl tokio::io::AsyncWrite for FlushProbe {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        if self.fail {
            std::task::Poll::Ready(Err(std::io::Error::other("private provider sentinel")))
        } else {
            std::task::Poll::Pending
        }
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

type Driver = SessionDriver<DuplexStream, DuplexStream, DuplexStream>;
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
    stderr: DuplexStream,
}
fn pair(capacity: usize, limits: Limits) -> (Driver, Peer) {
    let (stdin, input) = tokio::io::duplex(capacity);
    let (stdout, output) = tokio::io::duplex(capacity);
    let (stderr, error) = tokio::io::duplex(capacity);
    (
        Driver::new(stdout, stdin, stderr, limits).unwrap(),
        Peer {
            input: BufReader::new(input),
            output,
            stderr: error,
        },
    )
}
fn limits() -> Limits {
    Limits {
        write_timeout_ms: 500,
        event_wait_ms: 1000,
        lifetime_ms: 30_000,
    }
}
async fn read(peer: &mut Peer) -> Value {
    let mut line = String::new();
    peer.input.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}
fn encoded(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}
async fn initialize(driver: &mut Driver, peer: &mut Peer) {
    let (result, ()) = tokio::join!(driver.initialize(), async {
        let request = read(peer).await;
        assert_eq!(
            request["request"],
            json!({"subtype":"initialize","hooks":null})
        );
        peer.output
            .write_all(&encoded(json!({"type":"control_response","response":{
            "subtype":"success","request_id":request["request_id"],"response":{"claude_code_version":"2.1.321"}}})))
            .await
            .unwrap();
    });
    result.unwrap();
    assert_eq!(driver.phase(), Phase::Ready);
}
fn native_ui_invalidate() -> Value {
    serde_json::from_str(include_str!("fixtures/claude-native-ui-invalidate.json")).unwrap()
}
#[tokio::test]
async fn native_claude_initialize_ignores_captured_ui_frame_without_binding_a_session() {
    let (mut driver, mut peer) = pair(4096, limits());
    let (result, ()) = tokio::join!(driver.initialize(), async {
        let request = read(&mut peer).await;
        let mut bytes = encoded(native_ui_invalidate());
        bytes.extend(encoded(json!({"type":"control_response","response":{
            "subtype":"success","request_id":request["request_id"],
            "response":{"claude_code_version":"2.1.321", "account":{
                "tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","apiProvider":"firstParty"}}
        }})));
        peer.output.write_all(&bytes).await.unwrap();
    });
    result.unwrap();
    assert_eq!(driver.phase(), Phase::Ready);
    assert_eq!(driver.ignored_frames(), 1);
    assert!(driver.session_id().is_none());
    assert!(driver.observation_source().is_err());
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    assert_eq!(driver.session_id(), Some("session-one"));
    assert_eq!(driver.last_observation().unwrap().sequence(), 1);
}
#[tokio::test(start_paused = true)]
async fn native_claude_initialize_ui_metadata_does_not_hide_authority_or_extend_deadlines() {
    for invalid in [
        permission("early"),
        json!({"type":"control_response","response":{"subtype":"success","request_id":"wrong","response":{"claude_code_version":"2.1.321"}}}),
        init(),
    ] {
        let (mut driver, mut peer) = pair(4096, limits());
        let (result, ()) = tokio::join!(driver.initialize(), async {
            read(&mut peer).await;
            let mut bytes = encoded(native_ui_invalidate());
            bytes.extend(encoded(invalid));
            peer.output.write_all(&bytes).await.unwrap();
        });
        assert_eq!(result, Err(Error::Identity));
        assert_eq!(driver.phase(), Phase::Closed);
        assert_eq!(driver.ignored_frames(), 1);
    }
    let (mut driver, mut peer) = pair(64 * 1024, limits());
    let (result, ()) = tokio::join!(driver.initialize(), async {
        read(&mut peer).await;
        let bytes = encoded(native_ui_invalidate()).repeat(64);
        peer.output.write_all(&bytes).await.unwrap();
    });
    assert_eq!(result, Err(Error::Capacity));
    assert_eq!(driver.ignored_frames(), 64);

    let (mut driver, mut peer) = pair(4096, limits());
    let start = tokio::time::Instant::now();
    let ((result, elapsed), ()) = tokio::join!(
        async {
            let result = driver.initialize().await;
            (result, start.elapsed())
        },
        async {
            read(&mut peer).await;
            for _ in 0..3 {
                peer.output
                    .write_all(&encoded(native_ui_invalidate()))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    );
    assert_eq!(result, Err(Error::Timeout));
    assert_eq!(elapsed, Duration::from_millis(limits().event_wait_ms));
}
async fn prompt(driver: &mut Driver, peer: &mut Peer) {
    let (result, value) = tokio::join!(
        driver.prompt("literal $(not a shell) 中文\nnext"),
        read(peer)
    );
    let receipt = result.unwrap();
    assert_eq!(receipt.accepted_bytes, receipt.total_bytes);
    assert!(receipt.flushed);
    assert_eq!(
        value["message"]["content"],
        "literal $(not a shell) 中文\nnext"
    );
    assert_eq!(value["session_id"], "");
    assert!(value["parent_tool_use_id"].is_null());
}
async fn event(driver: &mut Driver, peer: &mut Peer, value: Value) -> Result<Message, Error> {
    let bytes = encoded(value);
    let (result, write) = tokio::join!(driver.next_message(), peer.output.write_all(&bytes));
    write.unwrap();
    result
}
fn init() -> Value {
    json!({"type":"system","subtype":"init","session_id":"session-one"})
}
fn permission(id: &str) -> Value {
    json!({"type":"control_request","request_id":id,
    "request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"private sentinel"}}})
}
async fn running() -> (Driver, Peer) {
    let (mut driver, mut peer) = pair(4096, limits());
    initialize(&mut driver, &mut peer).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    (driver, peer)
}

#[tokio::test]
async fn native_claude_startup_metadata_keeps_init_as_the_bound_observation() {
    for other_session in [false, true] {
        let (mut driver, mut peer) = pair(4096, limits());
        initialize(&mut driver, &mut peer).await;
        prompt(&mut driver, &mut peer).await;
        let mut bytes = encoded(json!({"type":"system","subtype":"session_state_changed",
            "state":"running","session_id":"session-one"}));
        for state in ["queued", "started"] {
            bytes.extend(encoded(
                json!({"type":"command_lifecycle","command_uuid":"command-one",
                "state":state,"session_id":"session-one"}),
            ));
        }
        let mut initialization = init();
        if other_session {
            initialization["session_id"] = json!("different-session");
        }
        bytes.extend(encoded(initialization));
        let (result, sent) = tokio::join!(driver.next_message(), peer.output.write_all(&bytes));
        sent.unwrap();
        if other_session {
            assert!(matches!(result, Err(Error::Identity)));
            assert_eq!(driver.phase(), Phase::Closed);
        } else {
            assert!(matches!(
                result,
                Ok(Message::Event {
                    kind: EventKind::System,
                    ..
                })
            ));
            assert_eq!(driver.last_observation().unwrap().sequence(), 1);
            assert!(driver.observation_source().is_ok());
        }
    }
}

#[tokio::test]
async fn native_claude_startup_metadata_cannot_hide_unbounded_or_unbound_input() {
    for unexpected_assistant in [false, true] {
        let (mut driver, mut peer) = pair(16384, limits());
        initialize(&mut driver, &mut peer).await;
        prompt(&mut driver, &mut peer).await;
        let frame = if unexpected_assistant {
            json!({"type":"assistant","session_id":"session-one","message":{"content":[]}})
        } else {
            json!({"type":"command_lifecycle","command_uuid":"command-one","state":"queued","session_id":"session-one"})
        };
        let bytes = encoded(frame).repeat(if unexpected_assistant { 1 } else { 64 });
        let (result, sent) = tokio::join!(driver.next_message(), peer.output.write_all(&bytes));
        sent.unwrap();
        assert!(matches!(result, Err(Error::Identity | Error::Capacity)));
        assert_eq!(driver.phase(), Phase::Closed);
    }
}

fn state(value: &str) -> Value {
    json!({"type":"system","subtype":"session_state_changed","state":value,"session_id":"session-one"})
}
fn result() -> Value {
    json!({"type":"result","subtype":"success","is_error":false,"result":"answer","session_id":"session-one",
        "modelUsage":{"main":{"inputTokens":1,"outputTokens":1,"cacheReadInputTokens":0,"cacheCreationInputTokens":0}}})
}
fn background(live: bool) -> Value {
    json!({"type":"system","subtype":"background_tasks_changed","session_id":"session-one",
        "tasks":if live {vec![json!({"task_id":"child-one","task_type":"local_agent"})]} else {vec![]}})
}

#[tokio::test]
async fn native_claude_completion_requires_idle_and_result_in_either_order() {
    for idle_first in [false, true] {
        let (mut driver, mut peer) = pair(4096, limits());
        driver.enable_native_completion().unwrap();
        initialize(&mut driver, &mut peer).await;
        prompt(&mut driver, &mut peer).await;
        event(&mut driver, &mut peer, init()).await.unwrap();
        event(
            &mut driver,
            &mut peer,
            if idle_first { state("idle") } else { result() },
        )
        .await
        .unwrap();
        assert_eq!(driver.phase(), Phase::Running);
        event(
            &mut driver,
            &mut peer,
            if idle_first { result() } else { state("idle") },
        )
        .await
        .unwrap();
        assert_eq!(driver.phase(), Phase::ResultObserved);
        assert!(!driver.needs_usage_snapshot());
    }
}

#[tokio::test]
async fn native_claude_foreground_children_also_require_whole_tree_usage() {
    let (mut driver, mut peer) = pair(4096, limits());
    driver.enable_native_completion().unwrap();
    initialize(&mut driver, &mut peer).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    event(&mut driver, &mut peer, json!({"type":"system","subtype":"task_started","session_id":"session-one","task_id":"child-one","task_type":"local_agent","is_backgrounded":false})).await.unwrap();
    event(&mut driver, &mut peer, json!({"type":"system","subtype":"task_notification","session_id":"session-one","task_id":"child-one","status":"completed"})).await.unwrap();
    let mut main_loop_only = result();
    main_loop_only.as_object_mut().unwrap().remove("modelUsage");
    event(&mut driver, &mut peer, main_loop_only).await.unwrap();
    event(&mut driver, &mut peer, state("idle")).await.unwrap();
    assert_eq!(driver.phase(), Phase::ResultObserved);
    assert!(driver.needs_usage_snapshot());
}

#[tokio::test]
async fn native_claude_background_work_keeps_approvals_live_and_refreshes_exact_call_usage() {
    for wrong_correlation in [false, true] {
        let (mut driver, mut peer) = pair(4096, limits());
        driver.enable_native_completion().unwrap();
        initialize(&mut driver, &mut peer).await;
        prompt(&mut driver, &mut peer).await;
        event(&mut driver, &mut peer, init()).await.unwrap();
        event(&mut driver, &mut peer, background(true))
            .await
            .unwrap();
        event(&mut driver, &mut peer, result()).await.unwrap();
        event(&mut driver, &mut peer, state("idle")).await.unwrap();
        assert_eq!(driver.phase(), Phase::Running);
        assert!(matches!(
            event(&mut driver, &mut peer, permission("child-permission"))
                .await
                .unwrap(),
            Message::Permission { .. }
        ));
        event(
            &mut driver,
            &mut peer,
            json!({"type":"control_cancel_request","request_id":"child-permission"}),
        )
        .await
        .unwrap();
        event(&mut driver, &mut peer, background(false))
            .await
            .unwrap();
        assert_eq!(driver.phase(), Phase::ResultObserved);
        assert!(driver.needs_usage_snapshot());
        let (sent, request) = tokio::join!(driver.request_usage_snapshot(), read(&mut peer));
        sent.unwrap();
        assert_eq!(request["message"]["content"], "/usage");
        assert_eq!(request["session_id"], "session-one");
        let mut snapshot_init = init();
        snapshot_init["user_message_uuids"] = json!([request["uuid"]]);
        event(&mut driver, &mut peer, snapshot_init).await.unwrap();
        let mut snapshot = result();
        snapshot["user_message_uuid"] = if wrong_correlation {
            json!("wrong-call")
        } else {
            request["uuid"].clone()
        };
        let observed = event(&mut driver, &mut peer, snapshot).await;
        if wrong_correlation {
            assert!(matches!(observed, Err(Error::Identity)));
            assert_eq!(driver.phase(), Phase::Closed);
        } else {
            observed.unwrap();
            assert_eq!(driver.phase(), Phase::Running);
            event(&mut driver, &mut peer, state("idle")).await.unwrap();
            assert_eq!(driver.phase(), Phase::ResultObserved);
            assert!(!driver.needs_usage_snapshot());
            assert!(driver.usage_snapshot_requested());
            assert!(driver.request_usage_snapshot().await.is_err());
        }
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_EXECUTABLE; isolated local /usage, no model request"]
async fn installed_claude_local_command_uses_the_real_parser_and_completion_path() {
    let executable =
        std::env::var_os("HAGENCY_TEST_CLAUDE_EXECUTABLE").expect("selected installed Claude");
    let root = tempfile::tempdir().unwrap();
    let mut command = tokio::process::Command::new(executable);
    command
        .args(hagency_runtime::claude::interactive_arguments("sonnet", "medium").unwrap())
        .env_clear()
        .env("HOME", root.path())
        .env("CLAUDE_CONFIG_DIR", root.path().join("config"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("ANTHROPIC_API_KEY", "offline-contract")
        .env("ANTHROPIC_BASE_URL", "http://127.0.0.1:9")
        .env("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1")
        .env("CLAUDE_CODE_BG_TASKS_REPORT_RUNNING", "1")
        .current_dir(root.path())
        .kill_on_drop(true)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = command.spawn().unwrap();
    let mut driver = SessionDriver::new(
        child.stdout.take().unwrap(),
        child.stdin.take().unwrap(),
        child.stderr.take().unwrap(),
        Limits {
            write_timeout_ms: 1000,
            event_wait_ms: 10000,
            lifetime_ms: 30000,
        },
    )
    .unwrap();
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    driver.initialize().await.unwrap();
    let mut previous_source = None;
    let mut native_session: Option<String> = None;
    for round in 0..3 {
        if round != 0 {
            assert!(driver.can_followup());
            driver
                .prepare_followup(Limits {
                    write_timeout_ms: 1000,
                    event_wait_ms: 10000,
                    lifetime_ms: 30000,
                })
                .await
                .unwrap();
        }
        driver.prompt("/usage").await.unwrap();
        let init = driver.next_message().await.unwrap();
        assert!(
            matches!(init, Message::Event { kind: EventKind::System, ref payload, .. } if payload["subtype"] == "init")
        );
        let source = driver.observation_source().unwrap();
        if let Some(prior) = previous_source.as_ref() {
            assert!(source != *prior);
            assert!(!driver.matches_observation_source(prior));
        }
        previous_source = Some(source);
        if let Some(expected) = &native_session {
            assert_eq!(driver.session_id(), Some(expected.as_str()));
        }
        native_session = driver.session_id().map(str::to_owned);
        let mut measured_zero = false;
        for _ in 0..64 {
            driver.next_message().await.unwrap();
            if let Some(observation) = driver.last_observation()
                && let hagency_runtime::claude::session::ObservationKind::Result { usage, is_error } =
                    observation.kind()
            {
                assert!(!is_error);
                assert_eq!(usage.counts().input(), Some(0));
                assert_eq!(usage.counts().output(), Some(0));
                measured_zero = true;
            }
            if driver.phase() == Phase::ResultObserved {
                break;
            }
        }
        assert!(measured_zero);
        assert_eq!(driver.phase(), Phase::ResultObserved);
        assert!(!driver.needs_usage_snapshot());
    }
    driver.close();
    child.kill().await.unwrap();
}

#[tokio::test]
async fn native_claude_session_identity_and_result() {
    let (mut driver, mut peer) = running().await;
    assert_eq!(driver.session_id(), Some("session-one"));
    assert!(matches!(
        event(&mut driver, &mut peer, permission("permission-one"))
            .await
            .unwrap(),
        Message::Permission { .. }
    ));
    event(
        &mut driver,
        &mut peer,
        json!({"type":"control_cancel_request","request_id":"permission-one"}),
    )
    .await
    .unwrap();
    let result = event(
        &mut driver,
        &mut peer,
        json!({"type":"result","session_id":"session-one",
        "subtype":"success","is_error":true,"result":"not canonical completion"}),
    )
    .await
    .unwrap();
    assert!(
        matches!(result,Message::Event {kind:EventKind::Result,payload,..} if payload["is_error"]==true)
    );
    assert_eq!(driver.phase(), Phase::ResultObserved);
    assert!(driver.termination().is_none());
    assert!(matches!(
        driver.prompt("cannot reuse").await,
        Err(Error::State)
    ));

    for value in [
        init(),
        json!({"type":"assistant","session_id":"other","message":{}}),
        json!({"type":"control_response","response":{"subtype":"success","request_id":"unsolicited","response":{}}}),
        json!({"type":"control_cancel_request","request_id":"unknown"}),
    ] {
        let (mut driver, mut peer) = running().await;
        assert!(matches!(
            event(&mut driver, &mut peer, value).await,
            Err(Error::Identity)
        ));
        assert_eq!(driver.phase(), Phase::Closed);
    }
    for early in [
        permission("early"),
        json!({"type":"assistant","session_id":"session-one","message":{}}),
    ] {
        let (mut driver, mut peer) = pair(4096, limits());
        initialize(&mut driver, &mut peer).await;
        prompt(&mut driver, &mut peer).await;
        assert!(matches!(
            event(&mut driver, &mut peer, early).await,
            Err(Error::Identity)
        ));
    }
    for cancelled in [false, true] {
        let (mut driver, mut peer) = running().await;
        event(&mut driver, &mut peer, permission("duplicate"))
            .await
            .unwrap();
        if cancelled {
            event(
                &mut driver,
                &mut peer,
                json!({"type":"control_cancel_request","request_id":"duplicate"}),
            )
            .await
            .unwrap();
        }
        assert!(matches!(
            event(&mut driver, &mut peer, permission("duplicate")).await,
            Err(Error::Identity)
        ));
    }
    let (mut driver, mut peer) = running().await;
    for n in 0..16 {
        event(&mut driver, &mut peer, permission(&format!("p-{n}")))
            .await
            .unwrap();
    }
    assert!(matches!(
        event(&mut driver, &mut peer, permission("over-cap")).await,
        Err(Error::Capacity)
    ));
    for refused in [false, true] {
        let (mut driver, mut peer) = pair(4096, limits());
        let (result, ()) = tokio::join!(driver.initialize(), async {
            let request = read(&mut peer).await;
            let response = if refused {
                json!({"subtype":"error","request_id":request["request_id"],"error":"private error"})
            } else {
                json!({"subtype":"success","request_id":"wrong","response":{}})
            };
            peer.output
                .write_all(&encoded(
                    json!({"type":"control_response","response":response}),
                ))
                .await
                .unwrap();
        });
        assert_eq!(
            result,
            Err(if refused {
                Error::Refused
            } else {
                Error::Identity
            })
        );
        assert_eq!(driver.phase(), Phase::Closed);
    }
    let (mut driver, mut peer) = pair(4096, limits());
    let (result, ()) = tokio::join!(driver.initialize(), async {
        let request = read(&mut peer).await;
        let mut bytes = encoded(json!({"type":"control_response","response":{
            "subtype":"success","request_id":request["request_id"],"response":{"claude_code_version":"2.1.321"}}}));
        bytes.extend(encoded(init()));
        peer.output.write_all(&bytes).await.unwrap();
    });
    result.unwrap();
    // Already-observed pre-prompt bytes cannot be relabeled as this prompt's
    // session. This does not claim causal proof for unread peer bytes.
    assert_eq!(
        driver.prompt("must refuse buffered pre-prompt input").await,
        Err(Error::Identity)
    );
}

#[tokio::test(start_paused = true)]
async fn native_claude_session_deadlines_and_cancellation() {
    for fail in [false, true] {
        let (stdout, _output) = tokio::io::duplex(64);
        let (stderr, _error) = tokio::io::duplex(64);
        let mut driver = SessionDriver::new(stdout, FlushProbe { fail }, stderr, limits()).unwrap();
        assert_eq!(
            driver.initialize().await,
            Err(if fail {
                Error::Io("stdin flush")
            } else {
                Error::Timeout
            })
        );
        let progress = driver.termination().unwrap().unconfirmed_write.unwrap();
        assert_eq!(progress.accepted_bytes, progress.total_bytes);
        assert!(!progress.flushed);
        assert!(!format!("{:?}", driver.termination()).contains("private provider sentinel"));
    }
    let (mut driver, _peer) = pair(8, limits());
    assert_eq!(driver.initialize().await, Err(Error::Timeout));
    let progress = driver.termination().unwrap().unconfirmed_write.unwrap();
    assert_eq!(progress.accepted_bytes, 8);
    assert!(progress.total_bytes > 8);
    assert!(!progress.flushed);
    assert_eq!(driver.initialize().await, Err(Error::Closed));

    let (mut driver, _peer) = pair(8, limits());
    drop(driver.initialize());
    assert_eq!(driver.phase(), Phase::New);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), driver.initialize())
            .await
            .is_err()
    );
    assert_eq!(driver.termination().unwrap().cause, Error::Cancelled);
    assert_eq!(
        driver
            .termination()
            .unwrap()
            .unconfirmed_write
            .unwrap()
            .accepted_bytes,
        8
    );
    assert_eq!(driver.initialize().await, Err(Error::Closed));

    let (mut driver, mut peer) = pair(512, limits());
    initialize(&mut driver, &mut peer).await;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(20),
            driver.prompt(&"x".repeat(64 * 1024))
        )
        .await
        .is_err()
    );
    assert_eq!(driver.termination().unwrap().cause, Error::Cancelled);
    let progress = driver.termination().unwrap().unconfirmed_write.unwrap();
    assert!(progress.accepted_bytes > 0 && progress.accepted_bytes < progress.total_bytes);
    assert_eq!(driver.phase(), Phase::Closed);

    let (mut driver, mut peer) = pair(
        4096,
        Limits {
            event_wait_ms: 20_000,
            ..limits()
        },
    );
    let started = tokio::time::Instant::now();
    let (result, ()) = tokio::join!(driver.initialize(), async {
        read(&mut peer).await;
        peer.output.write_all(b"{").await.unwrap();
    });
    assert_eq!(
        result,
        Err(Error::Protocol(hagency_runtime::claude::Error::Timeout))
    );
    assert_eq!(started.elapsed(), Duration::from_secs(10));

    let (mut driver, mut peer) = pair(
        4096,
        Limits {
            event_wait_ms: 20_000,
            lifetime_ms: 100,
            ..limits()
        },
    );
    let (result, ()) = tokio::join!(driver.initialize(), async {
        read(&mut peer).await;
    });
    assert_eq!(result, Err(Error::Timeout));
    assert!(driver.termination().unwrap().unconfirmed_write.is_none());

    let (mut driver, mut peer) = running().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), driver.next_message())
            .await
            .is_err()
    );
    assert_eq!(driver.termination().unwrap().cause, Error::Cancelled);
    assert!(peer.output.write_all(b"closed").await.is_err());
    for partial in [false, true] {
        let (mut driver, mut peer) = running().await;
        if partial {
            peer.output.write_all(b"{").await.unwrap();
        }
        drop(peer.output);
        let result = driver.next_message().await;
        let expected = if partial {
            Error::Protocol(hagency_runtime::claude::Error::UnexpectedEof)
        } else {
            Error::PeerEof
        };
        assert!(matches!(result,Err(error) if error==expected));
    }
}

#[tokio::test]
async fn native_claude_session_backpressure_and_capacity() {
    let (mut driver, mut peer) = pair(512, limits());
    initialize(&mut driver, &mut peer).await;
    let text = "x".repeat(64 * 1024);
    let (result, ()) = tokio::join!(driver.prompt(&text), async {
        // Peer cannot drain stdin until BOTH output pipes are drained by host.
        peer.stderr.write_all(&vec![b'e'; 64 * 1024]).await.unwrap();
        peer.output.write_all(&encoded(init())).await.unwrap();
        assert_eq!(read(&mut peer).await["message"]["content"], text);
    });
    assert!(result.unwrap().flushed);
    assert!(matches!(
        driver.next_message().await.unwrap(),
        Message::Event {
            kind: EventKind::System,
            ..
        }
    ));
    let stderr = driver.stderr_snapshot();
    assert_eq!(stderr.total_bytes, 64 * 1024);
    assert_eq!(stderr.tail, vec![b'e'; 16 * 1024]);

    for large in [false, true] {
        let (mut driver, mut peer) = pair(512, limits());
        initialize(&mut driver, &mut peer).await;
        let bytes = if large {
            encoded(
                json!({"type":"assistant","session_id":"session-one","message":{"text":"x".repeat(800*1024)}}),
            )
        } else {
            encoded(init())
        };
        let (result, _) = tokio::join!(driver.prompt(&text), async {
            for _ in 0..if large { 3 } else { 17 } {
                peer.output.write_all(&bytes).await?;
            }
            Ok::<(), std::io::Error>(())
        });
        assert_eq!(result, Err(Error::Capacity));
        let progress = driver.termination().unwrap().unconfirmed_write.unwrap();
        assert!(progress.accepted_bytes < progress.total_bytes);
        assert!(!progress.flushed);
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_EXECUTABLE; local Anthropic SSE only, no paid model"]
async fn installed_claude_question_contract_returns_native_multiselect_and_free_text() {
    use hagency_runtime::questions::Answer;
    use hagency_runtime::claude::session::{ApprovalControlPolicy, PreparedUpdate};
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture.py");
    std::fs::write(&fixture, include_str!("claude_question_fixture.py")).unwrap();
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
    driver.initialize().await.unwrap();
    driver
        .prompt("Ask the four offline questions and acknowledge my choices.")
        .await
        .unwrap();
    driver.next_message().await.unwrap();
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 20_000,
            response_reserve_ms: 2000,
        })
        .unwrap();
    let mut asked = false;
    for _ in 0..128 {
        let message = driver.next_message().await.unwrap();
        if let Message::Permission {
            request_id,
            tool_name,
            ..
        } = message
        {
            assert_eq!(tool_name, "AskUserQuestion");
            assert!(!asked);
            asked = true;
            assert_eq!(driver.question(&request_id).unwrap().len(), 4);
            let answers = [
                (
                    "q0".into(),
                    Answer::Options(vec!["One".into(), "Two".into()]),
                ),
                ("q1".into(), Answer::Text("My custom answer".into())),
                ("q2".into(), Answer::Option("One".into())),
                ("q3".into(), Answer::Option("Two".into())),
            ]
            .into();
            let mut prepared = driver.prepare_question(&request_id, &answers).unwrap();
            loop {
                if matches!(
                    driver.send_prepared_approval(&mut prepared).await.unwrap(),
                    PreparedUpdate::WriteAccepted(_)
                ) {
                    break;
                }
            }
        }
        if driver.phase() == Phase::ResultObserved {
            break;
        }
    }
    assert!(asked);
    assert_eq!(driver.phase(), Phase::ResultObserved);
    let answer = std::fs::read_to_string(root.path().join("native-answer.json")).unwrap();
    assert!(
        answer.contains("One, Two") && answer.contains("My custom answer"),
        "{answer}"
    );
    driver.close();
    child.kill().await.unwrap();
    server.kill().await.unwrap();
}

#[tokio::test]
async fn native_claude_protocol_version_is_checked_before_any_prompt() {
    for version in [
        Value::Null,
        json!(2),
        json!("2.1.320"),
        json!("3.0.0"),
        json!("2.01.321"),
        json!("2.1.321-beta"),
        json!("2.1.321"),
        json!("2.2.0"),
    ] {
        let supported = version == "2.1.321" || version == "2.2.0";
        let (mut driver, mut peer) = pair(8192, limits());
        let (result, ()) = tokio::join!(driver.initialize(), async {
            let request = read(&mut peer).await;
            peer.output
                .write_all(&encoded(json!({"type":"control_response","response":{
                "subtype":"success","request_id":request["request_id"],
                "response":{"claude_code_version":version}}})))
                .await
                .unwrap();
        });
        if supported {
            assert_eq!(result, Ok(()));
            assert_eq!(driver.phase(), Phase::Ready);
        } else {
            assert_eq!(result, Err(Error::UnsupportedVersion));
            assert_eq!(driver.phase(), Phase::Closed);
            let mut remaining = String::new();
            assert_eq!(peer.input.read_line(&mut remaining).await.unwrap(), 0);
            assert!(
                remaining.is_empty(),
                "unsupported CLI receives no model input"
            );
        }
    }
}
