use super::check::{CheckError, RepoAccess, check};
use super::*;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "github_pat_11TESTTOKENVALUE";

/// An in-memory `SecretStore`, as the OS store would behave.
#[derive(Default)]
struct MemoryStore(Mutex<HashMap<String, Vec<u8>>>);
fn key(r: &SecretRef) -> String {
    serde_json::to_string(r).unwrap()
}
impl SecretStore for MemoryStore {
    fn get(&self, r: &SecretRef) -> Result<hagency_store::Zeroizing<Vec<u8>>, StoreError> {
        let map = self.0.lock().unwrap();
        map.get(&key(r)).cloned().map(hagency_store::Zeroizing::new).ok_or(StoreError::Missing)
    }
    fn put(&self, r: &SecretRef, bytes: &[u8]) -> Result<(), StoreError> {
        self.0.lock().unwrap().insert(key(r), bytes.to_vec());
        Ok(())
    }
    fn remove(&self, r: &SecretRef) -> Result<(), StoreError> {
        self.0.lock().unwrap().remove(&key(r)).map(|_| ()).ok_or(StoreError::Missing)
    }
}

#[test]
fn tokens_round_trip_per_agent() {
    let store = TokenStore::with_store(Arc::new(MemoryStore::default()));
    let token = Token::parse(&format!("  {TOKEN}\n")).unwrap();
    store.save("agent-a", &token).unwrap();
    assert_eq!(store.load("agent-a").unwrap(), Some(token));
    assert_eq!(store.load("agent-b").unwrap(), None, "another Agent has no token");
    store.remove("agent-a").unwrap();
    assert_eq!(store.load("agent-a").unwrap(), None);
    store.remove("agent-a").unwrap();
}

#[test]
fn demo_file_backend_round_trips_in_a_private_file() {
    let dir = tempfile::tempdir().unwrap();
    let backend = backend_for(true, Some(dir.path().to_path_buf()));
    let store = TokenStore::for_backend(&backend);
    store.save("agent-a", &Token::parse(TOKEN).unwrap()).unwrap();
    assert_eq!(store.load("agent-a").unwrap().unwrap().expose(), TOKEN);
    let files: Vec<_> = std::fs::read_dir(dir.path().join(DEMO_DIRECTORY)).unwrap().map(|e| e.unwrap().path()).collect();
    assert_eq!(files.len(), 1);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777, 0o600);
    }
    store.remove("agent-a").unwrap();
    assert_eq!(store.load("agent-a").unwrap(), None, "a demo run never falls back to the Keychain");
}

#[test]
fn backend_selection_needs_demo_build_and_data_dir() {
    let dir = PathBuf::from("/tmp/demo-data");
    assert_eq!(backend_for(true, Some(dir.clone())), Backend::DemoFile(dir.join(DEMO_DIRECTORY)));
    assert_eq!(backend_for(false, Some(dir)), Backend::Keychain, "product build");
    assert_eq!(backend_for(true, None), Backend::Keychain, "demo build, no demo data dir");
    assert_eq!(backend_for(true, Some(PathBuf::new())), Backend::Keychain);
}

/// This workspace builds hagency-store without `development-credential-files`
/// (as a product build does), so the demo file backend is never chosen, even
/// with a demo data directory set.
#[test]
fn demo_backend_is_compiled_out_without_the_feature() {
    assert!(!development::COMPILED);
    assert_eq!(backend_for(development::COMPILED, Some(PathBuf::from("/tmp/demo"))), Backend::Keychain);
}

#[test]
fn token_never_shows_in_debug_or_errors() {
    let token = Token::parse(TOKEN).unwrap();
    assert!(!format!("{token:?}").contains("TESTTOKEN"));
    let text = format!("error sending request for url (https://x/?t={TOKEN})");
    assert_eq!(redact(&text, TOKEN), "error sending request for url (https://x/?t=[redacted])");
    assert!(Token::parse("   ").is_err());
    assert!(Token::parse("two words").is_err());
}

#[test]
fn runtime_environment_is_the_agents_own_token() {
    let store = TokenStore::with_store(Arc::new(MemoryStore::default()));
    store.save("agent-a", &Token::parse(TOKEN).unwrap()).unwrap();
    assert_eq!(store.runtime_environment("agent-a").unwrap().get("GH_TOKEN"), Some(TOKEN));
    assert!(store.runtime_environment("agent-b").unwrap().is_empty());
}

/// Answers like GitHub: `/user` with TOKEN, and two repositories.
async fn fake_github() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else { return };
            let mut buffer = vec![0; 8192];
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            let path = request.split_whitespace().nth(1).unwrap_or("").to_owned();
            let authorized = request.contains(&format!("Bearer {TOKEN}"));
            let (status, body) = match (authorized, path.as_str()) {
                (false, _) => ("401 Unauthorized", r#"{"message":"Bad credentials"}"#),
                (true, "/user") => ("200 OK", r#"{"login":"csheargm"}"#),
                (true, "/repos/csheargm/hagency-desktop") => ("200 OK", r#"{"permissions":{"admin":true,"push":true,"pull":true}}"#),
                (true, "/repos/hagency-org/hagency-desktop") => ("200 OK", r#"{"permissions":{"push":false,"pull":true}}"#),
                (true, p) if p.starts_with("/search/issues?q=repo:a/b+is:pr+is:open+author:csheargm") => ("200 OK", r#"{"total_count":140,"items":[]}"#),
                _ => ("404 Not Found", r#"{"message":"Not Found"}"#),
            };
            let reply = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    api
}

#[tokio::test]
async fn check_lists_login_and_per_repo_permissions() {
    let api = fake_github().await;
    let repos = ["csheargm/hagency-desktop", "hagency-org/hagency-desktop", "csheargm/private"].map(String::from);
    let report = check(&reqwest::Client::new(), &api, &Token::parse(TOKEN).unwrap(), &repos).await.unwrap();
    assert_eq!(report.login, "csheargm");
    assert_eq!(report.repos[0].1, RepoAccess::Granted { push: true, pull: true });
    assert_eq!(report.repos[1].1, RepoAccess::Granted { push: false, pull: true });
    assert_eq!(report.repos[2].1, RepoAccess::NotReachable);
}

#[tokio::test]
async fn check_reports_a_refused_token() {
    let api = fake_github().await;
    let result = check(&reqwest::Client::new(), &api, &Token::parse("github_pat_WRONG").unwrap(), &[]).await;
    assert_eq!(result, Err(CheckError::Unauthorized));
}

#[test]
fn the_highest_held_permission_is_named_like_the_collaborator_endpoint() {
    use super::check::highest_permission;
    let body = |p: serde_json::Value| serde_json::json!({"full_name": "a/b", "permissions": p});
    assert_eq!(highest_permission(&body(serde_json::json!({"admin": false, "maintain": false, "push": true, "triage": true, "pull": true}))), "write");
    assert_eq!(highest_permission(&body(serde_json::json!({"admin": true, "push": true, "pull": true}))), "admin");
    assert_eq!(highest_permission(&body(serde_json::json!({"pull": true}))), "read");
    assert_eq!(highest_permission(&serde_json::json!({"full_name": "a/b"})), "read", "a public repository read without permissions");
}

#[tokio::test]
async fn the_create_step_permission_read_names_merge_rights_and_a_404_is_none() {
    let api = fake_github().await;
    let token = Token::parse(TOKEN).unwrap();
    let client = reqwest::Client::new();
    assert_eq!(super::check::permission(&client, &api, &token, "csheargm/hagency-desktop").await.unwrap().as_deref(), Some("admin"));
    assert_eq!(super::check::permission(&client, &api, &token, "hagency-org/hagency-desktop").await.unwrap().as_deref(), Some("read"));
    assert_eq!(super::check::permission(&client, &api, &token, "csheargm/private").await.unwrap(), None);
    let bad = Token::parse("github_pat_WRONG").unwrap();
    assert!(super::check::permission(&client, &api, &bad, "csheargm/hagency-desktop").await.is_err(), "a refused token is an error, not a missing repository");
}

#[tokio::test]
async fn open_pull_requests_are_counted_past_the_first_page() {
    let api = fake_github().await;
    let count = crate::issue_watch::fetch_open_pr_count(&reqwest::Client::new(), &api, "a/b", "csheargm", TOKEN).await.unwrap();
    assert_eq!(count, 140);
    assert!(crate::issue_watch::fetch_open_pr_count(&reqwest::Client::new(), &api, "a/b", "bad login", TOKEN).await.is_err());
}
