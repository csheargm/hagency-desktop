//! The model's questions through the real original, successor and budget
//! drivers, SQLite ledger and supervised child process. Only the Matrix
//! boundary is a local adapter: the posted card and its edits, and the answer
//! events that reached the Agent's device from current room members.
use super::*;
use crate::agent_questions::{AnswerValue, Card, CardState};
use crate::console::native::shared_execution::questions::Fixture as Room;
use std::collections::BTreeMap;

const PROMPT: &str = "Private encrypted request that makes the Agent ask";
const MEMBER: &str = "@alice:test";
const STRANGER: &str = "@mallory:test";
const OTHER_MEMBER: &str = "@bob:test";
/// Another Agent in the room, joined like any member.
const OTHER_AGENT: &str = "@_hagency_agt_other:test";

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn lanes() -> Vec<&'static str> {
    let mut lanes = vec!["original", "successor"];
    if cfg!(feature = "shared-budget-execution") {
        lanes.push("budget");
    }
    lanes
}

async fn prepared(lane: &str) -> (Fixture, SharedExecutionTransport, SharedWorkItem) {
    let (f, t, _) = match lane {
        #[cfg(feature = "shared-budget-execution")]
        "budget" => Box::pin(budget_attempts::prepared()).await,
        "successor" => {
            let (f, t, c) = Box::pin(attempts::predecessor()).await;
            attempts::prepare_lost_start(&f, &t, &c).await;
            (f, t, c)
        }
        _ => Box::pin(fixture()).await,
    };
    let work = {
        let mut s = f.state.lock().unwrap();
        let w = s.shared.work.as_mut().unwrap();
        w.requester_mxid = MEMBER.into();
        w.encrypted = true;
        w.body = w.event_id.clone();
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
        w.clone()
    };
    (f, t, work)
}

async fn start(f: &Fixture, t: &SharedExecutionTransport, lane: &str, mode: &str) {
    let launch = launch(f, mode);
    let progress = match lane {
        "original" => t.poll_fixture(&f.registry, launch).await,
        "successor" => t.poll_successor_fixture(&f.registry, launch).await,
        #[cfg(feature = "shared-budget-execution")]
        _ => t.poll_budget_successor_fixture(&f.registry, launch).await,
        #[cfg(not(feature = "shared-budget-execution"))]
        _ => unreachable!(),
    }
    .unwrap();
    assert!(
        matches!(progress, SharedExecutionProgress::Running { .. }),
        "{lane}"
    );
}

async fn stopped_lane(f: &Fixture, lane: &str, within: Duration) {
    tokio::time::timeout(within, async {
        loop {
            let done = {
                let s = f.state.lock().unwrap();
                match lane {
                    "original" => s.shared.processes.values().any(|r| r.state == "stopped"),
                    "successor" => s
                        .shared
                        .attempts
                        .processes
                        .values()
                        .any(|r| r.state == "stopped"),
                    #[cfg(feature = "shared-budget-execution")]
                    _ => s
                        .shared
                        .budget_attempts
                        .processes
                        .values()
                        .any(|r| r.state == "stopped"),
                    #[cfg(not(feature = "shared-budget-execution"))]
                    _ => unreachable!(),
                }
            };
            if done {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{lane}: the run must continue and stop"));
    if lane == "original" {
        f.owner.0.shared_execution.drain().await.unwrap();
    }
}

async fn card(room: &Room, lane: &str) -> (String, Card, Value) {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some((event, content)) = room.cards().into_iter().next() {
                let card = Card::from_content(&content).expect("a valid card");
                return (event, card, content);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{lane}: the question card must be posted"))
}

fn response(f: &Fixture) -> Option<Value> {
    let path = f._root.path().join("shared-probe/question-response.json");
    std::fs::read(path)
        .ok()
        .map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

/// What the driver asked Codex for when it opened the run's thread.
fn thread_request(f: &Fixture) -> Value {
    let path = f._root.path().join("shared-probe/thread-request.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// The card edits the Agent showed, once at least `n` arrived: outcomes go
/// out from the Agent's outbox, after the run moved on.
async fn edits(room: &Room, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        let edits = room.edits();
        if edits.len() >= n {
            return edits;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    room.edits()
}

fn turn_starts(f: &Fixture) -> usize {
    std::fs::read_to_string(f._root.path().join("shared-probe/methods"))
        .unwrap()
        .lines()
        .filter(|l| *l == "turn/start")
        .count()
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe; exercises all native drivers"]
async fn native_shared_question_member_answer_resumes_the_run_with_exactly_that_answer() {
    for lane in lanes() {
        let (f, t, work) = prepared(lane).await;
        let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
        let (t, room) = t.with_question_fixture(&[MEMBER, OTHER_AGENT, OTHER_MEMBER], Duration::from_secs(60));
        let _ = std::fs::remove_file(f._root.path().join("shared-probe/thread-request.json"));
        start(&f, &t, lane, "text-question").await;
        let (card_event, card, content) = card(&room, lane).await;
        // Codex offers the model its question tool only when the thread asks.
        assert_eq!(
            thread_request(&f)["config"]["features.default_mode_request_user_input"],
            true,
            "{lane}"
        );
        // The card sits in the run's thread, under the message that started it,
        // with a plain-text fallback for other clients.
        assert_eq!(card.trigger_event_id, work.event_id, "{lane}");
        assert_eq!(card.thread_root, work.thread_root, "{lane}");
        assert_eq!(
            content["m.relates_to"]["event_id"],
            work.thread_root.as_str()
        );
        assert!(content["body"].as_str().unwrap().contains("Which database"));
        assert_eq!(card.state, CardState::Pending);
        // A stranger's well-formed answer and a member's answer with a choice
        // the card never offered are both ignored; the run keeps waiting.
        let postgres = BTreeMap::from([(
            "db".to_owned(),
            AnswerValue::Option("Postgres (Recommended)".into()),
        )]);
        room.answer(
            STRANGER,
            card.answer_content(&card_event, &postgres).unwrap(),
        );
        assert_eq!(card.responders, vec![MEMBER]);
        room.answer(OTHER_MEMBER, card.answer_content(&card_event, &postgres).unwrap());
        // Another Agent in the room never answers, though it is a member.
        room.answer(
            OTHER_AGENT,
            card.answer_content(&card_event, &postgres).unwrap(),
        );
        room.answer(
            MEMBER,
            json!({"version":card.version,"cardId":card.id,"answers":{"db":{"option":"MongoDB"}},
            "m.relates_to":{"rel_type":"m.reference","event_id":card_event}}),
        );
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            response(&f).is_none(),
            "{lane}: nothing valid was answered yet"
        );
        assert!(
            sent.lock().unwrap().is_empty(),
            "{lane}: the run is still waiting"
        );
        // The first valid answer from a member wins; a later one is never read.
        let sqlite = BTreeMap::from([("db".to_owned(), AnswerValue::Option("SQLite".into()))]);
        room.answer(MEMBER, card.answer_content(&card_event, &sqlite).unwrap());
        room.answer(MEMBER, card.answer_content(&card_event, &postgres).unwrap());
        stopped_lane(&f, lane, Duration::from_secs(30)).await;
        assert_eq!(
            response(&f),
            Some(json!({"answers":{"db":{"answers":["SQLite"]}}})),
            "{lane}"
        );
        {
            let sends = sent.lock().unwrap();
            assert_eq!(sends.len(), 1, "{lane}: one reply, after the answer");
            assert_eq!(
                sends[0].1, r#"Answer: {"db":{"answers":["SQLite"]}}"#,
                "{lane}"
            );
        }
        let edits = edits(&room, 1).await;
        assert_eq!(edits.len(), 1, "{lane}");
        let answered = Card::from_content(&edits[0]["m.new_content"]).unwrap();
        assert_eq!(answered.state, CardState::Answered);
        assert_eq!(answered.answered_by.as_deref(), Some(MEMBER));
        assert_eq!(answered.answers, sqlite);
        assert_eq!(edits[0]["m.relates_to"]["event_id"], card_event.as_str());
        // The answer was never intake: one work item, one model turn.
        assert_eq!(f.state.lock().unwrap().shared.starts, 1, "{lane}");
        assert_eq!(turn_starts(&f), 1, "{lane}");
        assert!(
            f.state
                .lock()
                .unwrap()
                .shared
                .receipts
                .values()
                .any(|r| r.state == "completed" && r.usage_complete),
            "{lane}: the turn completed with its usage"
        );
        f.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe; exercises all native drivers"]
async fn native_shared_question_timeout_continues_with_no_answers() {
    for lane in lanes() {
        let (f, t, _) = prepared(lane).await;
        let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
        let (t, room) = t.with_question_fixture(&[MEMBER], Duration::from_secs(2));
        start(&f, &t, lane, "text-question").await;
        let (card_event, _, _) = card(&room, lane).await;
        stopped_lane(&f, lane, Duration::from_secs(30)).await;
        assert_eq!(
            response(&f),
            Some(json!({"answers":{}})),
            "{lane}: the model decides on its own"
        );
        assert_eq!(sent.lock().unwrap()[0].1, "Answer: {}", "{lane}");
        let edits = edits(&room, 1).await;
        assert_eq!(edits.len(), 1, "{lane}");
        assert_eq!(
            Card::from_content(&edits[0]["m.new_content"])
                .unwrap()
                .state,
            CardState::Expired
        );
        assert_eq!(edits[0]["m.relates_to"]["event_id"], card_event.as_str());
        assert_eq!(turn_starts(&f), 1, "{lane}");
        f.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_question_secret_never_reaches_the_room() {
    // A secret question is withheld from the card and answered with nothing.
    let (f, t, _) = prepared("original").await;
    let (t, _sent) = t.with_crypto_fixture(Some(PROMPT.into()));
    let (t, room) = t.with_question_fixture(&[MEMBER], Duration::from_secs(60));
    start(&f, &t, "original", "text-question-secret").await;
    let (card_event, card, content) = card(&room, "original").await;
    assert_eq!(card.questions.len(), 1);
    assert_eq!(card.withheld, 1);
    assert!(!content.to_string().contains("deploy token"));
    let pick = BTreeMap::from([(
        "db".to_owned(),
        AnswerValue::Option("Postgres (Recommended)".into()),
    )]);
    room.answer(MEMBER, card.answer_content(&card_event, &pick).unwrap());
    stopped_lane(&f, "original", Duration::from_secs(30)).await;
    assert_eq!(
        response(&f),
        Some(
            json!({"answers":{"db":{"answers":["Postgres (Recommended)"]},"token":{"answers":[]}}})
        )
    );
    f.stop().await;
    // Only secret questions, or a room the card cannot reach: no card at all,
    // and the model is answered with nothing at once.
    for (mode, reachable) in [
        ("text-question-only-secret", true),
        ("text-question", false),
    ] {
        let (f, t, _) = prepared("original").await;
        let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
        let (t, room) = t.with_question_fixture(&[MEMBER], Duration::from_secs(60));
        room.unreachable
            .store(!reachable, std::sync::atomic::Ordering::SeqCst);
        start(&f, &t, "original", mode).await;
        stopped_lane(&f, "original", Duration::from_secs(15)).await;
        assert_eq!(response(&f), Some(json!({"answers":{}})), "{mode}");
        assert!(room.cards().is_empty(), "{mode}");
        assert_eq!(sent.lock().unwrap().len(), 1, "{mode}");
        f.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe; waits across several activity leases"]
async fn native_shared_question_long_wait_keeps_the_activity_lease() {
    let mut lanes = vec!["original"];
    if cfg!(feature = "shared-budget-execution") {
        lanes.push("budget");
    }
    for lane in lanes {
        let (f, t, _) = prepared(lane).await;
        let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
        let (t, room) = t.with_question_fixture(&[MEMBER], Duration::from_secs(120));
        start(&f, &t, lane, "text-question").await;
        let (card_event, card, _) = card(&room, lane).await;
        let before = f.state.lock().unwrap().shared.activity_epoch;
        // Longer than one 25-second activity lease: the waiting run renews it
        // as usual. The wait never extends the lease by itself.
        tokio::time::sleep(Duration::from_secs(35)).await;
        let during = f.state.lock().unwrap().shared.activity_epoch;
        assert!(
            during >= before + 3,
            "{lane}: renewed while waiting ({before} -> {during})"
        );
        assert!(
            sent.lock().unwrap().is_empty(),
            "{lane}: still waiting for the room"
        );
        let name = BTreeMap::from([("db".to_owned(), AnswerValue::Text("CockroachDB".into()))]);
        room.answer(MEMBER, card.answer_content(&card_event, &name).unwrap());
        stopped_lane(&f, lane, Duration::from_secs(30)).await;
        assert_eq!(
            response(&f),
            Some(
                json!({"answers":{"db":{"answers":["None of the above","user_note: CockroachDB"]}}})
            ),
            "{lane}"
        );
        assert_eq!(turn_starts(&f), 1, "{lane}");
        f.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_question_codex_approval_prompt_is_declined_without_a_card() {
    // Codex 0.166 asks an MCP tool approval as a question when MCP
    // elicitation is off. Room members must never grant it.
    let (f, t, _) = prepared("original").await;
    let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
    let (t, room) = t.with_question_fixture(&[MEMBER], Duration::from_secs(60));
    start(&f, &t, "original", "text-question-mcp-approval").await;
    stopped_lane(&f, "original", Duration::from_secs(15)).await;
    assert_eq!(
        response(&f),
        Some(json!({"answers":{}})),
        "declined: no answer allows nothing"
    );
    assert!(room.cards().is_empty(), "never shown to the room");
    assert_eq!(sent.lock().unwrap().len(), 1);
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_question_device_opening_comes_out_of_the_same_wait() {
    // The Agent's device must be opened first and takes longer than the
    // whole wait: the run stops waiting on time, posts nothing, and the model
    // decides on its own. The opening itself carries on.
    let wait = Duration::from_secs(4);
    let (f, t, _) = prepared("original").await;
    let (t, sent) = t.with_crypto_fixture(Some(PROMPT.into()));
    let (t, room) = t.with_question_fixture(&[MEMBER], wait);
    room.device_open
        .store(false, std::sync::atomic::Ordering::SeqCst);
    *room.open_delay.lock().unwrap() = wait + Duration::from_secs(3);
    let began = std::time::Instant::now();
    start(&f, &t, "original", "text-question").await;
    stopped_lane(&f, "original", Duration::from_secs(30)).await;
    assert!(
        began.elapsed() < wait + Duration::from_secs(3),
        "the queue waited only the run's own wait"
    );
    assert_eq!(response(&f), Some(json!({"answers":{}})));
    assert!(room.cards().is_empty());
    assert_eq!(sent.lock().unwrap().len(), 1);
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(
        room.device_open.load(std::sync::atomic::Ordering::SeqCst),
        "the opening was never cut short"
    );
    f.stop().await;

    // An opening that ends within the wait: the card is posted with what is
    // left of the wait, and its expiry is the run's own deadline.
    let wait = Duration::from_secs(20);
    let (f, t, _) = prepared("original").await;
    let (t, _sent) = t.with_crypto_fixture(Some(PROMPT.into()));
    let (t, room) = t.with_question_fixture(&[MEMBER], wait);
    room.device_open
        .store(false, std::sync::atomic::Ordering::SeqCst);
    *room.open_delay.lock().unwrap() = Duration::from_secs(3);
    let asked_by = now_ms() + wait.as_millis() as u64;
    start(&f, &t, "original", "text-question").await;
    let (_, card, _) = card(&room, "original").await;
    let posted = now_ms();
    assert!(
        card.expires_at_ms <= asked_by + 1_000,
        "expiry is counted from the question, not the post"
    );
    assert!(
        card.expires_at_ms + 2_000 < posted + wait.as_millis() as u64,
        "the opening came out of the wait"
    );
    stopped_lane(&f, "original", Duration::from_secs(40)).await;
    assert_eq!(response(&f), Some(json!({"answers":{}})));
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_question_plaintext_room_uses_only_an_open_device() {
    // Keep the base fixture's required V3 history adapter. The question path
    // must not open a device for a plaintext room; it can ask only when the
    // question device is already open.
    for open in [false, true] {
        let (f, t, _) = Box::pin(fixture()).await;
        let requester = f.state.lock().unwrap().shared.work.as_ref().unwrap().requester_mxid.clone();
        assert!(
            !f.state
                .lock()
                .unwrap()
                .shared
                .work
                .as_ref()
                .unwrap()
                .encrypted
        );
        let (t, room) = t.with_question_fixture(&[&requester], Duration::from_secs(60));
        room.device_open
            .store(open, std::sync::atomic::Ordering::SeqCst);
        start(&f, &t, "original", "text-question").await;
        if open {
            let (card_event, card, _) = card(&room, "plaintext").await;
            assert_eq!(card.responders, vec![requester.clone()]);
            let pick = BTreeMap::from([("db".to_owned(), AnswerValue::Option("SQLite".into()))]);
            room.answer(&requester, card.answer_content(&card_event, &pick).unwrap());
        }
        stopped_lane(&f, "original", Duration::from_secs(30)).await;
        if open {
            assert_eq!(
                response(&f),
                Some(json!({"answers":{"db":{"answers":["SQLite"]}}}))
            );
        } else {
            assert_eq!(
                response(&f),
                Some(json!({"answers":{}})),
                "no device opened for a plaintext room"
            );
            assert!(room.cards().is_empty());
            assert!(!room.device_open.load(std::sync::atomic::Ordering::SeqCst));
        }
        f.stop().await;
    }
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_question_card_whose_post_response_was_lost_is_closed() {
    // The room got the card but its response never came back in time: the
    // model decides on its own at once, and the card's own transaction later
    // finds that card and closes it.
    let (f, t, _) = prepared("original").await;
    let (t, _sent) = t.with_crypto_fixture(Some(PROMPT.into()));
    let (t, room) = t.with_question_fixture(&[MEMBER], Duration::from_secs(6));
    room.lost_responses
        .store(1, std::sync::atomic::Ordering::SeqCst);
    start(&f, &t, "original", "text-question").await;
    stopped_lane(&f, "original", Duration::from_secs(30)).await;
    assert_eq!(response(&f), Some(json!({"answers":{}})));
    let (card_event, card, _) = card(&room, "original").await;
    assert_eq!(card.state, CardState::Pending, "the card reached the room");
    let edits = edits(&room, 1).await;
    assert_eq!(edits.len(), 1, "closed exactly once");
    assert_eq!(edits[0]["m.relates_to"]["event_id"], card_event.as_str());
    assert_eq!(
        Card::from_content(&edits[0]["m.new_content"])
            .unwrap()
            .state,
        CardState::Expired
    );
    assert_eq!(room.cards().len(), 1, "never posted a second time");
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_question_slow_answer_read_cannot_extend_or_answer_after_expiry() {
    let (f, t, _) = prepared("original").await;
    let (t, _sent) = t.with_crypto_fixture(Some(PROMPT.into()));
    let (t, room) = t.with_question_fixture(&[MEMBER], Duration::from_secs(1));
    *room.answer_delay.lock().unwrap() = Duration::from_millis(1500);
    start(&f, &t, "original", "text-question").await;
    let (event, card, _) = card(&room, "original").await;
    let pick = BTreeMap::from([("db".into(), AnswerValue::Option("SQLite".into()))]);
    room.answer(MEMBER, card.answer_content(&event, &pick).unwrap());
    stopped_lane(&f, "original", Duration::from_secs(15)).await;
    assert_eq!(
        response(&f),
        Some(json!({"answers":{}})),
        "the read never completed within the card's wait"
    );
    let edits = edits(&room, 1).await;
    assert_eq!(
        Card::from_content(&edits[0]["m.new_content"])
            .unwrap()
            .state,
        CardState::Expired
    );
    f.stop().await;
}

#[tokio::test]
#[ignore = "requires explicit offline supervised runtime probe"]
async fn native_shared_question_without_a_recovery_record_is_not_posted() {
    let (f, t, _) = prepared("original").await;
    let (t, _sent) = t.with_crypto_fixture(Some(PROMPT.into()));
    let records = f._root.path().join("broken-question-records.json");
    hagency_store::private::replace(&records, b"unreadable previous records").unwrap();
    let (t, room) =
        t.with_question_fixture_records(&[MEMBER], Duration::from_secs(10), Some(records.clone()));
    start(&f, &t, "original", "text-question").await;
    stopped_lane(&f, "original", Duration::from_secs(15)).await;
    assert_eq!(response(&f), Some(json!({"answers":{}})));
    assert!(
        room.cards().is_empty(),
        "never post a card whose restart cleanup cannot be retained"
    );
    assert_eq!(
        std::fs::read(records).unwrap(),
        b"unreadable previous records"
    );
    f.stop().await;
}
