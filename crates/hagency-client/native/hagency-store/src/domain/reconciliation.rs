//! Read-only inventory on the original owned domain connection.
use super::*;
use crate::management::{
    AuditBlocker, AuditCount, AuditResource, ProfileLedgerAudit, ProfileLedgerHistory,
};
use crate::resource_publication_revision;

const MAX_TABLES: usize = 256;
const MAX_ROWS: u32 = 10_000;
const MAX_RESOURCES: usize = 64;
const MAX_RESOURCE_BYTES: usize = 16 * 1024;

impl DomainRepository {
    pub(crate) fn profile_ledger_audit(
        &mut self,
        profile: &str,
    ) -> Result<ProfileLedgerAudit, Error> {
        collect(&mut self.db, profile)
    }

    /// Observe an already-open read-only source. This never opens a repository,
    /// migrates schema, binds identity, runs recovery or grants adoption authority.
    /// The native host must retain source provenance and current account authority.
    pub fn audit_readonly_connection(
        db: &mut Connection,
        profile: &str,
    ) -> Result<ProfileLedgerAudit, Error> {
        if !db.is_readonly("main")? || !db.is_autocommit() {
            return Err(Error::LocalAuthority);
        }
        collect(db, profile)
    }
}

fn collect(db: &mut Connection, profile: &str) -> Result<ProfileLedgerAudit, Error> {
    // The snapshot is serialized with all normal jobs and does not upgrade
    // schema, checkpoint, run startup recovery or initialize another store.
    let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
    let schema: i32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let application: i32 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
    if !matches!(schema, 71..=111) || application != 0x48414732 {
        return Err(Error::Schema);
    }
    // Existing domain schema contains views. We never query them; every
    // named source read below must resolve to an ordinary table, not a view
    // or virtual/shadow table with externally implemented query behavior.
    let unsupported: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE schema='main' AND name NOT LIKE 'sqlite_%' AND type NOT IN ('table','view'))", [], |row| row.get(0))?;
    if unsupported {
        return Err(Error::Schema);
    }
    for (required, expected) in [
        ("management_identity", &["singleton", "profile_digest"][..]),
        (
            "management_commands",
            &["actor_scope", "command_id", "operation", "digest", "result"][..],
        ),
        (
            "management_intents",
            &[
                "actor_scope",
                "command_id",
                "operation",
                "digest",
                "input",
                "reply",
                "state",
            ][..],
        ),
        ("resources", &["id", "preset_id", "config"][..]),
        ("seats", &["id", "config"][..]),
    ]
    .into_iter()
    .chain((schema >= 77).then_some((
        "owner_managed_fences",
        &[
            "profile",
            "agent_id",
            "request_id",
            "original_command_id",
            "original_command_digest",
            "original_command",
            "project_reservation_id",
            "executor_device_id",
            "fence_revision",
        ][..],
    )))
    .chain((schema >= 78).then_some((
        "owner_resource_adoptions",
        &[
            "id",
            "profile",
            "plan_digest",
            "plan",
            "phase",
            "source_evidence",
            "custody_evidence",
            "result",
        ][..],
    )))
    .chain(
        (schema >= 79)
            .then_some([
                (
                    "owner_managed_runtime_lifetimes",
                    &[
                        "child_id",
                        "profile",
                        "assignment_id",
                        "command_id",
                        "command_payload",
                        "command_digest",
                        "binding_digest",
                        "provider_digest",
                        "issued_revision",
                        "incarnation",
                        "source_effect_state",
                    ][..],
                ),
                (
                    "owner_managed_runtime_stops",
                    &[
                        "child_id",
                        "command_id",
                        "command_payload",
                        "command_digest",
                        "fence_revision",
                    ][..],
                ),
                (
                    "owner_managed_runtime_observations",
                    &[
                        "id",
                        "child_id",
                        "command_id",
                        "payload",
                        "digest",
                        "ledger_revision",
                    ][..],
                ),
            ])
            .into_iter()
            .flatten(),
    )
    .chain(
        (schema >= 80)
            .then_some([
                (
                    "owner_shared_runtime_lifetimes",
                    &[
                        "child_id",
                        "profile",
                        "assignment_id",
                        "command_id",
                        "command_payload",
                        "command_digest",
                        "binding_digest",
                        "provider_digest",
                        "issued_revision",
                        "incarnation",
                    ][..],
                ),
                (
                    "owner_shared_process_observations",
                    &["id", "child_id", "payload", "digest", "ledger_revision"][..],
                ),
                (
                    "owner_shared_work_inbox",
                    &[
                        "id",
                        "child_id",
                        "execution_id",
                        "payload",
                        "digest",
                        "received_revision",
                    ][..],
                ),
                (
                    "owner_shared_work_events",
                    &[
                        "id",
                        "work_id",
                        "kind",
                        "payload",
                        "digest",
                        "ledger_revision",
                    ][..],
                ),
                (
                    "owner_shared_work_observations",
                    &["id", "work_id", "payload", "digest", "ledger_revision"][..],
                ),
                (
                    "owner_shared_activity_renewals",
                    &["id", "child_id", "requested_epoch", "requested_revision"][..],
                ),
            ])
            .into_iter()
            .flatten(),
    )
    .chain(
        (schema >= 81)
            .then_some([
                (
                    "owner_shared_successor_attempts",
                    &[
                        "id",
                        "child_id",
                        "profile",
                        "process_attempt",
                        "predecessor_id",
                        "predecessor_digest",
                        "predecessor_stop_digest",
                        "start_intent_id",
                        "command_payload",
                        "command_digest",
                        "original_runtime_digest",
                        "binding_digest",
                        "provider_digest",
                        "observed_revision",
                        "issued_revision",
                        "incarnation",
                    ][..],
                ),
                (
                    "owner_shared_successor_observations",
                    &["id", "attempt_id", "payload", "digest", "ledger_revision"][..],
                ),
                (
                    "owner_shared_successor_fences",
                    &[
                        "command_id",
                        "child_id",
                        "profile",
                        "process_attempt",
                        "predecessor_id",
                        "start_intent_id",
                        "command_payload",
                        "command_digest",
                        "fence_revision",
                        "claimed_revision",
                    ][..],
                ),
                (
                    "owner_shared_successor_call_pins",
                    &["call_id", "attempt_id", "work_id", "execution_digest"][..],
                ),
            ])
            .into_iter()
            .flatten(),
    )
    .chain(
        (schema >= 82)
            .then_some([(
                "owner_shared_start_intents",
                &[
                    "id",
                    "child_id",
                    "profile",
                    "device_id",
                    "device_generation",
                    "predecessor_digest",
                    "original_runtime_digest",
                    "original_runtime_payload",
                    "predecessor_payload",
                    "input_payload",
                    "input_digest",
                    "ledger_revision",
                ][..],
            )])
            .into_iter()
            .flatten(),
    )
    .chain((schema >= 83).then_some((
        "owner_resource_display_names",
        &["profile", "resource_id", "name", "revision"][..],
    )))
    .chain(
        (schema >= 84)
            .then_some([
                (
                    "owner_operator_task_histories",
                    &["profile", "migration_id", "receipt_digest", "receipt"][..],
                ),
                (
                    "owner_operator_tasks",
                    &[
                        "profile",
                        "id",
                        "revision",
                        "title",
                        "description",
                        "status",
                        "priority",
                        "granularity",
                        "assignee",
                        "created_by",
                        "created_at",
                        "updated_at",
                        "started_at",
                        "completed_at",
                        "heartbeat_at",
                        "waiting_reason",
                        "waiting_until",
                        "parent_id",
                        "labels",
                    ][..],
                ),
                (
                    "owner_operator_task_comments",
                    &["profile", "task_id", "ordinal", "author", "text", "at_ms"][..],
                ),
            ])
            .into_iter()
            .flatten(),
    )
    .chain(
        (schema >= 85)
            .then_some([
                (
                    "owner_shared_demand_controls",
                    &["child_id", "profile", "control_revision", "stopped_through"][..],
                ),
                (
                    "owner_shared_demands",
                    &[
                        "id",
                        "child_id",
                        "profile",
                        "device_id",
                        "device_generation",
                        "event_id",
                        "offer_payload",
                        "offer_digest",
                        "ledger_revision",
                    ][..],
                ),
                (
                    "owner_shared_demand_decisions",
                    &[
                        "demand_id",
                        "disposition",
                        "start_intent_id",
                        "input_payload",
                        "input_digest",
                        "budget_payload",
                        "ledger_revision",
                    ][..],
                ),
                (
                    "owner_shared_demand_bindings",
                    &[
                        "demand_id",
                        "work_id",
                        "binding_payload",
                        "binding_digest",
                        "ledger_revision",
                    ][..],
                ),
            ])
            .into_iter()
            .flatten(),
    )
    .chain(
        (schema >= 86)
            .then_some([
                (
                    "owner_shared_stop_intents",
                    &[
                        "command_id",
                        "child_id",
                        "profile",
                        "device_id",
                        "device_generation",
                        "actor_mxid",
                        "assignment_id",
                        "selected_revision",
                        "input_payload",
                        "input_digest",
                        "ledger_revision",
                    ][..],
                ),
                (
                    "owner_shared_stop_receipts",
                    &["command_id", "applied_revision", "ledger_revision"][..],
                ),
            ])
            .into_iter()
            .flatten(),
    ).chain((schema>=87).then_some([
        ("owner_shared_input_modes", &["work_id","mode","ledger_revision"][..]),
        ("owner_shared_contexts", &["work_id","assignment_id","room_id","thread_root","ordinal","payload","digest","input_digest","ledger_revision"][..]),
        ("owner_shared_reply_deliveries", &["work_id","payload","digest","ledger_revision"][..]),
        ("owner_shared_context_call_pins", &["call_id","work_id","context_digest","input_digest","ledger_revision"][..]),
    ]).into_iter().flatten()).chain((schema >= 88).then_some([
        ("owner_operator_task_transfer_tasks", &["migration_id", "task_id", "payload_digest", "payload"][..]),
        ("owner_operator_task_transfer_comments", &["migration_id", "sequence", "task_id", "ordinal", "payload_digest", "payload"][..]),
    ]).into_iter().flatten()).chain((schema >= 89).then_some([
        ("owner_managed_budget_commands", &["command_id","child_id","profile","executor_device_id","top_up_id","command_digest","command_payload","receipt_payload","receipt_digest","ledger_revision"][..]),
        ("owner_managed_budget_deltas", &["command_id","child_id","budget_revision","previous_digest","budget_digest","applied_tokens","effective_ceiling_tokens","ledger_revision"][..]),
    ]).into_iter().flatten()).chain((schema >= 90).then_some([
        ("owner_shared_budget_start_intents", &["id","child_id","profile","device_id","device_generation","predecessor_digest","original_runtime_digest","original_runtime_payload","input_payload","input_digest","ledger_revision"][..]),
        ("owner_shared_budget_demand_decisions", &["demand_id","budget_digest","execution_budget_payload","disposition","start_intent_id","input_payload","input_digest","ledger_revision"][..]),
        ("owner_shared_budget_work_refusals", &["work_id","call_id","command_digest","refusal_payload","refusal_digest","observation_id","ledger_revision"][..]),
    ]).into_iter().flatten()).chain((schema >= 91).then_some(("management_account_profiles", &["account_id","profile"][..]))).chain((schema >= 92).then_some([
        ("owner_resource_namespaces", &["resource_id","account_id","generation","profile"][..]),
        ("owner_namespace_enrollments", &["command_id","profile","input_digest","resource_id"][..]),
    ]).into_iter().flatten()).chain((schema >= 93).then_some((
        "owner_resource_provenance",
        &["resource_id","registry_id","creation_command_id","creation_digest","creation_profile","first_offer_kind","first_offer_profile","first_offer_command_id","first_offer_revision","first_offer_at_ms"][..],
    ))).chain((schema >= 94).then_some((
        "approval_binding_incarnations", &["engagement_id", "incarnation"][..],
    ))).chain((schema >= 95).then_some((
        "owned_dispatch_execution_policies", &["dispatch_id", "engagement_id", "yolo", "captured_at"][..],
    ))).chain((schema >= 96).then_some([
        ("owner_operator_graph_archives", &["command_id","registry_id","profile","owner_mxid","source_binding","manifest","state"][..]),
        ("owner_operator_graph_archive_chunks", &["command_id","document","chunk_index","bytes"][..]),
    ]).into_iter().flatten()).chain((schema >= 97).then_some([
        ("owner_operator_graphs", &["registry_id","profile","owner_mxid","scope_id","graph_id","revision","document","document_digest"][..]),
        ("owner_operator_graph_commands", &["profile","command_id","registry_id","owner_mxid","scope_id","graph_id","expected_revision","input_digest","request_digest","receipt"][..]),
        ("owner_operator_graph_outbox", &["registry_id","profile","owner_mxid","scope_id","graph_id","node_id","message_id","payload","payload_digest","state"][..]),
    ]).into_iter().flatten()).chain((schema >= 98).then_some((
        "owner_operator_graph_recipients", &["registry_id","profile","owner_mxid","scope_id","graph_id","node_id","revision","snapshot","snapshot_digest"][..],
    ))).chain((schema >= 99).then_some((
        "owner_web_catalog_settings", &["profile","kind","entity","value"][..],
    ))).chain((schema >= 100).then_some([
        ("owner_managed_agents", &["pool_grant_id","project_reservation_id"][..]),
        ("owner_managed_fences", &["pool_grant_id","project_reservation_id"][..]),
    ]).into_iter().flatten()).chain((schema >= 101).then_some((
        "owner_shared_tool_policies", &["child_id", "yolo", "revision"][..],
    ))).chain((schema >= 102).then_some([
        ("owner_shared_unstarted_attempts", &["command_id", "payload", "digest", "ledger_revision"][..]),
        ("owner_shared_budget_demand_terminals", &["demand_id", "payload", "digest", "ledger_revision"][..]),
    ]).into_iter().flatten()).chain((schema >= 104).then_some([
        ("owner_capacity_grant_successions", &["grant_id", "predecessor_grant_id", "profile", "kind", "state", "effective_from_ms", "command_id", "created_at_ms", "decided_at_ms"][..]),
        ("owner_managed_fence_times", &["child_id", "fenced_at_ms"][..]),
    ]).into_iter().flatten()).chain((schema >= 105).then_some((
        "owner_resource_ceiling_amendments",
        &["resource_id","sequence","profile","command_id","base_revision","previous_tokens","tokens","period","created_at_ms"][..],
    ))).chain((schema >= 107).then_some([
        ("owner_managed_agents", &["allocation_generation"][..]),
        ("owner_managed_fences", &["allocation_generation"][..]),
        ("owner_managed_periods", &["id","child_id","profile","agent_id","period_key","start_ms","end_ms","grant_id","tokens","command_id","command_digest","ledger_revision"][..]),
        ("owner_managed_call_periods", &["call_id","period_id"][..]),
    ]).into_iter().flatten()).chain((schema >= 108).then_some((
        "owner_resource_calendars", &["resource_id","profile","calendar","source_grant_id","created_at_ms"][..]
    ))).chain((schema >= 109).then_some(("owner_shared_start_intents", &["period_id"][..])))
    .chain((schema >= 110).then_some(("owner_shared_unstarted_runtimes", &["command_id","child_id","profile","command_payload","command_digest","receipt","receipt_digest","ledger_revision"][..])))
    .chain((schema >= 111).then_some([
        ("owner_shared_native_turns", &["work_id","scope_digest","process_digest","opened_revision"][..]),
        ("owner_shared_native_checkpoints", &["work_id","native_id","receipt_id"][..]),
    ]).into_iter().flatten()) {
        let ordinary = tx
            .query_row(
                "SELECT type='table' FROM pragma_table_list WHERE schema='main' AND name=?1",
                [required],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false);
        if !ordinary {
            return Err(Error::Schema);
        }
        let columns = tx.prepare("SELECT CASE WHEN length(CAST(name AS BLOB))<=128 THEN name END FROM pragma_table_info(?1) LIMIT 33")?
            .query_map([required], |row| row.get::<_, Option<String>>(0))?.collect::<Result<Vec<_>,_>>()?;
        if columns.len() > 32
            || columns.iter().any(Option::is_none)
            || expected
                .iter()
                .any(|name| !columns.iter().flatten().any(|actual| actual == name))
        {
            return Err(Error::Schema);
        }
    }
    let bound: Option<String> = tx
        .query_row(
            "SELECT profile_digest FROM management_identity WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if bound.as_deref() != Some(profile) {
        return Err(Error::LocalAuthority);
    }
    let tables = tx
        .prepare("SELECT CASE WHEN length(name)<=128 THEN name END FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1")?
        .query_map([MAX_TABLES as u32 + 1], |r| r.get::<_, Option<String>>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if tables.len() > MAX_TABLES || tables.iter().any(Option::is_none) {
        return Err(Error::Capacity);
    }
    let optional_history_tables_missing = [
        "owner_resource_identity",
        "owner_resource_accounts",
        "owner_resource_grants",
        "owner_resource_receipts",
        "owner_capacity_grants",
        "owner_project_reservations",
        "owner_capacity_receipts",
        "owner_parent_settlements",
        "owner_project_authorities",
        "owner_managed_agents",
        "owner_managed_effects",
        "owner_managed_calls",
        "owner_managed_assignment_pins",
        "owner_managed_call_pins",
        "owner_managed_observations",
        "owner_managed_fences",
        "owner_resource_adoptions",
        "owner_managed_runtime_lifetimes",
        "owner_managed_runtime_stops",
        "owner_managed_runtime_observations",
        "owner_shared_runtime_lifetimes",
        "owner_shared_process_observations",
        "owner_shared_work_inbox",
        "owner_shared_work_events",
        "owner_shared_work_observations",
        "owner_shared_activity_renewals",
        "owner_shared_successor_attempts",
        "owner_shared_successor_observations",
        "owner_shared_successor_call_pins",
        "owner_shared_successor_fences",
        "owner_shared_start_intents",
        "owner_resource_display_names",
        "owner_operator_task_histories",
        "owner_operator_tasks",
        "owner_operator_task_comments",
        "owner_shared_demand_controls",
        "owner_shared_demands",
        "owner_shared_demand_decisions",
        "owner_shared_demand_bindings",
        "owner_shared_stop_intents",
        "owner_shared_stop_receipts",
        "owner_shared_input_modes",
        "owner_shared_contexts",
        "owner_shared_reply_deliveries",
        "owner_shared_context_call_pins",
        "owner_operator_task_transfer_tasks",
        "owner_operator_task_transfer_comments",
        "owner_managed_budget_commands",
        "owner_managed_budget_deltas",
    ]
    .iter()
    .filter(|name| !tables.iter().flatten().any(|table| table == **name))
    .count() as u32;
    let mut history = ProfileLedgerHistory::default();
    let mut counts_complete = true;
    for table in tables.iter().flatten() {
        let quoted = table.replace('"', "\"\"");
        let count = count(&tx, &format!("SELECT 1 FROM \"{quoted}\""))?;
        counts_complete &= !count.capped;
        match table.as_str() {
            "management_identity" => {
                if count.observed != 1 {
                    return Err(Error::Schema);
                }
            }
            "resources" => history.resource_definitions = count,
            "seats" => history.seat_definitions = count,
            "management_commands" => history.command_receipts = count,
            "management_intents" => {}
            "engagements"
            | "usage_sources"
            | "usage_receipts"
            | "usage_periods"
            | "coordinator_resources"
            | "owner_capacity_grants"
            | "owner_capacity_grant_successions"
            | "owner_project_reservations"
            | "owner_parent_settlements"
            | "owner_resource_ceiling_amendments"
            | "owner_shared_unstarted_runtimes"
            | "owner_resource_calendars"
            | "owner_managed_periods"
            | "owner_managed_call_periods" => {
                history.capacity_and_usage_rows.add(&count);
            }
            "effects" | "runner_dispatches" | "runner_attempts" | "owned_task_completions" => {
                history.execution_rows.add(&count)
            }
            _ => {
                if count.observed > 0 {
                    history.other_populated_tables += 1;
                }
                history.other_history_rows.add(&count);
            }
        }
    }
    history.pending_intents = count(
        &tx,
        "SELECT 1 FROM management_intents WHERE state='pending'",
    )?;
    history.replied_intents = count(
        &tx,
        "SELECT 1 FROM management_intents WHERE state='replied'",
    )?;
    history.acknowledged_intents = count(
        &tx,
        "SELECT 1 FROM management_intents WHERE state='acknowledged'",
    )?;
    counts_complete &= !history.pending_intents.capped
        && !history.replied_intents.capped
        && !history.acknowledged_intents.capped;

    let rows = tx.prepare("SELECT CASE WHEN length(id)<=128 THEN id END, CASE WHEN length(CAST(config AS BLOB))<=?1 THEN config END FROM resources ORDER BY id LIMIT ?2")?
        .query_map(params![MAX_RESOURCE_BYTES as u32, MAX_RESOURCES as u32 + 1], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    let resources_complete = rows.len() <= MAX_RESOURCES;
    let mut resources = Vec::new();
    for (id, json) in rows.into_iter().take(MAX_RESOURCES) {
        let id = id.ok_or(Error::Schema)?;
        let resource: Resource = serde_json::from_str(&json.ok_or(Error::Capacity)?)?;
        resource.validate()?;
        if id != resource.id() {
            return Err(Error::Schema);
        }
        resources.push(AuditResource {
            id,
            revision: resource_publication_revision(&resource)?,
        });
    }
    let mut blockers = vec![
        AuditBlocker::ProviderMappingNotEstablished,
        AuditBlocker::PersonalLedgerNotInspected,
        AuditBlocker::SourceCensusNotEstablished,
    ];
    if schema != crate::DOMAIN_SCHEMA_VERSION || optional_history_tables_missing > 0 {
        blockers.push(AuditBlocker::OlderSourceSchema);
    }
    if history.pending_intents.observed > 0 || history.replied_intents.observed > 0 {
        blockers.push(AuditBlocker::PendingPrivateCommands);
    }
    if history.capacity_and_usage_rows.observed > 0 {
        blockers.push(AuditBlocker::CapacityOrUsageHistory);
    }
    if history.execution_rows.observed > 0 {
        blockers.push(AuditBlocker::ExecutionHistory);
    }
    if history.other_history_rows.observed > 0 {
        blockers.push(AuditBlocker::OtherHistory);
    }
    if !counts_complete || !resources_complete {
        blockers.push(AuditBlocker::ObservationLimit);
    }
    let result = ProfileLedgerAudit {
        version: 1,
        schema_version: schema,
        optional_history_tables_missing,
        observed_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|time| u64::try_from(time.as_millis()).ok())
            .ok_or(Error::Unavailable)?,
        scope: "current_profile_domain_only",
        adoption: "not_assessed",
        tables_observed: tables.len() as u32,
        counts_complete,
        resources_complete,
        resources,
        history,
        blockers,
    };
    // A read-only transaction commit performs no row/schema changes. It
    // does not make the snapshot valid for a later migration operation.
    tx.commit()?;
    Ok(result)
}

fn count(db: &Connection, selection: &str) -> Result<AuditCount, Error> {
    let rows: u32 = db.query_row(
        &format!("SELECT COUNT(*) FROM ({selection} LIMIT ?1)"),
        [MAX_ROWS + 1],
        |r| r.get(0),
    )?;
    Ok(AuditCount {
        observed: rows.min(MAX_ROWS),
        capped: rows > MAX_ROWS,
    })
}

#[cfg(test)]
mod tests;
