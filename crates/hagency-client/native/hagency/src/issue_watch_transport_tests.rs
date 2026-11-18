//! A GitHub request that cannot connect is reported as "can't reach GitHub"
//! with its whole cause chain; the same client still works once the server
//! answers.
use super::{FetchError, UNREACHABLE, fetch_open_issues, new_client};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// An address nothing listens on (bound, then released).
async fn closed_api() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[tokio::test]
async fn refused_connection_is_unreachable_with_its_cause_chain() {
    let error = fetch_open_issues(&new_client(), &closed_api().await, "owner/repo", "t").await.unwrap_err();
    let FetchError::Unreachable(chain) = &error else { panic!("expected Unreachable, got {error:?}") };
    // The chain names the cause, not only reqwest's "error sending request".
    assert!(chain.contains("error sending request"), "{chain}");
    assert!(chain.matches(": ").count() >= 2, "cause chain missing: {chain}");
    assert!(error.to_string().starts_with(UNREACHABLE), "{error}");
}

#[tokio::test]
async fn the_client_works_again_once_the_server_answers() {
    let client = new_client();
    let failed = fetch_open_issues(&client, &closed_api().await, "owner/repo", "t").await;
    assert!(matches!(failed, Err(FetchError::Unreachable(_))), "{failed:?}");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = [0u8; 4096];
        let _ = socket.read(&mut buf).await.unwrap();
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n[]").await.unwrap();
    });
    let issues = fetch_open_issues(&client, &api, "owner/repo", "t").await.unwrap();
    server.await.unwrap();
    assert!(issues.is_empty());
}

/// Serves `bodies` in order and hands back each request's text.
async fn recording(bodies: [&'static str; 2]) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let mut seen = Vec::new();
        for body in bodies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let n = socket.read(&mut buf).await.unwrap();
            seen.push(String::from_utf8_lossy(&buf[..n]).to_lowercase());
            let reply = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
            socket.write_all(reply.as_bytes()).await.unwrap();
        }
        seen
    });
    (api, server)
}

/// ADR 0023: room state widens card-free reads only to repositories GitHub
/// shows without a token; the check itself sends none.
#[tokio::test]
async fn visibility_is_read_without_a_token() {
    let (api, server) = recording([r#"{"private":false}"#, r#"{"private":true}"#]).await;
    let client = new_client();
    assert!(super::fetch_is_public(&client, &api, "owner/repo").await.unwrap());
    assert!(!super::fetch_is_public(&client, &api, "owner/secret").await.unwrap());
    assert!(server.await.unwrap().iter().all(|r| !r.contains("authorization:")));
    assert!(super::fetch_is_public(&client, &api, "not a repo").await.is_err());
}

/// ADR 0023: a repository only another owner watches is read without this
/// device's token; open and recently closed items come back together.
#[tokio::test]
async fn a_public_status_read_sends_no_token_and_an_own_read_does() {
    let open = r#"[{"number":3,"html_url":"u","title":"Open","created_at":"2026-11-16T00:00:00Z"}]"#;
    let closed = r#"[{"number":2,"title":"Fix","pull_request":{"merged_at":"2026-11-16T00:00:00Z"},"closed_at":"2026-11-16T00:00:00Z"}]"#;
    let (api, server) = recording([open, closed]).await;
    let items = super::fetch_status_items(&new_client(), &api, "owner/repo", None, 0).await.unwrap();
    assert_eq!(items.iter().map(|i| i.number).collect::<Vec<_>>(), vec![3, 2]);
    assert!(server.await.unwrap().iter().all(|r| !r.contains("authorization:")));
    let (api, server) = recording([open, closed]).await;
    super::fetch_status_items(&new_client(), &api, "owner/repo", Some("tok"), 0).await.unwrap();
    assert!(server.await.unwrap().iter().all(|r| r.contains("authorization: bearer tok")));
}
