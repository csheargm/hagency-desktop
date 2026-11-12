#![cfg(unix)]
use hagency_agent_local::codex::{self, BudgetMode, Error, Profile};
use hagency_agent_local::runtime::*;
use hagency_agent_local::{
    Budget, Layer, Ledger, Limit, Period, Policy, RequestPolicy, Scope, ToolPolicy,
};
use hagency_runtime::octos;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::{fs, path::PathBuf};

#[path = "runtime/continuous.rs"]
mod continuous;
#[path = "runtime/questions.rs"]
mod questions;
#[path = "runtime/postpaid.rs"]
mod postpaid;
#[path = "runtime/room.rs"]
mod room;
#[path = "runtime/controls.rs"]
mod controls;
#[path = "runtime/steer.rs"]
mod steer;
#[path = "runtime/long_stream.rs"]
mod long_stream;
#[path = "runtime/queued_cancel.rs"]
mod queued_cancel;
#[path = "runtime/answer_kept.rs"]
mod answer_kept;
#[path = "runtime/future_frames.rs"]
mod future_frames;
#[path = "runtime/resume_kept.rs"]
mod resume_kept;
#[path = "runtime/owner_command.rs"]
mod owner_command;

struct Fixture {
    root: tempfile::TempDir,
    ledger: Ledger,
    profile: Profile,
    reference: String,
    scope: Scope,
}
impl Fixture {
    fn new(kind: Kind, mode: &str, policy: ToolPolicy) -> Self {
        Self::called_by(kind, mode, policy, "@member:test")
    }
    fn called_by(kind: Kind, mode: &str, policy: ToolPolicy, requester: &str) -> Self {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap();
        for name in ["home", "provider", "work", "outside"] {
            fs::create_dir(directory.join(name)).unwrap();
        }
        let binary = directory.join("actor");
        let script = include_str!("fixture.py")
            .replace("__MODE__", &serde_json::to_string(mode).unwrap())
            .replace("__KIND__", &serde_json::to_string(kind.name()).unwrap())
            .replace("__ROOT__", &serde_json::to_string(&directory).unwrap());
        fs::write(&binary, script).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let profile = Profile {
            executable: binary,
            home: directory.join("home"),
            codex_home: directory.join("provider"),
            cwd: directory.join("work"),
            model: "test-model".into(),
            effort: if kind == Kind::Octos {
                "none"
            } else {
                "medium"
            }
            .into(),
            shared_auth: true,
            web_search: true,
        };
        let primary = octos::ProfileModel {
            family: "offline".into(),
            model: profile.model.clone(),
        };
        let reference = if kind == Kind::Octos {
            fs::create_dir(profile.codex_home.join("profiles")).unwrap();
            fs::write(profile.codex_home.join("profiles/coding.json"),json!({"id":"coding","config":{"llm":{"primary":{"family_id":"offline","model_id":"test-model"}}}}).to_string()).unwrap();
            local_reference(kind, &profile.codex_home, Some(("coding", &primary)))
        } else {
            local_reference(kind, &profile.codex_home, None)
        };
        let scope = Scope {
            agent: "agent".into(),
            binding: "binding".into(),
            room: "!room:test".into(),
            requester: requester.into(),
            thread: "thread".into(),
        };
        let mut ledger = Ledger::open(directory.join("ledger"), "@owner:test").unwrap();
        ledger.register_binding("@owner:test", &scope).unwrap();
        ledger
            .set_agent_policy(
                "@owner:test",
                "agent",
                0,
                &Policy {
                    budget: Budget {
                        limit: Limit::Unlimited,
                        period: Period::Lifetime,
                    },
                    requests: RequestPolicy::Allow,
                    high_risk: policy.clone(),
                },
            )
            .unwrap();
        for layer in [Layer::Room, Layer::Requester] {
            ledger
                .set_policy(
                    "@owner:test",
                    &scope,
                    layer,
                    0,
                    &Policy {
                        budget: Budget {
                            limit: Limit::Unlimited,
                            period: Period::Lifetime,
                        },
                        requests: RequestPolicy::Allow,
                        high_risk: policy.clone(),
                    },
                )
                .unwrap();
        }
        Self {
            root,
            ledger,
            profile,
            reference,
            scope,
        }
    }
    async fn process(&mut self) -> Process {
        let guardian = std::env::var_os("HAGENCY_TEST_GUARDIAN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hagency-local-probe")));
        let mut process = spawn_for_scope_with_guardian(
            &self.profile,
            &self.reference,
            &mut self.ledger,
            &self.scope,
            Some(&guardian),
        )
        .await
        .unwrap();
        if let Err(error) = process.session.initialize().await {
            let stderr = match &process.session {
                hagency_agent_local::runtime::Session::Claude { wire, .. } => {
                    String::from_utf8_lossy(&wire.stderr_snapshot().tail).into_owned()
                }
                _ => String::new(),
            };
            panic!("offline initialize failed: {error:?}; native stderr={stderr}");
        }
        process.session.verify_host_environment().await.unwrap();
        process.session.require_local_account().await.unwrap();
        process
    }
    fn path(&self, name: &str) -> PathBuf {
        self.root.path().canonicalize().unwrap().join(name)
    }
    fn spent(&self) -> (u64, u64) {
        self.ledger
            .account(&self.scope, Layer::Agent, Period::Lifetime, now())
            .unwrap()
    }
}
#[tokio::test]
async fn both_runtimes_settle_cache_inclusive_totals_and_preserve_context_after_reopen() {
    for kind in [Kind::Claude, Kind::Octos] {
        let mut f = Fixture::new(kind, "normal", ToolPolicy::Deny);
        for call in ["first", "second"] {
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (queue, _) = codex::approval_queue(1).unwrap();
            let reply = process
                .session
                .run_with_started(
                    &mut f.ledger,
                    "@owner:test",
                    &f.scope,
                    call,
                    call,
                    "hello",
                    BudgetMode::Postpaid,
                    &queue,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(reply.text, "offline reply");
            assert_eq!(reply.usage.input, 17);
            assert_eq!(reply.usage.output, 7);
            process.stop().await.unwrap();
            f.ledger = Ledger::open(f.path("ledger"), "@owner:test").unwrap();
        }
        assert_eq!(f.spent(), (48, 0));
        let argv = fs::read_to_string(f.path("argv")).unwrap();
        if kind == Kind::Claude {
            assert!(argv.contains("--resume"));
            assert!(!argv.contains("--strict-mcp-config"));
            assert!(!argv.contains("disableAllHooks"));
            assert!(!argv.contains("--disable-slash-commands"));
            assert!(argv.contains("--include-partial-messages"));
        }
        let frames = fs::read_to_string(f.path("frames")).unwrap();
        if kind == Kind::Octos {
            assert!(frames.contains("read_only"));
            assert!(frames.contains("on-request"));
        }
    }
}
#[tokio::test]
async fn claude_resumed_model_totals_charge_each_turn_and_subagent_once() {
    for mode in ["cumulative", "mixed_usage"] {
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::Deny);
        for call in ["first", "second", "third"] {
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (queue, _) = codex::approval_queue(1).unwrap();
            let reply = process
                .session
                .run_with_started(
                    &mut f.ledger,
                    "@owner:test",
                    &f.scope,
                    call,
                    call,
                    "hello",
                    BudgetMode::Postpaid,
                    &queue,
                    None,
                )
                .await
                .unwrap();
            if mode == "mixed_usage" && call == "first" {
                assert!(
                    !reply.usage.is_reported(),
                    "main-loop-only result is incomplete"
                );
            } else {
                assert!(reply.usage.is_reported());
                assert_eq!(reply.usage.input + reply.usage.output, 54);
            }
            process.stop().await.unwrap();
            f.ledger = Ledger::open(f.path("ledger"), "@owner:test").unwrap();
        }
        assert_eq!(
            f.spent(),
            if mode == "cumulative" {
                (162, 0)
            } else {
                (108, 0)
            }
        );
    }
}
#[tokio::test]
async fn claude_background_usage_is_settled_on_its_original_call_before_resume() {
    let mut f = Fixture::new(Kind::Claude, "background_usage", ToolPolicy::Deny);
    for (call, spent) in [("first", 64), ("second", 128)] {
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                call,
                call,
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(reply.text, "offline reply");
        assert_eq!(reply.usage.input + reply.usage.output, 64);
        assert!(f.path("background-completed").exists());
        assert_eq!(f.spent(), (spent, 0));
        process.stop().await.unwrap();
        f.ledger = Ledger::open(f.path("ledger"), "@owner:test").unwrap();
    }
    let frames = fs::read_to_string(f.path("frames")).unwrap();
    assert_eq!(
        frames
            .lines()
            .filter(|line| line.contains("/usage"))
            .count(),
        2
    );
}

#[tokio::test]
async fn claude_background_invalid_snapshot_keeps_the_answer_without_inventing_usage() {
    for mode in [
        "background_wrong_snapshot",
        "background_regressing",
        "background_snapshot_eof",
    ] {
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::Deny);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let reply = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "first",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            ),
        )
        .await
        .unwrap();
        let reply = reply.unwrap();
        assert_eq!(reply.text, "offline reply");
        assert!(!reply.usage.is_reported());
        assert_eq!(f.spent(), (0, 0));
        assert_eq!(
            f.ledger.outstanding_calls("@owner:test").unwrap()[0].state,
            "unknown"
        );
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn claude_background_tools_after_main_reply_still_wait_for_the_owner() {
    for allow in [false, true] {
        let mut f = Fixture::new(Kind::Claude, "background_approval", ToolPolicy::AskOwner);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, mut ui) = codex::approval_queue(1).unwrap();
        let effect = f.path("child-effect");
        let expected_scope = f.scope.clone();
        let owner = async {
            let pending = tokio::time::timeout(std::time::Duration::from_secs(5), ui.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(!effect.exists());
            assert_eq!(pending.proposal.scope, expected_scope);
            assert_eq!(pending.proposal.tool, "claude.Bash");
            pending.decide(allow);
        };
        let turn = process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "first",
            "dispatch",
            "hello",
            BudgetMode::Postpaid,
            &queue,
            None,
        );
        let (reply, ()) = tokio::join!(turn, owner);
        assert_eq!(reply.unwrap().text, "offline reply");
        assert_eq!(effect.exists(), allow);
        assert_eq!(f.spent(), (64, 0));
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn claude_same_process_counter_regression_keeps_answer_with_unknown_usage() {
    let mut f = Fixture::new(
        Kind::Claude,
        "continuous:regressing_usage",
        ToolPolicy::Deny,
    );
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    for call in ["first", "second"] {
        if call == "second" {
            process.session.park();
            process
                .session
                .continue_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
        }
        let (queue, _) = codex::approval_queue(1).unwrap();
        let result = process
            .session
            .run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                call,
                call,
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            )
            .await
            .unwrap();
        assert_eq!(result.text, "offline reply");
        assert_eq!(result.usage.is_reported(), call == "first");
    }
    assert!(!process.session.can_continue());
    process.stop().await.unwrap();
    assert_eq!(f.spent(), (54, 0));
    assert_eq!(
        f.ledger.outstanding_calls("@owner:test").unwrap()[0].state,
        "unknown"
    );
}
#[tokio::test]
async fn octos_turn_usage_followed_by_session_usage_never_charges_previous_turn_twice() {
    for kind in [Kind::Octos] {
        let mut f = Fixture::new(kind, "mixed_usage", ToolPolicy::Deny);
        for call in ["first", "second"] {
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (queue, _) = codex::approval_queue(1).unwrap();
            let reply = process
                .session
                .run_with_started(
                    &mut f.ledger,
                    "@owner:test",
                    &f.scope,
                    call,
                    call,
                    "hello",
                    BudgetMode::Postpaid,
                    &queue,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(reply.text, "offline reply");
            assert_eq!(reply.usage.input, 17);
            assert_eq!(reply.usage.output, 7);
            process.stop().await.unwrap();
            f.ledger = Ledger::open(f.path("ledger"), "@owner:test").unwrap();
        }
        assert_eq!(f.spent(), (48, 0));
        let argv = fs::read_to_string(f.path("argv")).unwrap();
        if kind == Kind::Claude {
            assert!(argv.contains("--resume"));
            assert!(argv.contains("--strict-mcp-config"));
            assert!(argv.contains("disableAllHooks"));
        }
        let frames = fs::read_to_string(f.path("frames")).unwrap();
        if kind == Kind::Octos {
            assert!(frames.contains("read_only"));
            assert!(frames.contains("on-request"));
        }
    }
}
#[tokio::test]
async fn tool_denial_and_exact_owner_verdict_control_real_fixture_effects() {
    for kind in [Kind::Claude, Kind::Octos] {
        for verdict in [false, true] {
            let mut f = Fixture::new(kind, "approval", ToolPolicy::AskOwner);
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (queue, mut ui) = codex::approval_queue(1).unwrap();
            let expected_scope = f.scope.clone();
            let effect = f.path("effect");
            let outside = f.path("outside");
            let owner = async {
                let pending = tokio::time::timeout(std::time::Duration::from_secs(15), ui.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(pending.proposal.scope, expected_scope);
                assert!(!effect.exists());
                assert_eq!(
                    pending.proposal.canonical_directory,
                    outside.to_string_lossy()
                );
                pending.decide(verdict);
            };
            let turn = process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "call",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            );
            let (result, ()) = tokio::join!(turn, owner);
            let result = result.unwrap();
            assert_eq!(result.tool_requests, 1);
            assert_eq!(result.tool_returns, 1);
            assert_eq!(f.path("effect").exists(), verdict);
            assert_eq!(f.spent(), (24, 0));
            process.stop().await.unwrap();
        }
        let mut f = Fixture::new(kind, "approval", ToolPolicy::Deny);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, mut ui) = codex::approval_queue(1).unwrap();
        process
            .session
            .run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "call",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            )
            .await
            .unwrap();
        assert!(ui.try_recv().is_err());
        assert!(!f.path("effect").exists());
        process.stop().await.unwrap();
    }
}
#[tokio::test]
async fn cancelled_callbacks_do_not_send_late_verdicts_or_lose_terminal_usage() {
    for kind in [Kind::Claude, Kind::Octos] {
        let mut f = Fixture::new(kind, "cancel", ToolPolicy::AskOwner);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, mut ui) = codex::approval_queue(1).unwrap();
        process
            .session
            .run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "call",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            )
            .await
            .unwrap();
        if let Ok(pending) = ui.try_recv() {
            assert!(pending.is_closed());
            pending.decide(true);
        }
        assert!(!f.path("decision").exists());
        assert!(!f.path("effect").exists());
        assert_eq!(f.spent(), (24, 0));
        process.stop().await.unwrap();
    }
}
#[tokio::test]
async fn missing_usage_stays_unknown_and_failed_known_usage_is_charged() {
    for kind in [Kind::Claude, Kind::Octos] {
        for mode in ["unknown", "failed"] {
            let mut f = Fixture::new(kind, mode, ToolPolicy::Deny);
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (queue, _) = codex::approval_queue(1).unwrap();
            let result = process
                .session
                .run_with_started(
                    &mut f.ledger,
                    "@owner:test",
                    &f.scope,
                    "call",
                    "dispatch",
                    "hello",
                    BudgetMode::Postpaid,
                    &queue,
                    None,
                )
                .await;
            if mode == "unknown" {
                let completed = result.unwrap();
                assert_eq!(completed.usage.input, 0);
                assert_eq!(completed.usage.output, 0);
                assert!(!completed.usage.is_reported());
                assert_eq!(f.spent(), (0, 0));
                assert_eq!(
                    f.ledger
                        .reserve(&f.scope, "new-call", "new-dispatch", 100, now())
                        .unwrap(),
                    hagency_agent_local::Reservation::New
                );
            } else {
                assert!(matches!(result, Err(Error::Failed)));
                assert_eq!(f.spent(), (24, 0));
                assert!(
                    f.ledger
                        .outstanding_calls("@owner:test")
                        .unwrap()
                        .is_empty()
                );
            }
            process.stop().await.unwrap();
        }
    }
}
#[tokio::test]
async fn cancelling_running_model_keeps_unknown_usage_after_process_stop() {
    for kind in [Kind::Claude, Kind::Octos] {
        let mut f = Fixture::new(kind, "hold", ToolPolicy::AskOwner);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, mut ui) = codex::approval_queue(1).unwrap();
        {
            let turn = process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "call",
                "dispatch",
                "hello",
                BudgetMode::Postpaid,
                &queue,
                None,
            );
            tokio::pin!(turn);
            tokio::select! {pending=ui.recv()=>assert!(pending.is_some()),result=&mut turn=>panic!("unexpected result: {result:?}")};
        }
        assert_eq!(
            f.ledger.outstanding_calls("@owner:test").unwrap()[0].state,
            "unknown"
        );
        assert_eq!(f.spent(), (0, 0));
        process.stop().await.unwrap();
        assert!(!f.path("effect").exists());
        if kind == Kind::Claude {
            // Even a first turn interrupted during approval keeps its native
            // conversation, while its original call remains unrepeatable.
            f.ledger = Ledger::open(f.path("ledger"), "@owner:test").unwrap();
            let mut next = f.process().await;
            let argv = fs::read_to_string(f.path("argv")).unwrap();
            let last: Vec<String> = serde_json::from_str(argv.lines().last().unwrap()).unwrap();
            assert!(
                last.windows(2)
                    .any(|pair| pair == ["--resume", "offline-session"])
            );
            next.stop().await.unwrap();
            assert_eq!(f.spent(), (0, 0));
        }
    }
}
#[tokio::test]
async fn different_room_or_requester_never_resumes_another_context() {
    let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "hello",
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    process.stop().await.unwrap();
    f.scope.requester = "@another:test".into();
    let mut process = f.process().await;
    process.stop().await.unwrap();
    let invocations = fs::read_to_string(f.path("argv")).unwrap();
    let last: Vec<String> = serde_json::from_str(invocations.lines().last().unwrap()).unwrap();
    assert!(!last.contains(&"--resume".to_owned()));
}

#[tokio::test]
async fn native_claude_extension_tools_require_the_same_exact_owner_decision() {
    for tool in ["Agent", "Skill", "WebFetch", "mcp__owner_server__publish"] {
        for allow in [false, true] {
            let mut f = Fixture::new(
                Kind::Claude,
                &format!("approval:{tool}"),
                ToolPolicy::AskOwner,
            );
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (queue, mut ui) = codex::approval_queue(1).unwrap();
            let expected_scope = f.scope.clone();
            let effect = f.path("effect");
            let owner = async {
                let pending = tokio::time::timeout(std::time::Duration::from_secs(15), ui.recv())
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(pending.proposal.scope, expected_scope);
                assert_eq!(pending.proposal.tool, format!("claude.{tool}"));
                assert!(!effect.exists());
                pending.decide(allow);
            };
            let turn = process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "call",
                "dispatch",
                "use the configured native tool",
                BudgetMode::Postpaid,
                &queue,
                None,
            );
            let (result, ()) = tokio::join!(turn, owner);
            assert_eq!(result.unwrap().tool_requests, 1);
            assert_eq!(f.path("effect").exists(), allow);
            assert_eq!(f.spent(), (24, 0));
            process.stop().await.unwrap();
        }
    }
}
#[test]
fn octos_profile_model_or_provider_changes_invalidate_saved_reference() {
    let f = Fixture::new(Kind::Octos, "normal", ToolPolicy::Deny);
    validate(&f.profile, &f.reference).unwrap();
    fs::write(f.profile.codex_home.join("profiles/coding.json"),json!({"id":"coding","config":{"llm":{"primary":{"family_id":"other-provider","model_id":"test-model"}}}}).to_string()).unwrap();
    assert!(validate(&f.profile, &f.reference).is_err());
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[tokio::test]
async fn claude_missing_native_context_rebuilds_once_without_replaying_or_rewriting_usage() {
    // Only the native missing-context diagnostic rebuilds. Every other failed
    // startup keeps the saved session: see `resume_kept`.
    for failure in ["deleted"] {
        let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
        for call in ["before_missing", "after_missing"] {
            if call == "after_missing" {
                fs::write(f.path("missing-context"), failure).unwrap();
            }
            let mut process = f.process().await;
            process
                .session
                .open_context(&mut f.ledger, &f.scope, &f.profile)
                .await
                .unwrap();
            let (queue, _) = codex::approval_queue(1).unwrap();
            let reply = process
                .session
                .run_with_started(
                    &mut f.ledger,
                    "@owner:test",
                    &f.scope,
                    call,
                    call,
                    call,
                    BudgetMode::Postpaid,
                    &queue,
                    None,
                )
                .await
                .unwrap();
            assert!(reply.usage.is_reported());
            process.stop().await.unwrap();
        }
        let argv = fs::read_to_string(f.path("argv")).unwrap();
        let launches: Vec<serde_json::Value> = argv
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(launches.len(), 3);
        assert!(
            launches[1]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "--resume")
        );
        assert!(
            !launches[2]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "--resume")
        );
        let frames = fs::read_to_string(f.path("frames")).unwrap();
        let prompts: Vec<serde_json::Value> = frames
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .filter(|v: &serde_json::Value| v["type"] == "user")
            .collect();
        assert_eq!(prompts.len(), 2);
        assert_eq!(prompts[0]["message"]["content"], "before_missing");
        assert_eq!(prompts[1]["message"]["content"], "after_missing");
        assert_eq!(f.spent(), (48, 0));
    }
}

#[tokio::test]
async fn claude_invalid_numeric_total_keeps_completed_answer_and_unknown_accounting() {
    let mut f = Fixture::new(Kind::Claude, "overflow_usage", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let result = process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "overflow",
            "overflow",
            "hello",
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result.text, "offline reply");
    assert_eq!(result.status, codex::TurnStatus::Completed);
    assert!(!result.usage.is_reported());
    assert!(!process.session.can_continue());
    assert_eq!(f.spent(), (0, 0));
    assert_eq!(
        f.ledger.outstanding_calls("@owner:test").unwrap()[0].state,
        "unknown"
    );
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_unsupported_version_is_actionable_without_prompt_or_fresh_retry() {
    let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "first",
            "first",
            "original",
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    process.stop().await.unwrap();
    fs::write(f.path("cli-version"), "2.1.321").unwrap();
    let guardian = std::path::Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let error = spawn_for_scope_with_guardian(
        &f.profile,
        &f.reference,
        &mut f.ledger,
        &f.scope,
        Some(guardian),
    )
    .await
    .err()
    .expect("unsupported CLI");
    assert!(matches!(error, Error::Protocol(message) if message.contains("2.1.322")));
    assert_eq!(
        fs::read_to_string(f.path("argv")).unwrap().lines().count(),
        2
    );
    let prompts = fs::read_to_string(f.path("frames"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|frame| frame["type"] == "user")
        .count();
    assert_eq!(
        prompts, 1,
        "unsupported resumed CLI receives no model prompt"
    );
    assert_eq!(f.spent(), (24, 0));
}

#[tokio::test]
async fn claude_personal_auth_and_launch_select_the_same_native_config_namespace() {
    for native_default in [false, true] {
        let mut f = Fixture::new(Kind::Claude, "normal", ToolPolicy::Deny);
        if native_default {
            let path = f.profile.home.join(".claude");
            fs::create_dir(&path).unwrap();
            f.profile.codex_home = path.canonicalize().unwrap();
            f.reference = local_reference(Kind::Claude, &f.profile.codex_home, None);
        }
        let expected = if native_default && std::env::var_os("CLAUDE_CONFIG_DIR").is_none() {
            "<native-default>".to_owned()
        } else {
            f.profile.codex_home.to_string_lossy().into_owned()
        };
        let mut process = f.process().await;
        assert_eq!(fs::read_to_string(f.path("auth-config")).unwrap(), expected);
        assert_eq!(
            fs::read_to_string(f.path("launch-config")).unwrap(),
            expected
        );
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn claude_native_background_followup_keeps_final_reply_and_complete_usage() {
    let mut f = Fixture::new(Kind::Claude, "background_native_followup", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let done = process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "call",
            "start native background work",
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    assert_eq!(done.text, "native background final answer");
    assert!(done.usage.is_reported());
    assert_eq!(done.usage.input + done.usage.output, 64);
    assert_eq!(f.spent(), (64, 0));
    let frames: Vec<serde_json::Value> = fs::read_to_string(f.path("frames"))
        .unwrap()
        .lines()
        .map(|v| serde_json::from_str(v).unwrap())
        .collect();
    assert_eq!(
        frames.iter().filter(|f| f["type"] == "user").count(),
        1,
        "the admitted native child supplies its own followup; host never replays a model prompt"
    );
    process.stop().await.unwrap();
}
