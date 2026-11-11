use hagency_runtime::claude::{
    self, ControlOutcome, Decoder, Error, EventKind, MAX_FRAME_BYTES, MAX_TEXT_BYTES, Message,
    PARTIAL_FRAME_MS,
};
use serde_json::{Value, json};

#[test]
fn shared_profiles_isolate_customizations_and_keep_host_permission_routing() {
    use hagency_runtime::owned::SharedClaudePolicy;
    for policy in [SharedClaudePolicy::Text, SharedClaudePolicy::LiveWeb,
        SharedClaudePolicy::Standard { web: false }, SharedClaudePolicy::Yolo { web: true }] {
        let args = policy.arguments("sonnet", "medium").unwrap();
        for required in ["--safe-mode", "--setting-sources=", "--strict-mcp-config", "--no-chrome"] {
            assert!(args.iter().any(|arg| arg == required), "{required}");
        }
        assert!(args.windows(2).any(|p| p == ["--mcp-config", "{\"mcpServers\":{}}"]));
        assert!(args.windows(2).any(|p| p == ["--permission-prompt-tool", "stdio"]));
        assert!(!args.iter().any(|p| p == "--dangerously-skip-permissions"));
        if policy.tools() { assert!(!args.iter().any(|p| p == "--tools")); }
    }
}

#[test]
fn interactive_profile_loads_native_capabilities_and_routes_all_tools_to_host() {
    let args = claude::interactive_arguments("sonnet", "xhigh").unwrap();
    assert!(args.iter().any(|arg| arg == "--include-partial-messages"));
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--permission-prompt-tool", "stdio"])
    );
    let settings: Value =
        serde_json::from_str(&args[args.iter().position(|arg| arg == "--settings").unwrap() + 1])
            .unwrap();
    assert_eq!(settings["permissions"]["ask"], json!(["*"]));
    assert_eq!(settings["sandbox"]["autoAllowBashIfSandboxed"], false);
    assert!(settings.get("disableAllHooks").is_none());
    for restricted in [
        "--tools",
        "--allowedTools",
        "--disallowedTools",
        "--strict-mcp-config",
        "--mcp-config",
        "--setting-sources",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
        "--dangerously-skip-permissions",
    ] {
        assert!(!args.iter().any(|arg| arg == restricted), "{restricted}");
    }
    assert!(claude::interactive_arguments("sonnet", "--dangerously-skip-permissions").is_err());
}
#[test]
fn native_model_aliases_context_suffixes_and_provider_ids_are_single_arguments() {
    for model in [
        "sonnet[1m]",
        "opus[1m]",
        "opusplan",
        "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-sonnet-4-6-v1:0",
        "claude-sonnet-4-6@20260217",
    ] {
        for args in [
            claude::interactive_arguments(model, "medium").unwrap(),
            claude::task_arguments(model, true).unwrap(),
        ] {
            assert_eq!(
                args.iter()
                    .filter(|arg| arg.starts_with("--model="))
                    .collect::<Vec<_>>(),
                vec![&format!("--model={model}")]
            );
        }
    }
}
fn line(value: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    bytes
}
fn parse(value: Value) -> Message {
    Decoder::default().feed(&line(value), 1).unwrap().1.unwrap()
}
fn malformed(bytes: &[u8], error: Error) {
    let mut decoder = Decoder::default();
    assert!(matches!(decoder.feed(bytes,1),Err(actual) if actual==error));
    assert!(matches!(decoder.feed(b"{}\n", 2), Err(Error::Closed)));
    assert_eq!(decoder.buffered_bytes(), 0);
}
#[test]
fn native_claude_stream_framing() {
    let original = line(
        json!({"type":"assistant","session_id":"session-1","message":{"content":[{"type":"text","text":"测试"}]}}),
    );
    let mut decoder = Decoder::default();
    for (index, byte) in original.iter().enumerate() {
        let (count, message) = decoder.feed(&[*byte], index as u64).unwrap();
        assert_eq!(count, 1);
        assert_eq!(message.is_some(), index == original.len() - 1);
    }
    assert_eq!(decoder.buffered_bytes(), 0);
    decoder.eof().unwrap();
    assert!(matches!(decoder.feed(&original, 1000), Err(Error::Closed)));
    let mut crlf = original[..original.len() - 1].to_vec();
    crlf.extend(b"\r\n");
    crlf.extend(&original);
    let mut decoder = Decoder::default();
    let (count, message) = decoder.feed(&crlf, 1).unwrap();
    assert_eq!(count, original.len() + 1);
    assert!(message.is_some());
    assert!(decoder.feed(&crlf[count..], 1).unwrap().1.is_some());
    malformed(
        b"{\"type\":\"result\",\"type\":\"assistant\"}\n",
        Error::Envelope,
    );
    malformed(
        b"{\"type\":\"result\",\"\\u0074ype\":\"assistant\"}\n",
        Error::Envelope,
    );
    malformed(
        b"{\"type\":\"assistant\",\"session_id\":\"s\",\"message\":{\"x\":1,\"x\":2}}\n",
        Error::Envelope,
    );
    malformed(b"\xff\n", Error::Envelope);
    malformed(b"\n", Error::Envelope);
    malformed(&vec![b' '; MAX_FRAME_BYTES + 1], Error::Capacity);
    let mut nested = json!(null);
    for _ in 0..66 {
        nested = json!([nested]);
    }
    malformed(
        &line(json!({"type":"assistant","session_id":"s","message":{"deep":nested}})),
        Error::Capacity,
    );
    let mut exact = original[..original.len() - 1].to_vec();
    exact.resize(MAX_FRAME_BYTES, b' ');
    exact.push(b'\n');
    assert!(Decoder::default().feed(&exact, 1).unwrap().1.is_some());
    exact.insert(MAX_FRAME_BYTES, b'\r');
    malformed(&exact, Error::Capacity);
    let mut decoder = Decoder::default();
    decoder.feed(b"{", 1).unwrap();
    decoder.check_deadline(PARTIAL_FRAME_MS).unwrap();
    assert_eq!(
        decoder.check_deadline(PARTIAL_FRAME_MS + 1),
        Err(Error::Timeout)
    );
    assert_eq!(
        decoder.check_deadline(PARTIAL_FRAME_MS + 2),
        Err(Error::Closed)
    );
    let mut decoder = Decoder::default();
    decoder.feed(b"{", 9).unwrap();
    assert_eq!(decoder.check_deadline(8), Err(Error::Clock));
    let mut decoder = Decoder::default();
    decoder.feed(b"{", 1).unwrap();
    assert_eq!(decoder.eof(), Err(Error::UnexpectedEof));
    assert_eq!(decoder.eof(), Err(Error::Closed));
}
#[test]
fn native_claude_stream_envelopes() {
    match parse(
        json!({"type":"control_request","request_id":"request-1","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool-1","input":{"command":"synthetic only"}}}),
    ) {
        Message::Permission {
            request_id,
            tool_name,
            tool_use_id,
            input,
        } => {
            assert_eq!(request_id, "request-1");
            assert_eq!(tool_name, "Bash");
            assert_eq!(tool_use_id.as_deref(), Some("tool-1"));
            assert_eq!(input["command"], "synthetic only");
        }
        _ => panic!("permission shape"),
    }
    match parse(
        json!({"type":"control_response","response":{"subtype":"success","request_id":"initialize-1","response":{"commands":[]}}}),
    ) {
        Message::ControlResponse {
            request_id,
            outcome: ControlOutcome::Success(body),
        } => {
            assert_eq!(request_id, "initialize-1");
            assert_eq!(body["commands"], json!([]));
        }
        _ => panic!("response shape"),
    }
    assert!(matches!(
        parse(
            json!({"type":"control_response","response":{"subtype":"error","request_id":"i","error":"private upstream text"}})
        ),
        Message::ControlResponse {
            outcome: ControlOutcome::Refused,
            ..
        }
    ));
    assert!(
        matches!(parse(json!({"type":"control_cancel_request","request_id":"request-1"})),Message::ControlCancel {request_id} if request_id=="request-1")
    );
    for (name, kind, extra) in [
        (
            "system",
            EventKind::System,
            json!({"subtype":"init","permissionMode":"auto"}),
        ),
        (
            "assistant",
            EventKind::Assistant,
            json!({"message":{"role":"assistant","content":[]}}),
        ),
        (
            "user",
            EventKind::User,
            json!({"message":{"role":"user","content":[]}}),
        ),
        (
            "result",
            EventKind::Result,
            json!({"subtype":"success","is_error":false,"result":"Done is only text"}),
        ),
        (
            "stream_event",
            EventKind::Stream,
            json!({"event":{"type":"message_start"}}),
        ),
        ("tool_progress", EventKind::ToolProgress, json!({})),
        ("tool_use_summary", EventKind::ToolSummary, json!({})),
        ("auth_status", EventKind::AuthStatus, json!({})),
        ("rate_limit_event", EventKind::RateLimit, json!({})),
    ] {
        let mut value = extra;
        value["type"] = json!(name);
        value["session_id"] = json!("original-session");
        value["parent_tool_use_id"] = json!(null);
        match parse(value.clone()) {
            Message::Event {
                session_id,
                kind: actual,
                payload,
            } => {
                assert_eq!(session_id, "original-session");
                assert_eq!(actual, kind);
                assert_eq!(payload, value);
            }
            _ => panic!("event shape"),
        }
    }
    // A misleading success subtype with is_error=true stays an error-shaped
    // upstream observation, never a completion or permission grant.
    match parse(
        json!({"type":"result","subtype":"success","is_error":true,"session_id":"s","result":"API failure","usage":{}}),
    ) {
        Message::Event {
            kind: EventKind::Result,
            payload,
            ..
        } => assert_eq!(payload["is_error"], true),
        _ => panic!("result shape"),
    }
    for value in [
        json!({"type":"control_cancel_request","request_id":1}),
        json!({"type":"control_cancel_request","request_id":"x\n"}),
        json!({"type":"control_response","response":{"subtype":"success","request_id":"i","response":{},"error":"conflicting"}}),
        json!({"type":"control_request","request_id":"i","request":{"subtype":"can_use_tool","tool_name":"Bash","input":[]}}),
        json!({"type":"result","subtype":"success","session_id":"s","result":"not enough"}),
        json!({"type":"assistant","session_id":"","message":{}}),
    ] {
        malformed(&line(value), Error::Envelope);
    }
    // An unknown control request subtype is refused without closing the
    // stream: see `future_control_request_subtype_keeps_the_stream_open`.
    let mut decoder = Decoder::default();
    assert!(
        decoder
            .feed(
                &line(json!({"type":"future_unhandled","session_id":"s"})),
                1
            )
            .unwrap()
            .1
            .is_none()
    );
    assert_eq!(decoder.ignored_frames(), 1);
    // Real installed CLI initialize metadata, values sanitized to empty arrays.
    assert!(matches!(
        parse(
            json!({"type":"control_response","response":{"subtype":"success","request_id":"init",
        "response":{},"pending_permission_requests":[],"pending_user_dialog_requests":[]}})
        ),
        Message::ControlResponse { .. }
    ));
    for name in [
        "pending_permission_requests",
        "pending_user_dialog_requests",
    ] {
        for (pending, expected) in [
            (json!(null), Error::Envelope),
            (json!({}), Error::Envelope),
            (json!([{"request_id":"preexisting"}]), Error::Unsupported),
        ] {
            let mut value = json!({"type":"control_response","response":{"subtype":"success","request_id":"init","response":{}}});
            value["response"][name] = pending;
            malformed(&line(value), expected);
        }
    }
}
#[test]
fn native_claude_stream_host_input() {
    let args = claude::arguments("claude-sonnet-4-6").unwrap();
    assert_eq!(
        args,
        vec![
            "--print",
            "--verbose",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--permission-mode",
            "auto",
            "--permission-prompt-tool",
            "stdio",
            "--model=claude-sonnet-4-6"
        ]
    );
    for model in [
        "",
        "--dangerously-skip-permissions",
        "model --permission-mode bypassPermissions",
        "x\0y",
    ] {
        assert!(claude::arguments(model).is_err());
    }
    for (bytes, subtype) in [
        (claude::initialize("original-1").unwrap(), "initialize"),
        (claude::interrupt("original-2").unwrap(), "interrupt"),
    ] {
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 1);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["request"]["subtype"], subtype);
        assert_eq!(value["type"], "control_request");
    }
    let first: Value =
        serde_json::from_slice(&claude::prompt("hello\nworld", None).unwrap()).unwrap();
    assert_eq!(first["session_id"], "");
    assert_eq!(first["message"]["content"], "hello\nworld");
    assert!(first["parent_tool_use_id"].is_null());
    let next: Value =
        serde_json::from_slice(&claude::prompt("next", Some("session-1")).unwrap()).unwrap();
    assert_eq!(next["session_id"], "session-1");
    // A prompt is never a slash command of the owner's CLI: the host's line
    // comes first, and the text itself is unchanged after it.
    for text in ["/cost", "/compact now\nmore"] {
        let led: Value =
            serde_json::from_slice(&claude::prompt(text, Some("session-1")).unwrap()).unwrap();
        assert_eq!(
            led["message"]["content"],
            format!("{}{text}", claude::PROMPT_LEAD)
        );
    }
    for text in [" /cost", "see /cost", "a/b"] {
        let plain: Value = serde_json::from_slice(&claude::prompt(text, None).unwrap()).unwrap();
        assert_eq!(plain["message"]["content"], text);
    }
    assert!(claude::prompt(&format!("/{}", "x".repeat(MAX_TEXT_BYTES - 1)), None).is_ok());
    assert!(claude::prompt(&"x".repeat(MAX_TEXT_BYTES), None).is_ok());
    assert!(claude::prompt(&"x".repeat(MAX_TEXT_BYTES + 1), None).is_err());
    assert!(claude::prompt("", None).is_err());
    assert!(claude::prompt("next", Some("")).is_err());
    assert!(claude::initialize("bad id").is_err());
    assert!(claude::interrupt(&"x".repeat(257)).is_err());
}

#[test]
fn unknown_metadata_is_counted_and_bounded_but_authority_frames_remain_strict() {
    let mut decoder = Decoder::default();
    let mut unknown = b"{\"type\":\"future_diagnostic\",\"payload\":\"".to_vec();
    unknown.extend(vec![b'x'; MAX_FRAME_BYTES * 3]);
    unknown.extend(b"\"}\n");
    for chunk in unknown.chunks(4093) {
        let (used, message) = decoder.feed(chunk, 1).unwrap();
        assert_eq!(used, chunk.len());
        assert!(message.is_none());
        assert!(decoder.buffered_bytes() <= MAX_FRAME_BYTES);
    }
    assert_eq!(decoder.buffered_bytes(), 0);
    assert_eq!(decoder.ignored_frames(), 1);
    let result = line(
        json!({"type":"result","subtype":"success","session_id":"s","is_error":false,"result":"kept"}),
    );
    assert!(matches!(
        decoder.feed(&result, 2).unwrap().1,
        Some(Message::Event {
            kind: EventKind::Result,
            ..
        })
    ));
    for kind in [
        "control_request",
        "control_response",
        "result",
        "control_future",
    ] {
        let mut oversized = format!("{{\"type\":\"{kind}\",\"payload\":\"").into_bytes();
        oversized.extend(vec![b'x'; MAX_FRAME_BYTES + 1]);
        malformed(&oversized, Error::Capacity);
    }
    let mut decoder = Decoder::default();
    let partial = &unknown[..unknown.len() - 1];
    assert!(decoder.feed(partial, 1).unwrap().1.is_none());
    assert_eq!(
        decoder.check_deadline(1 + PARTIAL_FRAME_MS),
        Err(Error::Timeout)
    );
}

/// F8: an unknown request subtype is never decoded as a host message, and
/// the stream stays open for the frames that follow it.
#[test]
fn future_control_request_subtype_keeps_the_stream_open() {
    let mut decoder = Decoder::default();
    let unknown = line(json!({"type":"control_request","request_id":"future-1",
        "request":{"subtype":"future_dialog","detail":{"private":"sentinel"}}}));
    let (used, message) = decoder
        .feed(&unknown, 1)
        .unwrap_or_else(|error| panic!("an unknown control request closed the stream: {error:?}"));
    assert_eq!(used, unknown.len());
    assert!(message.is_none());
    assert!(matches!(
        decoder
            .feed(&line(json!({"type":"control_cancel_request","request_id":"other"})), 2)
            .unwrap()
            .1,
        Some(Message::ControlCancel { request_id }) if request_id == "other"
    ));
    // The stream owes that request exactly one answer: an error that echoes
    // nothing but its identity.
    assert_eq!(
        decoder.take_unsupported_request(),
        Some(("future-1".into(), "future_dialog".into()))
    );
    assert_eq!(decoder.take_unsupported_request(), None);
    assert_eq!(decoder.ignored_frames(), 1);
    let reply = claude::unsupported_reply("future-1").unwrap();
    assert_eq!(reply.last(), Some(&b'\n'));
    assert_eq!(
        serde_json::from_slice::<Value>(&reply).unwrap(),
        json!({"type":"control_response","response":{
            "subtype":"error","request_id":"future-1","error":"Unsupported control request"}})
    );
    assert!(claude::unsupported_reply("bad id").is_err());
    // The native peer may withdraw it once; that carries nothing. Any other
    // withdrawal is still decoded for the session to judge.
    let withdrawn = line(json!({"type":"control_cancel_request","request_id":"future-1"}));
    assert!(decoder.feed(&withdrawn, 3).unwrap().1.is_none());
    assert_eq!(decoder.ignored_frames(), 2);
    assert!(matches!(
        decoder.feed(&withdrawn, 4).unwrap().1,
        Some(Message::ControlCancel { request_id }) if request_id == "future-1"
    ));
    // Answers that nobody takes are bounded.
    let mut decoder = Decoder::default();
    for n in 0..16 {
        let request = line(json!({"type":"control_request","request_id":format!("flood-{n}"),
            "request":{"subtype":"future_dialog"}}));
        assert!(decoder.feed(&request, 1).unwrap().1.is_none());
    }
    malformed_after(
        &mut decoder,
        &line(json!({"type":"control_request","request_id":"flood-16","request":{"subtype":"future_dialog"}})),
        Error::Capacity,
    );
}
fn malformed_after(decoder: &mut Decoder, bytes: &[u8], error: Error) {
    assert!(matches!(decoder.feed(bytes, 5), Err(actual) if actual == error));
    assert!(matches!(decoder.feed(b"{}\n", 6), Err(Error::Closed)));
}

/// F8: keys this host does not read never change the frame it decodes.
#[test]
fn control_frames_with_extra_keys_are_decoded_by_the_keys_this_host_reads() {
    let mut closed = Vec::new();
    let mut extra = |name: &str, value: Value| -> Option<Message> {
        match Decoder::default().feed(&line(value), 1) {
            Ok((_, message)) => message,
            Err(error) => {
                closed.push(format!("{name}: {error:?}"));
                None
            }
        }
    };
    let permission = extra(
        "permission envelope",
        json!({"type":"control_request","request_id":"request-1","uuid":"u-1","session_id":"s",
        "request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"tool-1","input":{"command":"exact"}}}),
    );
    let mcp = extra(
        "sdk mcp request",
        json!({"type":"control_request","request_id":"request-2","uuid":"u-2",
        "request":{"subtype":"mcp_message","server_name":"hagency_matrix","origin":"future",
            "message":{"jsonrpc":"2.0","id":1,"method":"ping"}}}),
    );
    let success = extra(
        "success response",
        json!({"type":"control_response","uuid":"u-3","response":{"subtype":"success",
        "request_id":"i","response":{"ok":true},"future_note":{"x":1},
        "pending_permission_requests":[],"pending_user_dialog_requests":[]}}),
    );
    // A refusal stays a refusal whatever else the native peer attaches to it.
    let refusal = extra(
        "error response",
        json!({"type":"control_response","response":{"subtype":"error","request_id":"i",
        "error":"private upstream text","pending_permission_requests":[{"request_id":"p"}],"code":7}}),
    );
    let cancel = extra(
        "cancel",
        json!({"type":"control_cancel_request","request_id":"request-1","reason":"future"}),
    );
    assert!(
        closed.is_empty(),
        "extra keys on a control frame closed the stream: {closed:?}"
    );
    match permission.unwrap() {
        Message::Permission {
            request_id,
            tool_name,
            tool_use_id,
            input,
        } => {
            assert_eq!(request_id, "request-1");
            assert_eq!(tool_name, "Bash");
            assert_eq!(tool_use_id.as_deref(), Some("tool-1"));
            assert_eq!(input, json!({"command":"exact"}));
        }
        _ => panic!("permission shape"),
    }
    match mcp.unwrap() {
        Message::Mcp {
            request_id,
            server_name,
            message,
        } => {
            assert_eq!(request_id, "request-2");
            assert_eq!(server_name, "hagency_matrix");
            assert_eq!(message, json!({"jsonrpc":"2.0","id":1,"method":"ping"}));
        }
        _ => panic!("mcp shape"),
    }
    assert!(matches!(
        success.unwrap(),
        Message::ControlResponse { request_id, outcome: ControlOutcome::Success(body) }
            if request_id == "i" && body == json!({"ok":true})
    ));
    assert!(matches!(
        refusal.unwrap(),
        Message::ControlResponse { request_id, outcome: ControlOutcome::Refused } if request_id == "i"
    ));
    assert!(matches!(
        cancel.unwrap(),
        Message::ControlCancel { request_id } if request_id == "request-1"
    ));
}

/// F8, unchanged on purpose: control frames that cannot be answered,
/// classified or adopted still close the stream.
#[test]
fn control_frames_that_cannot_be_answered_or_classified_still_close_the_stream() {
    for (value, error) in [
        // No usable identity to refuse.
        (json!({"type":"control_request","request":{"subtype":"future_dialog"}}), Error::Envelope),
        (json!({"type":"control_request","request_id":"bad id","request":{"subtype":"future_dialog"}}), Error::Envelope),
        (json!({"type":"control_request","request_id":"i","request":{}}), Error::Envelope),
        (json!({"type":"control_request","request_id":"i","request":"future_dialog"}), Error::Envelope),
        // A known request that cannot be presented faithfully.
        (json!({"type":"control_request","request_id":"i","request":{"subtype":"can_use_tool","input":{}}}), Error::Envelope),
        (json!({"type":"control_request","request_id":"i","request":{"subtype":"mcp_message","server_name":"hagency_matrix","message":[]}}), Error::Envelope),
        // An answer to this host's own request that is neither accepted nor refused.
        (json!({"type":"control_response","response":{"subtype":"partial","request_id":"i","response":{}}}), Error::Unsupported),
        (json!({"type":"control_response","response":{"subtype":"success","request_id":"i","response":{},"error":"conflicting"}}), Error::Envelope),
        (json!({"type":"control_response","response":{"subtype":"error","request_id":"i"}}), Error::Envelope),
        (json!({"type":"control_response","response":{"subtype":"error","request_id":"i","error":"refused","response":{}}}), Error::Envelope),
        // Work that predates this host's one admitted input.
        (json!({"type":"control_response","response":{"subtype":"success","request_id":"i","response":{},
            "pending_permission_requests":[{"request_id":"preexisting"}]}}), Error::Unsupported),
        // A control frame type this host has no reply shape for.
        (json!({"type":"control_future","request_id":"i"}), Error::Unsupported),
    ] {
        let mut decoder = Decoder::default();
        let bytes = line(value.clone());
        assert!(
            matches!(decoder.feed(&bytes, 1), Err(actual) if actual == error),
            "{value}"
        );
        assert!(matches!(decoder.feed(b"{}\n", 2), Err(Error::Closed)));
    }
}
