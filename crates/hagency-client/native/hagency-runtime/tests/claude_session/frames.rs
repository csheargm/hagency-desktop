//! F8: frames from a newer native CLI that this host can ignore or refuse
//! without ending the session, and the ones that stay fatal on purpose.
use super::*;

fn unknown_request(id: &str) -> Value {
    json!({"type":"control_request","request_id":id,
        "request":{"subtype":"future_dialog","detail":{"private":"sentinel"}}})
}
fn assistant() -> Value {
    json!({"type":"assistant","session_id":"session-one","message":{"content":[]}})
}
/// The next host frame, or `None` once the host closed its side.
async fn host_frame(peer: &mut Peer) -> Option<Value> {
    let mut line = String::new();
    match tokio::time::timeout(Duration::from_secs(5), peer.input.read_line(&mut line)).await {
        Ok(Ok(read)) if read > 0 => serde_json::from_str(&line).ok(),
        _ => None,
    }
}
/// The one reply an unknown native request gets: an error, never a decision.
fn assert_refusal(reply: &Value, id: &str) {
    assert_eq!(reply["type"], "control_response");
    assert_eq!(reply["response"]["subtype"], "error");
    assert_eq!(reply["response"]["request_id"], id);
    assert!(reply["response"]["error"].is_string());
    assert!(reply["response"].get("response").is_none());
    assert!(!reply.to_string().contains("sentinel"));
}

#[tokio::test]
async fn native_claude_unknown_control_request_is_refused_and_the_turn_continues() {
    let (mut driver, mut peer) = running().await;
    // The native peer waits for its answer before it continues the turn.
    let (message, reply) = tokio::join!(driver.next_message(), async {
        peer.output
            .write_all(&encoded(unknown_request("future-1")))
            .await
            .unwrap();
        let reply = host_frame(&mut peer).await;
        let _ = peer.output.write_all(&encoded(assistant())).await;
        reply
    });
    assert!(
        matches!(
            message,
            Ok(Message::Event {
                kind: EventKind::Assistant,
                ..
            })
        ),
        "an unknown control request ended the turn: {:?}",
        message.err()
    );
    assert_refusal(&reply.expect("the unknown request is answered"), "future-1");
    assert_eq!(driver.phase(), Phase::Running);
    assert!(driver.termination().is_none());
    // It was never a host message: the stream sequence counts init and the
    // assistant frame only.
    assert_eq!(driver.last_observation().unwrap().sequence(), 2);

    // The native peer may withdraw a request that this host only refused.
    let mut bytes = encoded(json!({"type":"control_cancel_request","request_id":"future-1"}));
    bytes.extend(encoded(assistant()));
    let (message, written) = tokio::join!(driver.next_message(), peer.output.write_all(&bytes));
    written.unwrap();
    assert!(matches!(
        message,
        Ok(Message::Event {
            kind: EventKind::Assistant,
            ..
        })
    ));
    // A real permission request is still presented, and an unknown identity
    // is still refused by closing the stream.
    assert!(matches!(
        event(&mut driver, &mut peer, permission("permission-one"))
            .await
            .unwrap(),
        Message::Permission { .. }
    ));
    assert!(matches!(
        event(
            &mut driver,
            &mut peer,
            json!({"type":"control_cancel_request","request_id":"never-seen"})
        )
        .await,
        Err(Error::Identity)
    ));
    assert_eq!(driver.phase(), Phase::Closed);
}

#[tokio::test]
async fn native_claude_unknown_control_request_before_the_initialize_response_is_refused() {
    let (mut driver, mut peer) = pair(4096, limits());
    let (result, reply) = tokio::join!(driver.initialize(), async {
        let request = read(&mut peer).await;
        peer.output
            .write_all(&encoded(unknown_request("future-start")))
            .await
            .unwrap();
        let reply = host_frame(&mut peer).await;
        let _ = peer
            .output
            .write_all(&encoded(json!({"type":"control_response","response":{
            "subtype":"success","request_id":request["request_id"],"response":{"claude_code_version":"2.1.321"}}})))
            .await;
        reply
    });
    assert_eq!(
        result,
        Ok(()),
        "an unknown control request ended the session before its first input"
    );
    assert_refusal(&reply.expect("the unknown request is answered"), "future-start");
    assert_eq!(driver.phase(), Phase::Ready);
    assert!(driver.session_id().is_none());
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    assert_eq!(driver.last_observation().unwrap().sequence(), 1);
}

/// A peer that asks faster than it reads its answers is not served without bound.
#[tokio::test]
async fn native_claude_unanswerable_flood_of_unknown_requests_is_bounded() {
    let (mut driver, mut peer) = pair(128, limits());
    initialize(&mut driver, &mut peer).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    let mut bytes = Vec::new();
    for n in 0..64 {
        bytes.extend(encoded(unknown_request(&format!("flood-{n}"))));
    }
    // The peer never reads its stdin, so no answer can leave.
    let (message, _) = tokio::join!(driver.next_message(), async {
        let _ = peer.output.write_all(&bytes).await;
    });
    assert!(
        matches!(message, Err(Error::Capacity)),
        "{:?}",
        message.err()
    );
    assert_eq!(driver.phase(), Phase::Closed);
}

#[tokio::test]
async fn native_claude_control_frames_with_extra_keys_keep_the_session() {
    // The initialize acknowledgement of a newer CLI.
    let (mut driver, mut peer) = pair(4096, limits());
    let (result, ()) = tokio::join!(driver.initialize(), async {
        let request = read(&mut peer).await;
        peer.output
            .write_all(&encoded(json!({"type":"control_response","uuid":"u-1","response":{
            "subtype":"success","request_id":request["request_id"],"future_note":{"x":1},
            "pending_permission_requests":[],"pending_user_dialog_requests":[],
            "response":{"claude_code_version":"2.1.321"}}})))
            .await
            .unwrap();
    });
    assert_eq!(
        result,
        Ok(()),
        "extra keys on the initialize response ended the session"
    );
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    // A permission request and its withdrawal, each with an extra envelope key.
    let mut request = permission("permission-one");
    request["uuid"] = json!("u-2");
    request["session_id"] = json!("session-one");
    let admitted = event(&mut driver, &mut peer, request).await;
    assert!(
        matches!(
            &admitted,
            Ok(Message::Permission { request_id, tool_name, input, .. })
                if request_id == "permission-one" && tool_name == "Bash"
                    && input == &json!({"command":"private sentinel"})
        ),
        "extra keys on a permission request ended the session: {:?}",
        admitted.err()
    );
    event(
        &mut driver,
        &mut peer,
        json!({"type":"control_cancel_request","request_id":"permission-one","reason":"future"}),
    )
    .await
    .unwrap();
    assert_eq!(driver.phase(), Phase::Running);

    // A refusal with extra keys is still exactly a refusal.
    let (mut driver, mut peer) = pair(4096, limits());
    let (result, ()) = tokio::join!(driver.initialize(), async {
        let request = read(&mut peer).await;
        peer.output
            .write_all(&encoded(json!({"type":"control_response","response":{
            "subtype":"error","request_id":request["request_id"],"error":"private error",
            "pending_permission_requests":[],"code":7}})))
            .await
            .unwrap();
    });
    assert_eq!(result, Err(Error::Refused));
    assert_eq!(driver.phase(), Phase::Closed);
}

#[tokio::test]
async fn native_claude_unknown_session_state_is_not_idle_and_keeps_the_turn() {
    for unknown_first in [false, true] {
        let (mut driver, mut peer) = pair(4096, limits());
        driver.enable_native_completion().unwrap();
        initialize(&mut driver, &mut peer).await;
        prompt(&mut driver, &mut peer).await;
        event(&mut driver, &mut peer, init()).await.unwrap();
        if unknown_first {
            // An earlier idle is no longer proven once the state changes again.
            event(&mut driver, &mut peer, state("idle")).await.unwrap();
        } else {
            event(&mut driver, &mut peer, result()).await.unwrap();
        }
        let observed = event(&mut driver, &mut peer, state("compacting")).await;
        assert!(
            observed.is_ok(),
            "an unknown session state ended the session: {:?}",
            observed.err()
        );
        if unknown_first {
            event(&mut driver, &mut peer, result()).await.unwrap();
        }
        // Only native idle completes the turn.
        assert_eq!(driver.phase(), Phase::Running);
        assert!(driver.completion_receipt().is_err());
        event(&mut driver, &mut peer, state("idle")).await.unwrap();
        assert_eq!(driver.phase(), Phase::ResultObserved);
        assert!(driver.completion_receipt().is_ok());
    }
    // A state frame without a readable state is not idle either.
    let (mut driver, mut peer) = pair(4096, limits());
    driver.enable_native_completion().unwrap();
    initialize(&mut driver, &mut peer).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    event(&mut driver, &mut peer, result()).await.unwrap();
    let mut unreadable = state("idle");
    unreadable["state"] = json!({"name":"idle"});
    assert!(event(&mut driver, &mut peer, unreadable).await.is_ok());
    assert_eq!(driver.phase(), Phase::Running);
}

#[tokio::test(start_paused = true)]
async fn native_claude_inert_metadata_before_the_initialize_response_is_ignored() {
    let (mut driver, mut peer) = pair(4096, limits());
    let (result, ()) = tokio::join!(driver.initialize(), async {
        let request = read(&mut peer).await;
        let mut bytes = encoded(native_ui_invalidate());
        for frame in [
            json!({"type":"auth_status","session_id":"startup","isAuthenticating":false,"output":[]}),
            json!({"type":"rate_limit_event","session_id":"startup","rate_limit_info":{}}),
            state("idle"),
        ] {
            bytes.extend(encoded(frame));
        }
        bytes.extend(encoded(json!({"type":"control_response","response":{
            "subtype":"success","request_id":request["request_id"],
            "response":{"claude_code_version":"2.1.321"}}})));
        peer.output.write_all(&bytes).await.unwrap();
    });
    assert_eq!(
        result,
        Ok(()),
        "inert metadata before the initialize response ended the session"
    );
    assert_eq!(driver.phase(), Phase::Ready);
    assert_eq!(driver.ignored_frames(), 4);
    // None of it bound a session or became an observation.
    assert!(driver.session_id().is_none());
    assert!(driver.observation_source().is_err());
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    assert_eq!(driver.session_id(), Some("session-one"));
    assert_eq!(driver.last_observation().unwrap().sequence(), 1);

    // Unchanged on purpose: activity, identity and authority frames before
    // the initialize response still end the session.
    for early in [
        state("running"),
        state("requires_action"),
        json!({"type":"command_lifecycle","command_uuid":"command-one","state":"started","session_id":"session-one"}),
        json!({"type":"system","subtype":"status","session_id":"session-one","status":"requesting"}),
        json!({"type":"system","subtype":"background_tasks_changed","session_id":"session-one","tasks":[]}),
        json!({"type":"tool_progress","session_id":"session-one"}),
        assistant(),
        super::result(),
        init(),
        permission("early"),
        json!({"type":"control_cancel_request","request_id":"early"}),
    ] {
        let (mut driver, mut peer) = pair(4096, limits());
        let (result, ()) = tokio::join!(driver.initialize(), async {
            read(&mut peer).await;
            peer.output.write_all(&encoded(early.clone())).await.unwrap();
        });
        assert_eq!(result, Err(Error::Identity), "{early}");
        assert_eq!(driver.phase(), Phase::Closed);
    }
    // Inert metadata neither extends the one response deadline nor is unbounded.
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
                    .write_all(&encoded(state("idle")))
                    .await
                    .unwrap();
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
    );
    assert_eq!(result, Err(Error::Timeout));
    assert_eq!(elapsed, Duration::from_millis(limits().event_wait_ms));
    let (mut driver, mut peer) = pair(64 * 1024, limits());
    let (result, ()) = tokio::join!(driver.initialize(), async {
        read(&mut peer).await;
        let bytes = encoded(state("idle")).repeat(64);
        peer.output.write_all(&bytes).await.unwrap();
    });
    assert_eq!(result, Err(Error::Capacity));
}
