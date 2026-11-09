#![cfg(unix)]
use hagency_agent_local::{
    Budget, Layer, Ledger, Limit, Period, Policy, RequestPolicy, Scope, ToolPolicy,
    codex::{self, BudgetMode, Profile},
    runtime,
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, BufReader};

#[path = "codex_native_tree/room.rs"]
mod room;
#[path = "codex_native_tree/shared_room.rs"]
mod shared_room;

#[tokio::test]
#[ignore = "requires explicit installed Codex; local Responses fixture only, no account or paid model"]
async fn native_codex_continuous_turns_and_cold_resume_settle_original_response_receipts() {
    let (_tmp, profile, mut model) = native_fixture().await;
    let root = _tmp.path().canonicalize().unwrap();
    let scope = Scope {
        agent: "agent".into(),
        binding: "binding".into(),
        room: "!room:test".into(),
        requester: "@member:test".into(),
        thread: "root".into(),
    };
    let mut ledger = Ledger::open(root.join("ledger"), "@owner:test").unwrap();
    ledger.register_binding("@owner:test", &scope).unwrap();
    let policy = Policy {
        budget: Budget {
            limit: Limit::Unlimited,
            period: Period::Lifetime,
        },
        requests: RequestPolicy::Allow,
        high_risk: ToolPolicy::Deny,
    };
    ledger
        .set_agent_policy("@owner:test", "agent", 0, &policy)
        .unwrap();
    for layer in [Layer::Room, Layer::Requester] {
        ledger
            .set_policy("@owner:test", &scope, layer, 0, &policy)
            .unwrap();
    }
    let guardian = Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut process = runtime::spawn_for_scope_with_guardian(
        &profile,
        "offline-test-only",
        &mut ledger,
        &scope,
        Some(guardian),
    )
    .await
    .unwrap();
    process.session.initialize().await.unwrap();
    // Deliberately no account admission: this exercises the real protocol
    // using a custom localhost provider, never a production provider proof.
    process
        .session
        .open_context(&mut ledger, &scope, &profile)
        .await
        .unwrap();
    for (index, call) in ["first", "continuous"].into_iter().enumerate() {
        if index != 0 {
            assert!(process.session.can_continue());
            process.session.park();
            process
                .session
                .continue_context(&mut ledger, &scope, &profile)
                .await
                .unwrap();
        }
        let (queue, _) = codex::approval_queue(1).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            process.session.run_with_started(
                &mut ledger,
                "@owner:test",
                &scope,
                call,
                call,
                "Exercise the offline child fixture",
                BudgetMode::Postpaid,
                &queue,
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            result.text, "ROOT_REPLY",
            "child text must not become the user's final answer"
        );
        assert_eq!(
            (
                result.usage.input,
                result.usage.output,
                result.usage.cached_input
            ),
            (30, 12, 6),
            "{call}: {}",
            result.usage.accounting_version
        );
        assert_eq!(
            result.usage.accounting_version,
            "codex-app-server-0.167/root-turn-responses-v1"
        );
        assert_eq!(
            ledger
                .account(
                    &scope,
                    Layer::Agent,
                    Period::Lifetime,
                    time::OffsetDateTime::now_utc().unix_timestamp()
                )
                .unwrap(),
            (42 * (index as u64 + 1), 0)
        );
    }
    process.stop().await.unwrap();
    let mut resumed = runtime::spawn_for_scope_with_guardian(
        &profile,
        "offline-test-only",
        &mut ledger,
        &scope,
        Some(guardian),
    )
    .await
    .unwrap();
    resumed.session.initialize().await.unwrap();
    resumed
        .session
        .open_context(&mut ledger, &scope, &profile)
        .await
        .unwrap();
    // Resume subscribes to no raw response events. Both the first cold turn and
    // its warm successor use original native records, with independent charges.
    for (index, call) in ["cold", "after-cold"].into_iter().enumerate() {
        if index != 0 {
            assert!(resumed.session.can_continue());
            resumed.session.park();
            resumed.session.continue_context(&mut ledger, &scope, &profile).await.unwrap();
        }
        let (queue, _) = codex::approval_queue(1).unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            resumed.session.run_with_started(
                &mut ledger,
                "@owner:test",
                &scope,
                call,
                call,
                "Exercise the offline child fixture",
                BudgetMode::Postpaid,
                &queue,
                None,
            ),
        ).await.unwrap().unwrap();
        assert_eq!(result.text, "ROOT_REPLY");
        assert_eq!((result.usage.input, result.usage.output, result.usage.cached_input), (30, 12, 6));
        assert_eq!(result.usage.accounting_version, "codex-app-server-0.167/root-turn-responses-v1");
        assert!(resumed.session.can_continue());
        assert_eq!(
            ledger.account(&scope, Layer::Agent, Period::Lifetime,
                time::OffsetDateTime::now_utc().unix_timestamp()).unwrap(),
            (42 * (index as u64 + 3), 0)
        );
    }
    resumed.stop().await.unwrap();
    model.kill().await.unwrap();
}

async fn native_fixture() -> (tempfile::TempDir, Profile, tokio::process::Child) {
    native_fixture_mode("children").await
}
async fn native_fixture_mode(mode: &str) -> (tempfile::TempDir, Profile, tokio::process::Child) {
    let executable = PathBuf::from(
        std::env::var_os("HAGENCY_TEST_CODEX_EXECUTABLE").expect("explicit native CLI"),
    )
    .canonicalize()
    .unwrap();
    let tmp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    for name in ["home", "provider", "work"] {
        fs::create_dir(root.join(name)).unwrap();
        fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut model = tokio::process::Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/codex_model_fixture.py"))
        .arg(mode)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut port = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        BufReader::new(model.stdout.take().unwrap()).read_line(&mut port),
    )
    .await
    .unwrap()
    .unwrap();
    let port: u16 = port.trim().parse().unwrap();
    fs::write(
        root.join("provider/config.toml"),
        format!(
            r#"model_provider = "offline"
model = "gpt-5.4"
web_search = "disabled"
[features]
multi_agent = true
[model_providers.offline]
name = "Offline protocol fixture"
base_url = "http://127.0.0.1:{port}"
wire_api = "responses"
requires_openai_auth = false
"#
        ),
    )
    .unwrap();
    let profile = Profile {
        executable,
        home: root.join("home"),
        codex_home: root.join("provider"),
        cwd: root.join("work"),
        model: "gpt-5.4".into(),
        effort: "medium".into(),
        shared_auth: false,
        web_search: false,
    };
    (tmp, profile, model)
}

#[tokio::test]
#[ignore = "requires explicit installed Codex; localhost fixture, no account or paid model"]
async fn native_codex_shared_adapter_accounts_for_native_descendants() {
    use hagency_runtime::{
        codex::{
            native_arguments,
            session::{Settings, Update, Outcome},
            transport::Limits,
        },
        owned::{OwnedSession, Cleanup},
    };
    let (_tmp, profile, mut model) = native_fixture().await;
    let launch = hagency_platform::Launch {
        executable: profile.executable,
        arguments: native_arguments(false, Some(false))
            .into_iter()
            .map(Into::into)
            .collect(),
        directory: profile.cwd.clone(),
        environment: std::collections::BTreeMap::from([
            ("HOME".into(), profile.home.into_os_string()),
            ("CODEX_HOME".into(), profile.codex_home.into_os_string()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
        require_crash_containment: false,
    };
    let settings = Settings::new(profile.cwd, profile.model, profile.effort).unwrap();
    let mut process = OwnedSession::spawn(
        Path::new(env!("CARGO_BIN_EXE_hagency-local-probe")),
        &launch,
        settings,
        Limits {
            write_timeout_ms: 5000,
            event_wait_ms: 20_000,
            lifetime_ms: 60_000,
        },
        5000,
    )
    .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        process.initialize().await.unwrap();
        process.start_thread().await.unwrap();
        process.start_turn("Exercise the offline child fixture".into()).await.unwrap();
        let source = process.observation_source().unwrap();
        let mut text=String::new();
        let (mut root_capacity, mut child_capacity) = (false, false);
        loop {
            let next=process.next_observed_update().await;
            let (update, observation)=next.unwrap_or_else(|error| panic!("shared native tree update: {error:?}, refused={:?}, reason={:?}", process.refused_notification(), process.turn_failure()));
            assert!(observation.source()==&source);
            if let Some(context) = observation.context_usage() {
                assert!(context.window() > context.used());
                if context.is_root() { root_capacity = true; } else { child_capacity = true; }
            }
            if let Update::TextDelta{delta,..}=&update {text.push_str(delta);}
            if matches!(update,Update::TurnEnded) {
                assert!(matches!(process.protocol_outcome(),Some(Outcome::Completed{text}) if text=="ROOT_REPLY"), "native outcome: {:?}", process.turn_failure());
                assert_eq!(text,"ROOT_REPLY");
                let usage=observation.terminal_usage().expect("whole native call usage");
                assert_eq!(usage.total().total_tokens(),Some(42));
                assert_eq!(usage.total().input_tokens(),Some(30));
                assert_eq!(usage.total().cached_input_tokens(),Some(6));
                break;
            }
        }
        assert!(root_capacity && child_capacity, "installed native protocol reports each context independently of response billing");
    }).await;
    assert!(matches!(process.stop(),Cleanup::Observed(report) if report.scope.whole_tree_stopped));
    model.kill().await.unwrap();
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires explicit installed Codex; localhost Responses fixture, no paid model"]
async fn native_codex_personal_question_roundtrip_uses_native_tool_and_settles_one_call() {
    use hagency_runtime::questions::Answer;
    let (_tmp,profile,mut model)=native_fixture_mode("questions").await;
    let scope=Scope{agent:"agent".into(),binding:"binding".into(),room:"!room:test".into(),requester:"@member:test".into(),thread:"root".into()};
    let mut ledger=Ledger::open(_tmp.path().join("ledger"),"@owner:test").unwrap();
    ledger.register_binding("@owner:test",&scope).unwrap();
    let policy=Policy{budget:Budget{limit:Limit::Unlimited,period:Period::Lifetime},requests:RequestPolicy::Allow,high_risk:ToolPolicy::Deny};
    ledger.set_agent_policy("@owner:test","agent",0,&policy).unwrap();
    for layer in [Layer::Room,Layer::Requester] {ledger.set_policy("@owner:test",&scope,layer,0,&policy).unwrap();}
    let guardian=Path::new(env!("CARGO_BIN_EXE_hagency-local-probe"));
    let mut process=runtime::spawn_for_scope_with_guardian(&profile,"offline-test-only",&mut ledger,&scope,Some(guardian)).await.unwrap();
    process.session.initialize().await.unwrap();
    process.session.open_context(&mut ledger,&scope,&profile).await.unwrap();
    let (queue,mut approvals)=codex::approval_queue(1).unwrap();
    let (questions,mut incoming)=hagency_agent_local::questions::question_queue();
    let queue=queue.with_questions(questions);
    let run=process.session.run_with_started(&mut ledger,"@owner:test",&scope,"call","dispatch","Ask which database",BudgetMode::Postpaid,&queue,None);
    let host=async {
        let pending=incoming.recv().await.unwrap();
        assert_eq!(pending.scope,scope);
        assert_eq!(pending.questions[0].id,"db");
        assert!(pending.reply([("db".into(),Answer::Option("SQLite".into()))].into()).accepted().await);
    };
    let (done,())=tokio::time::timeout(Duration::from_secs(20),async{tokio::join!(run,host)}).await.unwrap();
    let done=done.unwrap();
    assert_eq!(done.text,"QUESTION_REPLY");
    assert_eq!(done.usage.input+done.usage.output,28);
    assert!(approvals.try_recv().is_err());
    assert!(ledger.outstanding_calls("@owner:test").unwrap().is_empty());
    process.stop().await.unwrap(); model.kill().await.unwrap();
}

#[tokio::test]
#[ignore = "requires explicit installed Codex; localhost fixture, no account or paid model"]
async fn native_codex_cold_session_recovers_original_response_receipts_with_children() {
    use hagency_runtime::{codex::{native_arguments,session::{Settings,ResumeThreadId,Update},transport::Limits},owned::{OwnedSession,Cleanup}};
    let (_tmp,profile,mut model)=native_fixture().await;
    let launch=hagency_platform::Launch {
        executable:profile.executable.clone(),
        arguments:native_arguments(false,Some(false)).into_iter().map(Into::into).collect(),
        directory:profile.cwd.clone(),
        environment:std::collections::BTreeMap::from([
            ("HOME".into(),profile.home.clone().into_os_string()),
            ("CODEX_HOME".into(),profile.codex_home.clone().into_os_string()),
            ("PATH".into(),"/usr/bin:/bin".into()),
        ]),require_crash_containment:false,
    };
    let mut thread:Option<String>=None;
    for round in 0..2 {
        let settings=Settings::new(profile.cwd.clone(),profile.model.clone(),profile.effort.clone()).unwrap();
        let mut process=OwnedSession::spawn(Path::new(env!("CARGO_BIN_EXE_hagency-local-probe")),&launch,settings,Limits{write_timeout_ms:5000,event_wait_ms:20000,lifetime_ms:60000},5000).unwrap();
        let result=tokio::time::timeout(Duration::from_secs(30),async {
            process.initialize().await.unwrap();
            thread=Some(match thread.take() {
                Some(id)=>process.resume_thread(ResumeThreadId::new(id).unwrap()).await.unwrap(),
                None=>process.start_thread().await.unwrap(),
            });
            process.start_turn("Exercise the offline child fixture".into()).await.unwrap();
            let source=process.observation_source().unwrap();
            loop {
                let (update,observation)=process.next_observed_update().await.unwrap();
                assert!(observation.source()==&source);
                if matches!(update,Update::TurnEnded) {
                    let usage=observation.terminal_usage().expect("native response receipts");
                    assert_eq!(usage.total().total_tokens(),Some(42),"round {round}: complete original native root and child receipts");
                    assert_eq!(usage.total().input_tokens(),Some(30));
                    assert_eq!(usage.total().cached_input_tokens(),Some(6));
                    break;
                }
            }
        }).await;
        assert!(matches!(process.stop(),Cleanup::Observed(report) if report.scope.whole_tree_stopped));
        result.unwrap();
    }
    model.kill().await.unwrap();
}
