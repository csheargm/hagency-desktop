use super::*;
use hagency_runtime::codex::session::{RunAction, RunOutcome};

async fn control(s: &mut Session, p: &mut Peer, id: &str, action: RunAction) -> Value {
    assert_eq!(
        s.submit_run_control(id.into(), action).await.unwrap(),
        RunOutcome::Pending
    );
    read(&mut p.stdin).await
}

#[tokio::test]
async fn steer_receipt_is_native_scoped_and_never_replays() {
    let (mut s, mut p) = running().await;
    let sent = control(
        &mut s,
        &mut p,
        "append",
        RunAction::Steer {
            text: "keep the original task".into(),
        },
    )
    .await;
    assert_eq!(sent["method"], "turn/steer");
    assert_eq!(sent["params"]["expectedTurnId"], "turn-one");
    assert_eq!(sent["params"]["clientUserMessageId"], "append");
    assert!(s.run_control_receipts().is_empty());
    assert_eq!(
        s.submit_run_control(
            "append".into(),
            RunAction::Steer {
                text: "do not replay".into()
            }
        )
        .await
        .unwrap(),
        RunOutcome::InProgress
    );
    update(
        &mut s,
        &mut p,
        json!({"id":sent["id"],"result":{"turnId":"turn-one"}}),
    )
    .await
    .unwrap();
    let receipts = s.run_control_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].command_id, "append");
    assert_eq!(receipts[0].outcome, RunOutcome::Accepted);
    assert_eq!(s.phase(), Phase::Running);
    assert!(matches!(
        update(&mut s, &mut p, end("completed")).await.unwrap(),
        Update::TurnEnded
    ));
}

#[tokio::test(start_paused = true)]
async fn soft_steer_timeout_preserves_the_turn_and_correlates_late_ack() {
    let (mut s, mut p) = running().await;
    let sent = control(
        &mut s,
        &mut p,
        "append",
        RunAction::Steer {
            text: "extra".into(),
        },
    )
    .await;
    tokio::time::advance(Duration::from_secs(11)).await;
    let receipts = s.run_control_receipts();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].outcome, RunOutcome::Unknown);
    update(&mut s, &mut p, item_event("answer", "", false))
        .await
        .unwrap();
    assert!(matches!(
        update(&mut s, &mut p, delta("answer", "still running"))
            .await
            .unwrap(),
        Update::TextDelta { .. }
    ));
    update(
        &mut s,
        &mut p,
        json!({"id":sent["id"],"result":{"turnId":"turn-one"}}),
    )
    .await
    .unwrap();
    assert!(
        s.run_control_receipts().is_empty(),
        "late ACK does not emit a second outcome"
    );
    update(&mut s, &mut p, item_event("answer", "still running", true)).await.unwrap();
    assert!(matches!(
        update(&mut s, &mut p, end("completed")).await.unwrap(),
        Update::TurnEnded
    ));
}

#[tokio::test]
async fn terminal_waits_for_the_original_control_ack() {
    for stop in [false, true] {
        let (mut s, mut p) = running().await;
        let action = if stop {
            RunAction::Interrupt
        } else {
            RunAction::Steer {
                text: "extra".into(),
            }
        };
        let sent = control(&mut s, &mut p, "control", action).await;
        assert!(!matches!(
            update(
                &mut s,
                &mut p,
                end(if stop { "interrupted" } else { "completed" })
            )
            .await
            .unwrap(),
            Update::TurnEnded
        ));
        assert_eq!(s.phase(), Phase::Running);
        let result = if stop {
            json!({})
        } else {
            json!({"turnId":"turn-one"})
        };
        assert!(matches!(
            update(&mut s, &mut p, json!({"id":sent["id"],"result":result}))
                .await
                .unwrap(),
            Update::TurnEnded
        ));
        assert_eq!(s.run_control_receipts()[0].outcome, RunOutcome::Accepted);
        assert_eq!(s.phase(), Phase::Ended);
    }
}

#[tokio::test]
async fn refused_stop_releases_the_original_host_wait() {
    let (mut s, mut p) = running().await;
    let sent = control(&mut s, &mut p, "stop", RunAction::Interrupt).await;
    assert!(s.run_controls_hold_answers());
    assert!(!s.run_stop_accepted());
    update(
        &mut s,
        &mut p,
        json!({"id":sent["id"],"error":{"code":-32000,"message":"refused"}}),
    )
    .await
    .unwrap();
    assert!(!s.run_controls_hold_answers());
    assert!(!s.run_stop_accepted());
    assert_eq!(s.run_control_receipts()[0].outcome, RunOutcome::Rejected);
    let retry = control(&mut s, &mut p, "another-stop", RunAction::Interrupt).await;
    update(&mut s, &mut p, json!({"id":retry["id"],"result":{}}))
        .await
        .unwrap();
    assert!(s.run_stop_accepted());
}

#[tokio::test]
async fn accepted_stop_follows_a_late_child_and_waits_for_its_ack() {
    let (mut s, mut p) = running().await;
    let sent = control(&mut s, &mut p, "stop", RunAction::Interrupt).await;
    update(&mut s, &mut p, json!({"id":sent["id"],"result":{}}))
        .await
        .unwrap();
    assert_eq!(s.run_control_receipts()[0].outcome, RunOutcome::Accepted);
    update(&mut s, &mut p, note("turn/started", json!({"threadId":"child","turn":{"id":"child-turn","rootTurnId":"turn-one","status":"inProgress","items":[]}}))).await.unwrap();
    assert!(!matches!(
        update(&mut s, &mut p, end("interrupted")).await.unwrap(),
        Update::TurnEnded
    ));
    let child = read(&mut p.stdin).await;
    assert_eq!(child["method"], "turn/interrupt");
    assert_eq!(
        child["params"],
        json!({"threadId":"child","turnId":"child-turn"})
    );
    assert!(!matches!(update(&mut s, &mut p, note("turn/completed", json!({"threadId":"child","turn":{"id":"child-turn","rootTurnId":"turn-one","status":"interrupted","items":[]}}))).await.unwrap(), Update::TurnEnded));
    assert!(matches!(
        update(&mut s, &mut p, json!({"id":child["id"],"result":{}}))
            .await
            .unwrap(),
        Update::TurnEnded
    ));
    assert!(matches!(s.outcome(), Some(Outcome::Interrupted)));
    assert!(s.run_control_receipts().is_empty());
}

#[tokio::test(start_paused = true)]
async fn lost_stop_ack_and_wrong_turn_ack_never_prove_success() {
    let (mut s, mut p) = running().await;
    let sent = control(
        &mut s,
        &mut p,
        "append",
        RunAction::Steer {
            text: "extra".into(),
        },
    )
    .await;
    assert!(
        update(
            &mut s,
            &mut p,
            json!({"id":sent["id"],"result":{"turnId":"other-turn"}})
        )
        .await
        .is_err()
    );
    assert!(matches!(s.outcome(), Some(Outcome::Unknown { .. })));
    assert!(s.run_control_receipts().is_empty());
    let (mut s, mut p) = running().await;
    control(&mut s, &mut p, "stop", RunAction::Interrupt).await;
    update(&mut s, &mut p, end("interrupted")).await.unwrap();
    assert!(s.next_update().await.is_err());
    assert!(matches!(s.outcome(), Some(Outcome::Unknown { .. })));
    assert_eq!(s.phase(), Phase::Ended);
    assert!(s.run_control_receipts().is_empty());
}
