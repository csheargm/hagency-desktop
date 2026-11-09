//! Real HTTP + real owner SQLite; auth/provider inputs below are fixture-only.
use super::*;
#[path = "../managed_agent_settlement_transport_tests.rs"]
pub(super) mod settlements;
#[cfg(all(feature="agent-budget",feature="shared-execution"))]
#[path="../managed_agent_budget_transport_tests.rs"]
pub(super) mod budgets;
use crate::console::native::{ManagedAgentProgress, ManagedAgentTransport};
#[path = "../managed_agent_matrix_transport_tests.rs"]
mod matrix;
#[path = "../managed_runtime_transport_tests.rs"]
mod runtime;
#[cfg(feature = "shared-execution")]
#[path = "../shared_execution/tests.rs"]
pub(super) mod shared;
pub(super) fn management(path: &str, body: &Value, s: &mut State) -> Option<Value> {
    if path.contains("/agent-runtime/") {
        return runtime::management(path, body, s);
    }
    if path.contains("/agent-matrix/") {
        return matrix::management(path, body, s);
    }
    let at = now();
    let c = s.agent.clone().unwrap();
    let id = c["commandId"].as_str().unwrap();
    let hash = digest(&c).unwrap();
    if path.ends_with("/result") {
        assert_eq!(body["commandDigest"], hash);
        assert_eq!(body["receipt"]["commandId"], id);
        if let Some(old) = s.agent_receipts.get(id) {
            assert_eq!(old, &body["receipt"]);
        } else {
            s.agent_writes += 1;
            s.agent_receipts.insert(id.into(), body["receipt"].clone());
        }
        if s.invalidate_at_result {
            s.source.as_ref().unwrap().store(false, Ordering::Release);
        }
        if std::mem::take(&mut s.lose_result) {
            return None;
        }
        return Some(
            json!({"commandId":id,"commandDigest":hash,"receiptDigest":if s.wrong_ack{"f".repeat(64)}else{digest(&body["receipt"]).unwrap()},"assignment":s.agent_assignment,"observedAtMs":at}),
        );
    }
    let historical = path.ends_with("/recover");
    if historical {
        assert_eq!(body["commandDigest"], hash);
    }
    let recorded = s.agent_receipts.get(id).cloned();
    Some(
        json!({"delivery":{"command":c,"commandDigest":if s.tamper_digest{"f".repeat(64)}else{hash},"token":"e".repeat(64),"leaseUntilMs":if s.expired_ticket{at-1}else if s.short_ticket{at+1000}else{at+20000},"executionAllowed":if historical{s.historical_executable}else{recorded.is_none()},"recorded":recorded},"observedAtMs":at}),
    )
}
async fn prepared() -> (Fixture, ManagedAgentTransport, ManagedAgentReserveCommand) {
    prepared_with(Fixture::new().await).await
}
async fn prepared_with(f: Fixture) -> (Fixture, ManagedAgentTransport, ManagedAgentReserveCommand) {
    prepared_with_budget(f, 300, 100).await
}
async fn prepared_with_budget(
    f: Fixture,
    project_tokens: u64,
    child_tokens: u64,
) -> (Fixture, ManagedAgentTransport, ManagedAgentReserveCommand) {
    prepared_with_budget_lifetime(f, project_tokens, child_tokens, 60000).await
}
async fn prepared_with_budget_lifetime(
    f: Fixture, project_tokens: u64, child_tokens: u64, lifetime_ms: u64,
) -> (Fixture, ManagedAgentTransport, ManagedAgentReserveCommand) {
    let mut p = f.allocation();
    p.requested_tokens = project_tokens;
    p.expires_at_ms = (now() + lifetime_ms).min(p.grant_receipt.delegation_expires_at_ms);
    f.state.lock().unwrap().allocation = Some(p.clone());
    f.transport
        .poll_allocation(&f.registry, None)
        .await
        .unwrap();
    let r = f.state.lock().unwrap().allocation_receipt.clone().unwrap();
    let g = &p.grant_receipt;
    let resource = f.registry.resource(&f.transport.scope(Permission::PublishGrants).await.unwrap().profile, g.resource_id.clone()).await.unwrap();
    let model = hagency_core::qualification::ModelProfile {
        framework: resource.framework,
        model: resource.model,
        provider: None,
        reasoning: Some("medium".into()),
    };
    let role = hagency_core::qualification::roles()
        .find(|r| hagency_core::qualification::qualifies(&model, r, None))
        .unwrap();
    let definition = ManagedAgentDefinition {
        name: "Fixture helper".into(),
        capability: role.into(),
        description: "No runtime effects".into(),
        tool_mode: "none".into(),
    };
    let c = ManagedAgentReserveCommand {
            renewal_mode: None,
            allocation_generation: None,
        version: 1,
        pool_grant: None,
        capability: AGENT_CAPABILITY.into(),
        operation: "agent.reserve".into(),
        command_id: "agent_reserve_fixture".into(),
        request_id: "agent_request_fixture".into(),
        request_revision: 2,
        proposed_agent_id: "agent_fixture".into(),
        manager_mxid: p.manager_mxid.clone(),
        project_id: p.project_id.clone(),
        project_revision: p.project_revision,
        setup_id: "setup_fixture".into(),
        setup_binding_revision: 1,
        domain_project_id: "domain_project".into(),
        room_id: "!discussion:test".into(),
        authority_revision: 1,
        decision_actor_mxid: "@assigned_admin:test".into(),
        decision_digest: "d".repeat(64),
        allocation_id: p.allocation_id.clone(),
        allocation_receipt_digest: digest(&r).unwrap(),
        grant_receipt_digest: digest(g).unwrap(),
        enrollment_id: g.enrollment_id.clone(),
        executor_device_id: g.executor_device_id.clone(),
        executor_generation: g.executor_generation,
        resource_id: g.resource_id.clone(),
        resource_revision: g.resource_revision.clone(),
        definition_digest: digest(&definition).unwrap(),
        definition,
        requested_tokens: child_tokens,
        period_key: g.period_key.clone(),
        issued_at_ms: now() - 1,
        expires_at_ms: p.expires_at_ms,
    };
    f.state.lock().unwrap().agent = Some(serde_json::to_value(&c).unwrap());
    let transport = ManagedAgentTransport::new(f.owner.management(package(1)).await.unwrap());
    (f, transport, c)
}
fn fence(c: &ManagedAgentReserveCommand) -> ManagedAgentFenceCommand {
    ManagedAgentFenceCommand {
        version: 1,
        capability: AGENT_FENCE_CAPABILITY.into(),
        operation: "agent.fence".into(),
        command_id: "fence_agent_fixture".into(),
        reserve_command: c.clone(),
        reserve_command_digest: digest(c).unwrap(),
        issued_at_ms: now(),
    }
}
#[tokio::test]
async fn native_managed_agent_transport_loss_restart_recovers_original_receipt_without_reserving_again()
 {
    let (mut f, t, c) = prepared().await;
    f.state.lock().unwrap().lose_result = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    assert_eq!(f.state.lock().unwrap().agent_writes, 1);
    let original = f.state.lock().unwrap().agent_receipts[&c.command_id].clone();
    assert_eq!(original["state"], "reserved");
    f.registry.shutdown().await.unwrap();
    f.registry =
        OwnerResourceRegistry::isolated_for_tests(&f._root.path().join("registry")).unwrap();
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        ManagedAgentProgress::Recorded { .. }
    ));
    assert_eq!(f.state.lock().unwrap().agent_writes, 1);
    assert_eq!(
        f.state.lock().unwrap().agent_receipts[&c.command_id],
        original
    );
    f.stop().await;
}
#[tokio::test]
async fn native_managed_agent_transport_recovery_cannot_create_missing_hold_or_fence() {
    let (f, t, c) = prepared().await;
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        ManagedAgentProgress::HistoricalReceiptMissing { .. }
    ));
    assert_eq!(f.state.lock().unwrap().agent_writes, 0);
    f.state.lock().unwrap().historical_executable = true;
    assert!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .is_err()
    );
    let fence = fence(&c);
    {
        let mut s = f.state.lock().unwrap();
        s.historical_executable = false;
        s.agent = Some(serde_json::to_value(&fence).unwrap());
    }
    assert!(matches!(
        t.recover(
            &f.registry,
            fence.command_id.clone(),
            digest(&fence).unwrap()
        )
        .await
        .unwrap(),
        ManagedAgentProgress::HistoricalReceiptMissing { .. }
    ));
    assert_eq!(f.state.lock().unwrap().agent_writes, 0);
    f.stop().await;
}
#[tokio::test]
async fn native_managed_agent_transport_cleanup_before_reserve_keeps_semantic_tombstone() {
    let (f, t, c) = prepared().await;
    let fence = fence(&c);
    f.state.lock().unwrap().agent = Some(serde_json::to_value(&fence).unwrap());
    t.poll(&f.registry, None).await.unwrap();
    assert_eq!(
        f.state.lock().unwrap().agent_receipts[&fence.command_id]["state"],
        "absent_fenced"
    );
    f.state.lock().unwrap().agent = Some(serde_json::to_value(&c).unwrap());
    t.poll(&f.registry, None).await.unwrap();
    let r = f.state.lock().unwrap().agent_receipts[&c.command_id].clone();
    assert_eq!(r["state"], "refused");
    assert_eq!(r["agentReservationId"], Value::Null);
    f.stop().await;
}
#[tokio::test]
async fn native_managed_agent_transport_unknown_upload_and_stale_ticket_keep_receipt() {
    let (f, t, c) = prepared().await;
    {
        let mut s = f.state.lock().unwrap();
        s.short_ticket = true;
        s.delay_result = true;
    }
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "allocation_ticket_expired"
    );
    assert_eq!(f.state.lock().unwrap().agent_writes, 1);
    {
        let mut s = f.state.lock().unwrap();
        s.short_ticket = false;
        s.delay_result = false;
    }
    t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
        .await
        .unwrap();
    assert_eq!(f.state.lock().unwrap().agent_writes, 1);
    f.stop().await;
}
#[tokio::test]
async fn native_managed_agent_transport_scope_loss_before_reserve_and_after_result_is_fenced() {
    let (f, t, _) = prepared().await;
    f.state.lock().unwrap().invalidate_at_contribution = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    assert_eq!(f.state.lock().unwrap().agent_writes, 0);
    f.stop().await;
    let (f, t, _) = prepared().await;
    f.state.lock().unwrap().invalidate_at_result = true;
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "management_outcome_unknown"
    );
    assert_eq!(f.state.lock().unwrap().agent_writes, 1);
    f.stop().await;
}
#[tokio::test]
async fn native_managed_agent_transport_wrong_digest_device_permission_and_ack_fail_closed() {
    let (f, t, mut c) = prepared().await;
    f.state.lock().unwrap().tamper_digest = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    f.state.lock().unwrap().tamper_digest = false;
    c.executor_device_id = "other_device".into();
    f.state.lock().unwrap().agent = Some(serde_json::to_value(&c).unwrap());
    assert!(t.poll(&f.registry, None).await.is_err());
    assert_eq!(f.state.lock().unwrap().agent_writes, 0);
    c.executor_device_id = "device".into();
    f.state.lock().unwrap().agent = Some(serde_json::to_value(&c).unwrap());
    f.state.lock().unwrap().wrong_ack = true;
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "managed_agent_acknowledgement_conflict"
    );
    assert_eq!(f.state.lock().unwrap().agent_writes, 1);
    let mut p = package(2);
    p.permissions = vec![Permission::ExecuteAllocations];
    let denied = ManagedAgentTransport::new(f.owner.management(p).await.unwrap());
    assert!(denied.poll(&f.registry, None).await.is_err());
    f.stop().await;
}

#[tokio::test]
async fn native_managed_agent_transport_new_generation_cleans_up_without_new_old_generation_hold() {
    let (f, t, c) = prepared().await;
    t.poll(&f.registry, None).await.unwrap();
    let original = f.state.lock().unwrap().agent_receipts[&c.command_id].clone();
    explicit_new_generation(&f, 2).await;
    t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
        .await
        .unwrap();
    let mut stale = c.clone();
    stale.command_id = "another_agent_reserve".into();
    stale.request_id = "another_agent_request".into();
    stale.proposed_agent_id = "another_agent".into();
    f.state.lock().unwrap().agent = Some(serde_json::to_value(&stale).unwrap());
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "managed_agent_enrollment_changed"
    );
    assert_eq!(f.state.lock().unwrap().agent_writes, 1);
    let fence = fence(&c);
    {
        let mut s = f.state.lock().unwrap();
        s.revoked_grant = true;
        s.agent = Some(serde_json::to_value(&fence).unwrap());
    }
    t.poll(&f.registry, None).await.unwrap();
    let r = f.state.lock().unwrap().agent_receipts[&fence.command_id].clone();
    assert_eq!(r["state"], "fenced");
    assert_eq!(r["retainedTokens"], 0);
    assert_eq!(r["agentReservationId"], original["agentReservationId"]);
    assert!(r["ledgerRevision"].as_u64().unwrap() > original["ledgerRevision"].as_u64().unwrap());
    f.stop().await;
}

#[tokio::test]
async fn native_managed_agent_transport_fence_ack_cannot_substitute_assignment() {
    let (f, t, c) = prepared().await;
    t.poll(&f.registry, None).await.unwrap();
    let original = f.state.lock().unwrap().agent_receipts[&c.command_id].clone();
    let fence = fence(&c);
    {
        let mut s = f.state.lock().unwrap();
        s.agent = Some(serde_json::to_value(&fence).unwrap());
        s.agent_assignment = Some(json!({
            "id":"assignment_fixture", "requestId":c.request_id,
            "agentId":c.proposed_agent_id, "managerMxid":c.manager_mxid,
            "executorDeviceId":c.executor_device_id, "executorGeneration":c.executor_generation,
            "agentReservationId":"different_child", "reserveReceiptDigest":digest(&original).unwrap(),
            "state":"fenced", "generation":1, "executionAvailable":false
        }));
    }
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "managed_agent_acknowledgement_conflict"
    );
    assert_eq!(f.state.lock().unwrap().agent_writes, 2);
    t.recover(
        &f.registry,
        fence.command_id.clone(),
        digest(&fence).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(f.state.lock().unwrap().agent_writes, 2);
    f.stop().await;
}

#[cfg(feature = "resource-adoption")]
#[tokio::test]
async fn native_managed_agent_transport_host_epoch_loss_before_commit_keeps_original_capacity() {
    let (f, _, _) = prepared().await;
    let gate = Arc::new(AtomicBool::new(true));
    let authority = gate.clone();
    let transport = ManagedAgentTransport::new(f.owner.management(package(19)).await.unwrap())
        .restrict_to_host(Arc::new(move || authority.load(Ordering::Acquire)));
    {
        let mut state = f.state.lock().unwrap();
        state.source = Some(gate);
        state.invalidate_at_contribution = true;
    }
    assert!(transport.poll(&f.registry, None).await.is_err());
    assert!(
        f.current.load(Ordering::Acquire),
        "SDK source remains valid; only native host epoch changed"
    );
    assert_eq!(f.state.lock().unwrap().agent_writes, 0);
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM owner_managed_agents", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        0
    );
    drop(db);
    f.stop().await;
}
