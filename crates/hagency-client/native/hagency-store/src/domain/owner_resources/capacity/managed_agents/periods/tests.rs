use super::*;
use hagency_core::recurring_quota::{Cadence, Calendar};

#[test]
fn recurring_native_period_wire_matches_canonical_vector() {
    let v: serde_json::Value =
        serde_json::from_str(include_str!("../agent-period-v1.json")).unwrap();
    let c: ManagedAgentPeriodRenewCommand = serde_json::from_value(v["command"].clone()).unwrap();
    c.validate(&c.decision_actor_mxid).unwrap();
    assert_eq!(digest(&c), v["commandDigest"]);
    let r: ManagedAgentPeriodReceipt = serde_json::from_value(v["receipt"].clone()).unwrap();
    r.validate_for(&c).unwrap();
    assert_eq!(digest(&r), v["receiptDigest"]);
}

struct Fixture {
    _temp: tempfile::TempDir,
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    grant: NativeGrantReceipt,
    command: ManagedAgentReserveCommand,
    child: String,
}
fn digest<T: Serialize>(v: &T) -> String {
    canonical::digest(&serde_json::to_value(v).unwrap()).unwrap()
}
fn enrollment(f: &Fixture) -> VerifiedManagementEnrollment {
    let g = &f.grant;
    VerifiedManagementEnrollment::new(
        f.profile.clone(),
        g.enrollment_id.clone(),
        g.contribution_id.clone(),
        g.resource_id.clone(),
        g.resource_revision.clone(),
        g.owner_mxid.clone(),
        g.executor_device_id.clone(),
        g.executor_generation,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
    .with_admission_request(g.admission_request_id.clone().unwrap())
    .unwrap()
}
fn assignment(f: &Fixture) -> ManagedAssignmentAuthority {
    ManagedAssignmentAuthority::fixture(
        enrollment(f),
        f.child.clone(),
        "period_assignment".into(),
        1,
        Instant::now() + Duration::from_secs(30),
    )
}
fn call(f: &Fixture, id: &str, tokens: u64) -> ManagedCallAuthority {
    ManagedCallAuthority::fixture(assignment(f), id.into(), "c".repeat(64), tokens)
}
async fn fixture(mode: RenewalMode) -> Fixture {
    fixture_kind(mode, true, None).await
}
async fn fixture_kind(
    mode: RenewalMode,
    qualify: bool,
    clock: Option<Arc<std::sync::atomic::AtomicU64>>,
) -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let registry = OwnerResourceRegistry::open_root(&temp.path().join("owner")).unwrap();
    if let Some(clock) = clock {
        registry
            .0
            .store
            .call(1024, move |_| {
                graphs::install_test_clock(&clock);
                Ok(())
            })
            .await
            .unwrap();
    }
    let profile = ProfileAuthority::new(
        "a".repeat(64),
        "@owner:a.test".into(),
        Instant::now() + Duration::from_secs(180),
    )
    .unwrap();
    let resource = registry
        .prepare_enrollment(
            profile.clone(),
            "period_enroll".into(),
            VerifiedProviderAccount::new(
                "codex",
                "openai.chatgpt",
                "fixture_account",
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            1000,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
        .approve()
        .await
        .unwrap()
        .resource_id;
    let revision = registry
        .resource(&profile, resource.clone())
        .await
        .unwrap()
        .revision;
    let e = VerifiedManagementEnrollment::new(
        profile.clone(),
        "period_enrollment".into(),
        "period_contribution".into(),
        resource,
        revision,
        profile.account.clone(),
        "period_device".into(),
        1,
        Instant::now() + Duration::from_secs(60),
    )
    .unwrap()
    .with_admission_request("period_admission".into())
    .unwrap();
    let calendar = Calendar {
        cadence: Cadence::Daily,
        time_zone: "America/Los_Angeles".into(),
    };
    let now = graphs::now_ms().unwrap();
    let grant = registry
        .prepare_capacity_grant(
            e,
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: Some(calendar),
                command_id: "period_grant_consent".into(),
                grant_id: "period_grant".into(),
                coordinator_mxid: profile.account.clone(),
                delegation_expires_at_ms: now + 20 * 86_400_000,
                allow_self_approval: false,
                eligible_managers: vec!["@manager:a.test".into()],
                allocated_tokens: 700,
            },
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("../pool-agent-v1.json")).unwrap();
    let mut command: ManagedAgentReserveCommand =
        serde_json::from_value(vector["command"].clone()).unwrap();
    command.command_id = "period_reserve".into();
    command.request_id = "period_request".into();
    command.proposed_agent_id = "period_agent".into();
    command.allocation_id = command.request_id.clone();
    command.renewal_mode = Some(mode);
    command.pool_grant = Some(grant.clone());
    command.grant_receipt_digest = digest(&grant);
    command.manager_mxid = "@manager:a.test".into();
    command.decision_actor_mxid = profile.account.clone();
    command.enrollment_id = grant.enrollment_id.clone();
    command.executor_device_id = grant.executor_device_id.clone();
    command.executor_generation = grant.executor_generation;
    command.resource_id = grant.resource_id.clone();
    command.resource_revision = grant.resource_revision.clone();
    command.authority_revision = grant.grant_revision;
    command.requested_tokens = 200;
    command.period_key = grant.period_key.clone();
    command.issued_at_ms = now;
    command.expires_at_ms = grant.period_end_ms;
    let mut f = Fixture {
        _temp: temp,
        registry,
        profile,
        grant,
        command,
        child: String::new(),
    };
    let result = f
        .registry
        .reserve_managed_agent(
            VerifiedManagedAgentDelivery::for_pool(
                enrollment(&f),
                f.command.clone(),
                digest(&f.command),
                true,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.state, "reserved", "{:?}", result.reason);
    f.child = result.agent_reservation_id.unwrap();
    if qualify {
        let scope = match f
            .registry
            .claim_managed_provision(assignment(&f))
            .await
            .unwrap()
        {
            ManagedProvisionClaim::Issued(scope) => scope,
            _ => panic!("first provision"),
        };
        f.registry
            .record_managed_provision(
                &scope,
                ManagedProvisionObservation::fixture(
                    "period_qualified",
                    "qualified",
                    Some("a".repeat(64)),
                ),
            )
            .await
            .unwrap();
    }
    f
}
fn renewal(f: &Fixture, g: NativeGrantReceipt, id: &str) -> ManagedAgentPeriodRenewCommand {
    ManagedAgentPeriodRenewCommand {
        version: 1,
        capability: AGENT_PERIOD_CAPABILITY.into(),
        operation: "agent.period.renew".into(),
        command_id: id.into(),
        reserve_command: f.command.clone(),
        reserve_command_digest: digest(&f.command),
        agent_reservation_id: f.child.clone(),
        assignment_id: "period_assignment".into(),
        assignment_generation: 1,
        current_grant_digest: digest(&g),
        period: Period {
            key: g.period_key.clone(),
            start_ms: g.period_start_ms,
            end_ms: g.period_end_ms,
        },
        decision_actor_mxid: f.profile.account.clone(),
        renewal_mode: f.command.renewal_mode.unwrap(),
        issued_at_ms: g.period_start_ms,
        expires_at_ms: g.period_end_ms,
        current_grant: g,
    }
}
// Test-only clock seam invokes the real parent/native transactional writers.
async fn next_grant(
    f: &Fixture,
    previous: NativeGrantReceipt,
    at: u64,
    id: &str,
) -> NativeGrantReceipt {
    let consent = f
        .registry
        .prepare_capacity_grant_successor_at(
            enrollment(f),
            previous,
            "renewal",
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: f.grant.calendar.clone(),
                command_id: format!("{id}_consent"),
                grant_id: id.into(),
                coordinator_mxid: f.profile.account.clone(),
                delegation_expires_at_ms: f.grant.delegation_expires_at_ms,
                allow_self_approval: false,
                eligible_managers: f.grant.eligible_managers.clone(),
                allocated_tokens: 700,
            },
            Instant::now() + Duration::from_secs(30),
            at,
        )
        .unwrap();
    let g = f
        .registry
        .0
        .store
        .call(32768, move |db| {
            db.owner_capacity_grant_with_clock(consent, || Ok(at))
        })
        .await
        .unwrap();
    f.registry
        .confirm_capacity_grant_succession(
            f.profile.clone(),
            g.executor_device_id.clone(),
            g.grant_id.clone(),
        )
        .await
        .unwrap();
    g
}
async fn apply_at(
    f: &Fixture,
    c: ManagedAgentPeriodRenewCommand,
    at: u64,
) -> Result<ManagedAgentPeriodReceipt, Error> {
    let d = VerifiedManagedAgentPeriodDelivery::new(
        enrollment(f),
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(30),
    )?;
    let r = f.registry.0.clone();
    f.registry
        .0
        .store
        .call(32768, move |db| {
            d.check(&r)?;
            let tx = db
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = apply(&tx, &d, at)?;
            d.check(&r)?;
            tx.commit()?;
            Ok(value)
        })
        .await
}

#[tokio::test]
async fn recurring_native_calls_pin_first_period_and_charge_before_refusing_new_work() {
    let f = fixture(RenewalMode::Automatic).await;
    let prepared = f
        .registry
        .prepare_managed_call(call(&f, "all_credit", 200))
        .await
        .unwrap();
    let claim = f
        .registry
        .issue_managed_call(call(&f, "all_credit", 200))
        .await
        .unwrap();
    let ManagedCallClaim::Issued(scope) = claim else { panic!("first issue"); };
    f.registry.prepare_managed_call(call(&f, "unmeasured_does_not_hold", 0)).await.unwrap();
    let budget = f
        .registry
        .managed_agent_budget(enrollment(&f), f.child.clone())
        .await
        .unwrap();
    // Unmeasured calls keep their identity but consume no fictitious quota.
    assert_eq!(budget.remaining_tokens, 200);
    assert_eq!(budget.retained_tokens, 0);
    use hagency_metering::{
        observation::UsageObservation,
        runtime_usage::{CodexUsage, CounterBreakdown, ProjectionDiagnostics},
    };
    let counters = CounterBreakdown {
        total_tokens: Some(210), input_tokens: Some(210), cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0), output_tokens: Some(0), reasoning_output_tokens: Some(0),
    };
    let usage = UsageObservation::codex_runtime(CodexUsage {
        total: counters, last: counters, context_window: None,
        diagnostics: ProjectionDiagnostics::default(),
    }).unwrap();
    f.registry.record_managed_usage(&scope,
        ManagedUsageObservation::fixture("actual_overrun", Some(usage), true, true, false)
    ).await.unwrap();
    assert!(matches!(f.registry.prepare_managed_call(call(&f, "after_overrun", 0)).await, Err(Error::Capacity)));
    let budget = f.registry.managed_agent_budget(enrollment(&f), f.child.clone()).await.unwrap();
    assert_eq!(budget.known_consumed_tokens, 210);
    assert_eq!(budget.remaining_tokens, 0);
    let child = f.child.clone();
    f.registry.0.store.call(4096,move|db|{
        let period=current(&db.db,&child,graphs::now_ms()?)?.unwrap();
        let pin:String=db.db.query_row("SELECT period_id FROM owner_managed_call_periods WHERE call_id=?1",[&prepared.call_id],|r|r.get(0))?;assert_eq!(pin,period.id);
        assert!(db.db.execute("UPDATE owner_managed_call_periods SET period_id='another' WHERE call_id=?1",[&prepared.call_id]).is_err());
        assert!(db.db.execute("DELETE FROM owner_managed_periods WHERE child_id=?1",[&child]).is_err());Ok(())
    }).await.unwrap();
    let early = renewal(&f, f.grant.clone(), "early_refill");
    assert!(early.validate(&f.profile.account).is_err());
}

#[tokio::test]
async fn recurring_native_cycles_replay_skip_and_retain_old_unknown_on_original_grant() {
    let f = fixture(RenewalMode::Automatic).await;
    let old = f
        .registry
        .prepare_managed_call(call(&f, "old_unknown", 120))
        .await
        .unwrap();
    let scope = match f
        .registry
        .issue_managed_call(call(&f, "old_unknown", 120))
        .await
        .unwrap()
    {
        ManagedCallClaim::Issued(scope) => scope,
        _ => panic!("first issue"),
    };
    let next = next_grant(
        &f,
        f.grant.clone(),
        f.grant.period_end_ms + 1,
        "period_next",
    )
    .await;
    let c = renewal(&f, next.clone(), "renew_next");
    let (one, two) = tokio::join!(
        apply_at(&f, c.clone(), next.period_start_ms + 1),
        apply_at(&f, c.clone(), next.period_start_ms + 2)
    );
    let one = one.unwrap();
    let two = two.unwrap();
    assert_eq!(one.ledger_revision, two.ledger_revision);
    let mut duplicate = c.clone();
    duplicate.command_id = "duplicate_credit".into();
    assert!(matches!(
        apply_at(&f, duplicate, next.period_start_ms + 2).await,
        Err(Error::Conflict)
    ));
    let child = f.child.clone();
    let first = f.grant.grant_id.clone();
    let second = next.grant_id.clone();
    let at = next.period_start_ms + 1;
    f.registry
        .0
        .store
        .call(4096, move |db| {
            let p = current(&db.db, &child, at)?.unwrap();
            let u = usage(&db.db, &child, Some(&p.id))?;
            // Only known usage is held, but the unknown call keeps its period open.
            assert_eq!((u.consumed, u.held, u.prior_held), (0, 0, 0));
            assert_eq!(grant_usage(&db.db, &child, &first, at)?, (0, 0, false));
            assert_eq!(grant_usage(&db.db, &child, &second, at)?, (200, 0, true));
            let pinned: String = db.db.query_row(
                "SELECT period_id FROM owner_managed_call_periods WHERE call_id=?1",
                [&old.call_id],
                |r| r.get(0),
            )?;
            assert_ne!(pinned, p.id);
            Ok(())
        })
        .await
        .unwrap();
    // The actual usage-observation writer settles a late result against its
    // immutable original call; the new authorization remains unspent.
    use hagency_metering::{
        observation::UsageObservation,
        runtime_usage::{CodexUsage, CounterBreakdown, ProjectionDiagnostics},
    };
    let counters = CounterBreakdown {
        total_tokens: Some(80),
        input_tokens: Some(80),
        cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0),
        output_tokens: Some(0),
        reasoning_output_tokens: Some(0),
    };
    let evidence = UsageObservation::codex_runtime(CodexUsage {
        total: counters,
        last: counters,
        context_window: Some(100_000),
        diagnostics: ProjectionDiagnostics::default(),
    })
    .unwrap();
    f.registry
        .record_managed_usage(
            &scope,
            ManagedUsageObservation::fixture("late_settled", Some(evidence), true, true, false),
        )
        .await
        .unwrap();
    let child = f.child.clone();
    let first = f.grant.grant_id.clone();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            let p = current(&db.db, &child, at)?.unwrap();
            let u = usage(&db.db, &child, Some(&p.id))?;
            assert_eq!((u.consumed, u.held, u.prior_held), (0, 0, 0));
            assert_eq!(grant_usage(&db.db, &child, &first, at)?, (0, 0, true));
            Ok(())
        })
        .await
        .unwrap();
    let skipped_at = next.period_end_ms + 3 * 86_400_000;
    let skipped = next_grant(&f, next, skipped_at, "period_after_offline").await;
    apply_at(&f, renewal(&f, skipped, "renew_after_offline"), skipped_at)
        .await
        .unwrap();
    let child = f.child.clone();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            let count: u64 = db.db.query_row(
                "SELECT COUNT(*) FROM owner_managed_periods WHERE child_id=?1",
                [child],
                |r| r.get(0),
            )?;
            assert_eq!(count, 3);
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn recurring_native_manual_owner_revocation_and_profile_gates() {
    let f = fixture(RenewalMode::Owner).await;
    let next = next_grant(
        &f,
        f.grant.clone(),
        f.grant.period_end_ms + 1,
        "manual_next",
    )
    .await;
    let c = renewal(&f, next.clone(), "manual_renew");
    let mut wrong = c.clone();
    wrong.decision_actor_mxid = "@manager:a.test".into();
    assert!(wrong.validate(&f.profile.account).is_err());
    wrong = c.clone();
    wrong.renewal_mode = RenewalMode::Automatic;
    assert!(wrong.validate(&f.profile.account).is_err());
    assert!(matches!(
        apply_at(&f, c.clone(), f.grant.period_end_ms - 1).await,
        Err(Error::LocalAuthority)
    ));
    let child = f.child.clone();
    let at = next.period_start_ms + 1;
    f.registry
        .0
        .store
        .call(4096, move |db| {
            assert!(current(&db.db, &child, at)?.is_none());
            Ok(())
        })
        .await
        .unwrap();
    let mut e = enrollment(&f);
    e.profile = ProfileAuthority::new(
        "b".repeat(64),
        f.profile.account.clone(),
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let d = VerifiedManagedAgentPeriodDelivery::new(
        e,
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            assert!(matches!(apply(&db.db, &d, at), Err(Error::LocalAuthority)));
            Ok(())
        })
        .await
        .unwrap();
    let child = f.child.clone();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            db.db.execute(
                "UPDATE owner_managed_agents SET state='fenced',permanent_fence=1 WHERE id=?1",
                [child],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    assert!(matches!(
        apply_at(&f, c, at).await,
        Err(Error::LocalAuthority)
    ));
}

#[tokio::test]
async fn recurring_native_recovery_appends_generation_preserving_old_fence() {
    let f = fixture(RenewalMode::Owner).await;
    let fence = ManagedAgentFenceCommand {
        version: 1,
        capability: AGENT_FENCE_CAPABILITY.into(),
        operation: "agent.fence".into(),
        command_id: "old_generation_fence".into(),
        reserve_command: f.command.clone(),
        reserve_command_digest: digest(&f.command),
        issued_at_ms: graphs::now_ms().unwrap(),
    };
    let old = f
        .registry
        .apply_managed_agent_fence(
            VerifiedManagedAgentFence::for_pool(
                enrollment(&f),
                fence.clone(),
                digest(&fence),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old.state, "fenced");
    let mut c = f.command.clone();
    c.command_id = "recovered_reserve".into();
    c.request_id = "recovered_request".into();
    c.allocation_id = c.request_id.clone();
    c.allocation_generation = Some(2);
    let refused = f
        .registry
        .reserve_managed_agent(
            VerifiedManagedAgentDelivery::for_pool(
                enrollment(&f),
                c.clone(),
                digest(&c),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.reason.as_deref(), Some("period_already_authorized"));
    let next = next_grant(
        &f,
        f.grant.clone(),
        f.grant.period_end_ms + 1,
        "recovery_next",
    )
    .await;
    c.command_id = "recovered_next_reserve".into();
    c.pool_grant = Some(next.clone());
    c.grant_receipt_digest = digest(&next);
    c.period_key = next.period_key.clone();
    c.authority_revision = next.grant_revision;
    c.issued_at_ms = next.period_start_ms + 1;
    c.expires_at_ms = next.period_end_ms;
    let d = VerifiedManagedAgentDelivery::for_pool(
        enrollment(&f),
        c.clone(),
        digest(&c),
        true,
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    let r = f.registry.0.clone();
    let at = next.period_start_ms + 1;
    let new = f
        .registry
        .0
        .store
        .call(32768, move |db| db.owner_managed_agent_reserve_at(d, r, at))
        .await
        .unwrap();
    assert_eq!(new.state, "reserved", "{:?}", new.reason);
    assert_ne!(new.agent_reservation_id, old.agent_reservation_id);
    assert_eq!(new.agent_id, old.agent_id);
    let old_child = f.child.clone();
    let new_child = new.agent_reservation_id.unwrap();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            let old: bool = db.db.query_row(
                "SELECT permanent_fence FROM owner_managed_agents WHERE id=?1",
                [old_child],
                |r| r.get(0),
            )?;
            let new: bool = db.db.query_row(
                "SELECT permanent_fence FROM owner_managed_agents WHERE id=?1",
                [new_child],
                |r| r.get(0),
            )?;
            assert!(old);
            assert!(!new);
            assert!(
                db.db
                    .execute("DELETE FROM owner_managed_fences", [])
                    .is_err()
            );
            Ok(())
        })
        .await
        .unwrap();
    let replay = f
        .registry
        .apply_managed_agent_fence(
            VerifiedManagedAgentFence::for_pool(
                enrollment(&f),
                fence.clone(),
                digest(&fence),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.fence_revision, old.fence_revision);
}

#[path = "runtime_tests.rs"]
mod runtime;

#[tokio::test]
async fn recurring_native_pending_renewal_survives_same_period_parent_amendment() {
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(graphs::now_ms().unwrap()));
    graphs::install_test_clock(&clock);
    let f = fixture_kind(RenewalMode::Automatic, true, Some(clock.clone())).await;
    clock.store(
        f.grant.period_end_ms + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    let next = next_grant(
        &f,
        f.grant.clone(),
        graphs::now_ms().unwrap(),
        "pending_parent",
    )
    .await;
    let original = renewal(&f, next.clone(), "queued_period_renew");
    let amended = f
        .registry
        .prepare_capacity_grant_successor(
            enrollment(&f),
            next,
            "amendment",
            OwnerGrantSelection { renewal_mode: None, per_request_tokens: None,
                calendar: f.grant.calendar.clone(),
                command_id: "amend_pending_consent".into(),
                grant_id: "amended_parent".into(),
                coordinator_mxid: f.profile.account.clone(),
                delegation_expires_at_ms: f.grant.delegation_expires_at_ms,
                allow_self_approval: false,
                eligible_managers: f.grant.eligible_managers.clone(),
                allocated_tokens: 650,
            },
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    f.registry
        .confirm_capacity_grant_succession(
            f.profile.clone(),
            amended.executor_device_id.clone(),
            amended.grant_id.clone(),
        )
        .await
        .unwrap();
    let receipt = f
        .registry
        .renew_managed_agent_period(
            VerifiedManagedAgentPeriodDelivery::new(
                enrollment(&f),
                original.clone(),
                digest(&original),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    receipt.validate_for(&original).unwrap();
    assert_eq!(receipt.allocated_tokens, 200);
    let child = f.child.clone();
    let at = graphs::now_ms().unwrap();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            assert_eq!(
                grant_usage(&db.db, &child, &amended.grant_id, at)?,
                (200, 0, true)
            );
            Ok(())
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn recurring_native_regrant_uses_resource_calendar_and_unknown_provision_keeps_old_credit() {
    let clock = Arc::new(std::sync::atomic::AtomicU64::new(graphs::now_ms().unwrap()));
    graphs::install_test_clock(&clock);
    let f = fixture_kind(RenewalMode::Automatic, false, Some(clock.clone())).await;
    let read = || {
        f.registry.regrant_prior_grants(
            f.profile.clone(),
            f.grant.executor_device_id.clone(),
            f.grant.contribution_id.clone(),
            f.grant.admission_request_id.clone().unwrap(),
        )
    };
    assert!(read().await.is_err(), "same period cannot refill");
    let ManagedProvisionClaim::Issued(_unknown) = f
        .registry
        .claim_managed_provision(assignment(&f))
        .await
        .unwrap()
    else {
        panic!("first provision")
    };
    clock.store(
        f.grant.period_end_ms + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    assert_eq!(read().await.unwrap(), vec![f.grant.grant_id.clone()]);
    let next = next_grant(
        &f,
        f.grant.clone(),
        graphs::now_ms().unwrap(),
        "unknown_provision_next",
    )
    .await;
    let old = f.grant.grant_id.clone();
    f.registry
        .0
        .store
        .call(1024, move |db| {
            let floor = accounting::period_floor(&db.db, &old, graphs::now_ms()?)?;
            assert_eq!(floor.total.retained, 200);
            assert!(!floor.total.complete);
            Ok(())
        })
        .await
        .unwrap();
    let c = renewal(&f, next, "unknown_provision_renew");
    f.registry
        .renew_managed_agent_period(
            VerifiedManagedAgentPeriodDelivery::new(
                enrollment(&f),
                c.clone(),
                digest(&c),
                true,
                Instant::now() + Duration::from_secs(20),
            )
            .unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn recurring_native_dashboard_lists_direct_pool_and_separates_old_uncertainty(){
    let clock=Arc::new(std::sync::atomic::AtomicU64::new(graphs::now_ms().unwrap()));graphs::install_test_clock(&clock);
    let f=fixture_kind(RenewalMode::Automatic,true,Some(clock.clone())).await;
    f.registry.prepare_managed_call(call(&f,"dashboard_spent",50)).await.unwrap();
    let ManagedCallClaim::Issued(spent)=f.registry.issue_managed_call(call(&f,"dashboard_spent",50)).await.unwrap() else {panic!("call")};
    f.registry.record_managed_usage(&spent,runtime::observed("dashboard_used",50)).await.unwrap();
    f.registry.prepare_managed_call(call(&f,"dashboard_unknown",120)).await.unwrap();
    let ManagedCallClaim::Issued(_unknown)=f.registry.issue_managed_call(call(&f,"dashboard_unknown",120)).await.unwrap() else {panic!("call")};
    clock.store(f.grant.period_end_ms+1,std::sync::atomic::Ordering::SeqCst);
    let next=next_grant(&f,f.grant.clone(),graphs::now_ms().unwrap(),"dashboard_next").await;
    let c=renewal(&f,next.clone(),"dashboard_renew");
    f.registry.renew_managed_agent_period(VerifiedManagedAgentPeriodDelivery::new(enrollment(&f),c.clone(),digest(&c),true,Instant::now()+Duration::from_secs(20)).unwrap()).await.unwrap();
    let profile=f.profile.digest.clone();let request=f.command.request_id.clone();
    f.registry.0.store.call(4096,move|db|{
        let page=db.management_web_managed_usage(&profile)?;assert_eq!(page.items.len(),1);
        let row=&page.items[0];assert_eq!(row.period_key,next.period_key);
        assert_eq!((row.allocated_tokens,row.known_consumed_tokens,row.retained_tokens,row.prior_retained_tokens,row.call_count),(200,0,0,0,0));assert!(!row.usage_complete);
        let link=db.management_web_managed_link(&profile,&request)?.unwrap();assert_eq!(link.period_key,next.period_key);assert_eq!((link.known_consumed_tokens,link.prior_retained_tokens),(0,0));assert!(!link.usage_complete);
        assert!(db.management_web_managed_usage(&"b".repeat(64))?.items.is_empty());
        Ok(())
    }).await.unwrap();
}

#[tokio::test]
async fn recurring_native_unstarted_agent_renews_before_first_process_claim(){
    use shared_execution::*;
    let clock=Arc::new(std::sync::atomic::AtomicU64::new(graphs::now_ms().unwrap()));graphs::install_test_clock(&clock);
    let f=fixture_kind(RenewalMode::Automatic,false,Some(clock.clone())).await;
    let child=f.child.clone();f.registry.0.store.call(1024,move|db|{assert!(!db.db.query_row("SELECT EXISTS(SELECT 1 FROM owner_managed_assignment_pins WHERE child_id=?1)",[child],|r|r.get::<_,bool>(0))?);Ok(())}).await.unwrap();
    let saved=f.registry.recover_managed_agent_reservation(f.profile.clone(),f.grant.executor_device_id.clone(),f.command.command_id.clone(),digest(&f.command)).await.unwrap().unwrap();
    clock.store(f.grant.period_end_ms+1,std::sync::atomic::Ordering::SeqCst);
    let next=next_grant(&f,f.grant.clone(),graphs::now_ms().unwrap(),"unstarted_next").await;
    let c=renewal(&f,next,"unstarted_renew");
    let proof=||VerifiedManagedAgentPeriodDelivery::new(enrollment(&f),c.clone(),digest(&c),true,Instant::now()+Duration::from_secs(20)).unwrap();
    let first=f.registry.renew_managed_agent_period(proof()).await.unwrap();
    assert_eq!(digest(&first),digest(&f.registry.renew_managed_agent_period(proof()).await.unwrap()));
    let now=graphs::now_ms().unwrap();
    let runtime=SharedRuntimeCommand{version:1,capability:SHARED_EXECUTION_CAPABILITY.into(),operation:"agent.execution.provision".into(),command_id:"unstarted_first_runtime".into(),assignment_id:"period_assignment".into(),assignment_generation:1,agent_reservation_id:f.child.clone(),reserve_command:f.command.clone(),reserve_command_digest:digest(&f.command),reserve_receipt_digest:digest(&saved),policy:SHARED_TEXT_POLICY.into(),issued_at_ms:now,expires_at_ms:now+20_000};
    assert!(matches!(f.registry.claim_shared_runtime(VerifiedSharedRuntimeDelivery::new(enrollment(&f),runtime.clone(),digest(&runtime),true,Instant::now()+Duration::from_secs(20)).unwrap()).await.unwrap(),SharedRuntimeClaim::Issued(_)));
    clock.store(runtime.expires_at_ms+1,std::sync::atomic::Ordering::SeqCst);
    assert!(f.registry.record_unstarted_shared_runtime(ManagedCleanupAuthority::fixture(enrollment(&f),f.child.clone(),Instant::now()+Duration::from_secs(20)),runtime).await.unwrap().is_none(),"issued unknown custody is not non-issuance");
    let child=f.child.clone();f.registry.0.store.call(1024,move|db|{let pin:(String,u64)=db.db.query_row("SELECT assignment_id,assignment_generation FROM owner_managed_assignment_pins WHERE child_id=?1",[child],|r|Ok((r.get(0)?,r.get(1)?)))?;assert_eq!(pin,("period_assignment".into(),1));Ok(())}).await.unwrap();
}
