//! Installed CLI, real shared custody and the canonical store. Only the model
//! endpoint and provider credential are synthetic; no adapter frames are forged.
use super::*;
use hagency_runtime::claude::{account::SharedCredential, session::Phase};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, process::Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};

struct Native {
    binary: PathBuf,
    guardian: PathBuf,
    scope: PathBuf,
    port: u16,
    /// The owner's endpoint token and model, in place of a claude.ai sign-in.
    api: Option<(&'static str, &'static str)>,
}
impl Native {
    fn spawn(&self, binding: &AccountBinding, resume: Option<&str>) -> OwnedClaudeSession {
        let policy = SharedClaudePolicy::Text;
        let mut environment = BTreeMap::new();
        for (key, value) in [
            ("HOME", self.scope.join("home")),
            ("CLAUDE_CONFIG_DIR", self.scope.join("config")),
            ("TMPDIR", self.scope.join("tmp")),
            ("CLAUDE_CODE_TMPDIR", self.scope.join("tmp")),
        ] {
            environment.insert(key.into(), value.into_os_string());
        }
        for (key, value) in [
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            ("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1"),
            ("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1"),
            ("CLAUDE_CODE_BG_TASKS_REPORT_RUNNING", "1"),
        ] {
            environment.insert(key.into(), value.into());
        }
        let endpoint = format!("http://127.0.0.1:{}", self.port);
        let credential = match self.api {
            // The endpoint travels with the credential, as in production: the
            // host puts nothing about the provider into the environment itself.
            Some((token, _)) => SharedCredential::from_api_settings(
                Some(&endpoint),
                token.into(),
                false,
                &[],
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
            None => {
                environment.insert("ANTHROPIC_BASE_URL".into(), endpoint.into());
                // This is an isolated fake account already enrolled by the test host.
                // No real login is read, and no API-key path bypasses native tokenSource.
                SharedCredential::from_authenticated_login(
                    binding.clone(),
                    "offline-shared-contract-token".into(),
                )
                .unwrap()
            }
        };
        assert_eq!(credential.binding().provider_digest(), binding.provider_digest());
        OwnedClaudeSession::spawn_shared_isolated_resuming(
            &self.guardian,
            hagency_platform::Launch {
                executable: self.binary.clone(),
                arguments: policy
                    .arguments_resuming(self.api.map_or("claude-opus-5", |(_, model)| model), "medium", resume)
                    .unwrap()
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                directory: self.scope.join("work"),
                environment,
                require_crash_containment: false,
            },
            &self.scope,
            &credential,
            ClaudeLimits {
                write_timeout_ms: 1000,
                event_wait_ms: 10000,
                lifetime_ms: 30000,
            },
            policy,
            resume,
        )
        .unwrap()
    }
}

async fn initialize(process: &mut OwnedClaudeSession) {
    if let Err(error) = process.initialize().await {
        panic!(
            "native initialize: {error:?}; stderr={}",
            String::from_utf8_lossy(&process.stderr_snapshot().tail)
        );
    }
}

async fn open_native(
    f: Fixture,
    binding: AccountBinding,
    original: SharedRuntimeCommand,
    command: SharedAttemptCommand,
    id: &str,
    native: &Native,
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
        requester_mxid: "@owner:test".into(),
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
    let mut process = native.spawn(&binding, session.resume_id());
    let sink = Arc::new(
        (*custody)
            .bind_process(process.shared_process_source().unwrap())
            .unwrap(),
    );
    initialize(&mut process).await;
    sink.record(process.qualify_shared(&binding).unwrap())
        .await
        .unwrap();
    let expires = Instant::now() + Duration::from_secs(30);
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
        expires_at_ms: graphs::now_ms().unwrap() + 30000,
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

async fn turn_native(a: &mut Active, text: &str) -> (SharedWorkReceipt, Vec<serde_json::Value>) {
    a.attach().await;
    let input =
        a.f.registry
            .prepare_shared_decrypted_input(a.authority(), a.work.clone(), text.into())
            .await
            .unwrap();
    let call =
        a.f.registry
            .prepare_shared_input_call(a.authority(), a.work.clone(), input.clone())
            .await
            .unwrap();
    assert_eq!(call.reserved_tokens, 0);
    let start =
        a.f.registry
            .begin_shared_input_start(a.authority(), a.work.clone(), call.call_id, input)
            .await
            .unwrap();
    let custody =
        a.f.registry
            .issue_shared_call(
                start,
                SharedWorkStart {
                    work: a.work.clone(),
                    work_digest: digest(&a.work),
                    activity_epoch: 1,
                    newly_started: true,
                },
            )
            .await
            .unwrap();
    custody.start_claude_turn(&mut a.process).await.unwrap();
    a.process.next_message().await.unwrap();
    let mut capture = custody.bind_started_claude_turn(&a.process).await.unwrap();
    let session = a.session.take().unwrap();
    session.bind_capture(&mut capture, &a.process).unwrap();
    let mut answer = None;
    let mut results = Vec::new();
    loop {
        let message = a.process.next_message().await.unwrap_or_else(|error| {
            panic!(
                "native turn: {error:?}; stderr={}",
                String::from_utf8_lossy(&a.process.stderr_snapshot().tail)
            )
        });
        if let Message::Event { payload, .. } = &message {
            if payload["type"] == "result" {
                if !a.process.usage_snapshot_requested() {
                    assert_eq!(payload["is_error"], false, "{payload}");
                    answer = Some(payload["result"].as_str().unwrap().to_owned());
                }
                results.push(payload.clone());
            }
        }
        capture
            .observe_claude(a.process.last_observation().unwrap())
            .await
            .unwrap();
        if a.process.phase() == Phase::ResultObserved {
            if a.process.needs_usage_snapshot() {
                a.process.request_usage_snapshot().await.unwrap();
            } else {
                break;
            }
        }
    }
    let receipt = capture
        .finish_claude(&a.process, &answer.unwrap())
        .await
        .unwrap();
    session.retain(&capture, &a.process).await.unwrap();
    (receipt, results)
}

#[tokio::test]
#[ignore = "explicit installed Claude; shared canonical store and isolated localhost model only"]
async fn installed_claude_two_shared_rounds() {
    two_shared_rounds(None).await
}
/// The same two rounds for a pool made from the owner's endpoint token: the
/// token and endpoint reach the CLI only through the injected credential, the
/// process reports that source, and a model name the table does not know runs.
#[tokio::test]
#[ignore = "explicit installed Claude; shared canonical store and isolated localhost model only"]
async fn installed_claude_two_shared_rounds_on_an_endpoint_token() {
    two_shared_rounds(Some(("offline-endpoint-token", "open-model-a"))).await
}
async fn two_shared_rounds(api: Option<(&'static str, &'static str)>) {
    let binary = PathBuf::from(
        std::env::var_os("HAGENCY_TEST_INSTALLED_CLAUDE").expect("explicit installed CLI"),
    )
    .canonicalize()
    .unwrap();
    let guardian = PathBuf::from(
        std::env::var_os("HAGENCY_TEST_CLAUDE_GUARDIAN").expect("explicit process guardian"),
    )
    .canonicalize()
    .unwrap();
    let version = std::process::Command::new(&binary)
        .arg("--version")
        .output()
        .unwrap();
    assert!(version.status.success());
    // The provider first: an API credential's identity includes its endpoint.
    let provider_root = tempfile::tempdir().unwrap();
    let fixture = provider_root.path().join("provider.py");
    std::fs::write(&fixture, include_str!("shared_claude_installed_fixture.py")).unwrap();
    let mut model = tokio::process::Command::new("python3")
        .arg(fixture)
        .arg(provider_root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(model.stdout.take().unwrap())
        .read_line(&mut line)
        .await
        .unwrap();
    let port: u16 = line.trim().parse().unwrap();
    let (f, binding) = match api {
        Some((token, name)) => {
            let binding = AccountBinding::from_api_credential(
                Some(&format!("http://127.0.0.1:{port}")),
                token,
                false,
                Instant::now() + Duration::from_secs(45),
            )
            .unwrap();
            Box::pin(super::super::super::super::claude_process::fixture_claude_bound(binding, name)).await
        }
        None => Box::pin(super::super::super::super::claude_process::fixture_claude()).await,
    };
    let root = f._temp.path().canonicalize().unwrap();
    let scope = root.join("isolated-shared");
    std::fs::create_dir(&scope).unwrap();
    std::fs::set_permissions(&scope, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["home", "config", "work", "tmp"] {
        let path = scope.join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let native = Native {
        binary,
        guardian,
        scope,
        port,
        api,
    };
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
    let mut process = native.spawn(&binding, None);
    let sink = (*custody)
        .bind_process(process.shared_process_source().unwrap())
        .unwrap();
    initialize(&mut process).await;
    sink.record(process.qualify_shared(&binding).unwrap())
        .await
        .unwrap();
    sink.record(process.stop_shared_observation().unwrap())
        .await
        .unwrap();
    let command = attempt(&f, &original).await;
    let mut a = Box::pin(open_native(
        f,
        binding,
        original,
        command,
        "installed_first",
        &native,
    ))
    .await;
    assert!(!a.session.as_ref().unwrap().is_resuming());
    let (first, first_frames) = Box::pin(turn_native(&mut a, "Remember shared blue comet")).await;
    let id = a.process.session_id().unwrap().to_owned();
    let stopped = a
        .sink
        .record(a.process.stop_shared_observation().unwrap())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    a.f.registry.shutdown().await.unwrap();
    a.f.registry = OwnerResourceRegistry::open_root(&a.f._temp.path().join("owner")).unwrap();
    let command = next_attempt(&a.f, &a.command, &stopped).await;
    a = Box::pin(open_native(
        a.f,
        a.binding,
        a.original,
        command,
        "installed_second",
        &native,
    ))
    .await;
    assert_eq!(a.session.as_ref().unwrap().resume_id(), Some(id.as_str()));
    let (second, second_frames) =
        Box::pin(turn_native(&mut a, "Continue the shared conversation")).await;
    assert_eq!(a.process.session_id(), Some(id.as_str()));
    for (index, receipt) in [&first, &second].into_iter().enumerate() {
        assert_eq!(receipt.state, "completed");
        assert!(receipt.usage_complete);
        // Existing shared accounting records fresh+write+output; cache-read is
        // retained as a distinct native count, not charged again in this total.
        assert_eq!(receipt.known_consumed_tokens, 33);
        assert_eq!(receipt.retained_tokens, Some(33));
        assert_eq!(
            receipt.body.as_deref(),
            Some(format!("Shared native reply {}", index + 1).as_str())
        );
    }
    assert_ne!(first.call_id, second.call_id);
    assert_ne!(first.process_digest, second.process_digest);
    for frames in [&first_frames, &second_frames] {
        assert_eq!(frames.len(), 1, "one native model result for this call");
        let delta = frames[0]["model_usage_since_last_result"]
            .as_object()
            .unwrap();
        assert_eq!(delta.len(), 1);
        let usage = delta.values().next().unwrap();
        for (field, count) in [
            ("inputTokens", 17),
            ("outputTokens", 9),
            ("cacheReadInputTokens", 3),
            ("cacheCreationInputTokens", 7),
        ] {
            assert_eq!(usage[field], count, "{usage}");
        }
        assert!(
            usage["costUSD"].is_number() && usage["contextWindow"].is_number(),
            "native metadata must coexist with billable counters: {usage}"
        );
    }
    let requests: serde_json::Value =
        serde_json::from_slice(&std::fs::read(provider_root.path().join("api-requests.json")).unwrap()).unwrap();
    assert_eq!(requests.as_array().unwrap().len(), 2, "{requests}");
    let credentials: serde_json::Value =
        serde_json::from_slice(&std::fs::read(provider_root.path().join("api-credentials.json")).unwrap()).unwrap();
    for (request, credential) in requests.as_array().unwrap().iter().zip(credentials.as_array().unwrap()) {
        match api {
            Some((token, name)) => {
                assert_eq!(request["model"], name, "the owner's model name is sent as it is");
                assert_eq!(credential["authorization"], format!("Bearer {token}"));
                assert!(credential["x-api-key"].is_null(), "{credential}");
            }
            None => assert_eq!(credential["authorization"], "Bearer offline-shared-contract-token"),
        }
    }
    let resumed_context = requests[1]["messages"].to_string();
    assert!(resumed_context.contains("Remember shared blue comet"));
    assert!(resumed_context.contains("Shared native reply 1"));
    assert!(resumed_context.contains("Continue the shared conversation"));
    let evidence = serde_json::json!({
        "installed_version": String::from_utf8(version.stdout).unwrap().trim(),
        "first": first, "second": second,
        "first_native_results": first_frames, "second_native_results": second_frames,
        "api_requests": requests,
        "scope": "production isolated shared spawn; canonical registry reopened between calls",
    });
    if let Some(path) = std::env::var_os("HAGENCY_TEST_CLAUDE_ARTIFACTS") {
        let path = PathBuf::from(path);
        std::fs::create_dir_all(&path).unwrap();
        std::fs::write(
            path.join("shared-native-two-rounds.json"),
            serde_json::to_vec_pretty(&evidence).unwrap(),
        )
        .unwrap();
    }
    a.stop().await;
    model.kill().await.unwrap();
    model.wait().await.unwrap();
}
