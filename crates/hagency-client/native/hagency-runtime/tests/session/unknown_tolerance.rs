//! Board #94: unknown Codex traffic must be tolerated in EVERY phase.
//!
//! The retained runner's line handler is a flat `if (message.method === …)`
//! chain with no catch-all (`router/src/runner.ts:738-793`), so a notification
//! it does not match falls through and is ignored; and a server request it
//! cannot answer gets a JSON-RPC answer, never a silent death
//! (`router/src/runner.ts:709-736`). Live e2e died three times on protocol
//! traffic the fakes never sent (Busy, mcpServer/elicitation/request,
//! account/updated). One test per phase: startup, idle, in-turn, shutdown.
//! Each injects an INVENTED notification AND an INVENTED request, then proves
//! the turn still runs to completion.
use super::*;

fn invented_notification() -> Value {
    note("invented/notification", json!({"private": "peer-text"}))
}
fn invented_request() -> Value {
    json!({"id": "invented-request", "method": "invented/request",
        "params": {"threadId": "thread-one", "private": "peer-text"}})
}
fn initialize_result(id: &Value) -> Value {
    json!({"id": id, "result": {"userAgent": "fixture/0.167.0", "platformFamily": "unix",
        "platformOs": "fixture", "codexHome": "/fixture"}})
}
/// Run a turn to completion and assert the host sees the ending.
async fn completes(session: &mut Session, peer: &mut Peer) {
    start(session, peer, vec![]).await.unwrap();
    assert!(matches!(
        update(session, peer, end("completed")).await.unwrap(),
        Update::TurnEnded
    ));
    assert!(matches!(session.outcome(), Some(Outcome::Completed { .. })));
}

#[tokio::test]
async fn native_codex_session_unknown_traffic_during_startup() {
    let (mut session, mut peer) = fixture(false);
    let (result, ()) = tokio::join!(session.initialize(), async {
        let request = read(&mut peer.stdin).await;
        assert_eq!(request["method"], "initialize");
        // An invented NOTIFICATION arrives before the initialize result.
        write(&mut peer, invented_notification()).await;
        write(&mut peer, initialize_result(&request["id"])).await;
        assert_eq!(read(&mut peer.stdin).await["method"], "initialized");
    });
    result.expect("an invented notification must not fail startup");
    assert_eq!(session.phase(), Phase::Ready);

    let (result, answer) = tokio::join!(session.start_thread(), async {
        let request = read(&mut peer.stdin).await;
        assert_eq!(request["method"], "thread/start");
        // An invented REQUEST races the thread/start response.
        write(&mut peer, invented_request()).await;
        write(
            &mut peer,
            json!({"id": request["id"], "result": thread_result(false)}),
        )
        .await;
        read(&mut peer.stdin).await
    });
    result.expect("an invented request must not fail thread/start");
    assert_eq!(answer["error"]["code"], -32601);
    assert_eq!(session.phase(), Phase::ThreadReady);
    assert!(session.outcome().is_none());
    completes(&mut session, &mut peer).await;
}

#[tokio::test]
async fn native_codex_session_unknown_traffic_while_idle() {
    let (mut session, mut peer) = fixture(false);
    initialize(&mut session, &mut peer).await;
    open(&mut session, &mut peer, false).await;
    assert_eq!(session.phase(), Phase::ThreadReady);

    // Unknown traffic arrives while the thread is open and no turn is running.
    // `next_update` is only legal in `Running`, so it waits on the wire until
    // the next request pumps the queue — which is exactly the live shape.
    write(&mut peer, invented_notification()).await;
    write(&mut peer, invented_request()).await;

    // Starting the turn drains both: the invented request is answered with the
    // wire's own refusal on the way, and the invented notification is deferred
    // rather than fatal.
    let (result, answer) = tokio::join!(session.start_turn("fixture input".into()), async {
        let request = read(&mut peer.stdin).await;
        assert_eq!(request["method"], "turn/start");
        let answer = read(&mut peer.stdin).await;
        write(
            &mut peer,
            json!({"id": request["id"], "result": {"turn": turn("inProgress")}}),
        )
        .await;
        answer
    });
    result.expect("unknown idle traffic must not fail turn start");
    assert_eq!(answer["error"]["code"], -32601);
    assert_eq!(session.phase(), Phase::Running);
    assert!(session.outcome().is_none());

    // The deferred notification surfaces as a tolerated Notice, and the turn
    // still reaches its own ending.
    assert!(matches!(
        session.next_update().await.unwrap(),
        Update::Notice
    ));
    assert!(matches!(
        update(&mut session, &mut peer, end("completed"))
            .await
            .unwrap(),
        Update::TurnEnded
    ));
    assert!(matches!(session.outcome(), Some(Outcome::Completed { .. })));
}

#[tokio::test]
async fn native_codex_session_unknown_traffic_during_a_turn() {
    let (mut session, mut peer) = running().await;
    assert_eq!(session.phase(), Phase::Running);

    assert!(matches!(
        update(&mut session, &mut peer, invented_notification()).await,
        Ok(Update::Notice)
    ));
    let (result, answer) = tokio::join!(session.next_update(), async {
        write(&mut peer, invented_request()).await;
        read(&mut peer.stdin).await
    });
    assert!(matches!(result, Ok(Update::Notice)), "{:?}", result.err());
    assert_eq!(answer["error"]["code"], -32601);
    assert_eq!(session.phase(), Phase::Running);
    assert!(session.outcome().is_none());

    assert!(matches!(
        update(&mut session, &mut peer, end("completed"))
            .await
            .unwrap(),
        Update::TurnEnded
    ));
    assert!(matches!(session.outcome(), Some(Outcome::Completed { .. })));
}

#[tokio::test]
async fn native_codex_session_unknown_traffic_during_shutdown() {
    let (mut session, mut peer) = running().await;
    // The turn ends first; an invented notification and request follow it into
    // the terminal drain.
    peer.stdout
        .write_all(&bytes(&[
            end("completed"),
            invented_notification(),
            invented_request(),
        ]))
        .await
        .unwrap();
    let (result, answer) = tokio::join!(session.next_update(), read(&mut peer.stdin));
    assert!(
        matches!(result, Ok(Update::TurnEnded)),
        "{:?}",
        result.err()
    );
    assert_eq!(answer["error"]["code"], -32601);
    assert!(matches!(session.outcome(), Some(Outcome::Completed { .. })));
}
