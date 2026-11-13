use super::*;

// Frames as CLI 2.1.323 writes them for the owner's `/clear` on a resumed
// session (offline capture 2026-11-13: throwaway home, local fake model).
// The reset and the emptied task list come under the old session id; init,
// result, the command's end and idle come under a new one.
fn on(session: &str, mut value: Value) -> Value {
    value["session_id"] = session.into();
    value
}
fn tied(mut value: Value, prompt: &Value) -> Value {
    value["user_message_uuid"] = prompt["uuid"].clone();
    value["user_message_uuids"] = json!([prompt["uuid"]]);
    value
}
fn command(state: &str, prompt: &Value) -> Value {
    json!({"type":"command_lifecycle","command_uuid":prompt["uuid"],"state":state,
        "session_id":"session-one"})
}
fn reset(prompt: &Value) -> Value {
    json!({"type":"conversation_reset","new_conversation_id":"conversation-two",
        "uuid":"conversation-two","trigger":"clear","user_message_uuid":prompt["uuid"],
        "timestamp":"2026-11-13T02:37:07.395Z","session_id":"session-one"})
}
fn cleared() -> Value {
    json!({"type":"result","subtype":"success","is_error":false,"result":"",
        "local_command":"clear","num_turns":0,"session_id":"session-two","modelUsage":{},
        "usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":0,
            "cache_read_input_tokens":0}})
}
async fn resumed() -> (Driver, Peer) {
    let (mut driver, mut peer) = pair(16384, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    // One finished turn binds the session, as a resumed start does.
    let first = continuous::next_prompt(&mut driver, &mut peer).await;
    continuous::completed(&mut driver, &mut peer, &first).await;
    driver.begin_followup(limits()).unwrap();
    (driver, peer)
}
async fn frames(
    driver: &mut Driver,
    peer: &mut Peer,
    values: Vec<Value>,
) -> Result<Message, Error> {
    let bytes: Vec<u8> = values.into_iter().flat_map(encoded).collect();
    let (result, write) = tokio::join!(driver.next_message(), peer.output.write_all(&bytes));
    write.unwrap();
    result
}
fn is_init(message: &Message) -> bool {
    matches!(message, Message::Event { kind: EventKind::System, payload, .. } if payload["subtype"] == "init")
}

#[tokio::test(start_paused = true)]
async fn native_claude_owner_clear_moves_the_session_to_the_new_conversation() {
    let (mut driver, mut peer) = resumed().await;
    let (sent, prompt) = tokio::join!(driver.owner_command("/clear"), read(&mut peer));
    sent.unwrap();
    assert_eq!(prompt["message"]["content"], "/clear");
    assert_eq!(prompt["session_id"], "session-one");
    assert!(!driver.conversation_reset());
    let message = frames(
        &mut driver,
        &mut peer,
        vec![
            state("running"),
            command("queued", &prompt),
            command("started", &prompt),
            reset(&prompt),
            background(false),
            tied(on("session-two", init()), &prompt),
        ],
    )
    .await
    .unwrap();
    assert!(is_init(&message));
    assert_eq!(driver.session_id(), Some("session-two"));
    assert!(driver.conversation_reset());

    // The rest of the command is read under the new id, and it completes.
    event(&mut driver, &mut peer, tied(cleared(), &prompt))
        .await
        .unwrap();
    event(
        &mut driver,
        &mut peer,
        on("session-two", command("completed", &prompt)),
    )
    .await
    .unwrap();
    event(&mut driver, &mut peer, on("session-two", state("idle")))
        .await
        .unwrap();
    assert_eq!(driver.phase(), Phase::ResultObserved);

    // The same process takes the next prompt in the new conversation.
    assert!(driver.can_followup());
    tokio::time::advance(Duration::from_secs(31)).await;
    driver.begin_followup(limits()).unwrap();
    let (sent, next) = tokio::join!(driver.prompt("next turn"), read(&mut peer));
    sent.unwrap();
    assert_eq!(next["session_id"], "session-two");
    let message = frames(
        &mut driver,
        &mut peer,
        vec![
            on("session-two", state("running")),
            tied(on("session-two", init()), &next),
        ],
    )
    .await
    .unwrap();
    assert!(is_init(&message));
    assert_eq!(driver.session_id(), Some("session-two"));
    assert!(
        !driver.conversation_reset(),
        "one command's reset, not the session's"
    );
    // The old conversation is gone for this process.
    assert!(matches!(
        event(&mut driver, &mut peer, tied(result(), &next)).await,
        Err(Error::Identity)
    ));
}

#[tokio::test(start_paused = true)]
async fn native_claude_only_the_owners_command_may_reset_the_conversation() {
    // A prompt: a reset frame is not read, and another session at init still
    // ends the session, as it did before the frame was known.
    let (mut driver, mut peer) = resumed().await;
    let (sent, prompt) = tokio::join!(driver.prompt("/clear"), read(&mut peer));
    sent.unwrap();
    assert!(matches!(
        frames(
            &mut driver,
            &mut peer,
            vec![
                state("running"),
                reset(&prompt),
                tied(on("session-two", init()), &prompt),
            ],
        )
        .await,
        Err(Error::Identity)
    ));
    assert!(!driver.conversation_reset());
    assert_eq!(driver.phase(), Phase::Closed);

    // The owner's command, but no reset was reported: the id may not change.
    let (mut driver, mut peer) = resumed().await;
    let (sent, prompt) = tokio::join!(driver.owner_command("/cost"), read(&mut peer));
    sent.unwrap();
    assert!(matches!(
        frames(
            &mut driver,
            &mut peer,
            vec![state("running"), tied(on("session-two", init()), &prompt)],
        )
        .await,
        Err(Error::Identity)
    ));

    // A reset that names another message or another session is not this
    // command's reset.
    for wrong in ["user_message_uuid", "session_id"] {
        let (mut driver, mut peer) = resumed().await;
        let (sent, prompt) = tokio::join!(driver.owner_command("/clear"), read(&mut peer));
        sent.unwrap();
        let mut frame = reset(&prompt);
        frame[wrong] = "another".into();
        assert!(
            matches!(
                frames(&mut driver, &mut peer, vec![state("running"), frame]).await,
                Err(Error::Identity)
            ),
            "{wrong}"
        );
        assert_eq!(driver.phase(), Phase::Closed);
    }
}

#[tokio::test(start_paused = true)]
async fn native_claude_owner_command_status_before_init_is_not_a_fault() {
    // `/compact` on CLI 2.1.323 reports its progress before init.
    let (mut driver, mut peer) = resumed().await;
    let (sent, prompt) = tokio::join!(driver.owner_command("/compact"), read(&mut peer));
    sent.unwrap();
    let status = |value: Value| {
        let mut frame = json!({"type":"system","subtype":"status","session_id":"session-one"});
        frame["status"] = value;
        frame
    };
    let mut done = status(Value::Null);
    done["compact_result"] = "success".into();
    let message = frames(
        &mut driver,
        &mut peer,
        vec![
            state("running"),
            command("queued", &prompt),
            command("started", &prompt),
            status("compacting".into()),
            done,
            tied(init(), &prompt),
        ],
    )
    .await
    .unwrap();
    assert!(is_init(&message));
    assert_eq!(driver.session_id(), Some("session-one"));
    assert!(!driver.conversation_reset());

    // A prompt keeps the rule it had: such a frame before init ends it.
    let (mut driver, mut peer) = resumed().await;
    let (sent, prompt) = tokio::join!(driver.prompt("hello"), read(&mut peer));
    sent.unwrap();
    assert!(matches!(
        frames(
            &mut driver,
            &mut peer,
            vec![status("compacting".into()), tied(init(), &prompt)],
        )
        .await,
        Err(Error::Identity)
    ));
}
