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
