use hagency_runtime::claude::{Message, session::*};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};
type Driver = SessionDriver<DuplexStream, DuplexStream, DuplexStream>;
struct Peer {
    input: BufReader<DuplexStream>,
    output: DuplexStream,
    _stderr: DuplexStream,
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
    let mut p = Peer {
        input: BufReader::new(input),
        output,
        _stderr: error,
    };
    assert!(d.observation_source().is_err());
    let (result, ()) = tokio::join!(d.initialize(), async {
        let v = read(&mut p).await;
        p.output
            .write_all(&encoded(json!({"type":"control_response","response":{
            "subtype":"success","request_id":v["request_id"],"response":{}}})))
            .await
            .unwrap();
    });
    result.unwrap();
    let (result, _) = tokio::join!(d.prompt("offline usage"), read(&mut p));
    result.unwrap();
    assert!(d.observation_source().is_err());
    event(
        &mut d,
        &mut p,
        json!({"type":"system","subtype":"init","session_id":"same-session"}),
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
    "usage":{"input_tokens":9999,"output_tokens":9999},"modelUsage":{
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
        v.as_object_mut().unwrap().remove("modelUsage");
        if let Some(ref model) = model {
            v["modelUsage"] = model.clone();
        }
        event(&mut d, &mut p, v).await;
        if model.as_ref().is_none_or(Value::is_null) {
            assert!(usage(&d).coverage() == UsageCoverage::MainLoopResult);
            assert_eq!(counts(usage(&d)), [Some(9999), Some(9999), None, None]);
        } else {
            assert!(usage(&d).diagnostics().has_invalid_fields());
            assert_eq!(counts(usage(&d)), [None; 4]);
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
                v["modelUsage"] =
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
        final_result["modelUsage"] = if finish == "reported" {
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
