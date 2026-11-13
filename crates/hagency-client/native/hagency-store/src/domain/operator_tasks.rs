//! The operator's own task list (TS parity: `lib/task-store.js`,
//! `backend-v2.js:13194-13332`).
//!
//! This is the RETAINED operator store, not the runner task protocol: a task
//! here has no session, no dispatch and no capability, and the operator
//! creates, edits, comments on and deletes it with no agent in the loop. The
//! retained store is an in-memory map persisted to `tasks.json`; the native
//! store is `operator_tasks` + `operator_task_comments` (migration 040). The
//! canonical runner task (`canonical_tasks`, migration 003) is a different
//! object with a different mutation path and is deliberately untouched.
//!
//! Every rule below is the retained rule, including its pleasant ones: a
//! non-string `title` on PATCH is IGNORED rather than refused
//! (`lib/task-store.js:168-171`, `normalizeText` returns null and the setter
//! is skipped), a non-array `labels` clears the labels
//! (`:190-192`), and `updated_at` only moves when something actually changed.
use super::{DomainRepository, serialize};
use crate::Error;
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use serde_json::{Value, json};

/// The retained vocabulary (`lib/task-store.js:3-5`, `:8-13`).
pub const TASK_STATUSES: [&str; 5] = ["created", "accepted", "in_progress", "blocked", "done"];
pub const TASK_PRIORITIES: [&str; 4] = ["p0", "p1", "p2", "p3"];
pub const TASK_GRANULARITIES: [&str; 3] = ["epic", "task", "subtask"];

/// Maximum comments on one task (`lib/task-store.js:301`).
pub const MAX_TASK_COMMENTS: usize = 100;
/// Publication bound: one operator read returns at most this many rows. The
/// retained `parseTaskPageLimit` CLAMPS to 500 (`backend-v2.js:13145-13149`);
/// native keeps the retained ceiling and refuses a page beyond it, the way
/// every other bounded read in this store behaves.
pub const MAX_TASK_PAGE: usize = 500;
/// Durable bound on the operator task table. The retained store is an
/// in-memory map with no bound at all; a durable table needs one, and this is
/// the same order as the other operator-written tables (`bounded_row`
/// callers use 10,000-100,000).
const MAX_OPERATOR_TASKS: i64 = 100_000;
/// Retained per-field bounds (`normalizeText` call sites).
const TITLE_MAX: usize = 255;
const DESCRIPTION_MAX: usize = 4096;
const ASSIGNEE_MAX: usize = 128;
const COMMENT_MAX: usize = 4096;
const WAITING_REASON_MAX: usize = 1024;
const WAITING_UNTIL_MAX: usize = 64;
const PARENT_MAX: usize = 64;
const LABEL_MAX: usize = 64;
const LABELS_MAX: usize = 20;

/// The legal status pairs (`TRANSITIONS`, `lib/task-store.js:8-13`). Served to
/// the console on every task row as `next`, so the page renders a transition
/// control only where the server allowed one — the one server-owned map, the
/// same contract the alerts read uses. Named `operator_transitions` because
/// `allowed_transitions` is already the ceiling-alert map.
pub fn operator_transitions(status: &str) -> &'static [&'static str] {
    match status {
        "created" => &["accepted"],
        "accepted" => &["in_progress"],
        "in_progress" => &["blocked", "done"],
        "blocked" => &["in_progress"],
        _ => &[],
    }
}

/// One operator task on the wire. Timestamps are the retained ISO-8601 UTC
/// strings — the retained route serves them and the console renders them, so
/// this is the wire shape, not an internal figure.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, serde::Deserialize)]
pub struct OperatorTask {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub granularity: String,
    pub assignee: Option<String>,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub heartbeat_at: Option<String>,
    pub waiting_reason: Option<String>,
    pub waiting_until: Option<String>,
    pub parent_id: Option<String>,
    pub labels: Vec<String>,
    pub comments: Vec<OperatorTaskComment>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, serde::Deserialize)]
pub struct OperatorTaskComment {
    pub author: String,
    pub text: String,
    pub ts: String,
}

/// The list filters `GET /api/tasks` accepts (`backend-v2.js:13212-13220`).
#[derive(Debug, Default, Clone)]
pub struct TaskFilters {
    pub assignee: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub label: Option<String>,
    pub offset: u64,
    pub limit: Option<usize>,
}

/// `normalizeText` (`lib/task-store.js:23-27`): trim, empty means absent, and
/// truncate to the bound. Never an error — the retained normalizer has none.
fn trimmed(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    Some(text.chars().take(max).collect())
}

/// `normalizeLabels` (`lib/task-store.js:29-41`).
fn labels(value: Option<&Value>) -> Vec<String> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        if out.len() >= LABELS_MAX {
            break;
        }
        let Some(label) = trimmed(Some(item), LABEL_MAX) else {
            continue;
        };
        if !out.contains(&label) {
            out.push(label);
        }
    }
    out
}

/// `generateId` (`lib/task-store.js:17-21`): `task_<unix seconds>_<6 chars>`.
fn generate_id(now: u64) -> Result<String, Error> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let suffix: String = bytes
        .iter()
        .take(6)
        .map(|b| ALPHABET[usize::from(*b) % 36] as char)
        .collect();
    Ok(format!("task_{}_{suffix}", now / 1000))
}

/// ISO-8601 UTC with milliseconds, the retained `new Date(...).toISOString()`
/// spelling. Hand-rolled because the store's `time` pin carries no formatting
/// feature and a second date dependency for one wire string is not worth it.
pub fn iso8601(ms: u64) -> String {
    let seconds = ms / 1000;
    let millis = ms % 1000;
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to a calendar date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// The retained store's own error words, as the console names them
/// (`respondTaskStoreError`, `backend-v2.js:13151-13156`).
pub fn invalid(code: &str) -> Error {
    match code {
        "invalid_title" => hagency_core::InvalidInput("title is required"),
        "invalid_priority" => hagency_core::InvalidInput("invalid priority"),
        "invalid_granularity" => hagency_core::InvalidInput("invalid granularity"),
        "invalid_parent" => hagency_core::InvalidInput("parent task not found"),
        "invalid_status" => hagency_core::InvalidInput("invalid status"),
        "invalid_transition" => hagency_core::InvalidInput("invalid transition"),
        "missing_waiting_reason" => hagency_core::InvalidInput("waiting_reason is required"),
        "missing_waiting_until" => hagency_core::InvalidInput("waiting_until is required"),
        "invalid_comment" => hagency_core::InvalidInput("comment text is required"),
        "limit_exceeded" => hagency_core::InvalidInput("max comments per task"),
        _ => hagency_core::InvalidInput("invalid task command"),
    }
    .into()
}

/// The stored row, before comments are attached and before ISO conversion.
type Row = (
    String,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    u64,
    u64,
    Option<u64>,
    Option<u64>,
    Option<u64>,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
);

const SELECT: &str = "SELECT id,title,description,status,priority,granularity,assignee,created_by,\
     created_at,updated_at,started_at,completed_at,heartbeat_at,waiting_reason,waiting_until,\
     parent_id,labels FROM operator_tasks";

fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        r.get(7)?,
        r.get(8)?,
        r.get(9)?,
        r.get(10)?,
        r.get(11)?,
        r.get(12)?,
        r.get(13)?,
        r.get(14)?,
        r.get(15)?,
        r.get(16)?,
    ))
}

fn task_row(
    (
        id,
        title,
        description,
        status,
        priority,
        granularity,
        assignee,
        created_by,
        created_at,
        updated_at,
        started_at,
        completed_at,
        heartbeat_at,
        waiting_reason,
        waiting_until,
        parent_id,
        labels,
    ): Row,
    comments: Vec<OperatorTaskComment>,
) -> Result<OperatorTask, Error> {
    Ok(OperatorTask {
        id,
        title,
        description,
        status,
        priority,
        granularity,
        assignee,
        created_by,
        created_at: iso8601(created_at),
        updated_at: iso8601(updated_at),
        started_at: started_at.map(iso8601),
        completed_at: completed_at.map(iso8601),
        heartbeat_at: heartbeat_at.map(iso8601),
        waiting_reason,
        waiting_until,
        parent_id,
        labels: serde_json::from_str::<Vec<String>>(&labels).unwrap_or_default(),
        comments,
    })
}

/// The retained comments array, in insertion order (`sequence`). Bounded by
/// `MAX_TASK_COMMENTS` at write time, so this read is finite by construction.
fn comments(db: &rusqlite::Connection, id: &str) -> Result<Vec<OperatorTaskComment>, Error> {
    db.prepare(
        "SELECT author,body,created_at FROM operator_task_comments WHERE task_id=?1 \
             ORDER BY sequence LIMIT ?2",
    )?
    .query_map(params![id, MAX_TASK_COMMENTS as i64], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u64>(2)?,
        ))
    })?
    .map(|r| {
        let (author, text, ts) = r?;
        Ok(OperatorTaskComment {
            author,
            text,
            ts: iso8601(ts),
        })
    })
    .collect::<Result<Vec<_>, Error>>()
}

fn read_task(db: &rusqlite::Connection, id: &str) -> Result<OperatorTask, Error> {
    let stored = db
        .query_row(&format!("{SELECT} WHERE id=?1"), [id], row)
        .optional()?
        .ok_or(Error::NotFound)?;
    task_row(stored, comments(db, id)?)
}

/// The rows behind one list read. Filters are the retained ones
/// (`lib/task-store.js:150-157`), the paging is the retained route's
/// (`backend-v2.js:13220-13224`), applied in ONE statement so a row cannot
/// appear twice or vanish between the slice and the read.
fn list_rows(db: &rusqlite::Connection, filters: &TaskFilters) -> Result<Vec<String>, Error> {
    let mut where_parts: Vec<String> = Vec::new();
    let mut values: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(assignee) = &filters.assignee {
        values.push(assignee.clone().into());
        where_parts.push(format!("assignee=?{}", values.len()));
    }
    if let Some(status) = &filters.status {
        // A comma-separated list, as the retained filter accepts.
        let parts: Vec<&str> = status
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        if !parts.is_empty() {
            let mut clause = Vec::new();
            for part in parts {
                values.push(part.to_owned().into());
                clause.push(format!("status=?{}", values.len()));
            }
            where_parts.push(format!("({})", clause.join(" OR ")));
        }
    }
    if let Some(priority) = &filters.priority {
        values.push(priority.clone().into());
        where_parts.push(format!("priority=?{}", values.len()));
    }
    if let Some(label) = &filters.label {
        values.push(label.clone().into());
        where_parts.push(format!(
            "EXISTS(SELECT 1 FROM json_each(operator_tasks.labels) WHERE value=?{})",
            values.len()
        ));
    }
    let mut sql = SELECT.to_owned();
    if !where_parts.is_empty() {
        sql.push_str(&format!(" WHERE {}", where_parts.join(" AND ")));
    }
    // One ordering for every read: newest first, then id, so paging is
    // stable across calls.
    sql.push_str(" ORDER BY created_at DESC, id DESC");
    if let Some(limit) = filters.limit {
        sql.push_str(&format!(" LIMIT {}", limit.min(MAX_TASK_PAGE)));
        if filters.offset > 0 {
            sql.push_str(&format!(" OFFSET {}", filters.offset));
        }
    } else if filters.offset > 0 {
        sql.push_str(&format!(" LIMIT -1 OFFSET {}", filters.offset));
    }
    let mut query = db.prepare(&sql)?;
    Ok(query
        .query_map(rusqlite::params_from_iter(values), |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?)
}

/// The requirement that a referenced parent EXISTS (`lib/task-store.js:111-113`).
fn parent_exists(db: &rusqlite::Connection, parent: &str) -> Result<(), Error> {
    let exists: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM operator_tasks WHERE id=?1)",
        [parent],
        |r| r.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(invalid("invalid_parent"))
    }
}

fn version_guard(now: u64) -> Result<(), Error> {
    hagency_core::tasks::clock(now)?;
    Ok(())
}

impl DomainRepository {
    /// `createTask` (`lib/task-store.js:98-145`).
    pub fn create_operator_task(&mut self, body: &Value, now: u64) -> Result<OperatorTask, Error> {
        version_guard(now)?;
        let object = body.as_object().cloned().unwrap_or_default();
        let get = |key: &str| object.get(key);
        let Some(title) = trimmed(get("title"), TITLE_MAX) else {
            return Err(invalid("invalid_title"));
        };
        let priority = trimmed(get("priority"), 8).unwrap_or_else(|| "p2".to_owned());
        if !TASK_PRIORITIES.contains(&priority.as_str()) {
            return Err(invalid("invalid_priority"));
        }
        let granularity = trimmed(get("granularity"), 16).unwrap_or_else(|| "task".to_owned());
        if !TASK_GRANULARITIES.contains(&granularity.as_str()) {
            return Err(invalid("invalid_granularity"));
        }
        let assignee = trimmed(get("assignee"), ASSIGNEE_MAX);
        let created_by = trimmed(get("created_by"), ASSIGNEE_MAX);
        let parent_id = trimmed(get("parent_id"), PARENT_MAX);
        if let Some(parent) = &parent_id {
            parent_exists(&self.db, parent)?;
        }
        let description = trimmed(get("description"), DESCRIPTION_MAX).unwrap_or_default();
        let encoded = serialize(&labels(get("labels")))?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM operator_tasks", [], |r| r.get(0))?;
        if count >= MAX_OPERATOR_TASKS {
            return Err(Error::Capacity);
        }
        let mut id = generate_id(now)?;
        for _ in 0..10 {
            let taken: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM operator_tasks WHERE id=?1)",
                [&id],
                |r| r.get(0),
            )?;
            if !taken {
                break;
            }
            id = generate_id(now)?;
        }
        tx.execute(
            "INSERT INTO operator_tasks(id,title,description,status,priority,granularity,assignee,\
             created_by,created_at,updated_at,parent_id,labels) \
             VALUES(?1,?2,?3,'created',?4,?5,?6,?7,?8,?8,?9,?10)",
            params![
                id,
                title,
                description,
                priority,
                granularity,
                assignee,
                created_by,
                now,
                parent_id,
                encoded
            ],
        )?;
        let task = read_task(&tx, &id)?;
        tx.commit()?;
        Ok(task)
    }

    pub fn operator_task(&self, id: &str) -> Result<OperatorTask, Error> {
        read_task(&self.db, id)
    }

    pub fn operator_tasks(&self, filters: &TaskFilters) -> Result<Vec<OperatorTask>, Error> {
        if filters.limit.is_some_and(|l| l > MAX_TASK_PAGE) {
            return Err(hagency_core::InvalidInput("task page exceeds its bound").into());
        }
        list_rows(&self.db, filters)?
            .into_iter()
            .map(|id| read_task(&self.db, &id))
            .collect::<Result<Vec<_>, _>>()
    }

    /// `updateTask` (`lib/task-store.js:169-214`) — the operator's full-field
    /// edit. `updated_at` moves only when a field actually changed, and the
    /// write is skipped entirely when nothing did.
    pub fn update_operator_task(&mut self, id: &str, patch: &Value) -> Result<OperatorTask, Error> {
        let object = patch.as_object().cloned().unwrap_or_default();
        let get = |key: &str| object.get(key);
        let present = |key: &str| object.contains_key(key);
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<Row> = tx
            .query_row(&format!("{SELECT} WHERE id=?1"), [id], row)
            .optional()?;
        let Some(mut stored) = current else {
            return Err(Error::NotFound);
        };
        let mut changed = false;
        if present("title")
            && let Some(value) = trimmed(get("title"), TITLE_MAX)
        {
            stored.1 = value;
            changed = true;
        }
        if present("description") {
            stored.2 = trimmed(get("description"), DESCRIPTION_MAX).unwrap_or_default();
            changed = true;
        }
        if present("priority") {
            let value = trimmed(get("priority"), 8);
            match value {
                Some(v) if TASK_PRIORITIES.contains(&v.as_str()) => {
                    stored.4 = v;
                    changed = true;
                }
                _ => return Err(invalid("invalid_priority")),
            }
        }
        if present("granularity") {
            let value = trimmed(get("granularity"), 16);
            match value {
                Some(v) if TASK_GRANULARITIES.contains(&v.as_str()) => {
                    stored.5 = v;
                    changed = true;
                }
                _ => return Err(invalid("invalid_granularity")),
            }
        }
        if present("assignee") {
            stored.6 = trimmed(get("assignee"), ASSIGNEE_MAX);
            changed = true;
        }
        if present("labels") {
            stored.16 = serialize(&labels(get("labels")))?;
            changed = true;
        }
        if present("parent_id") {
            let parent = trimmed(get("parent_id"), PARENT_MAX);
            if let Some(parent) = &parent {
                parent_exists(&tx, parent)?;
            }
            stored.15 = parent;
            changed = true;
        }
        let updated_at = if changed { stored.9.max(1) } else { stored.9 };
        if changed {
            tx.execute(
                "UPDATE operator_tasks SET title=?2,description=?3,priority=?4,granularity=?5,\
                 assignee=?6,labels=?7,parent_id=?8,updated_at=?9 WHERE id=?1",
                params![
                    id, stored.1, stored.2, stored.4, stored.5, stored.6, stored.16, stored.15,
                    updated_at
                ],
            )?;
        }
        let task = read_task(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// `transitionTask` (`lib/task-store.js:246-291`). An illegal status or
    /// pair is refused BEFORE anything is written; the blocked metadata is
    /// validated before the mutation too.
    pub fn transition_operator_task(
        &mut self,
        id: &str,
        status: &str,
        extra: &Value,
        now: u64,
    ) -> Result<OperatorTask, Error> {
        version_guard(now)?;
        if !TASK_STATUSES.contains(&status) {
            return Err(invalid("invalid_status"));
        }
        let object = extra.as_object().cloned().unwrap_or_default();
        let reason = trimmed(object.get("waiting_reason"), WAITING_REASON_MAX);
        let until = trimmed(object.get("waiting_until"), WAITING_UNTIL_MAX);
        if status == "blocked" {
            if reason.is_none() {
                return Err(invalid("missing_waiting_reason"));
            }
            if until.is_none() {
                return Err(invalid("missing_waiting_until"));
            }
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<Row> = tx
            .query_row(&format!("{SELECT} WHERE id=?1"), [id], row)
            .optional()?;
        let Some(stored) = current else {
            return Err(Error::NotFound);
        };
        if !operator_transitions(&stored.3).contains(&status) {
            return Err(invalid("invalid_transition"));
        }
        let started = match status {
            "accepted" | "in_progress" => Some(stored.10.unwrap_or(now)),
            _ => stored.10,
        };
        let completed = if status == "done" {
            Some(now)
        } else {
            stored.11
        };
        let (waiting_reason, waiting_until) = match status {
            "blocked" => (reason, until),
            // Both `done` and `in_progress` clear the waiting metadata
            // (`lib/task-store.js:271-283`).
            "done" | "in_progress" => (None, None),
            _ => (stored.13, stored.14),
        };
        tx.execute(
            "UPDATE operator_tasks SET status=?2,updated_at=?3,started_at=?4,completed_at=?5,\
             waiting_reason=?6,waiting_until=?7 WHERE id=?1",
            params![
                id,
                status,
                now,
                started,
                completed,
                waiting_reason,
                waiting_until
            ],
        )?;
        let task = read_task(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// `addComment` (`lib/task-store.js:293-307`).
    pub fn comment_operator_task(
        &mut self,
        id: &str,
        comment: &Value,
        now: u64,
    ) -> Result<OperatorTask, Error> {
        version_guard(now)?;
        let object = comment.as_object().cloned().unwrap_or_default();
        let text =
            trimmed(object.get("text"), COMMENT_MAX).ok_or_else(|| invalid("invalid_comment"))?;
        let author =
            trimmed(object.get("author"), ASSIGNEE_MAX).unwrap_or_else(|| "anonymous".to_owned());
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: Option<String> = tx
            .query_row("SELECT id FROM operator_tasks WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if exists.is_none() {
            return Err(Error::NotFound);
        }
        let count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM operator_task_comments WHERE task_id=?1",
            [id],
            |r| r.get(0),
        )?;
        if count >= MAX_TASK_COMMENTS as i64 {
            return Err(invalid("limit_exceeded"));
        }
        tx.execute(
            "INSERT INTO operator_task_comments(task_id,author,body,created_at) VALUES(?1,?2,?3,?4)",
            params![id, author, text, now],
        )?;
        tx.execute(
            "UPDATE operator_tasks SET updated_at=?2 WHERE id=?1",
            params![id, now],
        )?;
        let task = read_task(&tx, id)?;
        tx.commit()?;
        Ok(task)
    }

    /// `deleteTask` (`lib/task-store.js:309-315`): a missing id is `None`, not
    /// an error — the route turns that into its 404.
    pub fn delete_operator_task(&mut self, id: &str) -> Result<Option<OperatorTask>, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<Row> = tx
            .query_row(&format!("{SELECT} WHERE id=?1"), [id], row)
            .optional()?;
        let Some(stored) = current else {
            return Ok(None);
        };
        let task = task_row(stored, comments(&tx, id)?)?;
        tx.execute("DELETE FROM operator_task_comments WHERE task_id=?1", [id])?;
        tx.execute("DELETE FROM operator_tasks WHERE id=?1", [id])?;
        tx.commit()?;
        Ok(Some(task))
    }

    /// The operator project board (TS parity: `GET /api/project-board`,
    /// `backend-v2.js:16025-16051` over `lib/project-board.js`).
    ///
    /// DELIBERATE, NAMED DIVERGENCE: the retained snapshot is built from
    /// Matrix groups, workflow bindings, agents with live filesystem
    /// `projectInspections`, the task graph store and the message corpus.
    /// Native has a source for exactly three of its columns — the registered
    /// projects, the engagements that name each project's agents, and the
    /// operator tasks. Everything the retained board shows that native cannot
    /// source is named in `unavailable` rather than served as zero, the way
    /// the roster (ADR-126) and project sides (ADR-132) already do.
    pub fn operator_project_board(&self, now: u64, activity_limit: u64) -> Result<Value, Error> {
        version_guard(now)?;
        let limit = activity_limit.clamp(1, 100);
        let mut projects: Vec<(String, String, Vec<String>)> = Vec::new();
        let mut query = self.db.prepare(
            "SELECT p.id,p.id,e.name FROM projects p \
             LEFT JOIN engagements e ON e.project_id=p.id AND e.state='active' \
             ORDER BY p.id,e.name",
        )?;
        let rows = query.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (id, name, agent) = row?;
            match projects.last_mut() {
                Some(last) if last.0 == id => {
                    if let Some(agent) = agent
                        && !last.2.contains(&agent)
                    {
                        last.2.push(agent);
                    }
                }
                _ => projects.push((id, name, agent.into_iter().collect())),
            }
        }
        let tasks = self.operator_tasks(&TaskFilters::default())?;
        let mut task_totals = serde_json::Map::new();
        for status in TASK_STATUSES {
            let count = tasks.iter().filter(|t| t.status == status).count();
            task_totals.insert(status.to_owned(), json!(count));
        }
        let totals = json!({
            "projects": projects.len(),
            "agents": projects.iter().map(|p| p.2.len()).sum::<usize>(),
            "tasks": Value::Object(task_totals),
        });
        let project_rows: Vec<Value> = projects
            .iter()
            .map(|(id, name, agents)| {
                let lanes = TASK_STATUSES
                    .iter()
                    .map(|status| {
                        let count = tasks
                            .iter()
                            .filter(|task| {
                                task.status == *status
                                    && task
                                        .assignee
                                        .as_ref()
                                        .is_some_and(|assignee| agents.contains(assignee))
                            })
                            .count();
                        ((*status).to_owned(), json!(count))
                    })
                    .collect::<serde_json::Map<String, Value>>();
                json!({
                    "id": id,
                    "name": name,
                    "agents": agents,
                    "taskLanes": Value::Object(lanes),
                })
            })
            .collect();
        Ok(json!({
            "generatedAt": iso8601(now),
            "staleAfterMs": 5 * 60 * 1000,
            "activityLimit": limit,
            "unavailable": UNAVAILABLE_BOARD,
            "totals": totals,
            "projects": project_rows,
        }))
    }
}

/// Every retained board column native has no source for in this slice.
const UNAVAILABLE_BOARD: [&str; 8] = [
    "createdAt",
    "binding",
    "health",
    "repositories",
    "worktrees",
    "specs",
    "issues",
    "activity",
];


#[cfg(test)]
mod tests {
    use super::*;

    /// The one server-owned transition map must stay the retained map.
    #[test]
    fn native_operator_task_transitions_match_retained_store() {
        assert_eq!(operator_transitions("created"), ["accepted"]);
        assert_eq!(operator_transitions("accepted"), ["in_progress"]);
        assert_eq!(operator_transitions("in_progress"), ["blocked", "done"]);
        assert_eq!(operator_transitions("blocked"), ["in_progress"]);
        assert!(operator_transitions("done").is_empty());
        assert!(operator_transitions("nonsense").is_empty());
    }

    /// `new Date(ms).toISOString()` spelling, including the leap-year and
    /// epoch boundaries the console renders.
    #[test]
    fn native_operator_task_iso8601_matches_retained_toisoformat() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_700_000_000_000), "2023-11-14T22:13:20.000Z");
        assert_eq!(iso8601(1_709_164_800_123), "2024-02-29T00:00:00.123Z");
        assert_eq!(iso8601(946_684_800_000), "2000-01-01T00:00:00.000Z");
    }

    /// `normalizeText`/`normalizeLabels` are the retained normalizers: a
    /// non-string is absent, a blank string is absent, a duplicate label is
    /// dropped and the list is capped.
    #[test]
    fn native_operator_task_normalizers_match_retained_store() {
        assert_eq!(trimmed(Some(&json!(7)), 10), None);
        assert_eq!(trimmed(Some(&json!("  ")), 10), None);
        assert_eq!(trimmed(Some(&json!("  hi  ")), 10).as_deref(), Some("hi"));
        assert_eq!(trimmed(Some(&json!("abcdef")), 3).as_deref(), Some("abc"));
        assert_eq!(labels(Some(&json!("not-an-array"))), Vec::<String>::new());
        assert_eq!(
            labels(Some(&json!(["a", "a", "", 4, "b"]))),
            ["a".to_owned(), "b".to_owned()]
        );
        let many: Vec<Value> = (0..30).map(|i| json!(format!("l{i}"))).collect();
        assert_eq!(labels(Some(&json!(many))).len(), LABELS_MAX);
    }
}
