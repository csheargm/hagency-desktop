use super::*;
use hagency_runtime::claude::session::{ApprovalControlPolicy, PermissionDecision};

fn correlated(mut value: Value, prompt: &Value) -> Value {
    value["user_message_uuid"] = prompt["uuid"].clone();
    value["user_message_uuids"] = json!([prompt["uuid"]]);
    value
}
pub(super) async fn next_prompt(driver: &mut Driver, peer: &mut Peer) -> Value {
    let (sent, prompt) = tokio::join!(driver.prompt("next turn"), read(peer));
    sent.unwrap();
    event(driver, peer, correlated(init(), &prompt))
        .await
        .unwrap();
    prompt
}
pub(super) async fn completed(driver: &mut Driver, peer: &mut Peer, prompt: &Value) {
    event(driver, peer, correlated(result(), prompt))
        .await
        .unwrap();
    assert!(!driver.can_followup());
    event(driver, peer, state("idle")).await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn native_claude_continuous_turns_keep_session_and_rotate_authority() {
    let (mut driver, mut peer) = pair(8192, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    assert!(driver.begin_followup(limits()).is_err());
    let first = next_prompt(&mut driver, &mut peer).await;
    let first_source = driver.observation_source().unwrap();
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 1000,
            response_reserve_ms: 1000,
        })
        .unwrap();
    event(&mut driver, &mut peer, permission("same-callback"))
        .await
        .unwrap();
    let mut old_grant = driver
        .prepare_approval("same-callback", PermissionDecision::Allow)
        .unwrap();
    completed(&mut driver, &mut peer, &first).await;
    assert!(
        !driver.can_followup(),
        "an outstanding callback prevents turn reuse"
    );
    // A cancellation observed before completion retires the pending callback.
    // Use another stream for the successful follow-up case below.
    driver.close();
    assert!(first_source.is_retired());
    assert!(driver.send_prepared_approval(&mut old_grant).await.is_err());

    let (mut driver, mut peer) = pair(8192, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    let first = next_prompt(&mut driver, &mut peer).await;
    let first_source = driver.observation_source().unwrap();
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 1000,
            response_reserve_ms: 1000,
        })
        .unwrap();
    event(&mut driver, &mut peer, permission("same-callback"))
        .await
        .unwrap();
    let mut old_grant = driver
        .prepare_approval("same-callback", PermissionDecision::Allow)
        .unwrap();
    event(
        &mut driver,
        &mut peer,
        json!({"type":"control_cancel_request","request_id":"same-callback"}),
    )
    .await
    .unwrap();
    completed(&mut driver, &mut peer, &first).await;
    assert!(driver.can_followup());
    tokio::time::advance(Duration::from_secs(31)).await;
    driver.begin_followup(limits()).unwrap();
    assert!(first_source.is_retired());
    let second = next_prompt(&mut driver, &mut peer).await;
    assert_ne!(first["uuid"], second["uuid"]);
    assert_eq!(second["session_id"], "session-one");
    assert!(driver.observation_source().unwrap() != first_source);
    driver
        .enable_approval_control(ApprovalControlPolicy {
            owner_wait_ms: 1000,
            response_reserve_ms: 1000,
        })
        .unwrap();
    event(&mut driver, &mut peer, permission("same-callback"))
        .await
        .unwrap();
    assert!(matches!(
        driver.send_prepared_approval(&mut old_grant).await,
        Err(Error::Identity)
    ));
    let mut unexpected = String::new();
    assert_eq!(
        peer.input.read_line(&mut unexpected).await.unwrap(),
        0,
        "an old approval must send no bytes"
    );
}

#[tokio::test]
async fn native_claude_continuous_turn_rejects_old_or_mixed_result_ids() {
    for mixed in [false, true] {
        let (mut driver, mut peer) = pair(8192, limits());
        driver.enable_native_completion().unwrap();
        driver.enable_continuous_session().unwrap();
        initialize(&mut driver, &mut peer).await;
        let first = next_prompt(&mut driver, &mut peer).await;
        completed(&mut driver, &mut peer, &first).await;
        driver.begin_followup(limits()).unwrap();
        let second = next_prompt(&mut driver, &mut peer).await;
        let mut replay = correlated(result(), &first);
        if mixed {
            replay["user_message_uuid"] = second["uuid"].clone();
        }
        assert!(matches!(
            event(&mut driver, &mut peer, replay).await,
            Err(Error::Identity)
        ));
        assert_eq!(driver.phase(), Phase::Closed);
    }
}

#[tokio::test(start_paused = true)]
async fn native_claude_followup_probe_drains_idle_metadata_after_prior_deadline() {
    let (mut driver, mut peer) = pair(8192, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    let first = next_prompt(&mut driver, &mut peer).await;
    let source = driver.observation_source().unwrap();
    completed(&mut driver, &mut peer, &first).await;
    // Native metadata may already be buffered when the prior call settles.
    peer.output
        .write_all(&encoded(state("idle")))
        .await
        .unwrap();
    tokio::time::advance(Duration::from_secs(31)).await;
    let native = async {
        let probe = read(&mut peer).await;
        assert_eq!(probe["request"]["subtype"], "mcp_status");
        peer.output.write_all(&encoded(json!({"type":"control_response","response":{"subtype":"success","request_id":probe["request_id"],"response":{"mcpServers":[]}}}))).await.unwrap();
    };
    let (prepared, ()) = tokio::join!(driver.prepare_followup(limits()), native);
    prepared.unwrap();
    assert!(source.is_retired());
    let second = next_prompt(&mut driver, &mut peer).await;
    completed(&mut driver, &mut peer, &second).await;
    assert!(driver.can_followup());
}

/// A hook of the account's own settings can end after the turn did. Its
/// report is then waiting when the next message is prepared.
#[tokio::test(start_paused = true)]
async fn native_claude_followup_probe_passes_a_late_hook_report_of_the_same_session() {
    for other_session in [false, true] {
        let (mut driver, mut peer) = pair(8192, limits());
        driver.enable_native_completion().unwrap();
        driver.enable_continuous_session().unwrap();
        initialize(&mut driver, &mut peer).await;
        let first = next_prompt(&mut driver, &mut peer).await;
        let session = driver.session_id().unwrap().to_owned();
        completed(&mut driver, &mut peer, &first).await;
        let mut report: Vec<Value> =
            serde_json::from_str(include_str!("../fixtures/claude-native-hook-reports.json"))
                .unwrap();
        let mut report = report.remove(1);
        report["hook_event"] = json!("Stop");
        report["session_id"] = json!(if other_session {
            "different-session".to_owned()
        } else {
            session
        });
        peer.output.write_all(&encoded(report)).await.unwrap();
        let native = async {
            let probe = read(&mut peer).await;
            assert_eq!(probe["request"]["subtype"], "mcp_status");
            peer.output.write_all(&encoded(json!({"type":"control_response","response":{"subtype":"success","request_id":probe["request_id"],"response":{"mcpServers":[]}}}))).await.unwrap();
        };
        let (prepared, ()) = tokio::join!(driver.prepare_followup(limits()), native);
        if other_session {
            assert_eq!(prepared, Err(Error::Identity));
            assert_eq!(driver.phase(), Phase::Closed);
        } else {
            prepared.unwrap();
            let second = next_prompt(&mut driver, &mut peer).await;
            completed(&mut driver, &mut peer, &second).await;
            assert!(driver.can_followup());
        }
    }
}

#[tokio::test(start_paused = true)]
async fn native_claude_supervision_is_per_turn_and_never_unbounds_metadata() {
    let (mut driver, mut peer) = pair(8192, limits());
    driver.enable_native_completion().unwrap();
    driver.enable_continuous_session().unwrap();
    initialize(&mut driver, &mut peer).await;
    let first = next_prompt(&mut driver, &mut peer).await;
    driver.supervise_turn().unwrap();
    tokio::time::advance(Duration::from_secs(1801)).await;
    completed(&mut driver, &mut peer, &first).await;
    assert!(driver.can_followup());
    driver.begin_followup(limits()).unwrap();
    next_prompt(&mut driver, &mut peer).await;
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_secs(2), driver.next_message()).await,
            Ok(Err(Error::Timeout))
        ),
        "a new turn needs its own host supervision"
    );

    let (mut driver, mut peer) = pair(8192, limits());
    driver.enable_native_completion().unwrap();
    initialize(&mut driver, &mut peer).await;
    prompt(&mut driver, &mut peer).await;
    event(&mut driver, &mut peer, init()).await.unwrap();
    driver.supervise_turn().unwrap();
    tokio::time::advance(Duration::from_secs(1801)).await;
    event(&mut driver, &mut peer, background(true))
        .await
        .unwrap();
    event(&mut driver, &mut peer, result()).await.unwrap();
    event(&mut driver, &mut peer, background(false))
        .await
        .unwrap();
    event(&mut driver, &mut peer, state("idle")).await.unwrap();
    assert!(driver.needs_usage_snapshot());
    let (sent, snapshot) = tokio::join!(driver.request_usage_snapshot(), read(&mut peer));
    sent.unwrap();
    assert_eq!(snapshot["message"]["content"], "/usage");
    assert!(
        matches!(
            tokio::time::timeout(Duration::from_secs(2), driver.next_message()).await,
            Ok(Err(Error::Timeout))
        ),
        "usage metadata still has a response deadline"
    );
}
