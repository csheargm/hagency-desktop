//! Fixed loopback API + real canonical ledger + explicit offline child process.
use super::*;
#[cfg(feature="shared-budget-execution")]
#[path="budget_attempt_tests.rs"]
mod budget_attempts;
#[cfg(feature="shared-context")]
#[path="context_tests.rs"]
mod contexts;
#[cfg(feature="shared-context")]
#[path="encrypted_execution_tests.rs"]
mod encrypted;
#[cfg(feature="shared-context")]
#[path="question_tests.rs"]
mod question_runs;
#[path = "claude_tests.rs"]
mod claude_runs;
#[cfg(feature = "shared-successors")]
#[path = "attempt_tests.rs"]
mod attempts;
#[cfg(feature = "shared-intake")]
#[path = "demand_tests.rs"]
mod demands;
#[cfg(feature = "shared-successors")]
#[path = "inventory_tests.rs"]
mod inventory;
#[cfg(feature = "shared-successors")]
#[path = "review_state_tests.rs"]
mod review_state;
use crate::console::native::{
    SharedExecutionProgress, SharedExecutionTransport, SharedWorkRecoveryProgress,
    managed_runtime::PreparedLaunch,
};
use hagency_runtime::owned::{
    MANAGED_DISABLED_FEATURES, SHARED_TEXT_INSTRUCTIONS, SHARED_TEXT_INSTRUCTIONS_FILE,
};
use hagency_store::owner_resources::shared_execution::*;
use sha2::{Digest, Sha256};
#[derive(Default)]
pub(crate) struct State {
    #[cfg(feature="shared-budget-execution")]
    budget_attempts: budget_attempts::State,
    #[cfg(feature="shared-context")]
    contexts: contexts::State,
    #[cfg(feature = "shared-intake")]
    demands: demands::State,
    #[cfg(feature = "shared-successors")]
    attempts: attempts::State,
    idle_work: bool,
    hold_idle_boundary: bool,
    hold_reply: bool,
    command: Option<SharedRuntimeCommand>,
    processes: BTreeMap<String, SharedProcessReceipt>,
    work: Option<SharedWorkItem>,
    receipts: BTreeMap<String, SharedWorkReceipt>,
    starts: usize,
    activity_epoch: u64,
    refuse_renewal: bool,
    delay_renewal_reply: bool,
    delay_start_reply: bool,
    delay_run_card_device: bool,
    run_card_device_requested: std::sync::atomic::AtomicBool,
    process_writes: usize,
    work_writes: usize,
    lose_start: bool,
    false_start: bool,
    lose_work_result: bool,
    lose_completed_work_result: bool,
    work_recover_calls: usize,
    forged_work: bool,
    lose_stop_before_write: bool,
    cleanup: bool,
    reply_pending: bool,
    stopped_before_reply: bool,
}
pub(crate) fn idle_boundary_delay(path: &str, s: &State) -> Option<Duration> {
    if s.delay_run_card_device && path.ends_with("/crypto/device") {
        s.run_card_device_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        return Some(Duration::from_secs(30));
    }
    if s.delay_renewal_reply && path.ends_with("/activity/renew") {
        return Some(Duration::from_secs(30));
    }
    // Central admission can consume the renewal margin while the provider
    // has not received turn/start yet.
    if s.delay_start_reply && path.ends_with("/work/start") {
        return Some(Duration::from_secs(6));
    }
    if !s.hold_idle_boundary || !path.ends_with("/shared-execution/work/poll") {
        return None;
    }
    let issued = s.command.as_ref()?.issued_at_ms;
    let elapsed = now().saturating_sub(issued);
    (28_000..31_500)
        .contains(&elapsed)
        .then(|| Duration::from_millis(31_500 - elapsed))
}
pub(crate) fn claude_enabled(s: &super::State) -> bool {
    s.shared.command.as_ref().is_some_and(|c| c.version == 4)
}
pub(crate) fn management(path: &str, body: &Value, s: &mut super::State) -> Option<Value> {
    if path.ends_with("/crypto/device") {
        // Plaintext execution fixtures have no Agent device grant. Run cards
        // are optional presentation: an unavailable grant must neither create
        // another model call nor break the original work/recovery path.
        assert_eq!(body["assignmentId"], s.shared.command.as_ref().unwrap().assignment_id);
        return None;
    }
    if path.ends_with("/crypto/send") {
        assert_eq!(body["workId"], s.shared.work.as_ref().unwrap().id);
        return Some(json!({"observedAtMs":now()}));
    }
    #[cfg(feature="shared-budget-execution")]
    if path.contains("/budget-attempts/") || path.contains("/budget-demands/") || path.ends_with("/budget-work/budget_demand_work/demand") || path.ends_with("/refuse-unissued") {return budget_attempts::management(path,body,s);}
    #[cfg(feature="shared-budget-execution")]
    let normalized=path.replace("/budget-work/","/work/").replace("/budget-activity/","/activity/");
    #[cfg(feature="shared-budget-execution")]
    let path=normalized.as_str();
    #[cfg(feature="shared-context")]
    if let Some(reply)=contexts::management(path,body,s){return reply;}
    #[cfg(feature = "shared-intake")]
    if let Some(reply) = demands::management(path, body, s) {
        return reply;
    }
    #[cfg(feature = "shared-successors")]
    if path.contains("/attempts/") {
        return attempts::management(path, body, s);
    }
    if path.ends_with("/demand") {
        return Some(json!({"demand":null,"observedAtMs":now()}));
    }
    let at = now();
    let c = s.shared.command.clone().unwrap();
    let h = digest(&c).unwrap();
    if path.ends_with("/runtime/poll") || path.ends_with("/runtime/poll-v4") || path.contains("/runtime/") && path.ends_with("/recover") {
        let history = path.ends_with("/recover");
        if history {
            assert_eq!(body["commandDigest"], h);
        }
        let selected = body["observationId"].as_str();
        let recorded = s
            .shared
            .processes
            .values()
            .filter(|v| selected.is_none_or(|id| id == v.observation_id))
            .max_by_key(|v| v.ledger_revision)
            .cloned();
        let executable = if history {
            s.historical_executable
        } else {
            !s.shared.cleanup && recorded.is_none()
        };
        return Some(
            json!({"delivery":{"command":c,"commandDigest":if s.tamper_digest{"f".repeat(64)}else{h},"token":"e".repeat(64),"leaseUntilMs":if s.expired_ticket{at-1}else{at+20000},"executionAllowed":executable,"cleanupAllowed":!history && s.shared.cleanup,"recorded":recorded},"observedAtMs":at}),
        );
    }
    if path.contains("/runtime/") && path.ends_with("/result") {
        let r: SharedProcessReceipt = serde_json::from_value(body["receipt"].clone()).unwrap();
        r.validate_for(&c).unwrap();
        assert_eq!(body["commandDigest"], h);
        if r.state == "stopped" && std::mem::take(&mut s.shared.lose_stop_before_write) {
            return None;
        }
        if r.state == "stopped" && s.shared.reply_pending {
            s.shared.stopped_before_reply = true;
        }
        if let Some(old) = s.shared.processes.get(&r.observation_id) {
            assert_eq!(digest(old).unwrap(), digest(&r).unwrap());
        } else {
            s.shared.process_writes += 1;
            s.shared
                .processes
                .insert(r.observation_id.clone(), r.clone());
        }
        let active = if r.state == "qualified" {
            s.shared.activity_epoch = 1;
            Some(
                json!({"authority":{"runtimeCommandId":c.command_id,"epoch":1,"token":"c".repeat(64)},"commandDigest":h,"processDigest":r.process_digest,"expiresAtMs":at+25000}),
            )
        } else {
            None
        };
        return Some(
            json!({"observationId":r.observation_id,"receiptDigest":digest(&r).unwrap(),"activity":active,"state":if r.state=="not_started" {"stopped"} else {&r.state},"observedAtMs":at}),
        );
    }
    #[cfg(feature = "shared-successors")]
    let (h, runtime_id, process_digest) = if let Some(attempt) = &s.shared.attempts.command {
        (
            digest(attempt).unwrap(),
            attempt.command_id.clone(),
            s.shared
                .attempts
                .processes
                .values()
                .find(|r| r.state == "qualified")
                .and_then(|r| r.process_digest.clone()),
        )
    } else {
        (
            h,
            c.command_id.clone(),
            s.shared
                .processes
                .values()
                .find(|r| r.state == "qualified")
                .and_then(|r| r.process_digest.clone()),
        )
    };
    #[cfg(not(feature = "shared-successors"))]
    let (runtime_id, process_digest) = (
        c.command_id.clone(),
        s.shared
            .processes
            .values()
            .find(|r| r.state == "qualified")
            .and_then(|r| r.process_digest.clone()),
    );
    #[cfg(feature="shared-budget-execution")]
    let (h,runtime_id,process_digest)=if let Some(c)=&s.shared.budget_attempts.command {
        (digest(c).unwrap(),c.command_id.clone(),s.shared.budget_attempts.processes.values().find(|r|r.state=="qualified").and_then(|r|r.process_digest.clone()))
    }else{(h,runtime_id,process_digest)};
    if path.ends_with("/activity/renew") {
        if s.shared.refuse_renewal { return Some(json!({"fixtureStatus":503,"error":"fixture_unavailable"})); }
        assert_eq!(body["authority"]["epoch"], s.shared.activity_epoch);
        s.shared.activity_epoch += 1;
        return Some(
            json!({"activity":{"authority":{"runtimeCommandId":runtime_id,"epoch":s.shared.activity_epoch,"token":"c".repeat(64)},"commandDigest":h,"processDigest":process_digest,"expiresAtMs":at+25000},"observedAtMs":at}),
        );
    }
    if path.ends_with("/work/poll") && s.shared.idle_work {
        return Some(json!({"work":null,"observedAtMs":at}));
    }
    let w = s.shared.work.clone().unwrap();
    let wh = digest(&w).unwrap();
    if path.ends_with("/work/poll") {
        let mut w = serde_json::to_value(w).unwrap();
        if s.shared.forged_work {
            w["assignmentId"] = json!("foreign_assignment");
        }
        return Some(json!({"work":w,"observedAtMs":at}));
    }
    if path.ends_with("/work/ack") {
        assert_eq!(body["workDigest"], wh);
        return Some(json!({"acknowledged":true,"observedAtMs":at}));
    }
    if path.ends_with("/work/start") {
        assert_eq!(body["workDigest"], wh);
        s.shared.starts += 1;
        if std::mem::take(&mut s.shared.lose_start) {
            return None;
        }
        return Some(
            json!({"work":w,"workDigest":wh,"activityEpoch":s.shared.activity_epoch,"newlyStarted":!s.shared.false_start && s.shared.starts==1,"observedAtMs":at}),
        );
    }
    if path.contains("/work/") && path.ends_with("/recover") {
        s.shared.work_recover_calls += 1;
        assert_eq!(body["runtimeCommandDigest"], h);
        assert_eq!(body["workDigest"], wh);
        let id = body["observationId"].as_str();
        let old = s
            .shared
            .receipts
            .values()
            .filter(|r| r.work_id == w.id && id.is_none_or(|v| v == r.observation_id))
            .max_by_key(|r| r.ledger_revision)
            .cloned();
        if old.is_some() && !s.shared.hold_reply {
            s.shared.reply_pending = false;
        }
        return Some(
            json!({"ticket":{"workId":w.id,"workDigest":wh,"token":"e".repeat(64),"leaseUntilMs":at+20000,"state":if old.is_some(){"completed"}else{"started"},"replyState":if old.is_some(){Some(if s.shared.hold_reply {"pending"} else {"sent"})}else{None},"recorded":old},"observedAtMs":at}),
        );
    }
    if path.contains("/work/") && path.ends_with("/result") {
        #[cfg(feature = "shared-successors")]
        if s.shared.attempts.fail_work_reports {
            return None;
        }
        let r: SharedWorkReceipt = serde_json::from_value(body["receipt"].clone()).unwrap();
        r.validate_for(&w, &r.process_digest).unwrap();
        assert_eq!(body["workDigest"], wh);
        if let Some(old) = s.shared.receipts.get(&r.observation_id) {
            assert_eq!(digest(old).unwrap(), digest(&r).unwrap());
        } else {
            s.shared.work_writes += 1;
            s.shared
                .receipts
                .insert(r.observation_id.clone(), r.clone());
        }
        s.shared.reply_pending = true;
        if std::mem::take(&mut s.shared.lose_work_result) || r.state=="completed" && std::mem::take(&mut s.shared.lose_completed_work_result) {
            return None;
        }
        return Some(
            json!({"observationId":r.observation_id,"receiptDigest":digest(&r).unwrap(),"state":r.state,"replyState":"pending","observedAtMs":at}),
        );
    }
    panic!("unexpected fixed shared test route")
}
async fn fixture() -> (Fixture, SharedExecutionTransport, SharedRuntimeCommand) {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("hagency::console::native::shared_execution=debug,hagency_runtime::owned=info")
        .with_test_writer()
        .try_init();
    let (f, t, c) =
        prepared_with(Fixture::with_provider("openai.chatgpt", "fixture_account").await).await;
    runtime_fixture(f, t, c).await
}

#[tokio::test]
async fn native_shared_recurring_expired_initial_command_uploads_exact_nonissuance_proof() {
    use hagency_core::recurring_quota::{Cadence, Calendar, RenewalMode};
    let f = Fixture::with_provider_budget_calendar("openai.chatgpt", "fixture_account", 1000, 700, 60000,
        Some(Calendar {cadence:Cadence::Daily,time_zone:"America/Los_Angeles".into()})).await;
    let g = f.state.lock().unwrap().grant.clone().unwrap();
    let model = hagency_core::qualification::ModelProfile {framework:"codex".into(), model:"gpt-5.6-sol".into(),provider:None,reasoning:Some("medium".into())};
    let role = hagency_core::qualification::roles().find(|r|hagency_core::qualification::qualifies(&model,r,None)).unwrap();
    let definition = ManagedAgentDefinition {name:"Recurring fixture".into(),capability:role.into(),description:"Offline proof".into(),tool_mode:"none".into()};
    let c = ManagedAgentReserveCommand {
        version:2,renewal_mode:Some(RenewalMode::Automatic),allocation_generation:None,pool_grant:Some(g.clone()),
        capability:"hagency.pool_agents.v1".into(),operation:"agent.reserve".into(),
        command_id:"recurring_reserve".into(),request_id:"recurring_request".into(),request_revision:2,
        proposed_agent_id:"recurring_agent".into(),manager_mxid:"@manager:test".into(),
        project_id:String::new(),project_revision:0,setup_id:String::new(),setup_binding_revision:0,domain_project_id:String::new(),
        room_id:"!discussion:test".into(),authority_revision:g.grant_revision,decision_actor_mxid:g.owner_mxid.clone(),decision_digest:"d".repeat(64),
        allocation_id:"recurring_request".into(),allocation_receipt_digest:String::new(),grant_receipt_digest:digest(&g).unwrap(),
        enrollment_id:g.enrollment_id.clone(),executor_device_id:g.executor_device_id.clone(),executor_generation:g.executor_generation,
        resource_id:g.resource_id.clone(),resource_revision:g.resource_revision.clone(),definition_digest:digest(&definition).unwrap(),definition,
        requested_tokens:100,period_key:g.period_key.clone(),issued_at_ms:now()-100,expires_at_ms:now()+60000,
    };
    c.validate("@owner:test").unwrap();
    f.state.lock().unwrap().agent=Some(serde_json::to_value(&c).unwrap());
    let transport = ManagedAgentTransport::new(f.owner.management(package(1)).await.unwrap());
    let (f,t,mut command)=runtime_fixture(f,transport,c).await;
    command.issued_at_ms=now()-50;
    command.expires_at_ms=now()-1;
    {
        let mut s=f.state.lock().unwrap();
        s.shared.command=Some(command.clone());
        s.shared.cleanup=true;
    }
    for _ in 0..2 {
        assert!(matches!(t.poll(&f.registry,None).await.unwrap(),SharedExecutionProgress::Recorded{process_state:Some(ref state),..} if state=="not_started"));
    }
    assert_eq!(rows(&f,"owner_shared_runtime_lifetimes"),0);
    assert_eq!(rows(&f,"owner_shared_unstarted_runtimes"),1);
    let receipt=f.state.lock().unwrap().shared.processes.values().next().unwrap().clone();
    assert_eq!(f.state.lock().unwrap().shared.process_writes,1);
    assert!(receipt.process_digest.is_none());
    assert!(matches!(t.recover(&f.registry,command.command_id.clone(),digest(&command).unwrap(),Some(receipt.observation_id)).await.unwrap(),SharedExecutionProgress::Recorded{process_state:Some(ref state),..} if state=="not_started"));
    assert_eq!(f.state.lock().unwrap().shared.starts,0);
    f.stop().await;
}

async fn runtime_fixture(
    f: Fixture,
    t: ManagedAgentTransport,
    c: ManagedAgentReserveCommand,
) -> (Fixture, SharedExecutionTransport, SharedRuntimeCommand) {
    t.poll(&f.registry, None).await.unwrap();
    let receipt: ManagedAgentReservationReceipt =
        serde_json::from_value(f.state.lock().unwrap().agent_receipts[&c.command_id].clone())
            .unwrap();
    let command = SharedRuntimeCommand {
        version: 1,
        capability: SHARED_EXECUTION_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "shared_command".into(),
        assignment_id: "shared_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: receipt.agent_reservation_id.unwrap(),
        reserve_command_digest: digest(&c).unwrap(),
        reserve_receipt_digest: digest(&f.state.lock().unwrap().agent_receipts[&c.command_id])
            .unwrap(),
        reserve_command: c,
        policy: SHARED_TEXT_POLICY.into(),
        issued_at_ms: now(),
        expires_at_ms: now() + 30000,
    };
    let work = SharedWorkItem {
        room_binding: None, encrypted: false,
        id: "work_fixture".into(),
        assignment_id: command.assignment_id.clone(),
        runtime_command_digest: digest(&command).unwrap(),
        event_id: "$input:test".into(),
        room_id: command.reserve_command.room_id.clone(),
        requester_mxid: command.reserve_command.manager_mxid.clone(),
        thread_root: "$input:test".into(),
        body: "Offline synthetic prompt".into(),
        body_digest: format!("{:x}", Sha256::digest(b"Offline synthetic prompt")),
        execution_id: "execution_fixture".into(),
        created_at_ms: now(),
    };
    {
        let mut s = f.state.lock().unwrap();
        s.shared.command = Some(command.clone());
        s.shared.work = Some(work);
    }
    let t = SharedExecutionTransport::new(f.owner.management(package(1)).await.unwrap());
    (f, t, command)
}
fn launch(f: &Fixture, mode: &str) -> PreparedLaunch {
    let binary = std::path::PathBuf::from(
        std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("explicit offline probe"),
    );
    let root = f._root.path().join("shared-probe");
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let instructions = root.join(SHARED_TEXT_INSTRUCTIONS_FILE);
    std::fs::write(&instructions, SHARED_TEXT_INSTRUCTIONS).unwrap();
    let features: serde_json::Map<String, Value> = MANAGED_DISABLED_FEATURES
        .iter()
        .map(|k| ((*k).into(), false.into()))
        .collect();
    let config = json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{},"model_instructions_file":instructions,"instructions":"","notify":[],"web_search":"disabled","project_doc_max_bytes":0,"skills":{"include_instructions":false,"bundled":{"enabled":false}},"developer_instructions":"","include_apps_instructions":false,"include_environment_context":false,"mcp_servers":{},"features":features});
    std::fs::write(
        root.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    PreparedLaunch::text_fixture(binary, root, mode)
}
fn rows(f: &Fixture, table: &str) -> u64 {
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}
async fn stopped(f: &Fixture) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if f.state
                .lock()
                .unwrap()
                .shared
                .processes
                .values()
                .any(|r| r.state == "stopped")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("retained process stops with exact receipt");
    f.owner.0.shared_execution.drain().await.unwrap();
}
#[tokio::test]
async fn native_shared_execution_history_tamper_and_expiry_never_issue() {
    let (f, t, c) = fixture().await;
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap(), None)
            .await
            .unwrap(),
        SharedExecutionProgress::HistoricalEvidenceMissing { .. }
    ));
    for case in ["history", "digest", "expiry"] {
        {
            let mut s = f.state.lock().unwrap();
            s.historical_executable = case == "history";
            s.tamper_digest = case == "digest";
            s.expired_ticket = case == "expiry";
        }
        assert!(
            t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap(), None)
                .await
                .is_err()
        );
    }
    assert_eq!(rows(&f, "owner_shared_runtime_lifetimes"), 0);
    assert_eq!(f.state.lock().unwrap().shared.process_writes, 0);
    f.stop().await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_one_turn_usage_and_reply_recovery_no_rerun() {
    let (f, t, c) = fixture().await;
    f.state.lock().unwrap().shared.lose_work_result = true;
    assert!(matches!(
        t.poll_fixture(&f.registry, launch(&f, "text-usage"))
            .await
            .unwrap(),
        SharedExecutionProgress::Running { .. }
    ));
    stopped(&f).await;
    let saved = f
        .state
        .lock()
        .unwrap()
        .shared
        .receipts
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(saved.known_consumed_tokens, 200);
    assert_eq!(saved.retained_tokens, Some(200));
    assert!(saved.usage_complete);
    let progress = t
        .recover_work(
            &f.registry,
            c.command_id.clone(),
            digest(&c).unwrap(),
            saved.work_id.clone(),
            Some(saved.observation_id.clone()),
        )
        .await
        .unwrap();
    assert!(
        matches!(progress,SharedWorkRecoveryProgress::Recorded{reply_state:Some(s),..} if s=="sent")
    );
    let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|l| *l == "turn/start").count(), 1);
    assert_eq!(methods.lines().filter(|l| *l == "initialize").count(), 1);
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    assert_eq!(f.state.lock().unwrap().shared.work_writes, 1);
    assert!(f.state.lock().unwrap().shared.work_recover_calls >= 2);
    assert!(
        !f.state.lock().unwrap().shared.stopped_before_reply,
        "the same process must remain live through pending central reply"
    );
    assert_eq!(rows(&f, "owner_shared_work_inbox"), 1);
    f.stop().await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_run_card_device_delay_does_not_block_native_turn() {
    let (f, t, _) = fixture().await;
    f.state.lock().unwrap().shared.delay_run_card_device = true;
    tokio::time::timeout(Duration::from_secs(8), async {
        t.poll_fixture(&f.registry, launch(&f, "text-renew")).await.unwrap();
        method_seen(&f, "turn/start").await;
        loop {
            if f.state.lock().unwrap().shared.run_card_device_requested.load(std::sync::atomic::Ordering::SeqCst) { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        std::fs::write(f._root.path().join("shared-probe/release-text"), b"release").unwrap();
        stopped(&f).await;
    }).await.expect("native work must start and finish while the optional card device is still unavailable");
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    let state = f.state.lock().unwrap();
    assert_eq!(state.shared.receipts.len(), 1);
    assert!(state.shared.receipts.values().all(|receipt| receipt.usage_complete));
    drop(state);
    f.stop().await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_uncertain_or_replayed_start_never_runs_model() {
    for lost in [true, false] {
        let (f, t, c) = fixture().await;
        {
            let mut s = f.state.lock().unwrap();
            s.shared.lose_start = lost;
            s.shared.false_start = !lost;
        }
        t.poll_fixture(&f.registry, launch(&f, "text-usage"))
            .await
            .unwrap();
        stopped(&f).await;
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap(), None)
            .await
            .unwrap();
        let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
        // Native thread creation prepares context before admission. Only
        // turn/start sends model input and must never happen on uncertainty.
        assert!(!methods.contains("turn/start"));
        assert_eq!(f.state.lock().unwrap().shared.starts, 1);
        assert_eq!(rows(&f, "owner_managed_calls"), 1);
        assert_eq!(f.state.lock().unwrap().shared.work_writes, 0);
        f.stop().await;
    }
}

async fn method_seen(f: &Fixture, method: &str) {
    let path = f._root.path().join("shared-probe/methods");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if std::fs::read_to_string(&path).is_ok_and(|s| s.lines().any(|v| v == method)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("actual child reached barrier");
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_waiter_cancellation_retains_original_task() {
    let (f, t, _) = fixture().await;
    let prepared = launch(&f, "text-held-qualify");
    let registry = f.registry.clone();
    let retained = Arc::new(t);
    let waiting_transport = retained.clone();
    let waiting =
        tokio::spawn(async move { waiting_transport.poll_fixture(&registry, prepared).await });
    method_seen(&f, "account/rateLimits/read").await;
    waiting.abort();
    let _ = waiting.await;
    std::fs::write(
        f._root.path().join("shared-probe/release-qualification"),
        b"release",
    )
    .unwrap();
    stopped(&f).await;
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    assert_eq!(f.state.lock().unwrap().shared.work_writes, 1);
    assert_eq!(rows(&f, "owner_shared_runtime_lifetimes"), 1);
    f.stop().await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_logout_during_turn_drains_and_retains_unknown_evidence() {
    let (f, t, _) = fixture().await;
    t.poll_fixture(&f.registry, launch(&f, "text-block"))
        .await
        .unwrap();
    method_seen(&f, "turn/start").await;
    f.current.store(false, Ordering::Release);
    f.owner.retire();
    f.owner.0.shared_execution.drain().await.unwrap();
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let saved:String=db.query_row("SELECT payload FROM owner_shared_work_observations ORDER BY ledger_revision DESC LIMIT 1",[],|r|r.get(0)).unwrap();
    let receipt: SharedWorkReceipt = serde_json::from_str(&saved).unwrap();
    assert_eq!(receipt.state, "unknown");
    assert!(!receipt.usage_complete);
    // Unreported usage counts as zero; the call never holds its reservation.
    assert_eq!(receipt.retained_tokens, Some(0));
    assert_eq!(db.query_row("SELECT COUNT(*) FROM owner_shared_process_observations WHERE json_extract(payload,'$.state')='stopped'",[],|r|r.get::<_,u64>(0)).unwrap(),1);
    assert_eq!(f.state.lock().unwrap().shared.work_writes, 0);
    drop(db);
    f.stop().await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_foreign_assignment_is_denied_before_call_hold() {
    let (f, t, _) = fixture().await;
    f.state.lock().unwrap().shared.forged_work = true;
    t.poll_fixture(&f.registry, launch(&f, "text-usage"))
        .await
        .unwrap();
    stopped(&f).await;
    assert_eq!(rows(&f, "owner_managed_calls"), 0);
    assert_eq!(f.state.lock().unwrap().shared.starts, 0);
    let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert!(!methods.contains("turn/start"));
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_authorized_group_requester_keeps_its_identity() {
    let (f, t, _) = fixture().await;
    f.state.lock().unwrap().shared.work.as_mut().unwrap().requester_mxid = "@member:test".into();
    t.poll_fixture(&f.registry, launch(&f, "text-usage")).await.unwrap();
    stopped(&f).await;
    {
        let state = f.state.lock().unwrap();
        assert_eq!(state.shared.starts, 1);
        assert_eq!(state.shared.work.as_ref().unwrap().requester_mxid, "@member:test");
        assert!(state.shared.receipts.values().any(|receipt| receipt.state == "completed"));
    }
    let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|line| *line == "turn/start").count(), 1);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_newer_local_stop_replaces_old_central_projection_without_spawn() {
    let (f, t, c) = fixture().await;
    f.state.lock().unwrap().shared.lose_stop_before_write = true;
    t.poll_fixture(&f.registry, launch(&f, "text-usage"))
        .await
        .unwrap();
    let completion = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if !f.state.lock().unwrap().shared.lose_stop_before_write {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if completion.is_err() {
        let s = f.state.lock().unwrap();
        let db =
            rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
        let process:Vec<String>=db.prepare("SELECT json_extract(payload,'$.state') FROM owner_shared_process_observations ORDER BY ledger_revision").unwrap().query_map([],|r|r.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
        panic!(
            "stop attempt missing: local={process:?}, central={:?}, starts={}, work={}, recover={}, pending={}",
            s.shared
                .processes
                .values()
                .map(|r| &r.state)
                .collect::<Vec<_>>(),
            s.shared.starts,
            s.shared.work_writes,
            s.shared.work_recover_calls,
            s.shared.reply_pending
        );
    }
    f.owner.0.shared_execution.drain().await.unwrap();
    assert_eq!(f.state.lock().unwrap().shared.processes.len(), 1);
    assert_eq!(
        f.state.lock().unwrap().shared.work_writes,
        1,
        "original turn must complete before restart fixture"
    );
    assert!(rows(&f, "owner_shared_work_observations") > 0);
    // Simulate a central result upload lost before recording; the private
    // canonical observation remains. Recovery must discover it without a work ID.
    f.state.lock().unwrap().shared.receipts.clear();
    assert_eq!(
        f.state
            .lock()
            .unwrap()
            .shared
            .processes
            .values()
            .next()
            .unwrap()
            .state,
        "qualified"
    );
    let reopened = SharedExecutionTransport::new(f.owner.management(package(2)).await.unwrap());
    reopened
        .recover(&f.registry, c.command_id.clone(), digest(&c).unwrap(), None)
        .await
        .unwrap();
    assert_eq!(f.state.lock().unwrap().shared.processes.len(), 2);
    assert_eq!(f.state.lock().unwrap().shared.receipts.len(), 1);
    assert_eq!(
        f.state
            .lock()
            .unwrap()
            .shared
            .receipts
            .values()
            .next()
            .unwrap()
            .known_consumed_tokens,
        200
    );
    assert!(
        f.state
            .lock()
            .unwrap()
            .shared
            .processes
            .values()
            .any(|r| r.state == "stopped")
    );
    let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m| *m == "initialize").count(), 1);
    assert_eq!(methods.lines().filter(|m| *m == "turn/start").count(), 1);
    f.stop().await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_cleanup_poll_stops_original_running_process() {
    let (f, t, _) = fixture().await;
    t.poll_fixture(&f.registry, launch(&f, "text-block"))
        .await
        .unwrap();
    method_seen(&f, "turn/start").await;
    f.state.lock().unwrap().shared.cleanup = true;
    assert!(matches!(
        t.poll(&f.registry, None).await.unwrap(),
        SharedExecutionProgress::Stopping { .. }
    ));
    stopped(&f).await;
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_renews_same_process_while_original_event_read_is_pending() {
    let (f, t, _) = fixture().await;
    t.poll_fixture(&f.registry, launch(&f, "text-renew"))
        .await
        .unwrap();
    method_seen(&f, "turn/start").await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f.state.lock().unwrap().shared.activity_epoch >= 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("same process renewed while original read pending");
    assert!(
        !f.state
            .lock()
            .unwrap()
            .shared
            .processes
            .values()
            .any(|r| r.state == "stopped")
    );
    std::fs::write(f._root.path().join("shared-probe/release-text"), b"release").unwrap();
    stopped(&f).await;
    {
        let s = f.state.lock().unwrap();
        assert_eq!(s.shared.starts, 1);
        assert_eq!(s.shared.receipts.len(), 1);
        assert_eq!(
            s.shared
                .receipts
                .values()
                .next()
                .unwrap()
                .known_consumed_tokens,
            200
        );
        assert!(!s.shared.stopped_before_reply);
    }
    let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m| *m == "initialize").count(), 1);
    assert_eq!(methods.lines().filter(|m| *m == "turn/start").count(), 1);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_interrupted_call_without_final_usage_preserves_unknown_and_session() {
    let (f, t, _) = fixture().await;
    t.poll_fixture(&f.registry, launch(&f, "text-block"))
        .await
        .unwrap();
    method_seen(&f, "turn/start").await;
    let resume = f
        .owner
        .0
        .host
        .state_directory()
        .join("server-login-sdk-resume.json");
    let original = std::fs::read(&resume).unwrap();
    f.owner.suspend().await.unwrap();
    assert_eq!(std::fs::read(&resume).unwrap(), original);
    assert!(f.owner.identity().await.is_err());
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let saved:String=db.query_row("SELECT payload FROM owner_shared_work_observations ORDER BY ledger_revision DESC LIMIT 1",[],|r|r.get(0)).unwrap();
    let receipt: SharedWorkReceipt = serde_json::from_str(&saved).unwrap();
    assert_eq!(receipt.state, "unknown");
    assert!(!receipt.usage_complete);
    // Unreported usage counts as zero; the call never holds its reservation.
    assert_eq!(receipt.retained_tokens, Some(0));
    assert_eq!(db.query_row("SELECT COUNT(*) FROM owner_shared_process_observations WHERE json_extract(payload,'$.state')='stopped'",[],|r|r.get::<_,u64>(0)).unwrap(),1);
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|v| *v == "initialize").count(), 1);
    assert_eq!(methods.lines().filter(|v| *v == "turn/start").count(), 1);
    drop(db);
    f.stop().await;
}

async fn refuse_renewal_and_wait_for_usage(f: &Fixture) {
    f.state.lock().unwrap().shared.refuse_renewal = true;
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if f.state.lock().unwrap().shared.receipts.values().any(|r|r.usage_complete && r.known_consumed_tokens==27) { break; }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("renewal refusal collected final usage without owner cancellation");
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_interrupted_call_collects_final_usage_before_stopping_original_and_successor() {
    for (successor, refused_renewal) in [(false,false),(true,false),(false,true),(true,true)] {
        let (f, t, c) = if successor { attempts::predecessor().await } else { fixture().await };
        if successor {
            attempts::prepare_lost_start(&f, &t, &c).await;
            t.poll_successor_fixture(&f.registry, launch(&f, "text-interrupt-usage")).await.unwrap();
        } else {
            t.poll_fixture(&f.registry, launch(&f, "text-interrupt-usage")).await.unwrap();
        }
        method_seen(&f, "turn/start").await;
        // A denied renewal must also stop cooperatively within the old lease.
        // Provider receipts, never the stop alone, release credit.
        if refused_renewal { refuse_renewal_and_wait_for_usage(&f).await; }
        f.owner.suspend().await.unwrap();
        let db=rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
        let row:(String,u64,u64,bool)=db.query_row("SELECT state,reserved_tokens,known_consumed_tokens,usage_complete FROM owner_managed_calls",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).unwrap();
        assert_eq!(row,("settled".into(),0,27,true));
        let raw:String=db.query_row("SELECT payload FROM owner_shared_work_observations ORDER BY ledger_revision DESC LIMIT 1",[],|r|r.get(0)).unwrap();
        let receipt:SharedWorkReceipt=serde_json::from_str(&raw).unwrap();
        assert_eq!(receipt.state,"failed");
        assert!(receipt.usage_complete);
        assert_eq!(receipt.retained_tokens,Some(27));
        assert_eq!(rows(&f,"owner_managed_budget_deltas"),0);
        let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
        assert_eq!(methods.lines().filter(|m|*m=="turn/start").count(),1);
        assert_eq!(methods.lines().filter(|m|*m=="turn/interrupt").count(),1);
        drop(db);
        f.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_interrupted_call_start_ack_survives_activity_renewal_without_second_call() {
    let (f,t,_)=fixture().await;
    t.poll_fixture(&f.registry,launch(&f,"text-start-renew")).await.unwrap();
    stopped(&f).await;
    let state=f.state.lock().unwrap();
    assert!(state.shared.activity_epoch>=2);
    assert_eq!(state.shared.starts,1);
    assert!(state.shared.receipts.values().any(|r|r.state=="completed" && r.usage_complete));
    drop(state);
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m|*m=="turn/start").count(),1);
    assert_eq!(rows(&f,"owner_managed_budget_deltas"),0);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_interrupted_call_slow_admission_does_not_strand_unused_budget() {
    let (f,t,_)=fixture().await;
    f.state.lock().unwrap().shared.delay_start_reply=true;
    t.poll_fixture(&f.registry,launch(&f,"text-small-usage")).await.unwrap();
    stopped(&f).await;
    let state=f.state.lock().unwrap();
    assert_eq!(state.shared.starts,1);
    assert!(state.shared.receipts.values().any(|r|r.state=="completed" && r.usage_complete));
    drop(state);
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m|*m=="turn/start").count(),1);
    let db=rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    // The fixture reports 30 input (10 cached) + 7 output. The existing
    // shared-budget unit excludes cache reads, so the measured charge is 27.
    let complete:bool=db.query_row("SELECT usage_complete AND reserved_tokens = 0 AND known_consumed_tokens = 27 FROM owner_managed_calls",[],|r|r.get(0)).unwrap();
    assert!(complete,"the real provider receipt is charged without an upfront estimate");
    drop(db);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_interrupted_call_failed_renewal_during_start_keeps_ack_and_usage() {
    let (f,t,_)=fixture().await;
    f.state.lock().unwrap().shared.refuse_renewal=true;
    t.poll_fixture(&f.registry,launch(&f,"text-start-interrupt-usage")).await.unwrap();
    stopped(&f).await;
    let state=f.state.lock().unwrap();
    assert_eq!(state.shared.starts,1);
    assert!(state.shared.receipts.values().any(|r|r.state=="failed" && r.usage_complete && r.known_consumed_tokens==27 && r.retained_tokens==Some(27)));
    drop(state);
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m|*m=="turn/start").count(),1);
    assert_eq!(methods.lines().filter(|m|*m=="turn/interrupt").count(),1);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_interrupted_call_unresponsive_renewal_leaves_time_for_final_usage() {
    let (f,t,_)=fixture().await;
    f.state.lock().unwrap().shared.delay_renewal_reply=true;
    t.poll_fixture(&f.registry,launch(&f,"text-interrupt-usage")).await.unwrap();
    method_seen(&f,"turn/start").await;
    tokio::time::timeout(Duration::from_secs(35),async {
        loop {
            let db=rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
            let (state,used,complete):(String,u64,bool)=db.query_row("SELECT state,known_consumed_tokens,usage_complete FROM owner_managed_calls LIMIT 1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
            if state!="issued" {
                assert_eq!((state,used,complete),("settled".into(),27,true));
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await.expect("a stalled renewal must not consume the cleanup deadline");
    f.owner.suspend().await.unwrap();
    let methods=std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|m|*m=="turn/start").count(),1);
    assert_eq!(methods.lines().filter(|m|*m=="turn/interrupt").count(),1);
    f.stop().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe; crosses original 30s authority lease"]
async fn native_shared_execution_idle_renewal_crosses_initial_authority_then_runs_original_work() {
    let (f, t, c) = fixture().await;
    {
        let mut state = f.state.lock().unwrap();
        state.shared.idle_work = true;
        state.shared.hold_idle_boundary = true;
        state.shared.hold_reply = true;
    }
    t.poll_fixture(&f.registry, launch(&f, "text-usage"))
        .await
        .unwrap();
    let remaining = c.expires_at_ms.saturating_sub(now());
    tokio::time::sleep(Duration::from_millis(remaining + 2500)).await;
    {
        let mut state = f.state.lock().unwrap();
        assert!(
            state.shared.activity_epoch >= 3,
            "idle process must renew its same activity"
        );
        assert!(
            state
                .shared
                .processes
                .values()
                .all(|r| r.state != "stopped"),
            "renewed idle process stopped at initial command/authority expiry"
        );
        assert_eq!(state.shared.starts, 0);
        state.shared.idle_work = false;
    }
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f.state
                .lock()
                .unwrap()
                .shared
                .receipts
                .values()
                .any(|r| r.usage_complete)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("completed usage reaches central before reply delivery");
    let pid: u32 = std::fs::read_to_string(f._root.path().join("shared-probe/process-id"))
        .unwrap()
        .parse()
        .unwrap();
    let alive = || {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(
        alive(),
        "actual original child remains alive with completed reply pending"
    );
    let epoch = f.state.lock().unwrap().shared.activity_epoch;
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            if f.state.lock().unwrap().shared.activity_epoch > epoch {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("same actual process renews after turn completion while reply is pending");
    assert!(alive());
    f.state.lock().unwrap().shared.hold_reply = false;
    stopped(&f).await;
    let methods = std::fs::read_to_string(f._root.path().join("shared-probe/methods")).unwrap();
    assert_eq!(methods.lines().filter(|v| *v == "initialize").count(), 1);
    assert_eq!(methods.lines().filter(|v| *v == "turn/start").count(), 1);
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    assert_eq!(rows(&f, "owner_shared_runtime_lifetimes"), 1);
    assert!(!alive(), "original child is reaped after reply delivery");
    assert!(
        f.state
            .lock()
            .unwrap()
            .shared
            .receipts
            .values()
            .any(|r| r.state == "completed" && r.known_consumed_tokens == 200 && r.usage_complete),
        "the delayed first work retains complete actual usage and output"
    );
    f.stop().await;
}

#[tokio::test]
#[ignore = "crosses the original finite native management scope"]
async fn native_shared_execution_scope_overlap_keeps_old_deadline_and_package_retirement() {
    let (f, _t, _) = fixture().await;
    let manager = f.owner.management(package(83)).await.unwrap();
    let (context, original) = manager.authorized().await.unwrap();
    let original_deadline = original.valid_until();
    let wake = original_deadline - Duration::from_secs(8);
    tokio::time::sleep_until(wake.into()).await;
    let (current, renewed) = manager.authorized().await.unwrap();
    assert_eq!(context, current);
    assert!(
        original.is_live(),
        "refresh must not invalidate an in-flight proof"
    );
    assert_eq!(
        original.valid_until(),
        original_deadline,
        "old authority is never extended"
    );
    assert!(renewed.valid_until() > original_deadline + Duration::from_secs(10));
    original
        .authorize(&context, Permission::ExecuteManagedAgents)
        .unwrap();
    renewed
        .authorize(&context, Permission::ExecuteManagedAgents)
        .unwrap();
    manager.retire();
    assert!(
        !original.is_live() && !renewed.is_live(),
        "package retirement fences every overlapping proof"
    );
    assert!(manager.authorized().await.is_err());
    f.stop().await;
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_execution_retirement_stops_completed_process_with_reply_pending() {
    let (f, t, _) = fixture().await;
    f.state.lock().unwrap().shared.hold_reply = true;
    t.poll_fixture(&f.registry, launch(&f, "text-usage"))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f.state
                .lock()
                .unwrap()
                .shared
                .receipts
                .values()
                .any(|r| r.usage_complete)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("original terminal receipt saved before retirement");
    let pid: u32 = std::fs::read_to_string(f._root.path().join("shared-probe/process-id"))
        .unwrap()
        .parse()
        .unwrap();
    let alive = || {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap()
            .status
            .success()
    };
    assert!(alive());
    f.current.store(false, Ordering::Release);
    f.owner.retire();
    f.owner.0.shared_execution.drain().await.unwrap();
    assert!(
        !alive(),
        "retirement reaps the same completed process even before reply delivery"
    );
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    let saved: String = db.query_row("SELECT payload FROM owner_shared_work_observations ORDER BY ledger_revision DESC LIMIT 1", [], |r|r.get(0)).unwrap();
    let saved: SharedWorkReceipt = serde_json::from_str(&saved).unwrap();
    assert!(saved.usage_complete);
    assert_eq!(saved.known_consumed_tokens, 200);
    assert_eq!(f.state.lock().unwrap().shared.starts, 1);
    assert_eq!(db.query_row("SELECT COUNT(*) FROM owner_shared_process_observations WHERE json_extract(payload,'$.state')='stopped'",[],|r|r.get::<_,u64>(0)).unwrap(),1);
    drop(db);
    f.stop().await;
}

/// An older server has no demand retry route (ADR-193 amendment).
pub(crate) fn demand_retry_enabled(s:&super::State)->bool{!s.shared.demands.no_retry}
#[cfg(feature="shared-context")]
pub(crate) fn context_enabled(s:&super::State)->bool{s.shared.contexts.active}
#[cfg(feature="shared-context")]
pub(crate) fn context_v2_enabled(s:&super::State)->bool{s.shared.contexts.active && s.shared.contexts.v2}

#[cfg(feature = "shared-budget-execution")]
#[path = "file_execution_tests.rs"]
mod file_execution;
