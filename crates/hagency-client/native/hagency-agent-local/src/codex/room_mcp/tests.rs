use super::*;
use crate::room_tools::room_queue;

fn queue(
    dispatch: &str,
) -> (
    RoomQueue,
    tokio::sync::mpsc::Receiver<crate::room_tools::PendingRoomCall>,
) {
    room_queue(
        &crate::Scope {
            agent: "agent".into(),
            binding: "binding".into(),
            room: "!room:test".into(),
            requester: "@owner:test".into(),
            thread: "!room:test".into(),
        },
        dispatch,
    )
    .unwrap()
}
fn params(thread: &str, turn: &str, id: &str) -> Value {
    json!({"name":"read_room_history","arguments":{"cursor":null},"_meta":{
        "callId":id,"itemId":id,"threadId":thread,
        "x-codex-turn-metadata":{"thread_id":thread,"turn_id":turn}
    }})
}
#[tokio::test]
async fn old_turn_and_cross_thread_cannot_use_new_dispatch_authority() {
    let bridge = Bridge::new(RoomTools::history()).await.unwrap();
    let (first, mut receives) = queue("first");
    let active = bridge.begin(&first, "native", "one").unwrap();
    for bad in [
        params("foreign", "one", "id"),
        params("native", "old", "id"),
        json!({"name":"read_room_history","arguments":{}}),
    ] {
        assert_eq!(
            call(&bridge.state, bridge.tools, &bad).await["isError"],
            true
        );
        assert!(receives.try_recv().is_err());
    }
    let pending_call = params("native", "one", "valid");
    let request = call(&bridge.state, bridge.tools, &pending_call);
    tokio::pin!(request);
    let mut pending = tokio::select! { _ = &mut request => panic!("must await host"), p = receives.recv() => p.unwrap() };
    drop(active);
    assert_eq!(request.await["isError"], true);
    pending.closed().await;
    assert!(pending.is_closed());
    let (second, mut next) = queue("second");
    let _active = bridge.begin(&second, "native", "two").unwrap();
    assert_eq!(
        call(&bridge.state, bridge.tools, &pending_call).await["isError"],
        true
    );
    assert!(next.try_recv().is_err());
}
#[tokio::test]
async fn native_descendant_and_each_call_are_bound_to_original_observation() {
    let bridge = Bridge::new(RoomTools::history_and_files()).await.unwrap();
    let (queue, mut receives) = queue("first");
    let active = bridge.begin(&queue, "root", "one").unwrap();
    let mut tree = TurnTree::new("root", "one").unwrap();
    let child = tree
        .observe(
            "turn/started",
            &json!({"threadId":"child","turn":{"id":"child-turn","rootTurnId":"one"}}),
            false,
        )
        .unwrap()
        .unwrap();
    active.observe(&child, &tree);
    let p = params("child", "child-turn", "call");
    let request = call(&bridge.state, bridge.tools, &p);
    tokio::pin!(request);
    let pending = tokio::select! { _ = &mut request => panic!("must await host"), p = receives.recv() => p.unwrap() };
    pending.reply(RoomReply::new("fixture-history".into(), false).unwrap());
    assert_eq!(request.await["content"][0]["text"], "fixture-history");
    assert_eq!(call(&bridge.state, bridge.tools, &p).await["isError"], true);
    let ended = tree
        .observe(
            "turn/completed",
            &json!({"threadId":"child","turn":{"id":"child-turn","status":"completed"}}),
            false,
        )
        .unwrap()
        .unwrap();
    active.observe(&ended, &tree);
    assert_eq!(
        call(
            &bridge.state,
            bridge.tools,
            &params("child", "child-turn", "later")
        )
        .await["isError"],
        true
    );
    assert!(receives.try_recv().is_err());
    assert!(bridge.config.get("default_tools_approval_mode").is_none());
    assert_eq!(bridge.config["tools"].as_object().unwrap().len(), 6);
    // The host's push-freely tool never asks Codex for its own approval.
    assert_eq!(bridge.config["tools"]["watched_repo_step"]["approval_mode"], "approve");
}

async fn http(
    bridge: &Bridge,
    method: &str,
    authorization: &str,
    origin: &str,
    body: &str,
) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let url = bridge.config["url"].as_str().unwrap();
    let authority = url
        .trim_start_matches("http://")
        .trim_end_matches("/matrix");
    let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
    let req = format!(
        "{method} /matrix HTTP/1.1\r\nHost: {authority}\r\nAuthorization: {authorization}\r\n{origin}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut response = String::new();
    tokio::time::timeout(Duration::from_secs(2), stream.read_to_string(&mut response))
        .await
        .unwrap()
        .unwrap();
    response
}
#[tokio::test]
async fn localhost_mcp_requires_capability_rejects_browser_origin_and_bounds_body() {
    let bridge = Bridge::new(RoomTools::history_and_files()).await.unwrap();
    let auth = bridge.config["http_headers"]["Authorization"]
        .as_str()
        .unwrap();
    let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
    assert!(
        http(&bridge, "POST", "Bearer wrong", "", list)
            .await
            .starts_with("HTTP/1.1 403")
    );
    assert!(
        http(
            &bridge,
            "POST",
            auth,
            "Origin: https://foreign.test\r\n",
            list
        )
        .await
        .starts_with("HTTP/1.1 403")
    );
    assert!(
        http(&bridge, "GET", auth, "", "")
            .await
            .starts_with("HTTP/1.1 405")
    );
    let reply = http(&bridge, "POST", auth, "", list).await;
    assert!(reply.starts_with("HTTP/1.1 200"));
    assert!(reply.contains("receive_file") && reply.contains("read_room_history"));
    assert!(
        http(&bridge, "POST", auth, "", &"x".repeat(MAX_BODY + 1))
            .await
            .starts_with("HTTP/1.1 413")
    );
}

/// Codex code-mode (scripted) calls: metadata as a JSON string, a distinct
/// item id, or no top-level thread id. All were refused
/// live as "room_tool_unavailable"; each must reach the host now.
#[tokio::test]
async fn code_mode_call_metadata_shapes_are_admitted() {
    let bridge = Bridge::new(RoomTools::history()).await.unwrap();
    let (queue, mut receives) = queue("first");
    let _active = bridge.begin(&queue, "native", "one").unwrap();
    let meta = json!({"thread_id":"native","turn_id":"one"});
    for (n, shape) in [
        json!({"callId":"a","itemId":"exec-a","threadId":"native","x-codex-turn-metadata":meta.to_string()}),
        json!({"callId":"b","x-codex-turn-metadata":meta}),
        json!({"itemId":"c","x-codex-turn-metadata":{"thread_id":"native","turn_id":"one"}}),
    ].into_iter().enumerate() {
        let p = json!({"name":"read_room_history","arguments":{"cursor":null},"_meta":shape});
        let request = call(&bridge.state, bridge.tools, &p);
        tokio::pin!(request);
        let pending = tokio::select! { r = &mut request => panic!("shape {n} refused: {r}"), p = receives.recv() => p.unwrap() };
        pending.reply(RoomReply::new("ok".into(), false).unwrap());
        assert_eq!(request.await["content"][0]["text"], "ok", "shape {n}");
    }
}
#[tokio::test]
async fn refusals_name_their_reason_and_contradictions_stay_refused() {
    let bridge = Bridge::new(RoomTools::history()).await.unwrap();
    let no_turn = call(&bridge.state, bridge.tools, &params("native", "one", "x")).await;
    assert!(no_turn["content"][0]["text"].as_str().unwrap().contains("no_active_turn"));
    let (queue, _r) = queue("first");
    let _active = bridge.begin(&queue, "native", "one").unwrap();
    let mismatch = json!({"name":"read_room_history","arguments":{},"_meta":{"callId":"x","threadId":"other",
        "x-codex-turn-metadata":{"thread_id":"native","turn_id":"one"}}});
    let reply = call(&bridge.state, bridge.tools, &mismatch).await;
    assert_eq!(reply["isError"], true);
    assert!(reply["content"][0]["text"].as_str().unwrap().contains("thread_mismatch"));
    let borrowed = json!({"name":"read_room_history","arguments":{},"_meta":{"callId":"y",
        "x-codex-turn-metadata":{"thread_id":"native","turn_id":"stale","root_turn_id":"one"}}});
    let reply = call(&bridge.state, bridge.tools, &borrowed).await;
    assert!(reply["content"][0]["text"].as_str().unwrap().contains("turn_not_active"), "a root hint grants nothing");
}

/// ADR 0022, "Codex": `watched_repo_step` is admitted like a room tool call
/// and answered by the turn that took the calls, never by the room queue.
#[tokio::test]
async fn a_repo_step_reaches_its_own_turn_only() {
    let bridge = Bridge::new(RoomTools::history()).await.unwrap();
    assert!(definitions(bridge.tools).iter().any(|d| d["name"] == "watched_repo_step"));
    let (room, mut room_calls) = queue("first");
    let active = bridge.begin(&room, "native", "one").unwrap();
    let step = |id: &str| json!({"name":"watched_repo_step","arguments":{"command":"git status"},"_meta":{
        "callId":id,"itemId":id,"threadId":"native","x-codex-turn-metadata":{"thread_id":"native","turn_id":"one"}}});
    let refused = call(&bridge.state, bridge.tools, &step("before")).await;
    assert!(refused["content"][0]["text"].as_str().unwrap().contains("repo_steps_unavailable"), "{refused}");
    let (sender, mut steps) = super::super::repo_steps::channel();
    assert!(active.take_repo_steps(sender));
    let first = step("one");
    let request = call(&bridge.state, bridge.tools, &first);
    tokio::pin!(request);
    let pending = tokio::select! { _ = &mut request => panic!("must await the turn"), s = steps.recv() => s.unwrap() };
    assert_eq!(pending.command, "git status");
    pending.reply.send(("## main".into(), false)).unwrap();
    let answer = request.await;
    assert_eq!((answer["content"][0]["text"].as_str(), answer["isError"].as_bool()), (Some("## main"), Some(false)));
    assert_eq!(call(&bridge.state, bridge.tools, &step("one")).await["isError"], true, "a call id is answered once");
    assert!(room_calls.try_recv().is_err(), "the room queue never sees it");
    drop(active);
    assert_eq!(call(&bridge.state, bridge.tools, &step("late")).await["isError"], true);
}
