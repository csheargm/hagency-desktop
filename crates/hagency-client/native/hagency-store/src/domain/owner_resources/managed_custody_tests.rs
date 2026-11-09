use super::*;
#[path = "managed_settlement_tests.rs"]
pub(super) mod settlement_tests;
#[cfg(feature = "agent-budget")]
#[path = "managed_budget_tests.rs"]
mod budget_tests;
use hagency_metering::{
    observation::UsageObservation,
    runtime_usage::{CodexUsage, CounterBreakdown, ProjectionDiagnostics},
};

fn assignment(f: &Fixture, child: &str) -> ManagedAssignmentAuthority {
    ManagedAssignmentAuthority::fixture(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        child.into(),
        "central_assignment".into(),
        1,
        Instant::now() + Duration::from_secs(20),
    )
}
fn clean(f: &Fixture, child: &str, generation: u64) -> ManagedCleanupAuthority {
    ManagedCleanupAuthority::fixture(
        enrollment_from(
            &f.profile,
            &f.resource,
            &f.grant.resource_revision,
            generation,
        ),
        child.into(),
        Instant::now() + Duration::from_secs(20),
    )
}
fn call(f: &Fixture, child: &str, id: &str, estimate: u64) -> ManagedCallAuthority {
    ManagedCallAuthority::fixture(assignment(f, child), id.into(), "c".repeat(64), estimate)
}
fn reported(id: &str, total: u64, terminal: bool, complete: bool) -> ManagedUsageObservation {
    let counters = CounterBreakdown {
        total_tokens: Some(total),
        input_tokens: Some(total),
        cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0),
        output_tokens: Some(0),
        reasoning_output_tokens: Some(0),
    };
    let usage = UsageObservation::codex_runtime(CodexUsage {
        total: counters,
        last: counters,
        context_window: Some(100_000),
        diagnostics: ProjectionDiagnostics::default(),
    })
    .unwrap();
    ManagedUsageObservation::fixture(id, Some(usage), terminal, complete, false)
}
async fn native_child(f: &Fixture) -> String {
    reserve(f, agent(f, "native", 150))
        .await
        .agent_reservation_id
        .unwrap()
}
async fn qualify(f: &Fixture, child: &str) {
    let scope = match f
        .registry
        .claim_managed_provision(assignment(f, child))
        .await
        .unwrap()
    {
        ManagedProvisionClaim::Issued(s) => s,
        _ => panic!("first provision did not issue"),
    };
    let result = f
        .registry
        .record_managed_provision(
            &scope,
            ManagedProvisionObservation::fixture("qualified", "qualified", Some("a".repeat(64))),
        )
        .await
        .unwrap();
    assert_eq!(result.state, "qualified");
}
async fn issue(f: &Fixture, child: &str, id: &str, estimate: u64) -> ManagedCallCustody {
    f.registry
        .prepare_managed_call(call(f, child, id, estimate))
        .await
        .unwrap();
    match f
        .registry
        .issue_managed_call(call(f, child, id, estimate))
        .await
        .unwrap()
    {
        ManagedCallClaim::Issued(s) => s,
        _ => panic!("first call did not issue"),
    }
}

#[tokio::test]
async fn managed_custody_provision_is_one_shot_and_results_are_exact() {
    let f = fixture().await;
    let child = native_child(&f).await;
    let scope = match f
        .registry
        .claim_managed_provision(assignment(&f, &child))
        .await
        .unwrap()
    {
        ManagedProvisionClaim::Issued(s) => s,
        _ => panic!("first claim"),
    };
    assert!(matches!(
        f.registry
            .claim_managed_provision(assignment(&f, &child))
            .await
            .unwrap(),
        ManagedProvisionClaim::History(_)
    ));
    let first = f
        .registry
        .record_managed_provision(
            &scope,
            ManagedProvisionObservation::fixture("qual", "qualified", Some("a".repeat(64))),
        )
        .await
        .unwrap();
    let replay = f
        .registry
        .record_managed_provision(
            &scope,
            ManagedProvisionObservation::fixture("qual", "qualified", Some("a".repeat(64))),
        )
        .await
        .unwrap();
    assert_eq!(first.ledger_revision, replay.ledger_revision);
    assert!(matches!(
        f.registry
            .record_managed_provision(
                &scope,
                ManagedProvisionObservation::fixture("qual", "qualified", Some("b".repeat(64)))
            )
            .await,
        Err(Error::Conflict)
    ));
    let replacement = ManagedAssignmentAuthority::fixture(
        enrollment_from(&f.profile, &f.resource, &f.grant.resource_revision, 1),
        child.clone(),
        "replacement".into(),
        1,
        Instant::now() + Duration::from_secs(20),
    );
    assert!(matches!(
        f.registry.claim_managed_provision(replacement).await,
        Err(Error::Conflict)
    ));
    assert_eq!(
        f.registry
            .managed_provision_history(clean(&f, &child, 1))
            .await
            .unwrap()
            .state,
        "qualified"
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_restart_returns_provision_history_and_late_cleanup_only() {
    let mut f = fixture().await;
    let child = native_child(&f).await;
    let scope = match f
        .registry
        .claim_managed_provision(assignment(&f, &child))
        .await
        .unwrap()
    {
        ManagedProvisionClaim::Issued(s) => s,
        _ => panic!("first claim"),
    };
    let target = scope.effect_id().to_owned();
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    assert!(
        matches!(f.registry.claim_managed_provision(assignment(&f,&child)).await.unwrap(),ManagedProvisionClaim::History(h) if h.state=="issued")
    );
    assert!(
        f.registry
            .record_managed_provision(
                &scope,
                ManagedProvisionObservation::fixture(
                    "old_handle",
                    "qualified",
                    Some("a".repeat(64))
                )
            )
            .await
            .is_err()
    );
    let interrupted = f
        .registry
        .fence_managed_agent(clean(&f, &child, 2), "interrupted_provision".into())
        .await
        .unwrap();
    assert!(!interrupted.usage_complete);
    let late = f
        .registry
        .record_late_managed_provision(
            clean(&f, &child, 2),
            target,
            ManagedProvisionObservation::fixture("late_qual", "qualified", Some("a".repeat(64))),
        )
        .await
        .unwrap();
    assert_eq!(late.state, "qualified");
    f.registry
        .fence_managed_agent(clean(&f, &child, 2), "retire".into())
        .await
        .unwrap();
    assert!(
        f.registry
            .prepare_managed_call(call(&f, &child, "forbidden", 1))
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_call_preparation_and_issue_preserve_capacity_and_restart_history() {
    let mut f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let prepared = f
        .registry
        .prepare_managed_call(call(&f, &child, "one", 100))
        .await
        .unwrap();
    assert_eq!(prepared.state, "prepared");
    let replay = f
        .registry
        .prepare_managed_call(call(&f, &child, "one", 100))
        .await
        .unwrap();
    assert_eq!(prepared.prepare_revision, replay.prepare_revision);
    assert!(matches!(
        f.registry
            .prepare_managed_call(call(&f, &child, "one", 101))
            .await,
        Err(Error::Conflict)
    ));
    // An old estimate is not a reservation or an admission bound.
    assert!(f.registry.prepare_managed_call(call(&f, &child, "two", 151)).await.is_ok());
    let (a, b) = tokio::join!(
        f.registry.issue_managed_call(call(&f, &child, "one", 100)),
        f.registry.issue_managed_call(call(&f, &child, "one", 100))
    );
    assert!(matches!(
        (&a, &b),
        (
            Ok(ManagedCallClaim::Issued(_)),
            Ok(ManagedCallClaim::History(_))
        ) | (
            Ok(ManagedCallClaim::History(_)),
            Ok(ManagedCallClaim::Issued(_))
        )
    ));
    let id = prepared.call_id;
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    assert!(
        matches!(f.registry.issue_managed_call(call(&f,&child,"one",100)).await.unwrap(),ManagedCallClaim::History(h) if h.call_id==id)
    );
    assert_eq!(
        f.registry
            .managed_call_history(clean(&f, &child, 1), id)
            .await
            .unwrap()
            .state,
        "issued"
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_prepared_calls_hold_nothing_and_issue_stops_at_known_allowance() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    // A prepared call holds nothing, so a second one is admitted beside it.
    for id in ["first", "second"] {
        let prepared = f
            .registry
            .prepare_managed_call(call(&f, &child, id, 150))
            .await
            .unwrap();
        assert_eq!(prepared.state, "prepared");
    }
    let spend = issue(&f, &child, "first", 150).await;
    f.registry
        .record_managed_usage(&spend, reported("all_spent", 150, true, true))
        .await
        .unwrap();
    // Known usage now reaches the allowance, so the waiting call can't start.
    assert!(matches!(
        f.registry
            .issue_managed_call(call(&f, &child, "second", 150))
            .await,
        Err(Error::Capacity)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn ended_month_keeps_the_full_hold_of_a_child_with_an_in_flight_call() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let end = f.grant.period_end_ms;
    let scope = issue(&f, &child, "month_end", 100).await;
    f.registry
        .record_managed_usage(&scope, reported("partial", 60, false, false))
        .await
        .unwrap();
    // Unknown usage blocks new capacity in and after the month. After it, the
    // parent's own allocation no longer counts, but the project and its
    // in-flight child keep their whole hold (400), not only the 340 unresolved.
    assert_eq!(
        pool_at(&f.registry, &f.resource, end - 1).await,
        (None, 700)
    );
    assert_eq!(pool_at(&f.registry, &f.resource, end).await, (None, 400));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_cumulative_usage_is_not_added_twice_and_zero_is_explicit() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let scope = issue(&f, &child, "one", 100).await;
    let p = f
        .registry
        .record_managed_usage(&scope, reported("partial", 30, false, false))
        .await
        .unwrap();
    assert_eq!(p.known_consumed_tokens, 30);
    assert!(!p.usage_complete);
    let replay = f
        .registry
        .record_managed_usage(&scope, reported("partial", 30, false, false))
        .await
        .unwrap();
    assert_eq!(p.ledger_revision, replay.ledger_revision);
    assert!(matches!(
        f.registry
            .record_managed_usage(&scope, reported("partial", 31, false, false))
            .await,
        Err(Error::Conflict)
    ));
    let next = f
        .registry
        .record_managed_usage(&scope, reported("next", 40, false, false))
        .await
        .unwrap();
    assert_eq!(next.known_consumed_tokens, 40);
    let complete = f
        .registry
        .record_managed_usage(&scope, reported("complete", 60, true, true))
        .await
        .unwrap();
    assert_eq!(complete.known_consumed_tokens, 60);
    assert!(complete.usage_complete);
    let stale = f
        .registry
        .record_managed_usage(&scope, reported("older_partial", 20, false, false))
        .await
        .unwrap();
    assert_eq!(stale.known_consumed_tokens, 60);
    assert!(stale.usage_complete);
    let zero = issue(&f, &child, "zero", 90).await;
    let done = f
        .registry
        .record_managed_usage(&zero, reported("reported_zero", 0, true, true))
        .await
        .unwrap();
    assert_eq!(done.known_consumed_tokens, 0);
    assert!(done.usage_complete);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_unknown_late_generation_and_overrun_remain_charged() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let scope = issue(&f, &child, "one", 100).await;
    let id = scope.call_id().to_owned();
    let missing = f
        .registry
        .record_managed_usage(
            &scope,
            ManagedUsageObservation::fixture("missing", None, true, false, false),
        )
        .await
        .unwrap();
    assert_eq!(missing.state, "unknown");
    assert!(!missing.usage_complete);
    f.registry
        .fence_managed_agent(clean(&f, &child, 1), "stop".into())
        .await
        .unwrap();
    assert_eq!(
        apply(
            &f.registry,
            &f.profile,
            release(&f, "release_unknown_native")
        )
        .await
        .reason
        .as_deref(),
        Some("unsettled_usage")
    );
    let late = f
        .registry
        .record_late_managed_usage(
            clean(&f, &child, 2),
            id.clone(),
            reported("late100", 100, true, true),
        )
        .await
        .unwrap();
    assert_eq!(late.known_consumed_tokens, 100);
    assert!(late.usage_complete);
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "release_known_native"))
            .await
            .retained_tokens,
        100
    );
    let settled = parent_close(&f, "parent_close_native", None).await;
    assert_eq!(settled.retained_tokens, 100);
    f.registry
        .record_late_managed_usage(
            clean(&f, &child, 3),
            id,
            reported("late900", 900, true, true),
        )
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .resource(&f.profile, f.resource.clone())
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(100)
    );
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "overrun_native"))
            .await
            .reason
            .as_deref(),
        Some("unsettled_usage")
    );
    let latest = parent_close(&f, "parent_settle_native", Some(&settled)).await;
    assert_eq!(latest.retained_tokens, 900);
    let original = f
        .registry
        .recover_managed_observation(
            clean(&f, &child, 3),
            "late100".into(),
            late.content_digest.clone(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(original.known_consumed_tokens, 100);
    assert_eq!(original.ledger_revision, late.ledger_revision);
    assert!(matches!(
        f.registry
            .recover_managed_observation(clean(&f, &child, 3), "late100".into(), "f".repeat(64))
            .await,
        Err(Error::Conflict)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_overflow_retains_unknown_and_prepared_fence_never_issues() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let scope = issue(&f, &child, "one", 100).await;
    let bad = f
        .registry
        .record_managed_usage(
            &scope,
            ManagedUsageObservation::fixture("overflow", None, true, false, true),
        )
        .await
        .unwrap();
    assert_eq!(bad.state, "unknown");
    assert!(!bad.usage_complete);
    let later = f
        .registry
        .record_managed_usage(&scope, reported("false_zero", 0, true, true))
        .await
        .unwrap();
    assert!(!later.usage_complete);
    assert!(
        f.registry
            .resource(&f.profile, f.resource.clone())
            .await
            .unwrap()
            .budget
            .remaining_tokens
            .is_none()
    );
    f.registry.shutdown().await.unwrap();
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let prepared = f
        .registry
        .prepare_managed_call(call(&f, &child, "never_issued", 100))
        .await
        .unwrap();
    f.registry
        .fence_managed_agent(clean(&f, &child, 1), "fence_prepared".into())
        .await
        .unwrap();
    assert!(
        f.registry
            .issue_managed_call(call(&f, &child, "never_issued", 100))
            .await
            .is_err()
    );
    assert_eq!(
        f.registry
            .managed_call_history(clean(&f, &child, 1), prepared.call_id)
            .await
            .unwrap()
            .state,
        "cancelled_before_issue"
    );
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "release_never_issued"))
            .await
            .retained_tokens,
        0
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_usage_failure_rolls_back_and_exact_evidence_is_retained() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let scope = issue(&f, &child, "one", 100).await;
    f.registry.0.store.call(1024,|db| {db.db.execute_batch("CREATE TRIGGER fail_usage_observation BEFORE INSERT ON owner_managed_observations WHEN NEW.id='atomic_usage' BEGIN SELECT RAISE(ABORT,'injected observation failure'); END;")?;Ok(())}).await.unwrap();
    assert!(
        f.registry
            .record_managed_usage(&scope, reported("atomic_usage", 40, true, true))
            .await
            .is_err()
    );
    let h = f
        .registry
        .managed_call_history(clean(&f, &child, 1), scope.call_id().into())
        .await
        .unwrap();
    assert_eq!(h.state, "issued");
    assert_eq!(h.known_consumed_tokens, 0);
    f.registry
        .0
        .store
        .call(1024, |db| {
            db.db
                .execute_batch("DROP TRIGGER fail_usage_observation;")?;
            Ok(())
        })
        .await
        .unwrap();
    f.registry
        .record_managed_usage(&scope, reported("atomic_usage", 40, true, true))
        .await
        .unwrap();
    f.registry.0.store.call(1024,|db| {
        let (digest,evidence):(String,String)=db.db.query_row("SELECT content_digest,evidence FROM owner_managed_observations WHERE id='atomic_usage'",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let value:serde_json::Value=serde_json::from_str(&evidence)?;
        assert_eq!(canonical::digest(&value)?,digest);
        assert_eq!(value["observation"]["usage"]["totals"]["input"],40);
        assert!(db.db.execute("DELETE FROM owner_managed_observations",[]).is_err());
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_codex_required_fields_and_capture_keep_reservation() {
    // Missing required fields in either total or last must not release the
    // original 6-token reservation, even when all chargeable categories exist.
    for case in 0..20 {
        let f = fixture().await;
        let child = native_child(&f).await;
        qualify(&f, &child).await;
        let id = format!("required_{case}");
        let scope = issue(&f, &child, &id, 6).await;
        let counters = CounterBreakdown {
            total_tokens: Some(5),
            input_tokens: Some(3),
            cached_input_tokens: Some(0),
            cache_write_input_tokens: Some(0),
            output_tokens: Some(2),
            reasoning_output_tokens: Some(0),
        };
        let mut raw = CodexUsage {
            total: counters,
            last: counters,
            context_window: None,
            diagnostics: ProjectionDiagnostics::default(),
        };
        let mut terminal = true;
        let mut captured = true;
        if case < 12 {
            let c = if case < 6 {
                &mut raw.total
            } else {
                &mut raw.last
            };
            match case % 6 {
                0 => c.total_tokens = None,
                1 => c.input_tokens = None,
                2 => c.cached_input_tokens = None,
                3 => c.cache_write_input_tokens = None,
                4 => c.output_tokens = None,
                _ => c.reasoning_output_tokens = None,
            }
        } else {
            match case {
                12 => raw.diagnostics.invalid = true,
                13 => raw.diagnostics.unsupported = true,
                14 => raw.total.total_tokens = Some(100_000), // context reset shape
                15 => raw.last.reasoning_output_tokens = Some(3),
                16 => raw.total.cache_write_input_tokens = Some(4),
                17 => terminal = false,
                18 => captured = false,
                _ => raw.diagnostics.missing = true,
            }
        }
        let usage = UsageObservation::codex_runtime(raw).unwrap();
        let receipt = f
            .registry
            .record_managed_usage(
                &scope,
                ManagedUsageObservation::fixture(&id, Some(usage), terminal, captured, false),
            )
            .await
            .unwrap();
        assert!(
            !receipt.usage_complete,
            "incomplete case {case} released its reservation"
        );
        let history = f
            .registry
            .managed_call_history(clean(&f, &child, 1), scope.call_id().into())
            .await
            .unwrap();
        assert!(!history.usage_complete);
        assert_eq!(history.reserved_tokens, 6);
        assert!(f.registry.prepare_managed_call(call(&f, &child, "next_task", 0)).await.is_ok());
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn managed_custody_codex_optional_capacity_settles_and_reset_stays_unknown() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let scope = issue(&f, &child, "defaulted_protocol", 100).await;
    // Values here are already decoded by the runtime; the real five-field wire
    // payload is exercised by runtime and pooled-daemon integration fixtures.
    let counters = CounterBreakdown {
        total_tokens: Some(5),
        input_tokens: Some(3),
        cached_input_tokens: Some(0),
        cache_write_input_tokens: Some(0),
        output_tokens: Some(2),
        reasoning_output_tokens: Some(0),
    };
    let usage = UsageObservation::codex_runtime(CodexUsage {
        total: counters,
        last: counters,
        context_window: None,
        diagnostics: ProjectionDiagnostics::default(),
    })
    .unwrap();
    assert!(usage.incomplete()); // Numerical shape cannot grant stream authority.
    let receipt = f
        .registry
        .record_managed_usage(
            &scope,
            ManagedUsageObservation::fixture("protocol_complete", Some(usage), true, true, false),
        )
        .await
        .unwrap();
    assert_eq!(receipt.known_consumed_tokens, 5);
    assert!(receipt.usage_complete);
    let second = issue(&f, &child, "after_defaulted_protocol", 100).await;
    f.registry
        .record_managed_usage(&second, reported("high_water", 10, false, false))
        .await
        .unwrap();
    let reset = f
        .registry
        .record_managed_usage(&second, reported("reset_terminal", 5, true, true))
        .await
        .unwrap();
    assert_eq!(reset.known_consumed_tokens, 10);
    assert!(!reset.usage_complete);
    let history = f
        .registry
        .managed_call_history(clean(&f, &child, 1), second.call_id().into())
        .await
        .unwrap();
    assert_eq!(history.reserved_tokens, 100);
    assert!(!history.usage_complete);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_codex_cache_input_uses_existing_ceiling_codec_once() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let scope = issue(&f, &child, "cache", 100).await;
    let counters = CounterBreakdown {
        total_tokens: Some(110),
        input_tokens: Some(100),
        cached_input_tokens: Some(20),
        cache_write_input_tokens: Some(10),
        output_tokens: Some(10),
        reasoning_output_tokens: Some(0),
    };
    let usage = UsageObservation::codex_runtime(CodexUsage {
        total: counters,
        last: counters,
        context_window: Some(100_000),
        diagnostics: ProjectionDiagnostics::default(),
    })
    .unwrap();
    let receipt = f
        .registry
        .record_managed_usage(
            &scope,
            ManagedUsageObservation::fixture("cache_usage", Some(usage), true, true, false),
        )
        .await
        .unwrap();
    assert_eq!(receipt.known_consumed_tokens, 90);
    assert!(receipt.usage_complete);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_cleanup_survives_acl_retirement_but_foreign_installation_does_not() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let scope = issue(&f, &child, "one", 100).await;
    let other = fixture().await;
    assert!(
        other
            .registry
            .record_managed_usage(&scope, reported("foreign", 1, true, true))
            .await
            .is_err()
    );
    other.registry.shutdown().await.unwrap();
    f.registry
        .prepare_revoke(
            f.profile.clone(),
            "remove_profile_access".into(),
            f.resource.clone(),
            Instant::now() + Duration::from_secs(20),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert!(
        f.registry
            .prepare_managed_call(call(&f, &child, "new", 1))
            .await
            .is_err()
    );
    let late = ManagedCleanupAuthority::fixture(
        enrollment_from(&f.profile, &f.resource, &"e".repeat(64), 2),
        child.clone(),
        Instant::now() + Duration::from_secs(20),
    );
    let result = f
        .registry
        .record_late_managed_usage(
            late,
            scope.call_id().into(),
            reported("revoked_late", 100, true, true),
        )
        .await
        .unwrap();
    assert_eq!(result.known_consumed_tokens, 100);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_schema76_migration_is_atomic_and_keeps_schema75_agent_hold() {
    let mut f = fixture().await;
    let child = native_child(&f).await;
    let root = f._temp.path().join("owner");
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE IF EXISTS owner_managed_runtime_observations;DROP TABLE IF EXISTS owner_managed_runtime_stops;DROP TABLE IF EXISTS owner_managed_runtime_lifetimes;DROP TABLE IF EXISTS owner_resource_adoptions;DROP TABLE owner_managed_fences;DROP TABLE owner_managed_observations;DROP TABLE owner_managed_call_pins;DROP TABLE owner_managed_assignment_pins;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=75;CREATE TABLE owner_managed_call_pins(collision INTEGER);").unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, u64>(0))
            .unwrap(),
        75
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name='owner_managed_assignment_pins'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        db.query_row(
            "SELECT allocated_tokens FROM owner_managed_agents",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        150
    );
    db.execute_batch("DROP TABLE owner_managed_call_pins;")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    qualify(&f, &child).await;
    assert_eq!(
        f.registry
            .resource(&f.profile, f.resource.clone())
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(300)
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_queued_retirement_cannot_issue_a_prepared_call() {
    let mut f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let prepared = f
        .registry
        .prepare_managed_call(call(&f, &child, "queued", 100))
        .await
        .unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let observed = live.clone();
    f.profile = f
        .profile
        .with_native_gate(move || observed.load(Ordering::Acquire))
        .unwrap();
    let a = call(&f, &child, "queued", 100);
    let store = f.registry.0.store.clone();
    let (started, ready) = tokio::sync::oneshot::channel();
    let (unblock, blocked) = std::sync::mpsc::channel();
    let blocker = tokio::spawn(async move {
        store
            .call(1024, move |_| {
                started.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            })
            .await
            .unwrap();
    });
    ready.await.unwrap();
    let mut pending = Box::pin(f.registry.issue_managed_call(a));
    tokio::select! {biased; _=&mut pending=>panic!("issue bypassed writer"),_=tokio::task::yield_now()=>{}}
    live.store(false, Ordering::Release);
    unblock.send(()).unwrap();
    blocker.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    f.profile = authority('a');
    let history = f
        .registry
        .managed_call_history(clean(&f, &child, 2), prepared.call_id)
        .await
        .unwrap();
    assert_eq!(history.state, "prepared");
    assert!(history.issue_revision.is_none());
    f.registry
        .fence_managed_agent(clean(&f, &child, 2), "queued_cleanup".into())
        .await
        .unwrap();
    assert_eq!(
        apply(&f.registry, &f.profile, release(&f, "release_queued"))
            .await
            .retained_tokens,
        0
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_aggregate_overflow_is_a_durable_unknown_observation() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let one = issue(&f, &child, "one", 50).await;
    f.registry
        .record_managed_usage(&one, reported("one_initial", 20, true, true))
        .await
        .unwrap();
    let two = issue(&f, &child, "two", 50).await;
    f.registry
        .record_managed_usage(&two, reported("two_initial", 20, true, true))
        .await
        .unwrap();
    let late = f
        .registry
        .record_managed_usage(&one, reported("huge_late", JSON_SAFE_MAX, true, true))
        .await
        .unwrap();
    assert_eq!(late.state, "unknown");
    assert!(!late.usage_complete);
    assert!(
        f.registry
            .resource(&f.profile, f.resource.clone())
            .await
            .is_err()
    );
    let fence = f
        .registry
        .fence_managed_agent(clean(&f, &child, 2), "overflow_fence".into())
        .await
        .unwrap();
    assert_eq!(fence.state, "fenced");
    assert!(!fence.usage_complete);
    assert_eq!(fence.known_consumed_tokens, JSON_SAFE_MAX);
    f.registry
        .0
        .store
        .call(1024, |db| {
            assert!(db.db.query_row(
                "SELECT unrepresentable_usage FROM owner_managed_calls WHERE execution_id='one'",
                [],
                |r| r.get::<_, bool>(0)
            )?);
            assert_eq!(
                db.db.query_row(
                    "SELECT count(*) FROM owner_managed_observations WHERE id='huge_late'",
                    [],
                    |r| r.get::<_, u64>(0)
                )?,
                1
            );
            Ok(())
        })
        .await
        .unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn managed_custody_parent_close_and_issue_share_one_ordered_fence() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let prepared = f
        .registry
        .prepare_managed_call(call(&f, &child, "racing", 100))
        .await
        .unwrap();
    let (close, issue) = tokio::join!(
        parent_close(&f, "racing_close", None),
        f.registry
            .issue_managed_call(call(&f, &child, "racing", 100))
    );
    let history = f
        .registry
        .managed_call_history(clean(&f, &child, 2), prepared.call_id)
        .await
        .unwrap();
    match issue {
        Ok(ManagedCallClaim::Issued(_)) => {
            assert!(history.issue_revision.unwrap() < close.fence_revision)
        }
        Err(Error::LocalAuthority) => assert!(history.issue_revision.is_none()),
        _ => panic!("unexpected first issue outcome"),
    }
    assert!(
        f.registry
            .issue_managed_call(call(&f, &child, "racing", 100))
            .await
            .is_err()
    );
    assert!(close.retained_tokens >= 400);
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn resource_alert_observer_counts_canonical_call_overrun_and_retains_unknown_liability() {
    let f=fixture().await;let child=native_child(&f).await;qualify(&f,&child).await;
    let call=issue(&f,&child,"alert_overrun",100).await;
    f.registry.record_managed_usage(&call,reported("alert_usage",1200,false,false)).await.unwrap();
    let id=f.resource.clone();
    f.registry.0.store.call(4096,move|db|{
        let before=db.resource_ceiling(&id,graphs::now_ms()?)?;
        assert_eq!(before.drawn,1200);assert_eq!(before.reserved,1200);
        let observed=db.observe_resource_alert(&id,graphs::now_ms()?,|_|Ok(()))?;assert_eq!(observed.raised,1);
        let alert=db.open_ceiling_alerts(1)?.remove(0);assert_eq!(alert.detail["drawnTokens"],1200);assert_eq!(alert.detail["overByTokens"],200);
        assert_eq!(db.resource_ceiling(&id,graphs::now_ms()?)?.drawn,before.drawn);
        assert!(crate::domain::owner_resources::unsettled_resource(&db.db,&serde_json::from_str(&db.db.query_row("SELECT config FROM resources WHERE id=?1",[&id],|r|r.get::<_,String>(0))?)?)?);
        let again=db.observe_resource_alert(&id,graphs::now_ms()?,|_|Ok(()))?;assert_eq!(again.updated,1);assert_eq!(db.open_ceiling_alerts(1)?[0].occurrences,2);
        Ok(())
    }).await.unwrap();f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_reconciliation_settles_only_calls_whose_process_is_gone() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let query = InterruptedCallQuery { agent_id: "agent_native".into() };
    let custody = issue(&f, &child, "one", 100).await;
    // While custody lives the call cannot be reconciled. Unknown usage does
    // not consume a fictitious reservation or block another task.
    let page = f.registry.interrupted_calls(f.profile.clone(), query.clone()).await.unwrap();
    assert_eq!(page.calls.len(), 1);
    assert!(page.calls[0].live && !page.calls[0].usage_observed && page.calls[0].state == "issued");
    assert_eq!((page.calls[0].reserved_tokens, page.calls[0].known_consumed_tokens), (100, 0));
    assert!(f.registry.prepare_managed_call(call(&f, &child, "two", 100)).await.is_ok());
    let input = ReconcileInterruptedCall {
        command_id: "rc_one".into(),
        agent_id: "agent_native".into(),
        call_id: custody.call_id().to_owned(),
        expected_reserved_tokens: 100,
        consumed_tokens: 0,
        evidence_reference: "process ended before any model request".into(),
    };
    assert!(matches!(
        f.registry.reconcile_interrupted_call(f.profile.clone(), input.clone()).await,
        Err(Error::LocalAuthority)
    ));
    drop(custody);
    let settled = f.registry.reconcile_interrupted_call(f.profile.clone(), input.clone()).await.unwrap();
    assert_eq!(
        (settled.state.as_str(), settled.consumed_tokens, settled.released_tokens, settled.replayed),
        ("settled", 0, 100, false)
    );
    // The identical declaration replays; a different one for the same call is refused.
    let again = f.registry.reconcile_interrupted_call(f.profile.clone(), input.clone()).await.unwrap();
    assert!(again.replayed && again.observation_id == settled.observation_id);
    let mut other = input.clone();
    other.command_id = "rc_other".into();
    other.consumed_tokens = 5;
    assert!(f.registry.reconcile_interrupted_call(f.profile.clone(), other).await.is_err());
    let page = f.registry.interrupted_calls(f.profile.clone(), query.clone()).await.unwrap();
    assert!(page.calls.is_empty());
    // The allowance is usable again.
    let next = issue(&f, &child, "two", 100).await;
    drop(next);
    // Unknown identities are refused outright.
    assert!(matches!(
        f.registry
            .interrupted_calls(f.profile.clone(), InterruptedCallQuery { agent_id: "agent_missing".into() })
            .await,
        Err(Error::Conflict)
    ));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_reconciliation_never_goes_below_observed_usage() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    let custody = issue(&f, &child, "one", 100).await;
    // A terminal but incomplete capture leaves the call unknown with a lower bound.
    let receipt = f
        .registry
        .record_managed_usage(&custody, reported("partial", 40, true, false))
        .await
        .unwrap();
    assert_eq!((receipt.state.as_str(), receipt.known_consumed_tokens, receipt.usage_complete), ("unknown", 40, false));
    let call_id = custody.call_id().to_owned();
    drop(custody);
    let query = InterruptedCallQuery { agent_id: "agent_native".into() };
    let page = f.registry.interrupted_calls(f.profile.clone(), query.clone()).await.unwrap();
    assert_eq!(page.calls.len(), 1);
    assert!(!page.calls[0].live && page.calls[0].usage_observed && page.calls[0].state == "unknown");
    let declare = |command: &str, consumed: u64, expected: u64| ReconcileInterruptedCall {
        command_id: command.into(),
        agent_id: "agent_native".into(),
        call_id: call_id.clone(),
        expected_reserved_tokens: expected,
        consumed_tokens: consumed,
        evidence_reference: "provider usage page 2026-11-03".into(),
    };
    // Below the observed lower bound or a stale original estimate is refused.
    assert!(f.registry.reconcile_interrupted_call(f.profile.clone(), declare("rc_low", 20, 100)).await.is_err());
    assert!(f.registry.reconcile_interrupted_call(f.profile.clone(), declare("rc_high", 101, 101)).await.is_err());
    assert!(f.registry.reconcile_interrupted_call(f.profile.clone(), declare("rc_stale", 40, 99)).await.is_err());
    assert!(f.registry.prepare_managed_call(call(&f, &child, "two", 100)).await.is_ok());
    let settled = f.registry.reconcile_interrupted_call(f.profile.clone(), declare("rc_ok", 40, 100)).await.unwrap();
    assert_eq!((settled.consumed_tokens, settled.released_tokens, settled.replayed), (40, 60, false));
    assert!(f.registry.interrupted_calls(f.profile.clone(), query).await.unwrap().calls.is_empty());
    // 150 allocated, 40 consumed: a 100-token call fits again.
    let next = issue(&f, &child, "two", 100).await;
    drop(next);
    f.registry.shutdown().await.unwrap();
}

async fn observed_window(f: &Fixture, child: &str) -> Option<u64> {
    let child = child.to_owned();
    f.registry
        .0
        .store
        .call(4096, move |db| {
            db.db.query_row("SELECT json_extract(evidence,'$.observation.usage.runtime_evidence.usage.context_window') FROM owner_managed_observations WHERE child_id=?1 AND kind='usage' AND json_type(evidence,'$.observation.usage.runtime_evidence.usage.context_window')='integer' ORDER BY ledger_revision DESC LIMIT 1", [&child], |r| r.get::<_,u64>(0)).optional().map_err(Into::into)
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn postpaid_managed_usage_and_owner_reconciliation_charge_overrun_without_an_estimate() {
    for known in [false, true] {
        let f = fixture().await;
        let child = native_child(&f).await;
        qualify(&f, &child).await;
        let custody = issue(&f, &child, "postpaid", 0).await;
        let id = custody.call_id().to_owned();
        let receipt = f.registry.record_managed_usage(&custody,
            reported("native_usage", if known { 200 } else { 20 }, true, known),
        ).await.unwrap();
        assert_eq!(receipt.known_consumed_tokens, if known {200} else {20});
        drop(custody);
        if !known {
            let declaration = ReconcileInterruptedCall {
                command_id: "verified_overrun".into(), agent_id: "agent_native".into(), call_id: id.clone(),
                expected_reserved_tokens: 0, consumed_tokens: 200,
                evidence_reference: "offline fixture verified receipt".into(),
            };
            let settled = f.registry.reconcile_interrupted_call(f.profile.clone(), declaration.clone()).await.unwrap();
            assert_eq!((settled.consumed_tokens, settled.released_tokens), (200, 0));
            assert!(f.registry.reconcile_interrupted_call(f.profile.clone(), declaration).await.unwrap().replayed);
        }
        let history = f.registry.managed_call_history(clean(&f, &child, 1), id).await.unwrap();
        assert_eq!((history.reserved_tokens,history.known_consumed_tokens,history.usage_complete), (0,200,true));
        assert!(matches!(f.registry.prepare_managed_call(call(&f,&child,"next",0)).await,Err(Error::Capacity)));
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn shared_context_window_follows_the_latest_usage_observation_of_the_child() {
    let f = fixture().await;
    let child = native_child(&f).await;
    qualify(&f, &child).await;
    assert_eq!(observed_window(&f, &child).await, None);
    let scope = issue(&f, &child, "window_one", 100).await;
    f.registry
        .record_managed_usage(&scope, reported("window_partial", 30, false, false))
        .await
        .unwrap();
    assert_eq!(observed_window(&f, &child).await, Some(100_000));
    // Another child's reports never leak into this Agent's budget.
    assert_eq!(observed_window(&f, "someone_else").await, None);
}
