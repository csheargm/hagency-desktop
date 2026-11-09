use super::*;
use shared_execution::*;
#[path = "shared_process_custody_tests.rs"]
mod process_custody;
#[path = "shared_successor_tests.rs"]
mod successor;
fn digest<T: Serialize>(v: &T) -> String {
    canonical::digest(&serde_json::to_value(v).unwrap()).unwrap()
}
async fn command(f: &Fixture) -> SharedRuntimeCommand {
    let reserve_command = agent(f, "shared", 150);
    let saved = reserve(f, reserve_command.clone()).await;
    let now = graphs::now_ms().unwrap();
    SharedRuntimeCommand {
        version: 1,
        capability: SHARED_EXECUTION_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "shared_original".into(),
        assignment_id: "shared_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: saved.agent_reservation_id.clone().unwrap(),
        reserve_command_digest: digest(&reserve_command),
        reserve_receipt_digest: digest(&saved),
        reserve_command,
        policy: SHARED_TEXT_POLICY.into(),
        issued_at_ms: now,
        expires_at_ms: now + 30_000,
    }
}
fn proof(f: &Fixture, c: SharedRuntimeCommand) -> VerifiedSharedRuntimeDelivery {
    VerifiedSharedRuntimeDelivery::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
fn metadata(c: &SharedRuntimeCommand) -> ManagedRuntimeProvisionCommand {
    ManagedRuntimeProvisionCommand {
        version: 1,
        capability: AGENT_RUNTIME_CAPABILITY.into(),
        operation: "agent.runtime.provision".into(),
        command_id: "metadata_original".into(),
        assignment_id: c.assignment_id.clone(),
        assignment_generation: 1,
        agent_reservation_id: c.agent_reservation_id.clone(),
        reserve_command: c.reserve_command.clone(),
        reserve_command_digest: c.reserve_command_digest.clone(),
        reserve_receipt_digest: c.reserve_receipt_digest.clone(),
        policy: "codex_no_tools_v1".into(),
        issued_at_ms: c.issued_at_ms,
        expires_at_ms: c.expires_at_ms,
    }
}
fn metadata_proof(
    f: &Fixture,
    c: ManagedRuntimeProvisionCommand,
) -> VerifiedManagedRuntimeDelivery {
    let c = ManagedRuntimeCommand::Provision(c);
    VerifiedManagedRuntimeDelivery::new(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap()
}
#[tokio::test]
async fn shared_execution_claim_is_one_shot_across_concurrency_and_restart() {
    let mut f = fixture().await;
    let c = command(&f).await;
    let (a, b) = tokio::join!(
        f.registry.claim_shared_runtime(proof(&f, c.clone())),
        f.registry.claim_shared_runtime(proof(&f, c.clone()))
    );
    let mut issued = 0;
    let mut history = 0;
    for result in [a.unwrap(), b.unwrap()] {
        match result {
            SharedRuntimeClaim::Issued(c) => {
                assert_eq!(c.provider_digest().len(), 64);
                assert!(!c.incarnation().is_empty());
                c.check().unwrap();
                issued += 1
            }
            SharedRuntimeClaim::History(_) => history += 1,
        }
    }
    assert_eq!((issued, history), (1, 1));
    assert!(
        f.registry
            .claim_managed_runtime(metadata_proof(&f, metadata(&c)))
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    assert!(matches!(
        f.registry
            .claim_shared_runtime(proof(&f, c.clone()))
            .await
            .unwrap(),
        SharedRuntimeClaim::History(_)
    ));
    let mut other = c.clone();
    other.command_id = "replacement".into();
    assert!(matches!(
        f.registry.claim_shared_runtime(proof(&f, other)).await,
        Err(Error::Conflict)
    ));
    let release = apply(&f.registry, &f.profile, release(&f, "release_issued")).await;
    assert_eq!(release.reason.as_deref(), Some("unsettled_usage"));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_execution_excludes_old_metadata_and_never_accepts_recovery_authority() {
    let f = fixture().await;
    let c = command(&f).await;
    let old = metadata(&c);
    f.registry
        .claim_managed_runtime(metadata_proof(&f, old))
        .await
        .unwrap();
    assert!(
        f.registry
            .claim_shared_runtime(proof(&f, c.clone()))
            .await
            .is_err()
    );
    assert!(
        VerifiedSharedRuntimeDelivery::new(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            c.clone(),
            digest(&c),
            false,
            Instant::now() + Duration::from_secs(20)
        )
        .is_err()
    );
    assert!(
        VerifiedSharedRuntimeDelivery::new(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 2),
            c.clone(),
            digest(&c),
            true,
            Instant::now() + Duration::from_secs(20)
        )
        .is_err()
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_execution_claim_failure_rolls_back_effect_and_assignment() {
    let f = fixture().await;
    let c = command(&f).await;
    f.registry.0.store.call(1024,|db|{db.db.execute_batch("CREATE TRIGGER reject_shared BEFORE INSERT ON owner_shared_runtime_lifetimes BEGIN SELECT RAISE(ABORT,'fixture');END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .claim_shared_runtime(proof(&f, c.clone()))
            .await
            .is_err()
    );
    let child = c.agent_reservation_id;
    assert!(f.registry.0.store.call(1024,move|db|Ok(db.db.query_row("SELECT (SELECT state FROM owner_managed_effects WHERE child_id=?1)='pending' AND NOT EXISTS(SELECT 1 FROM owner_shared_runtime_lifetimes) AND NOT EXISTS(SELECT 1 FROM owner_managed_assignment_pins)",[child],|r|r.get::<_,bool>(0))?)).await.unwrap());
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn shared_execution_schema80_upgrade_is_atomic_and_retains_old_history() {
    let mut f = fixture().await;
    let c = command(&f).await;
    let old = metadata(&c);
    f.registry
        .claim_managed_runtime(metadata_proof(&f, old.clone()))
        .await
        .unwrap();
    let path = f._temp.path().join("owner/domain.sqlite3");
    f.registry.shutdown().await.unwrap();
    let mut db = Connection::open(&path).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE owner_shared_activity_renewals;DROP TABLE owner_shared_work_observations;DROP TABLE owner_shared_work_events;DROP TABLE owner_shared_work_inbox;DROP TABLE owner_shared_process_observations;DROP TABLE owner_shared_runtime_lifetimes;DROP TRIGGER owner_metadata_excludes_shared;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=79;CREATE TABLE owner_shared_work_inbox(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).is_err());
    db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<i32, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        79
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='owner_shared_runtime_lifetimes'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TABLE owner_shared_work_inbox;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    assert!(matches!(
        f.registry
            .claim_managed_runtime(metadata_proof(&f, old))
            .await
            .unwrap(),
        ManagedRuntimeClaim::History(_)
    ));
    assert!(f.registry.claim_shared_runtime(proof(&f, c)).await.is_err());
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn shared_execution_actual_process_qualifies_holds_once_and_keeps_live_cleanup_uncertain() {
    run_actual_capture("text", false).await;
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn shared_execution_actual_usage_charges_overrun_without_double_cache_read() {
    run_actual_capture("text-usage", false).await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised runtime probe"]
async fn shared_execution_native_tree_totals_deduplicate_and_missing_child_keeps_hold() {
    for mode in ["text-tree-measured", "text-tree-missing", "text-tree-eof"] {
        run_actual_capture(mode, false).await;
    }
}
#[tokio::test]
#[ignore = "requires HAGENCY_TEST_RUNTIME_PROBE built from the supervised offline runtime fixture"]
async fn shared_execution_actual_escaped_output_keeps_usage_when_body_exceeds_wire_bound() {
    run_actual_capture("text-escaped", false).await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn shared_execution_rejects_actual_process_turn_with_different_input() {
    run_actual_capture("text-usage", true).await;
}
#[tokio::test]
#[ignore = "requires explicitly built offline supervised probe"]
async fn shared_execution_activity_clamps_shorter_wall_bound_without_extending_process() {
    run_actual_capture_bound("text-usage", false, true).await;
}
async fn run_actual_capture(mode: &str, mismatch: bool) {
    run_actual_capture_bound(mode, mismatch, false).await;
}
async fn run_actual_capture_bound(mode: &str, mismatch: bool, shorter_wall: bool) {
    use hagency_runtime::{
        codex::{session::Settings, transport::Limits},
        owned::{
            MANAGED_DISABLED_FEATURES, OwnedSession, SHARED_TEXT_INSTRUCTIONS,
            SHARED_TEXT_INSTRUCTIONS_FILE,
        },
    };
    let binary = std::path::PathBuf::from(
        std::env::var_os("HAGENCY_TEST_RUNTIME_PROBE").expect("supervised fixture"),
    );
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
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
            "text_provider".into(),
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
    let project = super::command(&grant, "project", 400);
    let receipt = apply(&registry, &profile, project.clone()).await;
    let f = Fixture {
        _temp: temp,
        registry,
        profile,
        resource,
        grant,
        project,
        receipt,
    };
    let c = command(&f).await;
    let SharedRuntimeClaim::Issued(custody) = f
        .registry
        .claim_shared_runtime(proof(&f, c.clone()))
        .await
        .unwrap()
    else {
        panic!("first custody")
    };
    let root = f._temp.path().join("process");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let instructions = root.join(SHARED_TEXT_INSTRUCTIONS_FILE);
    std::fs::write(&instructions, SHARED_TEXT_INSTRUCTIONS).unwrap();
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
        arguments: vec![mode.into(), root.as_os_str().into()],
        directory: root.clone(),
        environment: std::collections::BTreeMap::new(),
        require_crash_containment: false,
    };
    let settings = Settings::new(root, "fixture".into(), "medium".into())
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
    let sink = Arc::new(
        (*custody)
            .bind_process(process.shared_text_process_source())
            .unwrap(),
    );
    let qualified = sink
        .record(process.qualify_shared_text(&instructions).await.unwrap())
        .await
        .unwrap();
    assert_eq!(qualified.state, "qualified");
    let expires = Instant::now() + Duration::from_secs(9);
    let maximum = Instant::now() + Duration::from_secs(30);
    process
        .install_shared_text_activity(1, expires, maximum)
        .unwrap();
    let lease = SharedActivityLease {
        authority: SharedActivityRef {
            runtime_command_id: c.command_id.clone(),
            epoch: 1,
            token: "a".repeat(64),
        },
        command_digest: digest(&c),
        process_digest: sink.process_digest().unwrap(),
        expires_at_ms: graphs::now_ms().unwrap() + if shorter_wall { 8_998 } else { 10_000 },
    };
    if shorter_wall {
        let mut expired = lease.clone();
        expired.expires_at_ms = graphs::now_ms().unwrap().saturating_sub(1);
        assert!(
            sink.authorize_activity(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                &expired,
                expires,
            )
            .is_err()
        );
        assert!(
            sink.authorize_activity(
                enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
                &lease,
                Instant::now(),
            )
            .is_err()
        );
    }
    let active = || {
        sink.authorize_activity(
            enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
            &lease,
            expires,
        )
        .unwrap()
    };
    let body = "Offline project question".to_owned();
    use sha2::{Digest, Sha256};
    let work = SharedWorkItem {
        room_binding: None, encrypted: false,
        id: "shared_work".into(),
        assignment_id: c.assignment_id.clone(),
        runtime_command_digest: digest(&c),
        event_id: "$event".into(),
        room_id: c.reserve_command.room_id.clone(),
        requester_mxid: c.reserve_command.manager_mxid.clone(),
        thread_root: "$event".into(),
        body_digest: format!("{:x}", Sha256::digest(body.as_bytes())),
        body,
        execution_id: "shared_execution".into(),
        created_at_ms: graphs::now_ms().unwrap(),
    };
    assert!(
        f.registry
            .prepare_shared_call(active(), work.clone())
            .await
            .is_err()
    );
    f.registry
        .receive_shared_work(active(), work.clone())
        .await
        .unwrap();
    f.registry
        .receive_shared_work(active(), work.clone())
        .await
        .unwrap();
    let first = f
        .registry
        .prepare_shared_call(active(), work.clone())
        .await
        .unwrap();
    let again = f
        .registry
        .prepare_shared_call(active(), work.clone())
        .await
        .unwrap();
    assert_eq!(first.call_id, again.call_id);
    assert_eq!(first.reserved_tokens, 0);
    let mut wrong = work.clone();
    wrong.body.push('!');
    assert!(
        f.registry
            .receive_shared_work(active(), wrong)
            .await
            .is_err()
    );
    let attempt = f
        .registry
        .begin_shared_start(active(), work.clone(), first.call_id.clone())
        .await
        .unwrap();
    assert!(matches!(
        f.registry
            .begin_shared_start(active(), work.clone(), first.call_id.clone())
            .await,
        Err(Error::OutcomeUnknown)
    ));
    let call = f
        .registry
        .issue_shared_call(
            attempt,
            SharedWorkStart {
                work: work.clone(),
                work_digest: digest(&work),
                activity_epoch: 1,
                newly_started: true,
            },
        )
        .await
        .unwrap();
    call.check().unwrap();
    process.start_shared_text_thread().await.unwrap();
    call.check().unwrap();
    process
        .start_shared_text_turn(if mismatch {
            "Different unbound fixture text".into()
        } else {
            work.body.clone()
        })
        .await
        .unwrap();
    if mismatch {
        assert!(matches!(
            call.bind_turn(&process),
            Err(Error::LocalAuthority)
        ));
        sink.record(process.stop_shared_text_observation())
            .await
            .unwrap();
        assert_eq!(
            f.registry
                .0
                .store
                .call(1024, |db| Ok(db.db.query_row(
                    "SELECT COUNT(*) FROM owner_shared_work_observations",
                    [],
                    |r| r.get::<_, u64>(0)
                )?))
                .await
                .unwrap(),
            0
        );
        f.registry.shutdown().await.unwrap();
        return;
    }
    let mut capture = call.bind_turn(&process).unwrap();
    let final_receipt = loop {
        let event = process.next_shared_text_event().await.unwrap();
        let terminal = matches!(
            event.observation().kind(),
            hagency_runtime::codex::session::ObservationKind::TurnEnded(_)
        );
        let receipt = capture.observe(event).await.unwrap();
        if terminal {
            break receipt.unwrap();
        }
    };
    if mode == "text" {
        assert_eq!(final_receipt.state, "completed");
        assert_eq!(final_receipt.body.as_deref(), Some("Offline answer."));
        assert_eq!(final_receipt.known_consumed_tokens, 0);
        assert_eq!(final_receipt.retained_tokens, Some(0));
        assert!(
            !final_receipt.usage_complete,
            "terminal alone cannot invent usage"
        );
    } else if mode.starts_with("text-tree-") {
        assert_eq!(final_receipt.state, "completed");
        assert_eq!(final_receipt.body.as_deref(), Some("Offline answer."));
        let measured = mode == "text-tree-measured";
        assert_eq!(final_receipt.known_consumed_tokens, if measured {28} else {14});
        assert_eq!(final_receipt.retained_tokens, Some(if measured {28} else {14}));
        assert_eq!(final_receipt.usage_complete, measured);
    } else {
        assert_eq!(final_receipt.state, "completed");
        if mode == "text-escaped" {
            assert!(final_receipt.body.as_deref().unwrap().ends_with("[答复超过单条消息长度限制，以上内容已截断。]"));
        } else {
            assert_eq!(final_receipt.body.as_deref(), Some("Offline answer."));
        }
        assert_eq!(final_receipt.known_consumed_tokens, 200);
        assert_eq!(final_receipt.retained_tokens, Some(200));
        assert!(final_receipt.usage_complete);
        assert!(serde_json::to_vec(&final_receipt).unwrap().len() <= 48 * 1024);
    }
    assert!(
        process
            .start_shared_text_turn(work.body.clone())
            .await
            .is_err()
    );
    let release = apply(&f.registry, &f.profile, release(&f, "live_text_hold")).await;
    assert_eq!(release.reason.as_deref(), Some("unsettled_usage"));
    f.profile.retire();
    let stopped = sink
        .record(process.stop_shared_text_observation())
        .await
        .unwrap();
    assert_eq!(stopped.state, "stopped");
    assert!(stopped.ledger_revision > qualified.ledger_revision);
    f.registry.shutdown().await.unwrap();
}

#[path="shared_context_journal_tests.rs"]
mod context_journal;

#[test]
fn shared_execution_long_reply_migration_preserves_referenced_history_and_constraints() {
    let mut db=rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON;CREATE TABLE owner_shared_work_inbox(id TEXT PRIMARY KEY);INSERT INTO owner_shared_work_inbox VALUES('work');").unwrap();
    let old=include_str!("../../migrations/082-owner-shared-execution.sql");
    let table=old.split("CREATE TABLE owner_shared_work_observations (").nth(1).unwrap().split("CREATE TABLE owner_shared_activity_renewals").next().unwrap();
    db.execute_batch(&("CREATE TABLE owner_shared_work_observations (".to_owned()+table)).unwrap();
    db.execute_batch("CREATE TABLE retained_reference(observation_id TEXT REFERENCES owner_shared_work_observations(id));INSERT INTO owner_shared_work_observations VALUES('old','work','{\"body\":\"original\"}',printf('%064d',0),1);INSERT INTO retained_reference VALUES('old');").unwrap();
    let original:(String,String)=db.query_row("SELECT payload,digest FROM owner_shared_work_observations WHERE id='old'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    let migration=include_str!("../../migrations/105-owner-shared-long-replies.sql");
    // DDL rollback must restore the old bound without losing referenced bytes.
    {let tx=db.transaction().unwrap();tx.execute_batch(migration).unwrap();tx.rollback().unwrap();}
    assert!(db.execute("INSERT INTO owner_shared_work_observations VALUES('large','work',?1,printf('%064d',0),2)",[serde_json::json!({"body":"x".repeat(32000)}).to_string()]).is_err());
    {let tx=db.transaction().unwrap();tx.execute_batch(migration).unwrap();tx.commit().unwrap();}
    let after:(String,String)=db.query_row("SELECT payload,digest FROM owner_shared_work_observations WHERE id='old'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
    assert_eq!(after,original);
    assert_eq!(db.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check",[],|r|r.get::<_,i64>(0)).unwrap(),0);
    db.execute("INSERT INTO owner_shared_work_observations VALUES('large','work',?1,printf('%064d',0),2)",[serde_json::json!({"body":"x".repeat(32000)}).to_string()]).unwrap();
    assert!(db.execute("INSERT INTO owner_shared_work_observations VALUES('oversized','work',?1,printf('%064d',0),3)",[serde_json::json!({"body":"x".repeat(50000)}).to_string()]).is_err());
    assert!(db.execute("UPDATE owner_shared_work_observations SET payload=payload",[]).is_err());
    assert!(db.execute("DELETE FROM owner_shared_work_observations WHERE id='large'",[]).is_err());
}
