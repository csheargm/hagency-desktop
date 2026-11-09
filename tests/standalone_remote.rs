//! Standalone Rinx with an explicit remote octos server (ADR 0007), against
//! a real `octos serve` started here with a fixture bearer token: an
//! authenticated connection and a server-provisioned scoped workspace work;
//! a wrong token is refused; an account change and a server/profile
//! replacement drop late replies; Rinx's exit leaves the server running.
//!
//! Needs `RINX_TEST_OCTOS_BIN` (octos with UPCR-2026-034) and python3; without
//! the kernel it says so and passes. No real credentials: the token is a
//! fixture and the model is `tests/fixtures/mock_llm.py`.
#![cfg(feature = "octos-remote")]

use std::io::BufRead;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use octosense_app_peers::*;

struct Proc(std::process::Child);
impl Drop for Proc {
    fn drop(&mut self) {
        // `octos serve` starts gateway children: stop its whole process group.
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &format!("-{}", self.0.id())])
            .status();
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn model() -> (Proc, u16) {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_llm.py");
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    let port = line.trim().parse().unwrap();
    (Proc(child), port)
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

fn server_alive(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

fn spec(account: &str, instance: &str) -> ContextSpec {
    ContextSpec {
        account: account.into(),
        instance: instance.into(),
        services: OCTOS_SERVICES.iter().map(|s| s.to_string()).collect(),
    }
}

fn call(ctx: &Arc<dyn OctosContext>, op: ContextOp) -> std::sync::mpsc::Receiver<ContextEvent> {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    ctx.call(op, Arc::new(move |e| { let _ = tx.lock().unwrap().send(e); })).unwrap();
    rx
}

fn complete(rx: &std::sync::mpsc::Receiver<ContextEvent>, wait: Duration) -> Option<Result<serde_json::Value, String>> {
    let deadline = std::time::Instant::now() + wait;
    loop {
        match rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(ContextEvent::Complete(r)) => return Some(r),
            Ok(ContextEvent::Data(_)) => {}
            Err(_) => return None,
        }
    }
}

#[test]
fn standalone_rinx_uses_an_explicit_authenticated_remote_server() {
    let Some(kernel) = std::env::var_os("RINX_TEST_OCTOS_BIN") else {
        eprintln!("RINX_TEST_OCTOS_BIN is not set: skipping");
        return;
    };
    let dir = std::env::temp_dir().join(format!("rinx-standalone-remote-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let server_dir = dir.join("server");
    let rinx_dir = dir.join("rinx");
    std::fs::create_dir_all(server_dir.join("profiles")).unwrap();
    unsafe { std::env::set_var("HAGENCY_DESKTOP_DATA_DIR", &rinx_dir) };
    let (_model, model_port) = model();
    std::fs::write(
        server_dir.join("profiles/_main.json"),
        serde_json::json!({
            "id": "_main", "name": "Main", "enabled": true,
            "created_at": "2026-09-27T00:00:00Z", "updated_at": "2026-09-27T00:00:00Z",
            "config": {"llm": {"primary": {"family_id": "local", "model_id": "mock-model",
                "route": {"base_url": format!("http://127.0.0.1:{model_port}/v1"), "api_type": "openai"}}}}
        })
        .to_string(),
    )
    .unwrap();
    let port = free_port();
    let token = "rinx-test-fixture-token";
    let _server = Proc(
        std::process::Command::new(&kernel)
            .args(["serve", "--port", &port.to_string(), "--auth-token", token, "--data-dir"])
            .arg(&server_dir)
            .env("OCTOS_HOME", &server_dir)
            .process_group(0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if server_alive(port) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let url = format!("http://127.0.0.1:{port}");

    // A wrong token is refused; nothing runs.
    rinx::octos_service::use_remote(&url, "_main", "not-the-token").unwrap();
    let service = rinx::octos_service::service().unwrap();
    service.set_account(Some("@alice:example.org"));
    let ctx = service.open_context(spec("@alice:example.org", "notes-g1")).unwrap();
    let refused = complete(&call(&ctx, ContextOp::Open), Duration::from_secs(20)).expect("an answer");
    assert!(refused.is_err(), "{refused:?}");

    // The right token: an authenticated connection and a scoped workspace.
    rinx::octos_service::use_remote(&url, "_main", token).unwrap();
    let service = rinx::octos_service::service().unwrap();
    assert_eq!(service.deployment(), Deployment::StandaloneRemote);
    assert!(!ctx.is_open(), "replacing the server revoked the old contexts");
    service.set_account(Some("@alice:example.org"));
    let ctx = service.open_context(spec("@alice:example.org", "notes-g2")).unwrap();
    let answer = complete(&call(&ctx, ContextOp::Turn { text: "remote question".into() }), Duration::from_secs(90))
        .unwrap()
        .unwrap();
    assert_eq!(answer["text"], "ECHO: remote question");
    let workspaces = server_dir.join("profiles/_main/data/app-workspaces/app/rinx");
    assert!(workspaces.is_dir(), "the server provisioned the workspace: {}", workspaces.display());
    assert!(!rinx_dir.join("octos/.octos").exists(), "no local runtime in remote mode");
    let saved = std::fs::read_to_string(rinx_dir.join("octos/assistant.json")).unwrap();
    assert!(!saved.contains(token), "the token is never written to disk");

    // An account change drops the late reply of the previous account.
    let slow = call(&ctx, ContextOp::Turn { text: "SLOW private".into() });
    std::thread::sleep(Duration::from_secs(2));
    service.set_account(Some("@bob:example.org"));
    assert!(complete(&slow, Duration::from_secs(25)).is_none(), "a stale reply reached the next account");

    // Exit leaves the server running.
    rinx::octos_service::shutdown();
    std::thread::sleep(Duration::from_secs(1));
    assert!(server_alive(port), "Rinx's exit must not stop the server");
    let _ = std::fs::remove_dir_all(&dir);
}
