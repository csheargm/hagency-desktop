//! Original supervised Claude processes resume context with separate call custody.
#[cfg(target_os = "macos")]
#[path = "shared_claude_installed_tests.rs"]
mod installed;
use super::*;
use hagency_runtime::{
    claude::{account::AccountBinding, session::Limits as ClaudeLimits, Message},
    owned::{OwnedClaudeSession, SharedClaudePolicy},
};
use sha2::{Digest, Sha256};

struct Active {
    f: Fixture,
    binding: AccountBinding,
    original: SharedRuntimeCommand,
    command: SharedAttemptCommand,
    process: OwnedClaudeSession,
    sink: Arc<SharedAttemptProcessSink>,
    lease: SharedActivityLease,
    expires: Instant,
    session: Option<SharedClaudeSession>,
    work: SharedWorkItem,
}
impl Active {
    fn authority(&self) -> VerifiedSharedActivity {
        self.sink
            .authorize_activity(
                enrollment_from(
                    &self.f.profile,
                    &self.f.resource,
                    &self.f.grant.resource_revision,
                    1,
                ),
                &self.lease,
                self.expires,
            )
            .unwrap()
    }
    async fn attach(&mut self) {
        self.f
            .registry
            .receive_shared_work(self.authority(), self.work.clone())
            .await
            .unwrap();
        let proof = self.authority();
        self.session
            .as_mut()
            .unwrap()
            .attach(proof, self.work.clone(), &self.process)
            .await
            .unwrap();
        let proof = self.authority();
        assert!(
            self.session
                .as_mut()
                .unwrap()
                .attach(proof, self.work.clone(), &self.process)
                .await
                .is_err()
        );
    }
    async fn turn(&mut self, text: &str, expected_previous: &str) -> SharedWorkReceipt {
        self.attach().await;
        let input = self
            .f
            .registry
            .prepare_shared_decrypted_input(self.authority(), self.work.clone(), text.into())
            .await
            .unwrap();
        let call = self
            .f
            .registry
            .prepare_shared_input_call(self.authority(), self.work.clone(), input.clone())
            .await
            .unwrap();
        assert_eq!(call.reserved_tokens, 0);
        let start = self
            .f
            .registry
            .begin_shared_input_start(self.authority(), self.work.clone(), call.call_id, input)
            .await
            .unwrap();
        let custody = self
            .f
            .registry
            .issue_shared_call(
                start,
                SharedWorkStart {
                    work: self.work.clone(),
                    work_digest: digest(&self.work),
                    activity_epoch: 1,
                    newly_started: true,
                },
            )
            .await
            .unwrap();
        custody.start_claude_turn(&mut self.process).await.unwrap();
        self.process.next_message().await.unwrap();
        let mut capture = custody
            .bind_started_claude_turn(&self.process)
            .await
            .unwrap();
        let session = self.session.take().unwrap();
        session.bind_capture(&mut capture, &self.process).unwrap();
        let Message::Event { payload, .. } = self.process.next_message().await.unwrap() else {
            panic!("original result")
        };
        let answer = payload["result"].as_str().unwrap();
        assert_eq!(
            answer,
            format!("Previous native inputs: {expected_previous}")
        );
        let pending = capture
            .observe_claude(self.process.last_observation().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !pending.usage_complete,
            "a result alone never closes background work"
        );
        self.process.next_message().await.unwrap();
        capture
            .observe_claude(self.process.last_observation().unwrap())
            .await
            .unwrap();
        let receipt = capture.finish_claude(&self.process, answer).await.unwrap();
        session.retain(&capture, &self.process).await.unwrap();
        receipt
    }
    async fn advance(mut self, id: &str, requester: &str, restart: bool) -> Self {
        let stopped = self
            .sink
            .record(self.process.stop_shared_observation().unwrap())
            .await
            .unwrap();
        assert_eq!(stopped.state, "stopped");
        if restart {
            self.f.registry.shutdown().await.unwrap();
            self.f.registry =
                OwnerResourceRegistry::open_root(&self.f._temp.path().join("owner")).unwrap();
        }
        let command = next_attempt(&self.f, &self.command, &stopped).await;
        open(self.f, self.binding, self.original, command, id, requester).await
    }
    async fn stop(mut self) {
        self.sink
            .record(self.process.stop_shared_observation().unwrap())
            .await
            .unwrap();
        self.f.registry.shutdown().await.unwrap();
    }
}
fn spawn_claude(f: &Fixture, resume: Option<&str>) -> OwnedClaudeSession {
    let binary =
        PathBuf::from(std::env::var_os("HAGENCY_TEST_CLAUDE_RUNTIME_PROBE").expect("offline peer"));
    let root = f._temp.path().join("claude_native");
    std::fs::create_dir_all(&root).unwrap();
    let policy = SharedClaudePolicy::Text;
    let mut environment = std::collections::BTreeMap::new();
    environment.insert("CLAUDE_CONFIG_DIR".into(), root.as_os_str().into());
    for (key, value) in [
        ("HAGENCY_OFFLINE_CLAUDE_MODE", "shared-session"),
        ("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1"),
        ("CLAUDE_CODE_BG_TASKS_REPORT_RUNNING", "1"),
    ] {
        environment.insert(key.into(), value.into());
    }
    if let Some(value) = std::env::var_os("SystemRoot") {
        environment.insert("SystemRoot".into(), value);
    }
    let launch = hagency_platform::Launch {
        executable: binary.clone(),
        arguments: policy
            .arguments_resuming("claude-opus-5", "medium", resume)
            .unwrap()
            .into_iter()
            .map(Into::into)
            .collect(),
        directory: root,
        environment,
        require_crash_containment: false,
    };
    OwnedClaudeSession::spawn_shared_resuming(
        &binary,
        &launch,
        ClaudeLimits {
            write_timeout_ms: 1000,
            event_wait_ms: 2000,
            lifetime_ms: 10000,
        },
        policy,
        resume,
    )
    .unwrap()
}
async fn active() -> Active {
    let (f, binding) = super::super::super::claude_process::fixture_claude().await;
    let mut original = command(&f).await;
    original.version = 4;
    original.capability = SHARED_CLAUDE_CAPABILITY.into();
    original.policy = SHARED_CLAUDE_POLICY.into();
    let SharedRuntimeClaim::Issued(custody) = f
        .registry
        .claim_shared_runtime(proof(&f, original.clone()))
        .await
        .unwrap()
    else {
        panic!("root custody")
    };
    let mut process = spawn_claude(&f, None);
    let sink = (*custody)
        .bind_process(process.shared_process_source().unwrap())
        .unwrap();
    process.initialize().await.unwrap();
    sink.record(process.qualify_shared(&binding).unwrap())
        .await
        .unwrap();
    sink.record(process.stop_shared_observation().unwrap())
        .await
        .unwrap();
    let command = attempt(&f, &original).await;
    open(f, binding, original, command, "first", "@owner:test").await
}
async fn open(
    f: Fixture,
    binding: AccountBinding,
    original: SharedRuntimeCommand,
    command: SharedAttemptCommand,
    id: &str,
    requester: &str,
) -> Active {
    let body = format!("${id}");
    let work = SharedWorkItem {
        room_binding: None,
        encrypted: true,
        id: id.into(),
        assignment_id: command.assignment_id.clone(),
        runtime_command_digest: digest(&command),
        event_id: body.clone(),
        room_id: original.reserve_command.room_id.clone(),
        requester_mxid: requester.into(),
        thread_root: body.clone(),
        body_digest: format!("{:x}", Sha256::digest(body.as_bytes())),
        body,
        execution_id: format!("execution_{id}"),
        created_at_ms: graphs::now_ms().unwrap(),
    };
    let route = SharedSessionRoute {
        version: 1,
        runtime_command_id: command.command_id.clone(),
        runtime_command_digest: digest(&command),
        assignment_id: work.assignment_id.clone(),
        event_id: work.event_id.clone(),
        body_digest: work.body_digest.clone(),
        room_id: work.room_id.clone(),
        requester_mxid: work.requester_mxid.clone(),
        thread_root: work.thread_root.clone(),
        room_binding: work.room_binding.clone(),
        encrypted: work.encrypted,
        native_context_version: None,
    };
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, command.clone()))
        .await
        .unwrap()
    else {
        panic!("successor custody")
    };
    let session = custody
        .prepare_claude_session(Some(route), "a".repeat(64))
        .await
        .unwrap();
    let mut process = spawn_claude(&f, session.resume_id());
    let sink = Arc::new(
        (*custody)
            .bind_process(process.shared_process_source().unwrap())
            .unwrap(),
    );
    process.initialize().await.unwrap();
    sink.record(process.qualify_shared(&binding).unwrap())
        .await
        .unwrap();
    let expires = Instant::now() + Duration::from_secs(8);
    process
        .install_shared_activity(1, expires, expires)
        .unwrap();
    let lease = SharedActivityLease {
        authority: SharedActivityRef {
            runtime_command_id: command.command_id.clone(),
            epoch: 1,
            token: "a".repeat(64),
        },
        command_digest: digest(&command),
        process_digest: sink.process_digest().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + 8000,
    };
    Active {
        f,
        binding,
        original,
        command,
        process,
        sink,
        lease,
        expires,
        session: Some(session),
        work,
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_session_resumes_after_restart_and_only_charges_each_new_call() {
    let mut a = Box::pin(active()).await;
    assert!(!a.session.as_ref().unwrap().is_resuming());
    let first = a.turn("Remember blue comet", "").await;
    let native = a.process.session_id().unwrap().to_owned();
    a = a.advance("second", "@owner:test", true).await;
    assert_eq!(
        a.session.as_ref().unwrap().resume_id(),
        Some(native.as_str())
    );
    let second = a.turn("Continue", "Remember blue comet").await;
    a = a.advance("third", "@owner:test", false).await;
    assert_eq!(
        a.session.as_ref().unwrap().resume_id(),
        Some(native.as_str())
    );
    let third = a.turn("Again", "Remember blue comet; Continue").await;
    for receipt in [&first, &second, &third] {
        assert_eq!(receipt.state, "completed");
        assert!(receipt.usage_complete);
        assert_eq!(
            receipt.known_consumed_tokens, 16,
            "cumulative native context is never charged again"
        );
        assert_eq!(receipt.retained_tokens, Some(16));
    }
    assert_ne!(first.call_id, second.call_id);
    assert_ne!(first.process_digest, second.process_digest);
    assert_ne!(second.call_id, third.call_id);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_session_isolates_requesters_and_matches_route_before_model_input() {
    let mut a = Box::pin(active()).await;
    a.turn("Owner secret", "").await;
    a = a.advance("other", "@other:test", false).await;
    assert!(!a.session.as_ref().unwrap().is_resuming());
    a.turn("Other person's context", "").await;
    a = a.advance("owner_again", "@owner:test", false).await;
    assert!(a.session.as_ref().unwrap().is_resuming());
    let mut wrong = a.work.clone();
    wrong.requester_mxid = "@other:test".into();
    a.f.registry
        .receive_shared_work(a.authority(), wrong.clone())
        .await
        .unwrap();
    std::fs::remove_file(a.f._temp.path().join("claude_native/shared-claude.prompt")).unwrap();
    let proof = a.authority();
    assert!(
        a.session
            .as_mut()
            .unwrap()
            .attach(proof, wrong, &a.process)
            .await
            .is_err()
    );
    assert!(
        !a.f._temp
            .path()
            .join("claude_native/shared-claude.prompt")
            .exists()
    );
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_session_unfinished_opening_fences_older_checkpoint() {
    let mut a = Box::pin(active()).await;
    a.turn("Previous context", "").await;
    a = a.advance("interrupted", "@owner:test", false).await;
    assert!(a.session.as_ref().unwrap().is_resuming());
    a.attach().await;
    a = a.advance("after_restart", "@owner:test", true).await;
    assert!(!a.session.as_ref().unwrap().is_resuming());
    let result = a.turn("Start fresh", "").await;
    assert_eq!(result.known_consumed_tokens, 16);
    assert!(result.usage_complete);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_session_invalid_counters_keep_answer_unknown_and_are_not_reused() {
    let mut a = Box::pin(active()).await;
    a.turn("Previous context", "").await;
    a = a.advance("regression", "@owner:test", false).await;
    assert!(a.session.as_ref().unwrap().is_resuming());
    std::fs::write(
        a.f._temp
            .path()
            .join("claude_native/shared-claude.invalid_usage"),
        b"offline",
    )
    .unwrap();
    let result = a.turn("Broken counters", "Previous context").await;
    assert!(
        !result.usage_complete,
        "invalid counters cannot prove a free completed call"
    );
    assert_eq!(result.state, "completed");
    assert_eq!(
        result.body.as_deref(),
        Some("Previous native inputs: Previous context")
    );
    std::fs::remove_file(
        a.f._temp
            .path()
            .join("claude_native/shared-claude.invalid_usage"),
    )
    .unwrap();
    a = a.advance("after_regression", "@owner:test", false).await;
    assert!(!a.session.as_ref().unwrap().is_resuming());
    a.stop().await;
}
