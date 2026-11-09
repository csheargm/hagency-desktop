use super::*;

struct PoolFixture {
    _temp: tempfile::TempDir,
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    grant: NativeGrantReceipt,
}

#[tokio::test]
async fn pool_agent_inherits_owner_permissions_and_network_once_without_overwriting_overrides() {
    let f = pool_fixture().await;
    let mut first = None;
    for (n, yolo, web) in [(1,true,true), (2,false,false), (3,true,false)] {
        let profile = f.profile.digest.clone(); let resource = f.grant.resource_id.clone();
        f.registry.0.store.call(4096,move|db|{
            db.db.execute("INSERT INTO owner_web_catalog_settings(profile,kind,entity,value) VALUES(?1,'resource_default',?2,?3) ON CONFLICT(profile,kind,entity) DO UPDATE SET value=excluded.value",params![profile,resource,serde_json::json!({"yolo":yolo,"webSearch":web}).to_string()])?;
            Ok(())
        }).await.unwrap();
        let command = pool_command(&f, &format!("policy_{n}"), 100);
        let saved = pool_reserve(&f, command.clone()).await;
        let child = saved.agent_reservation_id.clone().unwrap();
        let profile = f.profile.digest.clone(); let agent=command.proposed_agent_id.clone();
        f.registry.0.store.call(4096,move|db|{
            let actual:bool=db.db.query_row("SELECT yolo FROM owner_shared_tool_policies WHERE child_id=?1",[&child],|r|r.get(0))?;
            assert_eq!(actual,yolo);
            assert_eq!(crate::domain::exec_policy::shared_web::read(&db.db,&child,&profile,&agent)?.enabled,web);
            Ok(())
        }).await.unwrap();
        if n == 1 { first=Some((command,saved)); }
    }
    let (first_command, first_receipt)=first.unwrap();
    let child=first_receipt.agent_reservation_id.clone().unwrap();
    let profile=f.profile.digest.clone(); let agent=first_command.proposed_agent_id.clone();
    f.registry.0.store.call(4096,move|db|{
        db.db.execute("UPDATE owner_shared_tool_policies SET yolo=0,revision=2 WHERE child_id=?1",[&child])?;
        crate::domain::exec_policy::shared_web::write(&db.db,&child,&profile,&agent,true)?;
        Ok(())
    }).await.unwrap();
    let replay=pool_reserve(&f,first_command.clone()).await;
    assert_eq!(replay.agent_reservation_id,first_receipt.agent_reservation_id);
    let child=replay.agent_reservation_id.unwrap(); let profile=f.profile.digest.clone();
    f.registry.0.store.call(4096,move|db|{
        let actual:(bool,u64)=db.db.query_row("SELECT yolo,revision FROM owner_shared_tool_policies WHERE child_id=?1",[&child],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(actual,(false,2));
        assert!(crate::domain::exec_policy::shared_web::read(&db.db,&child,&profile,&first_command.proposed_agent_id)?.enabled);
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}
fn proof(p: &ProfileAuthority, g: &NativeGrantReceipt) -> VerifiedManagementEnrollment {
    VerifiedManagementEnrollment::new(
        p.clone(),
        g.enrollment_id.clone(),
        g.contribution_id.clone(),
        g.resource_id.clone(),
        g.resource_revision.clone(),
        g.owner_mxid.clone(),
        g.executor_device_id.clone(),
        g.executor_generation,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
    .with_admission_request(g.admission_request_id.clone().unwrap())
    .unwrap()
}
async fn pool_fixture() -> PoolFixture { pool_fixture_limits(700, None).await }
async fn pool_fixture_limits(total: u64, limit: Option<u64>) -> PoolFixture {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    let profile = authority('a');
    let resource = registry.prepare_enrollment(profile.clone(), "enroll".into(), provider(), "gpt-5.6-sol".into(), Some("medium".into()), total.max(1000), Instant::now()+Duration::from_secs(30)).unwrap().approve().await.unwrap();
    let resource_revision = registry
        .resource(&profile, resource.resource_id.clone())
        .await
        .unwrap()
        .revision;
    let e = VerifiedManagementEnrollment::new(
        profile.clone(),
        "pool_enrollment".into(),
        "pool_contribution".into(),
        resource.resource_id.clone(),
        resource_revision,
        profile.account.clone(),
        "device_a".into(),
        1,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let grant = registry
        .prepare_capacity_grant(
            e,
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: limit,
                calendar: None,
                command_id: "pool_consent".into(),
                grant_id: "pool_grant".into(),
                coordinator_mxid: profile.account.clone(),
                delegation_expires_at_ms: graphs::now_ms().unwrap() + 60_000,
                allow_self_approval: false,
                eligible_managers: vec!["@manager:a.test".into()],
                allocated_tokens: total,
            },
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .with_admission_request("pool_admission".into())
        .unwrap()
        .approve()
        .await
        .unwrap();
    PoolFixture {
        _temp: temp,
        registry,
        profile,
        grant,
    }
}
fn pool_command(f: &PoolFixture, id: &str, tokens: u64) -> ManagedAgentReserveCommand {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("capacity/managed_agents/pool-agent-v1.json")).unwrap();
    let mut c: ManagedAgentReserveCommand =
        serde_json::from_value(fixture["command"].clone()).unwrap();
    let g = &f.grant;
    c.command_id = format!("pool_reserve_{id}");
    c.request_id = format!("pool_request_{id}");
    c.proposed_agent_id = format!("pool_agent_{id}");
    c.allocation_id = c.request_id.clone();
    c.manager_mxid = "@manager:a.test".into();
    c.decision_actor_mxid = f.profile.account.clone();
    c.pool_grant = Some(g.clone());
    c.grant_receipt_digest = canonical::digest(&serde_json::to_value(g).unwrap()).unwrap();
    c.enrollment_id = g.enrollment_id.clone();
    c.executor_device_id = g.executor_device_id.clone();
    c.executor_generation = g.executor_generation;
    c.resource_id = g.resource_id.clone();
    c.resource_revision = g.resource_revision.clone();
    c.authority_revision = g.grant_revision;
    c.requested_tokens = tokens;
    c.period_key = g.period_key.clone();
    c.issued_at_ms = graphs::now_ms().unwrap();
    c.expires_at_ms = g.delegation_expires_at_ms;
    c
}
async fn pool_reserve(
    f: &PoolFixture,
    c: ManagedAgentReserveCommand,
) -> ManagedAgentReservationReceipt {
    let digest = canonical::digest(&serde_json::to_value(&c).unwrap()).unwrap();
    f.registry
        .reserve_managed_agent(
            VerifiedManagedAgentDelivery::for_pool(
                proof(&f.profile, &f.grant),
                c,
                digest,
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn pool_agent_matrix_and_runtime_keep_direct_grant_authority_across_restart() {
    use shared_execution::*;
    let mut f = pool_fixture().await;
    let reserve = pool_command(&f, "runtime", 200);
    let saved = pool_reserve(&f, reserve.clone()).await;
    let digest = |v: &serde_json::Value| canonical::digest(v).unwrap();
    let now = graphs::now_ms().unwrap();
    let matrix = ManagedAgentMatrixChallenge {
        version: 1,
        capability: AGENT_MATRIX_CAPABILITY.into(),
        operation: "agent.matrix.check".into(),
        command_id: "pool_matrix".into(),
        assignment_id: "pool_assignment".into(),
        assignment_generation: 1,
        agent_reservation_id: saved.agent_reservation_id.clone().unwrap(),
        reserve_command: reserve.clone(),
        reserve_command_digest: digest(&serde_json::to_value(&reserve).unwrap()),
        reserve_receipt_digest: digest(&serde_json::to_value(&saved).unwrap()),
        space_id: String::new(),
        puppet_mxid: "@_hagency_pool:a.test".into(),
        issued_at_ms: now,
        expires_at_ms: now + 20000,
    };
    let observation = f
        .registry
        .check_managed_matrix_reservation(
            VerifiedManagedMatrixCheck::new(
                proof(&f.profile, &f.grant),
                matrix.clone(),
                digest(&serde_json::to_value(&matrix).unwrap()),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(observation.eligible);
    let runtime = SharedRuntimeCommand {
        version: 1,
        capability: SHARED_EXECUTION_CAPABILITY.into(),
        operation: "agent.execution.provision".into(),
        command_id: "pool_runtime".into(),
        assignment_id: matrix.assignment_id,
        assignment_generation: 1,
        agent_reservation_id: matrix.agent_reservation_id,
        reserve_command: reserve,
        reserve_command_digest: matrix.reserve_command_digest,
        reserve_receipt_digest: matrix.reserve_receipt_digest,
        policy: SHARED_TEXT_POLICY.into(),
        issued_at_ms: now,
        expires_at_ms: now + 20000,
    };
    let delivery = |f: &PoolFixture| {
        VerifiedSharedRuntimeDelivery::new(
            proof(&f.profile, &f.grant),
            runtime.clone(),
            digest(&serde_json::to_value(&runtime).unwrap()),
            true,
            Instant::now() + Duration::from_secs(20),
        )
        .unwrap()
    };
    assert!(matches!(
        f.registry.claim_shared_runtime(delivery(&f)).await.unwrap(),
        SharedRuntimeClaim::Issued(_)
    ));
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    assert!(matches!(
        f.registry.claim_shared_runtime(delivery(&f)).await.unwrap(),
        SharedRuntimeClaim::History(_)
    ));
}

#[tokio::test]
async fn pool_agent_reserves_without_project_and_cannot_exceed_parent_capacity() {
    let f = pool_fixture().await;
    let c = pool_command(&f, "first", 200);
    let first = pool_reserve(&f, c.clone()).await;
    assert_eq!(first.state, "reserved");
    assert_eq!(
        pool_reserve(&f, c).await.ledger_revision,
        first.ledger_revision
    );
    let over = pool_reserve(&f, pool_command(&f, "too_large", 501)).await;
    assert_eq!(over.reason.as_deref(), Some("insufficient_capacity"));
    assert_eq!(
        pool_reserve(&f, pool_command(&f, "remaining", 500))
            .await
            .state,
        "reserved"
    );
    f.registry.0.store.call(1024,|db| {
        assert_eq!(db.db.query_row("SELECT count(*) FROM owner_project_reservations",[],|r|r.get::<_,u64>(0))?,0);
        assert_eq!(db.db.query_row("SELECT sum(allocated_tokens) FROM owner_managed_agents WHERE pool_grant_id IS NOT NULL",[],|r|r.get::<_,u64>(0))?,700);
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[cfg(feature = "agent-budget")]
#[tokio::test]
async fn pool_agent_topup_requires_owner_keeps_unknown_usage_and_conserves_siblings() {
    use crate::owner_resources::budget::*;
    let f=pool_fixture().await;
    let c=pool_command(&f,"topup",200);
    let r=pool_reserve(&f,c.clone()).await;
    let child=r.agent_reservation_id.clone().unwrap();
    let saved_child=child.clone();
    let original=f.registry.0.store.call(1024,move|db|Ok(db.db.query_row("SELECT original_command FROM owner_managed_agents WHERE id=?1",[saved_child],|r|r.get::<_,String>(0))?)).await.unwrap();
    assert_eq!(pool_reserve(&f,pool_command(&f,"sibling",400)).await.state,"reserved");
    let assignment=||ManagedAssignmentAuthority::fixture(proof(&f.profile,&f.grant),child.clone(),"pool_topup_assignment".into(),1,Instant::now()+Duration::from_secs(20));
    let issued=match f.registry.claim_managed_provision(assignment()).await.unwrap(){ManagedProvisionClaim::Issued(v)=>v,_=>panic!("new provision")};
    f.registry.record_managed_provision(&issued,ManagedProvisionObservation::fixture("pool_qualified","qualified",Some("a".repeat(64)))).await.unwrap();
    let call=||ManagedCallAuthority::fixture(assignment(),"pool_unknown_call".into(),"b".repeat(64),200);
    f.registry.prepare_managed_call(call()).await.unwrap();
    let issued_call=match f.registry.issue_managed_call(call()).await.unwrap(){ManagedCallClaim::Issued(v)=>v,_=>panic!("new call")};
    f.registry.record_managed_usage(&issued_call,ManagedUsageObservation::fixture("interrupted",None,true,false,false)).await.unwrap();
    // No final usage evidence exists: the interrupted call holds only its known usage, none.
    let hash=|v:&serde_json::Value|canonical::digest(v).unwrap();
    let mut delta=AgentBudgetCommand { owner_funded: false,
        version:1,capability:AGENT_BUDGET_CAPABILITY.into(),operation:"agent.budget.increase".into(),
        command_id:"pool_budget_delta".into(),top_up_id:"pool_budget_request".into(),
        top_up_request_digest:"a".repeat(64),approval_command_id:"owner_approval".into(),approval_digest:"b".repeat(64),
        approving_administrator_mxid:f.profile.account.clone(),authority_revision:f.grant.grant_revision,
        assignment_id:"pool_topup_assignment".into(),assignment_generation:1,
        reserve_command:c.clone(),reserve_command_digest:hash(&serde_json::to_value(&c).unwrap()),
        reserve_receipt:r.clone(),reserve_receipt_digest:hash(&serde_json::to_value(&r).unwrap()),
        expected_budget_revision:1,expected_budget_digest:initial_budget_digest(&r).unwrap(),expected_ceiling_tokens:200,
        add_tokens:100,issued_at_ms:graphs::now_ms().unwrap(),expires_at_ms:c.expires_at_ms,
    };
    let mut foreign=delta.clone();foreign.approving_administrator_mxid=c.manager_mxid.clone();
    assert!(foreign.validate(&f.profile.account).is_err());
    foreign=delta.clone();foreign.authority_revision+=1;
    assert!(foreign.validate(&f.profile.account).is_err());
    let verified=|c:AgentBudgetCommand|VerifiedAgentBudgetDelivery::new(proof(&f.profile,&f.grant),c.clone(),hash(&serde_json::to_value(&c).unwrap()),true,false,Instant::now()+Duration::from_secs(20)).unwrap();
    let applied=f.registry.apply_managed_budget(verified(delta.clone())).await.unwrap();
    assert_eq!(applied.state,"applied");
    assert_eq!(f.registry.apply_managed_budget(verified(delta.clone())).await.unwrap().budget_digest,applied.budget_digest);
    let snapshot=f.registry.managed_agent_budget(proof(&f.profile,&f.grant),child.clone()).await.unwrap();
    assert_eq!((snapshot.effective_ceiling_tokens,snapshot.retained_tokens,snapshot.remaining_tokens),(300,0,300));
    assert!(!snapshot.usage_complete);
    delta.command_id="pool_budget_over".into();delta.top_up_id="pool_budget_over_request".into();
    delta.expected_budget_revision=applied.budget_revision;delta.expected_budget_digest=applied.budget_digest;
    delta.expected_ceiling_tokens=300;delta.add_tokens=1;
    let refused=f.registry.apply_managed_budget(verified(delta.clone())).await.unwrap();
    assert_eq!(refused.state,"refused");
    // An explicit resource-owner supplement may exceed the published 700,
    // but must still conserve the 1000-token machine/Seat including the sibling.
    delta.owner_funded=true;
    assert!(delta.validate(&f.profile.account).is_err(), "v1 cannot imply owner funding");
    delta.version=2;
    foreign=delta.clone();foreign.approving_administrator_mxid=c.manager_mxid.clone();
    assert!(foreign.validate(&f.profile.account).is_err());
    delta.command_id="owner_budget_over".into();delta.top_up_id="owner_over_request".into();delta.add_tokens=301;
    let over=f.registry.apply_managed_budget(verified(delta.clone())).await.unwrap();
    assert_eq!(over.reason.as_deref(),Some("insufficient_capacity"));
    delta.command_id="owner_budget_exact".into();delta.top_up_id="owner_exact_request".into();delta.add_tokens=300;
    let funded=f.registry.apply_managed_budget(verified(delta.clone())).await.unwrap();
    assert_eq!(funded.state,"applied");
    assert_eq!(funded.effective_ceiling_tokens,600);
    assert_eq!(f.registry.apply_managed_budget(verified(delta)).await.unwrap().budget_digest,funded.budget_digest);
    let snapshot=f.registry.managed_agent_budget(proof(&f.profile,&f.grant),child.clone()).await.unwrap();
    assert_eq!((snapshot.effective_ceiling_tokens,snapshot.retained_tokens,snapshot.remaining_tokens),(600,0,600));
    assert!(!snapshot.usage_complete);
    f.registry.0.store.call(2048,move|db| {
        assert_eq!(db.db.query_row("SELECT allocated_tokens FROM owner_capacity_grants",[],|r|r.get::<_,u64>(0))?,700);
        assert_eq!(db.db.query_row("SELECT count(*) FROM owner_managed_budget_deltas",[],|r|r.get::<_,u64>(0))?,2);
        assert_eq!(db.db.query_row("SELECT count(*) FROM owner_project_reservations",[],|r|r.get::<_,u64>(0))?,0);
        assert_eq!(db.db.query_row("SELECT original_command FROM owner_managed_agents WHERE id=?1",[child],|r|r.get::<_,String>(0))?,original);
        assert_eq!(db.db.query_row("SELECT reserved_tokens FROM owner_managed_calls",[],|r|r.get::<_,u64>(0))?,200);
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn closing_pool_preserves_direct_agent_liability() {
    let f = pool_fixture().await;
    assert_eq!(
        pool_reserve(&f, pool_command(&f, "retained", 200))
            .await
            .state,
        "reserved"
    );
    let command = GrantClosureCommand {
        version: 1,
        command_id: "close_pool".into(),
        operation: "grant.close".into(),
        grant_id: f.grant.grant_id.clone(),
        grant_receipt_digest: canonical::digest(&serde_json::to_value(&f.grant).unwrap()).unwrap(),
        previous_settlement_digest: None,
        reason: "owner_withdrawal".into(),
        requested_at_ms: graphs::now_ms().unwrap(),
    };
    let digest = canonical::digest(&serde_json::to_value(&command).unwrap()).unwrap();
    let result = f
        .registry
        .apply_parent_settlement(
            VerifiedParentDelivery::new(
                proof(&f.profile, &f.grant),
                command,
                digest,
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.retained_tokens, 200);
    assert_eq!(result.state, "retained");
    assert_eq!(
        pool_reserve(&f, pool_command(&f, "late", 100))
            .await
            .reason
            .as_deref(),
        Some("authority_changed")
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn ended_pool_month_frees_unused_capacity_but_live_agents_hold_until_fenced() {
    let f = pool_fixture().await;
    let end = f.grant.period_end_ms;
    let resource = f.grant.resource_id.clone();
    let reserve = pool_command(&f, "month_end", 200);
    pool_reserve(&f, reserve.clone()).await;
    // In its month the grant holds its whole allocation.
    assert_eq!(
        pool_at(&f.registry, &resource, end - 1).await,
        (Some(300), 700)
    );
    // After the month only the live Agent's unused allocation stays held.
    assert_eq!(pool_at(&f.registry, &resource, end).await, (Some(800), 200));
    let digest = |v: &serde_json::Value| canonical::digest(v).unwrap();
    let fence = ManagedAgentFenceCommand {
        version: 1,
        capability: AGENT_FENCE_CAPABILITY.into(),
        operation: "agent.fence".into(),
        command_id: "month_end_fence".into(),
        reserve_command_digest: digest(&serde_json::to_value(&reserve).unwrap()),
        reserve_command: reserve,
        issued_at_ms: graphs::now_ms().unwrap(),
    };
    let fenced = f
        .registry
        .apply_managed_agent_fence(
            VerifiedManagedAgentFence::for_pool(
                proof(&f.profile, &f.grant),
                fence.clone(),
                digest(&serde_json::to_value(&fence).unwrap()),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(fenced.usage_complete);
    // Fenced with complete usage: the ended month holds nothing more.
    assert_eq!(pool_at(&f.registry, &resource, end).await, (Some(1000), 0));
    assert_eq!(
        pool_at(&f.registry, &resource, end - 1).await,
        (Some(300), 700)
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn pool_agent_owner_fence_retains_history_and_releases_only_known_unused_capacity() {
    let f = pool_fixture().await;
    let reserve = pool_command(&f, "fenced", 200);
    let original = pool_reserve(&f, reserve.clone()).await;
    let digest = |v: &serde_json::Value| canonical::digest(v).unwrap();
    let fence = ManagedAgentFenceCommand {
        version: 1,
        capability: AGENT_FENCE_CAPABILITY.into(),
        operation: "agent.fence".into(),
        command_id: "pool_owner_fence".into(),
        reserve_command_digest: digest(&serde_json::to_value(&reserve).unwrap()),
        reserve_command: reserve.clone(),
        issued_at_ms: graphs::now_ms().unwrap(),
    };
    let delivery = VerifiedManagedAgentFence::for_pool(
        proof(&f.profile, &f.grant),
        fence.clone(),
        digest(&serde_json::to_value(&fence).unwrap()),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    let result = f
        .registry
        .apply_managed_agent_fence(delivery)
        .await
        .unwrap();
    assert_eq!(result.agent_reservation_id, original.agent_reservation_id);
    assert_eq!(result.retained_tokens, Some(0));
    assert!(result.usage_complete);
    assert_eq!(result.process_state, "not_started");
    let settlement=ManagedAgentSettlementCommand {version:1,capability:AGENT_SETTLEMENT_CAPABILITY.into(),operation:"agent.settle".into(),command_id:"pool_settle_unused".into(),fence_command:fence,fence_receipt_digest:digest(&serde_json::to_value(&result).unwrap()),issued_at_ms:graphs::now_ms().unwrap()};
    let settled=f.registry.settle_managed_agent(VerifiedManagedAgentSettlement::for_pool(proof(&f.profile,&f.grant),settlement.clone(),digest(&serde_json::to_value(&settlement).unwrap()),true,Instant::now()+Duration::from_secs(20)).unwrap()).await.unwrap();
    assert_eq!(settled.retained_tokens,0);assert_eq!(settled.known_consumed_tokens,0);assert!(settled.usage_complete);
    assert_eq!(
        pool_reserve(&f, reserve).await.ledger_revision,
        original.ledger_revision
    );
    assert_eq!(
        pool_reserve(&f, pool_command(&f, "new", 700)).await.state,
        "reserved"
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn pool_agent_schema_upgrade_preserves_populated_legacy_holds_and_rolls_back() {
    let mut f = super::fixture().await;
    let original = super::agent(&f, "legacy_reserved", 100);
    let receipt = super::reserve(&f, original.clone()).await;
    let fenced = super::agent(&f, "legacy_fenced", 100);
    super::reserve(&f, fenced.clone()).await;
    super::fences::apply_fence(&f, super::fences::command(&f, fenced, "legacy_fence")).await;
    f.registry.shutdown().await.unwrap();
    let root = f._temp.path().join("owner");
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    // Reconstruct schema 99, retaining real rows and dependent effects/receipts.
    db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    for table in ["owner_managed_agents", "owner_managed_fences"] {
        let schema: String = db
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        let old = schema
            .lines()
            .filter(|line| !line.contains("pool_grant_id"))
            .collect::<Vec<_>>()
            .join("\n")
            .replace(
                "project_reservation_id TEXT REFERENCES",
                "project_reservation_id TEXT NOT NULL REFERENCES",
            );
        let columns = db
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .into_iter()
            .filter(|c| c != "pool_grant_id")
            .collect::<Vec<_>>()
            .join(",");
        let objects=db.prepare("SELECT sql FROM sqlite_master WHERE tbl_name=?1 AND type IN ('trigger','index') AND sql IS NOT NULL AND name<>'owner_managed_pool_children'").unwrap().query_map([table],|r|r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        db.execute_batch(&format!("CREATE TEMP TABLE saved_{table} AS SELECT {columns} FROM {table};DROP TABLE {table};{old};INSERT INTO {table}({columns}) SELECT {columns} FROM saved_{table};DROP TABLE saved_{table};")).unwrap();
        for sql in objects {
            db.execute_batch(&sql.replace("NEW.pool_grant_id IS NOT OLD.pool_grant_id OR ", ""))
                .unwrap();
        }
    }
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=99;CREATE INDEX owner_managed_pool_children ON owner_capacity_grants(id);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        99
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM owner_managed_agents", [], |r| r
            .get::<_, u64>(0))
            .unwrap(),
        2
    );
    db.execute_batch("DROP INDEX owner_managed_pool_children")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(
        serde_json::to_value(super::reserve(&f, original).await).unwrap(),
        serde_json::to_value(receipt).unwrap()
    );
    f.registry.0.store.call(1024,|db| {
        assert_eq!(db.db.query_row("SELECT count(*) FROM owner_managed_agents WHERE pool_grant_id IS NULL AND project_reservation_id IS NOT NULL",[],|r|r.get::<_,u64>(0))?,2);
        assert_eq!(db.db.query_row("SELECT count(*) FROM owner_managed_fences",[],|r|r.get::<_,u64>(0))?,1);
        assert_eq!(db.db.query_row("SELECT count(*) FROM pragma_foreign_key_check",[],|r|r.get::<_,u64>(0))?,0);
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_agent_execution_policy_is_per_agent_and_retains_original_reservations() {
    use crate::management::*;
    let f=pool_fixture().await;
    let a=pool_command(&f,"policy_a",100);let b=pool_command(&f,"policy_b",100);
    assert_eq!(pool_reserve(&f,a.clone()).await.state,"reserved");
    assert_eq!(pool_reserve(&f,b.clone()).await.state,"reserved");
    let owner=f.profile.account.clone();
    f.registry.0.store.call(64*1024,move|db|{
        let query=ExecutionPolicyQuery{resource_id:a.resource_id.clone(),engagement_id:a.proposed_agent_id.clone()};
        let original=db.management_execution_policy(&query, |_|Ok(()))?;
        assert_eq!(original.shared_mode.as_deref(),Some("standard"));
        let page=db.management_execution_policies(&ExecutionPolicyListQuery{resource_id:a.resource_id.clone(),after:String::new()},|_|Ok(()))?;
        assert_eq!(page.agents.len(),2);
        let input=EditExecutionPolicy{web_search:None,command_id:"shared_yolo".into(),resource_id:a.resource_id.clone(),engagement_id:a.proposed_agent_id.clone(),expected_revision:original.revision.clone(),execution_policy:ExecutionPolicyValue{yolo:true}};
        let ctx=HostContext::new(owner,1,"a".repeat(64),1,"policy_device".into(),1)?;
        let receipt=||CommandReceipt::new(&ctx,"edit_execution_policy",&input.command_id,&input);
        assert!(db.management_edit_execution_policy(&input,receipt()?,|_|Err(Error::LocalAuthority)).is_err());
        let enabled=db.management_edit_execution_policy(&input,receipt()?,|_|Ok(()))?;
        assert_eq!(enabled.shared_mode.as_deref(),Some("yolo"));
        assert_eq!(db.management_edit_execution_policy(&input,receipt()?,|_|Ok(()))?,enabled);
        let other=db.management_execution_policy(&ExecutionPolicyQuery{resource_id:b.resource_id.clone(),engagement_id:b.proposed_agent_id.clone()},|_|Ok(()))?;
        assert_eq!(other.shared_mode.as_deref(),Some("standard"));
        let mut standard=input.clone();standard.command_id="shared_standard".into();standard.execution_policy.yolo=false;
        assert!(matches!(db.management_edit_execution_policy(&standard,CommandReceipt::new(&ctx,"edit_execution_policy",&standard.command_id,&standard)?,|_|Ok(())),Err(Error::Conflict)));
        standard.expected_revision=enabled.revision;
        let saved=db.management_edit_execution_policy(&standard,CommandReceipt::new(&ctx,"edit_execution_policy",&standard.command_id,&standard)?,|_|Ok(()))?;
        assert_eq!(saved.shared_mode.as_deref(),Some("standard"));
        let raw:String=db.db.query_row("SELECT original_command FROM owner_managed_agents WHERE agent_id=?1",[&a.proposed_agent_id],|r|r.get(0))?;
        assert_eq!(serde_json::from_str::<serde_json::Value>(&raw)?,serde_json::to_value(a)?);
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_agent_execution_policy_web_switch_is_independent_durable_and_owner_scoped() {
    use crate::management::*;
    let f = pool_fixture().await;
    let a = pool_command(&f, "web_a", 100);
    let b = pool_command(&f, "web_b", 100);
    pool_reserve(&f, a.clone()).await;
    pool_reserve(&f, b.clone()).await;
    let query = ExecutionPolicyQuery { resource_id: a.resource_id.clone(), engagement_id: a.proposed_agent_id.clone() };
    let query_again = query.clone();
    let root = f._temp.path().join("owner");
    // Existing opt-ins survive; an explicit UI disable must override this file.
    crate::private::write_new(&root.join("owner-shared-tools.json"), &serde_json::to_vec(&json!({"version":1,"liveWebAgentIds":[a.proposed_agent_id]})).unwrap()).unwrap();
    let owner = f.profile.account.clone();
    let (input, enabled) = f.registry.0.store.call(64 * 1024, move |db| {
        // Reconstruct Agents created before resource defaults were captured.
        // Their file-based opt-in must survive until an explicit owner edit.
        db.db.execute("DELETE FROM owner_shared_tool_policies", [])?;
        db.db.execute("DELETE FROM owner_web_catalog_settings WHERE kind='agent_live_web'", [])?;
        let ctx = HostContext::new(owner, 1, "a".repeat(64), 1, "web_device".into(), 1)?;
        let original = db.management_execution_policy(&query, |_| Ok(()))?;
        assert_eq!(original.web_search, Some(true));
        assert_eq!(original.shared_mode.as_deref(), Some("chat"));
        let off = EditExecutionPolicy { web_search: Some(false), command_id: "web_off".into(), resource_id: query.resource_id.clone(), engagement_id: query.engagement_id.clone(), expected_revision: original.revision, execution_policy: original.execution_policy };
        let receipt = |input: &EditExecutionPolicy| CommandReceipt::new(&ctx, "execution_policy.edit", &input.command_id, input);
        assert!(db.management_edit_execution_policy(&off, receipt(&off)?, |_| Err(Error::LocalAuthority)).is_err());
        let disabled = db.management_edit_execution_policy(&off, receipt(&off)?, |_| Ok(()))?;
        assert_eq!(disabled.web_search, Some(false));
        assert_eq!(disabled.shared_mode.as_deref(), Some("chat"));
        let mut on = off.clone(); on.command_id = "web_on".into(); on.web_search = Some(true);
        assert!(matches!(db.management_edit_execution_policy(&on, receipt(&on)?, |_| Ok(())), Err(Error::Conflict)));
        on.expected_revision = disabled.revision.clone();
        let mut mixed = on.clone(); mixed.command_id = "mixed".into(); mixed.execution_policy.yolo = true;
        assert!(db.management_edit_execution_policy(&mixed, receipt(&mixed)?, |_| Ok(())).is_err());
        let enabled = db.management_edit_execution_policy(&on, receipt(&on)?, |_| Ok(()))?;
        assert_eq!(enabled.web_search, Some(true));
        assert_eq!(enabled.shared_mode.as_deref(), Some("chat"));
        assert_eq!(db.management_edit_execution_policy(&off, receipt(&off)?, |_| Ok(()))?, disabled);
        assert_eq!(db.management_execution_policy(&query, |_| Ok(()))?, enabled);
        let other = db.management_execution_policy(&ExecutionPolicyQuery { resource_id: b.resource_id, engagement_id: b.proposed_agent_id }, |_| Ok(()))?;
        assert_eq!(other.web_search, Some(false));
        assert_eq!(db.db.query_row("SELECT count(*) FROM owner_shared_tool_policies", [], |r| r.get::<_,u64>(0))?, 0);
        let raw: String = db.db.query_row("SELECT original_command FROM owner_managed_agents WHERE agent_id=?1", [&a.proposed_agent_id], |r| r.get(0))?;
        assert_eq!(serde_json::from_str::<serde_json::Value>(&raw)?, serde_json::to_value(a)?);
        Ok((on, enabled))
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
    let reopened = OwnerResourceRegistry::open_root(&root).unwrap();
    reopened.0.store.call(16 * 1024, move |db| {
        assert_eq!(db.management_execution_policy(&query_again, |_| Ok(()))?, enabled);
        let ctx = HostContext::new("@owner:example.test".into(), 1, "a".repeat(64), 1, "foreign".into(), 1)?;
        let receipt = CommandReceipt::new(&ctx, "execution_policy.edit", &input.command_id, &input)?;
        assert!(db.management_edit_execution_policy(&input, receipt, |_| Err(Error::LocalAuthority)).is_err());
        Ok(())
    }).await.unwrap();
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn split_pool_limits_reserve_four_agents_reject_over_limit_and_preserve_replay() {
    let f = pool_fixture_limits(20_000_000, Some(5_000_000)).await;
    assert_eq!(f.grant.version, 5);
    assert_eq!(f.grant.allocated_tokens, 20_000_000);
    assert_eq!(f.grant.request_limit(), 5_000_000);
    let over = pool_reserve(&f, pool_command(&f, "over_request_limit", 5_000_001)).await;
    assert_eq!(over.reason.as_deref(), Some("insufficient_capacity"));
    let first_command = pool_command(&f, "first_of_four", 5_000_000);
    let first = pool_reserve(&f, first_command.clone()).await;
    assert_eq!(first.state, "reserved");
    for id in ["second", "third", "fourth"] {
        assert_eq!(pool_reserve(&f, pool_command(&f, id, 5_000_000)).await.state, "reserved");
    }
    assert_eq!(pool_reserve(&f, first_command).await.ledger_revision, first.ledger_revision);
    let fifth = pool_reserve(&f, pool_command(&f, "fifth", 1)).await;
    assert_eq!(fifth.reason.as_deref(), Some("insufficient_capacity"));
    f.registry.0.store.call(1024, |db| {
        assert_eq!(db.db.query_row("SELECT SUM(allocated_tokens) FROM owner_managed_agents", [], |r|r.get::<_,u64>(0))?,20_000_000);
        Ok(())
    }).await.unwrap();
}

#[test]
fn split_pool_limits_wire_rejects_invalid_caps_and_preserves_legacy_bytes() {
    let v: serde_json::Value = serde_json::from_str(include_str!("pool-grant-calendar-v4.json")).unwrap();
    let raw = &v[0]["receipt"];
    let mut grant: NativeGrantReceipt = serde_json::from_value(raw.clone()).unwrap();
    assert!(grant.supported_shape());
    assert!(!serde_json::to_value(&grant).unwrap().as_object().unwrap().contains_key("perRequestTokens"));
    grant.version = 5;
    grant.allocated_tokens = 20_000_000;
    grant.per_request_tokens = Some(5_000_000);
    assert!(grant.supported_shape());
    for bad in [None, Some(0), Some(20_000_001), Some(u64::MAX)] {
        grant.per_request_tokens = bad;
        assert!(!grant.supported_shape());
    }
}
