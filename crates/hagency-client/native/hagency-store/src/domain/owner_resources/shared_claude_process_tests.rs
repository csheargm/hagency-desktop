//! Original supervised Claude pipes through the production durable capture.
use super::*;
use hagency_runtime::{
    claude::{account::AccountBinding, session::Limits},
    owned::{OwnedClaudeSession, SharedClaudePolicy},
};
use sha2::{Digest, Sha256};

async fn fixture_claude() -> (Fixture, AccountBinding) {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
    let until = Instant::now() + Duration::from_secs(45);
    let binding = AccountBinding::from_authenticated_profile(
        "01234567-89ab-cdef-0123-456789abcdef",
        "89abcdef-0123-4567-89ab-cdef01234567",
        "owner@example.test",
        until,
    )
    .unwrap();
    let provider = VerifiedProviderAccount::from_claude_binding(binding.clone()).unwrap();
    let resource = registry
        .prepare_enrollment(
            profile.clone(),
            "claude_provider".into(),
            provider,
            "claude-opus-5".into(),
            Some("medium".into()),
            1000,
            until,
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
    (
        Fixture {
            _temp: temp,
            registry,
            profile,
            resource,
            grant,
            project,
            receipt,
        },
        binding,
    )
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_call_captures_native_usage_and_stop_never_completes_missing_idle() {
    for complete in [true, false] {
        let (f, binding) = Box::pin(fixture_claude()).await;
        let mut command = command(&f).await;
        // Legacy metadata cannot silently claim this resource as Codex.
        assert!(
            f.registry
                .claim_shared_runtime(proof(&f, command.clone()))
                .await
                .is_err()
        );
        command.version = 4;
        command.capability = SHARED_CLAUDE_CAPABILITY.into();
        command.policy = SHARED_CLAUDE_POLICY.into();
        let SharedRuntimeClaim::Issued(custody) = f
            .registry
            .claim_shared_runtime(proof(&f, command.clone()))
            .await
            .unwrap()
        else {
            panic!("fresh original custody")
        };
        let root = f._temp.path().join("native");
        std::fs::create_dir(&root).unwrap();
        let binary = PathBuf::from(
            std::env::var_os("HAGENCY_TEST_CLAUDE_RUNTIME_PROBE").expect("offline runtime fixture"),
        );
        let policy = SharedClaudePolicy::Text;
        let mut environment = std::collections::BTreeMap::new();
        environment.insert("CLAUDE_CONFIG_DIR".into(), root.as_os_str().into());
        for (key, value) in [
            ("HAGENCY_OFFLINE_CLAUDE_MODE", "shared-usage"),
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
                .arguments("claude-opus-5", "medium")
                .unwrap()
                .into_iter()
                .map(Into::into)
                .collect(),
            directory: root,
            environment,
            require_crash_containment: false,
        };
        let mut process = OwnedClaudeSession::spawn_shared(
            &binary,
            &launch,
            Limits {
                write_timeout_ms: 1000,
                event_wait_ms: 2000,
                lifetime_ms: 10000,
            },
            policy,
        )
        .unwrap();
        let sink = Arc::new(
            (*custody)
                .bind_process(process.shared_process_source().unwrap())
                .unwrap(),
        );
        process.initialize().await.unwrap();
        sink.record(process.qualify_shared(&binding).unwrap())
            .await
            .unwrap();
        let until = Instant::now() + Duration::from_secs(8);
        process.install_shared_activity(1, until, until).unwrap();
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
        let authorization = || {
            sink.authorize_activity(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                &lease,
                until,
            )
            .unwrap()
        };
        let body = "original admitted Claude question".to_owned();
        let work = SharedWorkItem {
            room_binding: None,
            encrypted: false,
            id: "claude_work".into(),
            assignment_id: command.assignment_id.clone(),
            runtime_command_digest: digest(&command),
            event_id: "$claude_event".into(),
            room_id: command.reserve_command.room_id.clone(),
            requester_mxid: command.reserve_command.manager_mxid.clone(),
            thread_root: "$claude_event".into(),
            body_digest: format!("{:x}", Sha256::digest(body.as_bytes())),
            body,
            execution_id: "claude_execution".into(),
            created_at_ms: graphs::now_ms().unwrap(),
        };
        f.registry
            .receive_shared_work(authorization(), work.clone())
            .await
            .unwrap();
        let call = f
            .registry
            .prepare_shared_call(authorization(), work.clone())
            .await
            .unwrap();
        let call_id = call.call_id.clone();
        let start = f
            .registry
            .begin_shared_start(authorization(), work.clone(), call.call_id)
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
        custody.start_claude_turn(&mut process).await.unwrap();
        process.next_message().await.unwrap();
        let mut capture = custody.bind_started_claude_turn(&process).await.unwrap();
        process.next_message().await.unwrap();
        let pending = capture
            .observe_claude(process.last_observation().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(
            !pending.usage_complete,
            "result alone never closes native background work"
        );
        // Existing billing units exclude cache reads: 10 input + 2 output + 4 cache writes.
        assert_eq!(pending.known_consumed_tokens, 16);
        if complete {
            process.next_message().await.unwrap();
            capture
                .observe_claude(process.last_observation().unwrap())
                .await
                .unwrap();
            let receipt = capture
                .finish_claude(&process, "shared offline answer")
                .await
                .unwrap();
            assert!(receipt.usage_complete);
            assert_eq!(receipt.known_consumed_tokens, 16);
            assert_eq!(receipt.state, "completed");
        } else {
            assert!(capture.finish_claude(&process, "premature").await.is_err());
            capture.record_unknown().await.unwrap();
        }
        sink.record(process.stop_shared_observation().unwrap())
            .await
            .unwrap();
        if !complete {
            let cleanup = f
                .registry
                .shared_cleanup_authority(
                    enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                    command.clone(),
                )
                .await
                .unwrap();
            let late = f
                .registry
                .record_late_managed_usage(
                    cleanup,
                    call_id.clone(),
                    ManagedUsageObservation::fixture(
                        "late_missing_final",
                        None,
                        true,
                        false,
                        false,
                    ),
                )
                .await
                .unwrap();
            assert_eq!(late.known_consumed_tokens, 16);
            assert!(
                !late.usage_complete,
                "a late incomplete receipt after stop is still incomplete"
            );
            assert_eq!(late.state, "unknown");
        }
        let cleanup = f
            .registry
            .shared_cleanup_authority(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                command.clone(),
            )
            .await
            .unwrap();
        let history = f
            .registry
            .managed_call_history(cleanup, call_id)
            .await
            .unwrap();
        assert_eq!(history.known_consumed_tokens, 16);
        assert_eq!(history.usage_complete, complete);
        assert_eq!(history.state, if complete { "settled" } else { "unknown" });
        f.registry.shutdown().await.unwrap();
    }
}
