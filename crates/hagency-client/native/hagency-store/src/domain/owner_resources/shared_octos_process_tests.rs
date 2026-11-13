//! Actual isolated Octos and local model through the original durable pool
//! custody and accounting path. No Matrix account or provider is contacted.
use super::*;
use hagency_agent_local::{
    codex,
    runtime::octos_turn,
    room_tools::{room_queue, RoomReply},
    questions::question_queue,
};
use hagency_runtime::{
    octos::{
        profile_binding::ProfileBinding,
        session::{Limits, Observation},
    },
    owned::{ManagedProcessState, OwnedOctosSession},
    questions::Answer,
    room_tools::{RoomOperation, RoomTools},
};
use sha2::{Digest, Sha256};
use std::{fs, process::Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};

struct TestHost {
    custody: Option<SharedCallCustody>,
    sender: Option<tokio::sync::oneshot::Sender<SharedCallCustody>>,
}
impl octos_turn::Host for TestHost {
    async fn start(
        &mut self,
        process: &mut OwnedOctosSession,
        turn: &str,
        input: &str,
    ) -> codex::Result<()> {
        let custody = self.custody.as_ref().ok_or(codex::Error::Unknown)?;
        if custody.model_input() != input {
            return Err(codex::Error::Unknown);
        }
        custody
            .start_octos_turn(process, turn)
            .await
            .map_err(|_| codex::Error::Unknown)?;
        eprintln!("fixture: admitted Octos turn started");
        let custody = self.custody.take().ok_or(codex::Error::Unknown)?;
        self.sender
            .take()
            .ok_or(codex::Error::Unknown)?
            .send(custody)
            .map_err(|_| codex::Error::Unknown)
    }
}
struct TestCapture {
    receiver: Option<tokio::sync::oneshot::Receiver<SharedCallCustody>>,
    sink: Option<SharedCallSink>,
    receipt: Option<SharedWorkReceipt>,
}
impl octos_turn::Capture for TestCapture {
    async fn bind(&mut self, process: &OwnedOctosSession) -> codex::Result<()> {
        let custody = self
            .receiver
            .take()
            .ok_or(codex::Error::Unknown)?
            .await
            .map_err(|_| codex::Error::Unknown)?;
        self.sink = Some(
            custody
                .bind_started_octos_turn(process)
                .await
                .map_err(|_| codex::Error::Unknown)?,
        );
        eprintln!("fixture: capture bound");
        Ok(())
    }
    async fn observe(&mut self, observation: &Observation) -> codex::Result<()> {
        self.sink
            .as_mut()
            .ok_or(codex::Error::Unknown)?
            .observe_octos(observation)
            .await
            .map_err(|_| codex::Error::Unknown)?;
        Ok(())
    }
    async fn complete(&mut self, process: &OwnedOctosSession, text: &str) -> codex::Result<()> {
        let sink = self.sink.as_mut().ok_or(codex::Error::Unknown)?;
        assert!(sink.finish_octos(process, "wrong answer").await.is_err());
        self.receipt = Some(
            sink.finish_octos(process, text)
                .await
                .map_err(|_| codex::Error::Unknown)?,
        );
        Ok(())
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_OCTOS_EXECUTABLE and HAGENCY_TEST_OCTOS_GUARDIAN"]
async fn shared_octos_original_process_accounts_and_finishes_the_admitted_interactive_work() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let model_script = root.join("model.py");
    fs::write(
        &model_script,
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../hagency-agent-local/tests/octos_offline_model.py"
        )),
    )
    .unwrap();
    let mut model = tokio::process::Command::new("python3")
        .arg(&model_script)
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(model.stdout.take().unwrap())
        .read_line(&mut port)
        .await
        .unwrap();
    let port: u16 = port.trim().parse().unwrap();
    let home = root.join("octos-owner");
    fs::create_dir(&home).unwrap();
    fs::create_dir(home.join("profiles")).unwrap();
    let selected = serde_json::json!({
        "id":"coding", "name":"Fixture", "enabled":true,
        "created_at":"2026-01-01T00:00:00Z", "updated_at":"2026-01-01T00:00:00Z",
        "config":{"llm":{"primary":{"family_id":"local","model_id":"test-model",
            "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}},
            "fallbacks":[{"family_id":"local","model_id":"different-model",
                "route":{"base_url":format!("http://127.0.0.1:{port}/v1"),"api_type":"openai"}}]}}
    });
    fs::write(home.join("profiles/coding.json"), selected.to_string()).unwrap();
    let mut other = selected.clone();
    other["id"] = "other".into();
    fs::write(home.join("profiles/other.json"), other.to_string()).unwrap();
    let expires = || Instant::now() + Duration::from_secs(45);
    let registry = OwnerResourceRegistry::open_root(&root.join("owner")).unwrap();
    let profile = authority('a');
    let observation = registry
        .observe_octos_profile(ProfileBinding::observe(&home, "coding", expires()).unwrap())
        .await
        .unwrap();
    let resource = registry
        .prepare_octos_enrollment(
            profile.clone(),
            "enroll_octos".into(),
            observation,
            1000,
            expires(),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id;
    let grant = grant(&registry, &profile, &resource, "parent", 700)
        .await
        .unwrap();
    let project = super::super::command(&grant, "project", 400);
    let receipt = apply(&registry, &profile, project.clone()).await;
    let f = Fixture {
        _temp: temp,
        registry,
        profile,
        resource,
        grant,
        project,
        receipt,
    };
    let mut reservation = agent(&f, "shared", 150);
    reservation.definition.capability = "coding".into();
    reservation.definition_digest = digest(&reservation.definition);
    let saved = reserve(&f, reservation.clone()).await;
    assert_eq!(saved.state, "reserved", "{:?}", saved.reason);
    let now = graphs::now_ms().unwrap();
    let command = SharedRuntimeCommand {
        version: 5,
        capability: SHARED_OCTOS_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "shared_original".into(),
        assignment_id: "shared_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: saved.agent_reservation_id.clone().unwrap(),
        reserve_command_digest: digest(&reservation),
        reserve_receipt_digest: digest(&saved),
        reserve_command: reservation,
        policy: SHARED_OCTOS_POLICY.into(),
        issued_at_ms: now,
        expires_at_ms: now + 30_000,
    };
    let SharedRuntimeClaim::Issued(custody) = f
        .registry
        .claim_shared_runtime(proof(&f, command.clone()))
        .await
        .unwrap()
    else {
        panic!("fresh original custody")
    };
    let room = root.join("room");
    fs::create_dir(&room).unwrap();
    for name in ["home", "config", "work", "tmp"] {
        fs::create_dir(room.join(name)).unwrap();
    }
    let executable = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_EXECUTABLE").unwrap());
    let guardian = PathBuf::from(std::env::var_os("HAGENCY_TEST_OCTOS_GUARDIAN").unwrap());
    let binding = ProfileBinding::observe(&home, "coding", expires()).unwrap();
    let mut workspace_session = custody.prepare_octos_session(None, "c".repeat(64)).unwrap();
    let bootstrap_scope = workspace_session.storage_scope().to_owned();
    let mut wrong_room = custody.prepare_octos_session(Some(SharedSessionRoute {
        version: 1, runtime_command_id: command.command_id.clone(), runtime_command_digest: digest(&command),
        assignment_id: command.assignment_id.clone(), event_id: "$octos_event".into(),
        body_digest: format!("{:x}", Sha256::digest(b"$octos_event")), room_id: "!other:test".into(),
        requester_mxid: command.reserve_command.manager_mxid.clone(), thread_root: "$octos_event".into(),
        room_binding: None, encrypted: true, native_context_version: None,
    }), "c".repeat(64)).unwrap();
    let mut process = OwnedOctosSession::spawn_shared_isolated(
        &guardian,
        &executable,
        &room,
        binding,
        Limits {
            write_timeout_ms: 5000,
            event_wait_ms: 20000,
            lifetime_ms: 60000,
        },
    )
    .unwrap();
    let sink = Arc::new(
        (*custody)
            .bind_process(process.shared_process_source().unwrap())
            .unwrap(),
    );
    process
        .hello()
        .await
        .unwrap_or_else(|error| panic!("{error}; {}", process.stderr_tail(4096)));
    let current = f
        .registry
        .observe_octos_profile(ProfileBinding::observe(&home, "coding", expires()).unwrap())
        .await
        .unwrap();
    let verified = f
        .registry
        .verify_octos_resource(f.profile.clone(), f.resource.clone(), current)
        .await
        .unwrap();
    let other_room = root.join("other-room");
    fs::create_dir(&other_room).unwrap();
    for name in ["home", "config", "work", "tmp"] {
        fs::create_dir(other_room.join(name)).unwrap();
    }
    let other_binding = ProfileBinding::observe(&home, "other", expires()).unwrap();
    let mut wrong_process = OwnedOctosSession::spawn_shared_isolated(
        &guardian,
        &executable,
        &other_room,
        other_binding,
        Limits::default(),
    )
    .unwrap();
    wrong_process.hello().await.unwrap();
    wrong_process
        .shared_process_source()
        .unwrap()
        .claim_managed_custody(&"a".repeat(64), "other-fixture")
        .unwrap();
    assert!(
        verified.qualify_octos_process(&mut wrong_process).is_err(),
        "a same-model profile cannot use another profile's budget"
    );
    let wrong_model_room = root.join("wrong-model-room");
    fs::create_dir(&wrong_model_room).unwrap();
    for name in ["home", "config", "work", "tmp"] { fs::create_dir(wrong_model_room.join(name)).unwrap(); }
    let wrong_model = ProfileBinding::observe_model(&home, "coding", "different-model", expires()).unwrap();
    let mut wrong_model_process = OwnedOctosSession::spawn_shared_isolated(
        &guardian, &executable, &wrong_model_room, wrong_model, Limits::default()).unwrap();
    wrong_model_process.hello().await.unwrap();
    wrong_model_process.shared_process_source().unwrap()
        .claim_managed_custody(&"a".repeat(64), "wrong-model-fixture").unwrap();
    assert!(verified.qualify_octos_process(&mut wrong_model_process).is_err(),
        "another model in the same profile cannot use this resource's budget");
    let qualified = verified.qualify_octos_process(&mut process).unwrap();
    sink.record(qualified).await.unwrap();
    let until = Instant::now() + Duration::from_secs(20);
    process.install_renewable_shared_activity(1, until).unwrap();
    let lease = SharedActivityLease {
        authority: SharedActivityRef {
            runtime_command_id: command.command_id.clone(),
            epoch: 1,
            token: "a".repeat(64),
        },
        command_digest: digest(&command),
        process_digest: sink.process_digest().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + 20000,
    };
    let authorization = || {
        sink.authorize_activity(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            &lease,
            until,
        )
        .unwrap()
    };
    let body = "$octos_event".to_owned();
    let work = SharedWorkItem {
        room_binding: None,
        encrypted: true,
        id: "octos_work".into(),
        assignment_id: command.assignment_id.clone(),
        runtime_command_digest: digest(&command),
        event_id: "$octos_event".into(),
        room_id: command.reserve_command.room_id.clone(),
        requester_mxid: command.reserve_command.manager_mxid.clone(),
        thread_root: "$octos_event".into(),
        body_digest: format!("{:x}", Sha256::digest(body.as_bytes())),
        body,
        execution_id: "octos_execution".into(),
        created_at_ms: graphs::now_ms().unwrap(),
    };
    f.registry
        .receive_shared_work(authorization(), work.clone())
        .await
        .unwrap();
    assert!(wrong_room.attach(authorization(), work.clone(), &process).await.is_err());
    workspace_session.attach(authorization(), work.clone(), &process).await.unwrap();
    assert_ne!(workspace_session.storage_scope(), bootstrap_scope);
    assert!(workspace_session.attach(authorization(), work.clone(), &process).await.is_err());
    let text = "original admitted Octos question".to_owned();
    assert!(!serde_json::to_string(&work).unwrap().contains(&text));
    assert!(
        f.registry
            .prepare_shared_call(authorization(), work.clone())
            .await
            .is_err()
    );
    let input = f
        .registry
        .prepare_shared_decrypted_input(authorization(), work.clone(), text.clone())
        .await
        .unwrap();
    assert!(
        f.registry
            .prepare_shared_decrypted_input(
                authorization(),
                work.clone(),
                "changed plaintext".into()
            )
            .await
            .is_err()
    );
    let call = f
        .registry
        .prepare_shared_input_call(authorization(), work.clone(), input.clone())
        .await
        .unwrap();
    let start = f
        .registry
        .begin_shared_input_start(authorization(), work.clone(), call.call_id, input)
        .await
        .unwrap();
    let custody = f
        .registry
        .issue_shared_call(
            start,
            SharedWorkStart {
                work: work.clone(),
                work_digest: digest(&work),
                activity_epoch: 1,
                newly_started: true,
            },
        )
        .await
        .unwrap();
    let scope = hagency_agent_local::Scope {
        agent: command.reserve_command.proposed_agent_id.clone(),
        binding: work.assignment_id.clone(),
        room: work.room_id.clone(),
        requester: work.requester_mxid.clone(),
        thread: work.thread_root.clone(),
    };
    let (room_sender, mut room_receiver) = room_queue(&scope, &work.id).unwrap();
    let (question_sender, mut question_receiver) = question_queue();
    let (queue, unused) = codex::approval_queue(1).unwrap();
    drop(unused);
    let queue = queue
        .with_room_tools(room_sender)
        .with_questions(question_sender);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut host = TestHost {
        custody: Some(custody),
        sender: Some(sender),
    };
    let mut capture = TestCapture {
        receiver: Some(receiver),
        sink: None,
        receipt: None,
    };
    let workspace = room.join("work");
    let run = octos_turn::run(
        &mut process,
        "coding",
        "coding:local:pool-test",
        workspace.to_str().unwrap(),
        "0192f0c1-0000-7000-8000-000000000001",
        &scope,
        &work.id,
        &text,
        &queue,
        None,
        None,
        Some(RoomTools::history()),
        &mut host,
        &mut capture,
    );
    let interaction = async {
        let request = room_receiver.recv().await.unwrap();
        eprintln!("fixture: received room tool");
        assert!(matches!(request.operation, RoomOperation::History { .. }));
        request.reply(RoomReply::new("OFFLINE_ROOM_RECEIPT".into(), false).unwrap());
        let question = question_receiver.recv().await.unwrap();
        eprintln!("fixture: received question");
        assert_eq!(question.questions[0].question, "Which option?");
        let answer = question.reply([("q0".into(), Answer::Option("One".into()))].into());
        assert!(answer.accepted().await);
        Ok::<(), codex::Error>(())
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(18), async {
        tokio::try_join!(run, interaction)
    }).await.expect("interactive fixture timed out").expect("shared Octos turn failed");
    assert_eq!(outcome.text, "Offline integration complete.");
    let result = capture.receipt.take().expect("durable Octos completion");
    assert_eq!(
        (result.state.as_str(), result.body.as_deref()),
        ("completed", Some("Offline integration complete."))
    );
    assert!(
        result.usage_complete,
        "the fresh instance's complete session totals settle the call"
    );
    assert!(result.known_consumed_tokens > 0);
    let stop = process.stop_shared_observation().unwrap();
    assert_eq!(stop.state(), ManagedProcessState::Stopped);
    sink.record(stop).await.unwrap();
    model.kill().await.unwrap();
    f.registry.shutdown().await.unwrap();
}
