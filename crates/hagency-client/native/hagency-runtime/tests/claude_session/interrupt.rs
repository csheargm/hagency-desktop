use super::*;
use hagency_runtime::claude::session::{
    ApprovalControlPolicy, InterruptUpdate, PermissionDecision, PreparedInterrupt,
};

async fn continuous() -> (Driver, Peer, Value) {
    let (mut driver, mut peer) = pair(8192, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    let prompt = super::continuous::next_prompt(&mut driver, &mut peer).await;
    driver.supervise_turn().unwrap();
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 30_000,
            response_reserve_ms: 1000,
        })
        .unwrap();
    (driver, peer, prompt)
}
async fn send(driver: &mut Driver, peer: &mut Peer, prepared: &mut PreparedInterrupt) {
    let (sent, request) = tokio::join!(driver.send_prepared_interrupt(prepared), read(peer));
    assert!(matches!(sent.unwrap(), InterruptUpdate::WriteAccepted(receipt) if receipt.flushed));
    assert_eq!(
        request,
        json!({"type":"control_request","request_id":prepared.request_id(),"request":{"subtype":"interrupt"}})
    );
    assert!(
        driver.interrupt_receipt().is_none(),
        "a pipe flush is not native acknowledgement"
    );
}
fn ack(id: &str) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":{"still_queued":[]}}})
}
fn terminal(prompt: &Value) -> Value {
    let mut value = result();
    value["user_message_uuid"] = prompt["uuid"].clone();
    value["is_error"] = json!(true);
    value["terminal_reason"] = json!("aborted_streaming");
    value
}

#[tokio::test]
async fn native_claude_interrupt_ack_and_completion_are_independent_and_scope_rotates() {
    for ack_first in [true, false] {
        let (mut driver, mut peer, prompt) = continuous().await;
        event(&mut driver, &mut peer, permission("owner-wait"))
            .await
            .unwrap();
        let mut prepared = driver.prepare_interrupt().unwrap();
        assert!(driver.prepare_interrupt().is_err());
        send(&mut driver, &mut peer, &mut prepared).await;
        assert!(driver.is_interrupting());
        if ack_first {
            event(&mut driver, &mut peer, ack(prepared.request_id()))
                .await
                .unwrap();
            assert_eq!(driver.phase(), Phase::Running);
            assert!(!driver.can_followup());
        }
        event(
            &mut driver,
            &mut peer,
            json!({"type":"control_cancel_request","request_id":"owner-wait"}),
        )
        .await
        .unwrap();
        event(&mut driver, &mut peer, terminal(&prompt))
            .await
            .unwrap();
        event(&mut driver, &mut peer, state("idle")).await.unwrap();
        if !ack_first {
            assert_eq!(
                driver.phase(),
                Phase::Running,
                "completion cannot discard an outstanding acknowledgement"
            );
            assert!(!driver.can_followup());
            event(&mut driver, &mut peer, ack(prepared.request_id()))
                .await
                .unwrap();
        }
        assert!(driver.interrupt_receipt().unwrap().accepted());
        assert_eq!(
            driver.interrupt_receipt().unwrap().still_queued(),
            Some([].as_slice())
        );
        assert_eq!(driver.phase(), Phase::ResultObserved);
        assert!(driver.can_followup());
        driver.begin_followup(limits()).unwrap();
        let next = super::continuous::next_prompt(&mut driver, &mut peer).await;
        assert_ne!(next["uuid"], prompt["uuid"]);
        assert!(driver.interrupt_receipt().is_none());
        assert!(!driver.is_interrupting());
        assert!(matches!(
            driver.send_prepared_interrupt(&mut prepared).await,
            Err(Error::Identity)
        ));
        assert_eq!(
            driver.phase(),
            Phase::Running,
            "stale request sends no bytes to the new turn"
        );
        let mut new = driver.prepare_interrupt().unwrap();
        assert_ne!(new.request_id(), prepared.request_id());
        send(&mut driver, &mut peer, &mut new).await;
    }
}

#[tokio::test]
async fn native_claude_interrupt_refusal_preserves_the_original_owner_wait() {
    let (mut driver, mut peer, _) = continuous().await;
    event(&mut driver, &mut peer, permission("owner-wait"))
        .await
        .unwrap();
    let mut prepared = driver.prepare_interrupt().unwrap();
    send(&mut driver, &mut peer, &mut prepared).await;
    event(&mut driver, &mut peer, json!({"type":"control_response","response":{"subtype":"error","request_id":prepared.request_id(),"error":"refused"}})).await.unwrap();
    assert!(!driver.interrupt_receipt().unwrap().accepted());
    assert!(!driver.is_interrupting());
    assert!(
        driver
            .prepare_approval("owner-wait", PermissionDecision::Deny)
            .is_ok()
    );
    assert_eq!(driver.phase(), Phase::Running);
    let mut retry = driver.prepare_interrupt().unwrap();
    assert_ne!(retry.request_id(), prepared.request_id());
    assert!(matches!(
        driver.send_prepared_interrupt(&mut prepared).await,
        Err(Error::Identity)
    ));
    send(&mut driver, &mut peer, &mut retry).await;
}

#[tokio::test]
async fn native_claude_interrupt_never_releases_a_prepared_tool_approval() {
    let (mut driver, mut peer, _) = continuous().await;
    event(&mut driver, &mut peer, permission("owner-wait"))
        .await
        .unwrap();
    let mut grant = driver
        .prepare_approval("owner-wait", PermissionDecision::Allow)
        .unwrap();
    let mut prepared = driver.prepare_interrupt().unwrap();
    send(&mut driver, &mut peer, &mut prepared).await;
    assert!(matches!(
        driver.send_prepared_approval(&mut grant).await,
        Err(Error::PermissionUnavailable)
    ));
    assert_eq!(driver.phase(), Phase::Closed);
    let mut extra = String::new();
    assert_eq!(peer.input.read_line(&mut extra).await.unwrap(), 0);
}

#[tokio::test]
async fn native_claude_interrupt_rejects_foreign_and_replayed_acknowledgements() {
    for replay in [false, true] {
        let (mut driver, mut peer, _) = continuous().await;
        let mut prepared = driver.prepare_interrupt().unwrap();
        send(&mut driver, &mut peer, &mut prepared).await;
        if replay {
            event(&mut driver, &mut peer, ack(prepared.request_id()))
                .await
                .unwrap();
        }
        let id = if replay {
            prepared.request_id()
        } else {
            "other-turn"
        };
        assert!(matches!(
            event(&mut driver, &mut peer, ack(id)).await,
            Err(Error::Identity)
        ));
        assert_eq!(driver.phase(), Phase::Closed);
    }
}

#[tokio::test]
async fn native_claude_interrupt_ack_before_the_request_is_written_is_rejected() {
    let (mut driver, mut peer, _) = continuous().await;
    let mut prepared = driver.prepare_interrupt().unwrap();
    peer.output
        .write_all(&encoded(ack(prepared.request_id())))
        .await
        .unwrap();
    assert!(matches!(
        driver.send_prepared_interrupt(&mut prepared).await,
        Err(Error::Identity)
    ));
    assert!(driver.interrupt_receipt().is_none());
    let mut extra = String::new();
    assert_eq!(
        peer.input.read_line(&mut extra).await.unwrap(),
        0,
        "unsolicited acknowledgement must not cause a write"
    );
}

#[tokio::test]
async fn native_claude_interrupt_completion_before_local_flush_keeps_the_original_writer() {
    use std::{
        pin::Pin,
        sync::{Arc, Mutex},
        task::{Context, Poll, Waker},
    };
    struct FlushGate {
        inner: DuplexStream,
        gate: Arc<Mutex<(bool, Option<Waker>)>>,
    }
    impl tokio::io::AsyncWrite for FlushGate {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.inner).poll_write(cx, bytes)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            {
                let mut gate = self.gate.lock().unwrap();
                if gate.0 {
                    gate.1 = Some(cx.waker().clone());
                    return Poll::Pending;
                }
            }
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }
    let (stdin, input) = tokio::io::duplex(8192);
    let (stdout, output) = tokio::io::duplex(8192);
    let (stderr, error) = tokio::io::duplex(8192);
    let gate = Arc::new(Mutex::new((false, None)));
    let mut driver = SessionDriver::new(
        stdout,
        FlushGate {
            inner: stdin,
            gate: gate.clone(),
        },
        stderr,
        limits(),
    )
    .unwrap();
    let mut peer = Peer {
        input: BufReader::new(input),
        output,
        stderr: error,
    };
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    let (ready, ()) = tokio::join!(driver.initialize(), async {
        let init = read(&mut peer).await;
        peer.output.write_all(&encoded(json!({"type":"control_response","response":{"subtype":"success","request_id":init["request_id"],"response":{}}}))).await.unwrap();
    });
    ready.unwrap();
    let (sent, prompt) = tokio::join!(driver.prompt("one turn"), read(&mut peer));
    sent.unwrap();
    let mut initialization = init();
    initialization["user_message_uuid"] = prompt["uuid"].clone();
    peer.output
        .write_all(&encoded(initialization))
        .await
        .unwrap();
    driver.next_message().await.unwrap();
    gate.lock().unwrap().0 = true;
    let mut prepared = driver.prepare_interrupt().unwrap();
    let id = prepared.request_id().to_owned();
    let (first, ()) = tokio::join!(driver.send_prepared_interrupt(&mut prepared), async {
        let request = read(&mut peer).await;
        assert_eq!(request["request_id"], id);
        let mut frames = encoded(ack(&id));
        frames.extend(encoded(terminal(&prompt)));
        frames.extend(encoded(state("idle")));
        peer.output.write_all(&frames).await.unwrap();
    });
    assert!(matches!(
        first.unwrap(),
        InterruptUpdate::Message(Message::ControlResponse { .. })
    ));
    assert!(driver.interrupt_receipt().unwrap().accepted());
    assert!(!driver.write_progress().unwrap().flushed);
    while driver.phase() != Phase::ResultObserved {
        match driver.send_prepared_interrupt(&mut prepared).await.unwrap() {
            InterruptUpdate::Message(_) => {}
            InterruptUpdate::WriteAccepted(_) => panic!("local flush is still paused"),
            InterruptUpdate::Unavailable => panic!("already written request must finish its flush"),
        }
    }
    let waiter = {
        let mut gate = gate.lock().unwrap();
        gate.0 = false;
        gate.1.take()
    };
    if let Some(waiter) = waiter {
        waiter.wake();
    }
    assert!(
        matches!(driver.send_prepared_interrupt(&mut prepared).await.unwrap(), InterruptUpdate::WriteAccepted(receipt) if receipt.flushed)
    );
    assert!(driver.can_followup());
}

#[tokio::test]
async fn native_claude_interrupted_queue_metadata_never_becomes_next_turn_authority() {
    for queued in [
        json!(["unexpected-command"]),
        json!("malformed"),
        json!([17]),
    ] {
        let (mut driver, mut peer, prompt) = continuous().await;
        let mut prepared = driver.prepare_interrupt().unwrap();
        send(&mut driver, &mut peer, &mut prepared).await;
        let mut response = ack(prepared.request_id());
        response["response"]["response"]["still_queued"] = queued.clone();
        let observed = event(&mut driver, &mut peer, response).await;
        if queued == json!(["unexpected-command"]) {
            observed.unwrap();
            event(&mut driver, &mut peer, terminal(&prompt))
                .await
                .unwrap();
            event(&mut driver, &mut peer, state("idle")).await.unwrap();
            assert_eq!(driver.phase(), Phase::ResultObserved);
            assert!(
                !driver.can_followup(),
                "host has not admitted the native queued command"
            );
        } else {
            assert!(matches!(observed, Err(Error::Malformed)));
            assert!(driver.interrupt_receipt().is_none());
            assert_eq!(driver.phase(), Phase::Closed);
        }
    }
}

#[tokio::test(start_paused = true)]
async fn native_claude_interrupt_partial_cancellation_preserves_uncertainty() {
    let (mut driver, mut peer) = pair(1, limits());
    initialize(&mut driver, &mut peer).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    let mut prepared = driver.prepare_interrupt().unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            driver.send_prepared_interrupt(&mut prepared)
        )
        .await
        .is_err()
    );
    assert_eq!(driver.phase(), Phase::Closed);
    assert!(driver.interrupt_receipt().is_none());
    let termination = driver.termination().unwrap();
    assert_eq!(termination.cause, Error::Cancelled);
    let bytes = termination.unconfirmed_write.unwrap();
    assert_eq!(bytes.accepted_bytes, 1);
    assert!(bytes.accepted_bytes < bytes.total_bytes && !bytes.flushed);
    assert!(driver.send_prepared_interrupt(&mut prepared).await.is_err());
}

#[tokio::test]
async fn native_claude_interrupt_is_not_sent_after_natural_completion() {
    let (mut driver, mut peer, prompt) = continuous().await;
    let mut prepared = driver.prepare_interrupt().unwrap();
    super::continuous::completed(&mut driver, &mut peer, &prompt).await;
    assert!(matches!(
        driver.send_prepared_interrupt(&mut prepared).await.unwrap(),
        InterruptUpdate::Unavailable
    ));
    assert!(!driver.is_interrupting());
    assert!(driver.can_followup());
    driver.begin_followup(limits()).unwrap();
    super::continuous::next_prompt(&mut driver, &mut peer).await; // Only the new prompt is on stdin.
}

#[tokio::test(start_paused = true)]
async fn native_claude_interrupt_ack_deadline_is_not_renewed_by_activity() {
    let (mut driver, mut peer, _) = continuous().await;
    let mut prepared = driver.prepare_interrupt().unwrap();
    send(&mut driver, &mut peer, &mut prepared).await;
    for _ in 0..9 {
        tokio::time::advance(Duration::from_secs(1)).await;
        event(&mut driver, &mut peer, state("running"))
            .await
            .unwrap();
    }
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(matches!(driver.next_message().await, Err(Error::Timeout)));
    assert!(driver.interrupt_receipt().is_none());
    assert_eq!(driver.phase(), Phase::Closed);
}

#[tokio::test]
#[ignore = "explicit installed Claude; isolated localhost model, no provider account or paid request"]
async fn installed_claude_interrupts_a_pending_question_without_answering_or_replaying() {
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    let fixture = root.path().join("fixture.py");
    std::fs::write(&fixture, include_str!("../claude_question_fixture.py")).unwrap();
    let mut server = tokio::process::Command::new("python3")
        .arg(&fixture)
        .arg(root.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut address = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut address)
        .await
        .unwrap();
    let executable =
        std::env::var_os("HAGENCY_TEST_CLAUDE_EXECUTABLE").expect("explicit installed Claude");
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
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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
        .prompt("Ask the offline questions and wait for my answer.")
        .await
        .unwrap();
    driver.next_message().await.unwrap();
    driver.supervise_turn().unwrap();
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 20_000,
            response_reserve_ms: 2000,
        })
        .unwrap();
    let mut question = None;
    for _ in 0..128 {
        if let Message::Permission {
            request_id,
            tool_name,
            ..
        } = driver.next_message().await.unwrap()
        {
            assert_eq!(tool_name, "AskUserQuestion");
            question = Some(request_id);
            break;
        }
    }
    assert!(question.is_some());
    let mut interrupt = driver.prepare_interrupt().unwrap();
    let mut messages = Vec::new();
    loop {
        match driver
            .send_prepared_interrupt(&mut interrupt)
            .await
            .unwrap()
        {
            InterruptUpdate::Message(message) => messages.push(message),
            InterruptUpdate::WriteAccepted(_) => break,
            InterruptUpdate::Unavailable => panic!("question still awaiting an answer"),
        }
    }
    for _ in 0..128 {
        if driver.phase() == Phase::ResultObserved {
            break;
        }
        messages.push(driver.next_message().await.unwrap());
    }
    assert_eq!(driver.phase(), Phase::ResultObserved);
    assert!(driver.interrupt_receipt().unwrap().accepted());
    assert_eq!(
        messages
            .iter()
            .filter(|message| matches!(
                message,
                Message::Event {
                    kind: EventKind::Result,
                    ..
                }
            ))
            .count(),
        1
    );
    assert!(messages.iter().any(|message| matches!(message, Message::ControlCancel { request_id } if Some(request_id) == question.as_ref())));
    assert!(
        !root.path().join("native-answer.json").exists(),
        "no fabricated question answer or second inference"
    );
    assert!(
        driver.can_followup(),
        "the native stop must leave the retained conversation reusable"
    );
    driver.close();
    child.kill().await.unwrap();
    server.kill().await.unwrap();
}
