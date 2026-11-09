//! Explicit native starts and background recovery of original journal custody.
use super::*;
use crate::console::native::shared_execution::{PreparedSharedSuccessor, SharedSuccessorResume};
impl CurrentSharedExecutor {
    /// Local records from this original execution device, not current readiness.
    pub async fn list_successors(
        &self,
        after: Option<String>,
    ) -> Result<crate::console::native::SharedSuccessorPage, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        let page = self
            .transport
            .list_successors(&self.selection.registry, after)
            .await?;
        self.selection.check_executor_owner(&self.owner).await?;
        Ok(page)
    }

    pub async fn assess_successor(
        &self,
        assignment_id: String,
    ) -> Result<crate::console::native::SharedSuccessorAssessment, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        let result = self
            .transport
            .assess_successor(&self.selection.registry, assignment_id)
            .await?;
        self.selection.check_executor_owner(&self.owner).await?;
        Ok(result)
    }
    pub async fn successor_description(
        &self,
        assignment_id: String,
    ) -> Result<crate::console::native::SharedSuccessorDescription, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        let result = self
            .transport
            .successor_description(&self.selection.registry, assignment_id)
            .await?;
        self.selection.check_executor_owner(&self.owner).await?;
        Ok(result)
    }
    pub async fn prepare_successor(
        &self,
        assignment_id: String,
    ) -> Result<PreparedSharedSuccessor, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        let prepared = self
            .transport
            .prepare_successor(&self.selection.registry, assignment_id)
            .await?;
        self.selection.check_executor_owner(&self.owner).await?;
        Ok(prepared)
    }
    /// Explicit host action only. The non-deserializable preparation must belong
    /// to this exact native executor; existing owner/admin decisions still apply.
    pub async fn start_successor(
        &self,
        prepared: PreparedSharedSuccessor,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .start_prepared_successor(&self.selection.registry, prepared)
            .await
    }
    /// Resume never prepares a new journal row, even if no old request exists.
    pub async fn resume_successor(
        &self,
        assignment_id: String,
    ) -> Result<Option<SharedExecutionProgress>, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .resume_successor(&self.selection.registry, assignment_id)
            .await
    }
    pub async fn resume_next_successor(
        &self,
        after_intent: Option<String>,
    ) -> Result<Option<SharedSuccessorResume>, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .resume_next_successor(&self.selection.registry, after_intent)
            .await
    }
    pub async fn poll_successor(
        &self,
        after_command: Option<String>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .poll_successor(&self.selection.registry, after_command)
            .await
    }
}
