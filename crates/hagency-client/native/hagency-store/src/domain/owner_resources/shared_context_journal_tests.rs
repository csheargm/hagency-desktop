use super::*;
const DROP_CONTEXT: &str = "DROP TABLE IF EXISTS owner_web_catalog_settings; DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations;DROP TRIGGER IF EXISTS owner_resource_provenance_immutable;DROP TRIGGER IF EXISTS owner_resource_provenance_retained;DROP TABLE IF EXISTS owner_resource_provenance;DROP TABLE IF EXISTS owner_namespace_enrollments;DROP TABLE IF EXISTS owner_resource_namespaces;DROP TABLE IF EXISTS management_account_profiles;DROP TABLE IF EXISTS owner_shared_budget_demand_decisions;DROP TABLE IF EXISTS owner_shared_budget_work_refusals;DROP TABLE IF EXISTS owner_shared_budget_start_intents;DROP TABLE IF EXISTS owner_managed_budget_deltas;DROP TABLE IF EXISTS owner_managed_budget_commands;DROP TABLE IF EXISTS owner_operator_task_transfer_comments;DROP TABLE IF EXISTS owner_operator_task_transfer_tasks;DROP TABLE owner_shared_context_call_pins;DROP TABLE owner_shared_reply_deliveries;DROP TABLE owner_shared_contexts;DROP TABLE owner_shared_input_modes;";
#[tokio::test]
async fn shared_context_schema87_upgrade_rolls_back_and_preserves_original_resource() {
    let mut f = fixture().await;
    let resource = f.resource.clone();
    f.registry.shutdown().await.unwrap();
    let path = f._temp.path().join("owner/domain.sqlite3");
    let db = Connection::open(&path).unwrap();
    let before: String = db
        .query_row(
            "SELECT config FROM resources WHERE id=?1",
            [&resource],
            |r| r.get(0),
        )
        .unwrap();
    db.execute_batch(&format!("{DROP_CONTEXT}DROP TABLE IF EXISTS owner_octos_model_resources;DROP TABLE IF EXISTS owner_octos_enrollments;DROP TABLE IF EXISTS owner_octos_profiles;DROP TABLE IF EXISTS owner_octos_key;DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;DROP TABLE IF EXISTS owner_shared_decrypted_inputs;DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;DROP TABLE IF EXISTS owner_resource_ceiling_amendments;DROP TABLE IF EXISTS owner_shared_unstarted_attempts;DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;DROP TABLE IF EXISTS owner_shared_tool_policies;PRAGMA user_version=86;CREATE TABLE owner_shared_contexts(collision INTEGER);")).unwrap();
    drop(db);
    assert!(OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).is_err());
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        86
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='owner_shared_input_modes'",
            [],
            |r| r.get::<_, u64>(0)
        )
        .unwrap(),
        0
    );
    db.execute_batch("DROP TABLE owner_shared_contexts")
        .unwrap();
    drop(db);
    f.registry = OwnerResourceRegistry::open_root(&f._temp.path().join("owner")).unwrap();
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(&path).unwrap();
    assert_eq!(
        db.pragma_query_value::<u64, _>(None, "user_version", |r| r.get(0))
            .unwrap(),
        crate::DOMAIN_SCHEMA_VERSION as u64
    );
    assert_eq!(
        db.query_row(
            "SELECT config FROM resources WHERE id=?1",
            [resource],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        before
    );
}
#[tokio::test]
async fn shared_context_journal_mode_and_delivery_rows_are_permanent() {
    let f = fixture().await;
    f.registry.shutdown().await.unwrap();
    let db = Connection::open(f._temp.path().join("owner/domain.sqlite3")).unwrap();
    db.execute_batch("PRAGMA foreign_keys=OFF;INSERT INTO owner_shared_input_modes VALUES('historical','thread_context_v1',1);INSERT INTO owner_shared_reply_deliveries VALUES('historical','{}',printf('%064d',0),2);").unwrap();
    for sql in [
        "UPDATE owner_shared_input_modes SET mode='plain_text_v1'",
        "DELETE FROM owner_shared_input_modes",
        "UPDATE owner_shared_reply_deliveries SET digest=printf('%064d',1)",
        "DELETE FROM owner_shared_reply_deliveries",
    ] {
        assert!(db.execute_batch(sql).is_err(), "{sql}");
    }
}
