//! A GitHub request that cannot connect is reported as "can't reach GitHub"
//! with its whole cause chain, and counts toward rebuilding the client; a
//! request that reaches the server starts the count again.
use super::{FetchError, fetch_open_issues, new_client, reset_transport_failures, transport_failures};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One test at a time: the failure count is process-wide.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// An address nothing listens on (bound, then released).
async fn closed_api() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[tokio::test]
async fn refused_connection_is_unreachable_with_its_cause_chain() {
    let _serial = SERIAL.lock().await;
    reset_transport_failures();
    let error = fetch_open_issues(&new_client(), &closed_api().await, "owner/repo", "t").await.unwrap_err();
    let FetchError::Unreachable(chain) = &error else { panic!("expected Unreachable, got {error:?}") };
    // The chain names the cause, not only reqwest's "error sending request".
    assert!(chain.contains("error sending request"), "{chain}");
    assert!(chain.matches(": ").count() >= 2, "cause chain missing: {chain}");
    assert!(error.to_string().starts_with("Can't reach GitHub (network/DNS)"), "{error}");
    assert_eq!(transport_failures(), 1);
}

#[tokio::test]
async fn a_request_that_reaches_github_resets_the_failure_count() {
    let _serial = SERIAL.lock().await;
    reset_transport_failures();
    let client = new_client();
    let _ = fetch_open_issues(&client, &closed_api().await, "owner/repo", "t").await;
    let _ = fetch_open_issues(&client, &closed_api().await, "owner/repo", "t").await;
    assert_eq!(transport_failures(), 2);
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
    assert_eq!(transport_failures(), 0);
}
