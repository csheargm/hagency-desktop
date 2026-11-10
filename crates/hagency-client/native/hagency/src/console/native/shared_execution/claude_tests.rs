//! Complete host execution with fixed localhost APIs and original native pipes.
use super::*;
use hagency_runtime::claude::account::{AccountBinding, PROVIDER};
#[path = "claude_stop_tests.rs"]
mod stop_tests;
#[path = "claude_archive_tests.rs"]
mod archive_tests;

fn native_marker(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    fn collect(root: &std::path::Path, name: &str, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(root).unwrap() {
            let entry = entry.unwrap(); let kind = entry.file_type().unwrap();
            if kind.is_dir() { collect(&entry.path(), name, found); }
            else if kind.is_file() && entry.file_name() == name { found.push(entry.path()); }
        }
    }
    let mut found = Vec::new(); collect(root, name, &mut found);
    assert_eq!(found.len(), 1, "exactly one marker in this authorized fixture scope: {name}");
    found.pop().unwrap()
}

#[tokio::test]
#[ignore = "requires explicit offline Claude probe; real host history reader and native MCP pipes"]
async fn native_shared_claude_full_host_reads_history_beyond_fallback_context_allowance() {
    let binding = AccountBinding::from_authenticated_profile(
        "01234567-89ab-cdef-0123-456789abcdef", "89abcdef-0123-4567-89ab-cdef01234567",
        "owner@example.test", Instant::now() + Duration::from_secs(45)).unwrap();
    let (f, transport, reserve) = prepared_with(Fixture::with_provider(PROVIDER, binding.subject()).await).await;
    let (f, transport, mut command) = runtime_fixture(f, transport, reserve).await;
    command.version = 4;
    command.capability = SHARED_CLAUDE_CAPABILITY.into();
    command.policy = SHARED_CLAUDE_POLICY.into();
    let work = {
        let mut state = f.state.lock().unwrap();
        state.shared.command = Some(command.clone());
        let w = state.shared.work.as_mut().unwrap();
        w.runtime_command_digest = digest(&command).unwrap();
        w.encrypted = true;
        w.body = w.event_id.clone();
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
        w.clone()
    };
    let root = f._root.path().join("claude-native-history");
    hagency_store::private::directory(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let (transport, sent) = transport.with_crypto_fixture(Some("Summarize the entire room".into()));
    let pages = (0..16).map(|page| (0..8).map(|message| json!({
        "eventId":format!("$history-{}:test",page * 8 + message),
        "sender":"@participant:test", "body":format!("Message {} {}",page * 8 + message,"x".repeat(700))
    })).collect()).collect();
    let (transport, reads) = transport.with_history_fixture(work.room_id, work.event_id, pages);
    transport.poll_claude_fixture(&f.registry, command.reserve_command.resource_id.clone(), root.clone(),
        binding, "shared-history").await.unwrap();
    stopped(&f).await;
    let pages: Vec<Value> = serde_json::from_slice(&std::fs::read(native_marker(&root, "shared-claude.history")).unwrap()).unwrap();
    assert!(pages.last().is_some_and(|p| p["complete"] == true));
    assert_eq!(pages[0]["alreadyRead"], true, "initial preview is not repeated by the tool");
    assert!(pages.iter().all(|p| p.to_string().len() <= 12 * 1024 && p.get("reason").is_none()));
    assert!(pages.iter().map(|p| p.to_string().len()).sum::<usize>() > 64 * 1024);
    let mut seen = std::collections::HashSet::new();
    for page in &pages {
        for message in page["messages"].as_array().unwrap() {
            assert!(seen.insert(message["eventId"].as_str().unwrap().to_owned()));
        }
    }
    assert_eq!(seen.len(), 120, "eight preview messages plus 120 tool messages cover the room");
    assert_eq!(reads.lock().unwrap().len(), 17, "one start and sixteen Matrix pages");
    assert!(sent.lock().unwrap().iter().any(|(_, text)| text == "shared offline answer"));
    {
        let state = f.state.lock().unwrap();
        let receipt = state.shared.receipts.values().max_by_key(|r| r.ledger_revision).unwrap();
        assert_eq!(receipt.state, "completed");
        assert!(receipt.usage_complete);
        assert_eq!(receipt.known_consumed_tokens, 16, "history bytes never become billable token estimates");
    }
    f.stop().await;
}

#[cfg(feature = "shared-successors")]
#[tokio::test]
#[ignore = "requires explicit offline Claude probe; real host, SQLite and HTTP routing"]
async fn native_shared_claude_full_host_resumes_encrypted_successor_without_double_billing() {
    resumed_host(false, false).await;
}

#[cfg(feature = "shared-successors")]
#[tokio::test]
#[ignore = "requires explicit offline Claude probe; original native processes and frozen V3 inputs"]
async fn native_shared_claude_full_host_resumes_plaintext_v3_without_history_reinjection() {
    resumed_host(true, false).await;
}

#[cfg(feature = "shared-successors")]
async fn resumed_host(plaintext: bool, missing: bool) {
    resumed_host_with(plaintext, missing, None).await
}
/// A failure of the resumed launch that happens while its saved native context
/// is still present (see the probe's `<id>.startup`).
#[cfg(feature = "shared-successors")]
#[derive(Clone, Copy)]
struct Startup {
    failure: &'static str,
    /// Consecutive failed starts of this same saved session by earlier
    /// requests. This harness drives one request per saved session, so they
    /// are entered in the host's count directly.
    earlier: u32,
    /// The fresh launch that replaces the saved session cannot start either.
    fresh_fails: bool,
}
#[cfg(feature = "shared-successors")]
fn resume_count(native: &str) -> String {
    format!("shared\n{native}")
}
#[cfg(feature = "shared-successors")]
async fn resumed_host_with(plaintext: bool, missing: bool, startup: Option<Startup>) {
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
        state.shared.contexts.active = plaintext;
        state.shared.contexts.v3 = plaintext;
        let w = state.shared.work.as_mut().unwrap();
        w.runtime_command_digest = digest(&command).unwrap();
        w.encrypted = !plaintext;
        w.body = if plaintext { "Remember encrypted blue comet".into() } else { w.event_id.clone() };
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
    }
    let root = f._root.path().join("claude-native-context");
    hagency_store::private::directory(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let (transport, _) = transport.with_crypto_fixture(Some("Remember encrypted blue comet".into()));
    let transport = if plaintext {
        let work = f.state.lock().unwrap().shared.work.clone().unwrap();
        transport.with_history_fixture(work.room_id, work.event_id, vec![]).0
    } else { transport };
    transport.poll_claude_fixture(&f.registry, command.reserve_command.resource_id.clone(), root.clone(),
        binding.clone(), "shared-session").await.unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        while !f.state.lock().unwrap().shared.processes.values().any(|r| r.state == "stopped") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("original process stops without closing the host");
    let native: String = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap()
        .query_row("SELECT native_id FROM owner_shared_native_checkpoints", [], |r| r.get(0)).unwrap();
    if missing { std::fs::remove_file(native_marker(&root, &format!("{native}.json"))).unwrap(); }
    if let Some(startup) = startup {
        let saved = native_marker(&root, &format!("{native}.json"));
        std::fs::write(saved.with_extension("startup"), startup.failure).unwrap();
        if startup.fresh_fails { std::fs::write(saved.with_file_name("fresh.startup"), "eof").unwrap(); }
        for _ in 0..startup.earlier { hagency_runtime::claude::resume_failures::record(&resume_count(&native)); }
    } else if !missing {
        // Earlier failed starts are forgotten once the session resumes.
        hagency_runtime::claude::resume_failures::record(&resume_count(&native));
        hagency_runtime::claude::resume_failures::record(&resume_count(&native));
    }
    attempts::prepare_lost_start(&f, &transport, &command).await;
    {
        let mut state = f.state.lock().unwrap();
        let w = state.shared.work.as_mut().unwrap();
        w.event_id = "$claude_followup".into();
        w.thread_root = w.event_id.clone();
        w.body = if plaintext { "What did I say?".into() } else { w.event_id.clone() };
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
    }
    let (second, sent) = transport.with_crypto_fixture(Some("What did I say?".into()));
    let second = if plaintext {
        let work = f.state.lock().unwrap().shared.work.clone().unwrap();
        second.with_history_fixture(work.room_id, work.event_id, vec![]).0
    } else { second };
    let polled = second.poll_claude_successor_fixture(&f.registry, command.reserve_command.resource_id.clone(), root.clone(),
        binding, "shared-session").await;
    let mut replaced = missing;
    if let Some(Startup { failure, earlier, fresh_fails }) = startup {
        let limit = hagency_runtime::claude::resume_failures::LIMIT;
        if earlier + 1 < limit || fresh_fails {
            // F5: the failure is returned, no fresh process answers in place of
            // the saved session, and the saved session is still the one on record.
            assert_eq!(polled.as_ref().err().map(|e| e.code.as_str()), Some("shared_execution_initialize_failed"),
                "a resumed launch that failed to start ({failure}, {earlier} earlier) was replaced by a fresh session");
            attempts::wait_stopped(&f).await;
            let saved: Vec<String> = {
                let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
                let mut rows = db.prepare("SELECT native_id FROM owner_shared_native_checkpoints").unwrap();
                rows.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
            };
            assert_eq!(saved, [native.clone()], "{failure}");
            let prompt: Value = serde_json::from_slice(&std::fs::read(native_marker(&root, "shared-claude.prompt")).unwrap()).unwrap();
            assert_eq!(prompt["message"]["content"].as_str().is_some_and(|text| text.contains("What did I say?")), false,
                "the new request reached no process: {failure}");
            assert_eq!(f.state.lock().unwrap().shared.receipts.values().filter(|r| r.state == "completed").count(), 1, "{failure}");
            // F5b: this failed start was counted once, and the count is kept.
            assert_eq!(hagency_runtime::claude::resume_failures::record(&resume_count(&native)), earlier + 2, "{failure}");
            hagency_runtime::claude::resume_failures::clear(&resume_count(&native));
            f.stop().await;
            return;
        }
        // F5b: the third consecutive failure continues in a fresh session.
        assert!(polled.is_ok(), "the third consecutive failed resume ({failure}) still failed the request: {:?}",
            polled.as_ref().err().map(|e| e.code.as_str()));
        replaced = true;
    }
    let missing = replaced;
    polled.unwrap();
    attempts::wait_stopped(&f).await;
    let prompt: Value = serde_json::from_slice(&std::fs::read(native_marker(&root, "shared-claude.prompt")).unwrap()).unwrap();
    assert_eq!(prompt["session_id"], if missing { "" } else { &native });
    if !plaintext && !missing {
        assert!(sent.lock().unwrap().iter().any(|(_, text)| text.contains("Remember encrypted blue comet")));
    } else if plaintext {
        let input = prompt["message"]["content"].as_str().unwrap();
        assert!(!input.contains("Remember encrypted blue comet"), "old question is absent from the new input");
        let current: Value = serde_json::from_str(input.split_once('\n').unwrap().1).unwrap();
        assert_eq!(current["version"], 3);
        assert_eq!(current["context"]["history"], json!([]));
        assert_eq!(current["currentRequest"]["body"], "What did I say?");
    }
    {
        let state = f.state.lock().unwrap();
        assert_eq!(state.shared.starts, 2);
        assert_eq!(state.shared.session_routes, 1);
        let completed: Vec<_> = state.shared.receipts.values().filter(|r| r.state == "completed").collect();
        assert_eq!(completed.len(), 2);
        if plaintext && !missing {
            assert!(completed.iter().any(|r| r.body.as_ref().is_some_and(|text| text.contains("Remember encrypted blue comet"))));
        }
        assert!(completed.iter().all(|r| r.usage_complete && r.known_consumed_tokens == 16));
        assert_ne!(completed[0].call_id, completed[1].call_id);
        assert_ne!(completed[0].process_digest, completed[1].process_digest);
    }
    // A session that resumed, or was discarded, has no failed starts counted.
    assert_eq!(hagency_runtime::claude::resume_failures::record(&resume_count(&native)), 1);
    hagency_runtime::claude::resume_failures::clear(&resume_count(&native));
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised process"]
async fn native_shared_claude_full_host_preserves_usage_recovery_and_missing_idle() {
    let _ = tracing_subscriber::fmt()
        .with_test_writer()
        .with_env_filter("hagency=debug")
        .try_init();
    // After "shared-usage", every stream fails, or needs a second accounting
    // query, only after the final answer was read: that answer is delivered and
    // its usage stays incomplete. An error result is never an answer.
    let mut dropped = Vec::new();
    for (mode, answered) in [
        ("shared-usage", true),
        ("shared-result-exit", true),
        ("shared-background-exit", true),
        ("shared-result-oversized", true),
        ("shared-result-garbled", true),
        ("shared-snapshot-again", true),
        ("shared-error-exit", false),
    ] {
        let complete = mode == "shared-usage";
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
                    mode
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
        // Keeping an answer never changes how its usage is classified.
        assert_eq!(saved.known_consumed_tokens, 16, "{mode}");
        assert_eq!(saved.retained_tokens, Some(16), "{mode}");
        assert_eq!(saved.usage_complete, complete, "{mode}");
        let expected = if answered {
            ("completed", Some("shared offline answer"))
        } else {
            ("unknown", None)
        };
        if (saved.state.as_str(), saved.body.as_deref()) != expected {
            dropped.push(format!("{mode}: {} {:?}", saved.state, saved.body));
        }
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
            &std::fs::read(native_marker(&root, "shared-claude.prompt")).unwrap(),
        )
        .unwrap();
        let input: Value = serde_json::from_str(
            prompt["message"]["content"].as_str().unwrap()
                .strip_prefix(CONTEXT_PREFIX_V3).expect("current V3 input"),
        ).unwrap();
        let work = f.state.lock().unwrap().shared.work.clone().unwrap();
        assert_eq!(input["version"], 3);
        assert_eq!(input["currentRequest"], json!({
            "eventId": work.event_id, "requesterMxid": work.requester_mxid,
            "body": "Offline synthetic prompt",
        }));
        assert_eq!(input["context"]["history"], json!([]));
        f.stop().await;
    }
    assert!(
        dropped.is_empty(),
        "receipt state and body differ from the answer that was read: {dropped:?}"
    );
}

#[tokio::test]
#[cfg(feature = "shared-successors")]
#[ignore = "requires explicit HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised process"]
async fn native_shared_claude_full_host_rebuilds_missing_rollout_before_input() {
    resumed_host(false, true).await;
    resumed_host(true, true).await;
}

/// F5: only the native missing-context diagnostic retires a saved shared
/// session. Any other failed startup of the resumed launch returns the failure
/// and keeps the saved session for the next request.
#[tokio::test]
#[cfg(feature = "shared-successors")]
#[ignore = "requires explicit HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised process"]
async fn native_shared_claude_full_host_keeps_the_saved_session_when_a_resumed_start_fails() {
    for failure in ["refused", "eof", "unexpected", "reworded"] {
        resumed_host_with(true, false, Some(Startup { failure, earlier: 0, fresh_fails: false })).await;
    }
    // The second consecutive failure keeps it too.
    resumed_host_with(true, false, Some(Startup { failure: "eof", earlier: 1, fresh_fails: false })).await;
}

/// F5b: a saved shared session whose resumed start fails three times in a row
/// before any input, without the missing-context diagnostic, is replaced by a
/// fresh session that answers the same request.
#[tokio::test]
#[cfg(feature = "shared-successors")]
#[ignore = "requires explicit HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised process"]
async fn native_shared_claude_full_host_starts_fresh_on_the_third_consecutive_failed_resume() {
    for failure in ["unexpected", "eof"] {
        resumed_host_with(true, false, Some(Startup { failure, earlier: 2, fresh_fails: false })).await;
    }
}

/// F5b: when the fresh launch cannot start either, nothing is retired and the
/// count is kept, so the next request tries the fresh start again.
#[tokio::test]
#[cfg(feature = "shared-successors")]
#[ignore = "requires explicit HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised process"]
async fn native_shared_claude_full_host_keeps_the_saved_session_when_the_fresh_start_also_fails() {
    resumed_host_with(true, false, Some(Startup { failure: "eof", earlier: 2, fresh_fails: true })).await;
}
