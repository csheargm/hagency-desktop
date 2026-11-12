//! Native, resource-scoped inspection and decisions for interrupted tasks.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoppedListQuery {
    pub resource_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub after: String,
}
impl StoppedListQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        for id in [&self.workspace_id, &self.after] {
            if !id.is_empty() {
                identifier(id, 128)?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoppedQuery {
    pub resource_id: String,
    pub engagement_id: String,
    pub dispatch_id: String,
    #[serde(default)]
    pub entry_offset: usize,
}
impl StoppedQuery {
    pub fn validate(&self) -> Result<(), Error> {
        for id in [&self.resource_id, &self.engagement_id, &self.dispatch_id] {
            identifier(id, 128)?;
        }
        if self.entry_offset > 1024 {
            return Err(Error::Capacity);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoppedAttempt {
    pub resource_id: String,
    pub engagement_id: String,
    pub dispatch_id: String,
    pub agent: String,
    pub project_id: String,
    pub task_id: Option<String>,
    pub fence: u64,
    pub reason: String,
    pub inspection_available: bool,
    pub shared_resource_authority: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoppedPage {
    pub resource_id: String,
    pub dispatches: Vec<StoppedAttempt>,
    pub next_after: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoppedInventoryEntry {
    pub relative_path: String,
    pub kind: String,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoppedInspection {
    pub attempt: StoppedAttempt,
    pub task_title: Option<String>,
    pub task_status: Option<String>,
    pub revision: String,
    pub resolvable: bool,
    pub can_continue: bool,
    pub entry_count: usize,
    pub entries: Vec<StoppedInventoryEntry>,
    pub next_entry: Option<usize>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReviewStopped {
    pub command_id: String,
    pub resource_id: String,
    pub engagement_id: String,
    pub dispatch_id: String,
    pub expected_revision: String,
}
fn revision(value: &str) -> Result<(), Error> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(hagency_core::InvalidInput("invalid stopped inspection revision").into());
    }
    Ok(())
}
impl ReviewStopped {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        self.query().validate()?;
        revision(&self.expected_revision)
    }
    pub(crate) fn query(&self) -> StoppedQuery {
        StoppedQuery {
            resource_id: self.resource_id.clone(),
            engagement_id: self.engagement_id.clone(),
            dispatch_id: self.dispatch_id.clone(),
            entry_offset: 0,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoppedReview {
    pub review_command_id: String,
    pub inspection_id: String,
    pub expires_at: u64,
    pub inspection: StoppedInspection,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoppedDecision {
    Continue,
    AcceptCompleted,
    KeepBlocked,
}
impl StoppedDecision {
    pub(crate) fn action(self) -> crate::OutcomeAction {
        match self {
            Self::Continue => crate::OutcomeAction::Continue,
            Self::AcceptCompleted => crate::OutcomeAction::AcceptCompleted,
            Self::KeepBlocked => crate::OutcomeAction::KeepBlocked,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveStopped {
    pub command_id: String,
    pub resource_id: String,
    pub engagement_id: String,
    pub dispatch_id: String,
    pub review_command_id: String,
    pub review_revision: String,
    pub action: StoppedDecision,
    pub operator_note: String,
    pub continuation_instruction: Option<String>,
}
impl ResolveStopped {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        self.review().validate()?;
        hagency_core::tasks::text(&self.operator_note, 2000)?;
        match (&self.action, &self.continuation_instruction) {
            (StoppedDecision::Continue, Some(instruction)) => {
                hagency_core::tasks::text(instruction, 8000)?
            }
            (StoppedDecision::AcceptCompleted | StoppedDecision::KeepBlocked, None) => {}
            _ => {
                return Err(hagency_core::InvalidInput(
                    "continuation requires an instruction only for continue",
                )
                .into());
            }
        }
        Ok(())
    }
    pub(crate) fn review(&self) -> ReviewStopped {
        ReviewStopped {
            command_id: self.review_command_id.clone(),
            resource_id: self.resource_id.clone(),
            engagement_id: self.engagement_id.clone(),
            dispatch_id: self.dispatch_id.clone(),
            expected_revision: self.review_revision.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StoppedResolution {
    pub command_id: String,
    pub resource_id: String,
    pub engagement_id: String,
    pub dispatch_id: String,
    pub action: StoppedDecision,
    pub task_id: String,
    pub task_status: String,
    pub replacement_id: Option<String>,
    pub resolved_at: u64,
}

impl Session {
    pub async fn stopped_tasks(
        &self,
        context: &HostContext,
        query: StoppedListQuery,
    ) -> Result<StoppedPage, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadStoppedTasks)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .stopped_tasks(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadStoppedTasks)?;
        Ok(result)
    }
    pub async fn stopped_inspection(
        &self,
        context: &HostContext,
        query: StoppedQuery,
    ) -> Result<StoppedInspection, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadStoppedTasks)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .stopped_inspection(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadStoppedTasks)?;
        Ok(result)
    }
    fn stopped_profile(&self) -> Result<&str, Error> {
        Ok(match self.local_store()? {
            SessionStore::Current(store) => store.identity().0,
        })
    }
    pub async fn review_stopped(
        &self,
        context: &HostContext,
        input: ReviewStopped,
    ) -> Result<StoppedReview, Error> {
        input.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::InspectStoppedTasks)?;
        let receipt = CommandReceipt::new(
            context,
            "stopped.inspect",
            &input.command_id,
            &serde_json::json!({"profile":self.stopped_profile()?,"input":input}),
        )?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .review_stopped(&self.current_profile(store)?, input, receipt)
                    .await?
            }
        };
        self.check(context, Permission::InspectStoppedTasks)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    pub async fn resolve_stopped(
        &self,
        context: &HostContext,
        input: ResolveStopped,
    ) -> Result<StoppedResolution, Error> {
        input.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::ResolveStoppedTasks)?;
        let receipt = CommandReceipt::new(
            context,
            "stopped.resolve",
            &input.command_id,
            &serde_json::json!({"profile":self.stopped_profile()?,"input":input}),
        )?;
        let review = input.review();
        let inspected = CommandReceipt::new(
            context,
            "stopped.inspect",
            &review.command_id,
            &serde_json::json!({"profile":self.stopped_profile()?,"input":review}),
        )?;
        let request_id = hagency_core::canonical::digest(&serde_json::json!([
            "native-stopped-resolution-v1",
            context.account,
            context.device,
            self.stopped_profile()?,
            input.command_id
        ]))?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .resolve_stopped(
                        &self.current_profile(store)?,
                        input,
                        receipt,
                        inspected,
                        request_id,
                    )
                    .await?
            }
        };
        self.check(context, Permission::ResolveStoppedTasks)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
