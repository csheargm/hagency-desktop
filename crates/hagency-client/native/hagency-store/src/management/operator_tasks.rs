//! Retained operator tasks on the already-bound profile ledger. This is not
//! shared execution work, and adopted resource storage supplies no task data.
use super::*;
use crate::{OperatorTask, TASK_PRIORITIES, TASK_STATUSES};
use std::collections::BTreeMap;

pub const TASK_READ_PAGE: usize = 16;
pub const TASK_COMMENT_PAGE: usize = 2;
const ARCHIVED: &str = "current_task_proof_required";
fn text(value: &Option<String>, max: usize) -> Result<(), Error> {
    if value
        .as_ref()
        .is_some_and(|s| s.chars().count() > max || s.len() > max * 4)
    {
        return Err(hagency_core::InvalidInput("invalid task filter").into());
    }
    Ok(())
}
#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorTaskQuery {
    pub assignee: Option<String>,
    pub status: Option<String>,
    pub priority: Option<String>,
    pub label: Option<String>,
    #[serde(default)]
    pub offset: u64,
}
impl OperatorTaskQuery {
    pub fn validate(&self) -> Result<(), Error> {
        text(&self.assignee, 128)?;
        text(&self.label, 64)?;
        text(&self.status, 64)?;
        text(&self.priority, 8)?;
        if self.offset > 100_000
            || self.status.as_deref().is_some_and(|s| {
                s.split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .any(|s| !TASK_STATUSES.contains(&s))
            })
            || self
                .priority
                .as_deref()
                .is_some_and(|s| !s.is_empty() && !TASK_PRIORITIES.contains(&s))
        {
            return Err(hagency_core::InvalidInput("invalid task page").into());
        }
        Ok(())
    }
    pub(crate) fn filters(&self) -> crate::TaskFilters {
        let nonempty = |s: &Option<String>| s.clone().filter(|s| !s.is_empty());
        crate::TaskFilters {
            assignee: nonempty(&self.assignee),
            status: nonempty(&self.status),
            priority: nonempty(&self.priority),
            label: nonempty(&self.label),
            offset: self.offset,
            limit: Some(TASK_READ_PAGE + 1),
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorTaskDetailQuery {
    pub id: String,
    #[serde(default)]
    pub comment_offset: usize,
}
impl OperatorTaskDetailQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.id, 64)?;
        if self.comment_offset > crate::MAX_TASK_COMMENTS {
            return Err(hagency_core::InvalidInput("invalid task comment page").into());
        }
        Ok(())
    }
}
#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorBoardQuery {
    pub after: Option<String>,
    pub project_id: Option<String>,
    pub agent_after: Option<String>,
}
impl OperatorBoardQuery {
    pub fn validate(&self) -> Result<(), Error> {
        if let Some(s) = &self.after {
            identifier(s, 128)?
        }
        if let Some(s) = &self.project_id {
            identifier(s, 128)?
        }
        text(&self.agent_after, 128)?;
        if self.project_id.is_none() && self.agent_after.is_some()
            || self.project_id.is_some() && self.after.is_some()
        {
            return Err(hagency_core::InvalidInput("invalid board cursor").into());
        }
        Ok(())
    }
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorTaskSummary {
    pub id: String,
    pub title: String,
    pub status: String,
    pub priority: String,
    pub granularity: String,
    pub assignee: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub parent_id: Option<String>,
    pub label_count: usize,
}
impl From<OperatorTask> for OperatorTaskSummary {
    fn from(t: OperatorTask) -> Self {
        Self {
            id: t.id,
            title: t.title,
            status: t.status,
            priority: t.priority,
            granularity: t.granularity,
            assignee: t.assignee,
            created_at: t.created_at,
            updated_at: t.updated_at,
            parent_id: t.parent_id,
            label_count: t.labels.len(),
        }
    }
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorTaskPage {
    pub write_supported: bool,
    #[serde(deserialize_with = "crate::management::wire_static::many")]
    pub write_operations: Vec<crate::management::wire_static::Label>,
    pub supported: bool,
    #[serde(deserialize_with = "crate::management::wire_static::optional")]
    pub reason: Option<crate::management::wire_static::Label>,
    pub tasks: Vec<OperatorTaskSummary>,
    pub next_offset: Option<u64>,
    pub observed_at_ms: u64,
    #[serde(deserialize_with = "crate::management::wire_static::many")]
    pub unavailable: Vec<crate::management::wire_static::Label>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorTaskDetail {
    pub write_supported: bool,
    #[serde(deserialize_with = "crate::management::wire_static::many")]
    pub write_operations: Vec<crate::management::wire_static::Label>,
    pub revision: Option<u64>,
    pub supported: bool,
    #[serde(deserialize_with = "crate::management::wire_static::optional")]
    pub reason: Option<crate::management::wire_static::Label>,
    pub task: Option<OperatorTask>,
    pub next: Vec<String>,
    pub comment_count: usize,
    pub next_comment_offset: Option<usize>,
    pub observed_at_ms: u64,
    #[serde(deserialize_with = "crate::management::wire_static::many")]
    pub unavailable: Vec<crate::management::wire_static::Label>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorBoardProject {
    pub id: String,
    pub name: String,
    pub agents: Vec<String>,
    pub agent_count: u64,
    pub next_agent_after: Option<String>,
    pub task_lanes: BTreeMap<String, u64>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperatorBoardPage {
    pub supported: bool,
    #[serde(deserialize_with = "crate::management::wire_static::optional")]
    pub reason: Option<crate::management::wire_static::Label>,
    pub projects: Vec<OperatorBoardProject>,
    pub next_after: Option<String>,
    pub project_count: u64,
    pub agent_count: u64,
    pub task_totals: BTreeMap<String, u64>,
    pub observed_at_ms: u64,
    pub stale_after_ms: u64,
    #[serde(deserialize_with = "crate::management::wire_static::many")]
    pub unavailable: Vec<crate::management::wire_static::Label>,
}
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}
pub(crate) fn bounded<T: Serialize>(value: T) -> Result<T, Error> {
    if serde_json::to_vec(&value)?.len() > 120 * 1024 {
        Err(Error::Capacity)
    } else {
        Ok(value)
    }
}
impl Session {
    pub async fn operator_task_page(
        &self,
        context: &HostContext,
        query: OperatorTaskQuery,
    ) -> Result<OperatorTaskPage, Error> {
        self.check(context, Permission::ReadTasks)?;
        query.validate()?;
        let mut out = match self.local_store()? {
            SessionStore::Current(store) => store
                .operator_task_page(&self.current_profile(store)?, query, now())
                .await?
                .unwrap_or_else(|| OperatorTaskPage {
                    write_supported: false,
                    write_operations: vec![],
                    supported: false,
                    reason: Some(ARCHIVED),
                    tasks: vec![],
                    next_offset: None,
                    observed_at_ms: now(),
                    unavailable: vec!["health"],
                }),
        };
        self.check(context, Permission::ReadTasks)?;
        out.write_operations = if out.write_supported {
            self.operator_task_write_operations(context)
        } else {
            vec![]
        };
        out.write_supported &= !out.write_operations.is_empty();
        bounded(out)
    }
    pub async fn operator_task_detail(
        &self,
        context: &HostContext,
        query: OperatorTaskDetailQuery,
    ) -> Result<OperatorTaskDetail, Error> {
        self.check(context, Permission::ReadTasks)?;
        query.validate()?;
        let mut out = match self.local_store()? {
            SessionStore::Current(store) => store
                .operator_task_detail(&self.current_profile(store)?, query, now())
                .await?
                .unwrap_or_else(|| OperatorTaskDetail {
                    write_supported: false,
                    write_operations: vec![],
                    revision: None,
                    supported: false,
                    reason: Some(ARCHIVED),
                    task: None,
                    next: vec![],
                    comment_count: 0,
                    next_comment_offset: None,
                    observed_at_ms: now(),
                    unavailable: vec!["health"],
                }),
        };
        self.check(context, Permission::ReadTasks)?;
        out.write_operations = if out.write_supported {
            self.operator_task_write_operations(context)
        } else {
            vec![]
        };
        out.write_supported &= !out.write_operations.is_empty();
        bounded(out)
    }
    pub async fn operator_board(
        &self,
        context: &HostContext,
        query: OperatorBoardQuery,
    ) -> Result<OperatorBoardPage, Error> {
        self.check(context, Permission::ReadTaskBoard)?;
        query.validate()?;
        let out = match self.local_store()? {
            SessionStore::Current(store) => store
                .operator_board(&self.current_profile(store)?, query, now())
                .await?
                .unwrap_or_else(|| OperatorBoardPage {
                    supported: false,
                    reason: Some(ARCHIVED),
                    projects: vec![],
                    next_after: None,
                    project_count: 0,
                    agent_count: 0,
                    task_totals: BTreeMap::new(),
                    observed_at_ms: now(),
                    stale_after_ms: 300_000,
                    unavailable: vec!["tasks", "projects"],
                }),
        };
        self.check(context, Permission::ReadTaskBoard)?;
        bounded(out)
    }
}
