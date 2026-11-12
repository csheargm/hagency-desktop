use super::*;

async fn ready() -> (Session, Peer) {
    let (mut session, mut peer) = fixture(true);
    initialize(&mut session, &mut peer).await;
    session.enable_room_history().unwrap();
    let (started, request) = tokio::join!(
        session.start_thread(),
        exchange(&mut peer, thread_result(true), vec![])
    );
    started.unwrap();
    assert_eq!(
        request["params"]["dynamicTools"][0]["name"],
        "read_room_history"
    );
    assert_eq!(request["params"]["sandbox"], "read-only");
    // Room history must not force an early compaction point (see session.rs).
    assert!(request["params"]["config"]
        .get("model_auto_compact_token_limit")
        .is_none());
    let (started, _) = tokio::join!(
        session.start_turn("Summarize this room".into()),
        exchange(&mut peer, json!({"turn":turn("inProgress")}), vec![])
    );
    started.unwrap();
    (session, peer)
}
fn request(thread: &str, tool: &str) -> Value {
    json!({"id":91,"method":"item/tool/call","params":{"threadId":thread,"turnId":"turn-one","callId":"history-call","tool":tool,"arguments":{"cursor":null}}})
}

async fn file_ready() -> (Session, Peer) {
    let (mut session, mut peer) = fixture(false);
    initialize(&mut session, &mut peer).await;
    session.enable_room_history().unwrap();
    session.enable_room_files().unwrap();
    let (started, request) = tokio::join!(session.start_thread(), exchange(&mut peer, thread_result(false), vec![]));
    started.unwrap();
    let tools=request["params"]["dynamicTools"].as_array().unwrap();
    assert_eq!(tools.len(),5);
    for name in hagency_runtime::codex::session::ROOM_FILE_TOOLS { assert!(tools.iter().any(|t|t["name"]==name)); }
    let (started, _) = tokio::join!(session.start_turn("Read my file and send the result".into()), exchange(&mut peer,json!({"turn":turn("inProgress")}),vec![]));
    started.unwrap();
    assert!(session.enable_room_files().is_err());
    (session,peer)
}

#[tokio::test]
async fn native_room_file_callback_is_scoped_bounded_and_answered_once() {
    let (mut session,mut peer)=file_ready().await;
    let mut message=request("thread-one","receive_file");
    message["params"]["arguments"]=json!({"event_id":"$attachment:test"});
    write(&mut peer,message).await;
    let Update::RoomFile(call)=session.next_update().await.unwrap() else { panic!("file callback missing") };
    assert!(session.respond_room_file(&call,"x".repeat(13*1024)).await.is_err());
    session.respond_room_file(&call,"{\"path\":\"received.pdf\"}".into()).await.unwrap();
    let response=read(&mut peer.stdin).await;
    assert_eq!(response["id"],91);
    assert_eq!(response["result"]["success"],true);
    assert!(session.respond_room_file(&call,"duplicate".into()).await.is_err());
}

#[tokio::test]
async fn native_room_file_tools_require_writable_opt_in_and_reject_scope_injection() {
    let (mut readonly,mut peer)=fixture(true);
    initialize(&mut readonly,&mut peer).await;
    assert!(readonly.enable_room_files().is_err());
    for (thread,args) in [
        ("foreign",json!({"event_id":"$file:test"})),
        ("thread-one",json!({"event_id":"$file:test","room_id":"!other:test"})),
        ("thread-one",json!({"event_id":"$file:test","url":"https://attacker.test"})),
    ] {
        let (mut session,mut peer)=file_ready().await;
        let mut message=request(thread,"receive_file");
        message["params"]["arguments"]=args;
        write(&mut peer,message).await;
        assert!(session.next_update().await.is_err());
    }
    let (mut session,mut peer)=ready().await;
    let mut message=request("thread-one","receive_file");
    message["params"]["arguments"]=json!({"event_id":"$file:test"});
    write(&mut peer,message).await;
    assert!(session.next_update().await.is_err(),"text-only room history cannot escalate to file writes");
}

#[tokio::test]
async fn native_room_file_callback_arriving_during_approval_remains_answerable() {
    use hagency_runtime::codex::session::{ApprovalControlPolicy,PreparedUpdate};
    let (mut session,mut peer)=file_ready().await;
    session.enable_approval_control(ApprovalControlPolicy {owner_wait_ms:5000,response_reserve_ms:500}).unwrap();
    write(&mut peer,approval_event(json!(7))).await;
    let Update::Approval(approval)=session.next_update().await.unwrap() else {panic!("approval missing")};
    let mut prepared=session.prepare_approval(approval.response(false)).unwrap();
    let mut message=request("thread-one","receive_file");
    message["params"]["arguments"]=json!({"event_id":"$attachment:test"});
    write(&mut peer,message).await;
    let PreparedUpdate::Update(Update::RoomFile(call),_)=session.send_prepared_approval(&mut prepared).await.unwrap() else {
        panic!("file callback must be yielded before the approval write");
    };
    assert!(matches!(session.send_prepared_approval(&mut prepared).await.unwrap(),PreparedUpdate::WriteAccepted(_)));
    assert_eq!(read(&mut peer.stdin).await["id"],7);
    session.respond_room_file(&call,"{\"status\":\"ready\"}".into()).await.unwrap();
    assert_eq!(read(&mut peer.stdin).await["id"],91);
}

#[tokio::test]
async fn native_room_history_tool_answers_only_original_read_callback_once() {
    let (mut session, mut peer) = ready().await;
    write(&mut peer, request("thread-one", "read_room_history")).await;
    let (update, observation) = session.next_observed_update().await.unwrap();
    assert!(matches!(
        observation.kind(),
        hagency_runtime::codex::session::ObservationKind::Ignored
    ));
    let Update::RoomHistory(call) = update else {
        panic!("history callback missing")
    };
    assert!(call.cursor.is_none());
    assert!(call.is_root());
    assert_eq!((call.thread_id(), call.turn_id()), ("thread-one", "turn-one"));
    assert!(
        session
            .respond_room_history(&call, "x".repeat(13 * 1024))
            .await
            .is_err()
    );
    session
        .respond_room_history(&call, "{\"complete\":true}".into())
        .await
        .unwrap();
    let wire = read(&mut peer.stdin).await;
    assert_eq!(wire["id"], 91);
    assert_eq!(wire["result"]["success"], true);
    assert_eq!(wire["result"]["contentItems"][0]["type"], "inputText");
    assert!(
        session
            .respond_room_history(&call, "duplicate".into())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn native_room_history_callback_retains_descendant_context() {
    let (mut session, mut peer) = ready().await;
    update(&mut session, &mut peer, note("turn/started", json!({
        "threadId":"child","turn":{"id":"child-turn","rootTurnId":"turn-one","status":"inProgress","items":[]}
    }))).await.unwrap();
    let mut message = request("child", "read_room_history");
    message["params"]["turnId"] = "child-turn".into();
    let Update::RoomHistory(call) = update(&mut session, &mut peer, message).await.unwrap() else {
        panic!("child history callback missing");
    };
    assert!(!call.is_root());
    assert_eq!((call.thread_id(), call.turn_id()), ("child", "child-turn"));
    session.respond_room_history(&call, "{\"complete\":true}".into()).await.unwrap();
    assert_eq!(read(&mut peer.stdin).await["id"], 91);
}

#[tokio::test]
async fn native_room_history_tool_rejects_foreign_turn_and_unregistered_tool() {
    for (thread, tool) in [
        ("foreign", "read_room_history"),
        ("thread-one", "send_message"),
    ] {
        let (mut session, mut peer) = ready().await;
        write(&mut peer, request(thread, tool)).await;
        assert!(session.next_update().await.is_err());
    }
}

#[tokio::test]
async fn native_room_history_pagination_survives_compaction_and_more_than_128_items() {
    let (mut session, mut peer) = ready().await;
    for index in 0..150 {
        for (method, status) in [
            ("item/started", "inProgress"),
            ("item/completed", "completed"),
        ] {
            let timestamp = if method == "item/started" {
                "startedAtMs"
            } else {
                "completedAtMs"
            };
            write(&mut peer,note(method,json!({"threadId":"thread-one","turnId":"turn-one",timestamp:10,"item":{"id":format!("history-{index}"),"type":"dynamicToolCall","tool":"read_room_history","arguments":{"cursor":null},"status":status}}))).await;
            assert!(matches!(
                session.next_update().await.unwrap(),
                Update::Item { .. }
            ));
        }
        if index == 75 {
            write(
                &mut peer,
                note(
                    "thread/compacted",
                    json!({"threadId":"thread-one","turnId":"turn-one"}),
                ),
            )
            .await;
            assert!(matches!(
                session.next_update().await.unwrap(),
                Update::Progress
            ));
        }
    }
    assert_eq!(session.phase(), Phase::Running);
}
