use super::*;

pub(super) async fn input(a: &Active, w: &SharedWorkItem, version: u16) -> SharedPreparedInput {
    let c = SharedThreadContext {
        version,
        input_profile: INPUT_PROFILE_V3.into(),
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
        coverage: COVERAGE_NATIVE_HISTORY.into(),
        omitted_earlier: true,
        history: vec![],
    };
    let record = SharedContextRecord {
        context_digest: digest(&c),
        input_digest: format!(
            "{:x}",
            Sha256::digest(render_context(&c, w).unwrap().as_bytes())
        ),
        context: c,
    };
    a.f.registry
        .prepare_shared_input(
            a.authority(),
            w.clone(),
            SharedWorkInputMode {
                mode: "thread_context_v1".into(),
                context_digest: None,
                input_digest: None,
            },
            Some(record),
        )
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires original offline Codex processes, SQLite registry and frozen context pins"]
async fn shared_native_session_plaintext_v3_restores_across_restart() {
    let mut a = Box::pin(active_mode(false, "text-native-session")).await;
    let first = turn_mode(
        &mut a,
        "remember_plain",
        "Remember native plaintext comet",
        false,
        false,
        false,
        3,
    )
    .await;
    assert_eq!(first.body.as_deref(), Some("Previous native inputs: "));
    a = advance_with_restart(a, true).await;
    let follow = turn_mode(
        &mut a,
        "recall_plain",
        "What did I say?",
        false,
        true,
        true,
        3,
    )
    .await;
    assert!(
        follow
            .body
            .as_ref()
            .unwrap()
            .contains("Remember native plaintext comet")
    );
    assert_ne!(first.call_id, follow.call_id);
    assert_ne!(first.process_digest, follow.process_digest);
    assert_eq!(follow.known_consumed_tokens, 11);
    a.stop().await;
}
