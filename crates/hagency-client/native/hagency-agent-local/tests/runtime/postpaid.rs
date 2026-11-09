use super::*;

fn quota(f: &mut Fixture, tokens: u64) {
    f.ledger
        .set_agent_policy(
            "@owner:test",
            "agent",
            1,
            &Policy {
                budget: Budget {
                    limit: Limit::Tokens(tokens),
                    period: Period::Lifetime,
                },
                requests: RequestPolicy::Allow,
                high_risk: ToolPolicy::Deny,
            },
        )
        .unwrap();
}

#[tokio::test]
async fn postpaid_native_turn_finishes_over_budget_and_only_next_task_is_refused() {
    for (kind, mode, cost) in [
        (Kind::Claude, "continuous:background_usage", 64),
        (Kind::Octos, "normal", 24),
    ] {
        let mut f = Fixture::new(kind, mode, ToolPolicy::Deny);
        quota(&mut f, 1);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let reply = process
            .session
            .run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "first",
                "first",
                "finish the work",
                BudgetMode::Postpaid,
                &queue,
                None,
            )
            .await
            .unwrap();
        assert_eq!(reply.text, "offline reply");
        assert_eq!(reply.usage.input + reply.usage.output, cost);
        assert_eq!(f.spent(), (cost, 0));
        let result = process
            .session
            .run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "next",
                "next",
                "new task",
                BudgetMode::Postpaid,
                &queue,
                None,
            )
            .await;
        assert!(matches!(
            result,
            Err(Error::Ledger(hagency_agent_local::Error::Budget))
        ));
        assert!(
            f.ledger
                .outstanding_calls("@owner:test")
                .unwrap()
                .is_empty()
        );
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn postpaid_missing_native_usage_is_unresolved_without_freezing_quota() {
    for kind in [Kind::Claude, Kind::Octos] {
        let mut f = Fixture::new(kind, "unknown", ToolPolicy::Deny);
        quota(&mut f, 1);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let reply = process
            .session
            .run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "unknown",
                "unknown",
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            )
            .await
            .unwrap();
        assert!(!reply.usage.is_reported());
        assert_eq!(f.spent(), (0, 0));
        process.stop().await.unwrap();
        f.ledger = Ledger::open(f.path("ledger"), "@owner:test").unwrap();
        let pending = f.ledger.outstanding_calls("@owner:test").unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state, "unknown");
        assert_eq!(pending[0].reserved, 0);
        assert_eq!(
            f.ledger
                .begin_postpaid(&f.scope, "new", "new", now())
                .unwrap(),
            hagency_agent_local::Reservation::New
        );
    }
}
