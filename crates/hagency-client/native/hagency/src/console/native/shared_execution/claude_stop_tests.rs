//! Full host cancellation keeps the original provider and usage custody.
use super::*;

fn markers(root: &std::path::Path, extension: &str) -> Vec<std::path::PathBuf> {
    let mut result = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                result.extend(markers(&path, extension));
            } else if path.extension().is_some_and(|value| value == extension) {
                result.push(path);
            }
        }
    }
    result
}

#[tokio::test]
#[ignore = "explicit offline Claude probe; original host, native pipes and canonical ledger"]
async fn native_shared_claude_daemon_stop_collects_final_output_usage() {
    stopped_turn(false).await;
}

#[tokio::test]
#[ignore = "explicit offline Claude probe; owner policy and native cooperative stop"]
async fn native_shared_claude_explicit_owner_idle_policy_settles_silent_child() {
    stopped_turn(true).await;
}

async fn stopped_turn(owner_idle: bool) {
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
    }
    let root = f._root.path().join("claude-stop");
    hagency_store::private::directory(&root).unwrap();
    let root = root.canonicalize().unwrap();
    if owner_idle {
        hagency_store::private::replace(
            &root.join("shared-claude-turn-policy.json"),
            br#"{"noProgressMs":500}"#,
        )
        .unwrap();
    }
    transport
        .poll_claude_fixture(
            &f.registry,
            command.reserve_command.resource_id.clone(),
            root.clone(),
            binding,
            "shared-interrupt",
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while markers(&root, "awaiting-interrupt").is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("original prompt started and produced partial usage");
    assert_eq!(markers(&root, "awaiting-interrupt").len(), 1);
    assert_eq!(markers(&root, "prompt").len(), 1);
    if owner_idle {
        stopped(&f).await;
    } else {
        f.owner.suspend().await.unwrap();
    }
    // This is original SQLite evidence even when the canceled management
    // session cannot upload a final receipt. No provider or model is restarted.
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let (state, tokens, complete): (String, u64, bool) = db
        .query_row(
            "SELECT state,known_consumed_tokens,usage_complete FROM owner_managed_calls",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    // Shared accounting excludes cached reads: 10 input + 4 creation + 2
    // terminal output. The partial's deliberately bogus output=999 is ignored.
    assert_eq!((state.as_str(), tokens, complete), ("settled", 16, true));
    let raw: String = db.query_row("SELECT payload FROM owner_shared_work_observations ORDER BY ledger_revision DESC LIMIT 1", [], |row| row.get(0)).unwrap();
    let receipt: SharedWorkReceipt = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        receipt.state, "failed",
        "accepted interruption is not a successful task"
    );
    assert_eq!(receipt.known_consumed_tokens, 16);
    assert!(receipt.usage_complete);
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    drop(db);
    f.stop().await;
}
