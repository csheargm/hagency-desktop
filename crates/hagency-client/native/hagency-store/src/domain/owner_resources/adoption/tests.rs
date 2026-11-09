use super::*;

// Rewind only disposable test schemas before exercising an older upgrade path.
// Reconstruct the pre-pool tables too; changing user_version alone is not an old database.
fn rewind_pool_schema(db: &Connection) {
    let expected = Connection::open_in_memory().unwrap();
    expected.execute_batch(include_str!("../../../domain.sql")).unwrap();
    for &(version, sql) in crate::domain::DOMAIN_MIGRATIONS {
        if version < 100 { expected.execute_batch(sql).unwrap(); }
    }
    let foreign: bool = db.pragma_query_value(None, "foreign_keys", |r| r.get(0)).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
    db.execute_batch("DROP TRIGGER IF EXISTS owner_managed_agent_fence_time;DROP TABLE IF EXISTS owner_capacity_grant_successions;DROP TABLE IF EXISTS owner_managed_fence_times;DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_managed_call_periods;DROP TABLE IF EXISTS owner_managed_periods;DROP TABLE IF EXISTS owner_resource_calendars;").unwrap();
    for table in ["owner_managed_agents", "owner_managed_fences", "owner_shared_work_observations", "owner_shared_start_intents"] {
        let current: String = db.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| r.get(0)).unwrap();
        let old: String = expected.query_row("SELECT sql FROM sqlite_master WHERE type='table' AND name=?1", [table], |r| r.get(0)).unwrap();
        if current == old { continue; }
        if current.contains("pool_grant_id") {
            assert_eq!(db.query_row(&format!("SELECT count(*) FROM {table} WHERE pool_grant_id IS NOT NULL"), [], |r| r.get::<_, u64>(0)).unwrap(), 0);
        }
        // Adoption compares exact trusted sqlite_schema SQL, including whitespace.
        // Restore the actual version-99 definitions instead of approximating them.
        let columns = expected.prepare(&format!("PRAGMA table_info({table})")).unwrap().query_map([], |r| r.get::<_, String>(1)).unwrap().collect::<Result<Vec<_>, _>>().unwrap().join(",");
        let objects = expected.prepare("SELECT sql FROM sqlite_master WHERE tbl_name=?1 AND type IN ('trigger','index') AND sql IS NOT NULL").unwrap().query_map([table], |r| r.get::<_, String>(0)).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        db.execute_batch(&format!("CREATE TEMP TABLE saved_{table} AS SELECT {columns} FROM {table};DROP TABLE {table};{old};INSERT INTO {table}({columns}) SELECT {columns} FROM saved_{table};DROP TABLE saved_{table};")).unwrap();
        for sql in objects { db.execute_batch(&sql).unwrap(); }
    }
    if foreign { db.execute_batch("PRAGMA foreign_keys=ON;").unwrap(); }
}
#[path = "workspace_tests.rs"]
mod workspace_tests;
use rusqlite::OpenFlags;
struct Fixture {
    _root: tempfile::TempDir,
    registry: OwnerResourceRegistry,
    source: PathBuf,
    personal: PathBuf,
    profile: ProfileAuthority,
    resource: Resource,
}
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().canonicalize().unwrap().join("source");
    let profile = ProfileAuthority::new(
        "a".repeat(64),
        "@owner:a.test".into(),
        Instant::now() + Duration::from_secs(120),
    )
    .unwrap();
    let mut db = DomainRepository::open_managed(&source, &profile.digest).unwrap();
    let resource:Resource=serde_json::from_value(json!({"presetId":"original","seatId":"seat_embedded_original","framework":"codex","model":"gpt-5.6-sol","reasoning":"medium","published":true,"ceiling":{"tokens":1000,"period":"monthly"}})).unwrap();
    db.put_seat(&Seat {
        id: resource.seat_id.clone(),
        declaration: None,
    })
    .unwrap();
    db.put_resource(&resource).unwrap();
    db.db.execute("INSERT INTO management_commands VALUES(?1,'original_command','resource.enroll',?2,?3)",params!["e".repeat(64),"b".repeat(64),serde_json::to_string(&json!({"resourceId":resource.id(),"revision":crate::resource_publication_revision(&resource).unwrap(),"published":resource.published})).unwrap()]).unwrap();
    drop(db);
    let personal = root.path().canonicalize().unwrap().join("personal.sqlite");
    crate::private::open(&personal, true).unwrap();
    drop(
        hagency_agent_local::Ledger::open_scoped(&personal, &profile.account, &profile.digest)
            .unwrap(),
    );
    let registry = OwnerResourceRegistry::open_root(&root.path().join("owner")).unwrap();
    Fixture {
        _root: root,
        registry,
        source,
        personal,
        profile,
        resource,
    }
}
fn inventory(f: &Fixture) -> Result<AdoptionInventory, Error> {
    let mut d = Connection::open_with_flags(
        f.source.join("domain.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut p = Connection::open_with_flags(&f.personal, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    AdoptionInventory::inspect(&mut d, &mut p, &f.profile.digest, &f.profile.account)
}
fn authority(f: &Fixture) -> AdoptionAuthority {
    AdoptionAuthority::new(
        f.registry.clone(),
        f.profile.clone(),
        Instant::now() + Duration::from_secs(30),
        || true,
    )
    .unwrap()
}
fn provider() -> VerifiedProviderAccount {
    VerifiedProviderAccount::new(
        "codex",
        "https://provider.test",
        "synthetic_stable_account",
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap()
}
async fn prepare(f: &Fixture, id: &str) -> Result<AdoptionSummary, Error> {
    f.registry
        .prepare_resource_adoption(
            authority(f),
            id.into(),
            provider(),
            inventory(f)?,
            "c".repeat(64),
            700,
        )
        .await
}
#[tokio::test]
async fn preparation_retains_original_definition_and_custody_without_copying_capacity_or_access() {
    let f = fixture().await;
    let before = std::fs::read(f.source.join("domain.sqlite3-wal")).unwrap();
    let value = prepare(&f, "original_transfer").await.unwrap();
    assert_eq!(value.phase, "prepared");
    assert_eq!(value.resource_ids, vec![f.resource.id()]);
    assert_eq!(value.completed_private_records, 1);
    assert!(value.introduces_shared_account_ceiling);
    assert_eq!(value.monthly_account_tokens, 700);
    assert_eq!(
        std::fs::read(f.source.join("domain.sqlite3-wal")).unwrap(),
        before
    );
    f.registry
        .0
        .store
        .call(1, |db| {
            for table in [
                "resources",
                "seats",
                "owner_resource_accounts",
                "owner_resource_grants",
                "owner_capacity_grants",
            ] {
                assert_eq!(
                    db.db
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                            .get::<_, u64>(0))?,
                    0
                );
            }
            Ok(())
        })
        .await
        .unwrap();
    let recovered = f
        .registry
        .resource_adoption(authority(&f), "original_transfer".into())
        .await
        .unwrap();
    assert_eq!(recovered.phase, "prepared");
    assert_eq!(
        prepare(&f, "original_transfer").await.unwrap().resource_ids,
        value.resource_ids
    );
    assert!(prepare(&f, "replacement_transfer").await.is_err());
}
#[tokio::test]
async fn populated_unknown_personal_and_pending_commands_are_not_empty_sources() {
    let f = fixture().await;
    let db = Connection::open(&f.personal).unwrap();
    db.execute_batch("CREATE TABLE unknown_old_usage(value TEXT); INSERT INTO unknown_old_usage VALUES('retained')").unwrap();
    assert!(inventory(&f).is_err());
    db.execute("DELETE FROM unknown_old_usage", []).unwrap();
    // Even an empty unexpected table is not this reviewed schema.
    assert!(inventory(&f).is_err());
    db.execute("DROP TABLE unknown_old_usage", []).unwrap();
    assert!(inventory(&f).is_ok());
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    d.execute("INSERT INTO management_intents VALUES('actor','uncertain','contribution.submit',?1,'{}',NULL,'pending')",["d".repeat(64)]).unwrap();
    assert!(inventory(&f).is_err());
    assert!(prepare(&f, "must_not_prepare").await.is_err());
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM owner_resource_adoptions", [], |r| r
                        .get::<_, u64>(
                        0
                    ))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
}
#[tokio::test]
async fn whole_inventory_and_existing_declared_total_must_be_preserved() {
    let f = fixture().await;
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    let mut other = f.resource.clone();
    other.preset_id = "other".into();
    other.seat_id = "another_account".into();
    d.execute(
        "INSERT INTO resources VALUES(?1,?2,?3)",
        params![
            other.id(),
            other.preset_id,
            serde_json::to_string(&other).unwrap()
        ],
    )
    .unwrap();
    assert!(inventory(&f).is_err());
    d.execute("DELETE FROM resources WHERE id=?1", [other.id()])
        .unwrap();
    d.execute(
        "UPDATE seats SET config=?1",
        [serde_json::to_string(
            &json!({"id":f.resource.seat_id,"declaration":{"quotaTokens":900,"period":"monthly"}}),
        )
        .unwrap()],
    )
    .unwrap();
    assert!(prepare(&f, "wrong_total").await.is_err());
}

#[tokio::test]
async fn substituted_views_missing_history_and_altered_custody_ddl_are_rejected() {
    for (personal, sql) in [
        (
            false,
            "ALTER TABLE resources RENAME TO hidden_resources; CREATE VIEW resources AS SELECT * FROM hidden_resources;",
        ),
        (false, "DROP TABLE owner_managed_calls;"),
        (false, "DROP TRIGGER owner_resource_adoptions_retained;"),
        (
            true,
            "ALTER TABLE identity RENAME TO hidden_identity; CREATE VIEW identity AS SELECT * FROM hidden_identity;",
        ),
        (true, "DROP TABLE calls;"),
        (
            true,
            "CREATE VIEW unexpected_history AS SELECT 1 AS liability;",
        ),
    ] {
        let f = fixture().await;
        let path = if personal {
            f.personal.clone()
        } else {
            f.source.join("domain.sqlite3")
        };
        let db = Connection::open(path).unwrap();
        db.execute_batch(sql).unwrap();
        assert!(
            matches!(inventory(&f), Err(Error::Schema)),
            "shape accepted: {sql}"
        );
        assert!(prepare(&f, "must_not_prepare_shape").await.is_err());
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn schema78_failure_rolls_back_and_reopen_retains_original_mapping_and_receipt() {
    let mut f = fixture().await;
    let receipt = f
        .registry
        .prepare_enrollment(
            f.profile.clone(),
            "original_enrollment".into(),
            provider(),
            "gpt-5.6-sol".into(),
            Some("medium".into()),
            1000,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    let root = f._root.path().join("owner");
    let original_id = f.registry.id().to_owned();
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    rewind_pool_schema(&db);
    db.execute_batch("DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE IF EXISTS owner_shared_context_call_pins;DROP TABLE IF EXISTS owner_shared_reply_deliveries;DROP TABLE IF EXISTS owner_shared_contexts;DROP TABLE IF EXISTS owner_shared_input_modes;DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE IF EXISTS owner_managed_runtime_observations;DROP TABLE IF EXISTS owner_managed_runtime_stops;DROP TABLE IF EXISTS owner_managed_runtime_lifetimes;DROP TABLE owner_resource_adoptions;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=77;CREATE TABLE owner_resource_adoptions(collision INTEGER);").unwrap();
    let original: (String,String,String) = db.query_row("SELECT (SELECT config FROM resources),(SELECT provider_digest FROM owner_resource_accounts),(SELECT result FROM owner_resource_receipts)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&root).is_err());
    let db = Connection::open(root.join("domain.sqlite3")).unwrap();
    assert_eq!(
        db.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        77
    );
    let after: (String,String,String) = db.query_row("SELECT (SELECT config FROM resources),(SELECT provider_digest FROM owner_resource_accounts),(SELECT result FROM owner_resource_receipts)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap();
    assert_eq!(original, after);
    db.execute_batch("DROP TABLE owner_resource_adoptions")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    assert_eq!(f.registry.id(), original_id);
    assert_eq!(
        f.registry
            .resource(&f.profile, receipt.resource_id)
            .await
            .unwrap()
            .budget
            .pool
            .remaining
            .map(u64::from),
        Some(1000)
    );
    f.registry.0.store.call(1, move |db| {
        assert_eq!(db.db.pragma_query_value(None,"user_version",|r|r.get::<_,i32>(0))?,crate::DOMAIN_SCHEMA_VERSION);
        let after: (String,String,String) = db.db.query_row("SELECT (SELECT config FROM resources),(SELECT provider_digest FROM owner_resource_accounts),(SELECT result FROM owner_resource_receipts)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        assert_eq!(original,after);
        Ok(())
    }).await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn original_plan_phase_and_evidence_are_permanent() {
    let f = fixture().await;
    prepare(&f, "retained_transfer").await.unwrap();
    f.registry
        .0
        .store
        .call(1, |db| {
            for sql in [
                "DELETE FROM owner_resource_adoptions",
                "UPDATE owner_resource_adoptions SET plan='{}'",
                "UPDATE owner_resource_adoptions SET phase=2,source_evidence='{}'",
            ] {
                assert!(db.db.execute(sql, []).is_err());
            }
            db.db.execute(
                "UPDATE owner_resource_adoptions SET phase=1,source_evidence='original_fence'",
                [],
            )?;
            for sql in [
                "UPDATE owner_resource_adoptions SET phase=0,source_evidence=NULL",
                "UPDATE owner_resource_adoptions SET source_evidence='replacement_fence'",
            ] {
                assert!(db.db.execute(sql, []).is_err());
            }
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .resource_adoption(authority(&f), "retained_transfer".into())
            .await
            .unwrap()
            .phase,
        "sources_fenced"
    );
    f.registry.shutdown().await.unwrap();
}

fn fenced_connections(f: &Fixture, transfer: &AdoptionTransfer) {
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    d.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    let p = Connection::open(&f.personal).unwrap();
    transfer.fixture_fence(&d, &p);
}
fn archive_proof(
    f: &Fixture,
    transfer: &AdoptionTransfer,
) -> Result<VerifiedAdoptionArchives, Error> {
    let mut d = Connection::open_with_flags(
        f.source.join("domain.sqlite3"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    let mut p = Connection::open_with_flags(&f.personal, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    transfer.inspect_archives(&mut d, &mut p)
}
#[tokio::test]
async fn inactive_import_replays_after_restart_and_cannot_be_bypassed_by_link_or_enrollment() {
    let mut f = fixture().await;
    prepare(&f, "import_original").await.unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "import_original".into())
        .await
        .unwrap();
    assert!(archive_proof(&f, &transfer).is_err());
    fenced_connections(&f, &transfer);
    assert!(
        f.registry
            .import_resource_adoption(authority(&f), archive_proof(&f, &transfer).unwrap())
            .await
            .is_err()
    );
    assert_eq!(
        f.registry
            .record_adoption_sources(authority(&f), archive_proof(&f, &transfer).unwrap())
            .await
            .unwrap()
            .phase,
        "sources_fenced"
    );
    let imported = f
        .registry
        .import_resource_adoption(authority(&f), archive_proof(&f, &transfer).unwrap())
        .await
        .unwrap();
    assert_eq!(imported.phase, "imported_inactive");
    assert_eq!(imported.resource_ids, vec![f.resource.id()]);
    assert_eq!(
        imported.resource_revisions,
        vec![crate::resource_publication_revision(&f.resource).unwrap()]
    );
    assert!(
        f.registry
            .resource(&f.profile, f.resource.id())
            .await
            .is_err()
    );
    assert!(
        f.registry
            .prepare_link(
                f.profile.clone(),
                "cannot_skip_custody".into(),
                f.resource.id(),
                Instant::now() + Duration::from_secs(20)
            )
            .unwrap()
            .approve()
            .await
            .is_err()
    );
    assert!(
        f.registry
            .prepare_enrollment(
                f.profile.clone(),
                "cannot_skip_seat_custody".into(),
                provider(),
                "other-model".into(),
                None,
                700,
                Instant::now() + Duration::from_secs(20)
            )
            .unwrap()
            .approve()
            .await
            .is_err()
    );
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM seats", [], |r| r.get::<_, u32>(0))?,
                1
            );
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, u32>(0))?,
                1
            );
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM owner_capacity_grants", [], |r| r
                        .get::<_, u32>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    let root = f._root.path().join("owner");
    f.registry.shutdown().await.unwrap();
    f.registry = OwnerResourceRegistry::open_root(&root).unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "import_original".into())
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .import_resource_adoption(authority(&f), archive_proof(&f, &transfer).unwrap())
            .await
            .unwrap()
            .phase,
        "imported_inactive"
    );
    assert!(
        f.registry
            .resource(&f.profile, f.resource.id())
            .await
            .is_err()
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn original_archive_binding_and_completed_private_receipts_are_reverified() {
    let f = fixture().await;
    prepare(&f, "archive_exact").await.unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "archive_exact".into())
        .await
        .unwrap();
    fenced_connections(&f, &transfer);
    assert!(archive_proof(&f, &transfer).is_ok());
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    d.execute("UPDATE management_commands SET result='{}'", [])
        .unwrap();
    assert!(archive_proof(&f, &transfer).is_err());
    assert_eq!(
        f.registry
            .resource_adoption(authority(&f), "archive_exact".into())
            .await
            .unwrap()
            .phase,
        "prepared"
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn import_is_atomic_when_inactive_grant_write_fails() {
    let f = fixture().await;
    prepare(&f, "atomic_import").await.unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "atomic_import".into())
        .await
        .unwrap();
    fenced_connections(&f, &transfer);
    f.registry
        .record_adoption_sources(authority(&f), archive_proof(&f, &transfer).unwrap())
        .await
        .unwrap();
    f.registry.0.store.call(1,|db|{
        db.db.execute_batch("CREATE TRIGGER failed_import BEFORE INSERT ON owner_resource_grants BEGIN SELECT RAISE(ABORT,'fixture failure'); END;")?;Ok(())
    }).await.unwrap();
    assert!(
        f.registry
            .import_resource_adoption(authority(&f), archive_proof(&f, &transfer).unwrap())
            .await
            .is_err()
    );
    assert_eq!(
        f.registry
            .resource_adoption(authority(&f), "atomic_import".into())
            .await
            .unwrap()
            .phase,
        "sources_fenced"
    );
    f.registry
        .0
        .store
        .call(1, |db| {
            for table in [
                "seats",
                "resources",
                "owner_resource_accounts",
                "owner_resource_grants",
            ] {
                assert_eq!(
                    db.db
                        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                            .get::<_, u32>(0))?,
                    0
                );
            }
            db.db.execute_batch("DROP TRIGGER failed_import")?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .import_resource_adoption(authority(&f), archive_proof(&f, &transfer).unwrap())
            .await
            .unwrap()
            .phase,
        "imported_inactive"
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn completed_unacknowledged_admission_and_scrubbed_tombstone_survive_with_original_bytes() {
    use crate::management::{IntentOperation, IntentReply};
    let f = fixture().await;
    let op = IntentOperation::Contribute;
    let input = json!({"commandId":"original_admission","resourceId":f.resource.id(),"coordinatorMxid":"@coordinator:a.test"});
    let input_bytes = serde_json::to_string(&input).unwrap();
    let reply_bytes = serde_json::to_string(&IntentReply::Success(
        json!({"admission":{"contribution":{"id":"con_original","state":"admitted"}}}),
    ))
    .unwrap();
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    d.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    d.execute(
        "INSERT INTO management_intents VALUES(?1,'original_admission',?2,?3,?4,?5,'replied')",
        params![
            "e".repeat(64),
            serde_json::to_string(&op).unwrap(),
            canonical::digest(&json!({"operation":op,"input":input})).unwrap(),
            input_bytes,
            reply_bytes
        ],
    )
    .unwrap();
    d.execute(
        "INSERT INTO management_intents VALUES(?1,'old_project',?2,?3,NULL,NULL,'acknowledged')",
        params![
            "e".repeat(64),
            serde_json::to_string(&IntentOperation::ProjectRequest).unwrap(),
            "d".repeat(64)
        ],
    )
    .unwrap();
    assert_eq!(
        prepare(&f, "completed_admission")
            .await
            .unwrap()
            .completed_private_records,
        3
    );
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "completed_admission".into())
        .await
        .unwrap();
    fenced_connections(&f, &transfer);
    f.registry
        .record_adoption_sources(authority(&f), archive_proof(&f, &transfer).unwrap())
        .await
        .unwrap();
    f.registry
        .import_resource_adoption(authority(&f), archive_proof(&f, &transfer).unwrap())
        .await
        .unwrap();
    let saved: (String, String) = d
        .query_row(
            "SELECT input,reply FROM management_intents WHERE command_id='original_admission'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(saved, (input_bytes, reply_bytes));
    assert_eq!(
        d.query_row(
            "SELECT state FROM management_intents WHERE command_id='old_project'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "acknowledged"
    );
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn unknown_completed_operations_and_mismatched_original_input_are_not_migratable() {
    use crate::management::{IntentOperation, IntentReply};
    let f = fixture().await;
    let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    d.execute(
        "INSERT INTO management_intents VALUES(?1,'unknown_old',?2,?3,NULL,NULL,'acknowledged')",
        params![
            "e".repeat(64),
            "\"unknown_future_operation\"",
            "d".repeat(64)
        ],
    )
    .unwrap();
    assert!(inventory(&f).is_err());
    d.execute("DELETE FROM management_intents", []).unwrap();
    d.execute(
        "INSERT INTO management_intents VALUES(?1,'changed_input',?2,?3,?4,?5,'replied')",
        params![
            "e".repeat(64),
            serde_json::to_string(&IntentOperation::Contribute).unwrap(),
            "d".repeat(64),
            "{\"commandId\":\"changed_input\"}",
            serde_json::to_string(&IntentReply::Success(json!({}))).unwrap()
        ],
    )
    .unwrap();
    assert!(matches!(inventory(&f), Err(Error::Quarantined)));
    f.registry.shutdown().await.unwrap();
}
#[tokio::test]
async fn queued_import_rechecks_original_source_authority_before_copying() {
    let f = fixture().await;
    prepare(&f, "queued_import").await.unwrap();
    let live = Arc::new(AtomicBool::new(true));
    let check = live.clone();
    let a = AdoptionAuthority::new(
        f.registry.clone(),
        f.profile.clone(),
        Instant::now() + Duration::from_secs(30),
        move || check.load(Ordering::Acquire),
    )
    .unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(a.clone(), "queued_import".into())
        .await
        .unwrap();
    fenced_connections(&f, &transfer);
    f.registry
        .record_adoption_sources(a.clone(), archive_proof(&f, &transfer).unwrap())
        .await
        .unwrap();
    let proof = archive_proof(&f, &transfer).unwrap();
    let store = f.registry.0.store.clone();
    let (ready, wait) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let barrier = tokio::spawn(async move {
        store
            .call(1, move |_| {
                ready.send(()).unwrap();
                blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok(())
            })
            .await
            .unwrap()
    });
    wait.await.unwrap();
    let mut pending = Box::pin(f.registry.import_resource_adoption(a, proof));
    tokio::select! {biased;_=&mut pending=>panic!("import skipped occupied writer"),_=tokio::task::yield_now()=>{}}
    live.store(false, Ordering::Release);
    release.send(()).unwrap();
    barrier.await.unwrap();
    assert!(matches!(pending.await, Err(Error::LocalAuthority)));
    f.registry
        .0
        .store
        .call(1, |db| {
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM resources", [], |r| r.get::<_, u32>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        f.registry
            .resource_adoption(authority(&f), "queued_import".into())
            .await
            .unwrap()
            .phase,
        "sources_fenced"
    );
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn original_journal_custody_precedes_activation_and_shared_seat_survives_next_model() {
    use crate::management::{
        HostContext, Intent, IntentOperation, IntentReply, ManagementAuthority, Permission,
        PrivateManagementJournal, Session,
    };
    let f = fixture().await;
    let context = HostContext::new(
        f.profile.account.clone(),
        1,
        "a".repeat(64),
        1,
        "original_device".into(),
        1,
    )
    .unwrap();
    let permissions = [
        Permission::ContributeResources,
        Permission::ReadIntents,
        Permission::AcknowledgeIntents,
    ];
    let source = DomainStore::start(
        DomainRepository::open_managed(&f.source, &f.profile.digest).unwrap(),
        4,
    )
    .unwrap();
    let session = Session::new(
        source.clone(),
        context.clone(),
        permissions,
        Instant::now() + Duration::from_secs(30),
    )
    .unwrap();
    let original = Intent::new(
        IntentOperation::Contribute,
        "original_card".into(),
        &json!({"commandId":"original_card","resourceId":f.resource.id()}),
    )
    .unwrap();
    session
        .prepare_intent(&context, original.clone())
        .await
        .unwrap();
    session
        .record_intent_reply(
            &context,
            original,
            IntentReply::Success(json!({"admission":{"contribution":{"id":"con_keep_original"}}})),
        )
        .await
        .unwrap();
    source.shutdown().await.unwrap();
    prepare(&f, "complete_transfer").await.unwrap();
    let transfer = f
        .registry
        .resource_adoption_transfer(authority(&f), "complete_transfer".into())
        .await
        .unwrap();
    fenced_connections(&f, &transfer);
    f.registry
        .record_adoption_sources(authority(&f), archive_proof(&f, &transfer).unwrap())
        .await
        .unwrap();
    f.registry
        .import_resource_adoption(authority(&f), archive_proof(&f, &transfer).unwrap())
        .await
        .unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let old = journal
        .session(
            ManagementAuthority::new(
                context.clone(),
                permissions,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
            &f.profile.digest,
        )
        .unwrap();
    let pending = old.pending_intents(&context).await.unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].command_id, "original_card");
    assert_eq!(
        pending[0].reply,
        Some(IntentReply::Success(
            json!({"admission":{"contribution":{"id":"con_keep_original"}}})
        ))
    );
    assert!(
        f.registry
            .activate_resource_adoption(
                authority(&f),
                transfer.verify_private_journal(&journal).await.unwrap(),
                provider()
            )
            .await
            .is_err()
    );
    assert_eq!(
        f.registry
            .record_adoption_custody(
                authority(&f),
                transfer.verify_private_journal(&journal).await.unwrap()
            )
            .await
            .unwrap()
            .phase,
        "custody_ready"
    );
    let wrong = VerifiedProviderAccount::new(
        "codex",
        "https://provider.test",
        "different-account",
        Instant::now() + Duration::from_secs(20),
    )
    .unwrap();
    assert!(
        f.registry
            .activate_resource_adoption(
                authority(&f),
                transfer.verify_private_journal(&journal).await.unwrap(),
                wrong
            )
            .await
            .is_err()
    );
    assert!(
        f.registry
            .resource(&f.profile, f.resource.id())
            .await
            .is_err()
    );
    assert_eq!(
        f.registry
            .activate_resource_adoption(
                authority(&f),
                transfer.verify_private_journal(&journal).await.unwrap(),
                provider()
            )
            .await
            .unwrap()
            .phase,
        "active"
    );
    let original = f
        .registry
        .resource(&f.profile, f.resource.id())
        .await
        .unwrap();
    assert_eq!(
        original.revision,
        crate::resource_publication_revision(&f.resource).unwrap()
    );
    assert_eq!(original.budget.seat.remaining.map(u64::from), Some(700));
    let extra = f
        .registry
        .prepare_enrollment(
            f.profile.clone(),
            "same_account_next_model".into(),
            provider(),
            "another_model".into(),
            None,
            700,
            Instant::now() + Duration::from_secs(20),
        )
        .unwrap()
        .approve()
        .await
        .unwrap();
    assert_ne!(extra.resource_id, f.resource.id());
    let original_seat = f.resource.seat_id.clone();
    f.registry
        .0
        .store
        .call(1, move |db| {
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM seats", [], |r| r.get::<_, u32>(0))?,
                1
            );
            assert_eq!(
                db.db
                    .query_row("SELECT seat_id FROM owner_resource_accounts", [], |r| {
                        r.get::<_, String>(0)
                    })?,
                original_seat
            );
            assert_eq!(
                db.db
                    .query_row("SELECT count(*) FROM owner_capacity_grants", [], |r| r
                        .get::<_, u32>(0))?,
                0
            );
            Ok(())
        })
        .await
        .unwrap();
    old.acknowledge_intent(&context, "original_card".into())
        .await
        .unwrap();
    journal.shutdown().await.unwrap();
    let journal =
        PrivateManagementJournal::open_existing(&f.source, transfer.archive_binding().unwrap())
            .unwrap();
    let old = journal
        .session(
            ManagementAuthority::new(
                context.clone(),
                permissions,
                Instant::now() + Duration::from_secs(30),
            )
            .unwrap(),
            &f.profile.digest,
        )
        .unwrap();
    assert!(old.pending_intents(&context).await.unwrap().is_empty());
    assert_eq!(
        f.registry
            .resource_adoption(authority(&f), "complete_transfer".into())
            .await
            .unwrap()
            .phase,
        "active"
    );
    journal.shutdown().await.unwrap();
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsupported_original_web_enforcement_is_never_dropped_during_adoption() {
    for field in ["rateCapPerDay", "executionPolicy"] {
        let f = fixture().await;
        let d = Connection::open(f.source.join("domain.sqlite3")).unwrap();
        let mut config = serde_json::to_value(&f.resource).unwrap();
        config[field] = json!(100);
        d.execute(
            "UPDATE resources SET config=?1",
            [serde_json::to_string(&config).unwrap()],
        )
        .unwrap();
        assert!(inventory(&f).is_err());
        assert!(prepare(&f, "unsupported_policy").await.is_err());
        f.registry.shutdown().await.unwrap();
    }
}

#[path = "resource_name_tests.rs"]
mod resource_name_tests;
#[path = "resource_edit_tests.rs"]
mod resource_edit_tests;
#[path = "resource_create_tests.rs"]
mod resource_create_tests;
#[path = "resource_ceiling_tests.rs"]
mod resource_ceiling_tests;

#[tokio::test]
async fn shared_demand_schema85_history_is_never_silently_adopted() {
    for sql in [
        "INSERT INTO owner_shared_demand_controls VALUES('child',?1,1,0)",
        "INSERT INTO owner_shared_demands VALUES('demand','child',?1,'device',1,'$event','{}',?1,1)",
        "INSERT INTO owner_shared_demand_decisions VALUES('demand','budget_blocked',NULL,NULL,NULL,'{}',1)",
        "INSERT INTO owner_shared_demand_bindings VALUES('demand','work','{}',?1,1)",
    ] {
        let f = fixture().await;
        assert!(inventory(&f).is_ok());
        let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        if sql.contains("?1") {
            db.execute(sql, [&f.profile.digest]).unwrap();
        } else {
            db.execute(sql, []).unwrap();
        }
        drop(db);
        assert!(matches!(inventory(&f), Err(Error::Quarantined)));
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn shared_demand_stop_schema86_history_is_never_silently_adopted() {
    for sql in [
        "INSERT INTO owner_shared_stop_intents VALUES('stop','child',?1,'device',1,'@owner:example.test','assignment',1,'{}',?1,1)",
        "INSERT INTO owner_shared_stop_receipts VALUES('stop',2,1)",
    ] {
        let f = fixture().await;
        assert!(inventory(&f).is_ok());
        let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();
        if sql.contains("?1") {
            db.execute(sql, [&f.profile.digest]).unwrap();
        } else {
            db.execute(sql, []).unwrap();
        }
        drop(db);
        assert!(matches!(inventory(&f), Err(Error::Quarantined)));
        f.registry.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn shared_context_schema87_sent_history_is_never_silently_adopted(){
 let f=fixture().await;assert!(inventory(&f).is_ok());let db=Connection::open(f.source.join("domain.sqlite3")).unwrap();db.execute_batch("PRAGMA foreign_keys=OFF;INSERT INTO owner_shared_reply_deliveries VALUES('work','{}',printf('%064d',0),1);").unwrap();drop(db);assert!(matches!(inventory(&f),Err(Error::Quarantined)));f.registry.shutdown().await.unwrap();
}

#[path = "account_inventory_tests.rs"]
mod account_inventory_tests;

#[path="resource_alert_tests.rs"]
mod resource_alert_tests;
#[path="approval_binding_tests.rs"]
mod approval_binding_tests;
#[path="tool_approval_tests.rs"]
mod tool_approval_tests;

#[path = "alert_transfer_tests.rs"]
mod alert_transfer_tests;

#[path="execution_policy_tests.rs"]
mod execution_policy_tests;
#[path="operator_graph_workflow_tests.rs"]
mod operator_graph_workflow_tests;

#[path = "resource_template_tests.rs"]
mod resource_template_tests;

#[path="web_catalog_tests.rs"]
mod web_catalog_tests;
#[path = "retirement_tests.rs"]
mod retirement_tests;
#[path="resource_publication_tests.rs"]
mod resource_publication_tests;

#[path="web_connection_tests.rs"] mod web_connection_tests;


#[tokio::test]
async fn ceiling_amendment_schema105_history_is_retained_and_missing_table_is_refused() {
    let f = fixture().await;
    assert!(inventory(&f).is_ok());
    let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    db.execute(
        "INSERT INTO owner_resource_ceiling_amendments VALUES(?1,1,?2,'source_amendment',?3,1000,1500,'monthly',1)",
        params![f.resource.id(), f.profile.digest, crate::resource_publication_revision(&f.resource).unwrap()],
    )
    .unwrap();
    // An amended maximum is capacity history: never silently adopted.
    assert!(matches!(inventory(&f), Err(Error::Quarantined)));
    db.execute_batch("PRAGMA foreign_keys=OFF;DROP TRIGGER owner_resource_amended_ceiling_retained;DROP TABLE owner_resource_ceiling_amendments;").unwrap();
    assert!(matches!(inventory(&f), Err(Error::Schema)));
    f.registry.shutdown().await.unwrap();
}

#[tokio::test]
async fn shared_tool_schema101_history_is_retained_and_missing_table_is_refused() {
    let f = fixture().await;
    assert!(inventory(&f).is_ok());
    let db = Connection::open(f.source.join("domain.sqlite3")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF;INSERT INTO owner_shared_tool_policies VALUES('retained-agent',1,1);").unwrap();
    assert!(matches!(inventory(&f), Err(Error::Quarantined)));
    db.execute_batch("DROP TABLE owner_shared_tool_policies;").unwrap();
    assert!(matches!(inventory(&f), Err(Error::Schema)));
    f.registry.shutdown().await.unwrap();
}
