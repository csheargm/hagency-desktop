//! The OUP codec (ADR-193): string IDs only, typed observations of the frames
//! the session driver reads, and every other frame ignored by OUP's additive
//! rule.
use hagency_runtime::octos::{
    Decoder, Error, Frame, MAX_FRAME_BYTES, MAX_PROFILE_BYTES, Notification, Outcome, Payload,
    ProfileModel, profile_id, profile_model, request, serve_arguments, session_key, uuid,
};
use serde_json::{Value, json};

fn frame(value: Value) -> Result<Frame, Error> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    let (consumed, frame) = Decoder::default().feed(&bytes, 0)?;
    assert_eq!(consumed, bytes.len());
    Ok(frame.expect("one whole line is one frame"))
}
fn notification(method: &str, params: Value) -> Notification {
    match frame(json!({"jsonrpc":"2.0","method":method,"params":params})).unwrap() {
        Frame::Notification(notification) => notification,
        Frame::Response { .. } => panic!("a method makes a notification"),
    }
}
fn envelope(payload: Value) -> Value {
    json!({"session_id":"coding:local:one","thread_id":"main","seq":4,"turn_id":"turn-1",
        "payload":payload})
}

#[test]
fn native_octos_reads_routed_plan_snapshots() {
    let plan = json!({"items":[{"title":"Inspect","status":"in_progress"}],"updated_at_ms":1});
    assert!(matches!(notification("plan/updated", json!({"session_id":"coding:local:one",
        "turn_id":"turn-1","plan":plan})), Notification::PlanUpdated { session_id, turn_id: Some(turn_id), plan }
        if session_id == "coding:local:one" && turn_id == "turn-1" && plan["items"][0]["title"] == "Inspect"));
    assert!(matches!(notification("plan/updated", json!({"session_id":"coding:local:one",
        "plan":{"items":[]}})), Notification::PlanUpdated { turn_id: None, .. }));
}

#[test]
fn native_octos_frames_answer_string_ids_only() {
    let Frame::Response { id, outcome } =
        frame(json!({"jsonrpc":"2.0","id":"hagency-1","result":{"accepted":true}})).unwrap()
    else {
        panic!("an answer");
    };
    assert_eq!(id, "hagency-1");
    assert_eq!(outcome.unwrap(), json!({"accepted":true}));
    let Frame::Response { outcome, .. } = frame(json!({"jsonrpc":"2.0","id":"hagency-2",
        "error":{"code":-32602,"message":"private upstream text","data":{"kind":"session_tool_list_invalid"}}}))
    .unwrap() else {
        panic!("an answer");
    };
    let refused = outcome.unwrap_err();
    assert_eq!(refused.code, -32602);
    assert_eq!(refused.kind.as_deref(), Some("session_tool_list_invalid"));
    // Hagency sends string IDs only; any other ID answers nothing it sent.
    for id in [json!(1), json!(null), json!(""), json!("two words")] {
        assert!(matches!(
            frame(json!({"jsonrpc":"2.0","id":id,"result":{}})),
            Err(Error::Envelope)
        ));
    }
    // The server never sends a request.
    assert!(matches!(
        frame(json!({"jsonrpc":"2.0","id":"server-1","method":"approval/respond","params":{}})),
        Err(Error::Unsupported)
    ));
    for invalid in [
        json!({"jsonrpc":"1.0","id":"hagency-1","result":{}}),
        json!({"jsonrpc":"2.0","id":"hagency-1"}),
        json!({"jsonrpc":"2.0","id":"hagency-1","result":{},"error":{"code":1}}),
        json!([]),
    ] {
        assert!(matches!(frame(invalid), Err(Error::Envelope)));
    }
}

#[test]
fn native_octos_frames_read_the_projection_the_driver_needs() {
    let Notification::Envelope(persisted) = notification(
        "projection/envelope",
        envelope(
            json!({"type":"assistant_persisted","data":{"text":"答复 reply",
            "assistant_segment_id":"s","meta":{"message_id":"m","persisted_at":"2026-10-08T00:00:00Z"}}}),
        ),
    ) else {
        panic!("an envelope");
    };
    assert_eq!(
        (
            persisted.session_id.as_str(),
            persisted.thread_id.as_str(),
            persisted.seq,
            persisted.turn_id.as_str()
        ),
        ("coding:local:one", "main", 4, "turn-1")
    );
    assert!(
        matches!(persisted.payload, Payload::AssistantPersisted { text, .. } if text == "答复 reply")
    );
    let Notification::Envelope(terminal) = notification(
        "projection/envelope",
        envelope(json!({"type":"turn_terminal","data":{"outcome":"errored",
            "error":{"code":"provider_unavailable","message":"private"},
            "token_usage":{"input_tokens":10,"output_tokens":7,"cache_read_tokens":20}}})),
    ) else {
        panic!("an envelope");
    };
    let Payload::Terminal {
        outcome,
        error_code,
        usage,
    } = terminal.payload
    else {
        panic!("a terminal");
    };
    assert_eq!(outcome, Outcome::Errored);
    assert_eq!(error_code.as_deref(), Some("provider_unavailable"));
    let usage = usage.expect("present usage");
    // Omitted counters are zero, as `EnvelopeTokenUsage` serializes them.
    assert_eq!(
        (
            usage.input,
            usage.output,
            usage.reasoning,
            usage.cache_read,
            usage.cache_write
        ),
        (10, 7, 0, 20, 0)
    );
    let Notification::Envelope(unknown) = notification(
        "projection/envelope",
        envelope(json!({"type":"turn_terminal","data":{"outcome":"completed"}})),
    ) else {
        panic!("an envelope");
    };
    // An absent object is unknown, never zero.
    assert!(matches!(
        unknown.payload,
        Payload::Terminal { usage: None, .. }
    ));
    // An end this host cannot classify refuses the turn.
    assert!(matches!(
        frame(json!({"jsonrpc":"2.0","method":"projection/envelope",
            "params":envelope(json!({"type":"turn_terminal","data":{"outcome":"abandoned"}}))})),
        Err(Error::Unsupported)
    ));
    // A future payload is ignored, but the envelope's routing stays strict.
    assert!(matches!(
        notification(
            "projection/envelope",
            envelope(json!({"type":"background/spawn_complete","data":{}}))
        ),
        Notification::Envelope(e) if matches!(e.payload, Payload::Other)
    ));
    for missing in ["thread_id", "seq", "turn_id", "session_id"] {
        let mut params = envelope(json!({"type":"tool_start","data":{}}));
        params.as_object_mut().unwrap().remove(missing);
        assert!(
            matches!(
                frame(json!({"jsonrpc":"2.0","method":"projection/envelope","params":params})),
                Err(Error::Envelope)
            ),
            "{missing}"
        );
    }
    // A topic routes to its `<session>#<topic>` key.
    let Notification::TurnStarted { session_id, .. } = notification(
        "turn/started",
        json!({"session_id":"coding:local:one","topic":"side","turn_id":"turn-2"}),
    ) else {
        panic!("a turn start");
    };
    assert_eq!(session_id, "coding:local:one#side");
    assert!(matches!(
        notification(
            "session/orchestration",
            json!({"session_id":"coding:local:one","active":false,"running_agents":0})
        ),
        Notification::Orchestration { active: false, .. }
    ));
    for method in [
        "approval/cancelled",
        "approval/decided",
        "approval/auto_resolved",
    ] {
        assert!(matches!(
            notification(method, json!({"session_id":"coding:local:one","approval_id":"a-1"})),
            Notification::ApprovalSettled { approval_id, .. } if approval_id == "a-1"
        ));
    }
    assert!(matches!(
        notification("message/delta", json!({"anything":true})),
        Notification::Other
    ));
}

#[test]
fn native_octos_requests_and_launch_arguments_are_bounded() {
    let bytes = request("hagency-1", "turn/start", json!({"session_id":"s"})).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        value,
        json!({"jsonrpc":"2.0","id":"hagency-1","method":"turn/start","params":{"session_id":"s"}})
    );
    assert!(matches!(
        request("hagency-1", "turn/start", json!([])),
        Err(Error::Input)
    ));
    assert!(matches!(
        request("two words", "turn/start", json!({})),
        Err(Error::Input)
    ));
    assert!(matches!(
        request(
            "hagency-1",
            "turn/start",
            json!({"text":"x".repeat(MAX_FRAME_BYTES)})
        ),
        Err(Error::Capacity)
    ));
    assert_eq!(
        serve_arguments(
            "/work/space",
            "/state/octos/agent",
            "/state/octos/agent/config.json"
        )
        .unwrap(),
        [
            "serve",
            "--stdio",
            "--cwd",
            "/work/space",
            "--no-network",
            "--instance-data-dir",
            "/state/octos/agent",
            "--config",
            "/state/octos/agent/config.json"
        ]
    );
    let scoped = hagency_runtime::octos::serve_arguments_scoped(
        "/work/space", "/state/octos/agent", "/state/octos/agent/config.json", "coding",
    ).unwrap();
    assert_eq!(&scoped[scoped.len()-2..], &["--host-scoped-profile", "coding"]);
    assert!(hagency_runtime::octos::serve_arguments_scoped(
        "/work/space", "/state/octos/agent", "/state/octos/agent/config.json", "../escape",
    ).is_err());
    for (workspace, instance, config) in [
        ("work", "/state", "/state/c.json"),
        ("/work", "state", "/state/c.json"),
        ("/work", "/state", "c.json"),
        ("/work\0", "/state", "/state/c.json"),
    ] {
        assert!(serve_arguments(workspace, instance, config).is_err());
    }
    // Hagency's own settings, as Octos reads them: no hooks, no MCP servers,
    // sessions out of the project.
    let config: Value = serde_json::from_slice(hagency_runtime::octos::CONFIG).unwrap();
    assert_eq!(
        config,
        json!({"version":1,"hooks":[],"mcp_servers":[],"appui":{"sessions_in_cwd":false}})
    );
    assert!(profile_id("coding") && profile_id("deep_seek-2"));
    let long = "p".repeat(65);
    for invalid in ["", "-lead", "a:b", "a b", "a#b", long.as_str()] {
        assert!(!profile_id(invalid), "{invalid}");
    }
    assert!(session_key("coding:local:hagency-1", "coding"));
    for invalid in [
        "coding:local:",
        "coding:matrix:hagency-1",
        "other:local:hagency-1",
        "coding:local:a:b",
        "coding:local:a#topic",
    ] {
        assert!(!session_key(invalid, "coding"), "{invalid}");
    }
    assert!(uuid("0192f0c1-0000-7000-8000-00000000c0de"));
    for invalid in [
        "0192F0C1-0000-7000-8000-00000000C0DE",
        "0192f0c1000070008000-00000000c0de",
        "not-a-uuid",
    ] {
        assert!(!uuid(invalid), "{invalid}");
    }
}

/// ADR-193 decision 7: Hagency reads a profile's own ID and its primary model
/// only. Keys beside them are never kept, a sub-account (whose model is its
/// parent's) is never read as runnable, and a file for another ID is refused.
#[test]
fn native_octos_profile_model_reads_the_primary_only() {
    let profile = json!({
        "id": "dev", "name": "Dev", "enabled": true,
        "created_at": "2026-10-08T00:00:00Z", "updated_at": "2026-10-08T00:00:00Z",
        "config": {
            "llm": {
                "primary": {"family_id": "zai-coding", "model_id": "glm-5.3-flash",
                    "route": {"base_url": "https://example.test/api"}},
                "fallbacks": [{"family_id": "deepseek", "model_id": "deepseek-v4-flash"}],
            },
            "env_vars": {"SYNTHETIC_API_KEY": "synthetic-secret-never-kept"},
        },
    });
    let bytes = serde_json::to_vec(&profile).unwrap();
    let model = profile_model("dev", &bytes).unwrap();
    assert_eq!(
        model,
        ProfileModel {
            family: "zai-coding".into(),
            model: "glm-5.3-flash".into(),
        }
    );
    assert!(!format!("{model:?}").contains("synthetic-secret"));
    // The file must be the profile the resource names.
    assert_eq!(profile_model("other", &bytes), None);
    assert_eq!(profile_model("../dev", &bytes), None);
    // A sub-account inherits its parent's primary: never read as its own.
    let mut child = profile.clone();
    child["parent_id"] = json!("dev");
    assert_eq!(
        profile_model("dev", &serde_json::to_vec(&child).unwrap()),
        None
    );
    // No primary, a partial primary, or a malformed file runs nothing.
    for config in [
        json!({}),
        json!({"llm": {"fallbacks": []}}),
        json!({"llm": {"primary": {"family_id": "zai-coding"}}}),
        json!({"llm": {"primary": {"family_id": "", "model_id": "glm-5.3-flash"}}}),
        json!({"llm": {"primary": {"family_id": "zai\ncoding", "model_id": "glm"}}}),
    ] {
        let mut changed = profile.clone();
        changed["config"] = config;
        assert_eq!(
            profile_model("dev", &serde_json::to_vec(&changed).unwrap()),
            None
        );
    }
    assert_eq!(profile_model("dev", b"{\"id\":\"dev\""), None);
    // A file over the bound is not read at all.
    let mut large = profile;
    large["padding"] = json!("x".repeat(MAX_PROFILE_BYTES as usize));
    assert_eq!(
        profile_model("dev", &serde_json::to_vec(&large).unwrap()),
        None
    );
}

#[test]
fn native_octos_decoder_bounds_partial_frames() {
    let mut decoder = Decoder::default();
    assert!(matches!(decoder.feed(b"{\"jsonrpc\"", 0), Ok((10, None))));
    assert_eq!(decoder.buffered_bytes(), 10);
    assert!(matches!(
        decoder.check_deadline(hagency_runtime::octos::PARTIAL_FRAME_MS),
        Err(Error::Timeout)
    ));
    let mut decoder = Decoder::default();
    let long = vec![b'x'; MAX_FRAME_BYTES + 1];
    assert!(matches!(decoder.feed(&long, 0), Err(Error::Capacity)));
    let mut decoder = Decoder::default();
    decoder.feed(b"{", 0).unwrap();
    assert!(matches!(decoder.eof(), Err(Error::UnexpectedEof)));
}
