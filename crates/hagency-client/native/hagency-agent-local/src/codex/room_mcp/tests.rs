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
    assert_eq!(bridge.config["tools"].as_object().unwrap().len(), 5);
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
