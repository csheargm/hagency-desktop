use super::*;
use hagency_agent_local::room_tools::{RoomReply, room_queue};
use hagency_runtime::{live_text::LiveText, room_tools::RoomTools};
use std::{path::Path, time::Duration};

async fn drained(path: &Path) {
    while !path.exists() {
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

#[tokio::test]
async fn claude_long_stream_settles_reported_usage_and_keeps_missing_usage_unknown() {
    for (mode, spent) in [
        ("long_stream", 24),
        ("long_usage", 13_200),
        ("long_stream_unknown", 0),
    ] {
        let mut f = Fixture::new(Kind::Claude, mode, ToolPolicy::Deny);
        let mut process = f.process().await;
        process
            .session
            .open_context(&mut f.ledger, &f.scope, &f.profile)
            .await
            .unwrap();
        let (queue, _) = codex::approval_queue(1).unwrap();
        let live = LiveText::default();
        let queue = queue.with_live_text(live.clone());
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            process.session.run_with_started(
                &mut f.ledger,
                "@owner:test",
                &f.scope,
                "call",
                "dispatch",
                "finish the task",
                BudgetMode::Postpaid,
                &queue,
                None,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.text, "offline reply");
        assert_eq!(result.status, codex::TurnStatus::Completed);
        assert_eq!(result.usage.is_reported(), mode != "long_stream_unknown");
        assert_eq!(f.spent(), (spent, 0));
        let outstanding = f.ledger.outstanding_calls("@owner:test").unwrap();
        if mode == "long_stream_unknown" {
            assert_eq!(outstanding.len(), 1);
            assert_eq!(outstanding[0].state, "unknown");
        } else {
            assert!(outstanding.is_empty());
        }
        if mode != "long_usage" {
            let (_, text) = live.since(0).unwrap();
            assert!(text.len() <= 4096);
            assert!(text.ends_with("streamed text "));
        }
        process.stop().await.unwrap();
    }
}

#[tokio::test]
async fn claude_long_stream_during_owner_wait_keeps_original_approval_and_tool_counts() {
    let mut f = Fixture::new(Kind::Claude, "long_wait", ToolPolicy::AskOwner);
    let mut process = f.process().await;
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let marker = f.path("stream-drained");
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "wait for owner",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        let pending = approvals.recv().await.unwrap();
        drained(&marker).await;
        assert!(!pending.is_closed());
        pending.decide(true);
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(run, host) })
            .await
            .unwrap();
    let result = result.unwrap();
    assert_eq!(result.status, codex::TurnStatus::Completed);
    assert_eq!((result.tool_requests, result.tool_returns), (1, 1));
    assert!(f.path("effect").exists());
    assert_eq!(f.spent(), (24, 0));
    process.stop().await.unwrap();
}

#[tokio::test]
async fn claude_long_stream_during_room_wait_preserves_each_original_reply() {
    let mut f = Fixture::new(Kind::Claude, "room_stream_wait", ToolPolicy::Deny);
    let mut process = f.process().await;
    process
        .session
        .enable_room_tools(RoomTools::history_and_files())
        .await
        .unwrap();
    process
        .session
        .open_context(&mut f.ledger, &f.scope, &f.profile)
        .await
        .unwrap();
    let (queue, mut approvals) = codex::approval_queue(1).unwrap();
    let (room, mut requests) = room_queue(&f.scope, "dispatch").unwrap();
    let queue = queue.with_room_tools(room);
    let markers = [f.path("room-drained-0"), f.path("room-drained-1")];
    let run = process.session.run_with_started(
        &mut f.ledger,
        "@owner:test",
        &f.scope,
        "call",
        "dispatch",
        "read and send",
        BudgetMode::Postpaid,
        &queue,
        None,
    );
    let host = async {
        for (marker, text) in markers
            .iter()
            .zip(["ROOM_HISTORY_RECEIPT", "ROOM_FILE_RECEIPT"])
        {
            let pending = requests.recv().await.unwrap();
            drained(marker).await;
            assert!(!pending.is_closed());
            pending.reply(RoomReply::new(text.into(), false).unwrap());
        }
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(run, host) })
            .await
            .unwrap();
    assert_eq!(result.unwrap().text, "offline reply");
    assert_eq!(f.spent(), (24, 0));
    assert!(approvals.try_recv().is_err());
    process.stop().await.unwrap();
}
