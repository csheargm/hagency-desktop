//! Loopback HTTP and a real SQLite ledger; no provider calls or invented stops.
use super::*;
use crate::console::native::{AgentSettlementProgress, AgentSettlementTransport};
#[derive(Default)]
pub(in crate::console::native::allocation_transport::tests) struct SettlementState {
    pub(in crate::console::native::allocation_transport::tests) missing_capability: bool,
    command: Option<ManagedAgentSettlementCommand>,
    recorded: Option<ManagedAgentSettlementReceipt>,
    writes: usize,
    requests: usize,
    lose_before_result: bool,
}
pub(in crate::console::native::allocation_transport::tests) fn management(
    path: &str,
    body: &Value,
    s: &mut State,
) -> Option<Value> {
    s.settlement.requests += 1;
    let at = now();
    let Some(c) = s.settlement.command.clone() else {
        return Some(json!({"delivery":null,"observedAtMs":at}));
    };
    let hash = digest(&c).unwrap();
    if path.ends_with("/result") {
        assert_eq!(body["commandDigest"], hash);
        let receipt: ManagedAgentSettlementReceipt =
            serde_json::from_value(body["receipt"].clone()).unwrap();
        receipt.validate_for(&c).unwrap();
        if std::mem::take(&mut s.settlement.lose_before_result) {
            return None;
        }
        if let Some(old) = &s.settlement.recorded {
            assert_eq!(digest(old).unwrap(), digest(&receipt).unwrap());
        } else {
            s.settlement.writes += 1;
            s.settlement.recorded = Some(receipt.clone());
        }
        if s.invalidate_at_result {
            s.source.as_ref().unwrap().store(false, Ordering::Release);
        }
        if std::mem::take(&mut s.lose_result) {
            return None;
        }
        let mut ack = serde_json::to_value(receipt).unwrap();
        if s.wrong_ack {
            ack["ledgerRevision"] = json!(9999);
        }
        return Some(json!({"receipt":ack,"observedAtMs":at}));
    }
    let historical = path.ends_with("/recover");
    if historical {
        assert_eq!(body["commandDigest"], hash);
    }
    Some(json!({"delivery":{
        "command":c,"commandDigest":if s.tamper_digest {"f".repeat(64)}else{hash},
        "token":"e".repeat(64),"leaseUntilMs":if s.expired_ticket{at-1}else{at+20000},
        "executionAllowed":if historical{s.historical_executable}else{s.settlement.recorded.is_none()},
        "recorded":s.settlement.recorded
    },"observedAtMs":at}))
}
async fn prepared_settlement(
    reserved: bool,
) -> (
    Fixture,
    AgentSettlementTransport,
    ManagedAgentSettlementCommand,
) {
    let (f, t, reserve) = prepared().await;
    if reserved {
        t.poll(&f.registry, None).await.unwrap();
    }
    let fc = fence(&reserve);
    f.state.lock().unwrap().agent = Some(serde_json::to_value(&fc).unwrap());
    t.poll(&f.registry, None).await.unwrap();
    let receipt = f.state.lock().unwrap().agent_receipts[&fc.command_id].clone();
    let c = ManagedAgentSettlementCommand {
        version: 1,
        capability: AGENT_SETTLEMENT_CAPABILITY.into(),
        operation: "agent.settle".into(),
        command_id: "settlement_fixture".into(),
        fence_command: fc,
        fence_receipt_digest: digest(&receipt).unwrap(),
        issued_at_ms: now(),
    };
    f.state.lock().unwrap().settlement.command = Some(c.clone());
    let transport = AgentSettlementTransport::new(f.owner.management(package(1)).await.unwrap());
    (f, transport, c)
}
fn counts(f: &Fixture) -> (u64, u64, u64) {
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    db.query_row("SELECT (SELECT count(*) FROM owner_capacity_receipts WHERE command_id='settlement_fixture'),(SELECT count(*) FROM owner_managed_calls),(SELECT count(*) FROM owner_managed_budget_deltas)", [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
}
#[tokio::test]
async fn native_settlement_lost_upload_restart_recovers_exact_receipt_without_new_work() {
    let (mut f, t, c) = prepared_settlement(true).await;
    let original = f.state.lock().unwrap().agent_receipts.clone();
    f.state.lock().unwrap().settlement.lose_before_result = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    assert_eq!(counts(&f), (1, 0, 0));
    assert_eq!(f.state.lock().unwrap().settlement.writes, 0);
    drop(t);
    f = f.reopen().await;
    let t = AgentSettlementTransport::new(f.owner.management(package(1)).await.unwrap());
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        AgentSettlementProgress::Recorded {
            usage_complete: true,
            ..
        }
    ));
    assert_eq!(counts(&f), (1, 0, 0));
    let receipt = f.state.lock().unwrap().settlement.recorded.clone().unwrap();
    assert_eq!(receipt.known_consumed_tokens, 0);
    assert_eq!(receipt.retained_tokens, 0);
    assert_eq!(receipt.process_state, "not_started");
    t.poll(&f.registry, None).await.unwrap();
    assert_eq!(counts(&f), (1, 0, 0));
    assert_eq!(f.state.lock().unwrap().settlement.writes, 1);
    assert_eq!(f.state.lock().unwrap().agent_receipts, original);
    f.stop().await;
}
#[tokio::test]
async fn native_settlement_historical_recovery_does_not_create_proof_or_absent_agent() {
    let (f, t, c) = prepared_settlement(false).await;
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        AgentSettlementProgress::HistoricalReceiptMissing { .. }
    ));
    assert_eq!(counts(&f), (0, 0, 0));
    f.state.lock().unwrap().historical_executable = true;
    assert!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .is_err()
    );
    f.state.lock().unwrap().historical_executable = false;
    t.poll(&f.registry, None).await.unwrap();
    let r = f.state.lock().unwrap().settlement.recorded.clone().unwrap();
    assert!(r.agent_reservation_id.is_none());
    assert!(r.budget_revision.is_none());
    assert_eq!(counts(&f), (1, 0, 0));
    f.stop().await;
}
#[tokio::test]
async fn native_settlement_capability_digest_device_and_permission_fail_before_local_commit() {
    let (f, t, c) = prepared_settlement(true).await;
    f.state.lock().unwrap().settlement.missing_capability = true;
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "agent_settlement_protocol_unavailable"
    );
    assert_eq!(f.state.lock().unwrap().settlement.requests, 0);
    f.state.lock().unwrap().settlement.missing_capability = false;
    f.state.lock().unwrap().expired_ticket = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    f.state.lock().unwrap().expired_ticket = false;
    f.state.lock().unwrap().tamper_digest = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    f.state.lock().unwrap().tamper_digest = false;
    let mut wrong = c.clone();
    wrong.fence_command.reserve_command.executor_device_id = "other".into();
    wrong.fence_command.reserve_command_digest =
        digest(&wrong.fence_command.reserve_command).unwrap();
    f.state.lock().unwrap().settlement.command = Some(wrong);
    assert!(t.poll(&f.registry, None).await.is_err());
    f.state.lock().unwrap().settlement.command = Some(c);
    let mut denied = package(2);
    denied.permissions = vec![Permission::ReadIntents];
    let denied = AgentSettlementTransport::new(f.owner.management(denied).await.unwrap());
    assert!(denied.poll(&f.registry, None).await.is_err());
    assert_eq!(counts(&f), (0, 0, 0));
    f.stop().await;
}
#[tokio::test]
async fn native_settlement_ack_loss_scope_loss_and_changed_ack_preserve_first_proof() {
    for mode in ["lost", "scope", "changed"] {
        let (f, t, c) = prepared_settlement(true).await;
        {
            let mut s = f.state.lock().unwrap();
            s.lose_result = mode == "lost";
            s.invalidate_at_result = mode == "scope";
            s.wrong_ack = mode == "changed";
        }
        assert!(t.poll(&f.registry, None).await.is_err());
        assert_eq!(counts(&f), (1, 0, 0));
        f.current.store(true, Ordering::Release);
        {
            let mut s = f.state.lock().unwrap();
            s.invalidate_at_result = false;
            s.wrong_ack = false;
        }
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap();
        assert_eq!(counts(&f), (1, 0, 0));
        assert_eq!(f.state.lock().unwrap().settlement.writes, 1);
        f.stop().await;
    }
}
#[cfg(feature = "resource-adoption")]
#[tokio::test]
async fn native_settlement_host_retirement_and_new_sdk_generation_keep_cleanup_bound_to_original() {
    let (f, t, _c) = prepared_settlement(true).await;
    let closed = AgentSettlementTransport::new(f.owner.management(package(2)).await.unwrap())
        .restrict_to_host(Arc::new(|| false));
    assert!(closed.poll(&f.registry, None).await.is_err());
    assert_eq!(counts(&f), (0, 0, 0));
    explicit_new_generation(&f, 2).await;
    t.poll(&f.registry, None).await.unwrap();
    assert_eq!(counts(&f), (1, 0, 0));
    f.stop().await;
}
