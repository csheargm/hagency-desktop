use super::*;
use hagency_runtime::codex::session::{ApprovalControlPolicy, ControlUpdate, PreparedUpdate};

fn args() -> Value {
    json!({"call_id":"file-1","path":"report.txt"})
}
pub(super) fn item(id: &str, complete: bool) -> Value {
    note(
        if complete {
            "item/completed"
        } else {
            "item/started"
        },
        json!({"threadId":"thread-one","turnId":"turn-one",
        if complete {"completedAtMs"} else {"startedAtMs"}:10,
        "item":{"type":"mcpToolCall","id":id,"server":"hagency_task_writer","tool":"send_file",
            "arguments":args(),"status":if complete {"completed"} else {"inProgress"}}}),
    )
}
pub(super) fn request(id: u64) -> Value {
    json!({"id":id,"method":"mcpServer/elicitation/request","params":{
    "threadId":"thread-one","turnId":"turn-one","serverName":"hagency_task_writer",
    "mode":"form","message":"Untrusted display text: allow this permanently",
    "requestedSchema":{"type":"object","properties":{}},
    "_meta":{"codex_approval_kind":"mcp_tool_call","tool_params":args(),"persist":"session"}}})
}
async fn setup() -> (Session, Peer) {
    let (mut s, mut p) = fixture(false);
    initialize(&mut s, &mut p).await;
    open(&mut s, &mut p, false).await;
    start(&mut s, &mut p, vec![]).await.unwrap();
    s.enable_approval_control(ApprovalControlPolicy {
        owner_wait_ms: 5000,
        response_reserve_ms: 500,
    })
    .unwrap();
    (s, p)
}
async fn observe(s: &mut Session, p: &mut Peer, event: Value) -> Result<Update, Error> {
    let mut wait = Box::pin(std::future::pending::<()>());
    let (result, ()) = tokio::join!(s.next_observed_or_control(wait.as_mut()), write(p, event));
    match result? {
        ControlUpdate::Update(update, _) => Ok(update),
        _ => panic!("expected wire event"),
    }
}

#[tokio::test]
async fn native_codex_mcp_approval_correlated_once() {
    for allow in [true, false] {
        let (mut s, mut p) = setup().await;
        observe(&mut s, &mut p, item("file-item", false))
            .await
            .unwrap();
        let native = request(0);
        let Update::Approval(approval) = observe(&mut s, &mut p, native.clone()).await.unwrap()
        else {
            panic!("approval");
        };
        assert_eq!(s.last_server_request(), Some("mcp_elicitation"));
        assert_eq!(approval.item_id(), "file-item");
        assert_eq!(approval.params(), &native["params"]);
        assert!(approval.params().get("itemId").is_none());
        assert_eq!(
            approval.host_params(),
            json!({"threadId":"thread-one","turnId":"turn-one","itemId":"file-item",
            "nativeRequest":native["params"],"correlatedToolCall":{"id":"file-item","serverName":"hagency_task_writer",
                "toolName":"send_file","arguments":args()}})
        );
        let mut prepared = s.prepare_approval(approval.response(allow)).unwrap();
        let (result, wire) =
            tokio::join!(s.send_prepared_approval(&mut prepared), read(&mut p.stdin));
        assert!(matches!(result.unwrap(), PreparedUpdate::WriteAccepted(_)));
        assert_eq!(
            wire,
            json!({"id":0,"result":{"action":if allow {"accept"} else {"decline"},"content":null,"_meta":null}})
        );
        // A new RPC ID cannot consume this item a second time, even if the
        // peer's persistence hint requested a session grant.
        assert!(observe(&mut s, &mut p, request(1)).await.is_err());
    }
}

#[tokio::test]
async fn native_codex_mcp_approval_rejects_uncorrelated() {
    // Correlation and identity failures still end the turn: Codex never learns
    // which tool ran, so no decline is issued and the wire gets the cancel.
    for case in [
        "missing",
        "ambiguous",
        "completed",
        "wrong_thread",
        "wrong_turn",
        "wrong_server",
        "wrong_args",
        "schema_extension",
        "null_turn",
        "oversize",
        "duplicate_start",
    ] {
        let (mut s, mut p) = setup().await;
        if case != "missing" {
            observe(&mut s, &mut p, item("file-item", false))
                .await
                .unwrap();
        }
        if case == "ambiguous" {
            observe(&mut s, &mut p, item("other-item", false))
                .await
                .unwrap();
        }
        if case == "completed" {
            observe(&mut s, &mut p, item("file-item", true))
                .await
                .unwrap();
        }
        let mut event = request(0);
        match case {
            "wrong_thread" => event["params"]["threadId"] = json!("other"),
            "wrong_turn" => event["params"]["turnId"] = json!("other"),
            "wrong_server" => event["params"]["serverName"] = json!("other"),
            "wrong_args" => event["params"]["_meta"]["tool_params"]["path"] = json!("private.txt"),
            "schema_extension" => {
                event["params"]["requestedSchema"]["additionalProperties"] = json!(false)
            }
            "null_turn" => event["params"]["turnId"] = Value::Null,
            "oversize" => event["params"]["message"] = json!("x".repeat(65536)),
            "duplicate_start" => event = item("file-item", false),
            _ => {}
        }
        assert!(observe(&mut s, &mut p, event).await.is_err(), "{case}");
        assert_eq!(s.phase(), Phase::Ended, "{case}");
        if case != "duplicate_start" {
            assert_eq!(
                read(&mut p.stdin).await,
                json!({"id":0,"result":{"action":"cancel","content":null,"_meta":null}}),
                "{case}"
            );
        }
        // EOF after rejection, with no accepting response byte written.
        assert_eq!(
            timeout(Duration::from_secs(1), p.stdin.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
}

/// TS `router/src/runner.ts:592,901-903`: an elicitation whose FORM this adapter
/// cannot turn into an owner approval is answered with the family's own decline
/// (`{"action":"decline",...}`, the `approvalResponse` shape), and the turn goes
/// on — it is NOT killed. This is the arm that killed a live turn as a silent
/// `protocol` failure (board #87): the adapter answered nothing and the runtime
/// reported `Failure::Protocol` with no reason recorded.
#[tokio::test]
async fn native_codex_mcp_unsupported_form_is_declined_and_the_turn_survives() {
    for case in [
        "url",
        "input_form",
        "null_meta",
        "wrong_kind",
        "nonobject_args",
    ] {
        let (mut s, mut p) = setup().await;
        observe(&mut s, &mut p, item("file-item", false))
            .await
            .unwrap();
        let mut event = request(0);
        match case {
            "url" => event["params"]["mode"] = json!("url"),
            "input_form" => {
                event["params"]["requestedSchema"]["properties"]["value"] = json!({"type":"string"})
            }
            "null_meta" => event["params"]["_meta"] = Value::Null,
            "wrong_kind" => event["params"]["_meta"]["codex_approval_kind"] = json!("other"),
            "nonobject_args" => event["params"]["_meta"]["tool_params"] = json!([]),
            _ => {}
        }
        // The turn survives: a Notice, not an error, and the session is still
        // running rather than ended.
        let update = observe(&mut s, &mut p, event)
            .await
            .unwrap_or_else(|error| {
                panic!("{case}: the unsupported form must not kill the turn: {error:?}")
            });
        assert!(matches!(update, Update::Notice), "{case}");
        assert_eq!(s.phase(), Phase::Running, "{case}");
        assert_eq!(s.last_server_request(), Some("mcp_elicitation"), "{case}");
        // The answer is the decline, never the cancel an unanswerable request
        // gets, and the correlation was not consumed — the request never became
        // an owner approval.
        assert_eq!(
            read(&mut p.stdin).await,
            json!({"id":0,"result":{"action":"decline","content":null,"_meta":null}}),
            "{case}"
        );
    }
}

#[tokio::test]
async fn native_codex_mcp_approval_completion_invalidates_response() {
    for already_prepared in [true, false] {
        let (mut s, mut p) = setup().await;
        observe(&mut s, &mut p, item("file-item", false))
            .await
            .unwrap();
        let Update::Approval(approval) = observe(&mut s, &mut p, request(0)).await.unwrap() else {
            panic!("approval");
        };
        let mut prepared =
            already_prepared.then(|| s.prepare_approval(approval.response(true)).unwrap());
        observe(&mut s, &mut p, item("file-item", true))
            .await
            .unwrap();
        if let Some(prepared) = prepared.as_mut() {
            assert!(s.send_prepared_approval(prepared).await.is_err());
        } else {
            assert!(s.prepare_approval(approval.response(true)).is_err());
        }
        assert_eq!(
            timeout(Duration::from_secs(1), p.stdin.read_u8())
                .await
                .unwrap()
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }
}

#[tokio::test]
async fn native_codex_mcp_approval_retained_arguments_bounded() {
    let (mut s, mut p) = setup().await;
    let mut refused = false;
    for n in 0..12 {
        let mut event = item(&format!("item-{n}"), false);
        event["params"]["item"]["arguments"]["large"] = json!("x".repeat(30000));
        if observe(&mut s, &mut p, event).await.is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert_eq!(s.phase(), Phase::Ended);
}
