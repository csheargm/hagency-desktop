use super::*;
async fn bind(a: &Active, w: &SharedWorkItem) -> SharedDemandBinding {
    let offer = SharedDemandOffer {
        version: 1,
        demand_id: format!("demand_{}", w.id),
        assignment_id: w.assignment_id.clone(),
        event_id: w.event_id.clone(),
        event_digest: "d".repeat(64),
        control_revision: 1,
        received_at_ms: w.created_at_ms,
        predecessor_command_id: a.command.command_id.clone(),
        predecessor_command_digest: digest(&a.command),
    };
    let proof = VerifiedSharedDemand::new(
        enrollment_from(&a.f.profile, &a.f.resource, &a.f.grant.resource_revision, 1),
        offer.clone(),
        digest(&offer),
        graphs::now_ms().unwrap(),
        Instant::now() + Duration::from_secs(8),
    )
    .unwrap();
    let pending =
        a.f.registry
            .prepare_shared_demand(proof, a.original.clone(), Some(a.command.clone()))
            .await
            .unwrap();
    assert_eq!(*pending.disposition(), SharedDemandDisposition::Pending);
    assert!(pending.input().is_none());
    assert!(
        a.f.registry
            .receive_shared_work(a.authority(), w.clone())
            .await
            .is_err()
    );
    let binding = SharedDemandBinding {
        version: 1,
        demand_id: offer.demand_id.clone(),
        demand_digest: digest(&offer),
        control_revision: 1,
        assignment_id: w.assignment_id.clone(),
        event_id: w.event_id.clone(),
        received_at_ms: w.created_at_ms,
        runtime_command_id: a.command.command_id.clone(),
        runtime_command_digest: digest(&a.command),
        work_id: w.id.clone(),
        work_digest: digest(w),
    };
    let mut wrong = binding.clone();
    wrong.event_id = "$different".into();
    assert!(
        a.f.registry
            .bind_shared_demand_work(a.authority(), wrong, w.clone())
            .await
            .is_err()
    );
    a.f.registry
        .bind_shared_demand_work(a.authority(), binding.clone(), w.clone())
        .await
        .unwrap();
    binding
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_demand_actual_idle_process_promotes_exact_event_without_new_start() {
    let mut a = Box::pin(active()).await;
    let w = a.work("demand_current_idle");
    let binding = bind(&a, &w).await;
    a.f.registry
        .bind_shared_demand_work(a.authority(), binding, w.clone())
        .await
        .unwrap();
    let h = prepare(&a, &w).await;
    assert_eq!(h.reserved_tokens, 0);
    let attempt =
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id)
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
    a.process.start_shared_text_thread().await.unwrap();
    custody.check().unwrap();
    a.process
        .start_shared_text_turn(w.body.clone())
        .await
        .unwrap();
    let mut capture = custody.bind_turn(&a.process).unwrap();
    loop {
        let event = a.process.next_shared_text_event().await.unwrap();
        let done = matches!(
            event.observation().kind(),
            hagency_runtime::codex::session::ObservationKind::TurnEnded(_)
        );
        let receipt = capture.observe(event).await.unwrap();
        if done {
            assert_eq!(receipt.unwrap().body.as_deref(), Some("Offline answer."));
            break;
        }
    }
    let rows = a
        .f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok((
                db.db
                    .query_row("SELECT count(*) FROM owner_shared_start_intents", [], |r| {
                        r.get::<_, u64>(0)
                    })?,
                db.db.query_row(
                    "SELECT count(*) FROM owner_shared_demand_bindings",
                    [],
                    |r| r.get::<_, u64>(0),
                )?,
            ))
        })
        .await
        .unwrap();
    assert_eq!(rows, (0, 1));
    let old =
        a.f.registry
            .managed_call_history(cleanup(&a.f, &a.original, 1).await, "old_unknown".into())
            .await
            .unwrap();
    assert_eq!((old.reserved_tokens, old.known_consumed_tokens), (40, 7));
    assert!(!old.usage_complete);
    assert_eq!(old.state, "unknown");
    a.stop().await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_demand_actual_stop_revision_blocks_prepared_call_before_issue() {
    let a = Box::pin(active()).await;
    let w = a.work("demand_paused");
    bind(&a, &w).await;
    let h = prepare(&a, &w).await;
    let start =
        a.f.registry
            .begin_shared_start(a.authority(), w.clone(), h.call_id.clone())
            .await
            .unwrap();
    a.f.registry
        .stop_shared_demand_revision(cleanup(&a.f, &a.original, 1).await, 1)
        .await
        .unwrap();
    assert!(
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
            .await
            .is_err()
    );
    let saved =
        a.f.registry
            .managed_call_history(cleanup(&a.f, &a.original, 1).await, h.call_id)
            .await
            .unwrap();
    assert!(saved.issue_revision.is_none());
    assert_eq!(saved.reserved_tokens, 0);
    a.stop().await;
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_demand_actual_stopped_predecessor_claims_once_and_requires_promotion() {
    let mut a = Box::pin(active()).await;
    let predecessor = a.command.clone();
    let stopped = a
        .sink
        .record(a.process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    let offer = SharedDemandOffer {
        version: 1,
        demand_id: "after_actual_stop".into(),
        assignment_id: a.original.assignment_id.clone(),
        event_id: "$original_queued_event".into(),
        event_digest: "d".repeat(64),
        control_revision: 1,
        received_at_ms: graphs::now_ms().unwrap(),
        predecessor_command_id: predecessor.command_id.clone(),
        predecessor_command_digest: digest(&predecessor),
    };
    let proof = VerifiedSharedDemand::new(
        enrollment_from(&a.f.profile, &a.f.resource, &a.f.grant.resource_revision, 1),
        offer.clone(),
        digest(&offer),
        graphs::now_ms().unwrap(),
        Instant::now() + Duration::from_secs(8),
    )
    .unwrap();
    let saved =
        a.f.registry
            .prepare_shared_demand(proof, a.original.clone(), Some(predecessor.clone()))
            .await
            .unwrap();
    let input = saved.input().unwrap().start.clone();
    assert_eq!(input.predecessor.stopped_receipt_digest, digest(&stopped));
    assert_eq!(
        (
            input.budget_observation.retained_tokens,
            input.budget_observation.remaining_tokens
        ),
        (7, 143)
    );
    let now = graphs::now_ms().unwrap();
    let command = SharedAttemptCommand {
        web_search: false,
        tool_policy: None,
        version: 2,
        capability: SHARED_ATTEMPT_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "demand_attempt_three".into(),
        assignment_id: a.original.assignment_id.clone(),
        assignment_generation: 1,
        agent_reservation_id: a.original.agent_reservation_id.clone(),
        process_attempt: 3,
        original_runtime_command: a.original.clone(),
        original_runtime_digest: digest(&a.original),
        predecessor: input.predecessor,
        budget_observation: input.budget_observation,
        start_intent_id: input.start_intent_id,
        issued_at_ms: now,
        expires_at_ms: now + 20_000,
    };
    let SharedAttemptClaim::Issued(custody) =
        a.f.registry
            .claim_shared_successor(attempt_proof(&a.f, command.clone()))
            .await
            .unwrap()
    else {
        panic!("one fresh demand claim")
    };
    assert!(matches!(
        a.f.registry
            .claim_shared_successor(attempt_proof(&a.f, command.clone()))
            .await
            .unwrap(),
        SharedAttemptClaim::History(_)
    ));
    let (mut process, instructions) = spawn(&a.f, "demand_attempt_three").await;
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
    a.lease = SharedActivityLease {
        authority: SharedActivityRef {
            runtime_command_id: command.command_id.clone(),
            epoch: 1,
            token: "a".repeat(64),
        },
        command_digest: digest(&command),
        process_digest: sink.process_digest().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + 9_000,
    };
    a.command = command;
    a.process = process;
    a.sink = sink;
    a.expires = expires;
    let mut w = a.work("new_original_event");
    w.event_id = offer.event_id.clone();
    w.created_at_ms = offer.received_at_ms;
    // A current runtime lease alone cannot bypass the exact demand promotion.
    assert!(
        a.f.registry
            .receive_shared_work(a.authority(), w.clone())
            .await
            .is_err()
    );
    let binding = SharedDemandBinding {
        version: 1,
        demand_id: offer.demand_id.clone(),
        demand_digest: digest(&offer),
        control_revision: 1,
        assignment_id: w.assignment_id.clone(),
        event_id: w.event_id.clone(),
        received_at_ms: w.created_at_ms,
        runtime_command_id: a.command.command_id.clone(),
        runtime_command_digest: digest(&a.command),
        work_id: w.id.clone(),
        work_digest: digest(&w),
    };
    a.f.registry
        .bind_shared_demand_work(a.authority(), binding, w.clone())
        .await
        .unwrap();
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
    custody.check().unwrap();
    a.process
        .start_shared_text_turn(w.body.clone())
        .await
        .unwrap();
    let mut capture = custody.bind_turn(&a.process).unwrap();
    loop {
        let event = a.process.next_shared_text_event().await.unwrap();
        let done = matches!(
            event.observation().kind(),
            hagency_runtime::codex::session::ObservationKind::TurnEnded(_)
        );
        let receipt = capture.observe(event).await.unwrap();
        if done {
            assert_eq!(receipt.unwrap().body.as_deref(), Some("Offline answer."));
            break;
        }
    }
    assert_eq!(
        a.sink
            .record(a.process.stop_shared_text_observation())
            .await
            .unwrap()
            .state,
        "stopped"
    );
    let mut next = offer;
    next.demand_id = "later_after_unreported_usage".into();
    next.event_id = "$later_event".into();
    next.predecessor_command_id = a.command.command_id.clone();
    next.predecessor_command_digest = digest(&a.command);
    let proof = VerifiedSharedDemand::new(
        enrollment_from(&a.f.profile, &a.f.resource, &a.f.grant.resource_revision, 1),
        next.clone(),
        digest(&next),
        graphs::now_ms().unwrap(),
        Instant::now() + Duration::from_secs(8),
    )
    .unwrap();
    // No final usage was reported. Postpaid admission permits the next event
    // while the old call remains explicitly unknown; stop never invents zero.
    let later =
        a.f.registry
            .prepare_shared_demand(proof, a.original.clone(), Some(a.command.clone()))
            .await
            .unwrap();
    assert_eq!(*later.disposition(), SharedDemandDisposition::StartPrepared);
    let observation = &later.input().unwrap().start.budget_observation;
    assert_eq!(
        (observation.retained_tokens, observation.remaining_tokens),
        (7, 143)
    );
    assert!(!observation.usage_complete);
    let old =
        a.f.registry
            .managed_call_history(cleanup(&a.f, &a.original, 1).await, "old_unknown".into())
            .await
            .unwrap();
    assert_eq!((old.reserved_tokens, old.known_consumed_tokens), (40, 7));
    assert!(!old.usage_complete);
    assert_eq!(old.state, "unknown");
    a.f.registry.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE offline supervised child"]
async fn shared_demand_actual_idle_promotion_cannot_skip_older_known_event() {
    let a = Box::pin(active()).await;
    let mut first = a.work("oldest_event");
    let second = a.work("later_event");
    first.created_at_ms = second.created_at_ms - 1;
    let mut bindings = Vec::new();
    for w in [&second, &first] {
        let offer = SharedDemandOffer {
            version: 1,
            demand_id: format!("demand_{}", w.id),
            assignment_id: w.assignment_id.clone(),
            event_id: w.event_id.clone(),
            event_digest: "d".repeat(64),
            control_revision: 1,
            received_at_ms: w.created_at_ms,
            predecessor_command_id: a.command.command_id.clone(),
            predecessor_command_digest: digest(&a.command),
        };
        let proof = VerifiedSharedDemand::new(
            enrollment_from(&a.f.profile, &a.f.resource, &a.f.grant.resource_revision, 1),
            offer.clone(),
            digest(&offer),
            graphs::now_ms().unwrap(),
            Instant::now() + Duration::from_secs(8),
        )
        .unwrap();
        assert_eq!(
            *a.f.registry
                .prepare_shared_demand(proof, a.original.clone(), Some(a.command.clone()))
                .await
                .unwrap()
                .disposition(),
            SharedDemandDisposition::Pending
        );
        bindings.push(SharedDemandBinding {
            version: 1,
            demand_id: offer.demand_id.clone(),
            demand_digest: digest(&offer),
            control_revision: 1,
            assignment_id: w.assignment_id.clone(),
            event_id: w.event_id.clone(),
            received_at_ms: w.created_at_ms,
            runtime_command_id: a.command.command_id.clone(),
            runtime_command_digest: digest(&a.command),
            work_id: w.id.clone(),
            work_digest: digest(w),
        });
    }
    assert!(matches!(
        a.f.registry
            .bind_shared_demand_work(a.authority(), bindings[0].clone(), second.clone())
            .await,
        Err(Error::LocalAuthority)
    ));
    assert!(
        a.f.registry
            .receive_shared_work(a.authority(), second)
            .await
            .is_err()
    );
    a.f.registry
        .bind_shared_demand_work(a.authority(), bindings.remove(1), first.clone())
        .await
        .unwrap();
    a.f.registry
        .receive_shared_work(a.authority(), first)
        .await
        .unwrap();
    a.stop().await;
}
