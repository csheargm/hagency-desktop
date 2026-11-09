//! Real supervised offline process; no external provider calls or credentials.
use super::*;
use sha2::{Digest, Sha256};

struct Active {
    f: Fixture,
    original: SharedRuntimeCommand,
    command: SharedBudgetAttemptCommand,
    process: OwnedSession,
    sink: Arc<SharedBudgetAttemptProcessSink>,
    lease: SharedActivityLease,
    expires: Instant,
    instructions: PathBuf,
}
impl Active {
    fn authority(&self) -> VerifiedSharedActivity {
        self.sink.authorize_activity(enrollment(&self.f), &self.lease, self.expires).unwrap()
    }
    fn work(&self, id: &str) -> SharedWorkItem {
        let body = "New independent offline question".to_owned();
        SharedWorkItem {
            room_binding: None, encrypted: false,
            id: id.into(), assignment_id: self.command.assignment_id.clone(),
            runtime_command_digest: digest(&self.command), event_id: format!("${id}"),
            room_id: self.original.reserve_command.room_id.clone(),
            requester_mxid: self.original.reserve_command.manager_mxid.clone(),
            thread_root: format!("${id}"), body_digest: format!("{:x}", Sha256::digest(body.as_bytes())),
            body, execution_id: format!("execution_{id}"), created_at_ms: graphs::now_ms().unwrap(),
        }
    }
    async fn prepare(&self, w: &SharedWorkItem) -> ManagedCallHistory {
        self.f.registry.receive_shared_work(self.authority(), w.clone()).await.unwrap();
        self.f.registry.prepare_shared_call(self.authority(), w.clone()).await.unwrap()
    }
    async fn stop(mut self) {
        assert_eq!(self.sink.record(self.process.stop_shared_text_observation()).await.unwrap().state, "stopped");
        self.f.registry.shutdown().await.unwrap();
    }
}
async fn active() -> Active {
    let (f, original, _, _) = Box::pin(original_stopped()).await;
    // An already settled overrun is never erased by a new approved delta.
    call(&f, &original, "old_overrun", "settled", 150, 181, true, false).await;
    increase(&f, &original, "resume_after_overrun", 100).await;
    let command = next(&f, &original, "budget_call_process").await;
    assert_eq!((command.budget_observation.allocated_tokens, command.budget_observation.retained_tokens, command.budget_observation.remaining_tokens), (250, 181, 69));
    let SharedBudgetAttemptClaim::Issued(custody) = f.registry.claim_shared_budget_successor(delivery(&f, command.clone())).await.unwrap() else { panic!("new custody") };
    let (mut process, instructions) = spawn(&f, "budget_call").await;
    let sink = Arc::new((*custody).bind_process(process.shared_text_process_source()).unwrap());
    sink.record(process.qualify_shared_text(&instructions).await.unwrap()).await.unwrap();
    let expires = Instant::now() + Duration::from_secs(8);
    process.install_shared_text_activity(1, expires, Instant::now() + Duration::from_secs(30)).unwrap();
    let lease = SharedActivityLease {
        authority: SharedActivityRef { runtime_command_id: command.command_id.clone(), epoch: 1, token: "a".repeat(64) },
        command_digest: digest(&command), process_digest: sink.process_digest().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + 9_000,
    };
    Active { f, original, command, process, sink, lease, expires, instructions }
}
fn started(w: &SharedWorkItem) -> SharedWorkStart {
    SharedWorkStart { work: w.clone(), work_digest: digest(w), activity_epoch: 1, newly_started: true }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn budget_execution_v3_call_uses_delta_keeps_overrun_and_exact_process_after_new_head() {
    let mut a = Box::pin(active()).await;
    let page=a.f.registry.shared_successor_inventory(a.f.profile.clone(),a.original.reserve_command.executor_device_id.clone(),1,None).await.unwrap();
    assert_eq!(page.items.len(),1);assert_eq!(page.items[0].predecessor_command_id,a.command.command_id);assert_eq!(page.items[0].process_state.as_deref(),Some("qualified"));
    let original=a.f.registry.shared_original_selection(a.f.profile.clone(),a.command.assignment_id.clone()).await.unwrap().unwrap();assert_eq!(digest(&original),digest(&a.original));
    let snapshot = a.f.registry.managed_agent_budget(enrollment(&a.f), a.original.agent_reservation_id.clone()).await.unwrap();
    assert_eq!((snapshot.original_ceiling_tokens, snapshot.effective_ceiling_tokens, snapshot.known_consumed_tokens, snapshot.remaining_tokens), (150, 250, 181, 69));
    let w = a.work("new_budget_work");
    let h = a.prepare(&w).await;
    assert_eq!(h.reserved_tokens, 0);
    assert_eq!(a.f.registry.prepare_shared_call(a.authority(), w.clone()).await.unwrap().call_id, h.call_id);
    let next = a.work("parallel_work");
    a.f.registry.receive_shared_work(a.authority(), next.clone()).await.unwrap();
    // A prepared call holds nothing, so a second one is admitted with the same
    // remainder; only known usage refuses work.
    assert_eq!(a.f.registry.prepare_shared_call(a.authority(), next).await.unwrap().reserved_tokens, 0);
    let intent = a.f.registry.begin_shared_start(a.authority(), w.clone(), h.call_id.clone()).await.unwrap();
    let custody = a.f.registry.issue_shared_call(intent, started(&w)).await.unwrap();
    assert!(a.f.registry.begin_shared_start(a.authority(), w.clone(), h.call_id).await.is_err());
    a.process.start_shared_text_thread().await.unwrap();
    custody.start_turn(&mut a.process).await.unwrap();
    let mut capture = custody.bind_turn(&a.process).unwrap();
    increase(&a.f, &a.original, "after_issue", 25).await;
    // The newer head never authorizes more work on this historical process.
    assert!(a.f.registry.receive_shared_work(a.authority(), a.work("after_delta")).await.is_err());
    // Its original in-flight call can finish and retain exact usage/output.
    a.f.registry.prepare_shared_renewal(a.authority(), "finish_old_call".into()).await.unwrap();
    let receipt = loop {
        let event = a.process.next_shared_text_event().await.unwrap();
        let done = matches!(event.observation().kind(), hagency_runtime::codex::session::ObservationKind::TurnEnded(_));
        let receipt = capture.observe(event).await.unwrap();
        if done { break receipt.unwrap(); }
    };
    assert_eq!(receipt.body.as_deref(), Some("Offline answer."));
    assert_eq!(receipt.runtime_command_digest, digest(&a.command));
    assert_eq!(receipt.retained_tokens, Some(0));
    assert!(!receipt.usage_complete, "missing provider usage counts as zero but stays incomplete");
    let old = a.f.registry.managed_call_history(cleanup(&a.f, &a.original, 1).await, "old_overrun".into()).await.unwrap();
    assert_eq!((old.reserved_tokens, old.known_consumed_tokens, old.usage_complete), (150, 181, true));
    let methods = std::fs::read_to_string(a.instructions.parent().unwrap().join("methods")).unwrap();
    assert_eq!(methods.lines().filter(|l| *l == "turn/start").count(), 1);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn budget_execution_v3_head_change_before_issue_never_starts_or_refunds_uncertain_call() {
    let a = Box::pin(active()).await;
    let w = a.work("head_race");
    let h = a.prepare(&w).await;
    let intent = a.f.registry.begin_shared_start(a.authority(), w.clone(), h.call_id.clone()).await.unwrap();
    increase(&a.f, &a.original, "before_issue", 25).await;
    assert!(a.f.registry.issue_shared_call(intent, started(&w)).await.is_err());
    let saved = a.f.registry.managed_call_history(cleanup(&a.f, &a.original, 1).await, h.call_id).await.unwrap();
    assert_eq!(saved.state, "prepared");
    assert_eq!(saved.reserved_tokens, 0);
    assert!(saved.issue_revision.is_none());
    no_model_calls(&a.instructions);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn budget_execution_v3_recovered_start_and_foreign_process_cannot_issue() {
    let a = Box::pin(active()).await;
    let w = a.work("lost_response");
    let h = a.prepare(&w).await;
    let intent = a.f.registry.begin_shared_start(a.authority(), w.clone(), h.call_id.clone()).await.unwrap();
    let mut recovered = started(&w);
    recovered.newly_started = false;
    assert!(matches!(a.f.registry.issue_shared_call(intent, recovered).await, Err(Error::OutcomeUnknown)));
    assert!(matches!(a.f.registry.begin_shared_start(a.authority(), w, h.call_id).await, Err(Error::OutcomeUnknown)));
    let mut forged = a.lease.clone();
    forged.command_digest = digest(&a.original);
    assert!(a.sink.authorize_activity(enrollment(&a.f), &forged, a.expires).is_err());
    no_model_calls(&a.instructions);
    a.stop().await;
}

fn refusal(a: &Active, w: &SharedWorkItem) -> SharedBudgetWorkRefusalRecord {
    let value = SharedBudgetWorkRefusal {
        version: 1, refusal_id: format!("sbr_{}", w.id), work_id: w.id.clone(), work_digest: digest(w),
        runtime_command_id: a.command.command_id.clone(), runtime_command_digest: digest(&a.command),
        input_digest: w.body_digest.clone(), execution_budget: a.command.execution_budget.clone(),
        current_budget_digest: "9".repeat(64), state: "not_started".into(), reason: "budget_head_changed".into(),
        refused_at_ms: graphs::now_ms().unwrap(),
    };
    SharedBudgetWorkRefusalRecord { refusal_digest: digest(&value), refusal: value }
}
fn refusal_proof(a: &Active, w: &SharedWorkItem, record: SharedBudgetWorkRefusalRecord) -> VerifiedSharedBudgetWorkRefusal {
    VerifiedSharedBudgetWorkRefusal::new(enrollment(&a.f), a.command.clone(), w.clone(), record, Instant::now() + Duration::from_secs(10)).unwrap()
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn budget_execution_v3_definitive_no_start_cancels_only_original_unissued_hold() {
    let mut a = Box::pin(active()).await;
    let w = a.work("refused_work");
    let h = a.prepare(&w).await;
    let intent = a.f.registry.begin_shared_start(a.authority(), w.clone(), h.call_id.clone()).await.unwrap();
    let record = refusal(&a, &w);
    let receipt = a.f.registry.refuse_unissued_shared_budget_work(refusal_proof(&a, &w, record.clone())).await.unwrap();
    assert_eq!((receipt.state.as_str(),receipt.retained_tokens,receipt.usage_complete), ("failed",Some(0),true));
    assert!(a.f.registry.issue_shared_call(intent, started(&w)).await.is_err());
    let replay = a.f.registry.refuse_unissued_shared_budget_work(refusal_proof(&a, &w, record.clone())).await.unwrap();
    assert_eq!(digest(&receipt),digest(&replay));
    let old = a.f.registry.managed_call_history(cleanup(&a.f,&a.original,1).await,"old_overrun".into()).await.unwrap();
    assert_eq!(old.known_consumed_tokens,181);
    a.sink.record(a.process.stop_shared_text_observation()).await.unwrap();
    a.f.registry.shutdown().await.unwrap();
    a.f.registry = OwnerResourceRegistry::open_root(&a.f._temp.path().join("owner")).unwrap();
    let replay = a.f.registry.refuse_unissued_shared_budget_work(refusal_proof(&a,&w,record)).await.unwrap();
    assert_eq!(digest(&receipt),digest(&replay));
    let saved = a.f.registry.recover_shared_budget_attempt_work(cleanup(&a.f,&a.original,1).await,a.command.clone(),w.id.clone(),None).await.unwrap().unwrap();
    assert_eq!(digest(&saved.receipt.unwrap()),digest(&receipt));
    assert_eq!(a.f.registry.shared_budget_attempt_work_ids(cleanup(&a.f,&a.original,1).await,a.command.clone()).await.unwrap(),vec![w.id]);
    no_model_calls(&a.instructions);
    a.f.registry.shutdown().await.unwrap();
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn budget_execution_v3_no_start_cannot_cancel_issued_or_changed_input() {
    let a = Box::pin(active()).await;
    let w = a.work("issued_refusal");
    let h = a.prepare(&w).await;
    let mut bad = refusal(&a, &w);
    bad.refusal.input_digest = "e".repeat(64);
    bad.refusal_digest = digest(&bad.refusal);
    assert!(a.f.registry.refuse_unissued_shared_budget_work(refusal_proof(&a,&w,bad)).await.is_err());
    let intent = a.f.registry.begin_shared_start(a.authority(),w.clone(),h.call_id.clone()).await.unwrap();
    let _custody = a.f.registry.issue_shared_call(intent,started(&w)).await.unwrap();
    assert!(a.f.registry.refuse_unissued_shared_budget_work(refusal_proof(&a,&w,refusal(&a,&w))).await.is_err());
    let saved = a.f.registry.managed_call_history(cleanup(&a.f,&a.original,1).await,h.call_id).await.unwrap();
    assert_eq!((saved.state.as_str(),saved.reserved_tokens), ("issued",0));
    assert!(saved.issue_revision.is_some());
    no_model_calls(&a.instructions);
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn budget_execution_v3_no_start_and_issue_race_are_mutually_exclusive() {
    let a = Box::pin(active()).await;
    let w = a.work("refusal_issue_race");
    let h = a.prepare(&w).await;
    let intent = a.f.registry.begin_shared_start(a.authority(),w.clone(),h.call_id.clone()).await.unwrap();
    let proof = refusal_proof(&a,&w,refusal(&a,&w));
    let (issued,refused)=tokio::join!(a.f.registry.issue_shared_call(intent,started(&w)),a.f.registry.refuse_unissued_shared_budget_work(proof));
    assert_ne!(issued.is_ok(),refused.is_ok());
    let saved=a.f.registry.managed_call_history(cleanup(&a.f,&a.original,1).await,h.call_id).await.unwrap();
    assert_eq!(saved.issue_revision.is_some(),issued.is_ok());
    assert_eq!(saved.state=="cancelled_before_issue",refused.is_ok());
    no_model_calls(&a.instructions);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn budget_execution_v3_idle_demand_uses_original_event_and_stop_fences_call() {
    let a=Box::pin(active()).await;
    let w=a.work("idle_budget_demand");
    let offer=SharedDemandOffer{version:1,demand_id:"v3_idle".into(),assignment_id:w.assignment_id.clone(),event_id:w.event_id.clone(),event_digest:"a".repeat(64),control_revision:1,received_at_ms:w.created_at_ms,predecessor_command_id:a.command.command_id.clone(),predecessor_command_digest:digest(&a.command)};
    let proof=VerifiedSharedDemand::new(enrollment(&a.f),offer.clone(),digest(&offer),graphs::now_ms().unwrap(),Instant::now()+Duration::from_secs(15)).unwrap();
    let saved=a.f.registry.prepare_shared_demand(proof,a.original.clone(),None).await.unwrap();
    assert_eq!(*saved.disposition(),SharedDemandDisposition::Pending);
    let binding=SharedDemandBinding{version:1,demand_id:offer.demand_id.clone(),demand_digest:digest(&offer),control_revision:1,assignment_id:w.assignment_id.clone(),event_id:w.event_id.clone(),received_at_ms:w.created_at_ms,runtime_command_id:a.command.command_id.clone(),runtime_command_digest:digest(&a.command),work_id:w.id.clone(),work_digest:digest(&w)};
    a.f.registry.bind_shared_demand_work(a.authority(),binding,w.clone()).await.unwrap();
    let call=a.prepare(&w).await;
    let intent=a.f.registry.begin_shared_start(a.authority(),w.clone(),call.call_id).await.unwrap();
    a.f.registry.stop_shared_demand_revision(cleanup(&a.f,&a.original,1).await,1).await.unwrap();
    assert!(a.f.registry.issue_shared_call(intent,started(&w)).await.is_err());
    no_model_calls(&a.instructions);a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn encrypted_shared_input_is_pinned_locally_and_cannot_fall_back_to_event_reference() {
    let mut a=Box::pin(active()).await;
    let mut w=a.work("encrypted_native");w.encrypted=true;w.body=w.event_id.clone();w.body_digest=format!("{:x}",Sha256::digest(w.body.as_bytes()));
    a.f.registry.receive_shared_work(a.authority(),w.clone()).await.unwrap();
    assert!(a.f.registry.prepare_shared_call(a.authority(),w.clone()).await.is_err());
    assert!(a.f.registry.prepare_shared_legacy_input(a.authority(),w.clone()).await.is_err());
    let text="Secret prompt decrypted only on the executor".to_owned();
    let input=a.f.registry.prepare_shared_decrypted_input(a.authority(),w.clone(),text.clone()).await.unwrap();
    assert_eq!(input.text(),text);
    assert!(a.f.registry.prepare_shared_decrypted_input(a.authority(),w.clone(),"Changed decrypted message".into()).await.is_err());
    let replay=a.f.registry.prepare_shared_decrypted_input(a.authority(),w.clone(),text.clone()).await.unwrap();
    assert_eq!(input.input_digest(),replay.input_digest());
    assert!(!serde_json::to_string(&w).unwrap().contains(&text));
    let call=a.f.registry.prepare_shared_input_call(a.authority(),w.clone(),input.clone()).await.unwrap();
    let start=a.f.registry.begin_shared_input_start(a.authority(),w.clone(),call.call_id,input).await.unwrap();
    let custody=a.f.registry.issue_shared_call(start,started(&w)).await.unwrap();
    a.process.start_shared_text_thread().await.unwrap();
    custody.start_turn(&mut a.process).await.unwrap();
    let mut capture=custody.bind_turn(&a.process).unwrap();
    let receipt=loop {
        let event=a.process.next_shared_text_event().await.unwrap();
        let done=matches!(event.observation().kind(),hagency_runtime::codex::session::ObservationKind::TurnEnded(_));
        let result=capture.observe(event).await.unwrap();
        if done {break result.unwrap();}
    };
    assert_eq!(receipt.body.as_deref(),Some("Offline answer."));
    assert_eq!(receipt.work_digest,digest(&w));
    let answer=receipt.body.clone().unwrap();
    let content=serde_json::json!({"msgtype":"m.text","body":answer,"m.relates_to":{"rel_type":"m.thread","event_id":w.thread_root,"is_falling_back":true,"m.in_reply_to":{"event_id":w.thread_root}}});
    let delivered=SharedReplyDelivery{version:1,work_id:w.id.clone(),work_digest:digest(&w),assignment_id:w.assignment_id.clone(),runtime_command_digest:w.runtime_command_digest.clone(),room_id:w.room_id.clone(),thread_root:w.thread_root.clone(),source_event_id:w.event_id.clone(),matrix_txn_id:format!("hagency_shared_{}",w.id),matrix_event_id:"$encrypted_sent".into(),matrix_content_digest:digest(&content),body:answer.clone(),formatted_body:None,body_digest:format!("{:x}",Sha256::digest(answer.as_bytes())),sent_at_ms:graphs::now_ms().unwrap()};
    let delivered=SharedDeliveryRecord{receipt_digest:digest(&delivered),receipt:delivered};
    a.f.registry.record_shared_reply_delivery(cleanup(&a.f,&a.original,1).await,w.clone(),delivered).await.unwrap();
    let mut next=a.work("encrypted_followup");next.encrypted=true;next.body=next.event_id.clone();next.body_digest=format!("{:x}",Sha256::digest(next.body.as_bytes()));next.thread_root=w.thread_root.clone();
    a.f.registry.receive_shared_work(a.authority(),next.clone()).await.unwrap();
    let follow=a.f.registry.prepare_shared_decrypted_input(a.authority(),next.clone(),"Secret followup".into()).await.unwrap();
    assert!(follow.text().contains(&text));assert!(follow.text().contains(&answer));assert!(follow.text().ends_with("Secret followup"));
    assert!(!serde_json::to_string(&next).unwrap().contains("Secret"));
    let mut room=a.work("encrypted_room_context");room.encrypted=true;room.body=room.event_id.clone();room.body_digest=format!("{:x}",Sha256::digest(room.body.as_bytes()));room.thread_root=w.thread_root.clone();
    a.f.registry.receive_shared_work(a.authority(),room.clone()).await.unwrap();
    let prepared="Room history with all participants. Current request once.".to_string();
    let room_input=a.f.registry.prepare_shared_room_input(a.authority(),room.clone(),prepared.clone()).await.unwrap();
    assert_eq!(room_input.text(),prepared,"room history must not get the old eight-turn transcript prepended again");
    let room_replay=a.f.registry.prepare_shared_room_input(a.authority(),room.clone(),prepared).await.unwrap();
    assert_eq!(room_input.input_digest(),room_replay.input_digest());
    assert!(a.f.registry.prepare_shared_room_input(a.authority(),room,"changed snapshot".into()).await.is_err());
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn postpaid_v3_late_overrun_refuses_new_work_but_running_call_renews_and_finishes() {
    let mut a = Box::pin(active()).await;
    let w = a.work("in_flight");
    let h = a.prepare(&w).await;
    assert_eq!(h.reserved_tokens, 0);
    let intent = a.f.registry.begin_shared_start(a.authority(), w.clone(), h.call_id).await.unwrap();
    let custody = a.f.registry.issue_shared_call(intent, SharedWorkStart {
        work: w.clone(), work_digest: digest(&w), activity_epoch: 1, newly_started: true,
    }).await.unwrap();
    a.process.start_shared_text_thread().await.unwrap();
    custody.start_turn(&mut a.process).await.unwrap();
    let mut capture = custody.bind_turn(&a.process).unwrap();
    // A late genuine usage receipt overdraws both the Agent and its pool while
    // an independent, already-admitted turn is running.
    call(&a.f, &a.original, "late_overrun", "settled", 1, 10_000, true, false).await;
    let next = a.work("after_overrun");
    a.f.registry.receive_shared_work(a.authority(), next.clone()).await.unwrap();
    assert!(matches!(a.f.registry.prepare_shared_call(a.authority(), next).await, Err(Error::Capacity)));
    a.f.registry.prepare_shared_renewal(a.authority(), "finish_running_task".into()).await.unwrap();
    let receipt = loop {
        let event = a.process.next_shared_text_event().await.unwrap();
        let done = matches!(event.observation().kind(), hagency_runtime::codex::session::ObservationKind::TurnEnded(_));
        let receipt = capture.observe(event).await.unwrap();
        if done { break receipt.unwrap(); }
    };
    assert_eq!(receipt.body.as_deref(), Some("Offline answer."));
    assert_eq!(receipt.runtime_command_digest, digest(&a.command));
    a.stop().await;
}
