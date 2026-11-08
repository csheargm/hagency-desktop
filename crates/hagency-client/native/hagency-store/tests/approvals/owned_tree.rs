use super::*;
use hagency_runtime::codex::{
    session::{SessionDriver, Settings, ObservationSource},
    transport::Limits,
};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

type Native = SessionDriver<DuplexStream, DuplexStream, DuplexStream>;
async fn read(stream: &mut DuplexStream) -> Value {
    let mut bytes = Vec::new();
    loop {
        let byte = stream.read_u8().await.unwrap();
        bytes.push(byte);
        assert!(bytes.len() < 65536);
        if byte == b'\n' {
            return serde_json::from_slice(&bytes).unwrap();
        }
    }
}
async fn send(stream: &mut DuplexStream, value: Value) {
    let mut bytes = serde_json::to_vec(&value).unwrap();
    bytes.push(b'\n');
    stream.write_all(&bytes).await.unwrap();
}
async fn native(root: &HostApprovalContext) -> (Native, [DuplexStream; 3], ObservationSource) {
    let (stdin, mut peer_in) = tokio::io::duplex(65536);
    let (stdout, mut peer_out) = tokio::io::duplex(65536);
    let (stderr, peer_err) = tokio::io::duplex(1024);
    let settings = Settings::new(
        root.workspace.clone().into(),
        "fixture".into(),
        "medium".into(),
    )
    .unwrap();
    let mut session = SessionDriver::new(
        stdout,
        stdin,
        stderr,
        settings,
        Limits {
            write_timeout_ms: 1000,
            event_wait_ms: 1000,
            lifetime_ms: 60_000,
        },
        1000,
    )
    .unwrap();
    let ((), ()) = tokio::join!(
        async {
            session.initialize().await.unwrap();
            session.start_thread().await.unwrap();
            session.start_turn("fixture".into()).await.unwrap();
        },
        async {
            let request = read(&mut peer_in).await;
            send(&mut peer_out, json!({"id":request["id"],"result":{"userAgent":"fixture/0.167.0","platformFamily":"unix","platformOs":"fixture","codexHome":"/fixture"}})).await;
            assert_eq!(read(&mut peer_in).await["method"], "initialized");
            let request = read(&mut peer_in).await;
            send(&mut peer_out, json!({"id":request["id"],"result":{
            "thread":{"id":root.thread_id,"cwd":root.workspace,"status":{"type":"idle"},"turns":[]},
            "cwd":root.workspace,"model":"fixture","modelProvider":"fixture","approvalPolicy":"on-request","approvalsReviewer":"user",
            "sandbox":{"type":"workspaceWrite","networkAccess":false}}})).await;
            let request = read(&mut peer_in).await;
            send(&mut peer_out, json!({"id":request["id"],"result":{"turn":{"id":root.turn_id,"status":"inProgress","items":[]}}})).await;
        }
    );
    let source = session.observation_source().unwrap();
    let (update, ()) = tokio::join!(
        session.next_update(),
        send(
            &mut peer_out,
            json!({"method":"turn/started","params":{"threadId":"child","turn":{"id":"child-turn","rootTurnId":root.turn_id,"status":"inProgress","items":[]}}})
        )
    );
    update.unwrap();
    (session, [peer_in, peer_out, peer_err], source)
}

#[tokio::test]
async fn native_owned_descendant_scope_cannot_rebind_authority_or_deadline() {
    let mut f = Fixture::configured(true, 1000, 60_000, false);
    let root = f.contexts[0].clone();
    let (mut native, _peer, source) = native(&root).await;
    let (other, _other_peer, _) = self::native(&root).await;
    let until = Instant::now() + Duration::from_secs(30);
    let scope =
        f.db.bind_owned_approval_tree_context(
            &f.caps[0],
            &f.fingerprints[0],
            &root,
            source,
            until,
            6000,
            1010,
        )
        .unwrap();
    let evidence = native.native_turn_context("child", "child-turn").unwrap();
    let mut child = root.clone();
    child.id = "child-context".into();
    child.thread_id = "child".into();
    child.turn_id = "child-turn".into();
    let wrong_process = other.native_turn_context("child", "child-turn").unwrap();
    assert!(
        f.db.register_owned_approval_descendant(&scope, &child, &wrong_process, 1011)
            .is_err()
    );
    for field in [
        "connection",
        "resource",
        "workspace",
        "environment",
        "write",
        "thread",
        "turn",
        "yolo",
        "root",
    ] {
        let mut changed = child.clone();
        match field {
            "connection" => changed.connection_id = "another".into(),
            "resource" => changed.workspace_resource = "another".into(),
            "workspace" => changed.workspace = "/different".into(),
            "environment" => changed.environment_id = Some("another".into()),
            "write" => changed.may_write = false,
            "thread" => changed.thread_id = "another".into(),
            "turn" => changed.turn_id = "another".into(),
            "yolo" => changed.yolo = true,
            _ => changed.id = root.id.clone(),
        }
        assert!(
            f.db.register_owned_approval_descendant(&scope, &changed, &evidence, 1011)
                .is_err(),
            "{field}"
        );
    }
    for _ in 0..2 {
        f.db.register_owned_approval_descendant(&scope, &child, &evidence, 1011)
            .unwrap();
    }
    assert_eq!(
        f.sql()
            .query_row("SELECT COUNT(*) FROM approval_contexts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        2
    );
    let mut request = f.input(0, 1);
    request.context_id = child.id.clone();
    request.params["threadId"] = child.thread_id.clone().into();
    request.params["turnId"] = child.turn_id.clone().into();
    let mut root_spoof = request.clone();
    root_spoof.context_id = root.id.clone();
    assert!(
        f.db.request_owner_approval(&f.caps[0], &root_spoof, 1012)
            .is_err()
    );
    let request =
        f.db.request_owner_approval(&f.caps[0], &request, 1012)
            .unwrap();
    assert!(f.db.maintain_owned_approval(&scope, 1013).unwrap().parked);
    f.choose(&request.id, ApprovalChoice::Deny);
    let mut grants = vec![
        f.db.authorize_approval_response(&f.caps[0], &request.id, 1014)
            .unwrap(),
    ];
    f.db.begin_approval_responses(&f.caps[0], &mut grants, until, 1015)
        .unwrap();
    f.db.observe_approval_response(
        &mut grants[0],
        hagency_store::ApprovalResponseObservation::WriteAccepted,
    )
    .unwrap();
    assert!(!f.db.maintain_owned_approval(&scope, 5999).unwrap().parked);
    assert!(
        f.db.register_owned_approval_descendant(&scope, &child, &evidence, 6000)
            .is_err()
    );
    assert!(f.db.maintain_owned_approval(&scope, 6000).is_err());
    let lease: u64 = f
        .sql()
        .query_row(
            "SELECT lease_until FROM runner_dispatches WHERE id='dispatch_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(lease, 6000);
    native.close();
    assert!(
        f.db.register_owned_approval_descendant(&scope, &child, &evidence, 1016)
            .is_err()
    );
}
