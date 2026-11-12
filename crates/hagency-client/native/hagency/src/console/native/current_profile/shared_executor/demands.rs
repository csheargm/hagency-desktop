//! Accepted owner-scope consumer. No mini-app permission or Run authority.
use super::*;
use crate::console::native::{
    SharedDemandPoll, SharedDemandResume, SharedDemandStopReceipt, SharedDemandStopResume,
};
impl CurrentSharedExecutor {
    pub async fn poll_demand(
        &self,
        after: Option<String>,
    ) -> Result<SharedDemandPoll, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .poll_demand(&self.selection.registry, after)
            .await
    }
    pub async fn resume_next_demand(
        &self,
        after: Option<String>,
    ) -> Result<Option<SharedDemandResume>, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .resume_next_demand(&self.selection.registry, after)
            .await
    }
    pub async fn resume_next_unfinished_demand(
        &self,
        after: Option<String>,
    ) -> Result<Option<SharedDemandResume>, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .resume_next_unfinished_demand(&self.selection.registry, after)
            .await
    }
    pub async fn stop_demand_assignment(
        &self,
        assignment: String,
    ) -> Result<SharedDemandStopReceipt, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .stop_demand_assignment(&self.selection.registry, assignment)
            .await
    }
    pub async fn resume_next_demand_stop(
        &self,
        after: Option<String>,
    ) -> Result<Option<SharedDemandStopResume>, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .resume_next_demand_stop(&self.selection.registry, after)
            .await
    }
}

#[cfg(feature="shared-budget-execution")]
impl CurrentSharedExecutor {
    pub async fn poll_budget_demand(&self,after:Option<String>)->Result<SharedDemandPoll,NativeError>{
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport.poll_budget_demand(&self.selection.registry,after).await
    }
    pub async fn resume_next_budget_demand(&self,after:Option<String>)->Result<Option<SharedDemandResume>,NativeError>{
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport.resume_next_budget_demand(&self.selection.registry,after).await
    }
    pub async fn poll_budget_successor(&self,after:Option<String>)->Result<SharedExecutionProgress,NativeError>{
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport.poll_budget_successor(&self.selection.registry,after).await
    }
}
