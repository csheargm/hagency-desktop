#![cfg(unix)]
//! "Ollama (local)" through the real Codex app-server: the provider binding is
//! the only difference, so host approval, accounting and the ledger are the
//! same code paths. Ignored by default: needs HAGENCY_TEST_CODEX_EXECUTABLE.
//! The live test also needs HAGENCY_OLLAMA_E2E_MODEL and a running Ollama.
use hagency_agent_local::{
    Budget, Layer, Ledger, Limit, Period, Policy, RequestPolicy, Scope, ToolPolicy,
    codex::{self, BudgetMode, Profile, ollama::LocalModel},
    runtime,
};
use std::{fs, os::unix::fs::PermissionsExt, path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, BufReader};

const FAKE_TIMEOUT_S: u64 = 60;
const LIVE_TIMEOUT_S: u64 = 900;

struct Rig { tmp: tempfile::TempDir, profile: Profile, ledger: Ledger, scope: Scope }

fn rig(endpoint: &str, model: &str) -> Rig {
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_CODEX_EXECUTABLE").expect("explicit native CLI"))
        .canonicalize().unwrap();
    let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for name in ["home", "provider", "work"] {
        fs::create_dir(root.join(name)).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let profile = Profile {
        executable, home: root.join("home"), codex_home: root.join("provider"), cwd: root.join("work"),
        model: model.into(), effort: "low".into(), shared_auth: false, web_search: false,
        environment: Default::default(), local_model: Some(LocalModel::new(endpoint).unwrap()),
    };
    let scope = Scope { agent: "agent".into(), binding: "binding".into(), room: "!room:test".into(),
        requester: "@owner:test".into(), thread: "root".into() };
    let mut ledger = Ledger::open(root.join("ledger"), "@owner:test").unwrap();
    ledger.register_binding("@owner:test", &scope).unwrap();
    let policy = Policy { budget: Budget { limit: Limit::Unlimited, period: Period::Lifetime },
        requests: RequestPolicy::Allow, high_risk: ToolPolicy::AskOwner };
    ledger.set_agent_policy("@owner:test", "agent", 0, &policy).unwrap();
    for layer in [Layer::Room, Layer::Requester] { ledger.set_policy("@owner:test", &scope, layer, 0, &policy).unwrap(); }
    Rig { tmp, profile, ledger, scope }
}

/// Runs one turn; the owner answers every approval with `allow`.
async fn turn(r: &mut Rig, call: &str, input: &str, allow: bool, limit: u64) -> (codex::Completed, usize) {
    let guardian = Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut process = runtime::spawn_for_scope_with_guardian(&r.profile, "ollama-test", &mut r.ledger, &r.scope, Some(guardian))
        .await.unwrap();
    process.session.initialize().await.unwrap();
    process.session.verify_host_environment().await.unwrap();
    process.session.require_local_account().await.unwrap();
    process.session.open_context(&mut r.ledger, &r.scope, &r.profile).await.unwrap();
    let (queue, mut pending) = codex::approval_queue(4).unwrap();
    let owner = tokio::spawn(async move {
        let mut asked = 0;
        while let Some(p) = pending.recv().await { asked += 1; p.decide(allow); }
        asked
    });
    let reply = tokio::time::timeout(Duration::from_secs(limit), process.session.run_with_started(
        &mut r.ledger, "@owner:test", &r.scope, call, call, input, BudgetMode::Postpaid, &queue, None,
    )).await.expect("turn finished in time").expect("turn completed");
    drop(queue);
    process.stop().await.unwrap();
    (reply, owner.await.unwrap())
}

async fn fake_ollama() -> (tokio::process::Child, String) {
    let mut child = tokio::process::Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/codex_model_fixture.py")).arg("shell")
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    (child, format!("http://127.0.0.1:{}", port.trim()))
}

fn spent(r: &Rig) -> u64 {
    r.ledger.account(&r.scope, Layer::Agent, Period::Lifetime, time::OffsetDateTime::now_utc().unix_timestamp()).unwrap().0
}

#[tokio::test]
#[ignore = "requires explicit installed Codex; localhost Ollama-shaped fixture"]
async fn ollama_tool_call_waits_for_owner_approval_and_is_accounted() {
    let (mut model, endpoint) = fake_ollama().await;
    let mut r = rig(&endpoint, "qwen3:4b");
    let (reply, asked) = turn(&mut r, "approved", "make hello.txt", true, FAKE_TIMEOUT_S).await;
    assert_eq!(reply.text, "SHELL_REPLY");
    assert_eq!(asked, 1, "the shell call went through the owner approval gate");
    assert_eq!(fs::read_to_string(r.profile.cwd.join("hello.txt")).unwrap().trim(), "hi");
    assert!(reply.usage.is_reported() && reply.usage.input > 0);
    assert!(spent(&r) > 0, "usage settled in the ledger");
    model.kill().await.unwrap();
    drop(r.tmp);
}

#[tokio::test]
#[ignore = "requires explicit installed Codex; localhost Ollama-shaped fixture"]
async fn ollama_denied_tool_call_does_not_run() {
    let (mut model, endpoint) = fake_ollama().await;
    let mut r = rig(&endpoint, "qwen3:4b");
    let (reply, asked) = turn(&mut r, "denied", "make hello.txt", false, FAKE_TIMEOUT_S).await;
    assert_eq!(asked, 1);
    assert_eq!(reply.text, "SHELL_REPLY");
    assert!(!r.profile.cwd.join("hello.txt").exists(), "denied command never ran");
    model.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "requires explicit installed Codex; localhost fixture"]
async fn ollama_settled_call_is_not_executed_again_after_restart() {
    let (mut model, endpoint) = fake_ollama().await;
    let mut r = rig(&endpoint, "qwen3:4b");
    turn(&mut r, "once", "make hello.txt", true, FAKE_TIMEOUT_S).await;
    fs::remove_file(r.profile.cwd.join("hello.txt")).unwrap();
    r.ledger = Ledger::open(r.tmp.path().canonicalize().unwrap().join("ledger"), "@owner:test").unwrap();
    let guardian = Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut process = runtime::spawn_for_scope_with_guardian(&r.profile, "ollama-test", &mut r.ledger, &r.scope, Some(guardian))
        .await.unwrap();
    process.session.initialize().await.unwrap();
    process.session.open_context(&mut r.ledger, &r.scope, &r.profile).await.unwrap();
    let (queue, _pending) = codex::approval_queue(1).unwrap();
    let again = process.session.run_with_started(&mut r.ledger, "@owner:test", &r.scope, "once", "once",
        "make hello.txt", BudgetMode::Postpaid, &queue, None).await;
    let refused = again.err().expect("the same call id is refused after restart");
    assert!(matches!(refused, codex::Error::Protocol("call already started; refusing provider replay")), "refused by the ledger, not another failure: {refused:?}");
    assert!(!r.profile.cwd.join("hello.txt").exists(), "no duplicate execution");
    process.stop().await.unwrap();
    model.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "real Ollama: set HAGENCY_OLLAMA_E2E_MODEL (e.g. qwen3:4b)"]
async fn live_ollama_model_runs_an_approved_shell_command() {
    // Empty configuration is a failure, never a vacuous pass.
    let model = std::env::var_os("HAGENCY_OLLAMA_E2E_MODEL").expect("set HAGENCY_OLLAMA_E2E_MODEL");
    let mut r = rig(codex::ollama::DEFAULT_ENDPOINT, model.to_str().unwrap());
    let started = std::time::Instant::now();
    let (reply, asked) = turn(&mut r, "live", "Use one shell command to create hello.txt containing hi, then reply DONE.",
        true, LIVE_TIMEOUT_S).await;
    eprintln!("live ollama: {:?}, asked={asked}, input={}, output={}, text={:?}",
        started.elapsed(), reply.usage.input, reply.usage.output, reply.text);
    assert!(asked >= 1);
    assert!(r.profile.cwd.join("hello.txt").exists());
    assert!(reply.usage.is_reported() && spent(&r) > 0);
}
