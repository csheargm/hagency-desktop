//! Removing a binding must not revive the original review on re-observation.
use super::*;
use hagency_store::management::{
    HostContext, Permission, ResourceApprovalBindingQuery, RetireResourceApprovalBinding, Session,
};
use std::time::{Duration, Instant};

#[test]
fn management_binding_filter_input_rejects_paths_and_non_room_values() {
    for project in ["project_one", "!project:example.test", ""] {
        let query = ResourceApprovalBindingQuery {
            resource_id: "resource".into(),
            agent: "agent_one".into(),
            project: project.into(),
            ..Default::default()
        };
        query.validate().unwrap();
    }
    for project in [
        "../private",
        "https://example.test",
        "!room",
        "!room:\0host",
    ] {
        let query = ResourceApprovalBindingQuery {
            resource_id: "resource".into(),
            project: project.into(),
            ..Default::default()
        };
        assert!(query.validate().is_err(), "{project}");
    }
    assert!(
        serde_json::from_value::<ResourceApprovalBindingQuery>(json!({
            "resourceId": "resource", "agent": "a", "project": "project_one", "owner": "forged"
        }))
        .is_err()
    );
}



#[test]
fn approval_binding_recreation_does_not_reuse_original_incarnation() {
    let mut f = Fixture::new(true);
    let original = f.admit(0, 900);
    let old_verdict = f.verdict(&original.id, ApprovalChoice::Once, "old_review");
    let removed =
        f.db.retire_approval_binding("a", "!private:example.test")
            .unwrap();
    f.db.observe_approval_room(&f.rooms[0], 1020).unwrap();
    let current =
        f.db.approval_bindings("a", "!private:example.test", 16)
            .unwrap();
    assert_eq!(current.len(), 1);
    assert!(
        current[0].incarnation > removed.incarnation,
        "recreated binding must not match an old context or review"
    );
    assert!(f.db.observe_owner_verdict(&old_verdict, 1021).is_err());
    assert!(
        f.db.request_owner_approval(&f.caps[0], &f.input(0, 901), 1022)
            .is_err()
    );
    // The other agent's independently observed binding remains unchanged.
    let other =
        f.db.approval_bindings("b", "!private:example.test", 16)
            .unwrap();
    assert_eq!(other[0].incarnation, 1);
}

#[test]
fn approval_binding_counter_survives_restart_and_ignores_duplicate_observations() {
    let mut f = Fixture::new(true);
    f.db.observe_approval_room(&f.rooms[0], 1020).unwrap();
    assert_eq!(
        f.db.approval_bindings("a", "", 16).unwrap()[0].incarnation,
        1
    );
    f.db.retire_approval_binding("a", "!private:example.test")
        .unwrap();
    let Fixture {
        root, db, rooms, ..
    } = f;
    drop(db);
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.observe_approval_room(&rooms[0], 1021).unwrap();
    assert_eq!(db.approval_bindings("a", "", 16).unwrap()[0].incarnation, 2);
    db.retire_approval_binding("a", "!private:example.test")
        .unwrap();
    db.observe_approval_room(&rooms[0], 1022).unwrap();
    db.observe_approval_room(&rooms[0], 1023).unwrap();
    assert_eq!(db.approval_bindings("a", "", 16).unwrap()[0].incarnation, 3);
}

#[test]
fn approval_binding_schema93_recovers_removed_binding_generation_from_context() {
    let mut f = Fixture::new(true);
    f.rooms[0].generation = 2;
    f.db.observe_approval_room(&f.rooms[0], 1020).unwrap();
    f.contexts[0].id = "new_context_a".into();
    f.db.bind_approval_context(&f.caps[0], &f.contexts[0], 1021)
        .unwrap();
    let request = f.input(0, 902);
    let approval =
        f.db.request_owner_approval(&f.caps[0], &request, 1022)
            .unwrap();
    let verdict = f.verdict(&approval.id, ApprovalChoice::Once, "before_upgrade");
    f.db.retire_approval_binding("a", "!private:example.test")
        .unwrap();
    let sql = f.sql();
    let before: String = sql
        .query_row(
            "SELECT config FROM approval_contexts WHERE id='new_context_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Reconstruct schema 93 without retaining later additions or table shapes.
    common::remove_owner_resource_additions_after_schema98(&sql);
    sql.execute_batch("DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE owned_dispatch_execution_policies; DROP TABLE approval_binding_incarnations;")
        .unwrap();
    common::restore_empty_historical_tables(
        &sql,
        &[
            include_str!("../../src/migrations/077-owner-managed-agents.sql"),
            include_str!("../../src/migrations/079-owner-managed-fences.sql"),
            include_str!("../../src/migrations/081-owner-managed-runtime.sql"),
            include_str!("../../src/migrations/082-owner-shared-execution.sql"),
            include_str!("../../src/migrations/084-owner-shared-start-intents.sql"),
        ],
        &[
            "owner_managed_agents",
            "owner_managed_fences",
            "owner_shared_work_observations",
            "owner_shared_start_intents",
        ],
    );
    sql.pragma_update(None, "user_version", 93).unwrap();
    drop(sql);
    let Fixture {
        root, db, rooms, ..
    } = f;
    drop(db);
    let mut db = DomainRepository::open(&root.path().join("state")).unwrap();
    db.observe_approval_room(&rooms[0], 1023).unwrap();
    assert_eq!(db.approval_bindings("a", "", 16).unwrap()[0].incarnation, 3);
    assert!(db.observe_owner_verdict(&verdict, 1024).is_err());
    let sql = rusqlite::Connection::open(root.path().join("state/domain.sqlite3")).unwrap();
    let after: String = sql
        .query_row(
            "SELECT config FROM approval_contexts WHERE id='new_context_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        sql.pragma_query_value(None, "user_version", |r| r.get::<_, i32>(0))
            .unwrap(),
        hagency_store::DOMAIN_SCHEMA_VERSION
    );
}

#[test]
fn approval_binding_counter_exhaustion_rolls_back_room_observation() {
    let mut f = Fixture::new(true);
    let sql = f.sql();
    sql.execute(
        "UPDATE approval_binding_incarnations SET incarnation=?1 WHERE engagement_id=?2",
        rusqlite::params![hagency_core::JSON_SAFE_MAX, f.agents[0]],
    )
    .unwrap();
    let before: String = sql
        .query_row(
            "SELECT config FROM approval_rooms WHERE room_id='!private:example.test'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    f.rooms[0].generation = 2;
    assert!(matches!(
        f.db.observe_approval_room(&f.rooms[0], 1020),
        Err(Error::Capacity)
    ));
    let after: String = sql
        .query_row(
            "SELECT config FROM approval_rooms WHERE room_id='!private:example.test'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(
        f.db.approval_bindings("a", "", 16).unwrap()[0].incarnation,
        1
    );
}

fn context() -> HostContext {
    HostContext::new(
        "@resource_owner:example.test".into(),
        1,
        "a".repeat(64),
        1,
        "management_device".into(),
        1,
    )
    .unwrap()
}
