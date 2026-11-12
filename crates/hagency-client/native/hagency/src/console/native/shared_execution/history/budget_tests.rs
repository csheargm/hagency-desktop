use super::*;
use hagency_runtime::codex::{
    session::{ContextUsage, SessionDriver, Settings},
    transport::Limits,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

async fn send(peer: &mut DuplexStream, value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    peer.write_all(&bytes).await.unwrap();
}
async fn read(peer: &mut DuplexStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let byte = peer.read_u8().await.unwrap();
        if byte == b'\n' {
            return serde_json::from_slice(&bytes).unwrap();
        }
        bytes.push(byte);
        assert!(bytes.len() < 65536);
    }
}
async fn reply(peer: &mut DuplexStream, result: Value) {
    let request = read(peer).await;
    send(peer, json!({"id":request["id"],"result":result})).await;
}
// Get capacity through the same scoped protocol decoder used in production,
// not a public DTO constructor or a manually replenished Reader field.
async fn samples() -> Vec<ContextUsage> {
    let (wire, mut peer) = tokio::io::duplex(65536);
    let (stdout, stdin) = tokio::io::split(wire);
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().canonicalize().unwrap();
    let mut session = SessionDriver::new(
        stdout,
        stdin,
        tokio::io::empty(),
        Settings::new(cwd.clone(), "fixture-model".into(), "medium".into()).unwrap(),
        Limits {
            write_timeout_ms: 1000,
            event_wait_ms: 1000,
            lifetime_ms: 10000,
        },
        1000,
    )
    .unwrap();
    let (result, ()) = tokio::join!(session.initialize(), async {
        reply(&mut peer, json!({"userAgent":"fixture/0.167.0","platformFamily":"unix","platformOs":"fixture","codexHome":"/fixture"})).await;
        assert_eq!(read(&mut peer).await["method"], "initialized");
    });
    result.unwrap();
    let (result, ()) = tokio::join!(
        session.start_thread(),
        reply(
            &mut peer,
            json!({
                "thread":{"id":"root","cwd":cwd,"status":{"type":"idle"},"turns":[]},
                "cwd":cwd,"model":"fixture-model","modelProvider":"fixture","approvalPolicy":"on-request","approvalsReviewer":"user",
                "sandbox":{"type":"workspaceWrite","networkAccess":false}
            })
        )
    );
    result.unwrap();
    let (result, ()) = tokio::join!(
        session.start_turn("Read the room".into()),
        reply(
            &mut peer,
            json!({"turn":{"id":"root-turn","status":"inProgress","items":[]}})
        )
    );
    result.unwrap();
    send(&mut peer, json!({"method":"turn/started","params":{"threadId":"child","turn":{"id":"child-turn","rootTurnId":"root-turn","status":"inProgress","items":[]}}})).await;
    session.next_observed_update().await.unwrap();
    let mut samples = Vec::new();
    for (thread, turn, used, total) in [
        ("child", "child-turn", 60_000, 100_000),
        ("root", "root-turn", 1_000, 900_000),
        ("child", "child-turn", 2_000, 100_001),
    ] {
        let count = |n| json!({"totalTokens":n,"inputTokens":n,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0});
        send(
            &mut peer,
            json!({"method":"thread/tokenUsage/updated","params":{"threadId":thread,"turnId":turn,
            "tokenUsage":{"total":count(total),"last":count(used),"modelContextWindow":65_536}}}),
        )
        .await;
        let (_, observation) = session.next_observed_update().await.unwrap();
        samples.push(observation.context_usage().unwrap().clone());
    }
    samples
}

#[tokio::test]
async fn room_history_context_budgets_are_separate_for_native_descendants() {
    let samples = samples().await;
    let mut reader = super::tests::paged_reader();
    let child = Some(("child", "child-turn"));
    reader.observe_context(&samples[0]);
    assert_eq!(reader.available(child), 0);
    assert_eq!(reader.available(None), 32 * 1024);
    reader.observe_context(&samples[1]);
    let root_space = reader.available(None);
    assert!(root_space > 32 * 1024);
    assert_eq!(
        reader.available(child),
        0,
        "root capacity never replenishes a child"
    );
    let blocked: Value =
        serde_json::from_str(&reader.read_scoped(None, child).await.unwrap()).unwrap();
    assert_eq!(blocked["reason"], "context_budget");
    assert!(!reader.started);
    let root_page: Value = serde_json::from_str(&reader.read(None).await.unwrap()).unwrap();
    assert!(reader.available(None) < root_space);
    assert_eq!(reader.available(child), 0);
    reader.observe_context(&samples[2]);
    let root_space = reader.available(None);
    let child_space = reader.available(child);
    let page: Value = serde_json::from_str(
        &reader
            .read_scoped(root_page["nextCursor"].as_str(), child)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        page["messages"]
            .as_array()
            .is_some_and(|records| !records.is_empty())
    );
    assert!(reader.available(child) < child_space);
    assert_eq!(
        reader.available(None),
        root_space,
        "child input is charged to its own window"
    );
    let spent = reader.available(child);
    reader.observe_context(&samples[2]);
    assert_eq!(
        reader.available(child),
        spent,
        "a replay cannot restore spent space"
    );
    assert_eq!(
        reader.available(Some(("sibling", "sibling-turn"))),
        32 * 1024
    );
}
