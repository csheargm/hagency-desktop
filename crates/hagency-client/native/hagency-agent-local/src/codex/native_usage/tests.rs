use super::*;
use std::io::Write;
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

type TestSession = Session<ReadHalf<DuplexStream>, WriteHalf<DuplexStream>>;

fn append(path: &std::path::Path, value: &Value) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{value}").unwrap();
}
fn receipt(thread: &str, turn: &str, root: &str) -> Value {
    let usage = value!({"total_tokens":14,"input_tokens":10,"output_tokens":4,
        "cached_input_tokens":2,"reasoning_output_tokens":1});
    value!({"type":"token_usage_record","payload":{
        "thread_id":thread,"turn_id":turn,"root_turn_id":root,
        "response_id":format!("{thread}-{turn}"),"usage":usage,
        "turn_token_usage":usage,"thread_token_usage":{"total_tokens":999999}
    }})
}
fn finish(turn: &str) -> Value {
    value!({"type":"event_msg","payload":{"type":"task_complete","turn_id":turn}})
}
fn completed_tree(child: bool) -> TurnTree {
    let mut tree = TurnTree::new("root", "turn").unwrap();
    if child {
        tree.observe(
            "turn/started",
            &value!({"threadId":"child",
            "turn":{"id":"child-turn","rootTurnId":"turn"}}),
            false,
        )
        .unwrap();
        tree.observe(
            "turn/completed",
            &value!({"threadId":"child",
            "turn":{"id":"child-turn","rootTurnId":"turn","status":"completed"}}),
            false,
        )
        .unwrap();
    }
    tree.observe(
        "turn/completed",
        &value!({"threadId":"root",
        "turn":{"id":"turn","status":"completed"}}),
        false,
    )
    .unwrap();
    tree
}
async fn fixture() -> (tempfile::TempDir, TestSession, DuplexStream) {
    let dir = tempfile::tempdir().unwrap();
    for thread in ["root", "child"] {
        std::fs::write(
            dir.path().join(thread),
            format!(
                "{}\n",
                value!({"type":"session_meta","payload":{"id":thread}})
            ),
        )
        .unwrap();
    }
    let (client, peer) = tokio::io::duplex(8192);
    let (reader, writer) = tokio::io::split(client);
    let mut session = Session::new(reader, writer);
    session.legacy_usage = false;
    session.thread = Some("root".into());
    session.native_rollout = Some(
        NativeUsageCursor::from_thread_reply(
            &value!({"thread":{"id":"root","path":dir.path().join("root")}}),
            "root",
            true,
        )
        .await
        .unwrap(),
    );
    (dir, session, peer)
}

#[tokio::test]
async fn cold_native_usage_reconciles_deferred_and_buffered_notifications() {
    for case in [
        "duplicate",
        "conflicting-deferred",
        "conflicting-buffered",
        "new-child",
        "callback",
        "wrong-metadata",
    ] {
        let (dir, mut session, peer) = fixture().await;
        for (thread, turn) in [("root", "turn"), ("child", "child-turn")] {
            append(&dir.path().join(thread), &receipt(thread, turn, "turn"));
            append(&dir.path().join(thread), &finish(turn));
        }
        let mut tree = completed_tree(true);
        let metadata = dir.path().join("child");
        let server = async move {
            let (reader, mut writer) = tokio::io::split(peer);
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["method"], "thread/read");
            assert_eq!(request["params"]["threadId"], "child");
            let mut late = value!({"method":"rawResponse/completed","params":{
                "threadId":"root","turnId":"turn","responseId":"root-turn",
                "usage":{"totalTokens":14,"inputTokens":10,"outputTokens":4,
                    "cachedInputTokens":2,"reasoningOutputTokens":1}}});
            if case.starts_with("conflicting") {
                late["params"]["usage"]["totalTokens"] = 15.into();
                late["params"]["usage"]["inputTokens"] = 11.into();
            } else if case == "new-child" {
                late = value!({"method":"turn/started","params":{"threadId":"later-child",
                    "turn":{"id":"later-turn","rootTurnId":"turn"}}});
            } else if case == "callback" {
                late = value!({"id":900,"method":"item/commandExecution/requestApproval",
                    "params":{"threadId":"root","turnId":"turn","command":"touch file"}});
            }
            let reply = value!({"id":request["id"],"result":{"thread":{
                "id":if case == "wrong-metadata" { "foreign" } else { "child" },"path":metadata}}});
            let batch = if case == "conflicting-buffered" {
                format!("{reply}\n{late}\n")
            } else {
                format!("{late}\n{reply}\n")
            };
            writer.write_all(batch.as_bytes()).await.unwrap();
            if case == "callback" {
                line.clear();
                reader.read_line(&mut line).await.unwrap();
                let denied: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(denied["id"], 900);
                assert!(denied.get("error").is_some());
            }
        };
        let ((), ()) = tokio::join!(session.recover_native_usage(&mut tree), server);
        assert_eq!(
            tree.usage().map(|u| (u.input, u.output, u.cached_input)),
            if case == "duplicate" {
                Some((20, 8, 4))
            } else {
                None
            },
            "{case}"
        );
    }
}

#[tokio::test]
async fn cold_native_usage_failure_never_falls_back_to_context_totals() {
    for case in [
        "missing",
        "unfinished",
        "foreign-root",
        "gap",
        "truncated",
        "invalid-stream",
    ] {
        let (dir, mut session, _peer) = fixture().await;
        let path = dir.path().join("root");
        let mut tree = completed_tree(false);
        if case != "missing" {
            let mut record = receipt("root", "turn", "turn");
            if case == "foreign-root" {
                record["payload"]["root_turn_id"] = "old-turn".into();
            }
            if case == "gap" {
                record["payload"]["turn_token_usage"]["total_tokens"] = 28.into();
            }
            append(&path, &record);
            if case != "unfinished" {
                append(&path, &finish("turn"));
            }
        }
        if case == "truncated" {
            std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"{")
                .unwrap();
        }
        if case == "invalid-stream" {
            tree.observe(
                "rawResponse/completed",
                &value!({"threadId":"root","turnId":"turn",
                "responseId":"root-turn","usage":{}}),
                false,
            )
            .unwrap();
        }
        session.recover_native_usage(&mut tree).await;
        assert!(tree.usage().is_none(), "{case}");
    }
}

#[tokio::test]
async fn cold_native_usage_metadata_timeout_keeps_usage_unknown() {
    let (_dir, mut session, _silent_peer) = fixture().await;
    let mut tree = completed_tree(true);
    tokio::time::timeout(
        Duration::from_secs(10),
        session.recover_native_usage(&mut tree),
    )
    .await
    .unwrap();
    assert!(tree.usage().is_none());
}
