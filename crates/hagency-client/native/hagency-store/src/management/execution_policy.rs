//! Owner-scoped, reviewed policy changes apply to the next dispatch only.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPolicyListQuery {
    pub resource_id: String,
    #[serde(default)]
    pub after: String,
}
impl ExecutionPolicyListQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        if !self.after.is_empty() {
            identifier(&self.after, 128)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExecutionPolicyPage {
    pub resource_id: String,
    pub agents: Vec<AgentExecutionPolicy>,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionPolicyQuery {
    pub resource_id: String,
    pub engagement_id: String,
}
impl ExecutionPolicyQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        identifier(&self.engagement_id, 128)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicyValue {
    pub yolo: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyApplication {
    NextDispatch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentExecutionPolicy {
    /// Hosted search is independent from command/file permissions. Shared Agents only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
    /// Present only for shared pool Agents. No saved policy means chat-only.
    #[serde(default, skip_serializing_if="Option::is_none")]
    pub shared_mode: Option<String>,
    pub resource_id: String,
    pub engagement_id: String,
    pub agent: String,
    pub project_id: String,
    pub project_name: Option<String>,
    pub framework: String,
    pub execution_policy: ExecutionPolicyValue,
    pub applies_to: PolicyApplication,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditExecutionPolicy {
    /// When present, edit only hosted search and preserve the current tool mode.
    /// Omission preserves the original command encoding and behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
    pub command_id: String,
    pub resource_id: String,
    pub engagement_id: String,
    pub expected_revision: String,
    pub execution_policy: ExecutionPolicyValue,
}
impl EditExecutionPolicy {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.resource_id, 128)?;
        identifier(&self.engagement_id, 128)?;
        if self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(hagency_core::InvalidInput("invalid execution policy revision").into());
        }
        Ok(())
    }
}

impl Session {
    pub async fn agent_execution_policies(
        &self,
        context: &HostContext,
        query: ExecutionPolicyListQuery,
    ) -> Result<AgentExecutionPolicyPage, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadExecutionPolicy)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .agent_execution_policies(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadExecutionPolicy)?;
        Ok(result)
    }
    pub async fn agent_execution_policy(
        &self,
        context: &HostContext,
        query: ExecutionPolicyQuery,
    ) -> Result<AgentExecutionPolicy, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadExecutionPolicy)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .agent_execution_policy(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadExecutionPolicy)?;
        Ok(result)
    }

    pub async fn edit_execution_policy(
        &self,
        context: &HostContext,
        input: EditExecutionPolicy,
    ) -> Result<AgentExecutionPolicy, Error> {
        input.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::EditExecutionPolicy)?;
        let profile = match self.local_store()? {
            SessionStore::Current(store) => store.identity().0,
        };
        let receipt = CommandReceipt::new(
            context,
            "execution_policy.edit",
            &input.command_id,
            &serde_json::json!({"profile":profile,"input":input}),
        )?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .edit_execution_policy(&self.current_profile(store)?, input, receipt)
                    .await?
            }
        };
        self.check(context, Permission::EditExecutionPolicy)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
