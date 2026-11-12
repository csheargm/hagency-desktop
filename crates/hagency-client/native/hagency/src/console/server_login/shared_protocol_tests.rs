use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn fixture(
    replies: Vec<(u16, Value)>,
) -> (ServerLogin, tokio::task::JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let login = ServerLogin::new(None).unwrap();
    let now = Instant::now();
    let (device, _) = fixture_device(&origin, "owner", "@owner:test");
    login.sessions.lock().await.insert(
        session_key("protocol-cookie"),
        RemoteSession {
            matrix_source: None,
            token: "owner-session".into(),
            oauth_token: "oauth-access".into(),
            user_id: "owner".into(),
            device: Some(device),
            refresh_token: None,
            oauth_expires: now + Duration::from_secs(900),
            authorized_until: now + Duration::from_secs(900),
            invalidated: false,
            binding: Binding {
                origin,
                client_id: "client".into(),
                installation_id: "protocol-fixture-installation".into(),
                name: "fixture".into(),
                owner: Some("@owner:test".into()),
                subject: Some("owner".into()),
            },
            checked: now,
            expires: now + Duration::from_secs(900),
        },
    );
    let server = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut raw = Vec::new();
            loop {
                let mut chunk = [0u8; 4096];
                let size = socket.read(&mut chunk).await.unwrap();
                assert!(size > 0, "request ended before its declared body");
                raw.extend_from_slice(&chunk[..size]);
                assert!(raw.len() <= 16 * 1024, "unexpected fixture request size");
                let request = String::from_utf8_lossy(&raw);
                if let Some((headers, body)) = request.split_once("\r\n\r\n") {
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (name, value) = line.split_once(':')?;
                            name.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(raw).unwrap());
            let body = body.to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
        requests
    });
    (login, server)
}

fn header_values<'a>(request: &'a str, name: &str) -> Vec<&'a str> {
    request
        .split_once("\r\n\r\n")
        .unwrap()
        .0
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
        .collect()
}

#[tokio::test]
async fn shared_protocol_header_is_sent_only_by_the_sealed_native_shared_request() {
    use crate::console::native::allocation_transport::protocol_header_fixture;

    let (login, server) = fixture(vec![(200, json!({"ok":true})); 3]).await;
    for operation in [
        protocol_header_fixture(true),
        protocol_header_fixture(false),
        OwnerOperation::Agents,
    ] {
        let reply = tokio::time::timeout(
            Duration::from_secs(5),
            login.owner_api("protocol-cookie", operation),
        )
        .await
        .expect("local request must settle")
        .unwrap();
        assert_eq!(reply.owner, "@owner:test");
        assert_eq!(reply.value, json!({"ok":true}));
    }
    let requests = server.await.unwrap();
    assert!(
        requests[0].starts_with("POST /hagency/api/v1/management/shared-execution/runtime/poll ")
    );
    assert!(requests[1].starts_with("GET /hagency/api/v1/management/session "));
    assert!(requests[2].starts_with("GET /hagency/api/v1/agents "));
    assert_eq!(
        header_values(&requests[0], "x-hagency-shared-protocol"),
        ["native-context-v3;claude-v4"]
    );
    for request in &requests[1..] {
        assert!(header_values(request, "x-hagency-shared-protocol").is_empty());
    }
    let device_bearer = format!("Bearer {}", "e".repeat(64));
    for request in &requests[..2] {
        assert_eq!(
            header_values(request, "authorization"),
            [device_bearer.as_str()]
        );
    }
    assert_eq!(
        header_values(&requests[2], "authorization"),
        ["Bearer owner-session"]
    );
}

#[tokio::test]
async fn shared_protocol_named_refusals_survive_the_native_http_boundary() {
    use crate::console::native::allocation_transport::protocol_header_fixture;

    let codes = ["owner_daemon_update_required", "owner_daemon_unavailable"];
    let replies = codes
        .iter()
        .map(|code| (409, json!({"code":code,"privateDetail":"must-not-surface"})))
        .collect();
    let (login, server) = fixture(replies).await;
    for code in codes {
        let error = tokio::time::timeout(
            Duration::from_secs(5),
            login.owner_api("protocol-cookie", protocol_header_fixture(true)),
        )
        .await
        .expect("local refusal must settle")
        .err()
        .expect("server refused this new operation");
        assert_eq!(error.status, 409);
        assert_eq!(error.code, code);
    }
    for request in server.await.unwrap() {
        assert_eq!(
            header_values(&request, "x-hagency-shared-protocol"),
            ["native-context-v3;claude-v4"]
        );
    }
}
