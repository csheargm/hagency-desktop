//! A current empty installation can observe old central history, but cannot
//! manufacture local process/usage evidence or bypass any live authority gate.
use super::*;
use crate::daemon::workers::{self, Admission, Cursors, PassIo, Step, StepFuture};
use crate::native_owner::{CurrentProfileSelection, CurrentSharedExecutor};

struct Successors(CurrentSharedExecutor);
impl PassIo for Successors {
    fn enabled(&self, step: Step) -> bool {
        step == Step::SuccessorPoll
    }
    fn run(&self, step: Step, after: Option<String>) -> StepFuture<'_> {
        // Only select the step here. Use the production daemon mapping and
        // actual current executor/HTTP/SQLite path for the outcome and cursor.
        PassIo::run(&self.0, step, after)
    }
}

async fn executor(f: &Fixture, live: Arc<AtomicBool>) -> Successors {
    let selection = CurrentProfileSelection::load_for_isolated_tests(
        &f._root.path().join("host"),
        &f.owner.0.expected_origin,
        "@owner:test",
        f.registry.clone(),
    )
    .unwrap()
    .unwrap();
    Successors(
        selection
            .shared_executor(
                &f.owner,
                package(81),
                Arc::new(move || live.load(Ordering::Acquire)),
            )
            .await
            .unwrap(),
    )
}

async fn original(pool: bool) -> (SharedRuntimeCommand, NativeGrantReceipt) {
    let (f, t, c) = if pool {
        use hagency_core::recurring_quota::{Cadence, Calendar, RenewalMode};
        let f = Fixture::with_provider_budget_calendar(
            "openai.chatgpt",
            "fixture_account",
            1000,
            700,
            60000,
            Some(Calendar {
                cadence: Cadence::Daily,
                time_zone: "America/Los_Angeles".into(),
            }),
        )
        .await;
        let g = f.state.lock().unwrap().grant.clone().unwrap();
        let model = hagency_core::qualification::ModelProfile {
            framework: "codex".into(),
            model: "gpt-5.6-sol".into(),
            provider: None,
            reasoning: Some("medium".into()),
        };
        let role = hagency_core::qualification::roles()
            .find(|r| hagency_core::qualification::qualifies(&model, r, None))
            .unwrap();
        let definition = ManagedAgentDefinition {
            name: "Historical helper".into(),
            capability: role.into(),
            description: "No runtime effects".into(),
            tool_mode: "none".into(),
        };
        let c = ManagedAgentReserveCommand {
            version: 2,
            renewal_mode: Some(RenewalMode::Automatic),
            allocation_generation: None,
            pool_grant: Some(g.clone()),
            capability: "hagency.pool_agents.v1".into(),
            operation: "agent.reserve".into(),
            command_id: "old_pool_reserve".into(),
            request_id: "old_pool_request".into(),
            request_revision: 2,
            proposed_agent_id: "old_pool_agent".into(),
            manager_mxid: "@manager:test".into(),
            project_id: String::new(),
            project_revision: 0,
            setup_id: String::new(),
            setup_binding_revision: 0,
            domain_project_id: String::new(),
            room_id: "!discussion:test".into(),
            authority_revision: g.grant_revision,
            decision_actor_mxid: g.owner_mxid.clone(),
            decision_digest: "d".repeat(64),
            allocation_id: "old_pool_request".into(),
            allocation_receipt_digest: String::new(),
            grant_receipt_digest: digest(&g).unwrap(),
            enrollment_id: g.enrollment_id.clone(),
            executor_device_id: g.executor_device_id.clone(),
            executor_generation: g.executor_generation,
            resource_id: g.resource_id.clone(),
            resource_revision: g.resource_revision.clone(),
            definition_digest: digest(&definition).unwrap(),
            definition,
            requested_tokens: 100,
            period_key: g.period_key.clone(),
            issued_at_ms: now(),
            expires_at_ms: now() + 60000,
        };
        c.validate("@owner:test").unwrap();
        f.state.lock().unwrap().agent = Some(serde_json::to_value(&c).unwrap());
        let t = ManagedAgentTransport::new(f.owner.management(package(1)).await.unwrap());
        runtime_fixture(f, t, c).await
    } else {
        fixture().await
    };
    let grant = f.state.lock().unwrap().grant.clone().unwrap();
    assert_eq!(f.state.lock().unwrap().shared.starts, 0);
    drop(t);
    f.stop().await;
    (c, grant)
}

fn successor(c: &SharedRuntimeCommand, id: &str) -> SharedAttemptCommand {
    let digest = digest(c).unwrap();
    let command = SharedAttemptCommand {
        web_search: false,
        tool_policy: None,
        version: 2,
        capability: SHARED_ATTEMPT_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: id.into(),
        assignment_id: c.assignment_id.clone(),
        assignment_generation: 1,
        agent_reservation_id: c.agent_reservation_id.clone(),
        process_attempt: 2,
        original_runtime_command: c.clone(),
        original_runtime_digest: digest.clone(),
        predecessor: SharedAttemptPredecessor {
            command_id: c.command_id.clone(),
            command_digest: digest,
            process_attempt: 1,
            stopped_receipt_digest: "b".repeat(64),
            process_digest: "a".repeat(64),
        },
        budget_observation: SharedBudgetObservation {
            observed_ledger_revision: 1,
            allocated_tokens: c.reserve_command.requested_tokens,
            known_consumed_tokens: 0,
            retained_tokens: 0,
            remaining_tokens: c.reserve_command.requested_tokens,
            usage_complete: true,
            liability_digest: "c".repeat(64),
        },
        start_intent_id: format!("start_{id}"),
        issued_at_ms: c.issued_at_ms,
        expires_at_ms: c.issued_at_ms + 30000,
    };
    command.validate("@owner:test").unwrap();
    command
}

fn stopped_receipt(c: &SharedAttemptCommand) -> SharedAttemptProcessReceipt {
    let receipt = SharedAttemptProcessReceipt {
        version: 2,
        observation_id: format!("obs_{}", c.command_id),
        command_id: c.command_id.clone(),
        command_digest: digest(c).unwrap(),
        assignment_id: c.assignment_id.clone(),
        agent_reservation_id: c.agent_reservation_id.clone(),
        process_attempt: c.process_attempt,
        original_runtime_digest: c.original_runtime_digest.clone(),
        ledger_revision: 1,
        process_digest: Some("e".repeat(64)),
        state: "stopped".into(),
        policy_digest: None,
        reason: None,
    };
    receipt.validate_for(c).unwrap();
    receipt
}

fn serve(
    f: &Fixture,
    grant: &NativeGrantReceipt,
    commands: Vec<SharedAttemptCommand>,
    recorded: bool,
) {
    let mut s = f.state.lock().unwrap();
    s.grant = Some(grant.clone());
    if recorded {
        let receipt = stopped_receipt(&commands[0]);
        s.shared
            .attempts
            .processes
            .insert(receipt.observation_id.clone(), receipt);
    }
    s.shared.attempts.command = Some(commands[0].clone());
    s.shared.attempts.historical_poll = Some(commands);
}

fn database(f: &Fixture) -> rusqlite::Connection {
    rusqlite::Connection::open_with_flags(
        f._root.path().join("registry/domain.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}
fn version(db: &rusqlite::Connection) -> u64 {
    db.query_row("PRAGMA data_version", [], |r| r.get(0))
        .unwrap()
}
fn snapshot(db: &rusqlite::Connection) -> BTreeMap<String, (usize, String)> {
    let tables = db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .unwrap().query_map([], |r| r.get::<_, String>(0)).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut query = db
                .prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\"")))
                .unwrap();
            let columns = query.column_count();
            let mut rows = query.query([]).unwrap();
            let mut count = 0;
            let mut hash = Sha256::new();
            while let Some(row) = rows.next().unwrap() {
                count += 1;
                for i in 0..columns {
                    hash.update(format!("{:?};", row.get_ref(i).unwrap()));
                }
            }
            (table, (count, format!("{:x}", hash.finalize())))
        })
        .collect()
}
fn remote_evidence(f: &Fixture) -> String {
    digest(&f.state.lock().unwrap().shared.attempts.processes).unwrap()
}
fn no_effects(
    f: &Fixture,
    db: &rusqlite::Connection,
    before: &(u64, BTreeMap<String, (usize, String)>),
    remote: &str,
) {
    assert_eq!(
        version(db),
        before.0,
        "no SQLite commit, even a compensating write"
    );
    assert_eq!(
        snapshot(db),
        before.1,
        "all current table contents remain exact"
    );
    assert_eq!(
        remote_evidence(f),
        remote,
        "central unknown/stopped evidence is not replaced"
    );
    let s = f.state.lock().unwrap();
    assert_eq!(
        s.shared.starts + s.shared.work_writes + s.shared.process_writes,
        0
    );
    assert!(s.shared.receipts.is_empty());
    assert!(s.shared.attempts.start_inputs.is_empty());
    assert!(
        s.shared
            .attempts
            .requests
            .iter()
            .all(|p| p.ends_with("/poll") || p.ends_with("/recover"))
    );
    assert_eq!(
        s.agent_writes + s.result_writes + s.publication_writes + s.runtime_writes,
        0
    );
    assert!(s.parent_receipt.is_none());
}

#[tokio::test]
async fn native_shared_successor_missing_history_advances_current_worker_without_effects() {
    for pool in [false, true] {
        let (original, grant) = original(pool).await;
        for recorded in [false, true] {
            let f = Fixture::empty_current().await;
            let first = successor(&original, "old_attempt_a");
            let next = successor(&original, "old_attempt_b");
            serve(&f, &grant, vec![first.clone(), next.clone()], recorded);
            let executor = executor(&f, Arc::new(AtomicBool::new(true))).await;
            let db = database(&f);
            let before = (version(&db), snapshot(&db));
            let remote = remote_evidence(&f);
            for table in [
                "resources",
                "owner_capacity_grants",
                "owner_project_reservations",
                "owner_managed_agents",
                "owner_managed_calls",
                "owner_shared_successor_attempts",
            ] {
                assert_eq!(before.1[table].0, 0, "{table}");
            }
            let mut cursors = Cursors::default();
            let admission = Admission::default();
            for command in [&first, &next] {
                let report = workers::pass(&executor, &mut cursors, || true, &admission).await;
                assert_eq!((report.ran, report.failed), (1, 0));
                assert_eq!(
                    cursors.get(Step::SuccessorPoll).as_deref(),
                    Some(command.command_id.as_str())
                );
                assert_eq!(
                    cursors.status(Step::SuccessorPoll),
                    Some(workers::HISTORICAL_EVIDENCE_MISSING)
                );
            }
            let report = workers::pass(&executor, &mut cursors, || true, &admission).await;
            assert_eq!(report.failed, 0);
            assert_eq!(
                cursors.get(Step::SuccessorPoll),
                Some(first.command_id.clone())
            );
            assert_eq!(
                cursors.status(Step::SuccessorPoll),
                Some(workers::HISTORICAL_EVIDENCE_MISSING),
                "the original unknown evidence remains discoverable after wrap"
            );
            assert_eq!(
                f.state.lock().unwrap().shared.attempts.poll_after,
                vec![None, Some(first.command_id), Some(next.command_id)]
            );
            no_effects(&f, &db, &before, &remote);
            drop(db);
            drop(executor);
            f.stop().await;
        }
    }
}

#[tokio::test]
async fn native_shared_successor_missing_history_never_masks_authority_or_cleanup() {
    let (original, grant) = original(true).await;
    for case in [
        "server401",
        "owner",
        "owner_after_poll",
        "host",
        "host_after_poll",
        "manager",
        "device",
        "digest",
        "expired",
        "recorded",
        "cleanup",
        "execute",
        "owner_at_contribution",
        "generation_at_contribution",
    ] {
        let f = Fixture::empty_current().await;
        let mut command = successor(&original, "old_attempt");
        if case == "device" {
            let reserve = &mut command.original_runtime_command.reserve_command;
            reserve.executor_device_id = "different_device".into();
            reserve.pool_grant.as_mut().unwrap().executor_device_id = "different_device".into();
            reserve.grant_receipt_digest = digest(reserve.pool_grant.as_ref().unwrap()).unwrap();
            command.original_runtime_command.reserve_command_digest = digest(reserve).unwrap();
            command.original_runtime_digest = digest(&command.original_runtime_command).unwrap();
            command.predecessor.command_digest = command.original_runtime_digest.clone();
            command.validate("@owner:test").unwrap();
        }
        serve(&f, &grant, vec![command], case == "recorded");
        let gate = Arc::new(AtomicBool::new(true));
        let executor = executor(&f, gate.clone()).await;
        let db = database(&f);
        let before = (version(&db), snapshot(&db));
        {
            let mut s = f.state.lock().unwrap();
            match case {
                "server401" => s.shared.attempts.refused_poll = true,
                "owner" => f.current.store(false, Ordering::Release),
                "owner_after_poll" => s.shared.attempts.retire_poll_gate = Some(f.current.clone()),
                "host" => gate.store(false, Ordering::Release),
                "host_after_poll" => s.shared.attempts.retire_poll_gate = Some(gate.clone()),
                "manager" => executor.0.retire(),
                "digest" => s.tamper_digest = true,
                "expired" => s.expired_ticket = true,
                "recorded" => {
                    s.shared
                        .attempts
                        .processes
                        .values_mut()
                        .next()
                        .unwrap()
                        .assignment_id = "swapped_assignment".into()
                }
                "cleanup" => s.shared.cleanup = true,
                "execute" => s.historical_executable = true,
                "owner_at_contribution" => s.invalidate_at_contribution = true,
                "generation_at_contribution" => s.generation = 2,
                "device" => {}
                _ => unreachable!(),
            }
        }
        let remote = remote_evidence(&f);
        let mut cursors = Cursors::default();
        let report = workers::pass(&executor, &mut cursors, || true, &Admission::default()).await;
        assert_eq!(report.failed, 1, "{case}: {report:?}");
        assert_eq!(cursors.get(Step::SuccessorPoll), None, "{case}");
        assert_ne!(
            cursors.status(Step::SuccessorPoll),
            Some(workers::HISTORICAL_EVIDENCE_MISSING),
            "{case}"
        );
        no_effects(&f, &db, &before, &remote);
        drop(db);
        drop(executor);
        f.stop().await;
    }
}

#[tokio::test]
async fn native_shared_successor_missing_history_recovery_validates_exact_receipt() {
    let (original, grant) = original(true).await;
    let f = Fixture::empty_current().await;
    let c = successor(&original, "old_attempt");
    serve(&f, &grant, vec![c.clone()], true);
    let t = SharedExecutionTransport::new(f.owner.management(package(1)).await.unwrap());
    let db = database(&f);
    let before = (version(&db), snapshot(&db));
    let remote = remote_evidence(&f);
    // An absent exact observation is unknown, never a reconstructed receipt.
    assert!(
        matches!(t.recover_successor(&f.registry, c.command_id.clone(), digest(&c).unwrap(), Some("absent_observation".into())).await.unwrap(),
        SharedExecutionProgress::HistoricalEvidenceMissing { command_id } if command_id == c.command_id)
    );
    // A substituted recorded event must still fail before the missing-custody
    // early return. The actual receipt can then be observed without importing it.
    f.state
        .lock()
        .unwrap()
        .shared
        .attempts
        .processes
        .values_mut()
        .next()
        .unwrap()
        .command_digest = "f".repeat(64);
    assert!(
        t.recover_successor(&f.registry, c.command_id.clone(), digest(&c).unwrap(), None)
            .await
            .is_err()
    );
    f.state
        .lock()
        .unwrap()
        .shared
        .attempts
        .processes
        .values_mut()
        .next()
        .unwrap()
        .command_digest = digest(&c).unwrap();
    assert!(
        matches!(t.recover_successor(&f.registry, c.command_id.clone(), digest(&c).unwrap(), Some(stopped_receipt(&c).observation_id)).await.unwrap(),
        SharedExecutionProgress::HistoricalEvidenceMissing { command_id } if command_id == c.command_id)
    );
    no_effects(&f, &db, &before, &remote);
    drop(db);
    drop(t);
    f.stop().await;
}

#[tokio::test]
async fn native_shared_successor_existing_history_still_checks_profile_and_command() {
    let (f, t, c) = fixture().await;
    let scope = f
        .transport
        .scope(Permission::ExecuteManagedAgents)
        .await
        .unwrap();
    let reserve = &c.reserve_command;
    let found = f
        .registry
        .find_managed_agent_project_history(
            scope.profile.clone(),
            "device".into(),
            reserve.allocation_id.clone(),
            reserve.allocation_receipt_digest.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    let enrollment = f
        .transport
        .enrollment(&scope, &found.grant_receipt, false)
        .await
        .unwrap();
    let db = database(&f);
    let before = (version(&db), snapshot(&db));
    assert!(
        f.registry
            .find_shared_cleanup_authority(enrollment, c.clone())
            .await
            .unwrap()
            .is_some()
    );
    let enrollment = f
        .transport
        .enrollment(&scope, &found.grant_receipt, false)
        .await
        .unwrap();
    let mut changed = c.clone();
    changed.reserve_command.definition.description = "Substituted original".into();
    changed.reserve_command.definition_digest =
        digest(&changed.reserve_command.definition).unwrap();
    changed.reserve_command_digest = digest(&changed.reserve_command).unwrap();
    changed.validate("@owner:test").unwrap();
    assert!(matches!(
        f.registry
            .find_shared_cleanup_authority(enrollment, changed)
            .await,
        Err(hagency_store::Error::Conflict)
    ));
    let wrong_profile = hagency_store::owner_resources::ProfileAuthority::new(
        "f".repeat(64),
        "@owner:test".into(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    assert!(matches!(
        f.registry
            .find_managed_agent_project_history(
                wrong_profile,
                "device".into(),
                reserve.allocation_id.clone(),
                reserve.allocation_receipt_digest.clone(),
            )
            .await,
        Err(hagency_store::Error::LocalAuthority)
    ));
    assert!(matches!(
        f.registry
            .find_managed_agent_project_history(
                scope.profile.clone(),
                "device".into(),
                reserve.allocation_id.clone(),
                "f".repeat(64),
            )
            .await,
        Err(hagency_store::Error::Conflict)
    ));
    scope.profile.retire();
    assert!(matches!(
        f.registry
            .find_managed_agent_project_history(
                scope.profile,
                "device".into(),
                "absent_allocation".into(),
                "f".repeat(64),
            )
            .await,
        Err(hagency_store::Error::LocalAuthority)
    ));
    assert_eq!(version(&db), before.0);
    assert_eq!(snapshot(&db), before.1);
    drop(db);
    drop(t);
    f.stop().await;
}

#[tokio::test]
async fn native_shared_successor_partial_project_history_is_not_absence() {
    let (f, t, c) = fixture().await;
    let scope = f
        .transport
        .scope(Permission::ExecuteManagedAgents)
        .await
        .unwrap();
    let reserve = &c.reserve_command;
    let db = rusqlite::Connection::open(f._root.path().join("registry/domain.sqlite3")).unwrap();
    // Fault injection in an isolated real schema: the retained authority is
    // normally undeletable. A broken JOIN must not look like an empty install.
    assert!(
        db.execute(
            "DELETE FROM owner_project_authorities WHERE allocation_id=?1",
            [&reserve.allocation_id]
        )
        .is_err()
    );
    db.execute_batch("DROP TRIGGER owner_project_authority_retained")
        .unwrap();
    assert_eq!(
        db.execute(
            "DELETE FROM owner_project_authorities WHERE allocation_id=?1",
            [&reserve.allocation_id]
        )
        .unwrap(),
        1
    );
    let before = (version(&db), snapshot(&db));
    assert_eq!(before.1["owner_project_reservations"].0, 1);
    assert!(matches!(
        f.registry
            .find_managed_agent_project_history(
                scope.profile,
                "device".into(),
                reserve.allocation_id.clone(),
                reserve.allocation_receipt_digest.clone(),
            )
            .await,
        Err(hagency_store::Error::LocalAuthority)
    ));
    assert_eq!(version(&db), before.0);
    assert_eq!(snapshot(&db), before.1);
    drop(db);
    drop(t);
    f.stop().await;
}
