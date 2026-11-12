//! Loopback authenticated delivery plus the real canonical delta journal.
//! This fixture claims process custody only to pin the original assignment;
//! it never spawns a provider or pretends that the unknown process stopped.
use super::*;
use crate::console::native::{AgentBudgetProgress, AgentBudgetTransport};
use hagency_store::owner_resources::{budget::*, shared_execution::*};

#[derive(Default)]
pub(in crate::console::native::allocation_transport::tests) struct BudgetState {
    pub(in crate::console::native::allocation_transport::tests) missing_capability: bool,
    command: Option<AgentBudgetCommand>,
    recorded: Option<AgentBudgetReceipt>,
    writes: usize,
    requests: usize,
    lose_before_result: bool,
    cancel: bool,
}
pub(in crate::console::native::allocation_transport::tests) fn management(path: &str, body: &Value, s: &mut State) -> Option<Value> {
    s.budget.requests += 1;
    let at = now();
    let Some(c) = s.budget.command.clone() else {
        return Some(json!({"delivery":null,"observedAtMs":at}));
    };
    let hash = digest(&c).unwrap();
    if path.ends_with("/result") {
        assert_eq!(body["commandDigest"], hash);
        let receipt: AgentBudgetReceipt = serde_json::from_value(body["receipt"].clone()).unwrap();
        receipt.validate_for(&c).unwrap();
        if std::mem::take(&mut s.budget.lose_before_result) {
            return None;
        }
        if let Some(old) = &s.budget.recorded {
            assert_eq!(digest(old).unwrap(), digest(&receipt).unwrap());
        } else {
            s.budget.writes += 1;
            s.budget.recorded = Some(receipt.clone());
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
        "applicationAllowed":if historical{s.historical_executable}else{s.budget.recorded.is_none()&&!s.budget.cancel},
        "cancellationRequested":s.budget.cancel,"recorded":s.budget.recorded
    },"observedAtMs":at}))
}
async fn prepared_budget() -> (Fixture, AgentBudgetTransport, AgentBudgetCommand) {
    let (f, t, reserve) = prepared().await;
    assert!(matches!(
        t.poll(&f.registry, None).await.unwrap(),
        ManagedAgentProgress::Recorded { .. }
    ));
    let receipt: ManagedAgentReservationReceipt =
        serde_json::from_value(f.state.lock().unwrap().agent_receipts[&reserve.command_id].clone())
            .unwrap();
    let scope = f
        .transport
        .scope(Permission::ExecuteManagedAgents)
        .await
        .unwrap();
    let grant = f.state.lock().unwrap().grant.clone().unwrap();
    let enrollment = f.transport.enrollment(&scope, &grant, true).await.unwrap();
    let runtime = SharedRuntimeCommand {
        version: 1,
        capability: SHARED_EXECUTION_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "budget_initial_runtime".into(),
        assignment_id: "budget_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: receipt.agent_reservation_id.clone().unwrap(),
        reserve_command: reserve.clone(),
        reserve_command_digest: digest(&reserve).unwrap(),
        reserve_receipt_digest: digest(&receipt).unwrap(),
        policy: SHARED_TEXT_POLICY.into(),
        issued_at_ms: now() - 1,
        expires_at_ms: now() + 20000,
    };
    let proof = VerifiedSharedRuntimeDelivery::new(
        enrollment,
        runtime.clone(),
        digest(&runtime).unwrap(),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert!(matches!(
        f.registry.claim_shared_runtime(proof).await.unwrap(),
        SharedRuntimeClaim::Issued(_)
    ));
    let command = AgentBudgetCommand { owner_funded: false,
        version: 1,
        capability: AGENT_BUDGET_CAPABILITY.into(),
        operation: "agent.budget.increase".into(),
        command_id: "budget_delta_fixture".into(),
        top_up_id: "topup_fixture".into(),
        top_up_request_digest: "a".repeat(64),
        approval_command_id: "approval_fixture".into(),
        approval_digest: "b".repeat(64),
        approving_administrator_mxid: "@assigned_admin:test".into(),
        authority_revision: 1,
        assignment_id: runtime.assignment_id,
        assignment_generation: 1,
        reserve_command: reserve.clone(),
        reserve_command_digest: digest(&reserve).unwrap(),
        reserve_receipt: receipt.clone(),
        reserve_receipt_digest: digest(&receipt).unwrap(),
        expected_budget_revision: 1,
        expected_budget_digest: initial_budget_digest(&receipt).unwrap(),
        expected_ceiling_tokens: 100,
        add_tokens: 50,
        issued_at_ms: now() - 1,
        expires_at_ms: reserve.expires_at_ms,
    };
    f.state.lock().unwrap().budget.command = Some(command.clone());
    let transport = AgentBudgetTransport::new(f.owner.management(package(1)).await.unwrap());
    (f, transport, command)
}
async fn budget_snapshot(f: &Fixture, c: &AgentBudgetCommand) -> AgentBudgetSnapshot {
    let scope = f
        .transport
        .scope(Permission::ExecuteManagedAgents)
        .await
        .unwrap();
    let grant = f.state.lock().unwrap().grant.clone().unwrap();
    let e = f.transport.enrollment(&scope, &grant, false).await.unwrap();
    f.registry
        .managed_agent_budget(e, c.reserve_receipt.agent_reservation_id.clone().unwrap())
        .await
        .unwrap()
}
fn counts(f: &Fixture) -> (u64, u64, u64) {
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    db.query_row("SELECT (SELECT count(*) FROM owner_managed_budget_commands),(SELECT count(*) FROM owner_managed_budget_deltas),(SELECT count(*) FROM owner_managed_calls)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
}
#[tokio::test]
async fn native_managed_budget_lost_upload_restart_recovers_exact_delta_without_applying_again() {
    let (mut f, t, c) = prepared_budget().await;
    f.state.lock().unwrap().budget.lose_before_result = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    assert_eq!(counts(&f), (1, 1, 0));
    assert_eq!(f.state.lock().unwrap().budget.writes, 0);
    let before = budget_snapshot(&f, &c).await;
    assert_eq!(before.effective_ceiling_tokens, 150);
    assert!(!before.execution_supported);
    drop(t);
    f = f.reopen().await;
    let t = AgentBudgetTransport::new(f.owner.management(package(1)).await.unwrap());
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        AgentBudgetProgress::Recorded {
            budget_revision: 2,
            ..
        }
    ));
    let original = f.state.lock().unwrap().budget.recorded.clone().unwrap();
    assert_eq!(f.state.lock().unwrap().budget.writes, 1);
    f.state.lock().unwrap().budget.cancel = true;
    assert!(
        matches!(t.poll(&f.registry,None).await.unwrap(),AgentBudgetProgress::Recorded{disposition,..} if disposition=="applied")
    );
    assert_eq!(counts(&f), (1, 1, 0));
    assert_eq!(
        digest(&f.state.lock().unwrap().budget.recorded.clone().unwrap()).unwrap(),
        digest(&original).unwrap()
    );
    assert_eq!(budget_snapshot(&f, &c).await.effective_ceiling_tokens, 150);
    f.stop().await;
}
#[tokio::test]
async fn native_managed_budget_committed_ack_loss_is_original_history_and_ack_tamper_is_unknown() {
    let (f, t, c) = prepared_budget().await;
    f.state.lock().unwrap().lose_result = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    assert_eq!(f.state.lock().unwrap().budget.writes, 1);
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        AgentBudgetProgress::Recorded { .. }
    ));
    assert_eq!(counts(&f), (1, 1, 0));
    f.stop().await;
    let (f, t, c) = prepared_budget().await;
    f.state.lock().unwrap().wrong_ack = true;
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "agent_budget_acknowledgement_conflict"
    );
    assert_eq!(counts(&f), (1, 1, 0));
    f.state.lock().unwrap().wrong_ack = false;
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        AgentBudgetProgress::Recorded { .. }
    ));
    assert_eq!(f.state.lock().unwrap().budget.writes, 1);
    f.stop().await;
}
#[tokio::test]
async fn native_managed_budget_historical_reads_do_not_apply_and_cancellation_is_first_disposition()
{
    let (f, t, c) = prepared_budget().await;
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        AgentBudgetProgress::HistoricalReceiptMissing { .. }
    ));
    assert_eq!(counts(&f), (0, 0, 0));
    f.state.lock().unwrap().budget.cancel = true;
    assert!(
        matches!(t.poll(&f.registry,None).await.unwrap(),AgentBudgetProgress::Recorded{disposition,..} if disposition=="cancelled_before_apply")
    );
    f.state.lock().unwrap().budget.cancel = false;
    assert!(
        matches!(t.poll(&f.registry,None).await.unwrap(),AgentBudgetProgress::Recorded{disposition,..} if disposition=="cancelled_before_apply")
    );
    assert_eq!(counts(&f), (1, 0, 0));
    assert_eq!(budget_snapshot(&f, &c).await.effective_ceiling_tokens, 100);
    f.stop().await;
}
#[tokio::test]
async fn native_managed_budget_missing_capability_bad_ticket_and_digest_have_no_effect() {
    let (f, t, c) = prepared_budget().await;
    f.state.lock().unwrap().budget.missing_capability = true;
    assert_eq!(
        t.poll(&f.registry, None).await.unwrap_err().code,
        "agent_budget_protocol_unavailable"
    );
    assert_eq!(f.state.lock().unwrap().budget.requests, 0);
    f.state.lock().unwrap().budget.missing_capability = false;
    f.state.lock().unwrap().expired_ticket = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    f.state.lock().unwrap().expired_ticket = false;
    f.state.lock().unwrap().tamper_digest = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    f.state.lock().unwrap().tamper_digest = false;
    f.state.lock().unwrap().historical_executable = true;
    assert!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .is_err()
    );
    assert_eq!(counts(&f), (0, 0, 0));
    let mut denied = package(2);
    denied.permissions = vec![Permission::ReadIntents];
    let denied = AgentBudgetTransport::new(f.owner.management(denied).await.unwrap());
    assert!(denied.poll(&f.registry, None).await.is_err());
    f.stop().await;
}
#[tokio::test]
async fn native_managed_budget_source_loss_after_commit_preserves_exact_local_liability() {
    let (f, t, c) = prepared_budget().await;
    f.state.lock().unwrap().invalidate_at_result = true;
    assert!(t.poll(&f.registry, None).await.is_err());
    assert_eq!(counts(&f), (1, 1, 0));
    assert_eq!(f.state.lock().unwrap().budget.writes, 1);
    // Fresh SDK proof restores only the same original journal. No new delta.
    f.current.store(true, Ordering::Release);
    f.state.lock().unwrap().invalidate_at_result = false;
    assert!(matches!(
        t.recover(&f.registry, c.command_id.clone(), digest(&c).unwrap())
            .await
            .unwrap(),
        AgentBudgetProgress::Recorded { .. }
    ));
    assert_eq!(counts(&f), (1, 1, 0));
    f.stop().await;
}
