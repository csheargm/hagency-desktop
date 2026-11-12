//! Real original/successor drivers, SQLite and child process; only Matrix I/O
//! uses a local test adapter. Neither model input nor answer may reach the API.
use super::*;

#[cfg(feature = "shared-budget-execution")]
#[path = "history_context_tests.rs"]
mod history_context;

const PROMPT: &str = "Private encrypted request for the new Agent";

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe; crosses cold Matrix recovery lease"]
async fn native_shared_encrypted_cold_recovery_renews_before_starting_original_call() {
    for successor in [false, true] {
        let (f, t, _) = Box::pin(setup(successor)).await;
        let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
        let t = t.with_crypto_input_delay(Duration::from_secs(32));
        if successor {
            t.poll_successor_fixture(&f.registry, launch(&f, "text-usage")).await.unwrap();
        } else {
            t.poll_fixture(&f.registry, launch(&f, "text-usage")).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                if !sent.lock().unwrap().is_empty() { break; }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }).await.expect("cold encrypted input must retain the same live process and complete");
        if successor { attempts::wait_stopped(&f).await; } else { stopped(&f).await; }
        assert!(f.state.lock().unwrap().shared.activity_epoch >= 3);
        assert_eq!(f.state.lock().unwrap().shared.starts, 1);
        assert_eq!(sent.lock().unwrap().len(), 1);
        let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
        assert_eq!(methods.lines().filter(|l| *l == "turn/start").count(), 1);
        assert_eq!(rows(&f, "owner_managed_budget_deltas"), 0);
        assert!(f.state.lock().unwrap().shared.receipts.values().any(|r|r.state=="completed" && r.usage_complete));
        f.stop().await;
    }
}

async fn setup(successor: bool) -> (Fixture, SharedExecutionTransport, SharedRuntimeCommand) {
    let (f, t, c) = if successor {
        let (f, t, c) = Box::pin(attempts::predecessor()).await;
        attempts::prepare_lost_start(&f, &t, &c).await;
        (f, t, c)
    } else {
        fixture().await
    };
    {
        let mut s = f.state.lock().unwrap();
        let w = s.shared.work.as_mut().unwrap();
        w.encrypted = true;
        w.body = w.event_id.clone();
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
    }
    (f, t, c)
}

async fn run(f: &Fixture, t: &SharedExecutionTransport, successor: bool) {
    let progress = if successor {
        t.poll_successor_fixture(&f.registry, launch(f, "text-usage")).await
    } else {
        t.poll_fixture(&f.registry, launch(f, "text-usage")).await
    }.unwrap();
    assert!(matches!(progress, SharedExecutionProgress::Running {..}));
    if successor { attempts::wait_stopped(f).await; } else { stopped(f).await; }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_encrypted_original_and_successor_keep_plaintext_local() {
    for successor in [false, true] {
        let (f, t, c) = Box::pin(setup(successor)).await;
        let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
        f.state.lock().unwrap().shared.lose_completed_work_result = true;
        run(&f, &t, successor).await;
        let receipt = f.state.lock().unwrap().shared.receipts.values()
            .find(|r| r.state == "completed").expect("encrypted request completes").clone();
        let sends = sent.lock().unwrap().clone();
        assert_eq!(sends.len(), 1, "lost result acknowledgement cannot duplicate the Matrix send");
        let wire: Value = serde_json::from_str(receipt.body.as_ref().unwrap()).unwrap();
        assert_eq!(wire["algorithm"], "m.megolm.v1.aes-sha2");
        assert_eq!(wire["delivery"], "sent");
        assert!(!receipt.body.as_ref().unwrap().contains(&sends[0].1));
        let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
        let (prompt, body): (String, String) = db.query_row(
            "SELECT prompt,body FROM owner_shared_decrypted_inputs", [], |r| Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(prompt, PROMPT);
        assert_eq!(body, PROMPT);
        let reply: String = db.query_row("SELECT payload FROM owner_shared_reply_deliveries", [], |r| r.get(0)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&reply).unwrap()["body"], sends[0].1);
        assert_eq!(rows(&f, "owner_managed_budget_deltas"), 0, "encryption needs no top-up");
        assert_eq!(f.state.lock().unwrap().shared.starts, 1);
        let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
        assert_eq!(methods.lines().filter(|l| *l == "turn/start").count(), 1);
        // Historical upload uses the same private delivery receipt, without a second turn/send.
        if successor {
            let attempt = f.state.lock().unwrap().shared.attempts.command.clone().unwrap();
            t.recover_successor(&f.registry, attempt.command_id.clone(), digest(&attempt).unwrap(), None).await.unwrap();
        } else {
            t.recover_work(&f.registry, c.command_id.clone(), digest(&c).unwrap(), receipt.work_id.clone(), Some(receipt.observation_id.clone())).await.unwrap();
        }
        assert_eq!(sent.lock().unwrap().len(), 1);
        f.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_encrypted_missing_keys_never_start_model() {
    for successor in [false, true] {
        let (f, t, _) = Box::pin(setup(successor)).await;
        let (t, sent) = t.with_crypto_fixture(None);
        run(&f, &t, successor).await;
        assert_eq!(f.state.lock().unwrap().shared.starts, 0);
        assert_eq!(rows(&f, "owner_managed_calls"), 0);
        assert_eq!(rows(&f, "owner_shared_decrypted_inputs"), 0);
        assert!(sent.lock().unwrap().is_empty());
        let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
        assert!(!methods.contains("turn/start"));
        f.stop().await;
    }
}

#[cfg(feature = "shared-budget-execution")]
#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe; exercises all three native drivers"]
async fn native_shared_encrypted_history_reaches_original_successor_and_budget_drivers() {
    for lane in ["original", "successor", "budget"] {
        let (f, t, _) = match lane {
            "budget" => Box::pin(budget_attempts::prepared()).await,
            _ => Box::pin(setup(lane == "successor")).await,
        };
        let work = {
            let mut state = f.state.lock().unwrap();
            let work = state.shared.work.as_mut().unwrap();
            work.encrypted = true;
            work.body = work.event_id.clone();
            work.body_digest = format!("{:x}", Sha256::digest(work.body.as_bytes()));
            work.clone()
        };
        let deltas_before = rows(&f, "owner_managed_budget_deltas");
        let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
        let recent = json!({"eventId":"$recent:history","sender":"@alice:test","body":"Recent unaddressed group message"});
        let older = json!({"eventId":"$older:history","sender":"@bob:test","body":"Earlier unaddressed group message"});
        let (t, reads) = t.with_history_fixture(work.room_id.clone(), work.event_id.clone(), vec![
            vec![json!({"eventId":work.event_id,"sender":work.requester_mxid,"body":PROMPT}), recent.clone()],
            vec![recent, older, json!({"eventId":"$missing:history","sender":"@carol:test","status":"unavailable_encrypted_message","unavailableReason":"unable_to_decrypt"})],
        ]);
        let launch = launch(&f, "text-room-history-usage");
        let progress = match lane {
            "original" => t.poll_fixture(&f.registry, launch).await,
            "successor" => t.poll_successor_fixture(&f.registry, launch).await,
            _ => t.poll_budget_successor_fixture(&f.registry, launch).await,
        }.unwrap();
        assert!(matches!(progress, SharedExecutionProgress::Running { .. }), "{lane}");
        match lane {
            "original" => stopped(&f).await,
            "successor" => attempts::wait_stopped(&f).await,
            _ => budget_attempts::wait_stopped(&f).await,
        }
        let root = f._root.path().join("shared-probe");
        let thread: Value = serde_json::from_slice(&std::fs::read(root.join("thread-request.json")).unwrap()).unwrap();
        assert_eq!(thread["dynamicTools"][0]["name"], "read_room_history", "{lane} must expose the history tool");
        let turn: Value = serde_json::from_slice(&std::fs::read(root.join("turn-request.json")).unwrap()).unwrap();
        let text = turn["input"][0]["text"].as_str().unwrap();
        assert!(text.contains("Recent unaddressed group message"), "{lane} must include recent room context");
        assert_eq!(text.matches(PROMPT).count(), 1, "{lane} must include the current request only once");
        let pages: Vec<Value> = std::fs::read_to_string(root.join("history-pages.jsonl")).unwrap()
            .lines().map(|line|serde_json::from_str(line).unwrap()).collect();
        assert_eq!(pages.len(), 2, "{lane}: repeated preview cursor then the next page");
        assert_eq!(pages[0]["alreadyRead"], true);
        assert_eq!(pages[0]["messages"], json!([]));
        assert_eq!(pages[1]["complete"], true);
        assert_eq!(pages[1]["unavailableMessages"], 1);
        assert_eq!(pages[1]["messages"][0]["eventId"], "$older:history");
        assert!(pages.iter().all(|page|page.to_string().len() <= 12 * 1024));
        assert_eq!(*reads.lock().unwrap(), ["start", "0", "1"]);
        {
            let sends = sent.lock().unwrap();
            assert_eq!(sends.len(), 1, "{lane}");
            assert_eq!(sends[0].1, "Earlier unaddressed group message");
            let state = f.state.lock().unwrap();
            assert_eq!(state.shared.starts, 1, "{lane}");
            let receipt = state.shared.receipts.values().find(|r|r.state=="completed").expect("history turn completes");
            assert!(receipt.usage_complete);
            assert!(!receipt.body.as_deref().unwrap_or_default().contains("unaddressed group message"));
        }
        assert_eq!(rows(&f, "owner_managed_budget_deltas"), deltas_before);
        f.stop().await;
    }
}
