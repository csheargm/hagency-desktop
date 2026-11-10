//! A host message that joins the running turn. `claude-native-steer.json`
//! holds what Claude Code 2.1.322 emitted in offline runs: its frames in
//! order, with bulky fields dropped and ids replaced by placeholders, and a
//! marker wherever the host wrote. A replay makes those host writes through
//! the driver and feeds it the recorded frames.
use super::interrupt::continuous;
use super::*;
use hagency_runtime::claude::session::{
    ApprovalControlPolicy, InterruptUpdate, ObservationKind, PermissionDecision, PreparedUpdate,
    SteerState, SteerUpdate, UsageCoverage, join_answer,
};
use std::collections::VecDeque;

fn recorded(name: &str) -> Vec<Value> {
    let all: Value =
        serde_json::from_str(include_str!("../fixtures/claude-native-steer.json")).unwrap();
    all[name].as_array().unwrap().clone()
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("the host wrote fewer frames than the recording")
}
/// Write one steer and return its message id. The flush is no acknowledgement.
async fn steer(driver: &mut Driver, peer: &mut Peer, text: &str) -> String {
    let mut prepared = driver.prepare_steer(text).unwrap();
    let (sent, frame) = bounded(async {
        tokio::join!(driver.send_prepared_steer(&mut prepared), read(peer))
    })
    .await;
    assert!(matches!(sent.unwrap(), SteerUpdate::WriteAccepted(receipt) if receipt.flushed));
    assert_eq!(
        frame,
        json!({"type":"user","uuid":prepared.message_id(),"session_id":"session-one",
            "parent_tool_use_id":null,"message":{"role":"user",
                "content":format!("{}{text}", hagency_runtime::claude::STEER_LEAD)}})
    );
    assert_eq!(
        driver.steer_state(prepared.message_id()),
        Some(SteerState::Unacknowledged)
    );
    prepared.message_id().to_owned()
}
/// Captured from Claude 2.1.322: a steer whose text is `/cost` ran the native
/// command and never reached the model. No sender text may come first.
#[tokio::test]
async fn native_claude_steer_text_is_never_a_native_slash_command() {
    let (mut driver, mut peer) = pair(256, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    super::continuous::next_prompt(&mut driver, &mut peer).await;
    for text in ["/cost", "/clear", "\n/compact"] {
        let mut prepared = driver.prepare_steer(text).unwrap();
        let (sent, frame) = bounded(async {
            tokio::join!(driver.send_prepared_steer(&mut prepared), read(&mut peer))
        })
        .await;
        assert!(matches!(sent.unwrap(), SteerUpdate::WriteAccepted(_)));
        let content = frame["message"]["content"].as_str().unwrap();
        assert!(!content.trim_start().starts_with('/'), "{content:?}");
        assert!(content.ends_with(text));
    }
}
/// Write a stop and return the frames of its one write, the interrupt last.
async fn stop(driver: &mut Driver, peer: &mut Peer, frames: usize) -> Vec<Value> {
    let mut prepared = driver.prepare_interrupt().unwrap();
    let (sent, lines) = bounded(async {
        tokio::join!(driver.send_prepared_interrupt(&mut prepared), async {
            let mut lines = Vec::new();
            for _ in 0..frames {
                let mut line = String::new();
                peer.input.read_line(&mut line).await.unwrap();
                lines.push(line);
            }
            lines
        })
    })
    .await;
    let InterruptUpdate::WriteAccepted(receipt) = sent.unwrap() else {
        panic!("the stop was not written");
    };
    assert_eq!(
        receipt.total_bytes,
        lines.iter().map(String::len).sum::<usize>(),
        "cancels and the interrupt leave as one write"
    );
    let lines: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines[frames - 1]["request_id"], prepared.request_id());
    assert_eq!(lines[frames - 1]["request"], json!({"subtype":"interrupt"}));
    lines
}
fn lifecycle(id: &str, state: &str) -> Value {
    json!({"type":"command_lifecycle","command_uuid":id,"state":state,"session_id":"session-one"})
}
fn cycle_init(ids: &[&str]) -> Value {
    json!({"type":"system","subtype":"init","session_id":"session-one","user_message_uuids":ids})
}
fn cycle_result(single: &str, ids: &[&str], text: &str) -> Value {
    let mut value = result();
    value["result"] = text.into();
    value["user_message_uuid"] = single.into();
    value["user_message_uuids"] = json!(ids);
    value
}
fn response(id: &Value, body: Value) -> Value {
    json!({"type":"control_response","response":{"subtype":"success","request_id":id,"response":body}})
}

struct Replay {
    driver: Driver,
    peer: Peer,
    entries: VecDeque<Value>,
    /// Recorded placeholder and the id this run used, both as JSON strings.
    ids: Vec<(String, String)>,
    /// The turn's answer as a host joins it from the driver's observations.
    answer: String,
}
impl Replay {
    async fn start(entries: Vec<Value>) -> Self {
        let (mut driver, mut peer) = pair(65_536, limits());
        driver.enable_native_completion().unwrap();
        driver.enable_continuous_session().unwrap();
        initialize(&mut driver, &mut peer).await;
        let (sent, prompt) = tokio::join!(driver.prompt("first"), read(&mut peer));
        sent.unwrap();
        let mut replay = Self {
            driver,
            peer,
            entries: entries.into(),
            ids: vec![("\"<first>\"".into(), prompt["uuid"].to_string())],
            answer: String::new(),
        };
        // The native prelude and init are one observation.
        let mut bytes = Vec::new();
        loop {
            let entry = replay.entries.pop_front().unwrap();
            let frame = replay.native(&entry);
            bytes.extend(encoded(frame.clone()));
            if frame["subtype"] == "init" {
                break;
            }
        }
        let (init, written) = tokio::join!(
            replay.driver.next_message(),
            replay.peer.output.write_all(&bytes)
        );
        written.unwrap();
        init.unwrap();
        replay.driver.supervise_turn().unwrap();
        replay
            .driver
            .enable_approval_control(ApprovalControlPolicy {
                owner_wait_ms: 30_000,
                response_reserve_ms: 1000,
            })
            .unwrap();
        replay
    }
    fn native(&self, entry: &Value) -> Value {
        let mut text = entry.to_string();
        for (placeholder, id) in &self.ids {
            text = text.replace(placeholder, id);
        }
        serde_json::from_str(&text).unwrap()
    }
    fn id(&self, placeholder: &str) -> String {
        self.native(&json!(placeholder)).as_str().unwrap().into()
    }
    fn state(&self, placeholder: &str) -> Option<SteerState> {
        self.driver.steer_state(&self.id(placeholder))
    }
    async fn feed(&mut self, entry: &Value) -> Result<Message, Error> {
        let frame = self.native(entry);
        let message = event(&mut self.driver, &mut self.peer, frame.clone()).await?;
        if frame["type"] == "result" {
            let text = frame["result"].as_str().unwrap_or("");
            if self.driver.last_observation().unwrap().continues_answer() {
                join_answer(&mut self.answer, text);
            } else {
                self.answer = text.into();
            }
        }
        Ok(message)
    }
    /// Make the next recorded host write or feed the next native frame, and
    /// return the entry as recorded.
    async fn step(&mut self) -> Option<Value> {
        let entry = self.entries.pop_front()?;
        match entry["host"].as_str() {
            Some("steer") => {
                let id = steer(&mut self.driver, &mut self.peer, "steer text").await;
                self.ids.push((entry["id"].to_string(), json!(id).to_string()));
            }
            Some("control") => {
                // Recorded back to back; the driver writes them as one frame.
                let mut expected = vec![entry.clone()];
                while self.entries.front().is_some_and(|e| e["host"] == "control") {
                    expected.push(self.entries.pop_front().unwrap());
                }
                let lines = stop(&mut self.driver, &mut self.peer, expected.len()).await;
                for (expected, line) in expected.iter().zip(&lines) {
                    assert_eq!(line["request"], self.native(&expected["request"]));
                    self.ids.push((
                        expected["request_id"].to_string(),
                        line["request_id"].to_string(),
                    ));
                }
            }
            Some("allow") => {
                let mut prepared = self
                    .driver
                    .prepare_approval(
                        entry["request_id"].as_str().unwrap(),
                        PermissionDecision::Allow,
                    )
                    .unwrap();
                let (sent, _) = bounded(async {
                    tokio::join!(
                        self.driver.send_prepared_approval(&mut prepared),
                        read(&mut self.peer)
                    )
                })
                .await;
                assert!(matches!(sent.unwrap(), PreparedUpdate::WriteAccepted(_)));
            }
            _ => {
                self.feed(&entry).await.unwrap();
            }
        }
        Some(entry)
    }
    async fn finish(&mut self) {
        while self.step().await.is_some() {}
    }
}

#[tokio::test]
async fn native_claude_steer_taken_into_the_running_cycle_is_named_by_its_one_result() {
    for (name, steers) in [("injected", 1), ("injected_two", 2)] {
        let mut replay = Replay::start(recorded(name)).await;
        let mut results = 0;
        while let Some(entry) = replay.step().await {
            if entry["type"] == "command_lifecycle" && entry["command_uuid"] != "<first>" {
                assert_eq!(
                    replay.state(entry["command_uuid"].as_str().unwrap()),
                    Some(match entry["state"].as_str().unwrap() {
                        "queued" => SteerState::Queued,
                        "started" => SteerState::Started,
                        _ => SteerState::Completed,
                    })
                );
            }
            if entry["type"] == "result" {
                results += 1;
                assert_eq!(entry["user_message_uuids"].as_array().unwrap().len(), 1 + steers);
                assert!(!replay.driver.last_observation().unwrap().continues_answer());
                assert_eq!(replay.driver.phase(), Phase::Running);
            }
        }
        assert_eq!(results, 1, "{name}");
        assert_eq!(replay.driver.phase(), Phase::ResultObserved);
        assert_eq!(replay.answer, "FIXTURE_REPLY_2");
        let receipt = replay.driver.completion_receipt().unwrap();
        assert!(receipt.matches_text("FIXTURE_REPLY_2"));
        assert!(!receipt.is_error() && !receipt.interrupted());
        assert!(replay.driver.can_followup());
        // The next turn knows nothing of this turn's steers.
        let added = replay.id("<added>");
        replay.driver.begin_followup(limits()).unwrap();
        assert_eq!(replay.driver.steer_state(&added), None);
    }
}

#[tokio::test]
async fn native_claude_text_only_steer_runs_as_a_second_cycle_and_joins_the_answer() {
    let mut replay = Replay::start(recorded("own_cycle")).await;
    let mut results = 0;
    while let Some(entry) = replay.step().await {
        if entry["type"] == "result" {
            results += 1;
            let observation = replay.driver.last_observation().unwrap();
            assert_eq!(observation.continues_answer(), results == 2);
            // Each native result reports its own delta; the call holds the sum.
            let ObservationKind::Result { usage, is_error } = observation.kind() else {
                panic!("a result observation carries usage");
            };
            assert!(!is_error);
            assert!(usage.coverage() == UsageCoverage::ReportedModelsResult);
            assert_eq!(
                (usage.counts().input(), usage.counts().output()),
                (Some(17 * results), Some(9 * results))
            );
        }
        // No native idle separates the two cycles and neither result ends it.
        if replay.entries.front().is_some() {
            assert_eq!(replay.driver.phase(), Phase::Running);
            assert!(replay.driver.completion_receipt().is_err());
        }
    }
    assert_eq!(results, 2);
    assert_eq!(replay.state("<added>"), Some(SteerState::Completed));
    assert_eq!(replay.driver.phase(), Phase::ResultObserved);
    assert_eq!(replay.answer, "FIXTURE_REPLY_1\n\nFIXTURE_REPLY_2");
    let receipt = replay.driver.completion_receipt().unwrap();
    assert!(receipt.matches_text("FIXTURE_REPLY_1\n\nFIXTURE_REPLY_2"));
    assert!(!receipt.matches_text("FIXTURE_REPLY_2"));
    assert!(!receipt.is_error() && !receipt.interrupted());
    assert!(!replay.driver.needs_usage_snapshot());
    assert!(replay.driver.can_followup());
}

#[tokio::test]
async fn native_claude_steer_read_only_after_idle_still_belongs_to_the_turn() {
    // The recording wrote the steer after idle. A host cannot see idle before
    // it decides to write, so replay the write just ahead of the result the
    // peer had already produced: the peer reads it only after going idle.
    let mut entries = recorded("late");
    let write = entries.iter().position(|e| e["host"] == "steer").unwrap();
    let result = entries.iter().position(|e| e["type"] == "result").unwrap();
    let entry = entries.remove(write);
    entries.insert(result, entry);
    let mut replay = Replay::start(entries).await;
    let mut idles = 0;
    while let Some(entry) = replay.step().await {
        if entry["state"] == "idle" {
            idles += 1;
            if idles == 1 {
                assert_eq!(replay.answer, "FIXTURE_REPLY_1");
                assert_eq!(replay.state("<added>"), Some(SteerState::Unacknowledged));
                assert_eq!(replay.driver.phase(), Phase::Running);
                assert!(replay.driver.completion_receipt().is_err());
                assert!(!replay.driver.can_followup());
            }
        }
    }
    assert_eq!(idles, 2);
    assert_eq!(replay.driver.phase(), Phase::ResultObserved);
    assert_eq!(replay.answer, "FIXTURE_REPLY_1\n\nFIXTURE_REPLY_2");
    assert!(
        replay
            .driver
            .completion_receipt()
            .unwrap()
            .matches_text(&replay.answer)
    );
    assert!(replay.driver.can_followup());
}

#[tokio::test]
async fn native_claude_steers_share_chain_and_join_cycles_as_recorded() {
    for (name, answer) in [
        // Two waiting steers run as one cycle with one result.
        ("batched", "FIXTURE_REPLY_1\n\nFIXTURE_REPLY_2"),
        // A steer written during a steer cycle runs as a third cycle.
        (
            "chained",
            "FIXTURE_REPLY_1\n\nFIXTURE_REPLY_2\n\nFIXTURE_REPLY_3",
        ),
        // Or that steer cycle takes it in at its next model request.
        (
            "injected_into_steer_cycle",
            "FIXTURE_REPLY_1\n\nFIXTURE_REPLY_3",
        ),
    ] {
        let mut replay = Replay::start(recorded(name)).await;
        replay.finish().await;
        assert_eq!(replay.driver.phase(), Phase::ResultObserved, "{name}");
        for steer in ["<added>", "<third>"] {
            assert_eq!(replay.state(steer), Some(SteerState::Completed), "{name}");
        }
        assert_eq!(replay.answer, answer, "{name}");
        let receipt = replay.driver.completion_receipt().unwrap();
        assert!(receipt.matches_text(answer), "{name}");
        assert!(!receipt.is_error() && !receipt.interrupted());
        assert!(replay.driver.can_followup(), "{name}");
    }
}

#[tokio::test]
async fn native_claude_steer_is_refused_outside_a_running_continuous_turn() {
    // A session without per-input native ids cannot tell a steer's frames apart.
    let (mut driver, _peer) = running().await;
    assert!(matches!(driver.prepare_steer("more"), Err(Error::State)));
    assert_eq!(driver.phase(), Phase::Running);

    let (mut driver, mut peer) = pair(8192, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    assert!(matches!(driver.prepare_steer("more"), Err(Error::State)));
    let (sent, prompt) = tokio::join!(driver.prompt("next turn"), read(&mut peer));
    sent.unwrap();
    let prompt = prompt["uuid"].as_str().unwrap();
    assert!(
        matches!(driver.prepare_steer("more"), Err(Error::State)),
        "the turn has no native identity before its init"
    );
    event(&mut driver, &mut peer, cycle_init(&[prompt]))
        .await
        .unwrap();
    for invalid in [String::new(), "x".repeat(64 * 1024 + 1)] {
        assert!(matches!(
            driver.prepare_steer(&invalid),
            Err(Error::Protocol(hagency_runtime::claude::Error::Input))
        ));
    }
    // Prepared while the turn ran, never started, and the turn then finished.
    let mut unsent = driver.prepare_steer("too late").unwrap();
    event(&mut driver, &mut peer, cycle_result(prompt, &[prompt], "answer"))
        .await
        .unwrap();
    event(&mut driver, &mut peer, state("idle")).await.unwrap();
    assert_eq!(driver.phase(), Phase::ResultObserved);
    assert!(matches!(
        driver.send_prepared_steer(&mut unsent).await,
        Ok(SteerUpdate::Unavailable)
    ));
    assert_eq!(driver.steer_state(unsent.message_id()), None);
    assert!(matches!(driver.prepare_steer("more"), Err(Error::State)));
    assert_eq!(driver.phase(), Phase::ResultObserved);
    assert!(driver.can_followup());
    // A frame prepared for one turn sends nothing to the next.
    driver.begin_followup(limits()).unwrap();
    super::continuous::next_prompt(&mut driver, &mut peer).await;
    assert!(matches!(
        driver.send_prepared_steer(&mut unsent).await,
        Err(Error::Identity)
    ));
    assert_eq!(driver.phase(), Phase::Running);
    for _ in 0..16 {
        steer(&mut driver, &mut peer, "more").await;
    }
    assert!(matches!(driver.prepare_steer("more"), Err(Error::Capacity)));
    assert_eq!(driver.phase(), Phase::Running);
}

#[tokio::test]
async fn native_claude_steer_write_resumes_across_native_events_and_admits_no_other_write() {
    let (mut driver, mut peer) = pair(256, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    super::continuous::next_prompt(&mut driver, &mut peer).await;
    let text = "x".repeat(2000);
    let mut prepared = driver.prepare_steer(&text).unwrap();
    // Nobody reads the peer side yet: the write stalls and the event returns.
    let running = encoded(state("running"));
    let (update, written) = tokio::join!(
        driver.send_prepared_steer(&mut prepared),
        peer.output.write_all(&running)
    );
    written.unwrap();
    assert!(matches!(update.unwrap(), SteerUpdate::Message(_)));
    assert!(
        driver
            .write_progress()
            .is_some_and(|progress| progress.accepted_bytes < progress.total_bytes)
    );
    assert!(driver.prepare_interrupt().is_err());
    assert!(driver.prepare_steer("second").is_err());
    assert_eq!(driver.phase(), Phase::Running);
    let (update, frame) = tokio::join!(driver.send_prepared_steer(&mut prepared), read(&mut peer));
    assert!(matches!(update.unwrap(), SteerUpdate::WriteAccepted(receipt) if receipt.flushed));
    assert_eq!(frame["uuid"], prepared.message_id());
    assert_eq!(
        frame["message"]["content"],
        format!("{}{text}", hagency_runtime::claude::STEER_LEAD)
    );
    assert!(matches!(
        driver.send_prepared_steer(&mut prepared).await,
        Err(Error::State)
    ));
    assert_eq!(driver.phase(), Phase::Running);

    // An acknowledgement cannot precede the bytes it acknowledges.
    let mut prepared = driver.prepare_steer(&text).unwrap();
    let early = encoded(lifecycle(prepared.message_id(), "queued"));
    let (update, written) = tokio::join!(
        driver.send_prepared_steer(&mut prepared),
        peer.output.write_all(&early)
    );
    written.unwrap();
    assert!(matches!(update, Err(Error::Identity)));
    assert_eq!(driver.phase(), Phase::Closed);
}

#[tokio::test(start_paused = true)]
async fn native_claude_unacknowledged_steer_times_out_and_a_queued_one_has_no_deadline() {
    let (mut driver, mut peer, _) = continuous().await;
    let first = steer(&mut driver, &mut peer, "first").await;
    tokio::time::advance(Duration::from_secs(9)).await;
    event(&mut driver, &mut peer, lifecycle(&first, "queued"))
        .await
        .unwrap();
    assert_eq!(driver.steer_state(&first), Some(SteerState::Queued));
    // Queued, it waits on the model like the turn itself.
    tokio::time::advance(Duration::from_secs(600)).await;
    event(&mut driver, &mut peer, state("running"))
        .await
        .unwrap();
    let written = tokio::time::Instant::now();
    steer(&mut driver, &mut peer, "second").await;
    // Other activity does not renew the acknowledgement bound.
    for _ in 0..9 {
        tokio::time::advance(Duration::from_secs(1)).await;
        event(&mut driver, &mut peer, state("running"))
            .await
            .unwrap();
    }
    assert!(matches!(driver.next_message().await, Err(Error::Timeout)));
    assert_eq!(written.elapsed(), Duration::from_secs(10));
    assert_eq!(driver.phase(), Phase::Closed);
}

#[tokio::test]
async fn native_claude_stop_withdraws_queued_steers_in_the_same_write() {
    for name in ["stop_queued", "stop_queued_tool"] {
        let mut replay = Replay::start(recorded(name)).await;
        while let Some(entry) = replay.step().await {
            // The cancel was written before the interrupt and for this steer:
            // `Replay::step` compared both frames with the recording.
            if entry["host"] == "control" {
                assert!(replay.driver.is_interrupting());
                assert!(
                    matches!(replay.driver.prepare_steer("more"), Err(Error::State)),
                    "nothing joins a turn that is being stopped"
                );
            }
            if entry["state"] == "started" {
                assert_eq!(entry["command_uuid"], "<first>", "the steer never runs");
            }
        }
        let added = replay.id("<added>");
        assert_eq!(replay.driver.steer_state(&added), Some(SteerState::Cancelled));
        assert_eq!(replay.driver.steer_cancelled(&added), Some(true));
        let receipt = replay.driver.interrupt_receipt().unwrap();
        assert!(receipt.accepted());
        assert_eq!(receipt.still_queued(), Some([].as_slice()));
        assert_eq!(replay.driver.phase(), Phase::ResultObserved, "{name}");
        assert!(replay.driver.answer_interrupted());
        let receipt = replay.driver.completion_receipt().unwrap();
        assert!(receipt.is_error() && receipt.interrupted());
        assert!(receipt.matches_text(&replay.answer));
        assert!(replay.driver.can_followup(), "{name}");
    }
}

#[tokio::test]
async fn native_claude_stop_ends_started_steers_with_their_cycle() {
    // Taken into the prompt's cycle before the stop: nothing is left to
    // withdraw, and the aborted result still names it.
    let mut replay = Replay::start(recorded("stop_injected")).await;
    replay.finish().await;
    let added = replay.id("<added>");
    assert_eq!(replay.driver.steer_state(&added), Some(SteerState::Cancelled));
    assert_eq!(replay.driver.steer_cancelled(&added), None);
    assert_eq!(replay.driver.phase(), Phase::ResultObserved);
    assert!(replay.driver.completion_receipt().unwrap().interrupted());
    assert!(replay.driver.can_followup());

    // Stopped while running as its own cycle: the first cycle's answer stays.
    let mut replay = Replay::start(recorded("stop_steer_cycle")).await;
    replay.finish().await;
    assert_eq!(replay.state("<added>"), Some(SteerState::Cancelled));
    assert_eq!(replay.answer, "FIXTURE_REPLY_1");
    let receipt = replay.driver.completion_receipt().unwrap();
    assert!(receipt.matches_text("FIXTURE_REPLY_1"));
    assert!(receipt.is_error() && receipt.interrupted());
    assert!(replay.driver.can_followup());

    // The same, with a third message still queued behind that cycle.
    let mut replay = Replay::start(recorded("stop_steer_cycle_queued")).await;
    replay.finish().await;
    let (added, third) = (replay.id("<added>"), replay.id("<third>"));
    assert_eq!(replay.driver.steer_state(&added), Some(SteerState::Cancelled));
    assert_eq!(replay.driver.steer_cancelled(&added), None);
    assert_eq!(replay.driver.steer_state(&third), Some(SteerState::Cancelled));
    assert_eq!(replay.driver.steer_cancelled(&third), Some(true));
    assert_eq!(replay.answer, "FIXTURE_REPLY_1");
    assert_eq!(replay.driver.phase(), Phase::ResultObserved);
    assert!(replay.driver.can_followup());
}

#[tokio::test]
async fn native_claude_steer_left_queued_by_an_accepted_stop_never_runs() {
    // Recorded with an interrupt alone: the peer reports the steer as still
    // queued and then runs it. The driver also writes a cancel; here the peer
    // refuses that, and the steer's start then ends the session.
    let mut replay = Replay::start(recorded("interrupt_alone")).await;
    let mut closed = false;
    while let Some(entry) = replay.entries.pop_front() {
        match entry["host"].as_str() {
            Some("steer") => {
                let id = steer(&mut replay.driver, &mut replay.peer, "steer text").await;
                replay.ids.push((entry["id"].to_string(), json!(id).to_string()));
            }
            Some("control") => {
                let lines = stop(&mut replay.driver, &mut replay.peer, 2).await;
                assert_eq!(
                    lines[0]["request"],
                    json!({"subtype":"cancel_async_message","message_uuid":replay.id("<added>")})
                );
                replay.ids.push((
                    entry["request_id"].to_string(),
                    lines[1]["request_id"].to_string(),
                ));
                let refused = json!({"type":"control_response","response":{
                    "subtype":"error","request_id":lines[0]["request_id"],"error":"refused"}});
                replay.feed(&refused).await.unwrap();
                let added = replay.id("<added>");
                assert_eq!(replay.driver.steer_cancelled(&added), Some(false));
                assert_eq!(replay.driver.steer_state(&added), Some(SteerState::Queued));
            }
            _ if entry["command_uuid"] == "<added>" && entry["state"] == "started" => {
                let added = replay.id("<added>");
                assert_eq!(
                    replay.driver.interrupt_receipt().unwrap().still_queued(),
                    Some([added].as_slice())
                );
                assert_eq!(replay.driver.phase(), Phase::Running);
                assert!(matches!(replay.feed(&entry).await, Err(Error::Identity)));
                assert_eq!(replay.driver.phase(), Phase::Closed);
                closed = true;
                break;
            }
            _ => {
                replay.feed(&entry).await.unwrap();
            }
        }
    }
    assert!(closed);
}

#[tokio::test(start_paused = true)]
async fn native_claude_steer_still_queued_after_stop_keeps_the_turn_open_until_the_drain_bound() {
    let (mut driver, mut peer, prompt) = continuous().await;
    let prompt = prompt["uuid"].as_str().unwrap();
    let added = steer(&mut driver, &mut peer, "more").await;
    event(&mut driver, &mut peer, lifecycle(&added, "queued"))
        .await
        .unwrap();
    let lines = stop(&mut driver, &mut peer, 2).await;
    event(
        &mut driver,
        &mut peer,
        response(&lines[0]["request_id"], json!({"cancelled":false})),
    )
    .await
    .unwrap();
    event(
        &mut driver,
        &mut peer,
        response(&lines[1]["request_id"], json!({"still_queued":[added]})),
    )
    .await
    .unwrap();
    let accepted = tokio::time::Instant::now();
    let mut aborted = cycle_result(prompt, &[prompt], "");
    aborted["is_error"] = json!(true);
    event(&mut driver, &mut peer, aborted).await.unwrap();
    event(&mut driver, &mut peer, state("idle")).await.unwrap();
    assert_eq!(driver.phase(), Phase::Running);
    assert!(driver.completion_receipt().is_err());
    assert!(!driver.can_followup());
    assert!(matches!(driver.next_message().await, Err(Error::Timeout)));
    assert_eq!(accepted.elapsed(), Duration::from_secs(10));
}

#[tokio::test]
async fn native_claude_stop_records_each_cancel_answer_and_refuses_inconsistent_ones() {
    // A refused interrupt leaves the turn running without the withdrawn steer.
    let (mut driver, mut peer, prompt) = continuous().await;
    let prompt = prompt["uuid"].as_str().unwrap();
    let added = steer(&mut driver, &mut peer, "more").await;
    event(&mut driver, &mut peer, lifecycle(&added, "queued"))
        .await
        .unwrap();
    let lines = stop(&mut driver, &mut peer, 2).await;
    assert_eq!(driver.steer_cancelled(&added), None);
    event(&mut driver, &mut peer, lifecycle(&added, "cancelled"))
        .await
        .unwrap();
    event(
        &mut driver,
        &mut peer,
        response(&lines[0]["request_id"], json!({"cancelled":true})),
    )
    .await
    .unwrap();
    assert_eq!(driver.steer_cancelled(&added), Some(true));
    event(&mut driver, &mut peer, json!({"type":"control_response","response":{
        "subtype":"error","request_id":lines[1]["request_id"],"error":"refused"}}))
    .await
    .unwrap();
    assert!(!driver.interrupt_receipt().unwrap().accepted());
    let again = steer(&mut driver, &mut peer, "again").await;
    event(&mut driver, &mut peer, lifecycle(&again, "queued"))
        .await
        .unwrap();
    event(&mut driver, &mut peer, lifecycle(&again, "started"))
        .await
        .unwrap();
    event(&mut driver, &mut peer, lifecycle(&again, "completed"))
        .await
        .unwrap();
    event(
        &mut driver,
        &mut peer,
        cycle_result(prompt, &[prompt, &again], "answer"),
    )
    .await
    .unwrap();
    event(&mut driver, &mut peer, state("idle")).await.unwrap();
    assert_eq!(driver.phase(), Phase::ResultObserved);
    assert!(driver.can_followup());

    for (body, error) in [
        // Not an answer at all.
        (json!({"cancelled":"yes"}), Error::Malformed),
        (json!({}), Error::Malformed),
        // Confirmed, yet the native queue never reported the withdrawal.
        (json!({"cancelled":true}), Error::Identity),
    ] {
        let (mut driver, mut peer, _) = continuous().await;
        let added = steer(&mut driver, &mut peer, "more").await;
        event(&mut driver, &mut peer, lifecycle(&added, "queued"))
            .await
            .unwrap();
        let lines = stop(&mut driver, &mut peer, 2).await;
        assert_eq!(
            event(&mut driver, &mut peer, response(&lines[0]["request_id"], body))
                .await
                .err(),
            Some(error)
        );
        assert_eq!(driver.phase(), Phase::Closed);
    }
    // One answer per cancel.
    let (mut driver, mut peer, _) = continuous().await;
    let added = steer(&mut driver, &mut peer, "more").await;
    event(&mut driver, &mut peer, lifecycle(&added, "queued"))
        .await
        .unwrap();
    let lines = stop(&mut driver, &mut peer, 2).await;
    let answer = response(&lines[0]["request_id"], json!({"cancelled":false}));
    event(&mut driver, &mut peer, answer.clone()).await.unwrap();
    assert!(matches!(
        event(&mut driver, &mut peer, answer).await,
        Err(Error::Identity)
    ));
}

#[tokio::test]
async fn native_claude_steer_correlation_and_lifecycle_stay_exact() {
    // Each case: the frames a steer's turn accepts, then one it must refuse.
    #[derive(Clone, Copy, Debug)]
    enum Case {
        UnknownMessageInResult,
        QueuedSteerInResult,
        SteerNamedTwiceInResult,
        SteerNamedByTwoResults,
        CycleForQueuedSteer,
        CycleForThePrompt,
        CycleBeforeAnyResult,
        CycleNamedTwice,
        PromptResultInsideSteerCycle,
        CycleResultWithoutItsList,
        CycleResultForAnotherSteer,
        StartedBeforeQueued,
        CompletedBeforeStarted,
        QueuedTwice,
        CancelledBeforeQueued,
        UnknownState,
        AfterCompletion,
    }
    use Case::*;
    for case in [
        UnknownMessageInResult,
        QueuedSteerInResult,
        SteerNamedTwiceInResult,
        SteerNamedByTwoResults,
        CycleForQueuedSteer,
        CycleForThePrompt,
        CycleBeforeAnyResult,
        CycleNamedTwice,
        PromptResultInsideSteerCycle,
        CycleResultWithoutItsList,
        CycleResultForAnotherSteer,
        StartedBeforeQueued,
        CompletedBeforeStarted,
        QueuedTwice,
        CancelledBeforeQueued,
        UnknownState,
        AfterCompletion,
    ] {
        let (mut driver, mut peer, prompt) = continuous().await;
        let prompt = prompt["uuid"].as_str().unwrap();
        let one = steer(&mut driver, &mut peer, "one").await;
        let two = steer(&mut driver, &mut peer, "two").await;
        let (one, two) = (one.as_str(), two.as_str());
        // The lifecycle of a message that is not a steer of this turn, the
        // prompt's own included, is not this module's to judge.
        for other in [prompt, "another-command"] {
            event(&mut driver, &mut peer, lifecycle(other, "completed"))
                .await
                .unwrap();
        }
        let mut accepted = Vec::new();
        let queued = |id: &str| lifecycle(id, "queued");
        let started = |id: &str| lifecycle(id, "started");
        let first = cycle_result(prompt, &[prompt], "first");
        let refused = match case {
            UnknownMessageInResult => {
                accepted.extend([queued(one), started(one)]);
                cycle_result(prompt, &[prompt, one, "unknown-message"], "x")
            }
            QueuedSteerInResult => {
                accepted.extend([queued(one)]);
                cycle_result(prompt, &[prompt, one], "x")
            }
            SteerNamedTwiceInResult => {
                accepted.extend([queued(one), started(one)]);
                cycle_result(prompt, &[prompt, one, one], "x")
            }
            SteerNamedByTwoResults => {
                accepted.extend([
                    queued(one),
                    started(one),
                    cycle_result(prompt, &[prompt, one], "x"),
                ]);
                cycle_result(prompt, &[prompt, one], "y")
            }
            CycleForQueuedSteer => {
                accepted.extend([queued(one), first]);
                cycle_init(&[one])
            }
            CycleForThePrompt => {
                accepted.extend([queued(one), started(one), first]);
                cycle_init(&[prompt])
            }
            CycleBeforeAnyResult => {
                accepted.extend([queued(one), started(one)]);
                cycle_init(&[one])
            }
            CycleNamedTwice => {
                accepted.extend([queued(one), first, started(one), cycle_init(&[one])]);
                cycle_init(&[one])
            }
            PromptResultInsideSteerCycle => {
                accepted.extend([queued(one), first, started(one), cycle_init(&[one])]);
                cycle_result(prompt, &[prompt], "x")
            }
            CycleResultWithoutItsList => {
                accepted.extend([queued(one), first, started(one), cycle_init(&[one])]);
                let mut value = cycle_result(one, &[one], "x");
                value.as_object_mut().unwrap().remove("user_message_uuids");
                value
            }
            CycleResultForAnotherSteer => {
                accepted.extend([
                    queued(one),
                    queued(two),
                    first,
                    started(one),
                    cycle_init(&[one]),
                    started(two),
                ]);
                cycle_result(two, &[two], "x")
            }
            StartedBeforeQueued => started(one),
            CompletedBeforeStarted => {
                accepted.extend([queued(one)]);
                lifecycle(one, "completed")
            }
            QueuedTwice => {
                accepted.extend([queued(one)]);
                queued(one)
            }
            CancelledBeforeQueued => lifecycle(one, "cancelled"),
            UnknownState => {
                accepted.extend([queued(one)]);
                lifecycle(one, "paused")
            }
            AfterCompletion => {
                accepted.extend([queued(one), started(one), lifecycle(one, "completed")]);
                started(one)
            }
        };
        for frame in accepted {
            event(&mut driver, &mut peer, frame).await.unwrap();
        }
        assert_eq!(driver.phase(), Phase::Running, "{case:?}");
        assert_eq!(
            event(&mut driver, &mut peer, refused).await.err(),
            Some(Error::Identity),
            "{case:?}"
        );
        assert_eq!(driver.phase(), Phase::Closed, "{case:?}");
    }
}
