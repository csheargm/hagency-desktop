//! Tests run the account calls against a canned local "Pasion" that answers
//! by request path, recording each request line and body.
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use super::*;

type Log = Arc<Mutex<Vec<String>>>;

/// Serve `routes` (path suffix -> JSON body) until the test ends.
fn fake_pasion(routes: Vec<(&'static str, &'static str)>) -> (String, Log) {
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
            seen.lock().unwrap().push(format!("{} {}", line.trim(), String::from_utf8_lossy(&body)));
            let reply = routes.iter().find(|(p, _)| path.ends_with(p)).map_or("{}", |(_, r)| r);
            let mut out = stream;
            write!(out, "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    });
    (origin, log)
}

#[test]
fn github_detection_uses_brand_name() {
    let p = |brand: Option<&str>| UpstreamProvider { id: "01".into(), brand_name: brand.map(str::to_owned), human_name: None };
    assert!(offers_github(&[p(Some("google")), p(Some("GitHub"))]));
    assert!(!offers_github(&[p(Some("google")), p(None)]));
    assert!(!offers_github(&[]));
}

#[test]
fn email_syntax_check() {
    for ok in ["a@b.co", " octo@example.com "] { assert!(plausible_email(ok), "{ok}"); }
    for bad in ["", "a", "a@b", "@b.co", "a@b..co", "a b@c.co", "a@b@c.co"] { assert!(!plausible_email(bad), "{bad}"); }
}

#[tokio::test]
async fn lists_providers() {
    let (origin, _) = fake_pasion(vec![("/providers", r#"{"providers":[{"id":"01H","human_name":"GitHub","brand_name":"github","authorize_url":"/x"}]}"#)]);
    let providers = upstream_providers(&origin).await.unwrap();
    assert!(offers_github(&providers));
}

#[tokio::test]
async fn recovery_start_posts_the_email_and_surfaces_errors() {
    let (origin, log) = fake_pasion(vec![("/recovery/start", r#"{"status":"success","id":"r1"}"#)]);
    start_recovery(&origin, " octo@example.com ").await.unwrap();
    assert!(log.lock().unwrap()[0].contains(r#""email":"octo@example.com""#));
    let (origin, _) = fake_pasion(vec![("/recovery/start", r#"{"status":"error","error":"recovery_disabled"}"#)]);
    let error = start_recovery(&origin, "a@b.co").await.unwrap_err().to_string();
    assert!(error.contains("recovery_disabled"), "{error}");
}

#[tokio::test]
async fn registration_with_email_stops_for_the_code_then_finishes() {
    let (origin, log) = fake_pasion(vec![
        ("/auth/login", r#"{"status":"error"}"#),
        ("/auth/register", r#"{"status":"success","id":"reg1","next_step":"verify_email"}"#),
        ("/register/reg1/verify-email", r#"{"status":"success","next_step":"finish"}"#),
        ("/register/reg1/finish", r#"{"status":"success"}"#),
    ]);
    let started = register(&origin, "octo", "pw", Some("octo@example.com")).await.unwrap();
    assert_eq!(started, Registration::NeedsEmailCode { registration_id: "reg1".into() });
    assert!(log.lock().unwrap().iter().any(|l| l.contains(r#""email":"octo@example.com""#)));
    let done = verify_registration_email(&origin, "reg1", "octo", " 123456 ").await.unwrap();
    assert_eq!(done, Registration::Created);
    assert!(log.lock().unwrap().iter().any(|l| l.contains("verify-email") && l.contains(r#""code":"123456""#)));
}

#[tokio::test]
async fn registration_without_email_sends_no_email_field() {
    let (origin, log) = fake_pasion(vec![
        ("/auth/login", r#"{"status":"error"}"#),
        ("/auth/register", r#"{"status":"success","id":"reg2","next_step":"finish"}"#),
        ("/register/reg2/finish", r#"{"status":"success"}"#),
    ]);
    assert_eq!(register(&origin, "octo", "pw", Some("  ")).await.unwrap(), Registration::Created);
    assert!(!log.lock().unwrap().iter().any(|l| l.contains("\"email\"")));
}
