use super::*;
use crate::management::{
    ClearWorkspaceDirty, CommandReceipt, WorkspaceListQuery, WorkspacePage, WorkspaceQuery,
    WorkspaceRelease, WorkspaceStatus,
};

impl CurrentManagementStore {
    pub(crate) async fn resource_workspaces(
        &self,
        profile: &ProfileAuthority,
        query: WorkspaceListQuery,
    ) -> Result<WorkspacePage, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(128 * 1024, move |db| {
                db.management_workspaces(&query, |tx, id| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn resource_workspace(
        &self,
        profile: &ProfileAuthority,
        query: WorkspaceQuery,
    ) -> Result<WorkspaceStatus, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(16 * 1024, move |db| {
                db.management_workspace(&query, |tx, id| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn clear_workspace_dirty(
        &self,
        profile: &ProfileAuthority,
        input: ClearWorkspaceDirty,
        receipt: CommandReceipt,
    ) -> Result<WorkspaceRelease, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(24 * 1024, move |db| {
                db.management_clear_workspace_dirty(&input, receipt, |tx, id| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, id)
                })
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
