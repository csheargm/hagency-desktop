//! Complete host execution with fixed localhost APIs and original native pipes.
use super::*;
use hagency_runtime::claude::account::{AccountBinding, PROVIDER};

#[tokio::test]
#[ignore = "requires explicit HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised process"]
async fn native_shared_claude_full_host_preserves_usage_recovery_and_missing_idle() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("hagency=debug")
        .try_init();
    for complete in [true, false] {
        let binding = AccountBinding::from_authenticated_profile(
            "01234567-89ab-cdef-0123-456789abcdef",
            "89abcdef-0123-4567-89ab-cdef01234567",
            "owner@example.test",
            Instant::now() + Duration::from_secs(45),
        )
        .unwrap();
        let (f, transport, reserve) =
            prepared_with(Fixture::with_provider(PROVIDER, binding.subject()).await).await;
        let (f, transport, mut command) = runtime_fixture(f, transport, reserve).await;
        command.version = 4;
        command.capability = SHARED_CLAUDE_CAPABILITY.into();
        command.policy = SHARED_CLAUDE_POLICY.into();
        {
            let mut state = f.state.lock().unwrap();
            state.shared.command = Some(command.clone());
            state.shared.work.as_mut().unwrap().runtime_command_digest = digest(&command).unwrap();
            // The latest original evidence must recover after a lost HTTP response.
            state.shared.lose_completed_work_result = complete;
        }
        let root = f._root.path().join("shared-claude-probe");
        hagency_store::private::directory(&root).unwrap();
        let root = root.canonicalize().unwrap();
        assert!(matches!(
            transport
                .poll_claude_fixture(
                    &f.registry,
                    command.reserve_command.resource_id.clone(),
                    root.clone(),
                    binding,
                    if complete {
                        "shared-usage"
                    } else {
                        "shared-result-exit"
                    }
                )
                .await
                .unwrap(),
            SharedExecutionProgress::Running { .. }
        ));
        stopped(&f).await;
        let saved = {
            let state = f.state.lock().unwrap();
            assert_eq!(state.shared.starts, 1);
            state
                .shared
                .receipts
                .values()
                .max_by_key(|r| r.ledger_revision)
                .unwrap()
                .clone()
        };
        assert_eq!(saved.state, if complete { "completed" } else { "unknown" });
        assert_eq!(saved.known_consumed_tokens, 16);
        assert_eq!(saved.retained_tokens, Some(16));
        assert_eq!(saved.usage_complete, complete);
        assert_eq!(
            saved.body.as_deref(),
            complete.then_some("shared offline answer")
        );
        assert_eq!(rows(&f, "owner_managed_calls"), 1);
        assert!(matches!(
            transport
                .recover_work(
                    &f.registry,
                    command.command_id.clone(),
                    digest(&command).unwrap(),
                    saved.work_id,
                    Some(saved.observation_id)
                )
                .await
                .unwrap(),
            SharedWorkRecoveryProgress::Recorded { .. }
        ));
        assert!(matches!(
            transport.poll(&f.registry, None).await.unwrap(),
            SharedExecutionProgress::Recorded { .. }
        ));
        assert_eq!(f.state.lock().unwrap().shared.starts, 1);
        let prompt: Value = serde_json::from_slice(
            &std::fs::read(root.join("work/agent_fixture/shared-claude.prompt")).unwrap(),
        )
        .unwrap();
        assert_eq!(prompt["message"]["content"], "Offline synthetic prompt");
        f.stop().await;
    }
}
