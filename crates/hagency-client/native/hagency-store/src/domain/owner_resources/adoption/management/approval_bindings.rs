//! Original canonical bindings, scoped to the currently granted resource.
use super::*;
use crate::management::{
    CommandReceipt, ResourceApprovalBindingPage, ResourceApprovalBindingQuery,
    RetireResourceApprovalBinding, RetiredResourceApprovalBinding,
};

impl CurrentManagementStore {
    pub(crate) async fn resource_approval_bindings(
        &self,
        profile: &ProfileAuthority,
        query: ResourceApprovalBindingQuery,
    ) -> Result<ResourceApprovalBindingPage, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(128 * 1024, move |db| {
                db.management_approval_bindings(&query, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &query.resource_id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn retire_resource_approval_binding(
        &self,
        profile: &ProfileAuthority,
        input: RetireResourceApprovalBinding,
        receipt: CommandReceipt,
    ) -> Result<RetiredResourceApprovalBinding, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(16 * 1024, move |db| {
                db.management_retire_approval_binding(&input, receipt, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &input.resource_id)
                })
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
