//! A message sent while a Claude turn runs joins that turn, as it does for
//! Codex. The offline actor replays the frame order recorded from the native
//! CLI for each case; the driver's own tests replay the recorded frames.
use super::*;
use hagency_agent_local::run_controls::{self, Action, Outcome, Sender};
use std::time::Duration;

fn steer(text: &str) -> Action {
    Action::Steer { text: text.into() }
}
async fn active(sender: &Sender) {
    while !sender.is_active() {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}
/// Every frame the host wrote to the native process, in order.
fn written(f: &Fixture) -> Vec<serde_json::Value> {
    fs::read_to_string(f.path("frames"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn claude_steer_during_an_owner_wait_joins_the_running_turn_and_is_bounded_per_turn() {
    for count in [1, 16] {
        let mut f = Fixture::new(Kind::Claude, "steer_injected", ToolPolicy::AskOwner);
        fs::write(f.path("steer-count"), count.to_string()).unwrap();
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
        let (queue, mut approvals) = codex::approval_queue(1).unwrap();
        let queue = queue.with_run_controls(inbox);
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
            let pending = approvals.recv().await.unwrap();
            for index in 0..count {
                // The native queue's acknowledgement, not the pipe flush, and
                // it does not wait for the owner's open decision.
                let sent = sender.submit(steer(&format!("also this {index}"))).unwrap();
                assert_eq!(sent.receipt.await.unwrap(), Outcome::Accepted);
            }
            if count == 16 {
                let extra = sender.submit(steer("one too many")).unwrap();
                assert_eq!(extra.receipt.await.unwrap(), Outcome::Busy);
            }
            assert!(!pending.is_closed());
            pending.decide(true);
        };
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, host) })
                .await
                .expect("a steer must not stall the turn");
        let done = done.unwrap();
        assert_eq!(done.text, "offline reply");
        assert_eq!(done.status, codex::TurnStatus::Completed);
        assert_eq!(f.spent(), (24, 0));
        assert!(f.path("effect").exists());
        let steers: Vec<_> = written(&f)
            .into_iter()
            .filter(|frame| frame["type"] == "user")
            .skip(1)
            .collect();
        assert_eq!(steers.len(), count, "a refused steer writes nothing");
        for (index, frame) in steers.iter().enumerate() {
            assert_eq!(
                frame["message"]["content"],
                format!("{}also this {index}", hagency_runtime::claude::STEER_LEAD)
            );
            assert_eq!(frame["session_id"], "offline-session");
        }
        assert!(!sender.is_active());
        assert!(matches!(
            sender.submit(steer("after the turn")),
            Err(run_controls::SubmitError::Inactive)
        ));
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn claude_text_only_steer_runs_a_second_cycle_and_settles_one_joined_answer() {
    let mut f = Fixture::new(Kind::Claude, "steer_cycle", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox);
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
        active(&sender).await;
        let sent = sender.submit(steer("and then this")).unwrap();
        assert_eq!(sent.receipt.await.unwrap(), Outcome::Accepted);
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, host) })
            .await
            .expect("the second cycle belongs to the same turn");
    let done = done.unwrap();
    // One call: both cycles' texts joined by a blank line, both cycles' usage.
    assert_eq!(done.text, "offline reply\n\nsteered reply");
    assert_eq!(done.status, codex::TurnStatus::Completed);
    assert_eq!((done.usage.input, done.usage.output), (34, 14));
    assert_eq!(f.spent(), (48, 0));
    assert_eq!(
        fs::read_to_string(f.path("steer-read")).unwrap(),
        format!("{}and then this", hagency_runtime::claude::STEER_LEAD)
    );
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_stop_withdraws_a_queued_steer_in_the_same_write_and_it_never_runs() {
    for acknowledged in [true, false] {
        let mode = if acknowledged {
            "steer_stop"
        } else {
            "steer_stop_unacked"
        };
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::Deny);
        let read = f.path("steer-read");
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let queue = queue.with_run_controls(inbox);
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
            active(&sender).await;
            let sent = sender.submit(steer("never mind")).unwrap();
            if acknowledged {
                assert_eq!(sent.receipt.await.unwrap(), Outcome::Accepted);
                let stop = sender.submit(Action::Interrupt).unwrap();
                assert_eq!(stop.receipt.await.unwrap(), Outcome::Accepted);
            } else {
                // The native process has the steer and has not answered for
                // it. The stop withdraws it: its sender learns the text was
                // not taken instead of an acceptance that never ran.
                while !read.exists() {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                let stop = sender.submit(Action::Interrupt).unwrap();
                assert_eq!(stop.receipt.await.unwrap(), Outcome::Accepted);
                assert_eq!(sent.receipt.await.unwrap(), Outcome::Unavailable);
            }
        };
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, host) })
                .await
                .expect("stop must settle the turn");
        assert_eq!(done.unwrap().status, codex::TurnStatus::Interrupted);
        assert_eq!(f.spent(), (24, 0));
        // The actor asserts the cancel names the steer and that the interrupt
        // follows it; nothing else was written after the steer.
        let frames = written(&f);
        let at = frames
            .iter()
            .position(|frame| {
                frame["message"]["content"]
                    .as_str()
                    .is_some_and(|text| text.ends_with("\nnever mind"))
            })
            .unwrap();
        let after: Vec<_> = frames[at + 1..]
            .iter()
            .map(|frame| frame["request"]["subtype"].as_str().unwrap_or("other"))
            .collect();
        assert_eq!(after, ["cancel_async_message", "interrupt"], "{mode}");
        assert_eq!(
            frames[at + 1]["request"]["message_uuid"],
            frames[at]["uuid"]
        );
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn claude_steer_after_a_stop_request_is_unavailable_and_writes_nothing() {
    let mut f = Fixture::new(Kind::Claude, "control_permission_accepted", ToolPolicy::AskOwner);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox);
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
        let _pending = approvals.recv().await.unwrap();
        // Both are waiting when the run reads its controls: stop goes first.
        let stop = sender.submit(Action::Interrupt).unwrap();
        let late = sender.submit(steer("too late")).unwrap();
        assert_eq!(stop.receipt.await.unwrap(), Outcome::Accepted);
        assert_eq!(late.receipt.await.unwrap(), Outcome::Unavailable);
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, host) })
            .await
            .expect("stop must settle the turn");
    assert_eq!(done.unwrap().status, codex::TurnStatus::Interrupted);
    assert_eq!(
        written(&f)
            .iter()
            .filter(|frame| frame["type"] == "user")
            .count(),
        1,
        "only the original prompt was written"
    );
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_steer_the_native_queue_never_answered_for_is_unknown_not_accepted() {
    let mut f = Fixture::new(Kind::Claude, "steer_exit", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (sender, inbox) = run_controls::channel(&f.scope, "dispatch", "call").unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let queue = queue.with_run_controls(inbox);
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
        active(&sender).await;
        let sent = sender.submit(steer("was it read?")).unwrap();
        // Written and flushed, yet the stream ended before any answer: the
        // text may or may not have been read and is never sent again.
        assert_eq!(sent.receipt.await.unwrap(), Outcome::Unknown);
    };
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(run, host) })
            .await
            .expect("a closed stream ends the turn");
    assert!(done.is_err());
    assert_eq!(
        fs::read_to_string(f.path("steer-read")).unwrap(),
        format!("{}was it read?", hagency_runtime::claude::STEER_LEAD)
    );
}
