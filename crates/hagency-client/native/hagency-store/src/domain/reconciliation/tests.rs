use super::*;
use crate::DomainStore;
use crate::management::{HostContext, Permission, Session};
use serde_json::json;
use std::time::{Duration, Instant};
use std::{future::Future, task::Poll};

fn profile() -> String {
    "a".repeat(64)
}
fn context() -> HostContext {
    HostContext::new(
        "@owner:example.test".into(),
        1,
        "b".repeat(64),
        1,
        "desktop_test".into(),
        1,
    )
    .unwrap()
}
fn session(store: &DomainStore, permissions: &[Permission]) -> Session {
    Session::new(
        store.clone(),
        context(),
        permissions.iter().copied(),
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
}
fn resource(preset: &str) -> Resource {
    serde_json::from_value(json!({
        "presetId":preset,"seatId":"original_local_seat","framework":"codex",
        "model":"gpt-5.6-sol","reasoning":"medium","published":false,
        "ceiling":{"tokens":5000,"period":"monthly"}
    }))
    .unwrap()
}
fn snapshot(db: &DomainRepository) -> (u64, serde_json::Value) {
    (
        db.db.total_changes(),
        db.coordinator_migration_inventory().unwrap(),
    )
}

#[test]
fn native_reconciliation_audit_preserves_wal_rows_schema_and_resource_identity() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("source");
    let mut db = DomainRepository::open_managed(&state, &profile()).unwrap();
    let original = resource("old_profile_preset");
    db.put_resource(&original).unwrap();
    let files = || {
        ["domain.sqlite3", "domain.sqlite3-wal"]
            .map(|name| std::fs::read(state.join(name)).unwrap())
    };
    let before_files = files();
    assert!(
        before_files[1].len() > 32,
        "committed frames must remain in WAL"
    );
    let before = snapshot(&db);
    let audit = db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(before, snapshot(&db));
    assert_eq!(before_files, files());
    assert_eq!(audit.history.resource_definitions.observed, 1);
    assert_eq!(audit.resources.len(), 1);
    assert_eq!(audit.resources[0].id, original.id());
    assert_eq!(
        audit.resources[0].revision,
        resource_publication_revision(&original).unwrap()
    );
    assert!(audit.resources_complete && audit.counts_complete);
    assert_eq!(audit.adoption, "not_assessed");
    assert_eq!(
        audit.blockers,
        [
            AuditBlocker::ProviderMappingNotEstablished,
            AuditBlocker::PersonalLedgerNotInspected,
            AuditBlocker::SourceCensusNotEstablished,
        ]
    );
    let json = serde_json::to_string(&audit).unwrap();
    assert!(!json.contains("original_local_seat"));
    assert!(!json.contains("old_profile_preset"));
    assert!(!json.contains(&profile()));
}

#[test]
fn native_reconciliation_audit_retains_unknown_history_and_private_custody() {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
    db.db.execute_batch(
        "CREATE TABLE private_unknown_history(value TEXT NOT NULL);
         INSERT INTO private_unknown_history VALUES('private credential locator and message');
         INSERT INTO management_commands VALUES('private actor','private command','resource.enroll','private digest','private receipt');
         INSERT INTO management_intents VALUES('actor','pending','ProjectRequest',printf('%064d',0),'private input',NULL,'pending');
         INSERT INTO management_intents VALUES('actor','replied','ProjectRequest',printf('%064d',0),'private input','private reply','replied');
         INSERT INTO management_intents VALUES('actor','acknowledged','ProjectRequest',printf('%064d',0),NULL,NULL,'acknowledged');"
    ).unwrap();
    let before = snapshot(&db);
    let audit = db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(snapshot(&db), before);
    assert_eq!(audit.history.command_receipts.observed, 1);
    assert_eq!(audit.history.pending_intents.observed, 1);
    assert_eq!(audit.history.replied_intents.observed, 1);
    assert_eq!(audit.history.acknowledged_intents.observed, 1);
    assert_eq!(audit.history.other_history_rows.observed, 1);
    assert_eq!(audit.history.other_populated_tables, 1);
    assert!(
        audit
            .blockers
            .contains(&AuditBlocker::PendingPrivateCommands)
    );
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    let public = serde_json::to_string(&audit).unwrap();
    for secret in [
        "private credential",
        "private input",
        "private receipt",
        "private_unknown_history",
        "private command",
    ] {
        assert!(!public.contains(secret));
    }
    // Even unusual SQLite names cannot escape quoting or leak their text.
    db.db.execute_batch("CREATE TABLE \"secret\"\"name\"(payload TEXT); INSERT INTO \"secret\"\"name\" VALUES('hidden');").unwrap();
    let second = db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(second.history.other_populated_tables, 2);
    assert!(!serde_json::to_string(&second).unwrap().contains("secret"));
}

#[test]
fn native_reconciliation_audit_bounds_observation_without_claiming_empty_history() {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
    db.db
        .execute_batch(
            "CREATE TABLE unresolved_history(id INTEGER PRIMARY KEY);
         WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<=10000)
         INSERT INTO unresolved_history SELECT i FROM n;",
        )
        .unwrap();
    for n in 0..=MAX_RESOURCES {
        db.put_resource(&resource(&format!("old_preset_{n}")))
            .unwrap();
    }
    let audit = db.profile_ledger_audit(&profile()).unwrap();
    assert!(!audit.counts_complete);
    assert!(!audit.resources_complete);
    assert_eq!(audit.resources.len(), MAX_RESOURCES);
    assert_eq!(audit.history.other_history_rows.observed, MAX_ROWS);
    assert!(audit.history.other_history_rows.capped);
    assert!(audit.blockers.contains(&AuditBlocker::ObservationLimit));
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert!(serde_json::to_vec(&audit).unwrap().len() < 16 * 1024);
}

#[test]
fn native_reconciliation_audit_does_not_recover_started_execution_or_drop_holds() {
    use crate::domain_worker::clock_fixtures;
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
    db.register(&clock_fixtures::registration()).unwrap();
    let source = clock_fixtures::resource("original", "legacy_seat", 1000);
    db.put_resource(&source).unwrap();
    let request = clock_fixtures::proof(&clock_fixtures::request(
        "original_request",
        "Worker",
        &source,
        700,
    ));
    db.admit(&request, 1000).unwrap();
    db.approve("original_decision", &request, 1000).unwrap();
    let effect = db.claim_effect().unwrap().unwrap();
    let before = snapshot(&db);
    let audit = db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(before, snapshot(&db));
    assert!(audit.history.capacity_and_usage_rows.observed >= 1);
    assert!(audit.history.execution_rows.observed >= 1);
    assert!(
        audit
            .blockers
            .contains(&AuditBlocker::CapacityOrUsageHistory)
    );
    assert!(audit.blockers.contains(&AuditBlocker::ExecutionHistory));
    let status: String = db
        .db
        .query_row("SELECT state FROM effects WHERE id=?1", [&effect.id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(status, "started");
}

#[test]
fn native_reconciliation_audit_rejects_invalid_schema_and_mismatched_resource_id() {
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
    db.put_resource(&resource("original")).unwrap();
    db.db
        .execute("UPDATE resources SET id='forged_resource'", [])
        .unwrap();
    assert!(matches!(
        db.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
    db.db
        .pragma_update(None, "user_version", crate::DOMAIN_SCHEMA_VERSION + 1)
        .unwrap();
    assert!(matches!(
        db.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
}

#[tokio::test]
async fn native_reconciliation_audit_requires_exact_profile_context_and_permission() {
    let root = tempfile::tempdir().unwrap();
    let db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
    let store = DomainStore::start(db, 8).unwrap();
    let allowed = session(&store, &[Permission::ReadResources]);
    assert!(
        allowed
            .reconciliation_audit(&context(), profile())
            .await
            .is_ok()
    );
    assert!(matches!(
        allowed
            .reconciliation_audit(&context(), "c".repeat(64))
            .await,
        Err(Error::LocalAuthority)
    ));
    assert!(matches!(
        allowed
            .reconciliation_audit(&context(), "invalid".into())
            .await,
        Err(Error::LocalAuthority)
    ));
    let denied = session(&store, &[Permission::ReadIntents]);
    assert!(matches!(
        denied.reconciliation_audit(&context(), profile()).await,
        Err(Error::LocalAuthority)
    ));
    let changed = HostContext::new(
        "@owner:example.test".into(),
        2,
        "b".repeat(64),
        1,
        "desktop_test".into(),
        1,
    )
    .unwrap();
    assert!(matches!(
        allowed.reconciliation_audit(&changed, profile()).await,
        Err(Error::LocalAuthority)
    ));
    allowed.retire();
    assert!(matches!(
        allowed.reconciliation_audit(&context(), profile()).await,
        Err(Error::LocalAuthority)
    ));
    store.shutdown().await.unwrap();
}

#[tokio::test]
async fn native_reconciliation_audit_rechecks_authority_after_queue_wait() {
    let root = tempfile::tempdir().unwrap();
    let db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
    let store = DomainStore::start(db, 8).unwrap();
    let allowed = session(&store, &[Permission::ReadResources]);
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let (resume, resume_rx) = std::sync::mpsc::channel();
    let blocker = {
        let store = store.clone();
        tokio::spawn(async move {
            store
                .call(1, move |_| {
                    entered.send(()).unwrap();
                    resume_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                    Ok(())
                })
                .await
        })
    };
    entered_rx.await.unwrap();
    let ctx = context();
    let mut queued = Box::pin(allowed.reconciliation_audit(&ctx, profile()));
    std::future::poll_fn(|cx| {
        assert!(matches!(queued.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    allowed.retire();
    resume.send(()).unwrap();
    blocker.await.unwrap().unwrap();
    assert!(matches!(queued.await, Err(Error::LocalAuthority)));
    store.shutdown().await.unwrap();
}

#[test]
fn native_readonly_domain_inspector_rejects_writable_connections_and_keeps_source_identity() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
    let resource = resource("retained");
    source.put_resource(&resource).unwrap();
    assert!(matches!(
        DomainRepository::audit_readonly_connection(&mut source.db, &profile()),
        Err(Error::LocalAuthority)
    ));
    let before = snapshot(&source);
    let mut observed = Connection::open_with_flags(
        path.join("domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let audit = DomainRepository::audit_readonly_connection(&mut observed, &profile()).unwrap();
    assert_eq!(audit.resources[0].id, resource.id());
    assert_eq!(before, snapshot(&source));
    assert!(matches!(
        DomainRepository::audit_readonly_connection(&mut observed, &"b".repeat(64)),
        Err(Error::LocalAuthority)
    ));
}

#[test]
fn native_readonly_audit_accepts_prior_source_without_migrating_optional_history() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
    source
        .put_resource(&resource("old-retained-resource"))
        .unwrap();
    source.db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE IF EXISTS owner_managed_runtime_observations;DROP TABLE IF EXISTS owner_managed_runtime_stops;DROP TABLE IF EXISTS owner_managed_runtime_lifetimes;DROP TABLE IF EXISTS owner_resource_adoptions; DROP TABLE owner_managed_fences; DROP TABLE owner_managed_observations; DROP TABLE owner_managed_call_pins; DROP TABLE owner_managed_assignment_pins; DROP TABLE owner_managed_calls; DROP TABLE owner_managed_effects; DROP TABLE owner_managed_agents; DROP TABLE owner_project_authorities; DROP TABLE owner_parent_settlements; DROP TABLE owner_project_reservations; DROP TABLE owner_capacity_receipts; DROP TABLE owner_capacity_grants; DROP TABLE owner_resource_grants; DROP TABLE owner_resource_accounts; DROP TABLE owner_resource_receipts; DROP TABLE owner_resource_identity; DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=71;").unwrap();
    let before = source.db.total_changes();
    let mut observed = Connection::open_with_flags(
        path.join("domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let evidence = DomainRepository::audit_readonly_connection(&mut observed, &profile()).unwrap();
    assert_eq!(evidence.schema_version, 71);
    assert_eq!(evidence.optional_history_tables_missing, 49);
    assert!(evidence.blockers.contains(&AuditBlocker::OlderSourceSchema));
    assert_eq!(evidence.resources.len(), 1);
    assert_eq!(source.db.total_changes(), before);
    let absent: bool = source
        .db
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='owner_capacity_grants')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(absent, "audit must not create later tables");
    let version: i32 = source
        .db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 71);
}

#[test]
fn native_readonly_audit_does_not_trust_a_version_without_required_columns_or_tables() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let source = DomainRepository::open_managed(&path, &profile()).unwrap();
    source
        .db
        .execute_batch("ALTER TABLE management_commands RENAME COLUMN result TO missing_result;")
        .unwrap();
    let mut observed = Connection::open_with_flags(
        path.join("domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert!(matches!(
        DomainRepository::audit_readonly_connection(&mut observed, &profile()),
        Err(Error::Schema)
    ));
    source.db.execute_batch("DROP TABLE management_commands; CREATE VIEW management_commands AS SELECT 'actor' actor_scope,'command' command_id,'operation' operation,'digest' digest,'result' result;").unwrap();
    assert!(matches!(
        DomainRepository::audit_readonly_connection(&mut observed, &profile()),
        Err(Error::Schema)
    ));
}

#[test]
fn native_schema77_audit_preserves_managed_custody_and_fence_history_as_import_blockers() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
    let resource = resource("managed-history-source");
    source
        .put_seat(&Seat {
            id: "original_local_seat".into(),
            declaration: None,
        })
        .unwrap();
    source.put_resource(&resource).unwrap();
    // Opaque retained records are intentionally not interpreted as fresh
    // authority. Even undecodable old receipts must remain blocking history.
    source.db.execute("INSERT INTO owner_capacity_grants(id,profile,resource_id,preset_id,seat_id,allocated_tokens,receipt,active) VALUES ('g',?1,?2,'p','original_local_seat',700,'retained-opaque-receipt',1)", params![profile(),resource.id()]).unwrap();
    source.db.execute("INSERT INTO owner_project_reservations(id,grant_id,project_id,project_revision,reserve_command_id,reserve_digest,requested_tokens,retained_tokens,state,binding_digest) VALUES ('allocation','g','project',1,'reserve',?1,400,400,'reserved',?1)", ["b".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_project_authorities VALUES ('allocation','{}',?1,'@manager:test',?1)", ["b".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_managed_agents(id,profile,agent_id,request_id,project_reservation_id,original_command_id,original_command_digest,original_command,definition_digest,allocated_tokens,budget_revision,state) VALUES ('child',?1,'agent','request','allocation','original',?2,'{}',?2,300,1,'reserved')",params![profile(),"c".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_managed_effects(id,child_id,kind,state,fence,payload_digest) VALUES ('effect','child','provision','unknown',1,?1)", ["d".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES ('call','child','execution',?1,1,'unknown',300,0,0)",["e".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_resource_accounts VALUES(?1,'original_local_seat','verified_provider')", ["f".repeat(64)]).unwrap();
    source
        .db
        .execute(
            "INSERT INTO owner_managed_assignment_pins VALUES('child','assignment',1,?1,?2)",
            params!["b".repeat(64), "f".repeat(64)],
        )
        .unwrap();
    source
        .db
        .execute(
            "INSERT INTO owner_managed_call_pins VALUES('call',?1,?2,1,2)",
            params!["b".repeat(64), "c".repeat(64)],
        )
        .unwrap();
    source.db.execute("INSERT INTO owner_managed_observations VALUES('observation','child','call','usage',?1,'{}','{}',3)",["d".repeat(64)]).unwrap();
    let prior_history = source
        .profile_ledger_audit(&profile())
        .unwrap()
        .history
        .other_history_rows
        .observed;
    source.db.execute("INSERT INTO owner_managed_fences(profile,agent_id,request_id,original_command_id,original_command_digest,original_command,project_reservation_id,executor_device_id,fence_revision) VALUES(?1,'absent-agent','absent-request','absent-original',?2,'{}','allocation','device',4)", params![profile(), "f".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_managed_runtime_lifetimes VALUES('child',?1,'assignment','runtime_original','{}',?2,?2,?3,5,'original_incarnation','unknown')",params![profile(),"a".repeat(64),"f".repeat(64)]).unwrap();
    source
        .db
        .execute(
            "INSERT INTO owner_managed_runtime_stops VALUES('child','runtime_stop','{}',?1,6)",
            ["a".repeat(64)],
        )
        .unwrap();
    source.db.execute("INSERT INTO owner_managed_runtime_observations VALUES('runtime_observation','child','runtime_original','{}',?1,7)",["a".repeat(64)]).unwrap();
    let before = source.db.total_changes();
    let mut observed = Connection::open_with_flags(
        path.join("domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let audit = DomainRepository::audit_readonly_connection(&mut observed, &profile()).unwrap();
    assert_eq!(audit.schema_version, crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.optional_history_tables_missing, 0);
    assert_eq!(audit.history.other_history_rows.observed, prior_history + 4);
    assert!(audit.history.other_history_rows.observed >= 12);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert!(
        audit
            .blockers
            .contains(&AuditBlocker::CapacityOrUsageHistory)
    );
    assert_eq!(source.db.total_changes(), before);
    let retained: (String,u64,u64) = source.db.query_row("SELECT state,reserved_tokens,known_consumed_tokens FROM owner_managed_calls WHERE id='call'",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(retained, ("unknown".into(), 300, 0));
    source.db.execute("INSERT INTO owner_managed_agents(id,profile,agent_id,request_id,project_reservation_id,original_command_id,original_command_digest,original_command,definition_digest,allocated_tokens,budget_revision,state) VALUES ('text_child',?1,'text_agent','text_request','allocation','text_original',?2,'{}',?2,100,1,'reserved')",params![profile(),"c".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_runtime_lifetimes VALUES('text_child',?1,'text_assignment','text_runtime','{}',?2,?2,?3,8,'text_incarnation')",params![profile(),"a".repeat(64),"f".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_process_observations VALUES('text_process','text_child','{}',?1,9)",["a".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_work_inbox VALUES('text_work','text_child','text_execution','{}',?1,10)",["a".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_work_events VALUES('text_event','text_work','start_requested','{}',?1,11)",["a".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_work_observations VALUES('text_result','text_work','{}',?1,12)",["a".repeat(64)]).unwrap();
    source
        .db
        .execute(
            "INSERT INTO owner_shared_activity_renewals VALUES('text_renewal','text_child',1,13)",
            [],
        )
        .unwrap();
    let after = DomainRepository::audit_readonly_connection(&mut observed, &profile()).unwrap();
    assert_eq!(
        after.history.other_history_rows.observed,
        audit.history.other_history_rows.observed + 7
    );
    assert!(after.blockers.contains(&AuditBlocker::OtherHistory));
    source.db.execute("INSERT INTO owner_shared_successor_attempts VALUES('attempt_two','text_child',?1,2,'text_runtime',?2,?2,'new_start','{}',?2,?2,?2,?3,13,14,'second_incarnation')",params![profile(),"a".repeat(64),"f".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_successor_observations VALUES('attempt_observation','attempt_two','{}',?1,15)",["a".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_successor_call_pins VALUES('call','attempt_two','text_work',?1)",["a".repeat(64)]).unwrap();
    source.db.execute("INSERT INTO owner_shared_successor_fences VALUES('attempt_two','text_child',?1,2,'text_runtime','new_start','{}',?2,16,14)",params![profile(),"a".repeat(64)]).unwrap();
    let with_attempt =
        DomainRepository::audit_readonly_connection(&mut observed, &profile()).unwrap();
    assert_eq!(
        with_attempt.history.other_history_rows.observed,
        after.history.other_history_rows.observed + 4
    );
    assert!(with_attempt.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(with_attempt.optional_history_tables_missing, 0);
    source.db.execute("INSERT INTO owner_shared_start_intents(id,child_id,profile,device_id,device_generation,predecessor_digest,original_runtime_digest,original_runtime_payload,predecessor_payload,input_payload,input_digest,ledger_revision) VALUES('accepted_start','text_child',?1,'device',1,?2,?2,'{}',NULL,'{}',?2,17)",params![profile(),"a".repeat(64)]).unwrap();
    let with_intent =
        DomainRepository::audit_readonly_connection(&mut observed, &profile()).unwrap();
    assert_eq!(
        with_intent.history.other_history_rows.observed,
        with_attempt.history.other_history_rows.observed + 1
    );
    assert!(with_intent.blockers.contains(&AuditBlocker::OtherHistory));
}

#[test]
fn native_schema81_audit_requires_real_successor_tables_and_columns() {
    for view in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let source = DomainRepository::open_managed(&path, &profile()).unwrap();
        if view {
            source.db.execute_batch("DROP TABLE owner_shared_successor_call_pins;CREATE VIEW owner_shared_successor_call_pins AS SELECT 'a' call_id,'b' attempt_id,'c' work_id,'d' execution_digest;").unwrap();
        } else {
            source.db.execute_batch("ALTER TABLE owner_shared_successor_attempts RENAME COLUMN predecessor_stop_digest TO missing_stop_digest;").unwrap();
        }
        let mut observed = Connection::open_with_flags(
            path.join("domain.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert!(matches!(
            DomainRepository::audit_readonly_connection(&mut observed, &profile()),
            Err(Error::Schema)
        ));
    }
}

#[test]
fn native_schema77_audit_requires_the_reviewed_fence_shape() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let source = DomainRepository::open_managed(&path, &profile()).unwrap();
    source.db.execute_batch("ALTER TABLE owner_managed_fences RENAME COLUMN original_command_digest TO missing_digest;").unwrap();
    let mut observed = Connection::open_with_flags(
        path.join("domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert!(matches!(
        DomainRepository::audit_readonly_connection(&mut observed, &profile()),
        Err(Error::Schema)
    ));
    source
        .db
        .execute_batch("DROP TABLE owner_managed_fences;")
        .unwrap();
    assert!(matches!(
        DomainRepository::audit_readonly_connection(&mut observed, &profile()),
        Err(Error::Schema)
    ));
}

#[test]
fn native_schema78_adoption_custody_is_history_and_missing_shape_is_not_empty() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
    source.db.execute("INSERT INTO owner_resource_adoptions(id,profile,plan_digest,plan,phase) VALUES('retained_transfer',?1,?2,'opaque-original-plan',0)",params![profile(),"c".repeat(64)]).unwrap();
    let before = source.db.total_changes();
    let audit = source.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version, crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.other_history_rows.observed, 1);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(source.db.total_changes(), before);
    source
        .db
        .execute_batch(
            "ALTER TABLE owner_resource_adoptions RENAME COLUMN plan_digest TO missing_digest;",
        )
        .unwrap();
    assert!(matches!(
        source.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
    source
        .db
        .execute_batch("DROP TABLE owner_resource_adoptions;")
        .unwrap();
    assert!(matches!(
        source.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
}

#[test]
fn native_schema79_audit_requires_reviewed_runtime_shape() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
    source.db.execute_batch("ALTER TABLE owner_managed_runtime_lifetimes RENAME COLUMN source_effect_state TO missing_state;").unwrap();
    assert!(matches!(
        source.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
    source.db.execute_batch("DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE owner_managed_runtime_observations;DROP TABLE owner_managed_runtime_stops;DROP TABLE owner_managed_runtime_lifetimes;").unwrap();
    assert!(matches!(
        source.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
}

#[test]
fn native_schema80_audit_rejects_missing_or_substituted_shared_work_shape() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source");
    let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
    source
        .db
        .execute_batch(
            "ALTER TABLE owner_shared_work_inbox RENAME COLUMN digest TO missing_digest;",
        )
        .unwrap();
    assert!(matches!(
        source.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
    source.db.execute_batch("DROP TABLE owner_shared_work_observations;DROP TABLE owner_shared_work_events;DROP TABLE owner_shared_work_inbox;CREATE VIEW owner_shared_work_inbox AS SELECT NULL AS id;").unwrap();
    assert!(matches!(
        source.profile_ledger_audit(&profile()),
        Err(Error::Schema)
    ));
}

#[test]
fn native_schema82_audit_requires_ordinary_start_intent_and_exact_columns() {
    for view in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let source = DomainRepository::open_managed(&path, &profile()).unwrap();
        if view {
            source.db.execute_batch("DROP TABLE owner_shared_start_intents;CREATE VIEW owner_shared_start_intents AS SELECT 1 id;").unwrap();
        } else {
            source.db.execute_batch("ALTER TABLE owner_shared_start_intents RENAME COLUMN input_digest TO missing_digest;").unwrap();
        }
        let mut observed = Connection::open_with_flags(
            path.join("domain.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert!(matches!(
            DomainRepository::audit_readonly_connection(&mut observed, &profile()),
            Err(Error::Schema)
        ));
    }
}

#[test]
fn native_schema83_audit_requires_ordinary_local_names_and_exact_columns() {
    for view in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let source = DomainRepository::open_managed(&path, &profile()).unwrap();
        if view {
            source.db.execute_batch("DROP TABLE owner_resource_display_names;CREATE VIEW owner_resource_display_names AS SELECT 1 profile;").unwrap();
        } else {
            source.db.execute_batch("ALTER TABLE owner_resource_display_names RENAME COLUMN revision TO missing_revision;").unwrap();
        }
        let mut observed = Connection::open_with_flags(
            path.join("domain.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert!(matches!(
            DomainRepository::audit_readonly_connection(&mut observed, &profile()),
            Err(Error::Schema)
        ));
    }
}

#[test]
fn canonical_operator_tasks_schema84_audit_requires_ordinary_tables_and_exact_columns() {
    for (table, column) in [
        ("owner_operator_task_histories", "receipt_digest"),
        ("owner_operator_tasks", "revision"),
        ("owner_operator_task_comments", "ordinal"),
    ] {
        for view in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("source");
            let source = DomainRepository::open_managed(&path, &profile()).unwrap();
            if view {
                source.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;DROP TABLE {table};CREATE VIEW {table} AS SELECT 1 profile;")).unwrap();
            } else {
                source
                    .db
                    .execute_batch(&format!(
                        "ALTER TABLE {table} RENAME COLUMN {column} TO missing_column;"
                    ))
                    .unwrap();
            }
            let mut observed = Connection::open_with_flags(
                path.join("domain.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            assert!(matches!(
                DomainRepository::audit_readonly_connection(&mut observed, &profile()),
                Err(Error::Schema)
            ));
        }
    }
}

#[test]
fn shared_demand_schema85_audit_requires_exact_ordinary_shapes() {
    for (table, column) in [
        ("owner_shared_demand_controls", "stopped_through"),
        ("owner_shared_demands", "offer_digest"),
        ("owner_shared_demand_decisions", "input_digest"),
        ("owner_shared_demand_bindings", "binding_digest"),
    ] {
        for view in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("source");
            let source = DomainRepository::open_managed(&path, &profile()).unwrap();
            if view {
                source.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;DROP TABLE {table};CREATE VIEW {table} AS SELECT 1 profile;")).unwrap();
            } else {
                source
                    .db
                    .execute_batch(&format!(
                        "ALTER TABLE {table} RENAME COLUMN {column} TO missing_column;"
                    ))
                    .unwrap();
            }
            let mut observed = Connection::open_with_flags(
                path.join("domain.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            assert!(matches!(
                DomainRepository::audit_readonly_connection(&mut observed, &profile()),
                Err(Error::Schema)
            ));
        }
    }
}

#[test]
fn shared_demand_schema85_all_retained_tables_are_audit_blockers() {
    for sql in [
        "INSERT INTO owner_shared_demand_controls VALUES('child',?1,1,0)",
        "INSERT INTO owner_shared_demands VALUES('demand','child',?1,'device',1,'$event','{}',?1,1)",
        "INSERT INTO owner_shared_demand_decisions VALUES('demand','budget_blocked',NULL,NULL,NULL,'{}',1)",
        "INSERT INTO owner_shared_demand_bindings VALUES('demand','work','{}',?1,1)",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
        // Deliberately opaque retained custody must not be interpreted as empty.
        source.db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        if sql.contains("?1") {
            source.db.execute(sql, [profile()]).unwrap();
        } else {
            source.db.execute(sql, []).unwrap();
        }
        let before = source.db.total_changes();
        let audit = source.profile_ledger_audit(&profile()).unwrap();
        assert_eq!(audit.schema_version, crate::DOMAIN_SCHEMA_VERSION);
        assert_eq!(audit.history.other_history_rows.observed, 1);
        assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
        assert_eq!(source.db.total_changes(), before);
    }
}

#[test]
fn shared_demand_stop_schema86_requires_exact_ordinary_shapes() {
    for (table, column) in [
        ("owner_shared_stop_intents", "selected_revision"),
        ("owner_shared_stop_receipts", "applied_revision"),
    ] {
        for view in [true, false] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("source");
            let source = DomainRepository::open_managed(&path, &profile()).unwrap();
            if view {
                source.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;DROP TABLE {table};CREATE VIEW {table} AS SELECT 1 profile;")).unwrap();
            } else {
                source
                    .db
                    .execute_batch(&format!(
                        "ALTER TABLE {table} RENAME COLUMN {column} TO missing_column;"
                    ))
                    .unwrap();
            }
            let mut observed = Connection::open_with_flags(
                path.join("domain.sqlite3"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            assert!(matches!(
                DomainRepository::audit_readonly_connection(&mut observed, &profile()),
                Err(Error::Schema)
            ));
        }
    }
}
#[test]
fn shared_demand_stop_schema86_retained_tables_block_audit() {
    for sql in [
        "INSERT INTO owner_shared_stop_intents VALUES('stop','child',?1,'device',1,'@owner:example.test','assignment',1,'{}',?1,1)",
        "INSERT INTO owner_shared_stop_receipts VALUES('stop',2,1)",
    ] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source");
        let mut source = DomainRepository::open_managed(&path, &profile()).unwrap();
        source.db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        if sql.contains("?1") {
            source.db.execute(sql, [profile()]).unwrap();
        } else {
            source.db.execute(sql, []).unwrap();
        }
        let before = source.db.total_changes();
        let audit = source.profile_ledger_audit(&profile()).unwrap();
        assert_eq!(audit.schema_version, crate::DOMAIN_SCHEMA_VERSION);
        assert_eq!(audit.history.other_history_rows.observed, 1);
        assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
        assert_eq!(source.db.total_changes(), before);
    }
}

#[test]
fn shared_context_schema87_history_and_missing_shape_fail_closed(){
 for sql in ["INSERT INTO owner_shared_input_modes VALUES('work','thread_context_v1',1)","INSERT INTO owner_shared_reply_deliveries VALUES('work','{}',printf('%064d',0),1)"] {
  let root=tempfile::tempdir().unwrap();let path=root.path().join("source");let mut source=DomainRepository::open_managed(&path,&profile()).unwrap();source.db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();source.db.execute_batch(sql).unwrap();let before=source.db.total_changes();let audit=source.profile_ledger_audit(&profile()).unwrap();assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));assert_eq!(source.db.total_changes(),before);
 }
 for table in ["owner_shared_input_modes","owner_shared_contexts","owner_shared_reply_deliveries","owner_shared_context_call_pins"]{
  let root=tempfile::tempdir().unwrap();let path=root.path().join("source");let mut source=DomainRepository::open_managed(&path,&profile()).unwrap();source.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;DROP TABLE {table};")).unwrap();assert!(source.profile_ledger_audit(&profile()).is_err());
 }
}

#[test]
fn managed_budget_schema89_history_and_exact_shape_are_never_ignored() {
    let root=tempfile::tempdir().unwrap();
    let path=root.path().join("source");
    let mut source=DomainRepository::open_managed(&path,&profile()).unwrap();
    source.db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    source.db.execute("INSERT INTO owner_managed_budget_commands VALUES('budget','child',?1,'device','topup',?1,'{}','{}',?1,1)",[profile()]).unwrap();
    let before=source.db.total_changes();
    let audit=source.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.other_history_rows.observed,1);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(source.db.total_changes(),before);
    for table in ["owner_managed_budget_commands","owner_managed_budget_deltas"] {
        let root=tempfile::tempdir().unwrap();
        let path=root.path().join("source");
        let mut source=DomainRepository::open_managed(&path,&profile()).unwrap();
        source.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;DROP TABLE {table};")).unwrap();
        assert!(source.profile_ledger_audit(&profile()).is_err());
    }
}

#[test]
fn operator_task_transfer_schema88_requires_exact_shape_and_retains_history() {
    for table in ["owner_operator_task_transfer_tasks", "owner_operator_task_transfer_comments"] {
        let root = tempfile::tempdir().unwrap();
        let mut source = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
        source.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;DROP TABLE {table};CREATE VIEW {table} AS SELECT 'fake' AS migration_id;")).unwrap();
        assert!(source.profile_ledger_audit(&profile()).is_err());
    }
    for sql in [
        "INSERT INTO owner_operator_task_transfer_tasks VALUES('original','task',printf('%064d',0),'{}')",
        "INSERT INTO owner_operator_task_transfer_comments VALUES('original',1,'task',0,printf('%064d',0),'{}')",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut source = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
        source.db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        source.db.execute_batch(sql).unwrap();
        let before = source.db.total_changes();
        let audit = source.profile_ledger_audit(&profile()).unwrap();
        assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
        assert_eq!(audit.history.other_history_rows.observed, 1);
        assert_eq!(source.db.total_changes(), before);
    }
}

#[test]
fn shared_budget_schema90_requires_exact_shape_and_retains_original_journals() {
    for table in ["owner_shared_budget_start_intents", "owner_shared_budget_work_refusals", "owner_shared_budget_demand_decisions"] {
        let root=tempfile::tempdir().unwrap();
        let mut source=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        source.profile_ledger_audit(&profile()).unwrap();
        source.db.execute_batch(&format!("PRAGMA foreign_keys=OFF;DROP TABLE {table};")).unwrap();
        assert!(source.profile_ledger_audit(&profile()).is_err());
    }
    for sql in [
        "INSERT INTO owner_shared_budget_start_intents VALUES('start','child',?1,'device',1,?1,?1,'{}','{}',?1,1)",
        "INSERT INTO owner_shared_budget_work_refusals VALUES('work','call',?1,'{}',?1,'observation',1)",
        "INSERT INTO owner_shared_budget_demand_decisions VALUES('demand',?1,'{}','budget_blocked',NULL,'{}',?1,1)",
    ] {
        let root=tempfile::tempdir().unwrap();
        let mut source=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        source.db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        source.db.execute(sql,[profile()]).unwrap();
        let before=source.db.total_changes();
        let audit=source.profile_ledger_audit(&profile()).unwrap();
        assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);
        assert_eq!(audit.history.other_history_rows.observed,1);
        assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
        assert_eq!(source.db.total_changes(),before);
    }
}

#[test]
fn account_preparation_schema91_requires_exact_shape_and_keeps_ownership_history() {
    for malformed in [
        "DROP TABLE management_account_profiles;",
        "ALTER TABLE management_account_profiles RENAME COLUMN profile TO missing_profile;",
        "DROP TABLE management_account_profiles;CREATE VIEW management_account_profiles AS SELECT 'fake' account_id,'fake' profile;",
    ] {
        let root=tempfile::tempdir().unwrap();
        let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        db.profile_ledger_audit(&profile()).unwrap();db.db.execute_batch(malformed).unwrap();
        assert!(db.profile_ledger_audit(&profile()).is_err());
    }
    let root=tempfile::tempdir().unwrap();
    let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
    let prepared=db.reserve_account(crate::ACCOUNT_PROFILE).unwrap();
    db.db.execute("INSERT INTO management_account_profiles VALUES(?1,?2)",params![prepared.id,profile()]).unwrap();
    let before=db.db.total_changes();let audit=db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert!(audit.history.other_history_rows.observed>=1);assert_eq!(db.db.total_changes(),before);
}

#[test]
fn namespace_schema92_requires_real_tables_and_current_columns() {
    for malformed in [
        "DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE owner_namespace_enrollments;",
        "DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE owner_namespace_enrollments; CREATE VIEW owner_namespace_enrollments AS SELECT 1 AS command_id;",
        "DROP TABLE owner_resource_namespaces; CREATE TABLE owner_resource_namespaces(resource_id TEXT);",
    ] {
        let root=tempfile::tempdir().unwrap();let path=root.path().join("source");
        let mut db=DomainRepository::open_managed(&path,&profile()).unwrap();
        assert_eq!(db.profile_ledger_audit(&profile()).unwrap().schema_version,crate::DOMAIN_SCHEMA_VERSION);
        db.db.execute_batch(malformed).unwrap();
        assert!(db.profile_ledger_audit(&profile()).is_err());
    }
}

#[test]
fn provenance_schema93_requires_real_table_and_original_evidence_columns() {
    for malformed in [
        "DROP TABLE owner_resource_provenance;",
        "DROP TABLE owner_resource_provenance; CREATE VIEW owner_resource_provenance AS SELECT 'fake' resource_id;",
        "ALTER TABLE owner_resource_provenance RENAME COLUMN first_offer_command_id TO missing_original_command;",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
        let before = db.db.total_changes();
        assert_eq!(db.profile_ledger_audit(&profile()).unwrap().schema_version, crate::DOMAIN_SCHEMA_VERSION);
        assert_eq!(db.db.total_changes(), before, "inventory is read only");
        db.db.execute_batch(malformed).unwrap();
        assert!(db.profile_ledger_audit(&profile()).is_err());
    }
}

#[test]
fn approval_counter_schema94_requires_the_real_table_and_retained_generation() {
    for malformed in [
        "DROP TABLE approval_binding_incarnations;",
        "DROP TABLE approval_binding_incarnations; CREATE VIEW approval_binding_incarnations AS SELECT 'fake' engagement_id, 1 incarnation;",
        "ALTER TABLE approval_binding_incarnations RENAME COLUMN incarnation TO missing_generation;",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
        let before = db.db.total_changes();
        assert_eq!(db.profile_ledger_audit(&profile()).unwrap().schema_version, crate::DOMAIN_SCHEMA_VERSION);
        assert_eq!(db.db.total_changes(), before);
        db.db.execute_batch(malformed).unwrap();
        assert!(db.profile_ledger_audit(&profile()).is_err());
    }
}

#[test]
fn execution_policy_schema95_audit_keeps_snapshots_and_requires_their_table() {
    for malformed in [
        "DROP TABLE owned_dispatch_execution_policies;",
        "ALTER TABLE owned_dispatch_execution_policies RENAME COLUMN yolo TO missing_policy;",
        "DROP TABLE owned_dispatch_execution_policies;CREATE VIEW owned_dispatch_execution_policies AS SELECT 'fake' dispatch_id,'fake' engagement_id,0 yolo,0 captured_at;",
    ] {
        let root=tempfile::tempdir().unwrap();
        let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        db.profile_ledger_audit(&profile()).unwrap();
        db.db.execute_batch(malformed).unwrap();
        assert!(matches!(db.profile_ledger_audit(&profile()),Err(Error::Schema)));
    }
    let root=tempfile::tempdir().unwrap();
    let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
    db.db.execute_batch("PRAGMA foreign_keys=OFF;INSERT INTO owned_dispatch_execution_policies VALUES('dispatch','agent',1,1000);").unwrap();
    let before=db.db.total_changes();
    let audit=db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.other_history_rows.observed,1);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(db.db.total_changes(),before);
}

#[test]
fn graph_archive_schema96_requires_both_original_tables_and_never_calls_history_empty() {
    for malformed in [
        "DROP TABLE owner_operator_graph_archive_chunks;",
        "ALTER TABLE owner_operator_graph_archives RENAME COLUMN source_binding TO missing_source;",
        "DROP TABLE owner_operator_graph_archive_chunks;CREATE VIEW owner_operator_graph_archive_chunks AS SELECT 'fake' command_id,0 document,0 chunk_index,X'41' bytes;",
    ] {
        let root=tempfile::tempdir().unwrap();
        let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        db.profile_ledger_audit(&profile()).unwrap();
        db.db.execute_batch(malformed).unwrap();
        assert!(matches!(db.profile_ledger_audit(&profile()),Err(Error::Schema)));
    }
    let root=tempfile::tempdir().unwrap();
    let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
    db.db.execute("INSERT INTO owner_operator_graph_archives VALUES('archive',?1,?2,'@owner:example.test',?3,'{}','copying')",params!["a".repeat(64),profile(),"b".repeat(64)]).unwrap();
    db.db.execute_batch("INSERT INTO owner_operator_graph_archive_chunks VALUES('archive',0,0,X'41');").unwrap();
    let before=db.db.total_changes();
    let audit=db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.other_history_rows.observed,2);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(db.db.total_changes(),before);
}

#[test]
fn graph_journal_schema97_requires_complete_tables_and_counts_retained_history() {
    for malformed in [
        "DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE owner_operator_graph_outbox;",
        "ALTER TABLE owner_operator_graphs RENAME COLUMN document_digest TO missing_digest;",
        "DROP TABLE owner_operator_graph_commands;CREATE VIEW owner_operator_graph_commands AS SELECT 'fake' command_id;",
    ] {
        let root=tempfile::tempdir().unwrap();
        let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        db.profile_ledger_audit(&profile()).unwrap();
        db.db.execute_batch(malformed).unwrap();
        assert!(matches!(db.profile_ledger_audit(&profile()),Err(Error::Schema)));
    }
    let root=tempfile::tempdir().unwrap();
    let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
    db.db.execute("INSERT INTO owner_operator_graphs VALUES(?1,?2,'@owner:example.test','scope','graph',1,X'7b7d',?3)",params!["a".repeat(64),profile(),"b".repeat(64)]).unwrap();
    let before=db.db.total_changes();
    let audit=db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.other_history_rows.observed,1);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(db.db.total_changes(),before);
}

#[test]
fn ceiling_amendment_schema105_requires_exact_table_and_counts_capacity_history() {
    for malformed in [
        "DROP TRIGGER owner_resource_amended_ceiling_retained;DROP TABLE owner_resource_ceiling_amendments;",
        "ALTER TABLE owner_resource_ceiling_amendments RENAME COLUMN base_revision TO missing_revision;",
        "DROP TRIGGER owner_resource_amended_ceiling_retained;DROP TABLE owner_resource_ceiling_amendments;CREATE VIEW owner_resource_ceiling_amendments AS SELECT 'fake' resource_id;",
    ] {
        let root=tempfile::tempdir().unwrap();
        let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        db.profile_ledger_audit(&profile()).unwrap();
        db.db.execute_batch(malformed).unwrap();
        assert!(matches!(db.profile_ledger_audit(&profile()),Err(Error::Schema)),"{malformed}");
    }
    let root=tempfile::tempdir().unwrap();
    let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
    let resource:Resource=serde_json::from_value(json!({"presetId":"audited","seatId":"audited_seat","framework":"codex","model":"gpt-5.6-sol","reasoning":"medium","ceiling":{"tokens":1000,"period":"monthly"}})).unwrap();
    db.put_resource(&resource).unwrap();
    db.db.execute("INSERT INTO owner_resource_ceiling_amendments VALUES(?1,1,?2,'audited_amendment',?3,1000,1500,'monthly',1)",params![resource.id(),profile(),resource_publication_revision(&resource).unwrap()]).unwrap();
    let before=db.db.total_changes();
    let audit=db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.capacity_and_usage_rows.observed,1);
    assert!(audit.blockers.contains(&AuditBlocker::CapacityOrUsageHistory));
    // The audit reports the definition's revision, which the amendment never changed.
    assert_eq!(audit.resources[0].revision,resource_publication_revision(&resource).unwrap());
    assert_eq!(db.db.total_changes(),before);
}

#[test]
fn graph_recipient_schema98_requires_exact_table_and_counts_review_as_retained_history() {
    for malformed in [
        "DROP TABLE owner_operator_graph_recipients;",
        "ALTER TABLE owner_operator_graph_recipients RENAME COLUMN snapshot_digest TO missing_digest;",
        "DROP TABLE owner_operator_graph_recipients;CREATE VIEW owner_operator_graph_recipients AS SELECT 'fake' node_id;",
    ] {
        let root=tempfile::tempdir().unwrap();
        let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
        db.profile_ledger_audit(&profile()).unwrap();
        db.db.execute_batch(malformed).unwrap();
        assert!(matches!(db.profile_ledger_audit(&profile()),Err(Error::Schema)));
    }
    let root=tempfile::tempdir().unwrap();
    let mut db=DomainRepository::open_managed(&root.path().join("source"),&profile()).unwrap();
    db.db.execute("INSERT INTO owner_operator_graphs VALUES(?1,?2,'@owner:example.test','scope','graph',1,X'7b7d',?3)",params!["a".repeat(64),profile(),"b".repeat(64)]).unwrap();
    db.db.execute("INSERT INTO owner_operator_graph_recipients VALUES(?1,?2,'@owner:example.test','scope','graph','node',2,X'7b7d',?3)",params!["a".repeat(64),profile(),hagency_core::project::hash(b"{}")]).unwrap();
    db.db.execute("UPDATE owner_operator_graphs SET revision=2",[]).unwrap();
    let before=db.db.total_changes();
    let audit=db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version,crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.other_history_rows.observed,2);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(db.db.total_changes(),before);
}

#[test]
fn native_session_schema111_remains_visible_to_readonly_audit() {
    for malformed in [
        "DROP TABLE owner_shared_native_checkpoints;",
        "ALTER TABLE owner_shared_native_turns RENAME COLUMN scope_digest TO missing_scope;",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
        db.profile_ledger_audit(&profile()).unwrap();
        db.db.execute_batch(malformed).unwrap();
        assert!(matches!(db.profile_ledger_audit(&profile()), Err(Error::Schema)));
    }
    let root = tempfile::tempdir().unwrap();
    let mut db = DomainRepository::open_managed(&root.path().join("source"), &profile()).unwrap();
    // Inventory must retain history even if its parent cannot be reconstructed.
    db.db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    db.db.execute("INSERT INTO owner_shared_native_turns VALUES('work',?1,?2,1)",params!["a".repeat(64),"b".repeat(64)]).unwrap();
    let before = db.db.total_changes();
    let audit = db.profile_ledger_audit(&profile()).unwrap();
    assert_eq!(audit.schema_version, crate::DOMAIN_SCHEMA_VERSION);
    assert_eq!(audit.history.other_history_rows.observed, 1);
    assert!(audit.blockers.contains(&AuditBlocker::OtherHistory));
    assert_eq!(db.db.total_changes(), before);
}
