use super::*;

#[tokio::test(start_paused = true)]
async fn canceled_and_terminal_approvals_withdraw_the_owner_wait_without_late_effects() {
    for kind in [
        "approval_cancel",
        "approval_terminal",
        "approval_partial_cancel",
    ] {
        let (_temp, mut ledger, profile) = setup(false);
        let (mut session, effects, peer) =
            fake(profile.cwd.to_str().unwrap().into(), true, true, 10, kind);
        session.initialize().await.unwrap();
        session
            .open_context(&mut ledger, &scope(), &profile)
            .await
            .unwrap();
        let (queue, mut ui) = approval_queue(2).unwrap();
        let host = async {
            let pending = ui.recv().await.unwrap();
            assert_eq!(effects.load(Ordering::SeqCst), 0);
            if kind == "approval_partial_cancel" {
                // The owner has already answered while cancellation is split
                // across two native reads. That answer must not overtake it.
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
            BudgetMode::Estimated { reservation: 20 },
            &queue,
        );
        let (done, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, host) })
                .await
                .expect("cancellation/terminal event must retire an unanswered owner callback");
        let done = done.unwrap();
        assert_eq!(done.text, "hello");
        assert_eq!(done.usage.input, 10);
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        assert_eq!(
            ledger
                .account(&scope(), Layer::Room, Period::Lifetime, now())
                .unwrap(),
            (10, 0)
        );
        assert!(ledger.outstanding_calls("@alice:test").unwrap().is_empty());
        drop(session);
        // The peer rejects every unexpected frame after completion, including
        // any approval sent late in response to the retired owner callback.
        peer.await.unwrap();
    }
}

#[tokio::test]
async fn owner_wait_does_not_hide_a_second_native_approval() {
    let (_temp, mut ledger, profile) = setup(false);
    let (mut session, effects, peer) = fake(
        profile.cwd.to_str().unwrap().into(),
        true,
        true,
        10,
        "approval_pair",
    );
    session.initialize().await.unwrap();
    session
        .open_context(&mut ledger, &scope(), &profile)
        .await
        .unwrap();
    let (queue, mut ui) = approval_queue(2).unwrap();
    let host = async {
        let first = ui.recv().await.unwrap();
        let second = ui.recv().await.unwrap();
        assert_eq!(first.proposal.arguments["command"], "touch forbidden");
        assert_eq!(second.proposal.arguments["command"], "echo second");
        assert!(!first.is_closed());
        assert!(!second.is_closed());
        assert_eq!(effects.load(Ordering::SeqCst), 0);
        second.decide(true);
        first.decide(false);
    };
    let bound = scope();
    let run = session.run(
        &mut ledger,
        "@alice:test",
        &bound,
        "call",
        "dispatch",
        "hello",
        BudgetMode::Estimated { reservation: 20 },
        &queue,
    );
    let (done, ()) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, host) })
            .await
            .expect("native reader must continue while the first approval is pending");
    assert_eq!(done.unwrap().usage.input, 10);
    assert_eq!(effects.load(Ordering::SeqCst), 1);
    assert_eq!(
        ledger
            .account(&scope(), Layer::Room, Period::Lifetime, now())
            .unwrap(),
        (10, 0)
    );
    drop(session);
    peer.await.unwrap();
}
