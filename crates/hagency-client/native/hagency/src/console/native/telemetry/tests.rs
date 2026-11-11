use super::*;
use hagency_metering::dashboard::ledger::{CountMode, CountScope, UsageSource};
use hagency_metering::dashboard::period::{TimeRange, UsagePeriod};
use serde_json::json;

const BUCKET: u64 = 900_000;
fn binding(id: &str, account: &str) -> ExecutionBinding {
    ExecutionBinding { execution_id: id.into(), resource_id: "same-resource".into(),
        account_id: Some(account.into()), allocation_id: None, agent_id: None,
        requester_id: None, room_id: None, runtime: Runtime::Codex, runtime_version: None,
        model: None, parent_execution_id: None, started_at_ms: 0 }
}
fn event(id: &str, n: u64, at: u64, sequence: u64) -> UsageEvent {
    UsageEvent { execution_id: id.into(), stream: "thread".into(), event_id: None,
        sequence: Some(sequence), observed_at_ms: at, measured_at_ms: None,
        mode: CountMode::Cumulative { epoch: "epoch".into() }, scope: CountScope::Own,
        counts: TokenCounts { input: Some(n), output: Some(0), cache_read: Some(0), cache_write: Some(0) },
        reasoning_output: None, complete: true, source: UsageSource::CodexRuntime }
}
fn query(start: u64, end: u64) -> UsageQuery {
    UsageQuery::range(TimeRange::new(start, end).unwrap())
}

#[test]
fn checkpoint_restart_retains_cumulative_baseline_and_duplicate_identity() {
    let dir = tempfile::tempdir().unwrap();
    let store = store::Store::open(&dir.path().join("telemetry"), 0).unwrap();
    store.put_binding(&binding("e1", "a"), 0).unwrap();
    store.put_event(&event("e1", 100, 1, 1), "epoch").unwrap();
    store.put_event(&event("e1", 150, BUCKET + 1, 2), "epoch").unwrap();
    let expected = store.load().unwrap().ledger.aggregate(&query(BUCKET, 2 * BUCKET));
    assert_eq!(expected.counts.input, Some(50));
    store.checkpoint().unwrap();
    drop(store);
    let store = store::Store::open(&dir.path().join("telemetry"), 3 * BUCKET).unwrap();
    assert_eq!(store.load().unwrap().ledger.aggregate(&query(BUCKET, 2 * BUCKET)), expected);
    store.put_event(&event("e1", 150, BUCKET + 1, 2), "epoch").unwrap();
    let duplicate = store.load().unwrap().ledger.aggregate(&query(BUCKET, 2 * BUCKET));
    assert_eq!(duplicate.counts, expected.counts);
    assert_eq!(duplicate.complete, expected.complete);
    assert_eq!(duplicate.diagnostics.duplicates, 1);
    store.put_event(&event("e1", 180, 2 * BUCKET + 1, 3), "epoch").unwrap();
    assert_eq!(store.load().unwrap().ledger.aggregate(&query(2 * BUCKET, 3 * BUCKET)).counts.input, Some(30));
}

#[test]
fn separate_quota_buckets_survive_restart_and_older_update_cannot_replace_newer() {
    let dir = tempfile::tempdir().unwrap();
    let store = store::Store::open(&dir.path().join("telemetry"), 0).unwrap();
    for (bucket, percent, at) in [("codex", 40, 10), ("review", 80, 20), ("codex", 10, 5)] {
        store.put_quota("account", CODEX_UPDATED, &json!({"rateLimits":{"limitId":bucket,"primary":{"usedPercent":percent,"windowDurationMins":300}}}), at).unwrap();
    }
    drop(store);
    let store = store::Store::open(&dir.path().join("telemetry"), 30).unwrap();
    let quotas = store.load().unwrap().quotas;
    assert_eq!(quotas.len(), 2);
    assert_eq!(quotas[0].1, CODEX_UPDATED);
    assert_eq!(quotas[0].2["rateLimits"]["primary"]["usedPercent"], 40);
    assert_eq!(quotas[1].2["rateLimits"]["limitId"], "review");
}

#[test]
fn short_unaligned_range_is_partial_instead_of_panicking_or_claiming_exact_zero() {
    let mut ledger = UsageLedger::new(0);
    ledger.bind_execution(binding("e1", "a")).unwrap();
    ledger.ingest(&event("e1", 100, 10, 1)).unwrap();
    let usage = ledger.aggregate(&query(1, 20));
    assert!(!usage.complete);
}

#[test]
fn account_history_is_not_reassigned_when_one_resource_switches_accounts() {
    let telemetry = OwnerTelemetry::new(0);
    {
        let mut inner = telemetry.lock().unwrap();
        for (execution, account, n) in [("old", "account-a", 100), ("new", "account-b", 200)] {
            inner.collector.ledger_mut().bind_execution(binding(execution, account)).unwrap();
            inner.collector.ledger_mut().ingest(&event(execution, n, 1, 1)).unwrap();
        }
    }
    let parts = telemetry.parts(BUCKET, SnapshotOptions { period: UsagePeriod::Range { start_ms: 0, end_ms: BUCKET }, utc_offset_minutes: 0 }).unwrap();
    assert_eq!(parts.resources.len(), 1);
    assert_eq!(parts.resources[0].account_id, None);
    assert_eq!(parts.resources[0].counts.input, Some(300));
    let inner = telemetry.lock().unwrap();
    let mut a = query(0, BUCKET);
    a.account_id = Some("account-a".into());
    assert_eq!(inner.collector.ledger().aggregate(&a).counts.input, Some(100));
    a.account_id = Some("account-b".into());
    assert_eq!(inner.collector.ledger().aggregate(&a).counts.input, Some(200));
}

#[test]
fn personal_runtime_categories_are_deduplicated_persisted_and_separate_from_rentals() {
    let dir = tempfile::tempdir().unwrap();
    let telemetry = OwnerTelemetry::new(0);
    telemetry.attach_store(&dir.path().join("telemetry")).unwrap();
    let at = now_ms();
    let start = at - at % BUCKET;
    for (index, runtime) in [Runtime::Codex, Runtime::Claude, Runtime::Octos].into_iter().enumerate() {
        let mut binding = binding(&format!("personal-{index}"), "unused");
        binding.account_id = None;
        binding.resource_id = "personal-agent:agent".into();
        binding.agent_id = Some("agent".into());
        binding.runtime = runtime;
        let usage = hagency_agent_local::Usage { input: 17, output: 3, cached_input: 5,
            cache_write_input: Some(2), reasoning_output: 0, accounting_version: "verified-test".into() };
        telemetry.personal_usage(&binding, &usage).unwrap();
        telemetry.personal_usage(&binding, &usage).unwrap();
    }
    drop(telemetry);
    let telemetry = OwnerTelemetry::new(at);
    telemetry.attach_store(&dir.path().join("telemetry")).unwrap();
    let aggregate = telemetry.read(|c| c.ledger().aggregate(&query(start, start + BUCKET))).unwrap();
    assert_eq!(aggregate.counts, TokenCounts { input: Some(30), output: Some(9), cache_read: Some(15), cache_write: Some(6) });
    assert_eq!(aggregate.executions, 3);
    let rows = telemetry.personal_resources(at, SnapshotOptions {
        period: UsagePeriod::Range { start_ms: start, end_ms: start + BUCKET }, ..Default::default()
    }).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "personal-agent:agent");
    assert_eq!(rows[0].provider, "mixed");
    assert_eq!(rows[0].account_id, None);
    assert_eq!(rows[0].budget_tokens, None);
}

#[test]
fn pooled_claude_calls_reach_the_pool_resource_once_and_survive_restart() {
    use shared_claude::pooled_claude_binding;
    assert!(capabilities().iter().any(|c| c.runtime == Runtime::Claude
        && c.path == ExecutionPath::ManagedRental
        && c.usage == Support::Collected));
    let dir = tempfile::tempdir().unwrap();
    // A store that has covered the whole period: no coverage gap in the reads below.
    drop(store::Store::open(&dir.path().join("telemetry"), 0).unwrap());
    let telemetry = OwnerTelemetry::new(0);
    telemetry.attach_store(&dir.path().join("telemetry")).unwrap();
    let at = now_ms();
    let start = at - at % BUCKET;
    let options = SnapshotOptions {
        period: UsagePeriod::Range { start_ms: start, end_ms: start + BUCKET }, ..Default::default()
    };
    let usage = hagency_agent_local::Usage { input: 17, output: 3, cached_input: 5,
        cache_write_input: Some(2), reasoning_output: 0, accounting_version: "claude-reported-v1".into() };
    let measured = pooled_claude_binding("pool-claude", "allocation", "agent", "@user:test", "!room:test", "call-1", "claude-test");
    assert_eq!(measured.execution_id, "shared:call-1");
    assert_eq!(measured.account_id, None);
    telemetry.pooled_claude_call(measured.clone(), Some(&usage)).unwrap();
    // Read at the end of the period: an open period is partial by definition.
    let pool = |telemetry: &OwnerTelemetry| telemetry.parts(start + BUCKET, options).unwrap().resources.into_iter()
        .find(|resource| resource.resource_id == "pool-claude").unwrap();
    let row = pool(&telemetry);
    assert_eq!(row.counts, TokenCounts { input: Some(10), output: Some(3), cache_read: Some(5), cache_write: Some(2) });
    assert_eq!((row.executions, row.state, row.account_id), (1, ResourceState::Measured, None));
    // The same call recorded again adds nothing.
    telemetry.pooled_claude_call(measured.clone(), Some(&usage)).unwrap();
    let row = pool(&telemetry);
    assert_eq!(row.counts, TokenCounts { input: Some(10), output: Some(3), cache_read: Some(5), cache_write: Some(2) });
    assert_eq!(row.executions, 1);
    // A call whose receipt does not hold complete usage is listed, never counted as zero.
    let unknown = pooled_claude_binding("pool-claude", "allocation", "agent", "@user:test", "!room:test", "call-2", "claude-test");
    telemetry.pooled_claude_call(unknown, None).unwrap();
    let row = pool(&telemetry);
    assert_eq!(row.counts, TokenCounts { input: Some(10), output: Some(3), cache_read: Some(5), cache_write: Some(2) });
    assert_eq!((row.executions, row.state), (2, ResourceState::Partial));
    drop(telemetry);
    let telemetry = OwnerTelemetry::new(at);
    telemetry.attach_store(&dir.path().join("telemetry")).unwrap();
    let row = pool(&telemetry);
    assert_eq!(row.counts.input, Some(10));
    assert_eq!((row.executions, row.state), (2, ResourceState::Partial));
    let report = telemetry.usage_report(start + BUCKET, options, None).unwrap();
    assert!(report.by_model.iter().any(|(model, usage)| model.as_deref() == Some("claude-test") && usage.counts.output == Some(3)));
    assert!(report.by_requester.iter().any(|(requester, _)| requester.as_deref() == Some("@user:test")));
}

#[test]
fn transient_storage_failure_retains_measurements_for_ordered_retry() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("telemetry");
    drop(store::Store::open(&folder,0).unwrap());
    let telemetry = OwnerTelemetry::new(0);
    telemetry.attach_store(&folder).unwrap();
    let mut execution = binding("e1", "a");
    execution.account_id = None;
    telemetry.bind_execution(execution).unwrap();
    let db = rusqlite::Connection::open(folder.join(STORE_FILE)).unwrap();
    db.execute_batch("ALTER TABLE usage_events RENAME TO unavailable_events").unwrap();
    telemetry.lock().unwrap().event(event("e1", 100, 1, 1), "epoch").unwrap();
    assert_eq!(telemetry.lock().unwrap().unsaved.len(), 1);
    assert!(telemetry.status().failures > 0);
    db.execute_batch("ALTER TABLE unavailable_events RENAME TO usage_events").unwrap();
    telemetry.lock().unwrap().event(event("e1", 150, 2, 2), "epoch").unwrap();
    assert!(telemetry.lock().unwrap().unsaved.is_empty());
    let options=SnapshotOptions{period:UsagePeriod::Range{start_ms:0,end_ms:BUCKET},..Default::default()};
    assert!(telemetry.usage_report(BUCKET,options,None).unwrap().total.complete,"a successfully retried write does not taint later totals forever");
    drop(db);
    drop(telemetry);
    let reopened = store::Store::open(&folder, 3).unwrap();
    assert_eq!(reopened.load().unwrap().ledger.aggregate(&query(0, BUCKET)).counts.input, Some(150));
}

#[test]
fn rejected_personal_observation_degrades_every_usage_projection() {
    let telemetry=OwnerTelemetry::new(0);
    let mut good=binding("accepted","unused");good.account_id=None;
    let usage=hagency_agent_local::Usage {input:10,output:2,cached_input:0,cache_write_input:Some(0),reasoning_output:0,accounting_version:"verified-test".into()};
    telemetry.personal_usage(&good,&usage).unwrap();
    let start = now_ms() / BUCKET * BUCKET;
    let options=SnapshotOptions{period:UsagePeriod::Range { start_ms: start, end_ms: start + BUCKET },..Default::default()};
    assert!(telemetry.usage_report(start + BUCKET,options,None).unwrap().total.complete);
    let mut invalid=good;invalid.execution_id="rejected".into();invalid.resource_id.clear();
    assert!(telemetry.personal_usage(&invalid,&usage).is_err());
    assert!(telemetry.status().failures>0);
    let report=telemetry.usage_report(start + BUCKET,options,None).unwrap();
    assert_eq!(report.total.counts.input,Some(10));
    assert!(!report.total.complete);
    assert!(report.by_resource.iter().all(|(_,usage)| !usage.complete));
    assert!(report.by_requester.iter().all(|(_,usage)| !usage.complete));
    let parts=telemetry.parts(start + BUCKET,options).unwrap();
    assert!(!parts.usage.complete);
    assert!(parts.resources.iter().all(|row|row.state!=ResourceState::Measured));
}

#[test]
fn capacity_rollover_preserves_totals_ids_dedup_and_late_corrections_across_restart() {
    use hagency_metering::dashboard::ledger::MAX_EXECUTIONS;
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("telemetry");
    let store=store::Store::open(&path,0).unwrap();let mut ledger=UsageLedger::new(0);
    for n in 0..MAX_EXECUTIONS { ledger.bind_execution(binding(&format!("e{n:06}"),"account")).unwrap(); }
    ledger.ingest(&event("e000000",100,1,1)).unwrap();
    store.replace_checkpoint(&ledger).unwrap();drop(store);drop(ledger);
    let telemetry=OwnerTelemetry::new(0);telemetry.attach_store(&path).unwrap();
    let mut next=binding("new-after-cap","account");next.account_id=None;telemetry.bind_execution(next).unwrap();
    telemetry.0.lock().unwrap().event(event("new-after-cap",25,2,1),"epoch").unwrap();
    telemetry.finish_execution("new-after-cap");
    let options=SnapshotOptions{period:UsagePeriod::Range{start_ms:0,end_ms:BUCKET},utc_offset_minutes:0};
    let report=telemetry.usage_report(BUCKET,options,None).unwrap();
    assert_eq!(report.total.counts.input,Some(125));
    assert_eq!(report.total.executions,MAX_EXECUTIONS as u64+1);
    assert_eq!(report.by_resource[0].1.counts.input,Some(125));
    assert_eq!(telemetry.parts(BUCKET,options).unwrap().usage.counts.input,Some(125));
    // The old immutable binding lives in its original shard and cannot be
    // reassigned to today's account or duplicated by re-delivery.
    assert!(!telemetry.bind_execution(binding("e000000","account")).unwrap());
    assert!(telemetry.bind_execution(binding("e000000","other-account")).is_err());
    assert_eq!(telemetry.0.lock().unwrap().event(event("e000000",100,1,1),"epoch").unwrap(),IngestOutcome::Duplicate);
    telemetry.0.lock().unwrap().event(event("e000000",110,3,2),"epoch").unwrap();
    assert_eq!(telemetry.usage_report(BUCKET,options,None).unwrap().total.counts.input,Some(135));
    drop(telemetry);
    let restored=OwnerTelemetry::new(0);restored.attach_store(&path).unwrap();
    let report=restored.usage_report(BUCKET,options,None).unwrap();
    assert_eq!(report.total.counts.input,Some(135));
    assert_eq!(report.total.executions,MAX_EXECUTIONS as u64+1);
    assert_eq!(restored.status().executions,MAX_EXECUTIONS as u64+1);
    assert!(!report.total.complete,"unmeasured executions stay unknown/partial");
}

#[test]
fn archived_family_contains_late_children_and_range_queries_skip_old_shards() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("telemetry");
    let store=store::Store::open(&path,0).unwrap();let mut old=UsageLedger::new(0);
    old.bind_execution(binding("parent","a")).unwrap();
    let mut parent=event("parent",100,1,1);parent.scope=CountScope::IncludesSubagents;old.ingest(&parent).unwrap();
    let mut child=binding("child","a");child.parent_execution_id=Some("parent".into());
    old.bind_execution(child).unwrap();old.ingest(&event("child",20,1,1)).unwrap();
    let archived=old.split_inactive(0,&Default::default());
    assert_eq!(archived.execution_count(),2);assert_eq!(old.execution_count(),0);
    store.archive(&archived,&old,false).unwrap();
    let mut visited=0;store.visit_archives(TimeRange::new(BUCKET,2*BUCKET).unwrap(),|_|visited+=1).unwrap();assert_eq!(visited,0);
    let mut late=binding("late-child","a");late.parent_execution_id=Some("parent".into());
    store.put_binding(&late,0).unwrap();store.put_event(&event("late-child",30,2,1),"epoch").unwrap();
    let mut count=0;store.visit_archives(TimeRange::new(0,BUCKET).unwrap(),|ledger| {
        let total=ledger.aggregate(&query(0,BUCKET));assert_eq!(total.counts.input,Some(100));assert_eq!(total.contained_executions,2);count+=1;
    }).unwrap();assert_eq!(count,1);
}

#[test]
fn archived_snapshot_is_pinned_before_a_concurrent_rollover() {
    let dir=tempfile::tempdir().unwrap();let store=store::Store::open(&dir.path().join("telemetry"),0).unwrap();
    let reader=store.reader().unwrap();
    let mut ledger=UsageLedger::new(0);ledger.bind_execution(binding("e","a")).unwrap();
    store.archive(&ledger,&UsageLedger::new(0),false).unwrap();
    let mut count=0;reader.visit_archives(TimeRange::ALL,|_|count+=1).unwrap();assert_eq!(count,0);
    store.visit_archives(TimeRange::ALL,|_|count+=1).unwrap();assert_eq!(count,1);
}

#[test]
fn archived_write_retry_preserves_cross_bucket_cumulative_order() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("telemetry");
    let store=store::Store::open(&path,0).unwrap();let mut old=UsageLedger::new(0);old.bind_execution(binding("cold","a")).unwrap();
    store.archive(&old,&UsageLedger::new(0),false).unwrap();drop(store);
    let telemetry=OwnerTelemetry::new(0);telemetry.attach_store(&path).unwrap();
    let db=rusqlite::Connection::open(path.join(STORE_FILE)).unwrap();
    db.execute_batch("CREATE TRIGGER fail_archive_update BEFORE UPDATE ON archives BEGIN SELECT RAISE(FAIL,'temporary failure'); END;").unwrap();
    assert!(telemetry.0.lock().unwrap().event(event("cold",100,1,1),"epoch").is_err());
    assert_eq!(telemetry.0.lock().unwrap().unsaved.len(),1);
    db.execute_batch("DROP TRIGGER fail_archive_update").unwrap();
    telemetry.0.lock().unwrap().event(event("cold",150,BUCKET+1,2),"epoch").unwrap();
    assert!(telemetry.0.lock().unwrap().unsaved.is_empty());
    for (start,expected) in [(0,100),(BUCKET,50)] {
        let options=SnapshotOptions{period:UsagePeriod::Range{start_ms:start,end_ms:start+BUCKET},..Default::default()};
        let report=telemetry.usage_report(2*BUCKET,options,None).unwrap();assert_eq!(report.total.counts.input,Some(expected));assert!(report.total.complete);
    }
}

#[test]
fn sharded_lower_bounds_equal_monolithic_with_unknown_early_coverage() {
    let mut all=UsageLedger::new(1);
    all.bind_execution(binding("unmeasured","a")).unwrap();
    let mut hot=binding("measured","a");hot.started_at_ms=2;all.bind_execution(hot).unwrap();all.ingest(&event("measured",100,3,1)).unwrap();
    let original=all.aggregate(&UsageQuery::range(TimeRange::ALL));
    let archived=all.split_inactive(1,&Default::default());
    let mut merged=all.aggregate(&UsageQuery::range(TimeRange::ALL));merged.merge(&archived.aggregate(&UsageQuery::range(TimeRange::ALL)));
    assert_eq!(merged,original);assert_eq!(merged.counts.input,Some(100));assert!(!merged.complete);
}

#[test]
fn dense_history_rotates_before_the_serialized_export_limit_without_losing_totals() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("telemetry");let telemetry=OwnerTelemetry::new(0);telemetry.attach_store(&path).unwrap();
    let mut ledger=UsageLedger::new(0);
    for n in 0..1200 {
        let id=format!("dense-{n}");ledger.bind_execution(binding(&id,"a")).unwrap();
        for sequence in 1..=250 {
            let mut observation=event(&id,sequence,1,sequence);
            observation.event_id=Some(format!("{:0250}",sequence));ledger.ingest(&observation).unwrap();
        }
    }
    assert!(ledger.encoded_size()>hagency_metering::dashboard::ledger::MAX_EXPORT_BYTES);
    *telemetry.0.lock().unwrap().collector.ledger_mut()=ledger;
    let mut next=binding("after-dense-history","a");next.account_id=None;telemetry.bind_execution(next).unwrap();
    assert!(telemetry.0.lock().unwrap().collector.ledger().encoded_size()<8*1024*1024);
    let options=SnapshotOptions{period:UsagePeriod::Range{start_ms:0,end_ms:BUCKET},..Default::default()};
    assert_eq!(telemetry.usage_report(BUCKET,options,None).unwrap().total.counts.input,Some(300_000));
    let db=rusqlite::Connection::open(path.join(STORE_FILE)).unwrap();
    let max:i64=db.query_row("SELECT MAX(length(payload)) FROM archives",[],|r|r.get(0)).unwrap();assert!(max<=4*1024*1024);
    drop(telemetry);let restored=OwnerTelemetry::new(0);restored.attach_store(&path).unwrap();
    assert_eq!(restored.usage_report(BUCKET,options,None).unwrap().total.counts.input,Some(300_000));
}

#[test]
fn quality_gap_survives_restart_and_does_not_forbid_rotation_or_identity_checks() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("telemetry");let telemetry=OwnerTelemetry::new(0);telemetry.attach_store(&path).unwrap();
    let mut ledger=UsageLedger::new(0);for n in 0..4096{ledger.bind_execution(binding(&format!("e{n}"),"a")).unwrap();}
    *telemetry.0.lock().unwrap().collector.ledger_mut()=ledger;
    telemetry.0.lock().unwrap().mark_gap();
    let mut next=binding("new","a");next.account_id=None;telemetry.bind_execution(next).unwrap();
    assert!(telemetry.status().executions>4096);
    let mut bad=binding("new","other");bad.parent_execution_id=Some("e0".into());assert!(telemetry.bind_execution(bad).is_err());
    drop(telemetry);let restored=OwnerTelemetry::new(0);restored.attach_store(&path).unwrap();assert!(restored.0.lock().unwrap().history_incomplete);
}

#[test]
fn late_child_binding_survives_transient_archive_write_failure() {
    let dir=tempfile::tempdir().unwrap();let path=dir.path().join("telemetry");
    let store=store::Store::open(&path,0).unwrap();let mut cold=UsageLedger::new(0);cold.bind_execution(binding("parent","a")).unwrap();
    store.archive(&cold,&UsageLedger::new(0),false).unwrap();drop(store);
    let telemetry=OwnerTelemetry::new(0);telemetry.attach_store(&path).unwrap();
    let db=rusqlite::Connection::open(path.join(STORE_FILE)).unwrap();
    db.execute_batch("CREATE TRIGGER fail_archive_update BEFORE DELETE ON archives BEGIN SELECT RAISE(FAIL,'temporary failure'); END;").unwrap();
    let mut child=binding("child","a");child.parent_execution_id=Some("parent".into());
    assert!(telemetry.bind_execution(child.clone()).unwrap());
    assert_eq!(telemetry.0.lock().unwrap().unsaved.len(),1);
    let mut conflicting=child.clone();conflicting.agent_id=Some("other".into());assert!(telemetry.bind_execution(conflicting).is_err());
    assert!(!telemetry.bind_execution(child).unwrap());
    db.execute_batch("DROP TRIGGER fail_archive_update").unwrap();
    telemetry.0.lock().unwrap().event(event("child",42,1,1),"epoch").unwrap();
    assert!(telemetry.0.lock().unwrap().unsaved.is_empty());
    let options=SnapshotOptions{period:UsagePeriod::Range{start_ms:0,end_ms:BUCKET},..Default::default()};
    assert_eq!(telemetry.usage_report(BUCKET,options,None).unwrap().total.counts.input,Some(42));
}
