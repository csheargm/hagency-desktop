//! Standalone Rinx with its own local assistant runtime (ADR 0007), at the
//! process level: the packaged kernel starts on the first authorized request,
//! two mini apps share it with isolated request state, a restart resumes the
//! same peer, and exit stops only the runtime Rinx owns.
//!
//! Needs an `octos` kernel with UPCR-2026-034 (octos-org/octos#2555) as
//! `RINX_TEST_OCTOS_BIN` and python3 for the scripted model
//! (`tests/fixtures/mock_llm.py`); without the kernel it says so and passes.
#![cfg(feature = "octos-local")]

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use octosense_app_peers::octos_core::{Core, Options};
use octosense_app_peers::*;

struct Child(std::process::Child, u16);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn model() -> Child {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/mock_llm.py");
    let mut child = std::process::Command::new("python3")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    Child(child, line.trim().parse().unwrap())
}

fn spec(instance: &str) -> ContextSpec {
    ContextSpec {
        account: "@alice:example.org".into(),
        instance: instance.into(),
        services: OCTOS_SERVICES.iter().map(|s| s.to_string()).collect(),
    }
}

fn turn(ctx: &Arc<dyn OctosContext>, text: &str) -> Result<serde_json::Value, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let tx = Mutex::new(tx);
    ctx.call(ContextOp::Turn { text: text.into() }, Arc::new(move |e| { let _ = tx.lock().unwrap().send(e); }))?;
    loop {
        match rx.recv_timeout(Duration::from_secs(90)).map_err(|_| "timed out".to_owned())? {
            ContextEvent::Complete(r) => return r,
            ContextEvent::Data(_) => {}
        }
    }
}

fn kernel_processes(core_dir: &std::path::Path) -> usize {
    let out = std::process::Command::new("pgrep").args(["-f", &core_dir.to_string_lossy()]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).lines().count()
}

#[test]
fn standalone_rinx_owns_one_local_runtime_and_stops_only_it() {
    let Some(kernel) = std::env::var_os("RINX_TEST_OCTOS_BIN") else {
        eprintln!("RINX_TEST_OCTOS_BIN is not set: skipping");
        return;
    };
    let data = std::env::temp_dir().join(format!("rinx-standalone-local-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    // Rinx's own data root and the packaged kernel, set explicitly (never PATH).
    unsafe {
        std::env::set_var("HAGENCY_DESKTOP_DATA_DIR", &data);
        // Release validation runs a copy of this executable beside the
        // packaged kernel, exercising production discovery without overrides.
        if std::env::var_os("RINX_TEST_PACKAGED_OCTOS").is_some() {
            std::env::remove_var("RINX_OCTOS_BIN");
        } else {
            std::env::set_var("RINX_OCTOS_BIN", &kernel);
        }
    }
    let model = model();
    rinx::octos_service::write_local_provider("local", "mock-model", &format!("http://127.0.0.1:{}/v1", model.1), "").unwrap();
    let core_dir = rinx::octos_service::local_core_dir();
    assert!(core_dir.starts_with(&data), "the kernel root is Rinx-owned: {}", core_dir.display());

    // Someone else's kernel on another core dir must survive Rinx's exit.
    let other_dir = data.join("someone-else/.octos");
    std::fs::create_dir_all(other_dir.join("profiles")).unwrap();
    std::fs::copy(core_dir.join("profiles/_main.json"), other_dir.join("profiles/_main.json")).unwrap();
    let other = Core::new(Options::default().core_dir(&other_dir).program(&kernel));
    let _other_conn = other.connect().unwrap();

    let service = rinx::octos_service::local_service().unwrap();
    assert_eq!(service.deployment(), Deployment::StandaloneLocal);
    assert_eq!(service.settings_entry(), SettingsEntry::AppLocal);
    assert_eq!(kernel_processes(&core_dir), 0, "nothing runs before the first request");
    service.set_account(Some("@alice:example.org"));
    let notes = service.open_context(spec("dev.example.notes-g1")).unwrap();
    let poll = service.open_context(spec("dev.example.poll-g1")).unwrap();
    assert_eq!(turn(&notes, "notes question").unwrap()["text"], "ECHO: notes question");
    assert_eq!(turn(&poll, "poll question").unwrap()["text"], "ECHO: poll question");
    assert_eq!(kernel_processes(&core_dir), 1, "one runtime shared by both mini apps");
    // Isolated request state: separate context workspaces and memory namespaces.
    let peers = core_dir.join("profiles/_main/data/peers");
    let peer = std::fs::read_dir(&peers).unwrap().flatten().next().unwrap().path();
    let contexts: Vec<String> = std::fs::read_dir(&peer)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("context-"))
        .collect();
    assert_eq!(contexts.len(), 2, "{contexts:?}");
    assert_eq!(std::fs::read_to_string(peer.join("originator")).unwrap(), "_main:api:rinx#root");

    // Exit: only Rinx's runtime stops.
    service.shutdown();
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(kernel_processes(&core_dir), 0, "Rinx's runtime stopped");
    assert!(other.status().running, "another owner's kernel keeps running");
    assert_eq!(kernel_processes(&other_dir), 1);

    // Restart: the same peer resumes.
    let service = rinx::octos_service::local_service().unwrap();
    service.set_account(Some("@alice:example.org"));
    let again = service.open_context(spec("dev.example.notes-g2")).unwrap();
    assert_eq!(turn(&again, "after restart").unwrap()["text"], "ECHO: after restart");
    let peers_now: Vec<_> = std::fs::read_dir(&peers).unwrap().flatten().collect();
    assert_eq!(peers_now.len(), 1, "resumed, not re-created");
    service.shutdown();
    drop(_other_conn);
    other.shutdown_within(Duration::from_secs(5));
    let _ = std::fs::remove_dir_all(&data);
}
