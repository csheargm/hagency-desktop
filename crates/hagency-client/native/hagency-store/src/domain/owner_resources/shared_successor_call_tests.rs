use super::*;
use sha2::{Digest, Sha256};
struct Active {
    f: Fixture,
    original: SharedRuntimeCommand,
    command: SharedAttemptCommand,
    process: OwnedSession,
    sink: Arc<SharedAttemptProcessSink>,
    lease: SharedActivityLease,
    expires: Instant,
}
impl Active {
    fn authority(&self) -> VerifiedSharedActivity {
        self.sink
            .authorize_activity(
                enrollment_from(
                    &self.f.profile,
                    &self.f.resource,
                    &self.f.grant.resource_revision,
                    1,
                ),
                &self.lease,
                self.expires,
            )
            .unwrap()
    }
    fn work(&self, id: &str) -> SharedWorkItem {
        let body = "New independent offline question".to_owned();
        SharedWorkItem {
            room_binding: None, encrypted: false,
            id: id.into(),
            assignment_id: self.command.assignment_id.clone(),
            runtime_command_digest: digest(&self.command),
            event_id: format!("${id}"),
            room_id: self.original.reserve_command.room_id.clone(),
            requester_mxid: self.original.reserve_command.manager_mxid.clone(),
            thread_root: format!("${id}"),
            body_digest: format!("{:x}", Sha256::digest(body.as_bytes())),
            body,
            execution_id: format!("execution_{id}"),
            created_at_ms: graphs::now_ms().unwrap(),
        }
    }
    async fn stop(self) {
        let mut process = self.process;
        assert_eq!(
            self.sink
                .record(process.stop_shared_text_observation())
                .await
                .unwrap()
                .state,
            "stopped"
        );
        self.f.registry.shutdown().await.unwrap();
    }
}
async fn active() -> Active {
    Box::pin(active_with_sibling(false)).await
}
async fn active_with_sibling(sibling: bool) -> Active {
    active_mode(sibling, "text").await
}
async fn active_mode(sibling: bool, mode: &str) -> Active {
    let (f, original, _, _) = Box::pin(original_stopped()).await;
    if sibling {
        let reserve_command = agent(&f, "sibling", 100);
        let receipt = reserve(&f, reserve_command.clone()).await;
        let mut other = original.clone();
        other.command_id = "sibling_runtime".into();
        other.assignment_id = "sibling_assignment".into();
        other.agent_reservation_id = receipt.agent_reservation_id.clone().unwrap();
        other.reserve_command_digest = digest(&reserve_command);
        other.reserve_receipt_digest = digest(&receipt);
        other.reserve_command = reserve_command;
        f.registry
            .claim_shared_runtime(proof(&f, other.clone()))
            .await
            .unwrap();
        call(
            &f,
            &other,
            "sibling_unknown",
            "unknown",
            40,
            7,
            false,
            false,
        )
        .await;
    }
    call(&f, &original, "old_unknown", "unknown", 40, 7, false, false).await;
    let command = attempt(&f, &original).await;
    let SharedAttemptClaim::Issued(custody) = f
        .registry
        .claim_shared_successor(attempt_proof(&f, command.clone()))
        .await
        .unwrap()
    else {
        panic!("new custody")
    };
    let (mut process, instructions) = spawn_mode(&f, "call_successor", mode).await;
    let sink = Arc::new(
        (*custody)
            .bind_process(process.shared_text_process_source())
            .unwrap(),
    );
    sink.record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    let expires = Instant::now() + Duration::from_secs(8);
    process
        .install_shared_text_activity(1, expires, Instant::now() + Duration::from_secs(30))
        .unwrap();
    let lease = SharedActivityLease {
        authority: SharedActivityRef {
            runtime_command_id: command.command_id.clone(),
            epoch: 1,
            token: "a".repeat(64),
        },
        command_digest: digest(&command),
        process_digest: sink.process_digest().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + 9_000,
    };
    Active {
        f,
        original,
        command,
        process,
        sink,
        lease,
        expires,
    }
}
async fn prepare(a: &Active, w: &SharedWorkItem) -> ManagedCallHistory {
    a.f.registry
        .receive_shared_work(a.authority(), w.clone())
        .await
        .unwrap();
    a.f.registry
        .prepare_shared_call(a.authority(), w.clone())
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_successor_call_failed_start_keeps_zero_and_unknown_distinct_after_retirement() {
    for dispatched in [false, true] {
        let mut a = Box::pin(active()).await;
        let w = a.work("failed_start");
        let h = prepare(&a, &w).await;
        let attempt = a.f.registry.begin_shared_start(a.authority(), w.clone(), h.call_id).await.unwrap();
        let custody = a.f.registry.issue_shared_call(attempt, SharedWorkStart {
            work: w.clone(), work_digest: digest(&w), activity_epoch: 1, newly_started: true,
        }).await.unwrap();
        a.process.start_shared_text_thread().await.unwrap();
        if dispatched { custody.start_turn(&mut a.process).await.unwrap(); }
        // Cleanup owns the original process evidence even after management
        // authority has retired. It can never authorize another model call.
        a.f.profile.retire();
        a.f.registry.0.closed.store(true, Ordering::Release);
        assert!(custody.check().is_err());
        let receipt = custody.finish_failed_start().await.unwrap();
        assert_eq!(receipt.known_consumed_tokens, 0);
        assert_eq!(receipt.usage_complete, !dispatched);
        // Neither start charges more than its known usage; the state and
        // completeness tell them apart.
        assert_eq!(receipt.retained_tokens, Some(0));
        assert_eq!(receipt.state, if dispatched {"unknown"} else {"failed"});
        assert!(a.process.start_shared_text_turn(w.body.clone()).await.is_err());
        let stop = a.sink.record(a.process.stop_shared_text_observation()).await.unwrap();
        assert_eq!(stop.state, "stopped", "dispatched={dispatched}; cleanup={:?}; guardian={}", a.process.stop(), a.process.guardian_stderr_tail());
        // Reset only the fixture's close flag so shutdown joins its writer.
        a.f.registry.0.closed.store(false, Ordering::Release);
        a.f.registry.shutdown().await.unwrap();
    }
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_successor_call_holds_old_unknown_and_captures_only_exact_new_input() {
    let mut a = Box::pin(active()).await;
    let w = a.work("new_work");
    let h = prepare(&a, &w).await;
    assert_eq!(h.reserved_tokens, 0);
    let replay =
        a.f.registry
            .prepare_shared_call(a.authority(), w.clone())
            .await
            .unwrap();
    assert_eq!(h.call_id, replay.call_id);
    let mut old = w.clone();
    old.runtime_command_digest = digest(&a.original);
    assert!(
        a.f.registry
            .receive_shared_work(a.authority(), old)
            .await
            .is_err()
    );
    let attempt =
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id.clone())
            .await
            .unwrap();
    let custody =
        a.f.registry
            .issue_shared_call(
                attempt,
                SharedWorkStart {
                    work: w.clone(),
                    work_digest: digest(&w),
                    activity_epoch: 1,
                    newly_started: true,
                },
            )
            .await
            .unwrap();
    let next = a.work("another");
    a.f.registry
        .receive_shared_work(a.authority(), next.clone())
        .await
        .unwrap();
    // A call that has not started holds nothing, so a second one is admitted
    // with the same remainder.
    assert_eq!(
        a.f.registry
            .prepare_shared_call(a.authority(), next)
            .await
            .unwrap()
            .reserved_tokens,
        0
    );
    assert!(matches!(
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id)
            .await,
        Err(Error::OutcomeUnknown)
    ));
    a.process.start_shared_text_thread().await.unwrap();
    custody.check().unwrap();
    a.process
        .start_shared_text_turn(w.body.clone())
        .await
        .unwrap();
    let mut capture = custody.bind_turn(&a.process).unwrap();
    let final_receipt = loop {
        let event = a.process.next_shared_text_event().await.unwrap();
        let done = matches!(
            event.observation().kind(),
            hagency_runtime::codex::session::ObservationKind::TurnEnded(_)
        );
        let receipt = capture.observe(event).await.unwrap();
        if done {
            break receipt.unwrap();
        }
    };
    assert_eq!(final_receipt.body.as_deref(), Some("Offline answer."));
    assert_eq!(final_receipt.runtime_command_digest, digest(&a.command));
    assert_eq!(final_receipt.retained_tokens, Some(0));
    assert!(!final_receipt.usage_complete);
    let old =
        a.f.registry
            .managed_call_history(cleanup(&a.f, &a.original, 1).await, "old_unknown".into())
            .await
            .unwrap();
    assert_eq!((old.reserved_tokens, old.known_consumed_tokens), (40, 7));
    assert!(!old.usage_complete);
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_successor_call_lost_or_replayed_start_cannot_issue_model() {
    let a = Box::pin(active()).await;
    let w = a.work("uncertain_start");
    let h = prepare(&a, &w).await;
    let start =
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id.clone())
            .await
            .unwrap();
    assert!(matches!(
        a.f.registry
            .issue_shared_call(
                start,
                SharedWorkStart {
                    work: w.clone(),
                    work_digest: digest(&w),
                    activity_epoch: 1,
                    newly_started: false
                }
            )
            .await,
        Err(Error::OutcomeUnknown)
    ));
    assert!(matches!(
        a.f.registry
            .begin_shared_start(a.authority(), w, h.call_id.clone())
            .await,
        Err(Error::OutcomeUnknown)
    ));
    let held =
        a.f.registry
            .managed_call_history(cleanup(&a.f, &a.original, 1).await, h.call_id)
            .await
            .unwrap();
    assert_eq!(held.state, "prepared");
    assert_eq!(held.reserved_tokens, 0);
    assert!(held.issue_revision.is_none());
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_successor_call_rechecks_overrun_before_issue() {
    let a = Box::pin(active()).await;
    let w = a.work("overrun_before_issue");
    let h = prepare(&a, &w).await;
    let start =
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id)
            .await
            .unwrap();
    a.f.registry
        .0
        .store
        .call(1024, |db| {
            db.db.execute(
                "UPDATE owner_managed_calls SET known_consumed_tokens=150 WHERE id='old_unknown'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        a.f.registry
            .issue_shared_call(
                start,
                SharedWorkStart {
                    work: w.clone(),
                    work_digest: digest(&w),
                    activity_epoch: 1,
                    newly_started: true
                }
            )
            .await,
        Err(Error::Capacity)
    ));
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_successor_call_input_mismatch_never_attributes_other_text() {
    let mut a = Box::pin(active()).await;
    let w = a.work("wrong_input");
    let h = prepare(&a, &w).await;
    let start =
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id)
            .await
            .unwrap();
    let custody =
        a.f.registry
            .issue_shared_call(
                start,
                SharedWorkStart {
                    work: w.clone(),
                    work_digest: digest(&w),
                    activity_epoch: 1,
                    newly_started: true,
                },
            )
            .await
            .unwrap();
    a.process.start_shared_text_thread().await.unwrap();
    a.process
        .start_shared_text_turn("wrong offline text".into())
        .await
        .unwrap();
    assert!(custody.bind_turn(&a.process).is_err());
    let n =
        a.f.registry
            .0
            .store
            .call(1024, |db| {
                Ok(db.db.query_row(
                    "SELECT COUNT(*) FROM owner_shared_work_observations",
                    [],
                    |r| r.get::<_, u64>(0),
                )?)
            })
            .await
            .unwrap();
    assert_eq!(n, 0);
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_successor_call_bounded_sibling_liability_is_preserved_without_blanket_refusal() {
    let a = Box::pin(active_with_sibling(true)).await;
    let w = a.work("with_sibling");
    let h = prepare(&a, &w).await;
    assert_eq!(h.reserved_tokens, 0);
    let prior=a.f.registry.0.store.call(1024,|db|Ok(db.db.query_row("SELECT reserved_tokens,known_consumed_tokens FROM owner_managed_calls WHERE id='sibling_unknown'",[],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?)))?)).await.unwrap();
    assert_eq!(prior, (40, 7));
    let start =
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id)
            .await
            .unwrap();
    a.f.registry.0.store.call(1024,|db|{db.db.execute("UPDATE owner_managed_calls SET known_consumed_tokens=1200 WHERE id='sibling_unknown'",[])?;Ok(())}).await.unwrap();
    assert!(matches!(
        a.f.registry
            .issue_shared_call(
                start,
                SharedWorkStart {
                    work: w.clone(),
                    work_digest: digest(&w),
                    activity_epoch: 1,
                    newly_started: true
                }
            )
            .await,
        Err(Error::Capacity)
    ));
    a.stop().await;
}

#[cfg(feature = "shared-intake")]
#[path = "shared_demand_process_tests.rs"]
mod demand_tests;

#[cfg(feature="shared-context")]
#[path="shared_context_call_tests.rs"]
mod context;

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn postpaid_v2_late_overrun_refuses_new_work_but_running_call_renews_and_finishes() {
    let mut a = Box::pin(active()).await;
    let w = a.work("in_flight");
    let h = prepare(&a, &w).await;
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

#[path = "shared_native_session_tests.rs"]
mod native_session;
