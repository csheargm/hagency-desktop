use hagency_runtime::claude::{Message, session::*};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
type Driver = SessionDriver<DuplexStream, DuplexStream, DuplexStream>;
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
    _stderr: DuplexStream,
    prompt: Value,
}
fn encoded(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}
async fn read(p: &mut Peer) -> Value {
    let mut line = String::new();
    p.input.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}
async fn event(d: &mut Driver, p: &mut Peer, v: Value) -> Message {
    let bytes = encoded(v);
    let (result, write) = tokio::join!(d.next_message(), p.output.write_all(&bytes));
    write.unwrap();
    result.unwrap()
}
async fn running() -> (Driver, Peer) {
    running_with_completion(false).await
}
async fn running_with_completion(completion: bool) -> (Driver, Peer) {
    let (stdin, input) = tokio::io::duplex(8192);
    let (stdout, output) = tokio::io::duplex(8192);
    let (stderr, error) = tokio::io::duplex(1024);
    let mut d = Driver::new(
        stdout,
        stdin,
        stderr,
        Limits {
            write_timeout_ms: 1000,
            event_wait_ms: 2000,
            lifetime_ms: 60_000,
        },
    )
    .unwrap();
    if completion {
        d.enable_native_completion().unwrap();
        d.enable_continuous_session().unwrap();
    }
    let mut p = Peer {
        input: BufReader::new(input),
        output,
        _stderr: error,
        prompt: Value::Null,
    };
    assert!(d.observation_source().is_err());
    let (result, ()) = tokio::join!(d.initialize(), async {
        let v = read(&mut p).await;
        p.output
            .write_all(&encoded(json!({"type":"control_response","response":{
            "subtype":"success","request_id":v["request_id"],"response":{"claude_code_version":"2.1.321"}}})))
            .await
            .unwrap();
    });
    result.unwrap();
    let (result, prompt) = tokio::join!(d.prompt("offline usage"), read(&mut p));
    result.unwrap();
    p.prompt = prompt.clone();
    assert!(d.observation_source().is_err());
    event(
        &mut d,
        &mut p,
        json!({"type":"system","subtype":"init","session_id":"same-session", "user_message_uuids": if completion { json!([prompt["uuid"]]) } else { Value::Null }}),
    )
    .await;
    (d, p)
}
fn step(id: &str) -> Value {
    json!({"type":"assistant","session_id":"same-session","uuid":"outer-uuid","parent_tool_use_id":null,
    "message":{"id":id,"usage":{"input_tokens":10,"output_tokens":999,"cache_read_input_tokens":20,"cache_creation_input_tokens":30}}})
}
fn result() -> Value {
    json!({"type":"result","session_id":"same-session","subtype":"success","is_error":true,
    "usage":{"input_tokens":9999,"output_tokens":9999},"uuid":"native-result-one","model_usage_since_last_result":{
        "model-private-one":{"inputTokens":100,"outputTokens":200,"cacheReadInputTokens":300,"cacheCreationInputTokens":400},
        "model-private-two":{"inputTokens":1,"outputTokens":2,"cacheReadInputTokens":3,"cacheCreationInputTokens":4}}})
}
fn usage(d: &Driver) -> &UsageEvidence {
    match d.last_observation().unwrap().kind() {
        ObservationKind::Usage(e) | ObservationKind::Result { usage: e, .. } => e,
        _ => panic!("expected bounded usage evidence"),
    }
}
fn counts(e: &UsageEvidence) -> [Option<u64>; 4] {
    let c = e.counts();
    [c.input(), c.output(), c.cache_read(), c.cache_write()]
}

#[tokio::test]
async fn native_claude_usage_source_and_order() {
    let (mut d, mut p) = running().await;
    let source = d.observation_source().unwrap();
    assert!(d.matches_observation_source(&source));
    let (other, _peer) = running().await;
    assert!(source != other.observation_source().unwrap());
    assert!(!other.matches_observation_source(&source));
    let mut raw = event(&mut d, &mut p, step("first")).await;
    if let Message::Event { payload, .. } = &mut raw {
        payload["message"]["usage"]["input_tokens"] = json!(999);
    }
    let original = d.last_observation().unwrap().clone();
    assert!(original.source() == &source);
    assert_eq!(original.sequence(), 2);
    assert_eq!(counts(usage(&d))[0], Some(10));
    assert!(d.observation_source().is_err());
    event(&mut d, &mut p, step("second")).await;
    assert_eq!(d.last_observation().unwrap().sequence(), 3);
    d.close();
    assert!(source.is_retired());
    assert!(original.source().is_retired());
    let dropped = other.observation_source().unwrap();
    drop(other);
    assert!(dropped.is_retired());
    let (mut d, mut p) = running().await;
    let source = d.observation_source().unwrap();
    p.output
        .write_all(&encoded(
            json!({"type":"assistant","session_id":"wrong","message":{}}),
        ))
        .await
        .unwrap();
    assert!(matches!(d.next_message().await, Err(Error::Identity)));
    assert!(source.is_retired());
    let (mut d, _p) = running().await;
    let source = d.observation_source().unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(10), d.next_message())
            .await
            .is_err()
    );
    assert_eq!(d.termination().unwrap().cause, Error::Cancelled);
    assert!(source.is_retired());

    let (mut d, mut p) = running().await;
    d.enable_approval_control(ApprovalControlPolicy {
        owner_wait_ms: 10_000,
        response_reserve_ms: 1000,
    })
    .unwrap();
    event(
        &mut d,
        &mut p,
        json!({"type":"control_request","request_id":"permission","request":{
        "subtype":"can_use_tool","tool_name":"Bash","input":{"command":"offline"}}}),
    )
    .await;
    assert_eq!(d.last_observation().unwrap().sequence(), 2);
    let mut prepared = d
        .prepare_approval("permission", PermissionDecision::Deny)
        .unwrap();
    p.output
        .write_all(&encoded(step("during-write")))
        .await
        .unwrap();
    assert!(matches!(
        d.send_prepared_approval(&mut prepared).await.unwrap(),
        PreparedUpdate::Message(_)
    ));
    assert_eq!(d.last_observation().unwrap().sequence(), 3);
    assert!(matches!(
        d.send_prepared_approval(&mut prepared).await.unwrap(),
        PreparedUpdate::WriteAccepted(_)
    ));
    read(&mut p).await;
    assert_eq!(d.last_observation().unwrap().sequence(), 3);
    let ready = std::future::ready(());
    tokio::pin!(ready);
    assert!(matches!(
        d.next_or_control(ready.as_mut()).await.unwrap(),
        ControlUpdate::Control(())
    ));
    assert_eq!(d.last_observation().unwrap().sequence(), 3);
    let pending = std::future::pending::<()>();
    tokio::pin!(pending);
    p.output
        .write_all(&encoded(step("during-control")))
        .await
        .unwrap();
    assert!(matches!(
        d.next_or_control(pending.as_mut()).await.unwrap(),
        ControlUpdate::Message(_)
    ));
    assert_eq!(d.last_observation().unwrap().sequence(), 4);
}

#[tokio::test]
async fn native_claude_usage_projection() {
    let (mut d, mut p) = running().await;
    let source = d.observation_source().unwrap();
    event(&mut d, &mut p, step("one")).await;
    assert_eq!(counts(usage(&d)), [Some(10), None, Some(20), Some(30)]);
    assert_eq!(usage(&d).steps(), 1);
    let mut duplicate = step("one");
    duplicate["uuid"] = json!("different-outer");
    duplicate["message"]["usage"]["output_tokens"] = json!(123456);
    event(&mut d, &mut p, duplicate).await;
    assert!(matches!(
        d.last_observation().unwrap().kind(),
        ObservationKind::Ignored
    ));
    let mut child = step("child");
    child["parent_tool_use_id"] = json!("parent");
    event(&mut d, &mut p, child).await;
    assert!(matches!(
        d.last_observation().unwrap().kind(),
        ObservationKind::Ignored
    ));
    event(&mut d, &mut p, step("two")).await;
    assert_eq!(counts(usage(&d)), [Some(20), None, Some(40), Some(60)]);
    event(&mut d, &mut p, result()).await;
    assert!(matches!(
        d.last_observation().unwrap().kind(),
        ObservationKind::Result { is_error: true, .. }
    ));
    assert_eq!(
        counts(usage(&d)),
        [Some(101), Some(202), Some(303), Some(404)]
    );
    assert!(usage(&d).coverage() == UsageCoverage::ReportedModelsResult);
    assert_eq!(usage(&d).models(), 2);
    assert_eq!(usage(&d).steps(), 2);
    assert!(!source.is_retired());
    assert_eq!(d.phase(), Phase::ResultObserved);
    for model in [
        None,
        Some(Value::Null),
        Some(json!({})),
        Some(json!("invalid")),
    ] {
        let (mut d, mut p) = running().await;
        let mut v = result();
        v.as_object_mut()
            .unwrap()
            .remove("model_usage_since_last_result");
        if let Some(ref model) = model {
            v["model_usage_since_last_result"] = model.clone();
        }
        event(&mut d, &mut p, v).await;
        if model.is_none() {
            assert!(usage(&d).coverage() == UsageCoverage::MainLoopResult);
            assert_eq!(counts(usage(&d)), [Some(9999), Some(9999), None, None]);
        } else if model
            .as_ref()
            .is_some_and(|v| v.as_object().is_some_and(|m| m.is_empty()))
        {
            assert_eq!(counts(usage(&d)), [Some(0); 4]);
        } else {
            assert!(matches!(
                d.last_observation().unwrap().kind(),
                ObservationKind::Invalidated
            ));
        }
    }
}

#[tokio::test]
async fn native_claude_usage_bounds() {
    for bad in [
        json!(-1),
        json!(1.5),
        json!("42"),
        json!(true),
        json!(9_007_199_254_740_992u64),
        Value::Null,
    ] {
        let (mut d, mut p) = running().await;
        let mut v = step("invalid-field");
        v["message"]["usage"]["input_tokens"] = bad.clone();
        event(&mut d, &mut p, v).await;
        assert_eq!(counts(usage(&d)), [None, None, Some(20), Some(30)]);
        assert_eq!(usage(&d).diagnostics().has_invalid_fields(), !bad.is_null());
        event(&mut d, &mut p, step("later")).await;
        assert_eq!(counts(usage(&d)), [None, None, Some(40), Some(60)]);
    }
    for mode in ["conflict", "missing-id", "long-id", "overflow", "models"] {
        let (mut d, mut p) = running().await;
        match mode {
            "conflict" => {
                event(&mut d, &mut p, step("same")).await;
                let mut v = step("same");
                v["message"]["usage"]["input_tokens"] = json!(11);
                event(&mut d, &mut p, v).await;
            }
            "missing-id" => {
                let mut v = step("id");
                v["message"].as_object_mut().unwrap().remove("id");
                event(&mut d, &mut p, v).await;
            }
            "long-id" => {
                event(&mut d, &mut p, step(&"x".repeat(513))).await;
            }
            "overflow" => {
                let mut v = step("max");
                v["message"]["usage"] = json!({"input_tokens":9_007_199_254_740_991u64,"cache_creation_input_tokens":1});
                event(&mut d, &mut p, v).await;
            }
            "models" => {
                let mut v = result();
                v["model_usage_since_last_result"] =
                    Value::Object((0..65).map(|n| (n.to_string(), json!({}))).collect());
                event(&mut d, &mut p, v).await;
            }
            _ => unreachable!(),
        }
        assert!(
            matches!(
                d.last_observation().unwrap().kind(),
                ObservationKind::Invalidated
            ),
            "{mode}"
        );
        if d.phase() == Phase::Running {
            event(&mut d, &mut p, result()).await;
            assert!(matches!(
                d.last_observation().unwrap().kind(),
                ObservationKind::Invalidated
            ));
        }
    }
    // Missing earlier input does not hide subsequent known same-category overflow.
    let (mut d, mut p) = running().await;
    for (id, input) in [
        ("unknown", Value::Null),
        ("max", json!(9_007_199_254_740_991u64)),
        ("overflow", json!(1)),
    ] {
        let mut v = step(id);
        v["message"]["usage"] = json!({"input_tokens":input,"cache_read_input_tokens":0,"cache_creation_input_tokens":0});
        event(&mut d, &mut p, v).await;
    }
    assert!(matches!(
        d.last_observation().unwrap().kind(),
        ObservationKind::Invalidated
    ));
}

#[tokio::test]
async fn native_claude_long_stream_keeps_source_and_sequence() {
    let (mut d, mut p) = running().await;
    let source = d.observation_source().unwrap();
    for _ in 0..40_000 {
        event(
            &mut d,
            &mut p,
            json!({"type":"system","subtype":"status","session_id":"same-session"}),
        )
        .await;
    }
    assert_eq!(d.last_observation().unwrap().sequence(), 40_001);
    assert!(!source.is_retired());
    assert!(d.matches_observation_source(&source));
    event(&mut d, &mut p, result()).await;
    assert_eq!(d.last_observation().unwrap().sequence(), 40_002);
    assert_eq!(d.phase(), Phase::ResultObserved);
    assert_eq!(
        counts(usage(&d)),
        [Some(101), Some(202), Some(303), Some(404)]
    );
}

#[tokio::test]
async fn native_claude_partial_cache_saturation_keeps_final_usage_and_duplicate_checks() {
    for finish in ["reported", "missing", "conflict"] {
        let (mut d, mut p) = running().await;
        for n in 0..1024 {
            event(&mut d, &mut p, step(&n.to_string())).await;
        }
        assert_eq!(usage(&d).steps(), 1024);
        assert_eq!(
            counts(usage(&d)),
            [Some(10_240), None, Some(20_480), Some(30_720)]
        );
        for n in 1024..4096 {
            event(&mut d, &mut p, step(&n.to_string())).await;
            assert!(matches!(
                d.last_observation().unwrap().kind(),
                ObservationKind::Ignored
            ));
        }
        // Never evict original IDs and count them as fresh usage again.
        event(&mut d, &mut p, step("0")).await;
        assert!(matches!(
            d.last_observation().unwrap().kind(),
            ObservationKind::Ignored
        ));
        if finish == "conflict" {
            let mut conflicting = step("0");
            conflicting["message"]["usage"]["input_tokens"] = json!(11);
            event(&mut d, &mut p, conflicting).await;
            event(&mut d, &mut p, result()).await;
            assert!(matches!(
                d.last_observation().unwrap().kind(),
                ObservationKind::Invalidated
            ));
            continue;
        }
        let mut final_result = result();
        final_result["model_usage_since_last_result"] = if finish == "reported" {
            json!({"main":{"inputTokens":40_960,"outputTokens":200,
                "cacheReadInputTokens":81_920,"cacheCreationInputTokens":122_880}})
        } else {
            json!({"main":{"inputTokens":40_960}})
        };
        event(&mut d, &mut p, final_result).await;
        let evidence = usage(&d);
        assert_eq!(evidence.steps(), 1024, "bounded partial cache is retained");
        assert!(evidence.coverage() == UsageCoverage::ReportedModelsResult);
        assert_eq!(
            evidence.diagnostics().has_missing_fields(),
            finish == "missing"
        );
        assert_eq!(
            counts(evidence),
            if finish == "reported" {
                [Some(40_960), Some(200), Some(81_920), Some(122_880)]
            } else {
                [Some(40_960), None, None, None]
            }
        );
    }
}

#[tokio::test]
async fn native_claude_real_model_metadata_does_not_double_count_or_hide_usage() {
    let (mut d, mut p) = running().await;
    let mut frame = result();
    frame["modelUsage"] = json!({"claude-sonnet-4-6": {
        "inputTokens":17, "outputTokens":9, "cacheReadInputTokens":3,
        "cacheCreationInputTokens":7, "webSearchRequests":0, "costUSD":0.001,
        "contextWindow":200000, "maxOutputTokens":64000, "thinkingTokens":4,
        "cacheCreationEphemeral5mInputTokens":5, "cacheCreationEphemeral1hInputTokens":2,
        "canonicalModel":"claude-sonnet-4-6", "provider":"firstParty", "costBasis":"synthetic",
        "futureMetadata":{"nested":["ignored", true]}
    }});
    frame["model_usage_since_last_result"] = frame["modelUsage"].clone();
    event(&mut d, &mut p, frame).await;
    let evidence = usage(&d);
    assert_eq!(counts(evidence), [Some(17), Some(9), Some(3), Some(7)]);
    assert!(!evidence.diagnostics().has_missing_fields());
    assert!(!evidence.diagnostics().has_invalid_fields());
    assert!(!evidence.diagnostics().has_unsupported_fields());
    assert_eq!(counts(evidence).into_iter().flatten().sum::<u64>(), 36);
}

/// Captured from installed Claude 2.1.321 with a localhost-only model: the
/// original result precedes the child, which triggers another native main loop.
#[tokio::test]
async fn native_claude_background_result_binds_original_task_and_counts_all_deltas() {
    let captured: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/claude-native-background.json")).unwrap();
    for fault in [
        None,
        Some("run"),
        Some("producer"),
        Some("notification"),
        Some("replay"),
    ] {
        let (mut d, mut p) = running_with_completion(true).await;
        let source = d.observation_source().unwrap();
        let mut rejected = false;
        let mut final_usage = None;
        for original in &captured {
            let mut frame = original.clone();
            frame["session_id"] = json!("same-session");
            if frame["type"] == "result" && frame["origin"].is_null() {
                // Bind the captured primary result to this test's real prompt.
                frame["user_message_uuid"] = p.prompt["uuid"].clone();
                frame["user_message_uuids"] = json!([p.prompt["uuid"]]);
            }
            if fault == Some("notification") && frame["subtype"] == "task_notification" {
                frame["run_id"] = json!("unstarted-native-run");
            }
            if frame["type"] == "result" && !frame["origin"].is_null() {
                match fault {
                    Some("run") => frame["origin"]["runId"] = json!("unstarted-native-run"),
                    Some("producer") => frame["origin"]["producer"] = json!("foreign-producer"),
                    _ => {}
                }
            }
            let bytes = encoded(frame);
            let (message, wrote) = tokio::join!(d.next_message(), p.output.write_all(&bytes));
            wrote.unwrap();
            if matches!(message, Err(Error::Identity)) {
                assert!(fault.is_some());
                assert!(source.is_retired());
                rejected = true;
                break;
            }
            message.unwrap();
            if let ObservationKind::Result { usage, .. } = d.last_observation().unwrap().kind() {
                final_usage = Some(usage.clone());
            }
            if fault == Some("replay")
                && original["type"] == "result"
                && !original["origin"].is_null()
            {
                let bytes =
                    encoded(json!({"type":"system","subtype":"init","session_id":"same-session"}));
                let (message, wrote) = tokio::join!(d.next_message(), p.output.write_all(&bytes));
                wrote.unwrap();
                assert!(matches!(message, Err(Error::Identity)));
                rejected = true;
                break;
            }
        }
        if fault.is_some() {
            assert!(rejected);
            continue;
        }
        assert_eq!(d.phase(), Phase::ResultObserved);
        assert!(!d.needs_usage_snapshot());
        let receipt = d.completion_receipt().unwrap();
        assert!(receipt.matches_text("MAIN_ANSWER_PRESERVED"));
        // Three main requests and four child requests. Cached and thinking
        // metadata are subsets, not extra charges on top of these totals.
        assert_eq!(d.last_observation().unwrap().sequence(), 9);
        assert_eq!(
            counts(&final_usage.unwrap()),
            [Some(215), Some(79), Some(29), Some(65)]
        );
        assert!(d.can_followup());
    }
}

#[tokio::test]
async fn native_claude_notified_children_hold_completion_until_their_bound_replies() {
    let captured: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/claude-native-background.json")).unwrap();
    let original = captured.iter().find(|v| v["type"] == "result").unwrap();
    let followup = captured
        .iter()
        .find(|v| v["type"] == "result" && !v["origin"].is_null())
        .unwrap();
    for final_error in [false, true] {
        let (mut d, mut p) = running_with_completion(true).await;
        event(&mut d, &mut p, json!({"type":"system","subtype":"background_tasks_changed","session_id":"same-session",
            "tasks":[{"task_id":"child-a","run_id":"run-a"},{"task_id":"child-b","run_id":"run-b"}]})).await;
        let mut primary = original.clone();
        primary["session_id"] = json!("same-session");
        primary["user_message_uuid"] = p.prompt["uuid"].clone();
        primary["user_message_uuids"] = json!([p.prompt["uuid"]]);
        primary["is_error"] = json!(!final_error);
        event(&mut d, &mut p, primary).await;
        let idle = json!({"type":"system","subtype":"session_state_changed","state":"idle","session_id":"same-session"});
        event(&mut d, &mut p, idle.clone()).await;
        for (task, run) in [("child-a", "run-a"), ("child-b", "run-b")] {
            event(
                &mut d,
                &mut p,
                json!({"type":"system","subtype":"task_notification","session_id":"same-session",
                "task_id":task,"run_id":run,"status":"completed"}),
            )
            .await;
        }
        event(&mut d, &mut p, json!({"type":"system","subtype":"background_tasks_changed","session_id":"same-session","tasks":[]})).await;
        assert_eq!(
            d.phase(),
            Phase::Running,
            "idle and an empty live set do not erase queued native replies"
        );
        let mut final_usage = None;
        for (task, run) in [("child-a", "run-a"), ("child-b", "run-b")] {
            event(
                &mut d,
                &mut p,
                json!({"type":"system","subtype":"init","session_id":"same-session"}),
            )
            .await;
            let mut reply = followup.clone();
            reply["session_id"] = json!("same-session");
            reply["uuid"] = json!(format!("reply-{run}"));
            reply["origin"]["runId"] = json!(run);
            reply["is_error"] = json!(final_error);
            reply["result"] = json!("final background reply");
            event(&mut d, &mut p, reply).await;
            final_usage = Some(usage(&d).clone());
            // A repeated notification cannot create a new result claim or
            // force an unnecessary accounting request after it was consumed.
            event(
                &mut d,
                &mut p,
                json!({"type":"system","subtype":"task_notification","session_id":"same-session",
                "task_id":task,"run_id":run,"status":"completed"}),
            )
            .await;
            event(&mut d, &mut p, idle.clone()).await;
            if run == "run-a" {
                assert_eq!(d.phase(), Phase::Running);
            }
        }
        assert_eq!(d.phase(), Phase::ResultObserved);
        assert!(!d.needs_usage_snapshot());
        let receipt = d.completion_receipt().unwrap();
        assert_eq!(
            receipt.is_error(),
            final_error,
            "the latest native reply determines the answer status"
        );
        assert!(receipt.matches_text("final background reply"));
        assert_eq!(
            counts(&final_usage.unwrap()),
            [Some(396), Some(140), Some(52), Some(116)]
        );
    }
}
