//! A failed private archive cannot erase original physical-stop evidence.
use super::*;

#[cfg(unix)]
#[tokio::test]
#[ignore = "explicit offline Claude probe; real canonical ledger and whole-tree stop"]
async fn native_shared_claude_archive_failure_keeps_the_original_stopped_receipt() {
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
    let root = f._root.path().join("claude-archive-failure");
    hagency_store::private::directory(&root).unwrap();
    let root = root.canonicalize().unwrap();
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
    let marker = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let scopes = root.join("work/scopes");
            if let Ok(entries) = std::fs::read_dir(scopes) {
                for entry in entries.flatten() {
                    let marker = entry.path().join("work/shared-claude.awaiting-interrupt");
                    if marker.exists() {
                        return marker;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("original model turn is running");
    let scope = marker.parent().unwrap().parent().unwrap();
    let outside = root.join("owner-secret");
    std::fs::write(&outside, "must never be archived").unwrap();
    std::os::unix::fs::symlink(&outside, scope.join("config/escape")).unwrap();
    assert!(
        f.owner.suspend().await.is_err(),
        "archive refusal must remain an explicit incomplete drain"
    );
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let stopped: u64 = db.query_row(
        "SELECT COUNT(*) FROM owner_shared_process_observations WHERE json_extract(payload,'$.state')='stopped'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(
        stopped, 1,
        "physical stop is durable before the rejected archive"
    );
    let (tokens, complete): (u64, bool) = db
        .query_row(
            "SELECT known_consumed_tokens,usage_complete FROM owner_managed_calls",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((tokens, complete), (16, true));
    assert_eq!(
        std::fs::read_to_string(outside).unwrap(),
        "must never be archived"
    );
    assert!(!scope.join("config").exists());
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    drop(db);
    f.stop().await;
}
