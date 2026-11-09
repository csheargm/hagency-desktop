//! Real funded drivers, ledger, workspace and supervised Codex protocol.
//! Only Matrix network I/O is replaced; media encryption/decryption stays real.
use super::*;

#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn native_shared_chat_files_cross_standard_yolo_successor_and_budget_runs() {
    for (budget, yolo, plaintext) in [
        (false, false, false), (false, true, false), (true, false, false), (true, true, false),
        (false, false, true), (false, true, true), (true, false, true), (true, true, true),
    ] {
        eprintln!("file roundtrip: budget={budget} yolo={yolo} plaintext={plaintext}");
        let (f, t, c) = if budget {
            Box::pin(budget_attempts::prepared_with_tools(Some(yolo))).await
        } else {
            let (f, t, c) = Box::pin(fixture()).await;
            let db =
                rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
            db.execute(
                "INSERT INTO owner_shared_tool_policies(child_id,yolo,revision) VALUES(?1,?2,1)",
                rusqlite::params![c.agent_reservation_id, yolo],
            )
            .unwrap();
            drop(db);
            t.poll_fixture(&f.registry,launch(&f,"text-usage")).await.unwrap();
            tokio::time::timeout(Duration::from_secs(15),async {
                loop {
                    if f.state.lock().unwrap().shared.processes.values().any(|r|r.state=="stopped") { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }).await.unwrap();
            demands::activate(&f);
            demands::offer(&f, 1);
            (f, t, c)
        };
        let work = {
            let mut s = f.state.lock().unwrap();
            s.shared.contexts.active = plaintext;
            s.shared.contexts.v3 = plaintext;
            let w = s.shared.work.as_mut().unwrap();
            w.encrypted = !plaintext;
            w.body = if plaintext { "Process the uploaded file and send the result".into() } else { w.event_id.clone() };
            w.body_digest = format!("{:x}", Sha256::digest(w.body.as_bytes()));
            w.clone()
        };
        let (t, replies) =
            t.with_crypto_fixture(Some("Process the uploaded file and send the result".into()));
        let (t,_)=t.with_history_fixture(work.room_id.clone(),work.event_id.clone(),vec![vec![json!({"eventId":"$attachment:test","sender":work.requester_mxid,
            "body":"uploaded.pdf","messageType":"m.file","attachment":{"filename":"uploaded.pdf","status":"not_read"}})]]);
        let (t, endpoint) = t.with_file_fixture(
            f._root.path().canonicalize().unwrap().join("file-receipts"),
            work.room_id.clone(),
        );
        let mode = if yolo {
            "text-workspace-files-yolo"
        } else {
            "text-workspace-files-standard"
        };
        if budget {
            t.poll_budget_successor_fixture(&f.registry, launch(&f, mode).shared_fixture())
                .await
                .unwrap();
            budget_attempts::wait_stopped(&f).await;
        } else {
            t.poll_demand_fixture(&f.registry, None, Some(launch(&f, mode).shared_fixture()))
                .await
                .unwrap()
                .outcome
                .unwrap();
            attempts::wait_stopped(&f).await;
        }
        let root = f._root.path().join("shared-probe");
        let thread: Value =
            serde_json::from_slice(&std::fs::read(root.join("thread-request.json")).unwrap())
                .unwrap();
        assert_eq!(
            thread["sandbox"],
            if yolo {
                "danger-full-access"
            } else {
                "workspace-write"
            }
        );
        assert_eq!(thread["dynamicTools"].as_array().unwrap().len(), 5);
        let responses: Vec<Value> = std::fs::read_to_string(root.join("file-responses.jsonl"))
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            responses.len(),
            6,
            "budget={budget},yolo={yolo}: {responses:?}"
        );
        assert_eq!(
            responses[0]["error"],
            "attachment_not_observed_in_this_room"
        );
        assert_eq!(responses[3], responses[4]);
        assert_eq!(responses[3], responses[5]);
        assert_eq!(
            endpoint.receives.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        let uploads = endpoint.uploads.lock().unwrap();
        assert_eq!(uploads.len(), 1);
        let events = endpoint.events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let event = events.values().next().unwrap();
        let mut descriptor = event["file"].clone();
        descriptor.as_object_mut().unwrap().remove("url");
        let descriptor = hagency_media::Descriptor::from_private_event_json(
            &serde_json::to_vec(&descriptor).unwrap(),
        )
        .unwrap();
        let codec =
            hagency_media::Codec::new(hagency_media::Limits::new(8 * 1024 * 1024, 1).unwrap());
        assert_eq!(
            codec.decrypt(&descriptor, &uploads[0]).unwrap().bytes(),
            b"original file\0\xff\nprocessed by offline agent"
        );
        drop(events);
        drop(uploads);
        if plaintext {
            assert!(f.state.lock().unwrap().shared.receipts.values().any(|r|
                r.body.as_deref() == Some("File received, processed and delivered")));
        } else {
            assert_eq!(replies.lock().unwrap().len(), 1);
            assert_eq!(replies.lock().unwrap()[0].1, "File received, processed and delivered");
        }
        assert_eq!(f.state.lock().unwrap().shared.starts, 1);
        assert!(
            f.state
                .lock()
                .unwrap()
                .shared
                .receipts
                .values()
                .any(|r| r.state == "completed" && r.usage_complete)
        );
        assert_eq!(c.assignment_id, work.assignment_id);
        f.stop().await;
    }
}
