//! Real scoped Octos and durable host custody; Matrix and model use local fixtures.
use super::*;
use crate::agent_questions::{AnswerValue, Card};
use hagency_runtime::octos::profile_binding::ProfileBinding;
use std::{collections::BTreeMap, fs, process::Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE and HAGENCY_TEST_OCTOS_GUARDIAN"]
async fn native_shared_octos_encrypted_fixture_answers_question_and_settles_original_call() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("hagency::console::native::shared_execution=debug,hagency_runtime::owned=info")
        .with_test_writer().try_init();
    let f = Fixture::empty_current().await;
    let root = f._root.path().canonicalize().unwrap();
    let home = root.join("octos-profile");
    fs::create_dir(&home).unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    let model_file = root.join("model.py");
    fs::write(&model_file, include_str!("../../../../../hagency-agent-local/tests/octos_offline_model.py")).unwrap();
    fs::write(root.join("offline-model-script.json"), json!([
        {"name":"discover_tools","arguments":{"name":"hagency_report_progress"}},
        {"name":"run_tool","arguments":{"name":"hagency_report_progress","arguments":{"text":"First finding"}}},
        {"name":"run_tool","arguments":{"name":"hagency_report_progress","arguments":{"text":"Second finding"}}},
        {"name":"discover_tools","arguments":{"name":"hagency_read_room_history"}},
        {"name":"run_tool","arguments":{"name":"hagency_read_room_history","arguments":{"cursor":null}}},
        {"name":"run_tool","arguments":{"name":"ask_user_question","arguments":{"questions":[{
            "header":"Choice","question":"Which option?","options":[{"label":"One","description":"First"},
                {"label":"Two","description":"Second"}],"multi_select":false}]}}},
        {"name":"run_tool","arguments":{"name":"hagency_request_user_input","arguments":{"questions":[{
            "id":"branch","header":"Branch","question":"What branch name?","options":[]}]}}}
    ]).to_string()).unwrap();
    let mut model = tokio::process::Command::new("python3").arg(&model_file).arg(&root)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    let port: u16 = port.trim().parse().unwrap();
    fs::write(home.join("profiles/coding.json"), json!({
        "id":"coding", "name":"Fixture", "enabled":true,
        "created_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"primary-model",
            "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}},
            "fallbacks":[{"family_id":"local","model_id":"test-model",
                "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}]}}
    }).to_string()).unwrap();
    let scope = f.transport.scope(Permission::PublishGrants).await.unwrap();
    let expiry = Instant::now() + Duration::from_secs(30);
    let primary = f.registry.observe_octos_profile(ProfileBinding::observe(&home, "coding", expiry).unwrap()).await.unwrap();
    f.registry.prepare_octos_enrollment(scope.profile.clone(), "octos_primary_fixture".into(), primary, 1000, expiry)
        .unwrap().approve().await.unwrap();
    let observed = f.registry.observe_octos_profile(ProfileBinding::observe_model(&home, "coding", "test-model", expiry).unwrap()).await.unwrap();
    let resource = f.registry.prepare_octos_enrollment(scope.profile.clone(), "octos_fixture".into(), observed, 1000, expiry)
        .unwrap().approve().await.unwrap();
    let resource_id = resource.resource_id.clone();
    let revision = f.registry.resource(&scope.profile, resource_id.clone()).await.unwrap().revision;
    let enrollment = VerifiedManagementEnrollment::new(scope.profile.clone(), "enrollment_fixture".into(),
        "con_fixture".into(), resource_id.clone(), revision, "@coordinator:test".into(), "device".into(), 1, scope.deadline).unwrap();
    let grant = f.registry.prepare_capacity_grant(enrollment, OwnerGrantSelection {
        renewal_mode: None, per_request_tokens: None, calendar: None,
        command_id: "grant_consent".into(), grant_id: "grant_fixture".into(),
        coordinator_mxid: "@coordinator:test".into(), delegation_expires_at_ms: now()+60_000,
        allow_self_approval: false, eligible_managers: vec!["@manager:test".into()], allocated_tokens: 700,
    }, scope.deadline).unwrap().approve().await.unwrap();
    f.state.lock().unwrap().grant = Some(grant);
    drop(scope);
    let (f, transport, reserve) = prepared_with(f).await;
    let (f, transport, mut command) = runtime_fixture(f, transport, reserve).await;
    command.version = 5;
    command.capability = SHARED_OCTOS_CAPABILITY.into();
    command.policy = SHARED_OCTOS_POLICY.into();
    let work = {
        let mut state = f.state.lock().unwrap();
        state.shared.command = Some(command.clone());
        let work = state.shared.work.as_mut().unwrap();
        work.runtime_command_digest = digest(&command).unwrap();
        work.encrypted = true;
        work.body = work.event_id.clone();
        work.body_digest = format!("{:x}", Sha256::digest(work.body.as_bytes()));
        work.clone()
    };
    let prompt = "Private fixture asks Octos to read history and choose";
    let (transport, sent) = transport.with_crypto_fixture(Some(prompt.into()));
    let transport = transport.with_lost_message_response();
    let (transport, history_reads) = transport.with_history_fixture(work.room_id.clone(), work.event_id.clone(), vec![]);
    let (transport, room) = transport.with_question_fixture(&["@manager:test"], Duration::from_secs(60));
    let workspace_root = root.join("octos-native");
    hagency_store::private::directory(&workspace_root).unwrap();
    let progress = transport.poll_octos_fixture(&f.registry, resource_id.clone(), home.clone(), workspace_root.clone(), "coding".into()).await.unwrap();
    assert!(matches!(progress, SharedExecutionProgress::Running { .. }));
    let (event, card) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some((event, content)) = room.cards().into_iter().next() {
                break (event, Card::from_content(&content).unwrap());
            }
            if f.state.lock().unwrap().shared.processes.values().any(|r| r.state == "stopped") {
                let state = f.state.lock().unwrap();
                let requests: Vec<Value> = fs::read(root.join("offline-model-requests.json"))
                    .ok().and_then(|v| serde_json::from_slice(&v).ok()).unwrap_or_default();
                let receipts: Vec<_> = state.shared.receipts.values().map(|r| (r.state.clone(), r.usage_complete)).collect();
                panic!("Octos stopped before posting its question: starts={}, model_calls={}, receipts={receipts:?}, encrypted_sends={}",
                    state.shared.starts, requests.len(), sent.lock().unwrap().len());
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("the original Octos question reaches the room");
    assert_eq!(card.questions[0].question, "Which option?");
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let rows = sent.lock().unwrap().iter().map(|(_, text)| text.clone()).collect::<Vec<_>>();
            if rows.iter().any(|text| text == "First finding") && rows.iter().any(|text| text == "Second finding") {
                assert!(!rows.iter().any(|text| text == "Offline integration complete."));
                assert_eq!(rows.iter().filter(|text| *text == "First finding").count(), 1);
                assert_eq!(rows.iter().filter(|text| *text == "Second finding").count(), 1);
                assert!(rows.iter().position(|text| text == "First finding") < rows.iter().position(|text| text == "Second finding"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("both progress messages reach the room before the question is answered");
    let answer = BTreeMap::from([(card.questions[0].id.clone(), AnswerValue::Option("One".into()))]);
    room.answer("@manager:test", card.answer_content(&event, &answer).unwrap());
    let (event, card) = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(result) = room.cards().into_iter().find_map(|(event, content)| {
                let card = Card::from_content(&content)?;
                (card.questions[0].question == "What branch name?").then_some((event, card))
            }) { break result; }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("the CLI-style free-form question reaches the room");
    let answer = BTreeMap::from([(card.questions[0].id.clone(), AnswerValue::Text("feature/fixture".into()))]);
    room.answer("@manager:test", card.answer_content(&event, &answer).unwrap());
    tokio::time::timeout(Duration::from_secs(15), async {
        while !f.state.lock().unwrap().shared.processes.values().any(|r| r.state == "stopped") {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.expect("the first original process stops");
    assert!(!history_reads.lock().unwrap().is_empty());
    assert!(sent.lock().unwrap().iter().any(|(_, text)| text == "Offline integration complete."));
    let requests: Vec<Value> = serde_json::from_slice(&fs::read(root.join("offline-model-requests.json")).unwrap()).unwrap();
    assert!(!requests.is_empty() && requests.iter().all(|request| request["model"] == "test-model"));
    assert!(requests.iter().any(|request| request.to_string().contains("feature/fixture")));
    {
        let state = f.state.lock().unwrap();
        assert_eq!(state.shared.starts, 1);
        let receipt = state.shared.receipts.values().find(|r| r.state == "completed").expect("original usage receipt");
        assert!(receipt.usage_complete && receipt.known_consumed_tokens > 0);
        assert!(!receipt.body.as_deref().unwrap().contains("Offline integration complete."));
    }
    let scopes = workspace_root.join("managed-octos/scopes");
    let paths = fs::read_dir(&scopes).unwrap().map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
    assert_eq!(paths.len(), 1);
    let marker = paths[0].join("work/retained-project.txt");
    fs::write(&marker, "synthetic project").unwrap();
    attempts::prepare_lost_start(&f, &transport, &command).await;
    let followup = {
        let mut state = f.state.lock().unwrap();
        let work = state.shared.work.as_mut().unwrap();
        work.event_id = "$octos-followup:test".into();
        work.thread_root = work.event_id.clone();
        work.body = work.event_id.clone();
        work.body_digest = format!("{:x}", Sha256::digest(work.body.as_bytes()));
        work.clone()
    };
    let (next, next_sent) = transport.with_crypto_fixture(Some("Follow up on the same room".into()));
    let (next, _) = next.with_history_fixture(followup.room_id, followup.event_id, vec![]);
    let next_progress = next.poll_octos_successor_fixture(&f.registry, resource_id, home, workspace_root, "coding".into()).await.unwrap();
    assert!(matches!(next_progress, SharedExecutionProgress::Running { .. }));
    attempts::wait_stopped(&f).await;
    assert_eq!(fs::read_to_string(marker).unwrap(), "synthetic project");
    assert!(next_sent.lock().unwrap().iter().any(|(_, text)| text == "Offline integration complete."));
    let state = f.state.lock().unwrap();
    assert_eq!(state.shared.starts, 2);
    assert_eq!(state.shared.receipts.values().filter(|r| r.state == "completed" && r.usage_complete).count(), 2);
    drop(state);
    f.owner.0.shared_execution.drain().await.unwrap();
    model.kill().await.unwrap();
    f.stop().await;
}

async fn coding_successor(yolo: bool, owner_allowed: bool) {
    use hagency_store::management::{EditExecutionPolicy, ExecutionPolicyQuery, ExecutionPolicyValue};
    let f = Fixture::empty_current().await;
    let root = f._root.path().canonicalize().unwrap();
    let home = root.join("octos-profile");
    fs::create_dir_all(home.join("profiles")).unwrap();
    let model_file = root.join("model.py");
    fs::write(&model_file, include_str!("../../../../../hagency-agent-local/tests/octos_offline_model.py")).unwrap();
    fs::write(root.join("offline-model-script.json"), "[null]").unwrap();
    let mut model = tokio::process::Command::new("python3").arg(&model_file).arg(&root)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap()).read_line(&mut port).await.unwrap();
    let port: u16 = port.trim().parse().unwrap();
    fs::write(home.join("profiles/coding.json"), json!({
        "id":"coding","name":"Fixture","enabled":true,
        "created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}}}
    }).to_string()).unwrap();
    let scope = f.transport.scope(Permission::PublishGrants).await.unwrap();
    let expiry = Instant::now() + Duration::from_secs(30);
    let observed = f.registry.observe_octos_profile(ProfileBinding::observe(&home, "coding", expiry).unwrap()).await.unwrap();
    let resource = f.registry.prepare_octos_enrollment(scope.profile.clone(), "octos_coding_fixture".into(), observed, 1000, expiry)
        .unwrap().approve().await.unwrap();
    let resource_id = resource.resource_id.clone();
    let revision = f.registry.resource(&scope.profile, resource_id.clone()).await.unwrap().revision;
    let enrollment = VerifiedManagementEnrollment::new(scope.profile.clone(), "enrollment_fixture".into(),
        "con_fixture".into(), resource_id.clone(), revision, "@coordinator:test".into(), "device".into(), 1, scope.deadline).unwrap();
    let grant = f.registry.prepare_capacity_grant(enrollment, OwnerGrantSelection {
        renewal_mode: None, per_request_tokens: None, calendar: None, command_id: "grant_consent".into(),
        grant_id: "grant_fixture".into(), coordinator_mxid: "@coordinator:test".into(), delegation_expires_at_ms: now()+60_000,
        allow_self_approval: false, eligible_managers: vec!["@manager:test".into()], allocated_tokens: 700,
    }, scope.deadline).unwrap().approve().await.unwrap();
    f.state.lock().unwrap().grant = Some(grant);
    drop(scope);
    let (f, transport, reserve) = prepared_with(f).await;
    let (f, transport, mut command) = runtime_fixture(f, transport, reserve).await;
    command.version = 5;
    command.capability = SHARED_OCTOS_CAPABILITY.into();
    command.policy = SHARED_OCTOS_POLICY.into();
    let work = {
        let mut state = f.state.lock().unwrap();
        state.shared.command = Some(command.clone());
        let work = state.shared.work.as_mut().unwrap();
        work.runtime_command_digest = digest(&command).unwrap();
        work.encrypted = true; work.body = work.event_id.clone();
        work.body_digest = format!("{:x}", Sha256::digest(work.body.as_bytes()));
        work.clone()
    };
    let (transport, _) = transport.with_crypto_fixture(Some("Start the coding task".into()));
    let (transport, _) = transport.with_history_fixture(work.room_id.clone(), work.event_id.clone(), vec![]);
    let workspace_root = root.join("octos-native");
    hagency_store::private::directory(&workspace_root).unwrap();
    let started = transport.poll_octos_fixture(&f.registry, resource_id.clone(), home.clone(), workspace_root.clone(), "coding".into()).await.unwrap();
    assert!(matches!(started, SharedExecutionProgress::Running { .. }));
    tokio::time::timeout(Duration::from_secs(30), async {
        while !f.state.lock().unwrap().shared.processes.values().any(|r| r.state == "stopped") {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }).await.unwrap();
    let mut policy_package = package(81);
    policy_package.permissions.extend([Permission::ReadExecutionPolicy, Permission::EditExecutionPolicy]);
    let manager = f.owner.management(policy_package).await.unwrap();
    let query = ExecutionPolicyQuery { resource_id: resource_id.clone(), engagement_id: command.reserve_command.proposed_agent_id.clone() };
    let before = manager.agent_execution_policy(query.clone()).await.unwrap();
    let saved = manager.edit_execution_policy(EditExecutionPolicy { command_id:"coding_policy".into(),
        resource_id:query.resource_id, engagement_id:query.engagement_id, expected_revision:before.revision,
        execution_policy:ExecutionPolicyValue { yolo }, web_search:None }).await.unwrap();
    assert_eq!(saved.shared_mode.as_deref(), Some(if yolo { "yolo" } else { "standard" }));
    let prior: Vec<Value> = serde_json::from_slice(&fs::read(root.join("offline-model-requests.json")).unwrap()).unwrap();
    let mut script = vec![Value::Null; prior.len()];
    script.push(json!({"name":"write_file","arguments":{"path":"pooled.txt","content":"room workspace"}}));
    script.push(json!({"name":"run_tool","arguments":{"name":"hagency_apply_patch","arguments":{
        "patch":"*** Begin Patch\n*** Add File: pooled-patch.txt\n+patched room\n*** End Patch\n"}}}));
    script.push(json!({"name":"run_tool","arguments":{"name":"hagency_shell","arguments":{"command":"printf shell-ran > shell-result.txt"}}}));
    script.push(json!({"name":"run_tool","arguments":{"name":"hagency_exec_command","arguments":{
        "cmd":"printf PTY_READY; IFS= read -r line; printf 'pty:%s' \"$line\" > interactive-result.txt",
        "tty":true,"yield_time_ms":2000}}}));
    if yolo || owner_allowed {
        script.push(json!({"name":"run_tool","arguments":{"name":"hagency_write_stdin","arguments":{
            "session_id":"$previous_session","chars":"fixture-answer\n","yield_time_ms":2000}}}));
    }
    fs::write(root.join("offline-model-script.json"), serde_json::to_vec(&script).unwrap()).unwrap();
    attempts::prepare_lost_start(&f, &transport, &command).await;
    let followup = {
        let mut state = f.state.lock().unwrap();
        let work = state.shared.work.as_mut().unwrap();
        work.event_id = "$coding-followup:test".into(); work.thread_root = work.event_id.clone();
        work.body = work.event_id.clone(); work.body_digest = format!("{:x}", Sha256::digest(work.body.as_bytes()));
        work.clone()
    };
    let (next, sent) = transport.with_crypto_fixture(Some("Write a file and run the command".into()));
    let (next, approvals) = next.with_approval_fixture(owner_allowed);
    let (next, _) = next.with_history_fixture(followup.room_id, followup.event_id, vec![]);
    let started = next.poll_octos_successor_fixture(&f.registry, resource_id, home, workspace_root.clone(), "coding".into()).await.unwrap();
    assert!(matches!(started, SharedExecutionProgress::Running { .. }));
    attempts::wait_stopped(&f).await;
    let scopes = fs::read_dir(workspace_root.join("managed-octos/scopes")).unwrap()
        .map(|entry| entry.unwrap().path()).collect::<Vec<_>>();
    assert!(scopes.iter().any(|scope| fs::read_to_string(scope.join("work/pooled.txt")).ok().as_deref() == Some("room workspace")));
    assert!(scopes.iter().any(|scope| fs::read_to_string(scope.join("work/pooled-patch.txt")).ok().as_deref() == Some("patched room")));
    assert_eq!(scopes.iter().any(|scope| fs::read_to_string(scope.join("work/shell-result.txt")).ok().as_deref() == Some("shell-ran")), yolo || owner_allowed);
    assert_eq!(scopes.iter().any(|scope| fs::read_to_string(scope.join("work/interactive-result.txt")).ok().as_deref() == Some("pty:fixture-answer")), yolo || owner_allowed);
    let requests = approvals.requests();
    assert_eq!(requests.len(), 2 * usize::from(!yolo));
    if !yolo {
        assert_eq!(requests[0]["method"], "octos/can_use_tool");
        assert_eq!(requests[0]["params"]["input"]["command"], "printf shell-ran > shell-result.txt");
        assert_eq!(requests[0]["sessionAllowed"], false);
        assert_eq!(requests[0]["managerCanDecide"], false);
        assert!(requests[1]["params"]["input"]["cmd"].as_str().unwrap().contains("PTY_READY"));
    }
    assert!(sent.lock().unwrap().iter().any(|(_, text)| text == "Offline integration complete."));
    let state = f.state.lock().unwrap();
    assert_eq!(state.shared.starts, 2);
    assert_eq!(state.shared.receipts.values().filter(|r| r.state == "completed" && r.usage_complete).count(), 2);
    drop(state);
    f.owner.0.shared_execution.drain().await.unwrap();
    model.kill().await.unwrap();
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE and HAGENCY_TEST_OCTOS_GUARDIAN"]
async fn native_shared_octos_successor_runs_owner_selected_coding_policy() {
    coding_successor(true, false).await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE and HAGENCY_TEST_OCTOS_GUARDIAN"]
async fn native_shared_octos_standard_policy_respects_owner_decisions() {
    coding_successor(false, false).await;
    coding_successor(false, true).await;
}
