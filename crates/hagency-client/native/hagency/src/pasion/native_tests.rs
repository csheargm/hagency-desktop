//! The native client against a canned local server.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;

type Log = Arc<Mutex<Vec<String>>>;

/// Serve `routes` (path prefix -> status, JSON) and record each request.
fn fake(routes: Vec<(&'static str, u16, String)>) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let log: Log = Arc::default();
    let seen = log.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                if header.trim().is_empty() { break; }
                if let Some(v) = header.to_ascii_lowercase().strip_prefix("content-length:") { length = v.trim().parse().unwrap(); }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let path = line.split_whitespace().nth(1).unwrap_or_default().to_owned();
            seen.lock().unwrap().push(format!("{path} {}", String::from_utf8_lossy(&body)));
            let (status, reply) = routes.iter().find(|(p, ..)| path.contains(p)).map_or((404, "{}".to_owned()), |(_, s, r)| (*s, r.clone()));
            let mut out = stream;
            write!(out, "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    });
    (origin, log)
}

const AUTHORIZE: &str = "http://h/auth/authorize?client_id=01NATIVE&redirect_uri=http%3A%2F%2F127.0.0.1%3A5123%2F&scope=openid&state=s1&code_challenge=abc&code_challenge_method=S256&prompt=login";

#[tokio::test]
async fn missing_api_means_none_and_config_parses() {
    let (origin, _) = fake(vec![]);
    assert_eq!(config(&origin).await.unwrap(), None);
    let body = r#"{"client_id":"01NATIVE","password_registration_enabled":true,"providers":[{"id":"01G","brand_name":"google"}]}"#;
    let (origin, _) = fake(vec![("/native/config", 200, body.into())]);
    let cfg = config(&origin).await.unwrap().unwrap();
    assert_eq!(cfg.client_id, "01NATIVE");
    assert!(cfg.provider("Google").is_some() && cfg.provider("github").is_none());
}

#[tokio::test]
async fn login_sends_the_sdk_request_and_returns_only_its_loopback() {
    let ok = r#"{"status":"success","redirect_url":"http://127.0.0.1:5123/?code=c&state=s1","username":"amy"}"#;
    let (origin, log) = fake(vec![("/native/login", 200, ok.into())]);
    let url = authorize(&origin, AUTHORIZE, "amy", "pw", false).await.unwrap();
    assert_eq!(url, "http://127.0.0.1:5123/?code=c&state=s1");
    let sent = log.lock().unwrap()[0].clone();
    for part in [r#""client_id":"01NATIVE""#, r#""code_challenge_method":"S256""#, r#""state":"s1""#, r#""username":"amy""#] {
        assert!(sent.contains(part), "{sent}");
    }
    let elsewhere = r#"{"status":"success","redirect_url":"http://127.0.0.1:9/?code=c"}"#;
    let (origin, _) = fake(vec![("/native/register", 200, elsewhere.into())]);
    assert!(authorize(&origin, AUTHORIZE, "amy", "pw", true).await.is_err());
}

#[tokio::test]
async fn server_error_codes_surface() {
    let (origin, _) = fake(vec![("/native/login", 401, r#"{"status":"error","error":"invalid_credentials"}"#.into())]);
    let error = authorize(&origin, AUTHORIZE, "amy", "bad", false).await.unwrap_err().to_string();
    assert!(error.contains("invalid_credentials"), "{error}");
    assert!(request_body("http://h/authorize?client_id=x", "a", "b").is_err(), "incomplete SDK request");
    let (origin, _) = fake(vec![("/native/login", 503, "<html>bad gateway</html>".into())]);
    let error = authorize(&origin, AUTHORIZE, "amy", "pw", false).await.unwrap_err().to_string();
    assert!(error.contains("503"), "{error}");
    let ok = r#"{"status":"success","redirect_url":"http://127.0.0.1:5123/?code=c&state=s1"}"#;
    let (origin, _) = fake(vec![("/native/login", 500, ok.into())]);
    assert!(authorize(&origin, AUTHORIZE, "amy", "pw", false).await.is_err(), "success JSON on an error status");
}

#[tokio::test]
async fn availability_maps_statuses() {
    let (origin, _) = fake(vec![("username=Bob", 200, r#"{"username":"bob","status":"available"}"#.into()), ("username=_x", 200, r#"{"username":null,"status":"invalid"}"#.into())]);
    assert_eq!(username_available(&origin, "Bob").await.unwrap(), Availability::Available("bob".into()));
    assert_eq!(username_available(&origin, "_x").await.unwrap(), Availability::Invalid);
}
