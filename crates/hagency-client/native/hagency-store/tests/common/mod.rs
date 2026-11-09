#![allow(dead_code)] // Shared by separate focused integration test executables.
use hagency_core::{authority::*, project::Resource};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub fn resource(preset: &str, seat: &str, tokens: u64) -> Resource {
    serde_json::from_value(
        json!({"presetId":preset,"seatId":seat,"framework":"codex","model":"gpt-5.6-sol","reasoning":"medium",
        "ceiling":{"tokens":tokens,"period":"monthly"}}),
    )
    .unwrap()
}
pub fn registration() -> Registration {
    let fleet = format!("hf_{}", "a".repeat(32));
    Registration {
        fleet_id: fleet.clone(),
        generation: 1,
        server_name: "example.test".into(),
        reception_room_id: "!reception:example.test".into(),
        representative_mxid: format!("@{fleet}_representative:example.test"),
        approval_bot_mxid: "@approval:example.test".into(),
    }
}
pub fn request(id: &str, name: &str, resource: &Resource, tokens: u64) -> ProjectRequest {
    serde_json::from_value(json!({"v":1,"fleetId":registration().fleet_id,"requestId":id,
        "requesterMxid":"@owner:example.test","sourceRoomId":"!reception:example.test","targetProjectId":"project_one","targetRoomId":"!project:example.test",
        "ownerMxid":"@owner:example.test","ownerDmRoomId":"!private:example.test","role":"coding","requestedTokens":tokens,"ratePerDay":null,"authVersion":1,
        "sourceEventId":format!("${id}"),"agentDefinition":{"name":name,"resourceId":resource.id()}})).unwrap()
}
fn room(id: &str, members: Vec<String>) -> RoomObservation {
    RoomObservation {
        room_id: id.into(),
        joined: BTreeSet::from_iter(members),
        invite_only: true,
        encryption: None,
        powers: BTreeMap::new(),
        default_power: 0,
        invite_power: 0,
        binding: None,
        name: None,
    }
}
pub fn observation(request: &ProjectRequest) -> RequestObservation {
    let reg = registration();
    let reception = room(
        &request.source_room_id,
        vec![
            request.requester_mxid.clone(),
            reg.representative_mxid.clone(),
        ],
    );
    let mut project = room(
        &request.target_room_id,
        vec![
            request.requester_mxid.clone(),
            request.owner_mxid.clone(),
            reg.representative_mxid,
        ],
    );
    project.powers.insert(request.owner_mxid.clone(), 100);
    project.name = Some("实际项目名称".into());
    project.binding = Some(
        json!({"v":1,"fleetId":reg.fleet_id,"purpose":"project","projectId":request.target_project_id,"ownerMxid":request.owner_mxid,"authVersion":1}),
    );
    let mut owner_room = room(
        &request.owner_dm_room_id,
        vec![request.owner_mxid.clone(), reg.approval_bot_mxid],
    );
    owner_room.encryption = Some("m.megolm.v1.aes-sha2".into());
    let mut content = serde_json::to_value(request).unwrap();
    content.as_object_mut().unwrap().remove("ownerDmRoomId");
    content.as_object_mut().unwrap().remove("sourceEventId");
    RequestObservation {
        registration_generation: 1,
        observed_at_ms: 1000,
        source: SourceObservation {
            event_id: request.source_event_id.clone(),
            room_id: request.source_room_id.clone(),
            sender: request.requester_mxid.clone(),
            event_type: "com.hagency.engagement.request.v1".into(),
            content,
        },
        reception,
        project,
        owner_room,
    }
}
pub fn proof(request: &ProjectRequest) -> VerifiedRequest {
    verify_request(&registration(), request.clone(), observation(request)).unwrap()
}
pub fn value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap()
}
/// Reconstruct pre-graph native schema for older migration regression fixtures.
pub fn remove_graph_schema(db: &rusqlite::Connection) {
    remove_reply_schema(db);
    let original: String = db
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='view' AND name='conversation_peer_inputs'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    db.execute_batch("DROP VIEW admissible_dispatch_peer_inputs; DROP VIEW graph_dispatch_scope; DROP VIEW graph_dispatch_ready; DROP VIEW current_graph_scopes; DROP VIEW live_peer_inputs; DROP VIEW conversation_peer_inputs; DROP TABLE graph_dependencies; DROP TABLE graph_commands; DROP TABLE graph_nodes; DROP TABLE task_graphs; DROP TABLE IF EXISTS room_trust;").unwrap();
    db.execute_batch(&original.replacen("conversation_peer_inputs", "live_peer_inputs", 1))
        .unwrap();
}
/// Restore schema 10 without inventing privacy for any earlier native session.
pub fn remove_reply_schema(db: &rusqlite::Connection) {
    remove_ingress_schema(db);
    db.execute_batch("DROP VIEW current_final_replies; DROP VIEW current_matrix_routes; DROP TABLE final_reply_inspections; DROP TABLE final_reply_calls; DROP TABLE final_replies; DROP TABLE matrix_session_routes; DROP TABLE matrix_room_memberships; DROP TABLE matrix_room_scopes; DROP TABLE matrix_transports; DROP INDEX canonical_runner_session; ALTER TABLE runner_sessions DROP COLUMN matrix_generation; CREATE UNIQUE INDEX canonical_runner_session ON runner_sessions(engagement_id,CASE WHEN json_extract(binding,'$.kind')='internal' THEN 'internal' ELSE 'matrix' END,CASE WHEN json_extract(binding,'$.kind')='internal' THEN id ELSE json_extract(binding,'$.room_id') END,COALESCE(json_extract(binding,'$.thread_root'),'')); DROP TABLE IF EXISTS room_trust;").unwrap();
}

/// Restore schema 11 without manufacturing new ingress provenance.
pub fn remove_ingress_schema(db: &rusqlite::Connection) {
    remove_approval_schema(db);
    db.execute_batch("DROP VIEW current_final_replies; DROP VIEW current_matrix_routes; DROP VIEW task_followup_ready; DROP TABLE verified_task_requests; DROP TABLE matrix_ingress_events; ALTER TABLE matrix_transports DROP COLUMN observed_at; ALTER TABLE matrix_room_scopes DROP COLUMN visibility_since; ALTER TABLE matrix_session_routes DROP COLUMN ingress_since; ALTER TABLE matrix_session_routes DROP COLUMN parent_session_id; ALTER TABLE session_inputs DROP COLUMN config; ALTER TABLE task_inputs DROP COLUMN config; ALTER TABLE task_inputs DROP COLUMN wake; ALTER TABLE task_notices DROP COLUMN verified_route; ALTER TABLE task_notices DROP COLUMN content_digest; DROP TABLE IF EXISTS room_trust;").unwrap();
    let routes = include_str!("../../src/migrations/011-final-replies.sql");
    db.execute_batch(reply_route_view(routes)).unwrap();
    db.execute_batch(&routes[routes.find("CREATE VIEW current_final_replies AS").unwrap()..])
        .unwrap();
    let tasks = include_str!("../../src/migrations/005-task-intents.sql");
    db.execute_batch(&tasks[tasks.find("CREATE VIEW task_followup_ready AS").unwrap()..])
        .unwrap();
}

/// Reconstruct schema 12 without backfilling owner approval authority.
pub fn remove_approval_schema(db: &rusqlite::Connection) {
    remove_notice_schema(db);
    // MA-S2's 033 contract: rewinding below 33 rebuilds runner_attempts to
    // its pre-033 shape — park_reason existed only from schema 33 on, and its
    // ALTER does not replay idempotently. 036 splits the frozen window the
    // same way: `addressed` exists only from schema 36 on. 037 adds the
    // attempt clock columns and the event table the same way.
    db.execute_batch("ALTER TABLE runner_attempts DROP COLUMN park_reason; ALTER TABLE dispatch_inputs DROP COLUMN addressed; DROP TABLE IF EXISTS dispatch_conversation_reads; ALTER TABLE runner_attempts DROP COLUMN started_at; ALTER TABLE runner_attempts DROP COLUMN parked_at; ALTER TABLE runner_attempts DROP COLUMN last_renew_at; ALTER TABLE runner_attempts DROP COLUMN settled_at; ALTER TABLE runner_attempts DROP COLUMN terminal_reason; DROP TABLE IF EXISTS runner_attempt_events; DROP TABLE IF EXISTS agent_fences; DROP TABLE IF EXISTS avatar_requests; DROP TABLE IF EXISTS agent_tombstones; DROP TABLE IF EXISTS delivery_events; DROP TABLE IF EXISTS operator_messages; DROP TABLE IF EXISTS dispatch_activity_events; DROP TABLE IF EXISTS dispatch_activity;  DROP TABLE IF EXISTS pending_invites;  DROP TABLE IF EXISTS ceiling_alert_notes; DROP TABLE IF EXISTS agent_lifecycle; DROP TABLE IF EXISTS side_registrations; DROP VIEW IF EXISTS current_command_notices; DROP TABLE IF EXISTS command_notice_inspections; DROP TABLE IF EXISTS command_notices; ALTER TABLE final_replies DROP COLUMN incidental; DROP TABLE IF EXISTS operator_tasks; DROP TABLE IF EXISTS operator_task_comments; DROP TABLE IF EXISTS side_records; DROP TABLE IF EXISTS side_projects;  DROP TABLE IF EXISTS reminders; DROP TABLE IF EXISTS room_trust;")
         .unwrap();
    db.execute_batch("DROP TRIGGER approval_room_retire_grants; DROP TRIGGER approval_project_retire; DROP TRIGGER approval_registration_retire; DROP TRIGGER approval_engagement_retire; DROP TRIGGER approval_task_retire; DROP VIEW current_approval_bindings; DROP TABLE approval_verdict_receipts; DROP TABLE approval_grants; DROP TABLE owner_approvals; DROP TABLE approval_contexts; DROP TABLE approval_bindings; DROP TABLE approval_rooms; DROP TABLE IF EXISTS room_trust;").unwrap();
}

/// SQL statement boundaries survive LF and Windows checkout CRLF spelling.
pub fn reply_route_view(routes: &str) -> &str {
    let start = routes.find("CREATE VIEW current_matrix_routes AS").unwrap();
    let end = routes.find("CREATE TABLE final_replies").unwrap();
    &routes[start..end]
}

/// Rebuild the actual schema13 notice shape; a pre-custody sender could hold a claim.
pub fn remove_notice_schema(db: &rusqlite::Connection) {
    remove_matrix_transport_schema(db);
    db.execute_batch("DROP TABLE notice_send_inspections; DROP INDEX task_notice_ready; ALTER TABLE task_notices RENAME TO task_notices_newer; DROP TABLE IF EXISTS room_trust;").unwrap();
    let schema = include_str!("../../src/migrations/005-task-intents.sql");
    let start = schema.find("CREATE TABLE task_notices").unwrap();
    let end = schema.find("-- One predicate").unwrap();
    db.execute_batch(&schema[start..end]).unwrap();
    db.execute_batch("ALTER TABLE task_notices ADD COLUMN verified_route TEXT CHECK(verified_route IS NULL OR json_valid(verified_route)); ALTER TABLE task_notices ADD COLUMN content_digest TEXT; INSERT INTO task_notices(id,task_id,config,state,claim_hash,claim_until,delivery,error_code,not_before,verified_route,content_digest) SELECT id,task_id,config,CASE WHEN state IN ('sending','uncertain') THEN 'claimed' ELSE state END,claim_hash,claim_until,delivery,error_code,not_before,verified_route,content_digest FROM task_notices_newer; DROP TABLE task_notices_newer; DROP TABLE IF EXISTS room_trust;").unwrap();
}

/// Restore schema14 without weakening its notice-send custody.
pub fn remove_matrix_transport_schema(db: &rusqlite::Connection) {
    remove_owned_completion_schema(db);
    db.execute_batch(
        "DROP TRIGGER matrix_transport_retire_approvals; DROP VIEW current_matrix_routes;",
    )
    .unwrap();
    let schema = include_str!("../../src/migrations/012-verified-ingress.sql");
    let start = schema.find("CREATE VIEW current_matrix_routes AS").unwrap();
    let end = schema.find("DROP VIEW task_followup_ready;").unwrap();
    db.execute_batch(&schema[start..end]).unwrap();
    db.execute_batch("ALTER TABLE matrix_transports DROP COLUMN available; ALTER TABLE matrix_transports DROP COLUMN invalidation;").unwrap();
}

/// Restore schema15 without manufacturing completion or owner authority.
pub fn remove_owned_completion_schema(db: &rusqlite::Connection) {
    remove_usage_schema(db);
    db.execute_batch("DROP TABLE owned_task_completions; DROP TABLE IF EXISTS room_trust;")
        .unwrap();
}

/// Restore schema16 without inventing usage for historical execution paths.
pub fn remove_usage_schema(db: &rusqlite::Connection) {
    remove_attachment_schema(db);
    db.execute_batch("DROP TABLE usage_receipts; DROP TABLE usage_periods; DROP TABLE usage_sources; DROP TABLE usage_clock; DROP TABLE IF EXISTS room_trust;").unwrap();
}

/// Remove delivery and upload additions before constructing an older database.
pub fn remove_upload_schema(db: &rusqlite::Connection) {
    remove_coordinator_schema(db);
    // Migration 025 adds columns to `ceiling_alerts`, and an ADD COLUMN replay
    // over an already-upgraded table fails on the duplicate column (no ADD
    // COLUMN migration in this store supports that replay). Every chained
    // `remove_*_schema` helper reaches this innermost one, so the table is
    // rebuilt from 024 here; the fixtures that drop tables directly instead
    // of chaining (`approvals/responses.rs`, `file_delivery.rs`,
    // `received_files.rs`, `schema_fixtures.rs`) carry the same drop.
    db.execute_batch("DROP TABLE IF EXISTS ceiling_alerts; DROP TABLE IF EXISTS ceiling_alert_notes; DROP TABLE IF EXISTS room_trust;")
        .unwrap();
    db.execute_batch("DROP TABLE IF EXISTS approval_responses; DROP TABLE IF EXISTS received_files; DROP TABLE IF EXISTS file_deliveries; DROP TABLE IF EXISTS file_uploads; DROP TABLE IF EXISTS room_trust;")
        .unwrap();
}

/// Remove schema99–111 additions before older fixtures alter their parent tables.
pub fn remove_owner_resource_additions_after_schema98(db: &rusqlite::Connection) {
    // These two triggers belong to older tables, so dropping only their newer
    // referenced tables would leave invalid schema during a later ALTER TABLE.
    db.execute_batch(
        "DROP TRIGGER IF EXISTS owner_managed_agent_fence_time;
         DROP TRIGGER IF EXISTS owner_resource_amended_ceiling_retained;
         DROP TABLE IF EXISTS owner_shared_native_checkpoints;DROP TABLE IF EXISTS owner_shared_native_turns;DROP TABLE IF EXISTS owner_shared_unstarted_runtimes;
         DROP TABLE IF EXISTS owner_resource_calendars;
         DROP TABLE IF EXISTS owner_managed_call_periods;
         DROP TABLE IF EXISTS owner_managed_periods;
         DROP TABLE IF EXISTS owner_shared_decrypted_inputs;
         DROP TABLE IF EXISTS owner_resource_ceiling_amendments;
         DROP TABLE IF EXISTS owner_capacity_grant_successions;
         DROP TABLE IF EXISTS owner_managed_fence_times;
         DROP TABLE IF EXISTS owner_shared_budget_demand_terminals;
         DROP TABLE IF EXISTS owner_shared_unstarted_attempts;
         DROP TABLE IF EXISTS owner_shared_tool_policies;
         DROP TABLE IF EXISTS owner_web_catalog_settings;",
    )
    .unwrap();
}

/// Restore original definitions only for tables with no fixture history to lose.
pub fn restore_empty_historical_tables(
    db: &rusqlite::Connection,
    migrations: &[&str],
    tables: &[&str],
) {
    let historical = rusqlite::Connection::open_in_memory().unwrap();
    for migration in migrations {
        historical.execute_batch(migration).unwrap();
    }
    for table in tables {
        assert_eq!(
            db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| {
                r.get::<_, u64>(0)
            })
            .unwrap(),
            0,
            "{table} must be empty before restoring its historical definition"
        );
    }
    for table in tables {
        let definition: String = historical
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name=?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        let objects = historical
            .prepare("SELECT sql FROM sqlite_master WHERE tbl_name=?1 AND type IN ('trigger','index') AND sql IS NOT NULL")
            .unwrap()
            .query_map([table], |r| r.get::<_, String>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        db.execute_batch(&format!("DROP TABLE {table};{definition};"))
            .unwrap();
        for object in objects {
            db.execute_batch(&object).unwrap();
        }
    }
}

/// Remove ADR 0011 additions when a migration fixture reconstructs an older DB.
pub fn remove_owner_resource_schema(db: &rusqlite::Connection) {
    remove_owner_resource_additions_after_schema98(db);
    db.execute_batch("DROP TABLE IF EXISTS owner_operator_graph_recipients; DROP TABLE IF EXISTS owner_operator_graph_outbox; DROP TABLE IF EXISTS owner_operator_graph_commands; DROP TABLE IF EXISTS owner_operator_graphs; DROP TABLE IF EXISTS owner_operator_graph_archive_chunks; DROP TABLE IF EXISTS owner_operator_graph_archives; DROP TABLE IF EXISTS owned_dispatch_execution_policies;DROP TABLE IF EXISTS approval_binding_incarnations; DROP TABLE IF EXISTS owner_resource_provenance;").unwrap();
    // Rewind schemas87–94 before their referenced parent tables. Otherwise
    // SQLite validates a surviving context trigger against a dropped table
    // during the older fixture's ALTER TABLE, before migration replay begins.
    db.execute_batch("DROP TABLE IF EXISTS owner_namespace_enrollments; DROP TABLE IF EXISTS owner_resource_namespaces; DROP TABLE IF EXISTS management_account_profiles; DROP TABLE IF EXISTS owner_shared_budget_demand_decisions; DROP TABLE IF EXISTS owner_shared_budget_work_refusals; DROP TABLE IF EXISTS owner_shared_budget_start_intents; DROP TABLE IF EXISTS owner_managed_budget_deltas; DROP TABLE IF EXISTS owner_managed_budget_commands; DROP TABLE IF EXISTS owner_operator_task_transfer_comments; DROP TABLE IF EXISTS owner_operator_task_transfer_tasks; DROP TABLE IF EXISTS owner_shared_context_call_pins; DROP TABLE IF EXISTS owner_shared_reply_deliveries; DROP TABLE IF EXISTS owner_shared_contexts; DROP TABLE IF EXISTS owner_shared_input_modes;").unwrap();
    db.execute_batch(
        "DROP TABLE IF EXISTS owner_shared_stop_receipts;DROP TABLE IF EXISTS owner_shared_stop_intents;DROP TABLE IF EXISTS owner_shared_demand_bindings;DROP TABLE IF EXISTS owner_shared_demand_decisions;DROP TABLE IF EXISTS owner_shared_demands;DROP TABLE IF EXISTS owner_shared_demand_controls;DROP TABLE IF EXISTS owner_operator_task_comments;DROP TABLE IF EXISTS owner_operator_tasks;DROP TABLE IF EXISTS owner_operator_task_histories;DROP TABLE IF EXISTS owner_resource_display_names;DROP TABLE IF EXISTS owner_shared_start_intents;DROP TABLE IF EXISTS owner_shared_successor_fences;DROP TABLE IF EXISTS owner_shared_successor_call_pins;DROP TABLE IF EXISTS owner_shared_successor_observations;DROP TABLE IF EXISTS owner_shared_successor_attempts;DROP TABLE IF EXISTS owner_shared_activity_renewals;DROP TABLE IF EXISTS owner_shared_work_observations;DROP TABLE IF EXISTS owner_shared_work_events;DROP TABLE IF EXISTS owner_shared_work_inbox;DROP TABLE IF EXISTS owner_shared_process_observations;DROP TABLE IF EXISTS owner_shared_runtime_lifetimes;DROP TRIGGER IF EXISTS owner_metadata_excludes_shared;DROP TABLE IF EXISTS owner_managed_runtime_observations;DROP TABLE IF EXISTS owner_managed_runtime_stops;DROP TABLE IF EXISTS owner_managed_runtime_lifetimes;DROP TABLE IF EXISTS owner_resource_adoptions; DROP TABLE IF EXISTS owner_managed_fences;",
    )
    .unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_managed_observations; DROP TABLE IF EXISTS owner_managed_call_pins; DROP TABLE IF EXISTS owner_managed_assignment_pins; DROP TABLE IF EXISTS owner_managed_calls; DROP TABLE IF EXISTS owner_managed_effects; DROP TABLE IF EXISTS owner_managed_agents; DROP TABLE IF EXISTS owner_project_authorities;").unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_parent_settlements; DROP TABLE IF EXISTS owner_capacity_receipts; DROP TABLE IF EXISTS owner_project_reservations; DROP TABLE IF EXISTS owner_capacity_grants;").unwrap();
    db.execute_batch("DROP TABLE IF EXISTS owner_resource_receipts; DROP TABLE IF EXISTS owner_resource_grants; DROP TABLE IF EXISTS owner_resource_accounts; DROP TABLE IF EXISTS owner_resource_identity;").unwrap();
}

pub fn remove_coordinator_schema(db: &rusqlite::Connection) {
    remove_owner_resource_schema(db);
    db.execute_batch(
        "DROP TABLE IF EXISTS management_intents; DROP TABLE IF EXISTS management_commands; DROP TABLE IF EXISTS management_identity;",
    )
    .unwrap();
    db.execute_batch("DROP TABLE IF EXISTS coordinator_migrations; DROP TABLE IF EXISTS coordinator_project_setup; DROP TABLE IF EXISTS coordinator_project_setup_attempts; DROP TABLE IF EXISTS coordinator_agent_profiles; DROP TABLE IF EXISTS coordinator_settlements; DROP TABLE IF EXISTS coordinator_refusals; DROP TABLE IF EXISTS coordinator_delegations; DROP TABLE IF EXISTS coordinator_deliveries; DROP TABLE IF EXISTS coordinator_publications; DROP TABLE IF EXISTS coordinator_commands; DROP TABLE IF EXISTS coordinator_agents; DROP TABLE IF EXISTS coordinator_projects; DROP TABLE IF EXISTS coordinator_resources; DROP TABLE IF EXISTS coordinator_engagements;").unwrap();
}

pub fn remove_attachment_schema(db: &rusqlite::Connection) {
    remove_upload_schema(db);
    db.execute_batch("DROP TABLE IF EXISTS dispatch_attachment_windows; DROP TABLE IF EXISTS session_attachment_visibility; DROP TABLE IF EXISTS matrix_attachments; DROP TABLE IF EXISTS room_trust;").unwrap();
}
