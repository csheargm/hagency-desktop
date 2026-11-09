//! Actual-process context custody, using the original child ledger and holds.
use super::*;
fn record(w: &SharedWorkItem) -> SharedContextRecord {
    let c = SharedThreadContext {
        version: 3,
        input_profile: "shared_native_text_v3".into(),
        work_id: w.id.clone(),
        work_digest: digest(w),
        assignment_id: w.assignment_id.clone(),
        runtime_command_digest: w.runtime_command_digest.clone(),
        demand_binding_digest: None,
        control_revision: None,
        room_id: w.room_id.clone(),
        thread_root: w.thread_root.clone(),
        event_id: w.event_id.clone(),
        received_at_ms: w.created_at_ms,
        context_ordinal: 1,
        coverage: "native_session_and_paged_room_history".into(),
        omitted_earlier: true,
        history: vec![],
    };
    SharedContextRecord {
        context_digest: digest(&c),
        input_digest: format!(
            "{:x}",
            Sha256::digest(render_context(&c, w).unwrap().as_bytes())
        ),
        context: c,
    }
}
async fn input(a: &Active, w: &SharedWorkItem) -> SharedPreparedInput {
    a.f.registry
        .receive_shared_work(a.authority(), w.clone())
        .await
        .unwrap();
    a.f.registry
        .prepare_shared_input(
            a.authority(),
            w.clone(),
            SharedWorkInputMode {
                mode: "thread_context_v1".into(),
                context_digest: None,
                input_digest: None,
            },
            Some(record(w)),
        )
        .await
        .unwrap()
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_context_call_plain_fallback_refused_and_exact_actual_input_is_captured() {
    let mut a = Box::pin(active()).await;
    let w = a.work("context_work");
    a.f.registry.receive_shared_work(a.authority(), w.clone()).await.unwrap();
    assert!(matches!(
        a.f.registry.prepare_shared_call(a.authority(), w.clone()).await,
        Err(Error::LocalAuthority)
    ), "fresh unmarked plaintext cannot bypass V3 preparation");
    let input = input(&a, &w).await;
    assert!(
        a.f.registry
            .prepare_shared_call(a.authority(), w.clone())
            .await
            .is_err()
    );
    let h =
        a.f.registry
            .prepare_shared_input_call(a.authority(), w.clone(), input.clone())
            .await
            .unwrap();
    assert_eq!(
        h.reserved_tokens, 0,
        "the admitted task holds no estimate, including when history is supplied"
    );
    assert!(
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id.clone())
            .await
            .is_err()
    );
    let start =
        a.f.registry
            .begin_shared_input_start(a.authority(), w.clone(), h.call_id.clone(), input.clone())
            .await
            .unwrap();
    let c = input.context().unwrap();
    let issued =
        a.f.registry
            .issue_shared_context_call(
                start,
                SharedContextStart {
                    start: SharedWorkStart {
                        work: w.clone(),
                        work_digest: digest(&w),
                        activity_epoch: 1,
                        newly_started: true,
                    },
                    context_digest: c.context_digest.clone(),
                    input_digest: c.input_digest.clone(),
                },
            )
            .await
            .unwrap();
    a.process.start_shared_text_thread().await.unwrap();
    assert!(
        a.process
            .start_shared_context_turn(issued.model_input().into(), &"f".repeat(64))
            .await
            .is_err(),
        "bad digest refuses before consuming actual turn"
    );
    issued.start_turn(&mut a.process).await.unwrap();
    let mut sink = issued.bind_turn(&a.process).unwrap();
    loop {
        let e = a.process.next_shared_text_event().await.unwrap();
        if let Some(r) = sink.observe(e).await.unwrap()
            && r.state == "completed"
        {
            assert_eq!(r.body.as_deref(), Some("Offline answer."));
            assert!(!r.usage_complete);
            break;
        }
    }
    assert!(
        a.f.registry
            .begin_shared_input_start(a.authority(), w.clone(), h.call_id, input)
            .await
            .is_err()
    );
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_context_call_body_only_actual_turn_cannot_claim_context_evidence() {
    let mut a = Box::pin(active()).await;
    let w = a.work("context_wrong_input");
    let input = input(&a, &w).await;
    let h =
        a.f.registry
            .prepare_shared_input_call(a.authority(), w.clone(), input.clone())
            .await
            .unwrap();
    let start =
        a.f.registry
            .begin_shared_input_start(a.authority(), w.clone(), h.call_id, input.clone())
            .await
            .unwrap();
    let c = input.context().unwrap();
    let issued =
        a.f.registry
            .issue_shared_context_call(
                start,
                SharedContextStart {
                    start: SharedWorkStart {
                        work: w.clone(),
                        work_digest: digest(&w),
                        activity_epoch: 1,
                        newly_started: true,
                    },
                    context_digest: c.context_digest.clone(),
                    input_digest: c.input_digest.clone(),
                },
            )
            .await
            .unwrap();
    a.process.start_shared_text_thread().await.unwrap();
    a.process.start_shared_text_turn(w.body).await.unwrap();
    assert!(issued.bind_turn(&a.process).is_err());
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_context_call_changed_snapshot_and_retired_scope_never_change_saved_input() {
    let a = Box::pin(active()).await;
    let w = a.work("context_saved");
    let input = input(&a, &w).await;
    assert!(
        a.f.registry
            .prepare_shared_input(a.authority(), w.clone(), SharedWorkInputMode { mode:"plain_text_v1".into(),context_digest:None,input_digest:None }, None)
            .await
            .is_err(),
        "missing capability cannot downgrade known contextual work"
    );
    let other = a.work("new_plain_after_context");
    a.f.registry
        .receive_shared_work(a.authority(), other.clone())
        .await
        .unwrap();
    assert!(
        a.f.registry
            .prepare_shared_input(a.authority(), other, SharedWorkInputMode { mode:"plain_text_v1".into(),context_digest:None,input_digest:None }, None)
            .await
            .is_err(),
        "missing capability cannot downgrade another work on the same child"
    );
    let mut changed = record(&w);
    changed.context.context_ordinal += 1;
    changed.context_digest = digest(&changed.context);
    changed.input_digest = format!(
        "{:x}",
        Sha256::digest(render_context(&changed.context, &w).unwrap().as_bytes())
    );
    assert!(
        a.f.registry
            .prepare_shared_input(
                a.authority(),
                w.clone(),
                SharedWorkInputMode {
                    mode: "thread_context_v1".into(),
                    context_digest: None,
                    input_digest: None
                },
                Some(changed)
            )
            .await
            .is_err()
    );
    let proof = a.authority();
    a.f.profile.retire();
    assert!(
        a.f.registry
            .prepare_shared_input_call(proof, w, input)
            .await
            .is_err()
    );
    a.stop().await;
}
