use super::*;
use crate::run_controls::{self, Action, Outcome, SubmitError};

type Reader = BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>;
type Writer = tokio::io::WriteHalf<tokio::io::DuplexStream>;
async fn read(reader: &mut Reader) -> Value {
    let mut line = String::new();
    assert_ne!(
        reader.read_line(&mut line).await.unwrap(),
        0,
        "native peer lost its original control"
    );
    serde_json::from_str(&line).unwrap()
}
async fn usage(writer: &mut Writer, thread: &str, turn: &str) {
    emit(writer, value!({"method":"rawResponse/completed","params":{"threadId":thread,"turnId":turn,"responseId":format!("response-{turn}"),"usage":{"inputTokens":10,"outputTokens":0,"cachedInputTokens":0,"cacheWriteInputTokens":0,"reasoningOutputTokens":0,"totalTokens":10}}})).await;
}
async fn terminal(writer: &mut Writer, thread: &str, turn: &str, status: &str) {
    usage(writer, thread, turn).await;
    emit(writer, value!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":turn,"rootTurnId":"turn-a","status":status}}})).await;
}
async fn approval(writer: &mut Writer, cwd: &str) {
    emit(writer, value!({"id":900,"method":"item/commandExecution/requestApproval","params":{"threadId":"thread-a","turnId":"turn-a","itemId":"command-a","command":"touch forbidden","cwd":cwd,"startedAtMs":1}})).await;
}
pub(super) async fn exchange(
    reader: &mut Reader,
    writer: &mut Writer,
    cwd: &str,
    kind: &str,
    count: &AtomicUsize,
) {
    if kind.starts_with("control_stop") {
        approval(writer, cwd).await;
    }
    let control = read(reader).await;
    count.fetch_add(1, Ordering::SeqCst);
    assert_eq!(control["params"]["threadId"], "thread-a");
    if kind.starts_with("control_stop") {
        assert_eq!(control["method"], "turn/interrupt");
        assert_eq!(control["params"]["turnId"], "turn-a");
        match kind {
            "control_stop_no_ack" => {
                tokio::time::sleep(Duration::from_secs(20)).await;
                return;
            }
            "control_stop_refused" => {
                emit(
                    writer,
                    value!({"id":control["id"],"error":{"code":-32000,"message":"Refused"}}),
                )
                .await;
                let decision = read(reader).await;
                assert_eq!(decision["id"], 900);
                assert_eq!(decision["result"]["decision"], "accept");
                terminal(writer, "thread-a", "turn-a", "completed").await;
                return;
            }
            "control_stop_children" => {
                // The root has already accepted stop before the child is
                // announced. Its original tree still owns that later child.
                emit(writer, value!({"id":control["id"],"result":{}})).await;
                emit(writer, value!({"method":"turn/started","params":{"threadId":"child","turn":{"id":"child-turn","rootTurnId":"turn-a","status":"inProgress"}}})).await;
                terminal(writer, "thread-a", "turn-a", "interrupted").await;
                let child = read(reader).await;
                assert_eq!(child["method"], "turn/interrupt");
                assert_eq!(
                    child["params"],
                    value!({"threadId":"child","turnId":"child-turn"})
                );
                count.fetch_add(1, Ordering::SeqCst);
                terminal(writer, "child", "child-turn", "interrupted").await;
                // Root/child completion cannot consume the wrong receipt or
                // finish before this outstanding child acknowledgement.
                tokio::time::sleep(Duration::from_secs(1)).await;
                emit(writer, value!({"id":child["id"],"result":{}})).await;
                return;
            }
            _ => emit(writer, value!({"id":control["id"],"result":{}})).await,
        }
        terminal(writer, "thread-a", "turn-a", "interrupted").await;
    } else {
        assert_eq!(control["method"], "turn/steer");
        assert_eq!(control["params"]["expectedTurnId"], "turn-a");
        assert_eq!(
            control["params"]["input"][0]["text"],
            "add this instruction"
        );
        assert!(
            control["params"]["clientUserMessageId"]
                .as_str()
                .unwrap()
                .len()
                > 64
        );
        if kind == "control_steer" {
            approval(writer, cwd).await;
            let decision = read(reader).await;
            assert_eq!(decision["id"], 900);
            assert_eq!(decision["result"]["decision"], "accept");
        }
        if kind == "control_steer_after_terminal" {
            terminal(writer, "thread-a", "turn-a", "completed").await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        if kind.contains("timeout") {
            tokio::time::sleep(Duration::from_secs(11)).await;
        }
        if kind != "control_steer_timeout" {
            emit(
                writer,
                value!({"id":control["id"],"result":{"turnId":"turn-a"}}),
            )
            .await;
        }
        if kind != "control_steer_after_terminal" {
            terminal(writer, "thread-a", "turn-a", "completed").await;
        }
    }
}

#[tokio::test(start_paused = true)]
async fn native_steer_preserves_callbacks_terminal_order_and_unknown_receipts() {
    for kind in [
        "control_steer",
        "control_steer_after_terminal",
        "control_steer_timeout",
        "control_steer_timeout_late",
    ] {
        let (_temp, mut ledger, profile) = setup(false);
        let (mut session, count, peer) =
            fake(profile.cwd.to_str().unwrap().into(), false, true, 10, kind);
        session.initialize().await.unwrap();
        session
            .open_context(&mut ledger, &scope(), &profile)
            .await
            .unwrap();
        let (sender, inbox) = run_controls::channel(&scope(), "dispatch", "call").unwrap();
        let (queue, mut ui) = approval_queue(2).unwrap();
        let queue = queue.with_run_controls(inbox);
        let (notice, started) = oneshot::channel();
        let host = async {
            started.await.unwrap();
            let action = sender
                .submit(Action::Steer {
                    text: "add this instruction".into(),
                })
                .unwrap();
            if kind == "control_steer" {
                ui.recv().await.unwrap().decide(true);
            }
            assert_eq!(
                action.receipt.await.unwrap(),
                if kind.contains("timeout") {
                    Outcome::Unknown
                } else {
                    Outcome::Accepted
                }
            );
        };
        let bound = scope();
        let run = session.run_with_started(
            &mut ledger,
            "@alice:test",
            &bound,
            "call",
            "dispatch",
            "hello",
            BudgetMode::Postpaid,
            &queue,
            Some(notice),
        );
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(15), async { tokio::join!(run, host) })
                .await
                .unwrap();
        assert_eq!(done.unwrap().status, TurnStatus::Completed);
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "a control is never replayed"
        );
        assert_eq!(
            ledger
                .account(&scope(), Layer::Room, Period::Lifetime, now())
                .unwrap(),
            (10, 0)
        );
        assert_eq!(session.can_continue(), kind != "control_steer_timeout");
        assert!(matches!(
            sender.submit(Action::Interrupt),
            Err(SubmitError::Inactive)
        ));
        drop(session);
        peer.await.unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn native_stop_withdraws_owner_wait_and_follows_children_after_root_completion() {
    for kind in [
        "control_stop_owner",
        "control_stop_children",
        "control_stop_refused",
        "control_stop_no_ack",
    ] {
        let (_temp, mut ledger, profile) = setup(false);
        let (mut session, count, peer) =
            fake(profile.cwd.to_str().unwrap().into(), false, true, 10, kind);
        session.initialize().await.unwrap();
        session
            .open_context(&mut ledger, &scope(), &profile)
            .await
            .unwrap();
        let (sender, inbox) = run_controls::channel(&scope(), "dispatch", "call").unwrap();
        let (queue, mut ui) = approval_queue(2).unwrap();
        let queue = queue.with_run_controls(inbox);
        let host = async {
            let pending = ui.recv().await.unwrap();
            let stop = sender.submit(Action::Interrupt).unwrap();
            let expected = match kind {
                "control_stop_refused" => Outcome::Rejected,
                "control_stop_no_ack" => Outcome::Unknown,
                _ => Outcome::Accepted,
            };
            assert_eq!(stop.receipt.await.unwrap(), expected);
            if kind == "control_stop_refused" {
                assert!(!pending.is_closed());
                pending.decide(true);
            } else {
                while !pending.is_closed() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                pending.decide(true);
            }
        };
        let bound = scope();
        let run = session.run(
            &mut ledger,
            "@alice:test",
            &bound,
            "call",
            "dispatch",
            "hello",
            BudgetMode::Postpaid,
            &queue,
        );
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(15), async { tokio::join!(run, host) })
                .await
                .unwrap();
        if kind == "control_stop_no_ack" {
            assert!(matches!(done, Err(Error::Timeout)));
            assert!(!session.can_continue());
            assert_eq!(
                ledger
                    .account(&scope(), Layer::Room, Period::Lifetime, now())
                    .unwrap(),
                (0, 0)
            );
        } else {
            let done = done.unwrap();
            assert_eq!(
                done.status,
                if kind == "control_stop_refused" {
                    TurnStatus::Completed
                } else {
                    TurnStatus::Interrupted
                }
            );
            assert!(session.can_continue());
            let total = if kind == "control_stop_children" {
                20
            } else {
                10
            };
            assert_eq!(done.usage.input, total);
            assert_eq!(
                ledger
                    .account(&scope(), Layer::Room, Period::Lifetime, now())
                    .unwrap(),
                (total, 0)
            );
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            if kind == "control_stop_children" {
                2
            } else {
                1
            }
        );
        assert!(matches!(
            sender.submit(Action::Interrupt),
            Err(SubmitError::Inactive)
        ));
        drop(session);
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn wrong_or_previously_claimed_control_binding_cannot_admit_a_provider_call() {
    let (_temp, mut ledger, profile) = setup(false);
    let (mut session, _, peer) = fake(
        profile.cwd.to_str().unwrap().into(),
        false,
        true,
        10,
        "command",
    );
    session.initialize().await.unwrap();
    session
        .open_context(&mut ledger, &scope(), &profile)
        .await
        .unwrap();
    let (_, inbox) = run_controls::channel(&scope(), "dispatch", "call").unwrap();
    let (queue, _) = approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox.clone());
    assert!(matches!(
        session
            .run(
                &mut ledger,
                "@alice:test",
                &scope(),
                "other",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue
            )
            .await,
        Err(Error::Protocol("run control execution differs"))
    ));
    drop(inbox.claim(&scope(), "dispatch", "call").unwrap());
    assert!(matches!(
        session
            .run(
                &mut ledger,
                "@alice:test",
                &scope(),
                "call",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue
            )
            .await,
        Err(Error::Protocol("run controls already used"))
    ));
    assert!(ledger.outstanding_calls("@alice:test").unwrap().is_empty());
    assert_eq!(
        ledger
            .account(&scope(), Layer::Room, Period::Lifetime, now())
            .unwrap(),
        (0, 0)
    );
    drop(session);
    peer.await.unwrap();
}
