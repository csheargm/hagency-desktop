use super::*;
use hagency_runtime::live_text::{LiveText, MessageError, MessageSink};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Messages(Mutex<Vec<(String, String)>>);
impl MessageSink for Messages {
    fn record(&self, id: &str, text: &str) -> Result<(), MessageError> {
        self.0.lock().unwrap().push((id.into(), text.into()));
        Ok(())
    }
}

#[tokio::test]
async fn claude_background_followup_preserves_the_original_answer_as_a_separate_message() {
    let mut f = Fixture::new(Kind::Claude, "background_native_followup", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let messages = Arc::new(Messages::default());
    let queue = queue.with_live_text(LiveText::with_messages(messages.clone()));
    let done = process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "call",
            "start background work",
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    assert_eq!(done.text, "native background final answer");
    let records = messages.0.lock().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].1, "offline reply");
    assert!(records[0].0.starts_with("claude:result:"));
    drop(records);
    process.stop().await.unwrap();
}

#[tokio::test]
async fn octos_can_report_twice_during_one_turn_and_keep_its_final_answer() {
    let mut f = Fixture::new(Kind::Octos, "octos_progress", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let messages = Arc::new(Messages::default());
    let queue = queue.with_live_text(LiveText::with_messages(messages.clone()));
    let done = process
        .session
        .run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            "call",
            "dispatch",
            "work on the task",
            BudgetMode::Postpaid,
            &queue,
            None,
        )
        .await
        .unwrap();
    assert_eq!(done.text, "offline reply");
    assert!(
        fs::read_to_string(f.path("turn-input"))
            .unwrap()
            .contains("hagency_report_progress")
    );
    let records = messages.0.lock().unwrap();
    assert_eq!(
        &*records,
        &[
            ("octos:tool:progress-1".into(), "first finding".into()),
            ("octos:tool:progress-2".into(), "second finding".into())
        ]
    );
    drop(records);
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_delivers_each_intermediate_message_before_its_tool_finishes() {
    let mut f = Fixture::new(Kind::Claude, "spontaneous_messages", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, _) = codex::approval_queue(1).unwrap();
    let messages = Arc::new(Messages::default());
    let queue = queue.with_live_text(LiveText::with_messages(messages.clone()));
    let marker = f.path("progress-observed");
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "work on the task",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let witness = async {
        loop {
            if messages
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|(_, text)| text == "First finding")
            {
                fs::write(&marker, "observed").unwrap();
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    };
    let (done, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        tokio::join!(run, witness)
    })
    .await
    .unwrap();
    assert_eq!(done.unwrap().text, "offline reply");
    let rows = messages.0.lock().unwrap();
    assert_eq!(
        &*rows,
        &[
            ("claude:assistant:progress-a".into(), "First finding".into()),
            (
                "claude:assistant:progress-b".into(),
                "Second finding".into()
            )
        ]
    );
    drop(rows);
    process.stop().await.unwrap();
}
