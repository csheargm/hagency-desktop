//! Original supervised Claude pipes through the production durable capture.
use super::*;
use hagency_runtime::{
    claude::{account::AccountBinding, session::Limits},
    owned::{OwnedClaudeSession, SharedClaudePolicy},
};
use sha2::{Digest, Sha256};

pub(super) async fn fixture_claude() -> (Fixture, AccountBinding) {
    let binding = AccountBinding::from_authenticated_profile(
        "01234567-89ab-cdef-0123-456789abcdef",
        "89abcdef-0123-4567-89ab-cdef01234567",
        "owner@example.test",
        Instant::now() + Duration::from_secs(45),
    )
    .unwrap();
    fixture_claude_bound(binding, "claude-opus-5").await
}
/// The same enrolled resource and grant for any binding the host observed.
pub(super) async fn fixture_claude_bound(binding: AccountBinding, model: &str) -> (Fixture, AccountBinding) {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
    let until = binding.expires();
    let provider = VerifiedProviderAccount::from_claude_binding(binding.clone()).unwrap();
    let resource = registry
        .prepare_enrollment(
            profile.clone(),
            "claude_provider".into(),
            provider,
            model.into(),
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
/// A pool made from the owner's API key or endpoint token: no sign-in folder,
/// one seat per endpoint and credential, and a model on the owner's own
/// endpoint is shared for the coding roles whatever it is called.
#[tokio::test]
async fn claude_api_credential_enrolls_without_a_folder_and_marks_the_owners_own_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
    let until = || Instant::now() + Duration::from_secs(45);
    let gateway = || {
        VerifiedProviderAccount::from_claude_binding(
            AccountBinding::from_api_credential(Some("https://gateway.example"), "token-one", false, until()).unwrap(),
        )
        .unwrap()
    };
    let enroll = |command: &str, proof: VerifiedProviderAccount, model: &str| {
        registry
            .prepare_enrollment(profile.clone(), command.into(), proof, model.into(), Some("medium".into()), 1000, until())
            .unwrap()
            .approve()
    };
    let own = enroll("own_endpoint", gateway(), "open-model-a").await.unwrap();
    let summary = registry.resource(&profile, own.resource_id.clone()).await.unwrap();
    assert_eq!((summary.framework.as_str(), summary.model.as_str()), ("claude", "open-model-a"));
    assert_eq!(summary.capabilities, ["coding", "testing", "integration", "documentation"]);
    assert!(
        registry.resource_namespace(profile.clone(), own.resource_id.clone(), summary.revision.clone()).await.unwrap().is_none(),
        "the shared launch takes a Claude resource only without a sign-in folder"
    );
    // The same command again is the same receipt; a second model is a second
    // pool on the same seat.
    let again = enroll("own_endpoint", gateway(), "open-model-a").await.unwrap();
    assert_eq!(again.resource_id, own.resource_id);
    let second = enroll("own_endpoint_second", gateway(), "open-model-b").await.unwrap();
    assert_ne!(second.resource_id, own.resource_id);
    registry.verify_provider_resource(profile.clone(), second.resource_id.clone(), gateway()).await.unwrap();
    // Another credential is not this resource's account.
    let other = VerifiedProviderAccount::from_claude_binding(
        AccountBinding::from_api_credential(Some("https://gateway.example"), "token-two", false, until()).unwrap(),
    )
    .unwrap();
    assert!(registry.verify_provider_resource(profile.clone(), own.resource_id.clone(), other).await.is_err());

    // A Console key on Anthropic's endpoint is rated by the table: a name it
    // knows keeps its own roles, and nothing marks the resource.
    let key = VerifiedProviderAccount::from_claude_binding(
        AccountBinding::from_api_credential(None, "sk-ant-api03-example", true, until()).unwrap(),
    )
    .unwrap();
    let rated = enroll("anthropic_key", key, "claude-opus-5").await.unwrap();
    let summary = registry.resource(&profile, rated.resource_id).await.unwrap();
    assert!(summary.capabilities.contains(&"architect".to_owned()), "{:?}", summary.capabilities);
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_call_captures_native_usage_and_stop_never_completes_missing_idle() {
    captured_calls(&[
        (true, false, None, false),
        (false, false, None, false),
        (true, true, None, false),
        (true, false, Some(false), false),
        (true, false, Some(true), false),
        (true, true, Some(false), false),
    ])
    .await;
}
/// A stop that the native peer accepts only after the final answer was read
/// ends nothing that answer depends on: the call completes with its body and
/// its settled usage.
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_stop_accepted_after_the_answer_still_completes_the_call() {
    captured_calls(&[(true, false, None, true)]).await;
}
/// The stream closes after the final answer was read and before native idle:
/// the call keeps that answer, and its usage stays incomplete as before.
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_CLAUDE_RUNTIME_PROBE offline supervised child"]
async fn shared_claude_answer_read_before_the_stream_closed_keeps_incomplete_usage() {
    captured_calls(&[(false, true, None, false)]).await;
}
/// Each case is `(complete, unaccounted, final_error, stop_after_answer)`.
async fn captured_calls(cases: &[(bool, bool, Option<bool>, bool)]) {
    for &(complete, unaccounted, final_error, stop_after_answer) in cases {
        let mode = match final_error {
            None if stop_after_answer => "shared-stop-after-answer",
            None => "shared-usage",
            Some(false) => "shared-followup-success",
            Some(true) => "shared-followup-error",
        };
        let answer = if final_error.is_some() {
            "shared background answer"
        } else {
            "shared offline answer"
        };
        let known = if final_error.is_some() { 32 } else { 16 };
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
            ("HAGENCY_OFFLINE_CLAUDE_MODE", mode),
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
            .prepare_current_call(|| authorization(), work.clone())
            .await
            .unwrap();
        let call_id = call.call_id.clone();
        let start = f
            .registry
            .begin_current_start(|| authorization(), work.clone(), call.call_id)
            .await
            .unwrap();
        let custody = f
            .registry
            .issue_current_call(
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
            if stop_after_answer {
                use hagency_runtime::claude::session::InterruptUpdate;
                let mut stop = process.prepare_interrupt().unwrap();
                loop {
                    match process.send_prepared_interrupt(&mut stop).await.unwrap() {
                        InterruptUpdate::WriteAccepted(_) => break,
                        InterruptUpdate::Message(_) => {
                            capture
                                .observe_claude(process.last_observation().unwrap())
                                .await
                                .unwrap();
                        }
                        InterruptUpdate::Unavailable => panic!("the turn is still running"),
                    }
                }
            }
            while process.phase() != hagency_runtime::claude::session::Phase::ResultObserved {
                process.next_message().await.unwrap();
                capture
                    .observe_claude(process.last_observation().unwrap())
                    .await
                    .unwrap();
            }
            assert_eq!(
                process.completion_receipt().unwrap().is_error(),
                final_error.unwrap_or(false)
            );
            let receipt = if unaccounted {
                let proof = process.completion_receipt().unwrap();
                process.stop();
                assert!(
                    capture
                        .finish_claude_unaccounted(&proof, "different answer")
                        .await
                        .is_err()
                );
                capture
                    .finish_claude_unaccounted(&proof, answer)
                    .await
                    .unwrap()
            } else {
                capture.finish_claude(&process, answer).await.unwrap()
            };
            assert_eq!(receipt.usage_complete, !unaccounted);
            if stop_after_answer {
                assert!(process.interrupt_receipt().unwrap().accepted());
                assert_eq!(receipt.known_consumed_tokens, known);
                assert_eq!(
                    (receipt.state.as_str(), receipt.body.as_deref()),
                    ("completed", Some(answer)),
                    "a stop accepted after the final answer keeps the completed answer"
                );
            }
            assert_eq!(
                receipt.body.as_deref(),
                if final_error == Some(true) {
                    None
                } else {
                    Some(answer)
                }
            );
            assert_eq!(receipt.known_consumed_tokens, known);
            assert_eq!(
                receipt.state,
                if final_error == Some(true) {
                    "failed"
                } else {
                    "completed"
                }
            );
        } else {
            assert!(process.completion_receipt().is_err());
            assert!(capture.finish_claude(&process, "premature").await.is_err());
            if unaccounted {
                assert!(
                    process.closed_answer_receipt().is_err(),
                    "an open stream continues to native idle instead"
                );
                process.stop();
                let proof = process.closed_answer_receipt().unwrap();
                assert!(
                    capture
                        .finish_claude_unaccounted(&proof, "different answer")
                        .await
                        .is_err()
                );
                let receipt = capture
                    .finish_claude_unaccounted(&proof, answer)
                    .await
                    .unwrap();
                assert_eq!(
                    (receipt.state.as_str(), receipt.body.as_deref()),
                    ("completed", Some(answer))
                );
                assert!(!receipt.usage_complete);
                assert_eq!(receipt.known_consumed_tokens, known);
            } else {
                capture.record_unknown().await.unwrap();
            }
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
        assert_eq!(history.known_consumed_tokens, known);
        assert_eq!(history.usage_complete, complete && !unaccounted);
        assert_eq!(
            history.state,
            if complete && !unaccounted {
                "settled"
            } else {
                "unknown"
            }
        );
        f.registry.shutdown().await.unwrap();
    }
}
