//! A pooled Claude Agent's question to its room, through the real host, the
//! SQLite ledger and the supervised process. Only the Matrix boundary is a
//! local adapter: the posted card, its edits and the members' answer events.
use super::*;
use crate::agent_questions::{AnswerValue, Card, CardState};
use std::collections::BTreeMap;

const MEMBER: &str = "@alice:test";
const OTHER_MEMBER: &str = "@bob:test";
const QUESTION: &str = "Which database should this use?";

/// Whether the offline CLI has its question's response already.
fn answered(root: &std::path::Path) -> bool {
    std::fs::read_dir(root).is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            let path = entry.path();
            if path.is_dir() {
                answered(&path)
            } else {
                path.extension().is_some_and(|e| e == "question")
            }
        })
    })
}

#[tokio::test]
#[ignore = "explicit offline Claude probe; real host, native question callback and canonical ledger"]
async fn native_shared_claude_full_host_question_card_gives_the_requesters_answer_to_the_turn() {
    let binding = AccountBinding::from_authenticated_profile(
        "01234567-89ab-cdef-0123-456789abcdef",
        "89abcdef-0123-4567-89ab-cdef01234567",
        "owner@example.test",
        Instant::now() + Duration::from_secs(45),
    )
    .unwrap();
    let (f, transport, reserve) =
        prepared_with(Fixture::with_provider(PROVIDER, binding.subject()).await).await;
    let (f, transport, mut command) = runtime_fixture(f, transport, reserve).await;
    command.version = 4;
    command.capability = SHARED_CLAUDE_CAPABILITY.into();
    command.policy = SHARED_CLAUDE_POLICY.into();
    let work = {
        let mut state = f.state.lock().unwrap();
        state.shared.command = Some(command.clone());
        let w = state.shared.work.as_mut().unwrap();
        w.runtime_command_digest = digest(&command).unwrap();
        w.requester_mxid = MEMBER.into();
        w.encrypted = true;
        w.body = w.event_id.clone();
        w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
        w.clone()
    };
    let root = f._root.path().join("claude-question");
    hagency_store::private::directory(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let (transport, sent) = transport.with_crypto_fixture(Some("Ask before choosing".into()));
    let (transport, room) =
        transport.with_question_fixture(&[MEMBER, OTHER_MEMBER], Duration::from_secs(60));
    assert!(matches!(
        transport
            .poll_claude_fixture(
                &f.registry,
                command.reserve_command.resource_id.clone(),
                root.clone(),
                binding,
                "shared-question",
            )
            .await
            .unwrap(),
        SharedExecutionProgress::Running { .. }
    ));
    let (card_event, content) = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(posted) = room.cards().into_iter().next() {
                return posted;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the question card must be posted in the room");
    let card = Card::from_content(&content).expect("a valid card");
    // The card sits under the message that started the run, and only the
    // member who asked may answer it.
    assert_eq!(card.trigger_event_id, work.event_id);
    assert_eq!(card.thread_root, work.thread_root);
    assert_eq!(card.responders, vec![MEMBER]);
    assert_eq!(card.state, CardState::Pending);
    assert!(content["body"].as_str().unwrap().contains(QUESTION));
    let id = card.questions[0].id.clone();
    let postgres = BTreeMap::from([(
        id.clone(),
        AnswerValue::Option("Postgres (Recommended)".into()),
    )]);
    let sqlite = BTreeMap::from([(id, AnswerValue::Option("SQLite".into()))]);
    // Another member's well-formed answer is not this run's answer.
    room.answer(OTHER_MEMBER, card.answer_content(&card_event, &postgres).unwrap());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !answered(&root),
        "the CLI's question is still waiting"
    );
    assert!(sent.lock().unwrap().is_empty(), "the run is still waiting");
    // The requester's first answer wins; a later one is never read.
    room.answer(MEMBER, card.answer_content(&card_event, &sqlite).unwrap());
    room.answer(MEMBER, card.answer_content(&card_event, &postgres).unwrap());
    stopped(&f).await;
    let response: Value = serde_json::from_slice(
        &std::fs::read(native_marker(&root, "shared-claude.question")).unwrap(),
    )
    .unwrap();
    // The native callback gets its own input back with the answer added: an
    // answer never becomes a new prompt or a tool permission.
    let decision = &response["response"]["response"];
    assert_eq!(response["response"]["request_id"], "owned-question");
    assert_eq!(decision["behavior"], "allow");
    assert_eq!(decision["updatedInput"]["answers"], json!({QUESTION: "SQLite"}));
    assert_eq!(decision["updatedInput"]["questions"][0]["question"], QUESTION);
    {
        let sends = sent.lock().unwrap();
        assert_eq!(sends.len(), 1, "one reply, after the answer: {sends:?}");
        assert_eq!(sends[0].1, format!(r#"Answer: {{"{QUESTION}":"SQLite"}}"#));
    }
    let edits = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let edits = room.edits();
            if !edits.is_empty() {
                return edits;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the card shows its outcome");
    let answered = Card::from_content(&edits[0]["m.new_content"]).unwrap();
    assert_eq!(answered.state, CardState::Answered);
    assert_eq!(answered.answered_by.as_deref(), Some(MEMBER));
    assert_eq!(answered.answers, sqlite);
    assert_eq!(edits[0]["m.relates_to"]["event_id"], card_event.as_str());
    {
        // The answer was never intake: one work item, one model turn, billed once.
        let state = f.state.lock().unwrap();
        assert_eq!(state.shared.starts, 1);
        let receipt = state
            .shared
            .receipts
            .values()
            .max_by_key(|r| r.ledger_revision)
            .unwrap();
        assert_eq!(receipt.state, "completed");
        assert!(receipt.usage_complete);
        assert_eq!(receipt.known_consumed_tokens, 16);
    }
    f.stop().await;
}
