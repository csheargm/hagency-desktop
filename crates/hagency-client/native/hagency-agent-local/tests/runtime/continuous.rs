use super::*;

async fn turn(
    f: &mut Fixture,
    process: &mut Process,
    call: &str,
    queue: &codex::ApprovalQueue,
) -> codex::Completed {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            call,
            call,
            "next message",
            BudgetMode::Postpaid,
            queue,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn claude_continuous_process_preserves_session_and_settles_each_dispatch_once() {
    for (mode, cost) in [
        ("continuous:cumulative", 54),
        ("continuous:background_usage", 64),
    ] {
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::Deny);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        for (round, call) in ["first", "second", "third"].into_iter().enumerate() {
            if round != 0 {
                assert!(process.session.can_continue());
                // Neither another requester nor a profile switch may borrow
                // this live native context, even in the same Matrix room.
                let mut stranger = f.scope.clone();
                stranger.requester = "@different:test".into();
                assert!(
                    process
                        .session
                        .continue_context(&mut f.ledger, &stranger, &f.profile)
                        .await
                        .is_err()
                );
                let mut changed = f.profile.clone();
                changed.model = "different-model".into();
                assert!(
                    process
                        .session
                        .continue_context(&mut f.ledger, &f.scope, &changed)
                        .await
                        .is_err()
                );
                process
                    .session
                    .continue_context(&mut f.ledger, &f.scope, &f.profile)
                    .await
                    .unwrap();
                assert!(!process.session.can_continue());
            }
            let reply = turn(&mut f, &mut process, call, &queue).await;
            assert_eq!(reply.text, "offline reply");
            assert_eq!(reply.usage.input + reply.usage.output, cost);
            assert_eq!(f.spent(), (cost * (round as u64 + 1), 0));
            process.session.park();
        }
        process.stop().await.unwrap();
        assert_eq!(
            fs::read_to_string(f.path("argv")).unwrap().lines().count(),
            1,
            "all three turns use one native process"
        );
        let frames: Vec<serde_json::Value> = fs::read_to_string(f.path("frames"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let prompts: Vec<_> = frames
            .iter()
            .filter(|v| v["type"] == "user" && v["message"]["content"] == "next message")
            .collect();
        assert_eq!(prompts.len(), 3);
        assert_eq!(prompts[0]["session_id"], "");
        assert_eq!(prompts[1]["session_id"], "offline-session");
        assert_eq!(prompts[2]["session_id"], "offline-session");
        assert_ne!(prompts[0]["uuid"], prompts[1]["uuid"]);
        assert_ne!(prompts[1]["uuid"], prompts[2]["uuid"]);
        assert!(
            f.ledger
                .outstanding_calls("@owner:test")
                .unwrap()
                .is_empty()
        );
    }
}

#[tokio::test]
async fn claude_continuous_tool_approval_is_requested_again_for_the_new_dispatch() {
    let mut f = Fixture::new(Kind::Claude, "continuous:approval", ToolPolicy::AskOwner);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    for (call, allow) in [("first", true), ("second", false)] {
        if !allow {
            fs::remove_file(f.path("effect")).unwrap();
            process
                .session
                .continue_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
        }
        let (queue, mut requests) = codex::approval_queue(1).unwrap();
        let approval = tokio::spawn(async move {
            let request = requests.recv().await.unwrap();
            assert_eq!(request.proposal.dispatch, call);
            assert_eq!(request.proposal.scope.requester, "@member:test");
            request.decide(allow);
        });
        assert_eq!(
            turn(&mut f, &mut process, call, &queue).await.text,
            "offline reply"
        );
        approval.await.unwrap();
        assert_eq!(f.path("effect").exists(), allow);
        process.session.park();
    }
    assert_eq!(f.spent(), (108, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_dead_or_unexpected_idle_process_cannot_reserve_or_receive_a_new_prompt() {
    for mode in ["continuous:exit", "continuous:late_callback"] {
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::Deny);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        turn(&mut f, &mut process, "first", &queue).await;
        assert!(process.session.can_continue());
        assert!(
            process
                .session
                .continue_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .is_err()
        );
        assert_eq!(f.spent(), (54, 0));
        assert!(
            f.ledger
                .outstanding_calls("@owner:test")
                .unwrap()
                .is_empty()
        );
        process.stop().await.unwrap();
        let frames = fs::read_to_string(f.path("frames")).unwrap();
        assert_eq!(
            frames
                .lines()
                .filter(|line| line.contains("next message"))
                .count(),
            1
        );
        assert!(!frames.contains("\"behavior\": \"allow\""));
    }
}
