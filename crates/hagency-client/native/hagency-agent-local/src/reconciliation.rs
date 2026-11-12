//! Read-only observations on a connection already owned by the native host.
//! No opener, startup recovery, provider identity or adoption authority.
use super::*;
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_TABLES: usize = 64;
const MAX_ROWS: u32 = 10_000;
const MAX_USAGE_BYTES: u32 = 2048;

/// Construct only from current native authentication and its synchronous live
/// fence. Neither this proof nor its profile may come from mini-app arguments.
pub struct PersonalAuditAuthority {
    owner: String,
    profile: String,
    expires: Instant,
    live: Arc<AtomicBool>,
    native_gate: Arc<dyn Fn() -> bool + Send + Sync>,
}
impl PersonalAuditAuthority {
    pub fn new(
        owner: String,
        profile: String,
        expires: Instant,
        native_gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        key(&owner)?;
        if !owner.starts_with('@')
            || !owner.contains(':')
            || owner.len() > 255
            || profile.len() != 64
            || !profile
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || expires > Instant::now() + Duration::from_secs(60)
        {
            return Err(Error::Unauthorized);
        }
        let result = Self {
            owner,
            profile,
            expires,
            live: Arc::new(AtomicBool::new(true)),
            native_gate: Arc::new(native_gate),
        };
        result.check()?;
        Ok(result)
    }
    pub fn retire(&self) {
        self.live.store(false, Ordering::Release);
    }
    fn check(&self) -> Result<()> {
        if !self.live.load(Ordering::Acquire)
            || Instant::now() >= self.expires
            || !(self.native_gate)()
        {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
}

/// A capped observation is a lower bound, never a complete census.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalAuditCount {
    pub observed: u32,
    pub capped: bool,
}
impl PersonalAuditCount {
    fn add(&mut self, other: &Self) {
        self.observed += other.observed;
        self.capped |= other.capped;
    }
}
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalLedgerHistory {
    pub calls: PersonalAuditCount,
    pub pending_calls: u32,
    pub unknown_calls: u32,
    pub settled_calls: u32,
    pub invalid_call_states: u32,
    pub missing_or_invalid_usage: u32,
    pub unreported_zero_usage: u32,
    pub reported_usage_records: u32,
    pub no_provider_usage_records: u32,
    pub unsupported_accounting_records: u32,
    pub inbox_records: PersonalAuditCount,
    pub queued_executions: u32,
    pub active_executions: u32,
    pub reply_ready_executions: u32,
    pub terminal_executions: u32,
    pub unknown_executions: u32,
    pub invalid_execution_states: u32,
    pub account_rows: PersonalAuditCount,
    pub positive_held_rows: u32,
    pub positive_spent_rows: u32,
    pub invalid_accounting_rows: u32,
    pub continuity_rows: PersonalAuditCount,
    pub explicit_usage_gaps: u32,
    pub missing_or_oversized_counters: u32,
    pub private_history_rows: PersonalAuditCount,
    pub other_history_rows: PersonalAuditCount,
    pub other_populated_tables: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalAuditBlocker {
    ProviderMappingNotEstablished,
    SourceCensusNotEstablished,
    MigrationNotAssessed,
    PendingCalls,
    UncertainExecutions,
    MissingOrUnreportedUsage,
    RetainedAccounting,
    ExecutionHistory,
    PrivateHistory,
    UnknownHistory,
    UnsupportedAccounting,
    ObservationLimit,
}
/// Display-only evidence. It deliberately cannot be deserialized as authority.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalLedgerAudit {
    pub version: u16,
    pub schema_version: i32,
    pub scope: &'static str,
    pub adoption: &'static str,
    pub observed_at_ms: u64,
    pub tables_observed: u32,
    pub counts_complete: bool,
    pub history: PersonalLedgerHistory,
    pub blockers: Vec<PersonalAuditBlocker>,
}

// user_version=1 permits these additive current columns; the audit must never
// initialize a legacy shape or assume a missing accounting column means zero.
const REQUIRED: &[(&str, &[&str])] = &[
    ("identity", &["owner"]),
    ("profile_identity", &["singleton", "digest"]),
    ("bindings", &["id", "agent", "room"]),
    ("policies", &["scope", "revision", "config"]),
    ("accounts", &["scope", "window", "spent", "held"]),
    (
        "calls",
        &[
            "id",
            "binding",
            "scope",
            "digest",
            "reserved",
            "snapshots",
            "state",
            "usage",
        ],
    ),
    (
        "approvals",
        &["digest", "binding", "proposal", "expires", "consumed"],
    ),
    ("contexts", &["scope", "model_session"]),
    (
        "model_profiles",
        &[
            "agent",
            "model",
            "credential_ref",
            "workspace_root",
            "reasoning_effort",
            "web_search",
        ],
    ),
    ("dispatches", &["id", "scope", "policies"]),
    ("codex_usage", &["scope", "counters"]),
    (
        "external_sessions",
        &["scope", "profile", "session", "counters"],
    ),
    (
        "local_inbox",
        &[
            "id",
            "binding",
            "event_id",
            "immutable_digest",
            "envelope",
            "state",
            "execution_id",
            "reply",
            "reply_digest",
            "received_at",
        ],
    ),
    ("inbox_limits", &["singleton", "max_records", "max_bytes"]),
];

impl Ledger {
    /// Inspect this owned connection without startup recovery or migration.
    pub fn reconciliation_audit(
        &mut self,
        authority: &PersonalAuditAuthority,
    ) -> Result<PersonalLedgerAudit> {
        authority.check()?;
        self.owner(&authority.owner)?;
        collect(&mut self.db, authority)
    }
}

/// Inspect an existing read-only connection without constructing a Ledger.
/// The caller retains the original filesystem evidence and current native scope.
pub fn audit_readonly_connection(
    db: &mut Connection,
    authority: &PersonalAuditAuthority,
) -> Result<PersonalLedgerAudit> {
    authority.check()?;
    if !db.is_readonly("main")? {
        return Err(Error::Unauthorized);
    }
    collect(db, authority)
}

fn collect(db: &mut Connection, authority: &PersonalAuditAuthority) -> Result<PersonalLedgerAudit> {
    if !db.is_autocommit() {
        return Err(Error::Conflict);
    }
    let tx = db.transaction_with_behavior(TransactionBehavior::Deferred)?;
    authority.check()?;
    let application: i64 = tx.pragma_query_value(None, "application_id", |r| r.get(0))?;
    let version: i32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if application != APP_ID || version != 1 {
        return Err(Error::ForeignDatabase);
    }
    let tables = schema(&tx, authority)?;
    let owners: Vec<Option<String>> = tx.prepare("SELECT CASE WHEN length(CAST(owner AS BLOB))<=255 THEN owner END FROM identity LIMIT 2")?
        .query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    let profiles: Vec<Option<String>> = tx.prepare("SELECT CASE WHEN length(CAST(digest AS BLOB))=64 THEN digest END FROM profile_identity LIMIT 2")?
        .query_map([], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
    if owners != [Some(authority.owner.clone())] || profiles != [Some(authority.profile.clone())] {
        return Err(Error::Unauthorized);
    }
    let mut history = PersonalLedgerHistory::default();
    let mut complete = true;
    for table in &tables {
        authority.check()?;
        let count = count(&tx, table)?;
        complete &= !count.capped;
        match table.as_str() {
            "identity" | "profile_identity" | "inbox_limits" => {}
            "calls" => {
                history.calls = count;
                calls(&tx, authority, &mut history)?;
            }
            "accounts" => {
                history.account_rows = count;
                accounts(&tx, authority, &mut history)?;
            }
            "local_inbox" => {
                history.inbox_records = count;
                inbox(&tx, authority, &mut history)?;
            }
            "codex_usage" | "external_sessions" => {
                history.continuity_rows.add(&count);
                continuity(&tx, table, authority, &mut history)?;
            }
            "bindings" | "policies" | "approvals" | "contexts" | "model_profiles"
            | "dispatches" => {
                history.private_history_rows.add(&count);
            }
            _ => {
                history.other_populated_tables += u32::from(count.observed > 0);
                history.other_history_rows.add(&count);
            }
        }
        authority.check()?;
    }
    let mut blockers = vec![
        PersonalAuditBlocker::ProviderMappingNotEstablished,
        PersonalAuditBlocker::SourceCensusNotEstablished,
        PersonalAuditBlocker::MigrationNotAssessed,
    ];
    if history.pending_calls > 0 {
        blockers.push(PersonalAuditBlocker::PendingCalls);
    }
    if history.unknown_calls > 0 || history.active_executions > 0 || history.unknown_executions > 0
    {
        blockers.push(PersonalAuditBlocker::UncertainExecutions);
    }
    if history.missing_or_invalid_usage > 0
        || history.unreported_zero_usage > 0
        || history.explicit_usage_gaps > 0
        || history.missing_or_oversized_counters > 0
    {
        blockers.push(PersonalAuditBlocker::MissingOrUnreportedUsage);
    }
    if history.positive_held_rows > 0 || history.positive_spent_rows > 0 {
        blockers.push(PersonalAuditBlocker::RetainedAccounting);
    }
    if history.calls.observed > 0 || history.inbox_records.observed > 0 {
        blockers.push(PersonalAuditBlocker::ExecutionHistory);
    }
    if history.private_history_rows.observed > 0 || history.continuity_rows.observed > 0 {
        blockers.push(PersonalAuditBlocker::PrivateHistory);
    }
    if history.other_history_rows.observed > 0
        || history.invalid_call_states > 0
        || history.invalid_execution_states > 0
    {
        blockers.push(PersonalAuditBlocker::UnknownHistory);
    }
    if history.unsupported_accounting_records > 0 || history.invalid_accounting_rows > 0 {
        blockers.push(PersonalAuditBlocker::UnsupportedAccounting);
    }
    if !complete {
        blockers.push(PersonalAuditBlocker::ObservationLimit);
    }
    let observed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|v| u64::try_from(v.as_millis()).ok())
        .filter(|n| *n <= MAX)
        .ok_or(Error::Invalid("audit clock"))?;
    authority.check()?;
    tx.commit()?;
    authority.check()?;
    Ok(PersonalLedgerAudit {
        version: 1,
        schema_version: version,
        scope: "current_profile_personal_snapshot",
        adoption: "not_assessed",
        observed_at_ms: observed,
        tables_observed: tables.len() as u32,
        counts_complete: complete,
        history,
        blockers,
    })
}

fn schema(db: &Connection, authority: &PersonalAuditAuthority) -> Result<Vec<String>> {
    let rows = db.prepare("SELECT CASE WHEN length(CAST(name AS BLOB))<=128 THEN name END, type FROM sqlite_schema WHERE type IN ('table','view') AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1")?
        .query_map([MAX_TABLES as u32 + 1], |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, String>(1)?)))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if rows.len() > MAX_TABLES {
        return Err(Error::Invalid("audit observation limit"));
    }
    let mut tables = Vec::new();
    for (name, kind) in rows {
        authority.check()?;
        let name = name.ok_or(Error::ForeignDatabase)?;
        let ordinary: bool = db.query_row(
            "SELECT type='table' FROM pragma_table_list WHERE schema='main' AND name=?1",
            [&name],
            |r| r.get(0),
        )?;
        if kind != "table" || !ordinary {
            return Err(Error::ForeignDatabase);
        }
        tables.push(name);
    }
    for (name, expected) in REQUIRED {
        authority.check()?;
        if !tables.iter().any(|table| table == name) {
            return Err(Error::ForeignDatabase);
        }
        let columns = db.prepare("SELECT CASE WHEN length(CAST(name AS BLOB))<=128 THEN name END FROM pragma_table_info(?1) LIMIT 33")?
            .query_map([name], |r| r.get::<_, Option<String>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if columns.len() > 32
            || columns.iter().any(Option::is_none)
            || columns
                .iter()
                .flatten()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != expected.iter().copied().collect()
        {
            return Err(Error::ForeignDatabase);
        }
    }
    Ok(tables)
}
fn quoted(table: &str) -> String {
    format!("\"{}\"", table.replace('"', "\"\""))
}
fn count(db: &Connection, table: &str) -> Result<PersonalAuditCount> {
    let n: u32 = db.query_row(
        &format!(
            "SELECT COUNT(*) FROM (SELECT 1 FROM {} LIMIT ?1)",
            quoted(table)
        ),
        [MAX_ROWS + 1],
        |r| r.get(0),
    )?;
    Ok(PersonalAuditCount {
        observed: n.min(MAX_ROWS),
        capped: n > MAX_ROWS,
    })
}
fn valid_number(value: rusqlite::types::ValueRef<'_>) -> Option<u64> {
    match value {
        rusqlite::types::ValueRef::Integer(n) if n >= 0 && n as u64 <= MAX => Some(n as u64),
        _ => None,
    }
}
fn calls(db: &Connection, a: &PersonalAuditAuthority, h: &mut PersonalLedgerHistory) -> Result<()> {
    let mut statement = db.prepare("SELECT CASE WHEN length(CAST(state AS BLOB))<=32 THEN state END,reserved,CASE WHEN length(CAST(usage AS BLOB))<=?1 THEN usage END FROM calls LIMIT ?2")?;
    let mut rows = statement.query((MAX_USAGE_BYTES, MAX_ROWS))?;
    while let Some(row) = rows.next()? {
        a.check()?;
        let state: Option<String> = row.get(0)?;
        match state.as_deref() {
            Some("pending") => h.pending_calls += 1,
            Some("unknown") => h.unknown_calls += 1,
            Some("settled") => h.settled_calls += 1,
            _ => h.invalid_call_states += 1,
        }
        if valid_number(row.get_ref(1)?).is_none() {
            h.invalid_accounting_rows += 1;
        }
        let encoded: Option<String> = row.get(2)?;
        let usage = encoded
            .as_deref()
            .and_then(|s| serde_json::from_str::<Usage>(s).ok())
            .filter(|u| u.total().is_ok());
        if let Some(usage) = usage {
            match usage.accounting_version.as_str() {
                UNREPORTED_ZERO => h.unreported_zero_usage += 1,
                UNREPORTED_USAGE => h.missing_or_invalid_usage += 1,
                "no-provider-call-v1" if usage.total()? == 0 => h.no_provider_usage_records += 1,
                "no-provider-call-v1" => h.missing_or_invalid_usage += 1,
                "codex-app-server-0.160/cumulative-input-output-v1"
                | "claude-reported-v1"
                | "octos-reported-v1" => h.reported_usage_records += 1,
                _ => h.unsupported_accounting_records += 1,
            }
        } else {
            h.missing_or_invalid_usage += 1;
        }
    }
    Ok(())
}
fn accounts(
    db: &Connection,
    a: &PersonalAuditAuthority,
    h: &mut PersonalLedgerHistory,
) -> Result<()> {
    let mut statement = db.prepare("SELECT spent,held FROM accounts LIMIT ?1")?;
    let mut rows = statement.query([MAX_ROWS])?;
    while let Some(row) = rows.next()? {
        a.check()?;
        match (valid_number(row.get_ref(0)?), valid_number(row.get_ref(1)?)) {
            (Some(spent), Some(held)) => {
                h.positive_spent_rows += u32::from(spent > 0);
                h.positive_held_rows += u32::from(held > 0);
            }
            _ => h.invalid_accounting_rows += 1,
        }
    }
    Ok(())
}
fn inbox(db: &Connection, a: &PersonalAuditAuthority, h: &mut PersonalLedgerHistory) -> Result<()> {
    let mut statement = db.prepare(
        "SELECT CASE WHEN length(CAST(state AS BLOB))<=32 THEN state END FROM local_inbox LIMIT ?1",
    )?;
    let mut rows = statement.query([MAX_ROWS])?;
    while let Some(row) = rows.next()? {
        a.check()?;
        match row.get::<_, Option<String>>(0)?.as_deref() {
            Some("received" | "acknowledged") => h.queued_executions += 1,
            Some("prepared" | "running") => h.active_executions += 1,
            Some("reply_ready") => h.reply_ready_executions += 1,
            Some("replied" | "rejected" | "failed") => h.terminal_executions += 1,
            Some("unknown") => h.unknown_executions += 1,
            _ => h.invalid_execution_states += 1,
        }
    }
    Ok(())
}
fn continuity(
    db: &Connection,
    table: &str,
    a: &PersonalAuditAuthority,
    h: &mut PersonalLedgerHistory,
) -> Result<()> {
    let mut statement = db.prepare(&format!(
        "SELECT CASE WHEN length(CAST(counters AS BLOB))<=?1 THEN counters END FROM {} LIMIT ?2",
        quoted(table)
    ))?;
    let mut rows = statement.query((MAX_USAGE_BYTES, MAX_ROWS))?;
    while let Some(row) = rows.next()? {
        a.check()?;
        let value: Option<String> = row.get(0)?;
        match value
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        {
            Some(serde_json::Value::Null) => h.explicit_usage_gaps += 1,
            Some(serde_json::Value::Object(_)) => {}
            _ => h.missing_or_oversized_counters += 1,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
