use super::*;
use hagency_runtime::codex::session::ObservationKind;

fn command(complete: bool) -> Value {
    note(
        if complete {
            "item/completed"
        } else {
            "item/started"
        },
        json!({
            "threadId":"thread-one","turnId":"turn-one",
            if complete {"completedAtMs"} else {"startedAtMs"}:10,
            "item":{"id":"command-one","type":"commandExecution","command":"sleep 20","cwd":cwd(),
                "status":if complete {"completed"} else {"inProgress"},"exitCode":if complete {Some(0)} else {None}}
        }),
    )
}
fn terminal(stdin: Value) -> Value {
    note(
        "item/commandExecution/terminalInteraction",
        json!({
            "threadId":"thread-one","turnId":"turn-one","itemId":"command-one","processId":"321","stdin":stdin
        }),
    )
}

#[tokio::test]
async fn native_codex_session_terminal_progress() {
    let (mut session, mut peer) = running().await;
    update(&mut session, &mut peer, command(false))
        .await
        .unwrap();
    for stdin in ["", "input\n\u{3}"] {
        let (result, ()) = tokio::join!(
            session.next_observed_update(),
            write(&mut peer, terminal(json!(stdin)))
        );
        let (update, observation) = result.unwrap();
        assert!(matches!(update, Update::Progress));
        assert!(matches!(observation.kind(), ObservationKind::Ignored));
        assert_eq!(session.phase(), Phase::Running);
        assert!(session.outcome().is_none());
        assert_eq!(session.item_count(), 1);
        assert_eq!(session.text_bytes(), 0);
    }
    update(&mut session, &mut peer, command(true))
        .await
        .unwrap();
    assert!(matches!(
        update(&mut session, &mut peer, end("completed"))
            .await
            .unwrap(),
        Update::TurnEnded
    ));

    for case in [
        "thread",
        "turn",
        "item",
        "absent",
        "completed",
        "wrong_kind",
        "process",
        "stdin",
        "large",
    ] {
        let (mut session, mut peer) = running().await;
        if case != "absent" {
            let mut item = command(false);
            if case == "wrong_kind" {
                item["params"]["item"]["type"] = "reasoning".into();
            }
            update(&mut session, &mut peer, item).await.unwrap();
            if case == "completed" {
                update(&mut session, &mut peer, command(true))
                    .await
                    .unwrap();
            }
        }
        let mut event = terminal(json!(""));
        let expected = match case {
            "thread" => {
                event["params"]["threadId"] = "other".into();
                Some(Error::Scope)
            }
            "turn" => {
                event["params"]["turnId"] = "other".into();
                Some(Error::Scope)
            }
            // A unified-exec terminal outlives the command item that opened it
            // (observed live with Codex 0.166): late interaction for an unknown
            // or finished command is ignored progress, never a scope failure
            // that ends the run.
            "item" => {
                event["params"]["itemId"] = "other".into();
                None
            }
            "absent" | "completed" => None,
            "wrong_kind" => Some(Error::Scope),
            "process" => {
                event["params"]["processId"] = json!(321);
                Some(Error::Malformed)
            }
            "stdin" => {
                event["params"]["stdin"] = Value::Null;
                Some(Error::Malformed)
            }
            "large" => {
                event["params"]["stdin"] = json!("x".repeat(MAX_TEXT_BYTES + 1));
                Some(Error::Capacity)
            }
            _ => unreachable!("every listed case is matched above"),
        };
        let result = update(&mut session, &mut peer, event).await;
        match expected {
            Some(expected) => {
                assert_eq!(result.err(), Some(expected), "{case}");
                unknown(&session, expected);
                assert_eq!(session.refused_notification(), Some("terminal_interaction"));
            }
            None => assert!(result.is_ok(), "{case}: late terminal progress is ignored"),
        }
    }
    // Board #94: an invented notification method during an active command is
    // logged and ignored; the command's own scope checks above stay strict.
    let (mut session, mut peer) = running().await;
    update(&mut session, &mut peer, command(false))
        .await
        .unwrap();
    let invented = note(
        "private-peer-method",
        json!({"threadId":"thread-one","turnId":"turn-one"}),
    );
    assert!(matches!(
        update(&mut session, &mut peer, invented).await,
        Ok(Update::Notice)
    ));
    assert_eq!(session.phase(), Phase::Running);
    assert!(session.outcome().is_none());
}

/// Live 2026-11-07: a website-building turn started a dev server that kept
/// running, and Codex completed the turn without completing that command. The
/// answer must be kept; an agent message still active stays a contradiction.
#[tokio::test]
async fn native_codex_session_turn_completes_with_a_command_still_running() {
    let (mut session, mut peer) = running().await;
    update(&mut session, &mut peer, command(false)).await.unwrap();
    update(&mut session, &mut peer, item_event("answer", "site ready", false)).await.unwrap();
    update(&mut session, &mut peer, item_event("answer", "site ready", true)).await.unwrap();
    assert!(matches!(
        update(&mut session, &mut peer, end("completed")).await.unwrap(),
        Update::TurnEnded
    ));
    assert!(matches!(session.outcome(), Some(Outcome::Completed { text }) if text == "site ready"));

    let (mut session, mut peer) = running().await;
    update(&mut session, &mut peer, item_event("answer", "partial", false)).await.unwrap();
    assert_eq!(
        update(&mut session, &mut peer, end("completed")).await.err(),
        Some(Error::State),
        "an unfinished answer still contradicts a completed turn"
    );
}

fn plain_item(kind: &str, id: &str, complete: bool) -> Value {
    note(
        if complete { "item/completed" } else { "item/started" },
        json!({
            "threadId": "thread-one", "turnId": "turn-one",
            if complete { "completedAtMs" } else { "startedAtMs" }: 10,
            "item": { "type": kind, "id": id }
        }),
    )
}

/// Live 2026-11-13, pooled Codex Agent on Codex 0.167: an instruction added
/// from the run card while the answer was streaming ended the run as
/// interrupted. Measured offline on 0.167.2: on `turn/steer` the app server
/// drops the model request it had open, never completes the answer or
/// reasoning item that request had started, and completes the turn with the
/// new answer only. That abandoned item must not contradict the completion,
/// and its text is not part of the answer.
#[tokio::test]
async fn native_codex_session_output_cut_short_by_added_input_does_not_contradict_completion() {
    for abandoned in ["agentMessage", "reasoning"] {
        let (mut session, mut peer) = running().await;
        update(&mut session, &mut peer, plain_item("userMessage", "first", false)).await.unwrap();
        update(&mut session, &mut peer, plain_item("userMessage", "first", true)).await.unwrap();
        if abandoned == "agentMessage" {
            update(&mut session, &mut peer, item_event("draft", "", false)).await.unwrap();
            update(&mut session, &mut peer, delta("draft", "PARTIAL ")).await.unwrap();
        } else {
            update(&mut session, &mut peer, plain_item("reasoning", "draft", false)).await.unwrap();
        }
        update(&mut session, &mut peer, plain_item("userMessage", "added", false)).await.unwrap();
        update(&mut session, &mut peer, plain_item("userMessage", "added", true)).await.unwrap();
        update(&mut session, &mut peer, item_event("answer", "", false)).await.unwrap();
        update(&mut session, &mut peer, item_event("answer", "FINAL", true)).await.unwrap();
        assert!(
            matches!(
                update(&mut session, &mut peer, end("completed")).await.unwrap(),
                Update::TurnEnded
            ),
            "{abandoned}"
        );
        assert!(
            matches!(session.outcome(), Some(Outcome::Completed { text }) if text == "FINAL"),
            "{abandoned}: the abandoned text is not part of the answer"
        );
    }

    // An answer that started after the added input and never completed is not
    // covered: nothing cut it short.
    let (mut session, mut peer) = running().await;
    update(&mut session, &mut peer, plain_item("userMessage", "added", false)).await.unwrap();
    update(&mut session, &mut peer, plain_item("userMessage", "added", true)).await.unwrap();
    update(&mut session, &mut peer, item_event("answer", "partial", false)).await.unwrap();
    assert_eq!(
        update(&mut session, &mut peer, end("completed")).await.err(),
        Some(Error::State)
    );

    // A command that was running when the input was added is not model output
    // and stays under its own rule; an abandoned answer beside it is fine.
    let (mut session, mut peer) = running().await;
    update(&mut session, &mut peer, plain_item("fileChange", "edit", false)).await.unwrap();
    update(&mut session, &mut peer, plain_item("userMessage", "added", false)).await.unwrap();
    update(&mut session, &mut peer, plain_item("userMessage", "added", true)).await.unwrap();
    assert_eq!(
        update(&mut session, &mut peer, end("completed")).await.err(),
        Some(Error::State),
        "added input does not excuse an unfinished file change"
    );
}
