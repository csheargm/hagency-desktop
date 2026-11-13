//! One owned `octos serve --stdio` session against the offline OUP peer
//! (ADR-193): the handshake Hagency sends, the dispatch's turn and the kernel's
//! continuation turns until Octos is idle, the reply and the usage receipts.
use hagency_platform::Launch;
use hagency_runtime::{
    octos::{
        Outcome,
        session::{
            Error, Event, Idle, Limits, ObservationKind, Permissions, Phase, QUIET_MS,
            UsageCoverage, UsageEvidence,
        },
    },
    owned::{Cleanup, OwnedOctosSession},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const TURN: &str = "0192f0c1-0000-7000-8000-000000000001";
const SESSION: &str = "coding:local:hagency-dispatch-1";

#[tokio::test]
async fn shared_octos_tool_surface_requires_the_kernel_to_acknowledge_the_exact_restriction() {
    use hagency_runtime::octos::session::HostTool;
    for (mode, has_tool, accepted) in [("normal", true, true), ("normal", false, true), ("ignore-generic-tools", true, false)] {
        let mut run = spawn(mode);
        run.runner.hello().await.unwrap();
        run.runner.open(SESSION, "coding", &run.workspace, Permissions::ReadOnly).await.unwrap();
        let tool = HostTool {
            name: "hagency.read_room_history".into(),
            description: "Read room history".into(),
            input_schema: json!({"type":"object","properties":{}}),
            read_only: true,
        };
        let tools = if has_tool { vec![tool] } else { Vec::new() };
        let result = run.runner.register_shared_text_tools(&tools).await;
        assert_eq!(result.is_ok(), accepted);
        let requests = run.requests();
        assert_eq!(requests[3]["params"]["generic_tools"], json!(["ask_user_question", "discover_tools", "run_tool"]));
        assert_eq!(requests[3]["params"]["tools"].as_array().unwrap().len(), usize::from(has_tool));
        if accepted { assert!(matches!(run.runner.stop(), Cleanup::Observed(report) if report.scope.leader_exited)); }
    }
}

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
        arguments: vec!["fake-octos".into(), mode.into(), marker.into()],
        directory: root.into(),
        environment,
        require_crash_containment: false,
    }
}
/// The event wait outlasts the quiet window, as a dispatch's operation
/// budget does.
fn limits() -> Limits {
    Limits {
        write_timeout_ms: 1000,
        event_wait_ms: QUIET_MS + 3000,
        lifetime_ms: 20_000,
    }
}
struct Run {
    _root: tempfile::TempDir,
    marker: PathBuf,
    workspace: String,
    runner: OwnedOctosSession,
}
fn spawn(mode: &str) -> Run {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join(mode);
    let runner =
        OwnedOctosSession::spawn(&binary(), &launch(root.path(), mode, &marker), limits()).unwrap();
    let workspace = root.path().join("工作 space");
    fs::create_dir(&workspace).unwrap();
    Run {
        workspace: workspace.to_str().unwrap().to_owned(),
        _root: root,
        marker,
        runner,
    }
}
impl Run {
    async fn start(&mut self) {
        self.runner.hello().await.unwrap();
        self.runner
            .open(
                SESSION,
                "coding",
                &self.workspace,
                Permissions::WorkspaceWrite,
            )
            .await
            .unwrap();
        self.runner
            .start_turn(TURN, "do the offline work")
            .await
            .unwrap();
        assert_eq!(self.runner.phase(), Phase::Running);
    }
    fn requests(&self) -> Vec<Value> {
        fs::read_to_string(self.marker.with_extension("requests"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn kind(&self) -> ObservationKind {
        self.runner.last_observation().unwrap().kind().clone()
    }
    /// Every event until idle, each with the observation it carried.
    async fn until_idle(&mut self) -> (Vec<String>, Vec<ObservationKind>, Idle) {
        let mut events = Vec::new();
        let mut kinds = Vec::new();
        loop {
            let event = next_event(&mut self.runner).await;
            kinds.push(self.kind());
            match event {
                Event::Idle(idle) => return (events, kinds, idle),
                Event::TurnStarted { turn_id } => events.push(format!("started {turn_id}")),
                Event::TurnEnded { turn_id, outcome } => {
                    events.push(format!("ended {turn_id} {outcome:?}"))
                }
                _ => panic!("no approval or tool call in this mode"),
            }
        }
    }
}
fn counts(e: &UsageEvidence) -> [Option<u64>; 5] {
    [
        e.input(),
        e.output(),
        e.reasoning(),
        e.cache_read(),
        e.cache_write(),
    ]
}
fn cleanup(value: Cleanup) {
    let Cleanup::Observed(report) = value else {
        panic!("fixture stop must be observed: {value:?}")
    };
    assert!(report.scope.leader_exited, "{report:?}");
    assert_eq!(
        report.scope.whole_tree_stopped,
        cfg!(any(target_os = "linux", target_os = "macos", windows)),
        "{report:?}"
    );
}

#[tokio::test]
async fn native_octos_owned_session_sends_the_fixed_handshake() {
    let mut run = spawn("normal");
    run.start().await;
    let (events, _, idle) = run.until_idle().await;
    assert_eq!(events, [format!("ended {TURN} Completed")]);
    assert_eq!(idle.outcome, Outcome::Completed);
    assert_eq!(idle.reply.as_deref(), Some("octos fixture reply"));
    assert_eq!(idle.turns, 1);
    assert_eq!(run.runner.phase(), Phase::Idle);
    let receipt = run.runner.completion_receipt().expect("original idle receipt");
    assert_eq!(receipt.outcome(), Outcome::Completed);
    assert!(receipt.matches_text("octos fixture reply"));
    assert!(!receipt.matches_text("different reply"));
    assert_eq!(receipt.sequence(), run.runner.last_observation().unwrap().sequence());
    assert!(run.runner.matches_observation_source(receipt.source()));
    let requests = run.requests();
    let methods: Vec<_> = requests.iter().map(|r| r["method"].clone()).collect();
    assert_eq!(
        methods,
        [
            "client_hello",
            "permission/profile/set",
            "session/open",
            "turn/start",
            "session/status/read"
        ]
    );
    let ids: Vec<_> = requests.iter().map(|r| r["id"].clone()).collect();
    assert_eq!(
        ids,
        [
            "hagency-1",
            "hagency-2",
            "hagency-3",
            "hagency-4",
            "hagency-5"
        ]
    );
    // Interactive questions and the guarded steer extension are opt-in.
    assert_eq!(
        requests[0]["params"],
        json!({"transport":"stdio","client":"hagency","supported_features":[
            "projection.envelope.v2","approval.typed.v1","session.workspace_cwd.v1",
            "user_question.v1","turn.steer_active_only.v1","event.turn_steer_dropped.v1",
            "runtime.host_scoped_profile.v1", "runtime.host_scoped_workspace_io.v1", "runtime.host_scoped_search.v1", "runtime.host_scoped_vision.v1", "runtime.host_patch_worker.v1",
            "runtime.host_scoped_web.v1", "plan.todos.v1"]})
    );
    assert_eq!(
        requests[1]["params"],
        json!({"session_id":SESSION,"update":{"mode":"workspace_write","network":"deny",
            "approval_policy":"on-request"}})
    );
    assert_eq!(
        requests[2]["params"],
        json!({"session_id":SESSION,"profile_id":"coding","cwd":run.workspace})
    );
    assert_eq!(
        requests[3]["params"],
        json!({"session_id":SESSION,"turn_id":TURN,
            "input":[{"kind":"text","text":"do the offline work"}]})
    );
    cleanup(run.runner.stop());
    assert!(run.runner.completion_receipt().is_none());
}

#[tokio::test]
async fn native_octos_owned_session_reports_usage_per_turn_and_at_idle() {
    let mut run = spawn("normal");
    run.start().await;
    // The baseline is taken after the turn is accepted, once.
    let source = run.runner.observation_source().unwrap();
    let (_, kinds, _) = run.until_idle().await;
    let [ObservationKind::Usage(turn), ObservationKind::Idle(idle)] = kinds.as_slice() else {
        panic!("one usage per terminal, then the idle usage");
    };
    assert!(turn.coverage() == UsageCoverage::Turns);
    assert_eq!(
        counts(turn),
        [Some(10), Some(7), Some(2), Some(20), Some(30)]
    );
    // The session's totals cover the turns: they close the record, without a
    // reasoning breakdown.
    assert!(idle.coverage() == UsageCoverage::Session);
    assert_eq!(counts(idle), [Some(10), Some(7), None, Some(20), Some(30)]);
    let last = run.runner.last_observation().unwrap();
    assert!(last.source() == &source);
    // The turn's tool start, its terminal's usage, then the idle usage.
    assert_eq!(last.sequence(), 3);
    assert!(run.runner.matches_observation_source(&source));
    assert!(matches!(run.runner.observation_source(), Err(Error::State)));
    cleanup(run.runner.stop());
    assert!(source.is_retired());
}

#[tokio::test]
async fn native_octos_owned_background_work_runs_until_octos_is_idle() {
    let mut run = spawn("background");
    run.start().await;
    let (events, kinds, idle) = run.until_idle().await;
    let continuation = "0192f0c1-0000-7000-8000-00000000c0de";
    // The child stream is no turn; the kernel's continuation turn is.
    assert_eq!(
        events,
        [
            format!("ended {TURN} Completed"),
            format!("started {continuation}"),
            format!("ended {continuation} Completed"),
        ]
    );
    assert_eq!(idle.reply.as_deref(), Some("octos background reply"));
    assert_eq!(idle.turns, 2);
    let [
        ObservationKind::Usage(_),
        ObservationKind::Ignored,
        ObservationKind::Usage(both),
        ObservationKind::Idle(total),
    ] = kinds.as_slice()
    else {
        panic!("usage at each terminal, then at idle");
    };
    assert_eq!(
        counts(both),
        [Some(15), Some(10), Some(4), Some(40), Some(60)]
    );
    // Only the session's totals include the sub-agent.
    assert!(total.coverage() == UsageCoverage::Session);
    assert_eq!(
        counts(total),
        [Some(915), Some(610), None, Some(60), Some(90)]
    );
    assert!(run.marker.with_extension("idle").exists());
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_quiet_session_is_idle_after_the_quiet_window() {
    let mut run = spawn("quiet");
    run.start().await;
    let started = Instant::now();
    let (_, _, idle) = run.until_idle().await;
    // Octos reported nothing: the quiet window, not a bridge-side cut.
    assert!(started.elapsed() >= Duration::from_millis(QUIET_MS));
    assert_eq!(idle.reply.as_deref(), Some("octos fixture reply"));
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_redelivery_and_a_second_terminal_change_nothing() {
    let mut run = spawn("duplicate");
    run.start().await;
    let (events, kinds, idle) = run.until_idle().await;
    assert_eq!(events, [format!("ended {TURN} Completed")]);
    assert_eq!(idle.outcome, Outcome::Completed);
    assert_eq!(idle.reply.as_deref(), Some("octos fixture reply"));
    let [ObservationKind::Usage(turn), ObservationKind::Idle(_)] = kinds.as_slice() else {
        panic!("one terminal counted");
    };
    assert_eq!(counts(turn)[0], Some(10));
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_errored_terminal_keeps_its_code() {
    let mut run = spawn("error");
    run.start().await;
    let (events, _, idle) = run.until_idle().await;
    assert_eq!(events, [format!("ended {TURN} Errored")]);
    assert_eq!(idle.outcome, Outcome::Errored);
    assert_eq!(idle.error_code.as_deref(), Some("provider_unavailable"));
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_absent_usage_stays_unknown() {
    let mut run = spawn("unknown-usage");
    run.start().await;
    let (_, kinds, _) = run.until_idle().await;
    let [ObservationKind::Usage(turn), ObservationKind::Idle(idle)] = kinds.as_slice() else {
        panic!("usage at the terminal and at idle");
    };
    // Never zero: an absent object and an empty total are unknown.
    assert_eq!(counts(turn), [None; 5]);
    assert!(idle.coverage() == UsageCoverage::Turns);
    assert_eq!(counts(idle), [None; 5]);
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_missing_turn_usage_cannot_be_closed_by_partial_session_totals() {
    // The ledger may contain successful earlier calls while this turn has no
    // usage receipt. Its nonempty counters do not prove coverage of this turn.
    let mut run = spawn("unknown-usage-with-totals");
    run.start().await;
    let (_, kinds, _) = run.until_idle().await;
    let [ObservationKind::Usage(turn), ObservationKind::Idle(idle)] = kinds.as_slice() else {
        panic!("usage at the terminal and at idle");
    };
    assert_eq!(counts(turn), [None; 5]);
    assert!(idle.coverage() == UsageCoverage::Turns);
    assert_eq!(counts(idle), [None; 5]);
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_lagging_totals_keep_the_turn_sums() {
    let mut run = spawn("late-totals");
    run.start().await;
    let (_, kinds, _) = run.until_idle().await;
    let Some(ObservationKind::Idle(idle)) = kinds.last() else {
        panic!("idle usage");
    };
    assert!(idle.coverage() == UsageCoverage::Turns);
    assert_eq!(
        counts(idle),
        [Some(10), Some(7), Some(2), Some(20), Some(30)]
    );
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_approval_reaches_the_host() {
    let mut run = spawn("approval");
    run.start().await;
    let Event::Approval {
        approval_id,
        turn_id,
        params,
    } = next_event(&mut run.runner).await
    else {
        panic!("the approval request");
    };
    assert_eq!(
        (approval_id.as_str(), turn_id.as_str()),
        ("approval-1", TURN)
    );
    assert_eq!(
        params["typed_details"]["command"]["command_line"],
        "rm -rf build"
    );
    assert!(matches!(run.kind(), ObservationKind::Ignored));
    cleanup(run.runner.stop());
}

#[tokio::test]
async fn native_octos_owned_refusals_and_bad_peers_close_the_session() {
    let mut run = spawn("refuse-open");
    run.runner.hello().await.unwrap();
    assert!(matches!(
        run.runner
            .open(SESSION, "coding", &run.workspace, Permissions::ReadOnly)
            .await,
        Err(Error::Refused(-32602))
    ));
    assert_eq!(run.runner.phase(), Phase::Closed);
    assert_eq!(run.requests()[1]["params"]["update"]["mode"], "read_only");
    cleanup(run.runner.cleanup());

    let mut run = spawn("malformed");
    assert!(matches!(
        run.runner.hello().await,
        Err(Error::Protocol(hagency_runtime::octos::Error::Envelope))
    ));
    cleanup(run.runner.cleanup());

    let mut run = spawn("stall");
    let started = Instant::now();
    assert!(matches!(run.runner.hello().await, Err(Error::Timeout)));
    assert!(started.elapsed() >= Duration::from_millis(limits().event_wait_ms));
    cleanup(run.runner.cleanup());

    // A session key outside Hagency's own form is refused before any byte.
    let mut run = spawn("normal");
    run.runner.hello().await.unwrap();
    assert!(matches!(
        run.runner
            .open(
                "coding:matrix:room",
                "coding",
                &run.workspace,
                Permissions::WorkspaceWrite
            )
            .await,
        Err(Error::Protocol(hagency_runtime::octos::Error::Input))
    ));
    assert_eq!(run.requests().len(), 1);
    cleanup(run.runner.cleanup());
}

/// The next event, past the tool start and end events every run carries for
/// the activity notice (they are counted, not acted on).
async fn next_event(runner: &mut OwnedOctosSession) -> Event {
    loop {
        let event = runner.next().await.unwrap();
        if !matches!(event, Event::Tool { .. }) {
            return event;
        }
    }
}
