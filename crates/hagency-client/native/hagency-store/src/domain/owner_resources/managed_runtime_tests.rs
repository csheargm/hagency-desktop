use super::*;
fn digest<T: Serialize>(v: &T) -> String {
    canonical::digest(&serde_json::to_value(v).unwrap()).unwrap()
}
async fn setup(f: &Fixture) -> ManagedRuntimeProvisionCommand {
    let reserve_command = agent(f, "runtime", 150);
    let saved = reserve(f, reserve_command.clone()).await;
    let now = graphs::now_ms().unwrap();
    ManagedRuntimeProvisionCommand {
        version: 1,
        capability: AGENT_RUNTIME_CAPABILITY.into(),
        operation: "agent.runtime.provision".into(),
        command_id: "runtime_provision".into(),
        assignment_id: "assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: saved.agent_reservation_id.clone().unwrap(),
        reserve_command_digest: digest(&reserve_command),
        reserve_receipt_digest: digest(&saved),
        reserve_command,
        policy: "codex_no_tools_v1".into(),
        issued_at_ms: now,
        expires_at_ms: now + 30_000,
    }
}
fn proof(f: &Fixture, c: ManagedRuntimeCommand, generation: u64) -> VerifiedManagedRuntimeDelivery {
    let e = enrollment_from(
        &f.profile,
        &f.resource,
        &f.grant.resource_revision,
        generation,
    );
    VerifiedManagedRuntimeDelivery::new(
        e,
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
fn stop(c: &ManagedRuntimeProvisionCommand) -> ManagedRuntimeCommand {
    ManagedRuntimeCommand::Stop(ManagedRuntimeStopCommand {
        version: 1,
        capability: AGENT_RUNTIME_CAPABILITY.into(),
        operation: "agent.runtime.stop".into(),
        command_id: format!("stop_{}", c.command_id),
        provision_command: c.clone(),
        provision_command_digest: digest(c),
        issued_at_ms: graphs::now_ms().unwrap(),
    })
}
fn clean(f: &Fixture, c: &ManagedRuntimeProvisionCommand) -> ManagedCleanupAuthority {
    ManagedCleanupAuthority::fixture(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        c.agent_reservation_id.clone(),
        Instant::now() + Duration::from_secs(20),
    )
}
#[tokio::test]
async fn managed_runtime_claim_is_atomic_one_shot_and_restart_only_reads_history() {
    let mut f = fixture().await;
    let c = setup(&f).await;
    let p = ManagedRuntimeCommand::Provision(c.clone());
    let first = f
        .registry
        .claim_managed_runtime(proof(&f, p.clone(), 1))
        .await
        .unwrap();
    let ManagedRuntimeClaim::Issued(custody) = first else {
        panic!("first issue");
    };
    assert_eq!(custody.command().command_id, c.command_id);
    assert_eq!(
        custody.effect_id(),
        format!("{}_provision", c.agent_reservation_id)
    );
    assert!(!custody.incarnation().is_empty());
    assert_eq!(custody.provider_digest().len(), 64);
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, p.clone(), 1))
            .await
            .unwrap(),
        ManagedRuntimeClaim::History(_)
    ));
    let h = f
        .registry
        .managed_runtime_history(clean(&f, &c), c.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(h.issued_revision.is_some());
    assert!(!h.permanently_stopped);
    assert_eq!(h.source_effect_state, "pending");
    drop(custody);
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, p, 1))
            .await
            .unwrap(),
        ManagedRuntimeClaim::History(_)
    ));
    let mut substitute = c.clone();
    substitute.command_id = "replacement".into();
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, ManagedRuntimeCommand::Provision(substitute), 1))
            .await,
        Err(Error::Conflict)
    ));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_runtime_stop_first_is_permanent_and_keeps_capacity_unknown() {
    let f = fixture().await;
    let c = setup(&f).await;
    let s = stop(&c);
    let h = f
        .registry
        .fence_managed_runtime(proof(&f, s.clone(), 1))
        .await
        .unwrap();
    assert!(h.permanently_stopped);
    assert!(h.issued_revision.is_none());
    assert_eq!(h.source_effect_state, "pending");
    let again = f
        .registry
        .fence_managed_runtime(proof(&f, s, 2))
        .await
        .unwrap();
    assert_eq!(h.stop_revision, again.stop_revision);
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, ManagedRuntimeCommand::Provision(c), 1))
            .await
            .unwrap(),
        ManagedRuntimeClaim::History(_)
    ));
    let result = apply(&f.registry, &f.profile, release(&f, "release_after_stop")).await;
    assert_eq!(result.state, "refused");
    assert_eq!(result.reason.as_deref(), Some("unsettled_usage"));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_runtime_issue_then_stop_never_grants_a_successor() {
    let f = fixture().await;
    let c = setup(&f).await;
    let p = ManagedRuntimeCommand::Provision(c.clone());
    let ManagedRuntimeClaim::Issued(_held) = f
        .registry
        .claim_managed_runtime(proof(&f, p.clone(), 1))
        .await
        .unwrap()
    else {
        panic!("issue");
    };
    let h = f
        .registry
        .fence_managed_runtime(proof(&f, stop(&c), 2))
        .await
        .unwrap();
    assert!(h.permanently_stopped);
    assert!(h.stop_revision.unwrap() > h.issued_revision.unwrap());
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, p, 1))
            .await
            .unwrap(),
        ManagedRuntimeClaim::History(_)
    ));
    let mut replacement = c.clone();
    replacement.assignment_id = "other_assignment".into();
    assert!(
        f.registry
            .fence_managed_runtime(proof(&f, stop(&replacement), 2))
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_runtime_rejects_recovery_wrong_receipt_generation_and_expiry() {
    let f = fixture().await;
    let c = setup(&f).await;
    let p = ManagedRuntimeCommand::Provision(c.clone());
    let e = enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1);
    assert!(
        VerifiedManagedRuntimeDelivery::new(
            e,
            p.clone(),
            digest(&p),
            false,
            Instant::now() + Duration::from_secs(20)
        )
        .is_err()
    );
    let e = enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2);
    assert!(
        VerifiedManagedRuntimeDelivery::new(
            e,
            p.clone(),
            digest(&p),
            true,
            Instant::now() + Duration::from_secs(20)
        )
        .is_err()
    );
    let mut wrong = c.clone();
    wrong.reserve_receipt_digest = "b".repeat(64);
    assert!(
        f.registry
            .claim_managed_runtime(proof(&f, ManagedRuntimeCommand::Provision(wrong), 1))
            .await
            .is_err()
    );
    let mut expired = c.clone();
    expired.expires_at_ms = graphs::now_ms().unwrap();
    expired.issued_at_ms = expired.expires_at_ms - 1;
    let p = ManagedRuntimeCommand::Provision(expired);
    let e = enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1);
    assert!(
        VerifiedManagedRuntimeDelivery::new(
            e,
            p.clone(),
            digest(&p),
            true,
            Instant::now() + Duration::from_secs(20)
        )
        .is_err()
    );
    assert!(
        f.registry
            .managed_runtime_history(clean(&f, &c), c)
            .await
            .unwrap()
            .is_none()
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_runtime_transaction_failure_rolls_back_pin_effect_and_revision() {
    let f = fixture().await;
    let c = setup(&f).await;
    let before: u64 = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db
                .db
                .query_row("SELECT revision FROM owner_resource_identity", [], |r| {
                    r.get(0)
                })?)
        })
        .await
        .unwrap();
    f.registry.0.store.call(1024,|db| {db.db.execute_batch("CREATE TRIGGER fail_runtime BEFORE INSERT ON owner_managed_runtime_lifetimes BEGIN SELECT RAISE(ABORT,'fixture');END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .claim_managed_runtime(proof(&f, ManagedRuntimeCommand::Provision(c.clone()), 1))
            .await
            .is_err()
    );
    let child = c.agent_reservation_id;
    assert!(f.registry.0.store.call(1024,move|db| Ok(db.db.query_row("SELECT (SELECT revision FROM owner_resource_identity)=?1 AND (SELECT state FROM owner_managed_effects WHERE child_id=?2)='pending' AND NOT EXISTS(SELECT 1 FROM owner_managed_assignment_pins)",params![before,child],|r|r.get::<_,bool>(0))?)).await.unwrap());
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_runtime_existing_issued_effect_is_not_relabelled_unissued() {
    let f = fixture().await;
    let c = setup(&f).await;
    let a = ManagedAssignmentAuthority::fixture(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        c.agent_reservation_id.clone(),
        c.assignment_id.clone(),
        1,
        Instant::now() + Duration::from_secs(20),
    );
    assert!(matches!(
        f.registry.claim_managed_provision(a).await.unwrap(),
        ManagedProvisionClaim::Issued(_)
    ));
    assert!(matches!(
        f.registry
            .prepare_managed_runtime_delivery(proof(
                &f,
                ManagedRuntimeCommand::Provision(c.clone()),
                1
            ))
            .await,
        Err(Error::OutcomeUnknown)
    ));
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, ManagedRuntimeCommand::Provision(c.clone()), 1))
            .await,
        Err(Error::OutcomeUnknown)
    ));
    let h = f
        .registry
        .fence_managed_runtime(proof(&f, stop(&c), 1))
        .await
        .unwrap();
    assert_eq!(h.source_effect_state, "issued");
    assert!(h.permanently_stopped);
    f.registry.shutdown().await.unwrap();
}
#[test]
fn managed_runtime_canonical_contract_matches_central_without_refund_fields() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "capacity/managed_agents/custody/runtime/management-agent-runtime-v1.json"
    ))
    .unwrap();
    for (kind, receipt) in [("provision", "qualified"), ("stop", "stopped")] {
        let c: ManagedRuntimeCommand =
            serde_json::from_value(fixture[format!("{kind}Command")].clone()).unwrap();
        c.validate("@owner:example.test").unwrap();
        assert_eq!(digest(&c), fixture[format!("{kind}CommandDigest")]);
        let r: ManagedRuntimeReceipt =
            serde_json::from_value(fixture[format!("{receipt}Receipt")].clone()).unwrap();
        r.validate_for(&c).unwrap();
        assert_eq!(digest(&r), fixture[format!("{receipt}ReceiptDigest")]);
        let mut bad = serde_json::to_value(&r).unwrap();
        bad["capacityReleased"] = json!(true);
        assert!(serde_json::from_value::<ManagedRuntimeReceipt>(bad).is_err());
    }
}

#[tokio::test]
async fn managed_runtime_schema79_upgrade_rolls_back_then_preserves_native_child_receipts() {
    let mut f = fixture().await;
    let c = setup(&f).await;
    let root = f._temp.path().join("owner");
    let original = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            c.reserve_command.command_id.clone(),
            c.reserve_command_digest.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE owner_managed_runtime_observations;DROP TABLE owner_managed_runtime_stops;DROP TABLE owner_managed_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=78;CREATE TABLE owner_managed_runtime_stops(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        78
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='owner_managed_runtime_lifetimes'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TABLE owner_managed_runtime_stops")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let recovered = f
        .registry
        .recover_managed_agent_reservation(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            c.reserve_command.command_id.clone(),
            c.reserve_command_digest.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&original), digest(&recovered));
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, ManagedRuntimeCommand::Provision(c), 1))
            .await
            .unwrap(),
        ManagedRuntimeClaim::Issued(_)
    ));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn managed_runtime_concurrent_claims_only_issue_once_and_stop_history_is_retained() {
    let f = fixture().await;
    let c = setup(&f).await;
    let p = ManagedRuntimeCommand::Provision(c.clone());
    let (a, b) = tokio::join!(
        f.registry.claim_managed_runtime(proof(&f, p.clone(), 1)),
        f.registry.claim_managed_runtime(proof(&f, p, 1))
    );
    assert_eq!(
        usize::from(matches!(a.unwrap(), ManagedRuntimeClaim::Issued(_)))
            + usize::from(matches!(b.unwrap(), ManagedRuntimeClaim::Issued(_))),
        1
    );
    f.registry
        .fence_managed_runtime(proof(&f, stop(&c), 1))
        .await
        .unwrap();
    assert!(
        f.registry
            .0
            .store
            .call(1024, |db| {
                for table in [
                    "owner_managed_runtime_lifetimes",
                    "owner_managed_runtime_stops",
                ] {
                    assert!(db.db.execute(&format!("DELETE FROM {table}"), []).is_err());
                    assert!(
                        db.db
                            .execute(&format!("UPDATE {table} SET command_id='replacement'"), [])
                            .is_err()
                    );
                }
                Ok(true)
            })
            .await
            .unwrap()
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_runtime_absent_stop_receipt_and_unknown_issued_cleanup_are_distinct() {
    for issued in [false, true] {
        let f = fixture().await;
        let c = setup(&f).await;
        if issued {
            assert!(matches!(
                f.registry
                    .claim_managed_runtime(proof(
                        &f,
                        ManagedRuntimeCommand::Provision(c.clone()),
                        1
                    ))
                    .await
                    .unwrap(),
                ManagedRuntimeClaim::Issued(_)
            ));
        }
        let stop = stop(&c);
        f.registry
            .fence_managed_runtime(proof(&f, stop.clone(), 2))
            .await
            .unwrap();
        let r = f
            .registry
            .collect_managed_runtime_stop(proof(&f, stop.clone(), 2))
            .await
            .unwrap();
        assert_eq!(r.state, if issued { "unknown" } else { "not_started" });
        r.validate_for(&stop).unwrap();
        let replay = f
            .registry
            .collect_managed_runtime_stop(proof(&f, stop.clone(), 2))
            .await
            .unwrap();
        assert_eq!(digest(&r), digest(&replay));
        let recovered = f
            .registry
            .recover_managed_runtime_receipt(
                f.profile.clone(),
                f.grant.executor_device_id.clone(),
                stop.id().into(),
                digest(&stop),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(digest(&r), digest(&recovered));
        assert!(
            f.registry
                .recover_managed_runtime_receipt(
                    authority('b'),
                    f.grant.executor_device_id.clone(),
                    stop.id().into(),
                    digest(&stop)
                )
                .await
                .is_err()
        );
        let released = apply(&f.registry, &f.profile, release(&f, "still_held")).await;
        assert_eq!(released.state, "refused");
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from hagency-runtime's supervised fixture"]
async fn managed_runtime_supervised_process_evidence_survives_retirement_and_late_cleanup() {
    use hagency_runtime::{
        codex::{session::Settings, transport::Limits},
        owned::{
            MANAGED_DISABLED_FEATURES, MANAGED_INSTRUCTIONS, MANAGED_INSTRUCTIONS_FILE,
            OwnedSession,
        },
    };
    let binary = std::path::PathBuf::from(
        std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("supervised fixture path"),
    );
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let gate = live.clone();
    let profile = authority('a')
        .with_native_gate(move || gate.load(Ordering::Acquire))
        .unwrap();
    let provider = VerifiedProviderAccount::new(
        "codex",
        "openai.chatgpt",
        "fixture_account",
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let resource = registry
        .prepare_enrollment(
            profile.clone(),
            "runtime_enroll".into(),
            provider,
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            1000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id;
    let grant = grant(&registry, &profile, &resource, "parent", 700)
        .await
        .unwrap();
    let project = command(&grant, "project", 400);
    let receipt = apply(&registry, &profile, project.clone()).await;
    let mut f = Fixture {
        _temp: temp,
        registry,
        profile,
        resource,
        grant,
        project,
        receipt,
    };
    let c = setup(&f).await;
    let ManagedRuntimeClaim::Issued(custody) = f
        .registry
        .claim_managed_runtime(proof(&f, ManagedRuntimeCommand::Provision(c.clone()), 1))
        .await
        .unwrap()
    else {
        panic!("issued");
    };
    f.registry
        .check_managed_runtime_custody(&custody)
        .await
        .unwrap();
    let root = f._temp.path().join("process");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let instructions = root.join(MANAGED_INSTRUCTIONS_FILE);
    std::fs::write(&instructions, MANAGED_INSTRUCTIONS).unwrap();
    let features: serde_json::Map<String, serde_json::Value> = MANAGED_DISABLED_FEATURES
        .iter()
        .map(|k| ((*k).into(), false.into()))
        .collect();
    let config = json!({"model_provider":"openai","chatgpt_base_url":"https://chatgpt.com/backend-api","model_providers":{},"model_instructions_file":instructions,"instructions":"","notify":[],"web_search":"disabled","project_doc_max_bytes":0,"skills":{"include_instructions":false,"bundled":{"enabled":false}},"developer_instructions":"","include_apps_instructions":false,"include_environment_context":false,"mcp_servers":{},"features":features});
    std::fs::write(
        root.join("config.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .unwrap();
    let launch = hagency_platform::Launch {
        executable: binary.clone(),
        arguments: vec!["normal".into(), root.as_os_str().into()],
        directory: root.clone(),
        environment: std::collections::BTreeMap::new(),
        require_crash_containment: false,
    };
    let settings = Settings::new(root.clone(), "fixture".into(), "medium".into())
        .unwrap()
        .read_only();
    let mut process = OwnedSession::spawn(
        &binary,
        &launch,
        settings,
        Limits {
            write_timeout_ms: 500,
            event_wait_ms: 1500,
            lifetime_ms: 10_000,
        },
        1500,
    )
    .unwrap();
    process.initialize().await.unwrap();
    let sink = f
        .registry
        .bind_managed_runtime_process(custody, process.managed_process_source())
        .unwrap();
    let observed = process
        .qualify_managed_no_tools(&instructions)
        .await
        .unwrap();
    let qualified = f
        .registry
        .record_managed_runtime_process(&sink, observed)
        .await
        .unwrap();
    assert_eq!(qualified.receipt.as_ref().unwrap().state, "qualified");
    let late = process
        .qualify_managed_no_tools(&instructions)
        .await
        .unwrap();
    live.store(false, Ordering::Release);
    let stopped = f
        .registry
        .record_managed_runtime_process(&sink, process.stop_managed_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    assert!(stopped.receipt.is_none());
    assert!(stopped.ledger_revision > qualified.ledger_revision);
    assert!(
        f.registry
            .record_managed_runtime_process(&sink, late)
            .await
            .is_err()
    );
    f.profile = authority('a');
    let recovered = f
        .registry
        .recover_managed_runtime_receipt(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            c.command_id.clone(),
            digest(&c),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered.ledger_revision, qualified.ledger_revision);
    let cleanup = stop(&c);
    f.registry
        .fence_managed_runtime(proof(&f, cleanup.clone(), 2))
        .await
        .unwrap();
    let recorded = f
        .registry
        .collect_managed_runtime_stop(proof(&f, cleanup.clone(), 2))
        .await
        .unwrap();
    assert_eq!(recorded.state, "stopped");
    assert!(recorded.ledger_revision > stopped.ledger_revision);
    recorded.validate_for(&cleanup).unwrap();
    let replay = f
        .registry
        .recover_managed_runtime_receipt(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            cleanup.id().into(),
            digest(&cleanup),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(digest(&recorded), digest(&replay));
    let still_held = apply(&f.registry, &f.profile, release(&f, "after_observed_stop")).await;
    assert_eq!(still_held.state, "refused");
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_runtime_unresolved_start_consumes_original_custody_without_not_started_or_refund()
{
    let f = fixture().await;
    let c = setup(&f).await;
    let command = ManagedRuntimeCommand::Provision(c.clone());
    let ManagedRuntimeClaim::Issued(custody) = f
        .registry
        .claim_managed_runtime(proof(&f, command.clone(), 1))
        .await
        .unwrap()
    else {
        panic!("issued");
    };
    let first = f
        .registry
        .record_managed_runtime_unresolved(custody)
        .await
        .unwrap();
    assert_eq!(first.state, "unknown");
    assert_eq!(first.receipt.as_ref().unwrap().process_state, "unknown");
    assert!(matches!(
        f.registry
            .claim_managed_runtime(proof(&f, command, 1))
            .await
            .unwrap(),
        ManagedRuntimeClaim::History(_)
    ));
    let stop = stop(&c);
    f.registry
        .fence_managed_runtime(proof(&f, stop.clone(), 2))
        .await
        .unwrap();
    let cleaned = f
        .registry
        .collect_managed_runtime_stop(proof(&f, stop, 2))
        .await
        .unwrap();
    assert_eq!(cleaned.state, "unknown");
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "still_held"))
            .await
            .reason
            .as_deref(),
        Some("unsettled_usage")
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_runtime_preflight_is_read_only_and_never_reissues_history() {
    let f = fixture().await;
    let c = setup(&f).await;
    let p = ManagedRuntimeCommand::Provision(c.clone());
    let before: u64 = f
        .registry
        .0
        .store
        .call(1024, |db| {
            Ok(db
                .db
                .query_row("SELECT revision FROM owner_resource_identity", [], |r| {
                    r.get(0)
                })?)
        })
        .await
        .unwrap();
    let check = f
        .registry
        .prepare_managed_runtime_delivery(proof(&f, p.clone(), 1))
        .await
        .unwrap()
        .unwrap();
    assert!(
        f.registry
            .managed_runtime_history(clean(&f, &c), c.clone())
            .await
            .unwrap()
            .is_none()
    );
    let child = c.agent_reservation_id.clone();
    assert!(f.registry.0.store.call(1024, move |db| Ok(db.db.query_row("SELECT (SELECT revision FROM owner_resource_identity)=?1 AND (SELECT state FROM owner_managed_effects WHERE child_id=?2)='pending' AND NOT EXISTS(SELECT 1 FROM owner_managed_assignment_pins) AND NOT EXISTS(SELECT 1 FROM owner_managed_runtime_lifetimes)", params![before,child], |r| r.get::<_,bool>(0))?)).await.unwrap());
    let ManagedRuntimeClaim::Issued(custody) =
        f.registry.claim_managed_runtime(check).await.unwrap()
    else {
        panic!("first issuance");
    };
    drop(custody);
    assert!(
        f.registry
            .prepare_managed_runtime_delivery(proof(&f, p.clone(), 1))
            .await
            .unwrap()
            .is_none()
    );
    f.registry.shutdown().await.unwrap();
}
