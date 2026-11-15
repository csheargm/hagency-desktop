use super::*;

#[test]
fn tags_keep_text_models_cloud_first() {
    let tags = json!({"models": [
        {"name": "x/z-image-turbo:latest", "capabilities": ["image"]},
        {"name": "qwen3:8b", "capabilities": ["completion", "tools"]},
        {"name": "gpt-oss:120b-cloud", "remote_host": "https://ollama.com", "capabilities": ["completion"]},
        {"name": "old-model"}]});
    let names: Vec<_> = parse_tags(&tags).into_iter().map(|m| (m.name, m.cloud)).collect();
    assert_eq!(names, vec![("gpt-oss:120b-cloud".into(), true), ("qwen3:8b".into(), false), ("old-model".into(), false)]);
    assert!(parse_tags(&Value::Null).is_empty());
}
#[test]
fn labels_say_where_the_backup_runs() {
    assert_eq!(label("ollama:gpt-oss:120b-cloud"), "Ollama: gpt-oss:120b-cloud (cloud)");
    assert_eq!(label("ollama:qwen3:8b"), "Ollama: qwen3:8b (local)");
    assert_eq!(switched_notice("usage limit", "ollama:qwen3:8b"),
        "ℹ️ Main model unavailable (usage limit); answered by the backup model Ollama: qwen3:8b (local), without tools.");
}
#[test]
fn store_saves_loads_and_clears() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh").join(STORE_FILE);
    assert_eq!(load_from(&path, "a1"), None, "off unless configured");
    save_to(&path, "a1", Some("ollama:qwen3:8b")).unwrap();
    save_to(&path, "a2", Some("ollama:gpt-oss:120b-cloud")).unwrap();
    assert_eq!(load_from(&path, "a1").as_deref(), Some("ollama:qwen3:8b"));
    save_to(&path, "a1", None).unwrap();
    assert_eq!(load_from(&path, "a1"), None);
    assert_eq!(load_from(&path, "a2").as_deref(), Some("ollama:gpt-oss:120b-cloud"));
}
#[test]
fn truncation_keeps_char_boundaries() {
    assert_eq!(truncate("答案", 4), "答");
}

mod retry {
    use super::super::retry_once;
    use hagency_agent_local::codex::Error;
    use hagency_agent_local::provider_failure::{failed_turn, Unavailability};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const BACKUP: &str = "ollama:gpt-oss:120b-cloud";
    async fn run(stored: Option<&str>, main: Error, calls: &AtomicUsize) -> hagency_agent_local::codex::Result<hagency_agent_local::codex::Completed> {
        retry_once(stored.map(str::to_owned), Err(main), |_| async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok("pong".to_owned())
        }).await
    }
    #[tokio::test]
    async fn usage_limit_retries_once_on_the_backup() {
        let calls = AtomicUsize::new(0);
        let done = run(Some(BACKUP), failed_turn(Some(Unavailability::UsageLimit), 0), &calls).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(done.text.starts_with("ℹ️ Main model unavailable (usage limit); answered by the backup model Ollama: gpt-oss:120b-cloud (cloud), without tools."));
        assert!(done.text.ends_with("pong"));
        assert!(!done.usage.is_reported());
    }
    #[tokio::test]
    async fn no_retry_after_a_tool_ran_or_on_denial_or_stop() {
        let calls = AtomicUsize::new(0);
        for main in [failed_turn(Some(Unavailability::UsageLimit), 1), failed_turn(None, 0),
            Error::Ledger(hagency_agent_local::Error::Denied), Error::Timeout] {
            assert!(run(Some(BACKUP), main, &calls).await.is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn without_a_backup_the_failure_stays() {
        let calls = AtomicUsize::new(0);
        assert!(matches!(run(None, Error::Unavailable(Unavailability::Overloaded), &calls).await, Err(Error::Unavailable(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    #[tokio::test]
    async fn a_failed_backup_reports_failure() {
        let failed = retry_once(Some(BACKUP.into()), Err(Error::Unavailable(Unavailability::Outage)), |_| async { Err("backup unreachable") }).await;
        assert!(matches!(failed, Err(Error::Failed)));
    }
}
