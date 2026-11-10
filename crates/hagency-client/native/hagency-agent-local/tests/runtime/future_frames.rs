//! F8 end to end: frames of a newer native CLI that the host can ignore or
//! refuse do not end the session, and change no decision, answer or charge.
use super::*;
use std::time::Duration;

#[tokio::test]
async fn claude_inert_startup_frames_and_extra_acknowledgement_keys_do_not_fail_the_start() {
    let mut f = Fixture::new(Kind::Claude, "future_start", ToolPolicy::Deny);
    let mut process = spawn_for_scope_with_guardian(
        &f.profile,
        &f.reference,
        &mut f.ledger,
        &f.scope,
        Some(&PathBuf::from(env!("CARGO_BIN_EXE_hagency-local-probe"))),
    )
    .await
    .unwrap();
    let started = process.session.initialize().await;
    assert!(
        started.is_ok(),
        "startup frames of a newer CLI ended the session before its first input: {:?}",
        started.err()
    );
    process.session.verify_host_environment().await.unwrap();
    process.session.require_local_account().await.unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let done = tokio::time::timeout(
        Duration::from_secs(20),
        process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "hello",
            BudgetMode::Postpaid,
            &queue,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(done.text, "offline reply");
    assert_eq!(done.status, codex::TurnStatus::Completed);
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_unknown_request_and_state_mid_turn_keep_the_turn_and_the_exact_owner_verdict() {
    for verdict in [true, false] {
        let mut f = Fixture::new(Kind::Claude, "future_turn", ToolPolicy::AskOwner);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, mut ui) = codex::approval_queue(1).unwrap();
        let effect = f.path("effect");
        let owner = async {
            // The unknown request is never presented; the real one is.
            let pending = tokio::time::timeout(Duration::from_secs(15), ui.recv())
                .await
                .ok()??;
            assert_eq!(pending.proposal.tool, "claude.Write");
            assert!(!effect.exists());
            pending.decide(verdict);
            Some(())
        };
        let turn = process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "hello",
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let (result, presented) =
            tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(turn, owner) })
                .await
                .expect("the turn ends");
        let done = match result {
            Ok(done) => done,
            Err(error) => panic!("a frame of a newer CLI ended the turn: {error:?}"),
        };
        assert!(presented.is_some(), "the real permission request is presented");
        assert_eq!(done.text, "offline reply");
        assert_eq!(done.status, codex::TurnStatus::Completed);
        assert_eq!(done.tool_requests, 1);
        assert_eq!(done.tool_returns, 1);
        // The unknown request got an error and nothing else; the real request
        // got exactly the owner's verdict.
        let refusal: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(f.path("refusal")).unwrap()).unwrap();
        assert_eq!(refusal["response"]["subtype"], "error");
        assert_eq!(refusal["response"]["request_id"], "future-1");
        assert!(refusal["response"].get("response").is_none());
        let decision: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(f.path("decision")).unwrap()).unwrap();
        assert_eq!(decision["behavior"], if verdict { "allow" } else { "deny" });
        assert_eq!(f.path("effect").exists(), verdict);
        assert_eq!(f.spent(), (24, 0));
        process.stop().await.unwrap();
    }
}
