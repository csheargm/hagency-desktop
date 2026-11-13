use super::*;

fn written(f: &Fixture) -> Vec<String> {
    fs::read_to_string(f.path("frames"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|frame| frame["type"] == "user")
        .map(|frame| frame["message"]["content"].as_str().unwrap().to_owned())
        .collect()
}

async fn turn(f: &mut Fixture, process: &mut Process, call: &str, input: &str) -> codex::Completed {
    let (queue, _) = codex::approval_queue(1).unwrap();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        process.session.run_with_started(
            &mut f.ledger,
            "@owner:test",
            &f.scope,
            call,
            call,
            input,
            BudgetMode::Postpaid,
            &queue,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn claude_owner_command_runs_as_a_command_and_its_output_is_the_reply() {
    let mut f = Fixture::called_by(
        Kind::Claude,
        "continuous:local_command",
        ToolPolicy::Deny,
        "@owner:test",
    );
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let reply = turn(&mut f, &mut process, "first", "/cost").await;
    assert_eq!(written(&f), ["/cost"]);
    assert_eq!(reply.text, "Total cost:            $0.0000");
    assert_eq!(reply.status, codex::TurnStatus::Completed);
    // A local command makes no model request: reported, and nothing charged.
    assert!(reply.usage.is_reported());
    assert_eq!(reply.usage.input + reply.usage.output, 0);
    assert_eq!(f.spent(), (0, 0));

    // The same process takes the owner's next message as a prompt.
    assert!(process.session.can_continue());
    process
        .session
        .continue_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let reply = turn(&mut f, &mut process, "second", "hello").await;
    assert_eq!(written(&f), ["/cost", "hello"]);
    assert_eq!(reply.text, "offline reply");
    assert!(reply.usage.is_reported());
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_member_slash_text_stays_a_prompt() {
    let mut f = Fixture::new(Kind::Claude, "continuous:local_command", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let reply = turn(&mut f, &mut process, "first", "/cost").await;
    assert_eq!(
        written(&f),
        [format!("{}/cost", hagency_runtime::claude::PROMPT_LEAD)]
    );
    assert_eq!(reply.text, "offline reply");
    process.stop().await.unwrap();
}

fn saved_session(f: &Fixture) -> String {
    rusqlite::Connection::open(f.path("ledger"))
        .unwrap()
        .query_row("SELECT session FROM external_sessions", [], |row| row.get(0))
        .unwrap()
}
fn sent_sessions(f: &Fixture) -> Vec<String> {
    fs::read_to_string(f.path("frames"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|frame| frame["type"] == "user")
        .map(|frame| frame["session_id"].as_str().unwrap().to_owned())
        .collect()
}

/// Live 2026-11-13: the owner's `/clear` ended as "interrupted, result
/// unconfirmed". The CLI reports the reset and opens another session; the
/// Agent's saved conversation follows it and the room gets an answer.
#[tokio::test]
async fn claude_owner_clear_starts_a_new_conversation_that_the_agent_follows() {
    let mut f = Fixture::called_by(
        Kind::Claude,
        "continuous:local_command",
        ToolPolicy::Deny,
        "@owner:test",
    );
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    assert_eq!(turn(&mut f, &mut process, "first", "hello").await.text, "offline reply");
    assert_eq!(saved_session(&f), "offline-session");

    process
        .session
        .continue_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let reply = turn(&mut f, &mut process, "second", "/clear").await;
    assert_eq!(reply.status, codex::TurnStatus::Completed);
    assert_eq!(reply.text, "✅ 已执行 /clear：对话已清空，下一条消息开始新的对话。");
    assert_eq!(reply.usage.input + reply.usage.output, 0);
    assert_eq!(saved_session(&f), "cleared-session");

    // The next message goes to the new conversation: in the retained process
    // when it can be reused, otherwise in one that resumes the new session.
    if process.session.can_continue() {
        process
            .session
            .continue_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
    } else {
        process.stop().await.unwrap();
        process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
    }
    assert_eq!(turn(&mut f, &mut process, "third", "after").await.text, "offline reply");
    assert_eq!(written(&f), ["hello", "/clear", "after"]);
    assert_eq!(sent_sessions(&f).last().unwrap(), "cleared-session");
    assert_eq!(saved_session(&f), "cleared-session");
    process.stop().await.unwrap();

    // A later start resumes the new conversation, not the cleared one.
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    assert_eq!(turn(&mut f, &mut process, "fourth", "later").await.text, "offline reply");
    let launches = fs::read_to_string(f.path("argv")).unwrap();
    let last: Vec<String> = serde_json::from_str(launches.lines().last().unwrap()).unwrap();
    assert!(last.windows(2).any(|pair| pair[0] == "--resume" && pair[1] == "cleared-session"));
    process.stop().await.unwrap();
}
