use hagency_platform::Launch;
use hagency_runtime::claude::session::ObservationKind;
use hagency_runtime::claude::session::{ApprovalControlPolicy, PermissionDecision, PreparedUpdate};
use hagency_runtime::{
    claude::{
        EventKind, Message,
        session::{Error, Limits, Phase},
    },
    owned::{Cleanup, OwnedClaudeSession, StartError},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn binary() -> PathBuf {
    env!("CARGO_BIN_EXE_hagency-runtime-probe").into()
}
fn launch(root: &Path, mode: &str, marker: &Path) -> Launch {
    let mut environment = BTreeMap::new();
    environment.insert("PATH".into(), "".into());
    if let Some(root) = std::env::var_os("SystemRoot") {
        environment.insert("SystemRoot".into(), root);
    }
    Launch {
        executable: binary(),
        arguments: vec!["fake-claude".into(), mode.into(), marker.into()],
        directory: root.into(),
        environment,
        require_crash_containment: false,
    }
}
fn limits() -> Limits {
    Limits {
        write_timeout_ms: 1000,
        event_wait_ms: 2000,
        lifetime_ms: 10_000,
    }
}
fn spawn(root: &Path, mode: &str, marker: &Path) -> OwnedClaudeSession {
    OwnedClaudeSession::spawn(&binary(), &launch(root, mode, marker), limits()).unwrap()
}
fn cleanup(value: Cleanup) {
    let Cleanup::Observed(report) = value else {
        panic!("fixture stop must be observed: {value:?}")
    };
    assert!(report.scope.leader_exited);
    assert!(report.scope.signals_accepted);
    assert_eq!(
        report.scope.whole_tree_stopped,
        cfg!(any(target_os = "linux", target_os = "macos", windows))
    );
}
fn pulse(marker: &Path) -> u64 {
    fs::metadata(marker.with_extension("pulse")).map_or(0, |value| value.len())
}
fn live(marker: &Path) {
    let before = pulse(marker);
    let until = Instant::now() + Duration::from_secs(2);
    while pulse(marker) <= before {
        assert!(Instant::now() < until, "offline peer must still be live");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn stopped(marker: &Path) {
    let before = pulse(marker);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(pulse(marker), before);
}

#[tokio::test]
async fn native_claude_account_observation_binds_original_process_and_rejects_shadowed_auth() {
    use hagency_runtime::claude::account::AccountBinding;
    let binding = AccountBinding::from_authenticated_profile(
        "01234567-89ab-cdef-0123-456789abcdef",
        "89abcdef-0123-4567-89ab-cdef01234567",
        "owner@example.test",
        Instant::now() + Duration::from_secs(45),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut first = spawn(root.path(), "account-match", &root.path().join("first"));
    let mut second = spawn(root.path(), "account-match", &root.path().join("second"));
    assert!(first.qualify_account(&binding).is_err());
    first.initialize().await.unwrap();
    second.initialize().await.unwrap();
    let receipt = first.qualify_account(&binding).unwrap();
    assert_eq!(receipt.provider_digest(), binding.provider_digest());
    assert!(first.matches_account_observation(&receipt));
    assert!(!second.matches_account_observation(&receipt));
    assert!(
        !root.path().join("first.prompt").exists(),
        "qualification must not send model input"
    );
    cleanup(first.stop());
    assert!(!first.matches_account_observation(&receipt));
    cleanup(second.stop());
    for mode in [
        "account-wrong-email",
        "account-api-key",
        "account-provider",
        "account-missing",
    ] {
        let mut runner = spawn(root.path(), mode, &root.path().join(mode));
        runner.initialize().await.unwrap();
        assert!(runner.qualify_account(&binding).is_err(), "{mode}");
        assert_eq!(
            runner.phase(),
            Phase::Ready,
            "missing shared proof must not break personal execution"
        );
        cleanup(runner.stop());
    }
}

#[tokio::test]
async fn native_claude_interrupt_foreign_or_repeated_preparation_cannot_stop_current_process() {
    let root = tempfile::tempdir().unwrap();
    let first_marker = root.path().join("first");
    let current_marker = root.path().join("current");
    let mut first = spawn(root.path(), "permission-hold", &first_marker);
    let mut current = spawn(root.path(), "permission-hold", &current_marker);
    for runner in [&mut first, &mut current] {
        runner.initialize().await.unwrap();
        runner.prompt("wait for the owner").await.unwrap();
        runner.next_message().await.unwrap();
        assert!(matches!(
            runner.next_message().await.unwrap(),
            Message::Permission { .. }
        ));
    }
    let mut foreign = first.prepare_interrupt().unwrap();
    let _original = current.prepare_interrupt().unwrap();
    assert!(matches!(current.prepare_interrupt(), Err(Error::State)));
    assert!(matches!(
        current.send_prepared_interrupt(&mut foreign).await,
        Err(Error::Identity)
    ));
    assert_eq!(current.phase(), Phase::Running);
    assert_eq!(current.cleanup(), Cleanup::Pending);
    assert!(current.termination().is_none());
    assert!(current.interrupt_receipt().is_none());
    live(&current_marker);
    cleanup(first.stop());
    cleanup(current.stop());
}

#[tokio::test]
async fn native_claude_interrupt_unacknowledged_cancellation_keeps_process_custody() {
    use hagency_runtime::claude::session::InterruptUpdate;
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("unacknowledged");
    let mut runner = spawn(root.path(), "permission-hold", &marker);
    runner.initialize().await.unwrap();
    runner.prompt("wait for the owner").await.unwrap();
    runner.next_message().await.unwrap();
    runner.next_message().await.unwrap();
    let mut interrupt = runner.prepare_interrupt().unwrap();
    assert!(matches!(
        runner
            .send_prepared_interrupt(&mut interrupt)
            .await
            .unwrap(),
        InterruptUpdate::WriteAccepted(_)
    ));
    assert!(runner.interrupt_receipt().is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), runner.next_message())
            .await
            .is_err()
    );
    assert_eq!(runner.phase(), Phase::Closed);
    assert!(runner.interrupt_receipt().is_none());
    cleanup(runner.cleanup());
    stopped(&marker);
}
#[tokio::test]
async fn native_claude_owned_usage() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("usage");
    let mut runner = spawn(root.path(), "usage", &marker);
    runner.initialize().await.unwrap();
    runner.prompt("offline usage").await.unwrap();
    runner.next_message().await.unwrap();
    let source = runner.observation_source().unwrap();
    assert!(runner.matches_observation_source(&source));
    for sequence in 2..=6 {
        runner.next_message().await.unwrap();
        let event = runner.last_observation().unwrap();
        assert_eq!(event.sequence(), sequence);
        assert!(event.source() == &source);
        match sequence {
            2 | 5 => assert!(
                matches!(event.kind(),ObservationKind::Usage(e) if e.counts().output().is_none())
            ),
            3 | 4 => assert!(matches!(event.kind(), ObservationKind::Ignored)),
            6 => assert!(
                matches!(event.kind(),ObservationKind::Result {usage,is_error:true} if usage.counts().input()==Some(101))
            ),
            _ => unreachable!(),
        }
    }
    assert_eq!(runner.cleanup(), Cleanup::Pending);
    assert!(!source.is_retired());
    live(&marker);
    cleanup(runner.stop());
    assert!(source.is_retired());
    stopped(&marker);
}
#[tokio::test]
async fn native_claude_owned_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("normal");
    let mut runner = spawn(root.path(), "normal", &marker);
    assert!(runner.id() > 1);
    runner.initialize().await.unwrap();
    runner.prompt("owned stdin 中文 $(literal)").await.unwrap();
    for expected in [EventKind::System, EventKind::Assistant, EventKind::Result] {
        assert!(
            matches!(runner.next_message().await.unwrap(),Message::Event {kind,..} if kind==expected)
        );
    }
    assert_eq!(runner.phase(), Phase::ResultObserved);
    assert_eq!(runner.session_id(), Some("owned-claude"));
    assert_eq!(runner.cleanup(), Cleanup::Pending);
    assert!(runner.termination().is_none());
    live(&marker);
    let prompt: serde_json::Value =
        serde_json::from_slice(&fs::read(marker.with_extension("prompt")).unwrap()).unwrap();
    assert_eq!(prompt["message"]["content"], "owned stdin 中文 $(literal)");
    cleanup(runner.stop());
    stopped(&marker);
    assert_eq!(runner.initialize().await, Err(Error::Closed));
}

#[tokio::test]
async fn native_claude_owned_failure_and_cancel() {
    let root = tempfile::tempdir().unwrap();
    for mode in ["malformed", "stall", "cancel", "drop"] {
        let marker = root.path().join(mode);
        let mut runner = spawn(
            root.path(),
            match mode {
                "malformed" => mode,
                "drop" => "normal",
                _ => "stall",
            },
            &marker,
        );
        match mode {
            "malformed" => assert!(matches!(runner.initialize().await, Err(Error::Protocol(_)))),
            "stall" => assert_eq!(runner.initialize().await, Err(Error::Timeout)),
            "cancel" => {
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), runner.initialize())
                        .await
                        .is_err()
                );
                assert_eq!(runner.termination().unwrap().cause, Error::Cancelled);
            }
            "drop" => {
                runner.initialize().await.unwrap();
                runner.prompt("offline drop witness").await.unwrap();
                for _ in 0..3 {
                    runner.next_message().await.unwrap();
                }
                live(&marker);
                // A live pulse after result proves this tests owner Drop, not
                // merely a peer that exited by itself before the assertion.
                drop(runner);
                stopped(&marker);
                continue;
            }
            _ => unreachable!(),
        }
        cleanup(runner.cleanup());
        stopped(&marker);
    }
    let marker = root.path().join("invalid");
    let request = launch(root.path(), "normal", &marker);
    assert!(matches!(
        OwnedClaudeSession::spawn(
            &binary(),
            &request,
            Limits {
                lifetime_ms: 0,
                ..limits()
            }
        ),
        Err(StartError::Settings)
    ));
    assert!(!marker.with_extension("entered").exists());
    let mut missing = launch(root.path(), "normal", &marker);
    missing.executable = root.path().join("absent.exe");
    assert!(matches!(
        OwnedClaudeSession::spawn(&binary(), &missing, limits()),
        Err(StartError::Uncertain { .. })
    ));
}

#[tokio::test]
async fn native_claude_owned_permission_roundtrip() {
    let root = tempfile::tempdir().unwrap();
    for mode in [
        "permission-allow",
        "permission-deny",
        "permission-cancel",
        "permission-hold",
    ] {
        let marker = root.path().join(mode);
        let mut runner = spawn(root.path(), mode, &marker);
        runner.initialize().await.unwrap();
        runner.prompt("offline permission test").await.unwrap();
        assert!(matches!(
            runner.next_message().await.unwrap(),
            Message::Event {
                kind: EventKind::System,
                ..
            }
        ));
        runner
            .enable_approval_control(ApprovalControlPolicy {
                owner_wait_ms: 3000,
                response_reserve_ms: 1000,
            })
            .unwrap();
        assert!(
            matches!(runner.next_message().await.unwrap(),Message::Permission {request_id,..} if request_id=="owned-permission")
        );
        if mode == "permission-hold" {
            live(&marker);
            let pending = std::future::pending::<()>();
            tokio::pin!(pending);
            assert!(
                tokio::time::timeout(
                    Duration::from_millis(30),
                    runner.next_or_control(pending.as_mut())
                )
                .await
                .is_err()
            );
            assert_eq!(runner.termination().unwrap().cause, Error::Cancelled);
            cleanup(runner.cleanup());
            stopped(&marker);
            continue;
        }
        let decision = if mode == "permission-deny" {
            PermissionDecision::Deny
        } else {
            PermissionDecision::Allow
        };
        let mut prepared = runner
            .prepare_approval("owned-permission", decision)
            .unwrap();
        if mode == "permission-cancel" {
            let until = Instant::now() + Duration::from_secs(2);
            while !marker.with_extension("cancelled").is_file() {
                assert!(
                    Instant::now() < until,
                    "offline cancellation marker must arrive"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(matches!(
                runner.send_prepared_approval(&mut prepared).await.unwrap(),
                PreparedUpdate::Message(Message::ControlCancel { .. })
            ));
            assert_eq!(runner.write_progress().unwrap().accepted_bytes, 0);
            assert!(matches!(
                runner.send_prepared_approval(&mut prepared).await,
                Err(Error::PermissionUnavailable)
            ));
            assert_eq!(
                runner
                    .termination()
                    .unwrap()
                    .unconfirmed_write
                    .unwrap()
                    .accepted_bytes,
                0
            );
            cleanup(runner.cleanup());
            stopped(&marker);
            continue;
        }
        // The peer answers the instant it has read the response, so on a fast
        // runner its next event can be observed before this host observes its own
        // flush (hosted Ubuntu: three runs of four). The send is re-entrant for
        // exactly that: it hands back what arrived and resumes the same frame.
        // Both orders are one exchange, so keep sending until the write is
        // acknowledged and keep what arrived first, in order.
        let mut early = std::collections::VecDeque::new();
        loop {
            match runner.send_prepared_approval(&mut prepared).await.unwrap() {
                PreparedUpdate::WriteAccepted(progress) => {
                    assert!(progress.flushed);
                    break;
                }
                PreparedUpdate::Message(message) => early.push_back(message),
            }
            assert!(
                early.len() <= 2,
                "only the two answer events may precede the receipt"
            );
        }
        for expected in [EventKind::Assistant, EventKind::Result] {
            let message = match early.pop_front() {
                Some(message) => message,
                None => runner.next_message().await.unwrap(),
            };
            assert!(matches!(message,Message::Event {kind,..} if kind==expected));
        }
        assert!(early.is_empty());
        let received: serde_json::Value =
            serde_json::from_slice(&fs::read(marker.with_extension("response")).unwrap()).unwrap();
        assert_eq!(received["response"]["request_id"], "owned-permission");
        assert_eq!(
            received["response"]["response"]["behavior"],
            if decision == PermissionDecision::Allow {
                "allow"
            } else {
                "deny"
            }
        );
        assert!(
            received["response"]["response"]
                .get("updatedPermissions")
                .is_none()
        );
        assert_eq!(runner.cleanup(), Cleanup::Pending);
        live(&marker);
        cleanup(runner.stop());
        stopped(&marker);
    }
}

fn shared_launch(root: &Path, mode: &str) -> Launch {
    use hagency_runtime::owned::SharedClaudePolicy;
    let mut value = launch(root, mode, &root.join("unused"));
    value.arguments = SharedClaudePolicy::Standard { web: false }
        .arguments("sonnet", "medium")
        .unwrap()
        .into_iter()
        .map(Into::into)
        .collect();
    for (key, value_) in [
        ("HAGENCY_OFFLINE_CLAUDE_MODE", mode),
        ("CLAUDE_CODE_EMIT_SESSION_STATE_EVENTS", "1"),
        ("CLAUDE_CODE_BG_TASKS_REPORT_RUNNING", "1"),
    ] {
        value.environment.insert(key.into(), value_.into());
    }
    value
        .environment
        .insert("CLAUDE_CONFIG_DIR".into(), root.as_os_str().into());
    value
}
async fn shared_session(root: &Path, mode: &str, until: Instant) -> OwnedClaudeSession {
    shared_session_resuming(root, mode, until, None).await
}
async fn shared_session_resuming(root: &Path, mode: &str, until: Instant, resume: Option<&str>) -> OwnedClaudeSession {
    use hagency_runtime::{claude::account::AccountBinding, owned::SharedClaudePolicy};
    let policy = SharedClaudePolicy::Standard { web: false };
    let mut launch = shared_launch(root, mode);
    launch.arguments = policy.arguments_resuming("sonnet", "medium", resume).unwrap()
        .into_iter().map(Into::into).collect();
    let mut session =
        OwnedClaudeSession::spawn_shared_resuming(&binary(), &launch, limits(), policy, resume)
            .unwrap();
    session.initialize().await.unwrap();
    let binding = AccountBinding::from_authenticated_profile(
        "01234567-89ab-cdef-0123-456789abcdef",
        "89abcdef-0123-4567-89ab-cdef01234567",
        "owner@example.test",
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    assert!(
        session.qualify_shared(&binding).is_err(),
        "qualification needs original managed custody"
    );
    session
        .shared_process_source()
        .unwrap()
        .claim_managed_custody(&"a".repeat(64), "offline-original")
        .unwrap();
    let qualification = session.qualify_shared(&binding).unwrap();
    assert_eq!(
        qualification.provider_digest(),
        Some(binding.provider_digest())
    );
    session.install_renewable_shared_activity(1, until).unwrap();
    session
}
#[tokio::test]
async fn shared_claude_resume_rejects_mismatched_launch_and_native_identity() {
    use hagency_runtime::owned::SharedClaudePolicy;
    let root = tempfile::tempdir().unwrap();
    let policy = SharedClaudePolicy::Standard { web: false };
    let mut launch = shared_launch(root.path(), "shared-session");
    assert!(OwnedClaudeSession::spawn_shared_resuming(&binary(), &launch, limits(), policy,
        Some("original-native")).is_err());
    launch.arguments = policy.arguments_resuming("sonnet", "medium", Some("original-native"))
        .unwrap().into_iter().map(Into::into).collect();
    assert!(OwnedClaudeSession::spawn_shared(&binary(), &launch, limits(), policy).is_err());
    assert!(OwnedClaudeSession::spawn_shared_resuming(&binary(), &launch, limits(), policy,
        Some("different-native")).is_err());
    for bad in ["", "../other", "--dangerously-skip-permissions", "with space", "a\nb"] {
        // Leading '-' is a valid identifier character but may never be a CLI option.
        assert!(policy.arguments_resuming("sonnet", "medium", Some(bad)).is_err(), "{bad}");
    }
    assert!(!root.path().join("shared-claude.entered").exists());
    fs::write(root.path().join("original-native.json"), b"[]").unwrap();
    for marker in ["shared-claude.wrong_identity", "shared-claude.early_permission"] {
        fs::write(root.path().join(marker), b"offline").unwrap();
        let mut session = shared_session_resuming(root.path(), "shared-session",
            Instant::now() + Duration::from_secs(5), Some("original-native")).await;
        session.prompt("new work").await.unwrap();
        assert!(matches!(session.next_message().await, Err(Error::Identity)));
        assert!(session.observation_source().is_err());
        cleanup(session.stop());
        fs::remove_file(root.path().join(marker)).unwrap();
    }
}
#[tokio::test]
async fn shared_claude_original_native_source_requires_account_policy_and_idle() {
    use hagency_runtime::owned::SharedClaudePolicy;
    let root = tempfile::tempdir().unwrap();
    let policy = SharedClaudePolicy::Standard { web: false };
    let mut wrong = shared_launch(root.path(), "shared-usage");
    wrong
        .arguments
        .push("--dangerously-skip-permissions".into());
    assert!(matches!(
        OwnedClaudeSession::spawn_shared(&binary(), &wrong, limits(), policy),
        Err(StartError::Settings)
    ));
    assert!(!root.path().join("shared-claude.entered").exists());
    let mut session = shared_session(
        root.path(),
        "shared-usage",
        Instant::now() + Duration::from_secs(5),
    )
    .await;
    let source = session.shared_process_source().unwrap();
    assert!(!root.path().join("shared-claude.prompt").exists());
    session.prompt("original input").await.unwrap();
    session.next_message().await.unwrap();
    let native_source = session.observation_source().unwrap();
    session.next_message().await.unwrap();
    assert_eq!(session.phase(), Phase::Running, "result is not native idle");
    assert!(matches!(
        session.last_observation().unwrap().kind(),
        ObservationKind::Result { .. }
    ));
    session.next_message().await.unwrap();
    assert_eq!(session.phase(), Phase::ResultObserved);
    assert!(!session.needs_usage_snapshot());
    assert!(session.matches_observation_source(&native_source));
    assert!(source.is_live());
    assert_eq!(
        session.stop_shared_observation().unwrap().state(),
        hagency_runtime::owned::ManagedProcessState::Stopped
    );
    assert!(!source.is_live());
    assert!(native_source.is_retired());
}
#[tokio::test]
async fn shared_claude_renewal_keeps_original_pending_read_until_actual_expiry() {
    let root = tempfile::tempdir().unwrap();
    let initial = Instant::now() + Duration::from_secs(2);
    let mut session = shared_session(root.path(), "shared-wait", initial).await;
    session.prompt("original pending read").await.unwrap();
    session.next_message().await.unwrap();
    let control = session.shared_activity_control().unwrap();
    // Shorten the initial lease before beginning this one pending native read.
    let old = Instant::now() + Duration::from_millis(200);
    control.renew_lease(2, old).unwrap();
    let read = session.next_message();
    tokio::pin!(read);
    tokio::select! {biased;result=&mut read=>panic!("read ended before renewal: {:?}",result.err()),_=tokio::time::sleep(Duration::from_millis(50))=>{}}
    let renewed = Instant::now() + Duration::from_millis(500);
    control.renew_lease(3, renewed).unwrap();
    tokio::select! {biased;result=&mut read=>panic!("renewal interrupted original read: {:?}",result.err()),_=tokio::time::sleep_until((old+Duration::from_millis(100)).into())=>{}}
    assert!(
        read.await.is_err(),
        "actual lease expiry closes the process"
    );
}

#[tokio::test]
async fn missing_resume_requires_original_refusal_and_whole_process_stop() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("missing");
    let mut l = launch(root.path(), "shared-session", &marker);
    l.arguments.extend(["--resume".into(), "missing-native".into()]);
    let mut process = OwnedClaudeSession::spawn(&binary(), &l, limits()).unwrap();
    process.enable_native_completion().unwrap();
    process.enable_continuous_session().unwrap();
    process.expect_resume("missing-native").unwrap();
    assert_eq!(process.missing_resume_id(), None);
    assert_eq!(process.initialize().await, Err(Error::MissingResume));
    cleanup(process.cleanup());
    assert_eq!(process.missing_resume_id(), Some("missing-native"));
    assert!(!marker.with_extension("prompt").exists());
    process.stop();
    assert_eq!(process.missing_resume_id(), Some("missing-native"));
}
