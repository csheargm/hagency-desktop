use super::*;
use std::collections::HashSet;

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe; exercises all three native drivers"]
async fn native_shared_encrypted_history_context_recovers_after_compaction_without_billing_history()
{
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
        let pages = (0..16).map(|page| {
            (0..8).map(|message| json!({
                "eventId":format!("$history-{}:test",page * 8 + message),
                "sender":"@alice:test", "body":format!("Message {} {}",page * 8 + message,"x".repeat(700))
            })).collect()
        }).collect();
        let (t, reads) = t.with_history_fixture(work.room_id.clone(), work.event_id.clone(), pages);
        let launch = launch(&f, "text-room-history-context-usage");
        let progress = match lane {
            "original" => t.poll_fixture(&f.registry, launch).await,
            "successor" => t.poll_successor_fixture(&f.registry, launch).await,
            _ => t.poll_budget_successor_fixture(&f.registry, launch).await,
        }
        .unwrap();
        assert!(
            matches!(progress, SharedExecutionProgress::Running { .. }),
            "{lane}"
        );
        match lane {
            "original" => stopped(&f).await,
            "successor" => attempts::wait_stopped(&f).await,
            _ => budget_attempts::wait_stopped(&f).await,
        }
        let root = f._root.path().join("shared-probe");
        let pages: Vec<Value> = std::fs::read_to_string(root.join("history-pages.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(
            pages.last().is_some_and(|page| page["complete"] == true),
            "{lane}: {pages:?}"
        );
        assert!(pages.iter().all(|page| page.to_string().len() <= 12 * 1024));
        assert!(
            pages
                .iter()
                .map(|page| page.to_string().len())
                .sum::<usize>()
                > 64 * 1024
        );
        assert_eq!(pages[0]["alreadyRead"], true);
        assert_eq!(
            pages[1]["reason"], "context_budget",
            "initial near-full context must stop input"
        );
        let blocked: Vec<_> = pages
            .iter()
            .enumerate()
            .filter(|(_, page)| page["reason"] == "context_budget")
            .collect();
        assert!(blocked.len() >= 3, "{lane}");
        assert_eq!(
            blocked[2].0,
            blocked[1].0 + 1,
            "duplicate context cannot replenish the spent allowance"
        );
        assert_eq!(blocked[1].1["nextCursor"], blocked[2].1["nextCursor"]);
        let first_batch: usize = pages[2..blocked[1].0]
            .iter()
            .map(|page| page.to_string().len())
            .sum();
        assert!(
            first_batch > 32 * 1024,
            "{lane}: known capacity must not retain the old 32 KiB cap"
        );
        let mut seen = HashSet::new();
        for page in &pages {
            for message in page["messages"].as_array().unwrap() {
                assert!(
                    seen.insert(message["eventId"].as_str().unwrap()),
                    "{lane}: repeated event"
                );
            }
        }
        assert_eq!(
            seen.len(),
            120,
            "the first eight records occur only in the original preview"
        );
        let reads = reads.lock().unwrap().clone();
        assert_eq!(reads.len(), 17, "start plus each server page exactly once");
        assert_eq!(sent.lock().unwrap().len(), 1);
        let methods = std::fs::read_to_string(root.join("methods")).unwrap();
        assert_eq!(
            methods.lines().filter(|line| *line == "turn/start").count(),
            1
        );
        {
            let state = f.state.lock().unwrap();
            assert_eq!(state.shared.starts, 1);
            let receipt = state
                .shared
                .receipts
                .values()
                .find(|r| r.state == "completed")
                .expect("history turn completes");
            assert!(receipt.usage_complete);
            // The owner's token policy excludes the 100 cached input tokens.
            assert_eq!(
                receipt.known_consumed_tokens, 200,
                "900k cumulative history is never billed"
            );
        }
        assert_eq!(rows(&f, "owner_managed_budget_deltas"), deltas_before);
        f.stop().await;
    }
}
