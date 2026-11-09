//! Current profile/package and exact resource authority on every observation.
use super::*;
use crate::management::{
    CommandReceipt, ToolApprovalQuery, ToolApprovalLookup, ToolApprovalSnapshot, ToolApprovalPage,
    ToolGrantQuery, ToolGrantPage, RevokeToolGrant, RevokedToolGrant,
};
impl CurrentManagementStore {
    pub(crate) async fn tool_approvals(
        &self,
        profile: &ProfileAuthority,
        query: ToolApprovalQuery,
    ) -> Result<ToolApprovalPage, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(131072, move |db| {
                db.management_tool_approvals(&query, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &query.resource_id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn tool_approval(
        &self,
        profile: &ProfileAuthority,
        query: ToolApprovalLookup,
    ) -> Result<ToolApprovalSnapshot, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(8192, move |db| {
                db.management_tool_approval(&query, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &query.resource_id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn tool_grants(
        &self,
        profile: &ProfileAuthority,
        query: ToolGrantQuery,
    ) -> Result<ToolGrantPage, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(131072, move |db| {
                db.management_tool_grants(&query, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &query.resource_id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn revoke_tool_grant(
        &self,
        profile: &ProfileAuthority,
        input: RevokeToolGrant,
        receipt: CommandReceipt,
    ) -> Result<RevokedToolGrant, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(24 * 1024, move |db| {
                db.management_revoke_tool_grant(&input, receipt, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &input.resource_id)
                })
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
