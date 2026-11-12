//! Resource-scoped workspace inspection and an exact reviewed release.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceListQuery {
    pub resource_id: String,
    #[serde(default)]
    pub after: String,
}
impl WorkspaceListQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        if !self.after.is_empty() {
            identifier(&self.after, 128)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceQuery {
    pub resource_id: String,
    pub workspace_id: String,
}
impl WorkspaceQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        identifier(&self.workspace_id, 128)?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceReleaseBlock {
    InspectionRequired,
    ActiveExecution,
    SharedResourceAuthority,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    pub resource_id: String,
    pub workspace_id: String,
    pub dirty: bool,
    pub active_leases: u64,
    pub quarantined: bool,
    pub shared_resource_count: u64,
    pub release_block: Option<WorkspaceReleaseBlock>,
    pub last_released_at: Option<u64>,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspacePage {
    pub resource_id: String,
    pub workspaces: Vec<WorkspaceStatus>,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClearWorkspaceDirty {
    pub command_id: String,
    pub resource_id: String,
    pub workspace_id: String,
    pub expected_revision: String,
}
impl ClearWorkspaceDirty {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        self.query().validate()?;
        if self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(hagency_core::InvalidInput("invalid workspace revision").into());
        }
        Ok(())
    }
    pub(crate) fn query(&self) -> WorkspaceQuery {
        WorkspaceQuery {
            resource_id: self.resource_id.clone(),
            workspace_id: self.workspace_id.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRelease {
    pub workspace: WorkspaceStatus,
    pub cleared: bool,
}

impl Session {
    pub async fn resource_workspaces(
        &self,
        context: &HostContext,
        query: WorkspaceListQuery,
    ) -> Result<WorkspacePage, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadWorkspaces)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .resource_workspaces(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadWorkspaces)?;
        Ok(result)
    }
    pub async fn resource_workspace(
        &self,
        context: &HostContext,
        query: WorkspaceQuery,
    ) -> Result<WorkspaceStatus, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadWorkspaces)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .resource_workspace(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadWorkspaces)?;
        Ok(result)
    }
    pub async fn clear_workspace_dirty(
        &self,
        context: &HostContext,
        input: ClearWorkspaceDirty,
    ) -> Result<WorkspaceRelease, Error> {
        input.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::ClearWorkspaceDirty)?;
        let profile = match self.local_store()? {
            SessionStore::Current(store) => store.identity().0,
        };
        let receipt = CommandReceipt::new(
            context,
            "workspace.clear_dirty",
            &input.command_id,
            &serde_json::json!({"profile":profile,"input":input}),
        )?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .clear_workspace_dirty(&self.current_profile(store)?, input, receipt)
                    .await?
            }
        };
        self.check(context, Permission::ClearWorkspaceDirty)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
