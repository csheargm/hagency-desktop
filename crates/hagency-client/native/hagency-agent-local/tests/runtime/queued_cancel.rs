use super::*;
use hagency_agent_local::{questions::question_queue, room_tools::room_queue};
use hagency_runtime::{live_text::LiveText, room_tools::RoomTools};
use std::time::Duration;

/// Native parallel tool calls queue their callbacks behind one owner wait. The
/// CLI may withdraw a queued callback before the owner answers the first one.
/// Only the withdrawn callback disappears: the turn, the answered request and
/// its exact decision are unaffected, and the withdrawn tool is never presented.
async fn withdrawn_while_the_owner_answers_another(mode: &str) {
    for verdict in [false, true] {
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::AskOwner);
        let mut process = f.process().await;
        let (room, mut room_calls) = room_queue(&f.scope, "dispatch").unwrap();
        let (queue, mut approvals) = codex::approval_queue(1).unwrap();
        let (questions, mut asked) = question_queue();
        let live = LiveText::default();
        let mut queue = queue
            .with_live_text(live.clone())
            .with_questions(questions);
        if mode == "room_queued_cancel" {
            process
                .session
                .enable_room_tools(RoomTools::history_and_files())
                .await
                .unwrap();
            queue = queue.with_room_tools(room);
        }
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let effect = f.path("effect");
        let owner = async {
            let pending = tokio::time::timeout(Duration::from_secs(15), approvals.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(pending.proposal.tool, "claude.Write");
            // The fixture streams this text after its cancel frame, so the
            // host has read the withdrawal before the owner answers.
            tokio::time::timeout(Duration::from_secs(15), async {
                while !live
                    .since(0)
                    .is_some_and(|(_, text)| text.contains("queued callback withdrawn"))
                {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            })
            .await
            .expect("the host keeps reading native frames during the owner wait");
            assert!(!pending.is_closed(), "the first request stays open");
            assert!(!effect.exists());
            pending.decide(verdict);
            approvals
        };
        let turn = process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "use two tools",
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let (result, mut approvals) =
            tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(turn, owner) })
                .await
                .unwrap();
        let done = result.unwrap_or_else(|error| {
            panic!("a withdrawn queued callback ended the turn (owner verdict {verdict}): {error:?}")
        });
        assert_eq!(done.text, "offline reply");
        assert_eq!(done.status, codex::TurnStatus::Completed);
        assert_eq!((done.tool_requests, done.tool_returns), (2, 1));
        let decision: serde_json::Value =
            serde_json::from_slice(&fs::read(f.path("decision")).unwrap()).unwrap();
        assert_eq!(
            decision["behavior"],
            if verdict { "allow" } else { "deny" },
            "the answered request keeps its exact owner decision"
        );
        assert_eq!(f.path("effect").exists(), verdict);
        assert!(
            approvals.try_recv().is_err(),
            "the withdrawn tool is never presented to the owner"
        );
        assert!(
            asked.try_recv().is_err(),
            "the withdrawn question is never asked"
        );
        assert!(
            room_calls.try_recv().is_err(),
            "the withdrawn room call never reaches Matrix IO"
        );
        let frames = fs::read_to_string(f.path("frames")).unwrap();
        assert_eq!(
            frames
                .lines()
                .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
                .filter(|frame| frame["response"]["request_id"] == "queued")
                .count(),
            0,
            "no response is written for the withdrawn callback"
        );
        assert_eq!(f.spent(), (24, 0));
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn claude_cancel_of_a_queued_permission_keeps_the_turn_and_the_answered_decision() {
    withdrawn_while_the_owner_answers_another("queued_cancel").await;
}

#[tokio::test]
async fn claude_cancel_of_a_queued_question_keeps_the_turn_and_the_answered_decision() {
    withdrawn_while_the_owner_answers_another("queued_cancel_question").await;
}

#[tokio::test]
async fn claude_cancel_of_a_queued_room_call_keeps_the_turn_and_the_answered_decision() {
    withdrawn_while_the_owner_answers_another("room_queued_cancel").await;
}
