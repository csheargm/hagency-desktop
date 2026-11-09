use super::*;
use crate::management::{
    AgentExecutionPolicy, AgentExecutionPolicyPage, CommandReceipt, EditExecutionPolicy,
    ExecutionPolicyListQuery, ExecutionPolicyQuery,
};

impl CurrentManagementStore {
    pub(crate) async fn agent_execution_policies(
        &self,
        profile: &ProfileAuthority,
        query: ExecutionPolicyListQuery,
    ) -> Result<AgentExecutionPolicyPage, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(128 * 1024, move |db| {
                db.management_execution_policies(&query, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &query.resource_id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn agent_execution_policy(
        &self,
        profile: &ProfileAuthority,
        query: ExecutionPolicyQuery,
    ) -> Result<AgentExecutionPolicy, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(16 * 1024, move |db| {
                db.management_execution_policy(&query, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &query.resource_id)
                })
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn edit_execution_policy(
        &self,
        profile: &ProfileAuthority,
        input: EditExecutionPolicy,
        receipt: CommandReceipt,
    ) -> Result<AgentExecutionPolicy, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(24 * 1024, move |db| {
                db.management_edit_execution_policy(&input, receipt, |tx| {
                    backing.check(&authority)?;
                    authorize_resource(tx, &authority, &input.resource_id)
                })
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
