//! Complete host execution with fixed localhost APIs and original native pipes.
use super::*;
use hagency_runtime::claude::account::{AccountBinding, PROVIDER};

#[cfg(feature = "shared-successors")]
#[tokio::test]
#[ignore = "requires explicit offline Claude probe; real host, SQLite and HTTP routing"]
async fn native_shared_claude_full_host_resumes_encrypted_successor_without_double_billing() {
    let binding = AccountBinding::from_authenticated_profile(
        "01234567-89ab-cdef-0123-456789abcdef", "89abcdef-0123-4567-89ab-cdef01234567",
        "owner@example.test", Instant::now() + Duration::from_secs(45)).unwrap();
    let (f, transport, reserve) = prepared_with(Fixture::with_provider(PROVIDER, binding.subject()).await).await;
    let (f, transport, mut command) = runtime_fixture(f, transport, reserve).await;
    command.version = 4;
    command.capability = SHARED_CLAUDE_CAPABILITY.into();
    command.policy = SHARED_CLAUDE_POLICY.into();
    {
        let mut state = f.state.lock().unwrap();
        state.shared.command = Some(command.clone());
        let w = state.shared.work.as_mut().unwrap();
        w.runtime_command_digest = digest(&command).unwrap();
        w.encrypted = true;
        w.body = w.event_id.clone();
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
    }
    let root = f._root.path().join("claude-native-context");
    hagency_store::private::directory(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let (transport, _) = transport.with_crypto_fixture(Some("Remember encrypted blue comet".into()));
    transport.poll_claude_fixture(&f.registry, command.reserve_command.resource_id.clone(), root.clone(),
        binding.clone(), "shared-session").await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !f.state.lock().unwrap().shared.processes.values().any(|r| r.state == "stopped") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("original process stops without closing the host");
    let native: String = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap()
        .query_row("SELECT native_id FROM owner_shared_native_checkpoints", [], |r| r.get(0)).unwrap();
    attempts::prepare_lost_start(&f, &transport, &command).await;
    {
        let mut state = f.state.lock().unwrap();
        let w = state.shared.work.as_mut().unwrap();
        w.event_id = "$claude_followup".into();
        w.thread_root = w.event_id.clone();
        w.body = w.event_id.clone();
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
    }
    let (second, sent) = transport.with_crypto_fixture(Some("What did I say?".into()));
    second.poll_claude_successor_fixture(&f.registry, command.reserve_command.resource_id.clone(), root.clone(),
        binding, "shared-session").await.unwrap();
    attempts::wait_stopped(&f).await;
    let prompt: Value = serde_json::from_slice(&std::fs::read(root.join("work/agent_fixture/shared-claude.prompt")).unwrap()).unwrap();
    assert_eq!(prompt["session_id"], native);
    assert!(sent.lock().unwrap().iter().any(|(_, text)| text.contains("Remember encrypted blue comet")));
    {
        let state = f.state.lock().unwrap();
        assert_eq!(state.shared.starts, 2);
        assert_eq!(state.shared.session_routes, 1);
        let completed: Vec<_> = state.shared.receipts.values().filter(|r| r.state == "completed").collect();
        assert_eq!(completed.len(), 2);
        assert!(completed.iter().all(|r| r.usage_complete && r.known_consumed_tokens == 16));
        assert_ne!(completed[0].call_id, completed[1].call_id);
        assert_ne!(completed[0].process_digest, completed[1].process_digest);
    }
    f.stop().await;
}

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
