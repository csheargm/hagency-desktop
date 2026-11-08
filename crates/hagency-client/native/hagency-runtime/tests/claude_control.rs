use hagency_runtime::claude::{
    Message,
    session::{
        ApprovalControlPolicy, ControlUpdate, Error, Limits, PermissionDecision, Phase,
        PreparedUpdate, SessionDriver,
    },
};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, DuplexStream};

type Driver = SessionDriver<DuplexStream, DuplexStream, DuplexStream>;
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
    _stderr: DuplexStream,
}
fn limits() -> Limits {
    Limits {
        write_timeout_ms: 500,
        event_wait_ms: 2000,
        lifetime_ms: 30_000,
    }
}
fn policy() -> ApprovalControlPolicy {
    ApprovalControlPolicy {
        owner_wait_ms: 1000,
        response_reserve_ms: 1000,
    }
}
fn frame(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}
async fn read(peer: &mut Peer) -> Value {
    let mut line = String::new();
    assert!(peer.input.read_line(&mut line).await.unwrap() > 0);
    serde_json::from_str(&line).unwrap()
}
fn request(id: &str, input: Value) -> Value {
    json!({"type":"control_request","request_id":id,
    "request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool-one","input":input}})
}
fn notice() -> Value {
    json!({"type":"system","subtype":"status","session_id":"session-one"})
}
fn cancel(id: &str) -> Value {
    json!({"type":"control_cancel_request","request_id":id})
}
fn result() -> Value {
    json!({"type":"result","subtype":"success","session_id":"session-one","is_error":false,"result":"not Done"})
}
async fn event(driver: &mut Driver, peer: &mut Peer, value: Value) -> Result<Message, Error> {
    let bytes = frame(value);
    let (received, written) = tokio::join!(driver.next_message(), peer.output.write_all(&bytes));
    written.unwrap();
    received
}
async fn running(capacity: usize, enabled: bool) -> (Driver, Peer) {
    let (stdin, input) = tokio::io::duplex(capacity);
    let (stdout, output) = tokio::io::duplex(capacity);
    let (stderr, error) = tokio::io::duplex(capacity);
    let mut driver = Driver::new(stdout, stdin, stderr, limits()).unwrap();
    let mut peer = Peer {
        input: BufReader::new(input),
        output,
        _stderr: error,
    };
    let (initialized, ()) = tokio::join!(driver.initialize(), async {
        let value = read(&mut peer).await;
        peer.output
            .write_all(&frame(json!({"type":"control_response","response":{
            "subtype":"success","request_id":value["request_id"],"response":{}}})))
            .await
            .unwrap();
    });
    initialized.unwrap();
    let (prompted, _) = tokio::join!(driver.prompt("offline"), read(&mut peer));
    prompted.unwrap();
    event(
        &mut driver,
        &mut peer,
        json!({"type":"system","subtype":"init","session_id":"session-one"}),
    )
    .await
    .unwrap();
    if enabled {
        driver.enable_approval_control(policy()).unwrap();
    }
    (driver, peer)
}
async fn callback(driver: &mut Driver, peer: &mut Peer, id: &str, input: Value) {
    assert!(matches!(
        event(driver, peer, request(id, input)).await.unwrap(),
        Message::Permission { .. }
    ));
}

#[tokio::test]
async fn native_claude_many_sequential_tools_keep_replay_protection() {
    let (mut driver, mut peer) = running(4096, true).await;
    for n in 0..40 {
        let id = format!("request-{n}");
        let input = json!({"command":format!("step {n}")});
        callback(&mut driver, &mut peer, &id, input.clone()).await;
        let mut response = driver
            .prepare_approval(&id, PermissionDecision::Allow)
            .unwrap();
        let (sent, reply) = tokio::join!(
            driver.send_prepared_approval(&mut response),
            read(&mut peer)
        );
        assert!(matches!(sent.unwrap(), PreparedUpdate::WriteAccepted(_)));
        assert_eq!(reply["response"]["response"]["updatedInput"], input);
    }
    assert!(matches!(
        event(
            &mut driver,
            &mut peer,
            request("request-0", json!({"command":"changed"}))
        )
        .await,
        Err(Error::Identity)
    ));
}
#[tokio::test]
async fn native_claude_permission_exact_response() {
    for decision in [PermissionDecision::Allow, PermissionDecision::Deny] {
        let (mut driver, mut peer) = running(4096, true).await;
        let original = json!({"command":"printf '%s' 'literal 中文'","nested":{"x":[1,null,true]}});
        let mut observed = event(
            &mut driver,
            &mut peer,
            request("request-one", original.clone()),
        )
        .await
        .unwrap();
        if let Message::Permission { input, .. } = &mut observed {
            *input = json!({"command":"substituted input"});
        }
        let mut prepared = driver.prepare_approval("request-one", decision).unwrap();
        assert_eq!(prepared.request_id(), "request-one");
        assert!(prepared.response_deadline() > driver.approval_deadline("request-one").unwrap());
        let (sent, response) = tokio::join!(
            driver.send_prepared_approval(&mut prepared),
            read(&mut peer)
        );
        assert!(
            matches!(sent.unwrap(),PreparedUpdate::WriteAccepted(progress) if progress.flushed && progress.accepted_bytes==progress.total_bytes)
        );
        let expected = match decision {
            PermissionDecision::Allow => json!({"behavior":"allow","updatedInput":original}),
            PermissionDecision::Deny => {
                json!({"behavior":"deny","message":"Permission denied by Hagency.","interrupt":true})
            }
        };
        assert_eq!(
            response,
            json!({"type":"control_response","response":{
            "subtype":"success","request_id":"request-one","response":expected}})
        );
        // A flushed response still leaves the session running; no application
        // acknowledgement, task completion or cleanup was invented.
        assert_eq!(driver.phase(), Phase::Running);
        assert!(driver.termination().is_none());
        assert!(matches!(
            driver.send_prepared_approval(&mut prepared).await,
            Err(Error::PermissionUnavailable)
        ));
    }
    let (mut first, mut peer) = running(4096, true).await;
    callback(&mut first, &mut peer, "same-id", json!({"command":"first"})).await;
    let mut prepared = first
        .prepare_approval("same-id", PermissionDecision::Allow)
        .unwrap();
    let (mut foreign, mut other) = running(4096, true).await;
    callback(
        &mut foreign,
        &mut other,
        "same-id",
        json!({"command":"other"}),
    )
    .await;
    assert!(matches!(
        foreign.send_prepared_approval(&mut prepared).await,
        Err(Error::Identity)
    ));
    let (sent, response) =
        tokio::join!(first.send_prepared_approval(&mut prepared), read(&mut peer));
    assert!(matches!(sent.unwrap(), PreparedUpdate::WriteAccepted(_)));
    assert_eq!(
        response["response"]["response"]["updatedInput"],
        json!({"command":"first"})
    );

    let (mut driver, mut peer) = running(4096, true).await;
    callback(&mut driver, &mut peer, "once", json!({})).await;
    let _original = driver
        .prepare_approval("once", PermissionDecision::Deny)
        .unwrap();
    assert!(matches!(
        driver.prepare_approval("once", PermissionDecision::Allow),
        Err(Error::PermissionUnavailable)
    ));
}

#[tokio::test(start_paused = true)]
async fn native_claude_supervised_turn_survives_silence_without_renewing_approval() {
    let (mut driver, mut peer) = running(4096, false).await;
    driver.supervise_turn().unwrap();
    driver.enable_approval_control(policy()).unwrap();
    let started = tokio::time::Instant::now();
    let (received, ()) = tokio::join!(driver.next_message(), async {
        tokio::time::sleep(Duration::from_secs(1801)).await;
        peer.output.write_all(&frame(notice())).await.unwrap();
    });
    assert!(matches!(received.unwrap(), Message::Event { .. }));
    assert_eq!(started.elapsed(), Duration::from_secs(1801));
    callback(
        &mut driver,
        &mut peer,
        "fixed-owner-wait",
        json!({"command":"write"}),
    )
    .await;
    let deadline = driver.approval_deadline("fixed-owner-wait").unwrap();
    assert_eq!(
        driver.supervise_turn(),
        Err(Error::State),
        "cannot extend a pending grant"
    );
    assert_eq!(
        driver.approval_deadline("fixed-owner-wait").unwrap(),
        deadline
    );
    tokio::time::advance(Duration::from_millis(1001)).await;
    assert!(matches!(
        driver.prepare_approval("fixed-owner-wait", PermissionDecision::Allow),
        Err(Error::Timeout)
    ));
}

#[tokio::test(start_paused = true)]
async fn native_claude_supervised_wait_keeps_partial_frame_and_host_stop() {
    for partial in [false, true] {
        let (mut driver, mut peer) = running(4096, false).await;
        driver.supervise_turn().unwrap();
        driver.enable_approval_control(policy()).unwrap();
        let source = driver.observation_source().unwrap();
        tokio::time::advance(Duration::from_secs(1801)).await;
        if partial {
            peer.output.write_all(b"{").await.unwrap();
        }
        let started = tokio::time::Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(20), driver.next_message()).await;
        if partial {
            assert!(matches!(
                result,
                Ok(Err(Error::Protocol(
                    hagency_runtime::claude::Error::Timeout
                )))
            ));
            assert_eq!(started.elapsed(), Duration::from_secs(10));
        } else {
            assert!(result.is_err());
            assert_eq!(driver.termination().unwrap().cause, Error::Cancelled);
        }
        assert!(source.is_retired());
        assert_eq!(driver.phase(), Phase::Closed);
        assert!(peer.output.write_all(b"no replay").await.is_err());
    }
}

#[tokio::test(start_paused = true)]
async fn native_claude_permission_control_deadlines() {
    let (mut driver, mut peer) = running(4096, true).await;
    callback(&mut driver, &mut peer, "pending", json!({})).await;
    let owner = driver.approval_deadline("pending").unwrap();
    let (send, receive) = tokio::sync::oneshot::channel::<usize>();
    tokio::pin!(receive);
    peer.output.write_all(&frame(notice())).await.unwrap();
    assert!(matches!(
        driver.next_or_control(receive.as_mut()).await.unwrap(),
        ControlUpdate::Message(_)
    ));
    send.send(7).unwrap();
    assert!(matches!(
        driver.next_or_control(receive.as_mut()).await.unwrap(),
        ControlUpdate::Control(Ok(7))
    ));
    assert_eq!(driver.approval_deadline("pending").unwrap(), owner);
    tokio::time::advance(Duration::from_millis(1000)).await;
    assert!(matches!(
        driver.prepare_approval("pending", PermissionDecision::Allow),
        Err(Error::Timeout)
    ));

    let (mut driver, _peer) = running(4096, true).await;
    for _ in 0..4 {
        let ready = std::future::ready(9);
        tokio::pin!(ready);
        assert!(matches!(
            driver.next_or_control(ready.as_mut()).await.unwrap(),
            ControlUpdate::Control(9)
        ));
        tokio::time::advance(Duration::from_millis(500)).await;
    }
    let ready = std::future::ready(9);
    tokio::pin!(ready);
    assert!(matches!(
        driver.next_or_control(ready.as_mut()).await,
        Err(Error::Timeout)
    ));

    let (mut driver, mut peer) = running(4096, true).await;
    // Both messages are returned by one actual pipe read. Consuming the second
    // later must not replace its original receive timestamp with "now".
    let mut bytes = frame(notice());
    bytes.extend(frame(request("delayed", json!({}))));
    peer.output.write_all(&bytes).await.unwrap();
    driver.next_message().await.unwrap();
    tokio::time::advance(Duration::from_millis(1001)).await;
    assert!(matches!(driver.next_message().await, Err(Error::Timeout)));

    let (mut driver, mut peer) = running(4096, true).await;
    callback(&mut driver, &mut peer, "fixed-write", json!({})).await;
    let mut prepared = driver
        .prepare_approval("fixed-write", PermissionDecision::Allow)
        .unwrap();
    peer.output.write_all(&frame(notice())).await.unwrap();
    assert!(matches!(
        driver.send_prepared_approval(&mut prepared).await.unwrap(),
        PreparedUpdate::Message(_)
    ));
    tokio::time::advance(Duration::from_millis(501)).await;
    assert!(matches!(
        driver.send_prepared_approval(&mut prepared).await,
        Err(Error::Timeout)
    ));
    assert_eq!(
        driver
            .termination()
            .unwrap()
            .unconfirmed_write
            .unwrap()
            .accepted_bytes,
        0
    );
}

#[tokio::test(start_paused = true)]
async fn native_claude_permission_write_barriers() {
    for terminal in [false, true] {
        let (mut driver, mut peer) = running(4096, true).await;
        callback(&mut driver, &mut peer, "barrier", json!({})).await;
        let mut prepared = driver
            .prepare_approval("barrier", PermissionDecision::Allow)
            .unwrap();
        let bytes = frame(if terminal {
            result()
        } else {
            cancel("barrier")
        });
        peer.output
            .write_all(&bytes[..bytes.len() - 1])
            .await
            .unwrap();
        let (sent, ()) = tokio::join!(driver.send_prepared_approval(&mut prepared), async {
            tokio::time::sleep(Duration::from_millis(25)).await;
            // A readable partial stdout frame must fence the first response byte.
            let mut byte = [0];
            assert!(
                tokio::time::timeout(Duration::from_millis(1), peer.input.read(&mut byte))
                    .await
                    .is_err()
            );
            peer.output.write_all(b"\n").await.unwrap();
        });
        assert!(matches!(sent.unwrap(), PreparedUpdate::Message(_)));
        assert_eq!(driver.write_progress().unwrap().accepted_bytes, 0);
        assert!(matches!(
            driver.send_prepared_approval(&mut prepared).await,
            Err(Error::State | Error::PermissionUnavailable)
        ));
        assert_eq!(
            driver
                .termination()
                .unwrap()
                .unconfirmed_write
                .unwrap()
                .accepted_bytes,
            0
        );
    }
    let (mut driver, mut peer) = running(128, true).await;
    callback(
        &mut driver,
        &mut peer,
        "partial",
        json!({"command":"x".repeat(4000)}),
    )
    .await;
    let mut prepared = driver
        .prepare_approval("partial", PermissionDecision::Allow)
        .unwrap();
    let (sent, ()) = tokio::join!(driver.send_prepared_approval(&mut prepared), async {
        // Observe a prefix before sending cancellation; leave the rest blocked.
        let mut prefix = [0; 8];
        peer.input.read_exact(&mut prefix).await.unwrap();
        peer.output
            .write_all(&frame(cancel("partial")))
            .await
            .unwrap();
    });
    assert!(matches!(
        sent.unwrap(),
        PreparedUpdate::Message(Message::ControlCancel { .. })
    ));
    let before = driver.write_progress().unwrap();
    assert!(before.accepted_bytes > 0 && before.accepted_bytes < before.total_bytes);
    let ready = std::future::ready(());
    tokio::pin!(ready);
    assert!(matches!(
        driver.next_or_control(ready.as_mut()).await.unwrap(),
        ControlUpdate::Control(())
    ));
    assert_eq!(driver.write_progress().unwrap(), before);
    assert!(matches!(
        driver.send_prepared_approval(&mut prepared).await,
        Err(Error::PermissionUnavailable)
    ));
    assert_eq!(
        driver.termination().unwrap().unconfirmed_write.unwrap(),
        before
    );
}

#[tokio::test(start_paused = true)]
async fn native_claude_permission_bounds_and_cancel() {
    let (mut driver, mut peer) = running(4096, false).await;
    callback(&mut driver, &mut peer, "disabled", json!({})).await;
    assert!(matches!(
        driver.prepare_approval("disabled", PermissionDecision::Allow),
        Err(Error::State)
    ));
    for policy in [
        ApprovalControlPolicy {
            owner_wait_ms: 0,
            ..policy()
        },
        ApprovalControlPolicy {
            response_reserve_ms: 499,
            ..policy()
        },
        ApprovalControlPolicy {
            owner_wait_ms: 30_000,
            ..policy()
        },
    ] {
        let (mut driver, _) = running(4096, false).await;
        assert!(driver.enable_approval_control(policy).is_err());
        assert_eq!(driver.phase(), Phase::Closed);
    }
    let (mut driver, mut peer) = running(4096, true).await;
    assert!(matches!(
        event(
            &mut driver,
            &mut peer,
            request("oversize", json!({"x":"x".repeat(64*1024)}))
        )
        .await,
        Err(Error::Capacity)
    ));
    let (mut driver, mut peer) = running(4096, true).await;
    for n in 0..16 {
        callback(&mut driver, &mut peer, &format!("p-{n}"), json!({})).await;
    }
    assert!(matches!(
        event(&mut driver, &mut peer, request("over-count", json!({}))).await,
        Err(Error::Capacity)
    ));

    let (mut driver, _peer) = running(4096, true).await;
    let pending = std::future::pending::<()>();
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            driver.next_or_control(pending.as_mut())
        )
        .await
        .is_err()
    );
    assert_eq!(driver.termination().unwrap().cause, Error::Cancelled);

    let (mut driver, mut peer) = running(128, true).await;
    callback(
        &mut driver,
        &mut peer,
        "cancel-send",
        json!({"x":"x".repeat(4000)}),
    )
    .await;
    let mut prepared = driver
        .prepare_approval("cancel-send", PermissionDecision::Allow)
        .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(10),
            driver.send_prepared_approval(&mut prepared)
        )
        .await
        .is_err()
    );
    assert_eq!(driver.termination().unwrap().cause, Error::Cancelled);
    let progress = driver.termination().unwrap().unconfirmed_write.unwrap();
    assert!(progress.accepted_bytes > 0 && progress.accepted_bytes < progress.total_bytes);
    assert!(matches!(
        driver.send_prepared_approval(&mut prepared).await,
        Err(Error::Closed)
    ));
}

/// Stdin whose flush can be held back, so the peer can read a complete
/// response and answer before the host observes its own flush.
struct HeldFlush {
    inner: DuplexStream,
    gate: std::sync::Arc<std::sync::Mutex<(bool, Option<std::task::Waker>)>>,
}
impl tokio::io::AsyncWrite for HeldFlush {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        {
            let mut gate = self.gate.lock().unwrap();
            if !gate.0 {
                gate.1 = Some(cx.waker().clone());
                return std::task::Poll::Pending;
            }
        }
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The peer reads the whole response and ends its turn before the host sees
/// its own flush. The started write is still completed after the Result, and
/// the answer events arrive first, in order.
#[tokio::test]
async fn native_claude_permission_answer_before_flush_completes_the_write() {
    let gate = std::sync::Arc::new(std::sync::Mutex::new((true, None::<std::task::Waker>)));
    let (stdin, input) = tokio::io::duplex(4096);
    let (stdout, output) = tokio::io::duplex(4096);
    let (stderr, error) = tokio::io::duplex(4096);
    let stdin = HeldFlush {
        inner: stdin,
        gate: gate.clone(),
    };
    let mut driver = SessionDriver::new(stdout, stdin, stderr, limits()).unwrap();
    let mut peer = Peer {
        input: BufReader::new(input),
        output,
        _stderr: error,
    };
    let (initialized, ()) = tokio::join!(driver.initialize(), async {
        let value = read(&mut peer).await;
        peer.output
            .write_all(&frame(json!({"type":"control_response","response":{
            "subtype":"success","request_id":value["request_id"],"response":{}}})))
            .await
            .unwrap();
    });
    initialized.unwrap();
    let (prompted, _) = tokio::join!(driver.prompt("offline"), read(&mut peer));
    prompted.unwrap();
    let init = frame(json!({"type":"system","subtype":"init","session_id":"session-one"}));
    let (received, written) = tokio::join!(driver.next_message(), peer.output.write_all(&init));
    written.unwrap();
    received.unwrap();
    driver.enable_approval_control(policy()).unwrap();
    let permission = frame(request("early", json!({})));
    let (received, written) =
        tokio::join!(driver.next_message(), peer.output.write_all(&permission));
    written.unwrap();
    assert!(matches!(received.unwrap(), Message::Permission { .. }));
    let mut prepared = driver
        .prepare_approval("early", PermissionDecision::Allow)
        .unwrap();
    gate.lock().unwrap().0 = false;
    let assistant = json!({"type":"assistant","session_id":"session-one","message":{"content":[]}});
    let (first, ()) = tokio::join!(driver.send_prepared_approval(&mut prepared), async {
        let response = read(&mut peer).await;
        assert_eq!(response["response"]["request_id"], "early");
        peer.output.write_all(&frame(assistant)).await.unwrap();
        peer.output.write_all(&frame(result())).await.unwrap();
    });
    let mut early = vec![first.unwrap()];
    while early.len() < 2 {
        early.push(driver.send_prepared_approval(&mut prepared).await.unwrap());
    }
    assert!(
        early
            .iter()
            .all(|update| matches!(update, PreparedUpdate::Message(_)))
    );
    assert_eq!(driver.phase(), Phase::ResultObserved);
    {
        let mut held = gate.lock().unwrap();
        held.0 = true;
        if let Some(waker) = held.1.take() {
            waker.wake();
        }
    }
    assert!(matches!(
        driver.send_prepared_approval(&mut prepared).await.unwrap(),
        PreparedUpdate::WriteAccepted(progress) if progress.flushed
    ));
    assert!(driver.termination().is_none());
}

fn question_input() -> Value {
    json!({"questions": (0..4).map(|i| json!({
        "question":format!("Question {i}?"), "header":format!("Q{i}"), "multiSelect":i == 0,
        "options":[{"label":"One","description":"First"},{"label":"Two","description":"Second"}]
    })).collect::<Vec<_>>(), "metadata":{"source":"native"}, "answers":{"injected":"ignored"}})
}
async fn question(driver: &mut Driver, peer: &mut Peer, input: Value) {
    let mut value = request("question", input);
    value["request"]["tool_name"] = json!("AskUserQuestion");
    event(driver, peer, value).await.unwrap();
}

#[tokio::test]
async fn native_question_four_questions_multiselect_and_text_return_original_input() {
    use hagency_runtime::questions::Answer;
    let (mut driver, mut peer) = running(8192, true).await;
    let input = question_input();
    question(&mut driver, &mut peer, input.clone()).await;
    let data = driver.question("question").unwrap();
    assert_eq!(data.len(), 4);
    assert!(data[0].multi_select && data[3].accepts_text());
    let answers = [
        (
            "q0".into(),
            Answer::Options(vec!["Two".into(), "One".into()]),
        ),
        ("q1".into(), Answer::Option("One".into())),
        ("q2".into(), Answer::Text("Custom answer".into())),
        ("q3".into(), Answer::Option("Two".into())),
    ]
    .into();
    let mut response = driver.prepare_question("question", &answers).unwrap();
    let (sent, value) = tokio::join!(
        driver.send_prepared_approval(&mut response),
        read(&mut peer)
    );
    assert!(matches!(sent, Ok(PreparedUpdate::WriteAccepted(_))));
    let native = &value["response"]["response"];
    assert_eq!(native["behavior"], "allow");
    assert_eq!(native["updatedInput"]["questions"], input["questions"]);
    assert_eq!(native["updatedInput"]["metadata"], input["metadata"]);
    assert_eq!(
        native["updatedInput"]["answers"],
        json!({"Question 0?":"Two, One","Question 1?":"One","Question 2?":"Custom answer","Question 3?":"Two"})
    );
    assert!(native.get("updatedPermissions").is_none());
    assert!(
        driver.prepare_question("question", &answers).is_err(),
        "cannot replay a consumed callback"
    );
}

#[tokio::test]
async fn question_channel_cannot_approve_tools_or_encode_invalid_native_answers() {
    use hagency_runtime::questions::Answer;
    let (mut driver, mut peer) = running(8192, true).await;
    callback(&mut driver, &mut peer, "tool", question_input()).await;
    assert!(driver.question("tool").is_err());
    assert!(
        driver
            .prepare_question("tool", &Default::default())
            .is_err()
    );
    for answers in [
        [(
            "q1".into(),
            Answer::Options(vec!["One".into(), "Two".into()]),
        )]
        .into(),
        [(
            "q0".into(),
            Answer::Options(vec!["One".into(), "One".into()]),
        )]
        .into(),
        [("q0".into(), Answer::Option("Not an option".into()))].into(),
        [("foreign".into(), Answer::Text("Injected".into()))].into(),
    ] {
        let (mut driver, mut peer) = running(8192, true).await;
        question(&mut driver, &mut peer, question_input()).await;
        assert!(driver.prepare_question("question", &answers).is_err());
    }
    let (mut driver, mut peer) = running(8192, true).await;
    question(&mut driver, &mut peer, question_input()).await;
    assert!(
        driver
            .prepare_approval("question", PermissionDecision::Allow)
            .is_err(),
        "a Boolean tool grant cannot invent an answer"
    );
}

#[tokio::test]
async fn unanswered_question_continues_without_interrupt_and_cancelled_question_cannot_send() {
    let (mut driver, mut peer) = running(8192, true).await;
    question(&mut driver, &mut peer, question_input()).await;
    let mut response = driver
        .prepare_question("question", &Default::default())
        .unwrap();
    let (sent, value) = tokio::join!(
        driver.send_prepared_approval(&mut response),
        read(&mut peer)
    );
    assert!(matches!(sent, Ok(PreparedUpdate::WriteAccepted(_))));
    assert_eq!(value["response"]["response"]["behavior"], "deny");
    assert_eq!(value["response"]["response"]["interrupt"], false);
    let (mut driver, mut peer) = running(8192, true).await;
    question(&mut driver, &mut peer, question_input()).await;
    let mut response = driver
        .prepare_question("question", &Default::default())
        .unwrap();
    event(&mut driver, &mut peer, cancel("question"))
        .await
        .unwrap();
    assert!(driver.send_prepared_approval(&mut response).await.is_err());
}
