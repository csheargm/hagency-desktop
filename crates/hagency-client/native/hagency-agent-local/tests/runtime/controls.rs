use super::*;
use hagency_agent_local::{
    questions::question_queue,
    room_tools::room_queue,
    run_controls::{self, Action, Outcome},
};
use hagency_runtime::{questions::Answer, room_tools::RoomTools};
use std::time::Duration;

#[tokio::test]
async fn claude_stop_during_owner_or_question_wait_is_cooperative_and_bills_actual_usage() {
    for question in [false, true] {
        for refused in [false, true] {
            let mode = format!(
                "control_{}_{}",
                if question { "question" } else { "permission" },
                if refused { "refused" } else { "accepted" }
            );
            let mut f = Fixture::new(Kind::Claude, &mode, ToolPolicy::AskOwner);
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
            let (queue, mut approvals) = codex::approval_queue(1).unwrap();
            let (questions, mut incoming) = question_queue();
            let queue = queue.with_run_controls(inbox).with_questions(questions);
            let run = process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "call",
                "dispatch",
                "original task",
                BudgetMode::Postpaid,
                &queue,
                None,
            );
            let host = async {
                if question {
                    let pending = incoming.recv().await.unwrap();
                    let stop = sender.submit(Action::Interrupt).unwrap();
                    assert_eq!(
                        stop.receipt.await.unwrap(),
                        if refused {
                            Outcome::Rejected
                        } else {
                            Outcome::Accepted
                        }
                    );
                    if !refused {
                        while !pending.is_closed() {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    }
                    assert_eq!(
                        pending
                            .reply([("q0".into(), Answer::Option("Yes".into()))].into())
                            .accepted()
                            .await,
                        refused
                    );
                } else {
                    let pending = approvals.recv().await.unwrap();
                    let stop = sender.submit(Action::Interrupt).unwrap();
                    assert_eq!(
                        stop.receipt.await.unwrap(),
                        if refused {
                            Outcome::Rejected
                        } else {
                            Outcome::Accepted
                        }
                    );
                    if !refused {
                        while !pending.is_closed() {
                            tokio::time::sleep(Duration::from_millis(1)).await;
                        }
                    }
                    pending.decide(true);
                }
            };
            let (done, ()) =
                tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
                    .await
                    .expect("native reader must remain live during the host wait");
            assert_eq!(
                done.unwrap().status,
                if refused {
                    codex::TurnStatus::Completed
                } else {
                    codex::TurnStatus::Interrupted
                }
            );
            assert_eq!(
                f.spent(),
                (24, 0),
                "interruption settles the actual reported usage"
            );
            assert_eq!(f.path("effect").exists(), refused);
            assert!(!sender.is_active());
            process.stop().await.unwrap();
        }
    }
}

#[tokio::test]
async fn claude_stop_withdraws_slow_room_io_without_waiting_for_the_network() {
    let mut f = Fixture::new(Kind::Claude, "room_stop", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .enable_room_tools(RoomTools::history_and_files())
        .await
        .unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let (room, mut incoming) = room_queue(&f.scope, "dispatch").unwrap();
    let queue = queue.with_run_controls(inbox).with_room_tools(room);
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "read history",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let mut pending = incoming.recv().await.unwrap();
        assert_eq!(
            sender
                .submit(Action::Interrupt)
                .unwrap()
                .receipt
                .await
                .unwrap(),
            Outcome::Accepted
        );
        pending.closed().await;
        assert!(pending.is_closed());
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(run, host) })
            .await
            .expect("stop must not wait for room IO");
    assert_eq!(done.unwrap().status, codex::TurnStatus::Interrupted);
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}
