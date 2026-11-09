//! Closed operator-only task edits. Neither assignees nor task status dispatch work.
use super::*;

/// A missing patch member preserves the original value; explicit null clears it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TaskField<T> {
    #[default]
    Unchanged,
    Set(Option<T>),
}
impl<T> TaskField<T> {
    pub fn is_unchanged(&self) -> bool {
        matches!(self, Self::Unchanged)
    }
}
impl<T: Serialize> Serialize for TaskField<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Unchanged => serializer.serialize_none(),
            Self::Set(value) => value.serialize(serializer),
        }
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for TaskField<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::Set(Option::deserialize(d)?))
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateOperatorTask {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub granularity: Option<String>,
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditOperatorTask {
    pub id: String,
    pub expected_revision: u64,
    #[serde(default, skip_serializing_if = "TaskField::is_unchanged")]
    pub title: TaskField<String>,
    #[serde(default, skip_serializing_if = "TaskField::is_unchanged")]
    pub description: TaskField<String>,
    #[serde(default, skip_serializing_if = "TaskField::is_unchanged")]
    pub priority: TaskField<String>,
    #[serde(default, skip_serializing_if = "TaskField::is_unchanged")]
    pub granularity: TaskField<String>,
    #[serde(default, skip_serializing_if = "TaskField::is_unchanged")]
    pub assignee: TaskField<String>,
    #[serde(default, skip_serializing_if = "TaskField::is_unchanged")]
    pub parent_id: TaskField<String>,
    #[serde(default, skip_serializing_if = "TaskField::is_unchanged")]
    pub labels: TaskField<Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorTaskVersion {
    pub id: String,
    pub expected_revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransitionOperatorTask {
    pub id: String,
    pub expected_revision: u64,
    pub status: String,
    #[serde(default)]
    pub waiting_reason: Option<String>,
    #[serde(default)]
    pub waiting_until: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommentOperatorTask {
    pub id: String,
    pub expected_revision: u64,
    pub text: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "operation",
    content = "input",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum OperatorTaskChange {
    Create(CreateOperatorTask),
    Edit(EditOperatorTask),
    Delete(OperatorTaskVersion),
    Accept(OperatorTaskVersion),
    Transition(TransitionOperatorTask),
    Comment(CommentOperatorTask),
}
impl OperatorTaskChange {
    pub fn permission(&self) -> Permission {
        match self {
            Self::Create(_) => Permission::CreateTasks,
            Self::Edit(_) => Permission::EditTasks,
            Self::Delete(_) => Permission::DeleteTasks,
            Self::Accept(_) => Permission::AcceptTasks,
            Self::Transition(_) => Permission::TransitionTasks,
            Self::Comment(_) => Permission::CommentTasks,
        }
    }
    pub fn operation(&self) -> IntentOperation {
        match self {
            Self::Create(_) => IntentOperation::TaskCreate,
            Self::Edit(_) => IntentOperation::TaskEdit,
            Self::Delete(_) => IntentOperation::TaskDelete,
            Self::Accept(_) => IntentOperation::TaskAccept,
            Self::Transition(_) => IntentOperation::TaskTransition,
            Self::Comment(_) => IntentOperation::TaskComment,
        }
    }
    pub fn task_id(&self) -> Option<&str> {
        match self {
            Self::Create(_) => None,
            Self::Edit(v) => Some(&v.id),
            Self::Delete(v) | Self::Accept(v) => Some(&v.id),
            Self::Transition(v) => Some(&v.id),
            Self::Comment(v) => Some(&v.id),
        }
    }
    pub fn expected_revision(&self) -> Option<u64> {
        match self {
            Self::Create(_) => None,
            Self::Edit(v) => Some(v.expected_revision),
            Self::Delete(v) | Self::Accept(v) => Some(v.expected_revision),
            Self::Transition(v) => Some(v.expected_revision),
            Self::Comment(v) => Some(v.expected_revision),
        }
    }
    pub fn validate(&self) -> Result<(), Error> {
        if serde_json::to_vec(self)?.len() > 12 * 1024 {
            return Err(Error::Capacity);
        }
        if let Some(id) = self.task_id() {
            identifier(id, 64)?;
        }
        if self
            .expected_revision()
            .is_some_and(|v| v == 0 || v >= 9_007_199_254_740_991)
        {
            return Err(hagency_core::InvalidInput("invalid task revision").into());
        }
        match self {
            Self::Create(v) => {
                required(&v.title, 255)?;
                vocabulary(
                    v.priority
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or("p2"),
                    &crate::TASK_PRIORITIES,
                )?;
                vocabulary(
                    v.granularity
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                        .unwrap_or("task"),
                    &crate::TASK_GRANULARITIES,
                )?;
            }
            Self::Edit(v) => {
                for (field, allowed) in [
                    (&v.priority, crate::TASK_PRIORITIES.as_slice()),
                    (&v.granularity, crate::TASK_GRANULARITIES.as_slice()),
                ] {
                    if let TaskField::Set(s) = field {
                        vocabulary(s.as_deref().unwrap_or_default(), allowed)?;
                    }
                }
            }
            Self::Transition(v) => {
                vocabulary(&v.status, &crate::TASK_STATUSES)?;
                if v.status.trim() == "blocked" {
                    required(v.waiting_reason.as_deref().unwrap_or_default(), 1024)?;
                    required(v.waiting_until.as_deref().unwrap_or_default(), 64)?;
                }
            }
            Self::Comment(v) => {
                required(&v.text, 4096)?;
            }
            _ => {}
        }
        Ok(())
    }
}
pub(crate) fn clean(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}
pub(crate) fn required(s: &str, max: usize) -> Result<String, Error> {
    let s = clean(s, max);
    if s.is_empty() {
        Err(hagency_core::InvalidInput("task text required").into())
    } else {
        Ok(s)
    }
}
fn vocabulary(s: &str, allowed: &[&str]) -> Result<(), Error> {
    if allowed.contains(&s.trim()) {
        Ok(())
    } else {
        Err(hagency_core::InvalidInput("invalid task vocabulary").into())
    }
}
pub(crate) fn task_labels(labels: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for s in labels {
        let s = clean(s, 64);
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
            if out.len() == 20 {
                break;
            }
        }
    }
    out
}
/// Native-owned command envelope. The script prepares only a closed change and
/// later selects this original ID; it cannot supply a command/task ID for create.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorTaskCommand {
    pub command_id: String,
    pub task_id: String,
    pub change: OperatorTaskChange,
}
impl OperatorTaskCommand {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.task_id, 64)?;
        self.change.validate()?;
        if self.change.task_id().is_some_and(|id| id != self.task_id) {
            return Err(Error::Conflict);
        }
        Ok(())
    }
}
/// Immutable original result, not current task state. Read detail to refresh.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OperatorTaskResult {
    pub command_id: String,
    pub task_id: String,
    pub revision: u64,
    pub deleted: bool,
    pub status: String,
    pub updated_at: String,
    pub comment_count: usize,
}
impl Session {
    pub async fn operator_task_write_access(&self, context: &HostContext) -> Result<bool, Error> {
        self.check(context, Permission::ReadTasks)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .operator_task_write_access(&self.current_profile(store)?)
                    .await?
            }
        };
        self.check(context, Permission::ReadTasks)?;
        Ok(result)
    }

    pub async fn apply_operator_task(
        &self,
        context: &HostContext,
        input: OperatorTaskCommand,
    ) -> Result<OperatorTaskResult, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        let permission = input.change.permission();
        self.check(context, permission)?;
        input.validate()?;

        {
            let SessionStore::Current(store) = self.local_store()?;
            let receipt = command_receipt(context, store.identity().0, &input)?;
            let result = store
                .apply_operator_task(
                    &self.current_profile(store)?,
                    input,
                    context.account.clone(),
                    receipt,
                )
                .await?;
            self.check(context, permission)
                .map_err(|_| Error::OutcomeUnknown)?;
            Ok(result)
        }
    }
}
fn task_operation(op: IntentOperation) -> &'static str {
    match op {
        IntentOperation::TaskCreate => "task.create",
        IntentOperation::TaskEdit => "task.edit",
        IntentOperation::TaskDelete => "task.delete",
        IntentOperation::TaskAccept => "task.accept",
        IntentOperation::TaskTransition => "task.transition",
        IntentOperation::TaskComment => "task.comment",
        _ => unreachable!("closed task operation"),
    }
}

pub(crate) fn command_receipt(
    context: &HostContext,
    profile: &str,
    input: &OperatorTaskCommand,
) -> Result<CommandReceipt, Error> {
    CommandReceipt::new(
        context,
        task_operation(input.change.operation()),
        &input.command_id,
        &serde_json::json!({"profile":profile,"author":context.account,"command":input}),
    )
}

impl Session {
    pub(crate) fn operator_task_write_operations(
        &self,
        context: &HostContext,
    ) -> Vec<&'static str> {
        [
            (Permission::CreateTasks, "create"),
            (Permission::EditTasks, "edit"),
            (Permission::DeleteTasks, "delete"),
            (Permission::AcceptTasks, "accept"),
            (Permission::TransitionTasks, "transition"),
            (Permission::CommentTasks, "comment"),
        ]
        .into_iter()
        .filter_map(|(p, n)| self.check(context, p).is_ok().then_some(n))
        .collect()
    }
}
