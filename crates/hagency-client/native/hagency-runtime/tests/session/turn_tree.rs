use super::*;
use hagency_runtime::codex::session::{Observation, ApprovalControlPolicy, PreparedUpdate};

fn child(event: Value) -> Value {
    let mut event = event;
    event["params"]["threadId"] = "child".into();
    if event["params"].get("turnId").is_some() {
        event["params"]["turnId"] = "child-turn".into();
    }
    if event["params"].get("turn").is_some() {
        event["params"]["turn"]["id"] = "child-turn".into();
        event["params"]["turn"]["rootTurnId"] = "turn-one".into();
    }
    event
}
fn child_start() -> Value {
    child(note(
        "turn/started",
        json!({"threadId":"thread-one","turn":turn("inProgress")}),
    ))
}
fn receipt(id: &str) -> Value {
    note(
        "rawResponse/completed",
        json!({"threadId":"thread-one","turnId":"turn-one","responseId":id,"usage":{
        "totalTokens":14,"inputTokens":10,"outputTokens":4,"cachedInputTokens":2,"reasoningOutputTokens":1}}),
    )
}

#[tokio::test]
async fn native_turn_context_requires_live_ancestry_and_original_connection() {
    let (mut s, mut p) = running().await;
    let source = s.observation_source().unwrap();
    assert!(source.matches_root("thread-one", "turn-one"));
    assert!(s.native_turn_context("child", "child-turn").is_err());
    observed(&mut s, &mut p, child_start()).await;
    let child_context = s.native_turn_context("child", "child-turn").unwrap();
    assert!(child_context.source() == &source);
    let (other, _) = running().await;
    assert!(child_context.source() != &other.observation_source().unwrap());
    assert!(s.native_turn_context("child", "another-turn").is_err());
    observed(&mut s, &mut p, child(end("completed"))).await;
    assert!(s.native_turn_context("child", "child-turn").is_err());
    assert!(s.native_turn_context("thread-one", "turn-one").is_ok());
    s.close();
    assert!(child_context.source().is_retired());
    assert!(!source.matches_root("thread-one", "turn-one"));
}
async fn observed(s: &mut Session, p: &mut Peer, event: Value) -> (Update, Observation) {
    let (result, ()) = tokio::join!(s.next_observed_update(), write(p, event));
    result.unwrap()
}
#[tokio::test]
async fn descendants_finish_under_original_source_without_text_or_usage_replay() {
    let (mut s, mut p) = running().await;
    let source = s.observation_source().unwrap();
    let mut sequence = 0;
    let events = [
        child_start(),
        receipt("root-response"),
        receipt("root-response"),
        item_event("answer", "", false),
        delta("answer", "ROOT"),
        item_event("answer", "ROOT", true),
        end("completed"),
        child(item_event("answer", "", false)),
        child(delta("answer", "PRIVATE CHILD")),
        child(item_event("answer", "PRIVATE CHILD", true)),
        child(note(
            "thread/tokenUsage/updated",
            json!({"threadId":"thread-one","turnId":"turn-one","tokenUsage":{"total":{"totalTokens":999999}}}),
        )),
        child(receipt("child-response")),
        child(end("completed")),
    ];
    let mut text = String::new();
    for (index, event) in events.into_iter().enumerate() {
        let (update, observation) = observed(&mut s, &mut p, event).await;
        sequence += 1;
        assert!(observation.source() == &source);
        assert_eq!(observation.sequence(), sequence);
        if let Update::TextDelta { delta, .. } = &update {
            text.push_str(delta);
        }
        if index == 12 {
            assert!(matches!(update, Update::TurnEnded));
            let usage = observation.terminal_usage().unwrap();
            assert_eq!(usage.total().total_tokens(), Some(28));
            assert_eq!(usage.total().cached_input_tokens(), Some(4));
            assert!(!usage.diagnostics().has_missing_fields());
        } else {
            assert!(!matches!(update, Update::TurnEnded));
            assert_eq!(s.phase(), Phase::Running);
            assert!(s.outcome().is_none());
        }
    }
    assert_eq!(text, "ROOT");
    assert!(matches!(s.outcome(),Some(Outcome::Completed{text}) if text=="ROOT"));
}
#[tokio::test]
async fn missing_child_receipt_does_not_turn_parent_usage_into_complete_measurement() {
    for missing_root in [false, true] {
        let (mut s, mut p) = running().await;
        observed(&mut s, &mut p, child_start()).await;
        observed(
            &mut s,
            &mut p,
            if missing_root {
                child(receipt("child"))
            } else {
                receipt("root")
            },
        )
        .await;
        observed(&mut s, &mut p, end("completed")).await;
        let (update, observation) = observed(&mut s, &mut p, child(end("completed"))).await;
        assert!(matches!(update, Update::TurnEnded));
        let usage = observation.terminal_usage().unwrap();
        assert!(usage.diagnostics().has_missing_fields());
        assert_eq!(usage.total().total_tokens(), None);
    }
}
#[tokio::test]
async fn child_mcp_approval_keeps_native_scope_and_cannot_borrow_root_item() {
    for root_only in [false, true] {
        let (mut s, mut p) = running().await;
        s.enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 5000,
            response_reserve_ms: 500,
        })
        .unwrap();
        observed(&mut s, &mut p, child_start()).await;
        let item = super::mcp_approval::item("same-item", false);
        observed(&mut s, &mut p, item.clone()).await;
        if !root_only {
            observed(&mut s, &mut p, child(item)).await;
        }
        let request = child(super::mcp_approval::request(0));
        if root_only {
            let (result, ()) = tokio::join!(s.next_observed_update(), write(&mut p, request));
            assert!(matches!(result, Err(Error::Scope)));
            assert_eq!(read(&mut p.stdin).await["result"]["action"], "cancel");
            continue;
        }
        // Root ends while the child's exact owner callback is still possible.
        observed(&mut s, &mut p, super::mcp_approval::item("same-item", true)).await;
        observed(&mut s, &mut p, end("completed")).await;
        let (Update::Approval(request), _) = observed(&mut s, &mut p, request).await else {
            panic!("child approval")
        };
        assert_eq!(request.thread_id(), "child");
        assert_eq!(request.turn_id(), "child-turn");
        assert_eq!(request.item_id(), "same-item");
        let mut prepared = s.prepare_approval(request.response(true)).unwrap();
        let (result, wire) =
            tokio::join!(s.send_prepared_approval(&mut prepared), read(&mut p.stdin));
        assert!(matches!(result.unwrap(), PreparedUpdate::WriteAccepted(_)));
        assert_eq!(wire["result"]["action"], "accept");
    }
}
#[tokio::test]
async fn foreign_root_binding_and_finished_child_callback_are_refused() {
    for finished in [false, true] {
        let (mut s, mut p) = running().await;
        s.enable_approvals().unwrap();
        let event = if finished {
            observed(&mut s, &mut p, child_start()).await;
            observed(&mut s, &mut p, child(end("completed"))).await;
            child(super::mcp_approval::request(0))
        } else {
            let mut event = child_start();
            event["params"]["turn"]["rootTurnId"] = "other-root".into();
            event
        };
        let (result, ()) = tokio::join!(s.next_observed_update(), write(&mut p, event));
        assert!(matches!(result, Err(Error::Scope)));
    }
}

#[tokio::test]
async fn completed_message_forwards_only_text_not_already_streamed() {
    for streamed in ["", "First "] {
        let (mut s, mut p) = running().await;
        observed(&mut s, &mut p, item_event("answer", "", false)).await;
        if !streamed.is_empty() {
            observed(&mut s, &mut p, delta("answer", streamed)).await;
        }
        let (update, _) =
            observed(&mut s, &mut p, item_event("answer", "First answer", true)).await;
        let Update::TextDelta { delta, .. } = update else {
            panic!("unseen final suffix");
        };
        assert_eq!(streamed.to_owned() + &delta, "First answer");
        observed(&mut s, &mut p, end("completed")).await;
    }
}

#[tokio::test]
async fn prepared_child_grant_becomes_unsendable_after_that_child_ends() {
    let (mut s, mut p) = running().await;
    s.enable_approval_control(ApprovalControlPolicy {
        owner_wait_ms: 5000,
        response_reserve_ms: 500,
    })
    .unwrap();
    observed(&mut s, &mut p, child_start()).await;
    let request = json!({"id":0,"method":"item/fileChange/requestApproval","params":{
        "threadId":"child","turnId":"child-turn","itemId":"change","startedAtMs":1,"reason":"test","grantRoot":cwd()}});
    let (Update::Approval(request), _) = observed(&mut s, &mut p, request).await else {
        panic!("file approval");
    };
    let mut prepared = s.prepare_approval(request.response(true)).unwrap();
    observed(&mut s, &mut p, child(end("completed"))).await;
    assert_eq!(s.phase(), Phase::Running);
    assert!(matches!(
        s.send_prepared_approval(&mut prepared).await,
        Err(Error::Scope)
    ));
    assert_eq!(
        p.stdin.read_u8().await.unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[tokio::test]
async fn child_stream_eof_preserves_validated_root_answer_with_unknown_usage() {
    for controlled in [false, true] {
        let (mut s, mut p) = running().await;
        let source = s.observation_source().unwrap();
        observed(&mut s, &mut p, child_start()).await;
        observed(&mut s, &mut p, receipt("root")).await;
        observed(&mut s, &mut p, item_event("answer", "", false)).await;
        observed(&mut s, &mut p, item_event("answer", "ROOT", true)).await;
        observed(&mut s, &mut p, end("completed")).await;
        p.stdout.shutdown().await.unwrap();
        let (update, observation) = if controlled {
            let mut never = Box::pin(std::future::pending::<()>());
            let hagency_runtime::codex::session::ControlUpdate::Update(update, observation) =
                s.next_observed_or_control(never.as_mut()).await.unwrap() else { panic!("terminal answer"); };
            (update, *observation)
        } else { s.next_observed_update().await.unwrap() };
        assert!(matches!(update, Update::TurnEnded));
        assert!(observation.source() == &source);
        assert!(matches!(s.outcome(), Some(Outcome::Completed { text }) if text == "ROOT"));
        assert!(observation.terminal_usage().unwrap().diagnostics().has_missing_fields());
        assert_eq!(observation.terminal_usage().unwrap().total().total_tokens(), None);
        assert_eq!(s.transport_termination().unwrap().cause, TransportError::PeerEof);
        assert!(s.next_update().await.is_err());
    }
}
