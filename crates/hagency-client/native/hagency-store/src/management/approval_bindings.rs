//! Exact resource/engagement binding review; removal never grants tool authority.
use super::*;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceApprovalBindingQuery {
    pub resource_id: String,
    #[serde(default)]
    pub after: String,
    #[serde(default)]
    pub agent: String,
    /// An exact project identifier or its Matrix project-room ID.
    #[serde(default)]
    pub project: String,
}
impl ResourceApprovalBindingQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        if !self.agent.is_empty() {
            identifier(&self.agent, 128)?;
        }
        if !self.project.is_empty() {
            if self.project.starts_with('!') {
                let (_, server) = self
                    .project
                    .split_once(':')
                    .ok_or(hagency_core::InvalidInput("invalid project room"))?;
                hagency_core::replies::matrix_room(&self.project, server)?;
            } else {
                identifier(&self.project, 128)?;
            }
        }
        if !self.after.is_empty() {
            identifier(&self.after, 128)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceApprovalBinding {
    pub resource_id: String,
    pub engagement_id: String,
    pub agent: String,
    pub fleet_id: String,
    pub project_id: String,
    pub server_name: String,
    pub room_id: String,
    pub owner_mxid: String,
    pub room_generation: u64,
    pub incarnation: u64,
    pub active: bool,
    pub revision: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceApprovalBindingPage {
    pub resource_id: String,
    pub agent: String,
    pub project: String,
    pub bindings: Vec<ResourceApprovalBinding>,
    pub next_after: Option<String>,
    pub observed_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RetireResourceApprovalBinding {
    pub command_id: String,
    pub resource_id: String,
    pub engagement_id: String,
    pub expected_revision: String,
}
impl RetireResourceApprovalBinding {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.resource_id, 128)?;
        identifier(&self.engagement_id, 128)?;
        if self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(hagency_core::InvalidInput("invalid binding revision").into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RetiredResourceApprovalBinding {
    pub binding: ResourceApprovalBinding,
    pub revoked_grants: u64,
    pub unbound: bool,
}

impl Session {
    pub async fn resource_approval_bindings(
        &self,
        context: &HostContext,
        query: ResourceApprovalBindingQuery,
    ) -> Result<ResourceApprovalBindingPage, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadApprovalBindings)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .resource_approval_bindings(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadApprovalBindings)?;
        Ok(result)
    }

    pub async fn retire_resource_approval_binding(
        &self,
        context: &HostContext,
        input: RetireResourceApprovalBinding,
    ) -> Result<RetiredResourceApprovalBinding, Error> {
        input.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::RetireApprovalBindings)?;
        let profile = match self.local_store()? {
            SessionStore::Current(store) => store.identity().0,
        };
        let receipt = CommandReceipt::new(
            context,
            "approval_binding.retire",
            &input.command_id,
            &serde_json::json!({"profile":profile,"input":input}),
        )?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .retire_resource_approval_binding(&self.current_profile(store)?, input, receipt)
                    .await?
            }
        };
        self.check(context, Permission::RetireApprovalBindings)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
fn require_resource(db: &Connection, id: &str) -> Result<(), Error> {
    if db
        .query_row("SELECT 1 FROM resources WHERE id=?1", [id], |r| {
            r.get::<_, u8>(0)
        })
        .optional()?
        .is_none()
    {
        return Err(Error::NotFound);
    }
    Ok(())
}
