//! Trusted original-device shared text execution. The registry, private inbox
//! and native execution permission never cross the mini-app boundary.
use super::*;
mod demands;
mod successors;
use crate::console::native::shared_execution::{
    SharedExecutionProgress, SharedExecutionTransport, SharedWorkRecoveryProgress,
};
use crate::console::native::{
    managed_agent_matrix_transport::{ManagedAgentMatrixTransport, ManagedMatrixProgress},
    managed_agent_transport::{ManagedAgentProgress, ManagedAgentTransport},
};

pub struct CurrentSharedExecutor {
    selection: CurrentProfileSelection,
    owner: NativeOwner,
    manager: Arc<NativeManagement>,
    transport: SharedExecutionTransport,
    agents: ManagedAgentTransport,
    settlement: crate::console::native::AgentSettlementTransport,
    #[cfg(feature="agent-budget")]
    budget: crate::console::native::AgentBudgetTransport,
    matrix: ManagedAgentMatrixTransport,
}
impl CurrentProfileSelection {
    pub async fn shared_executor(
        &self,
        owner: &NativeOwner,
        mut package: ManagementPackage,
        host_gate: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<CurrentSharedExecutor, NativeError> {
        if !cfg!(feature = "shared-execution") {
            return Err(failure(409, "shared_execution_disabled"));
        }
        if !host_gate() {
            return Err(failure(401, "management_scope_changed"));
        }
        self.check_executor_owner(owner).await?;
        // This is a separate native session, never an extension of the mini-app
        // or allocation worker's permissions. Assigned project admin admission
        // and exact local holds remain mandatory before the runtime can start.
        package.permissions = vec![ManagementPermission::ExecuteManagedAgents];
        let manager = owner.management(package).await?;
        if let Err(error) = self.check_executor_owner(owner).await {
            manager.retire();
            return Err(error);
        }
        if !host_gate() {
            manager.retire();
            return Err(failure(401, "management_scope_changed"));
        }
        Ok(CurrentSharedExecutor {
            selection: self.clone(),
            owner: owner.clone(),
            transport: SharedExecutionTransport::new(manager.clone())
                .restrict_to_host(host_gate.clone()),
            agents: ManagedAgentTransport::new(manager.clone()).restrict_to_host(host_gate.clone()),
            settlement: crate::console::native::AgentSettlementTransport::new(manager.clone()).restrict_to_host(host_gate.clone()),
            #[cfg(feature="agent-budget")]
            budget: crate::console::native::AgentBudgetTransport::new(manager.clone()).restrict_to_host(host_gate.clone()),
            matrix: ManagedAgentMatrixTransport::new(manager.clone()).restrict_to_host(host_gate),
            manager,
        })
    }
}
impl CurrentSharedExecutor {
    /// Subscribe before polling so arrivals during a pass are not lost. This
    /// carries no content or authority; poll methods retain all native checks.
    pub fn work_changes(&self) -> tokio::sync::watch::Receiver<()> {
        self.transport.work_changes()
    }

    pub fn retire(&self) {
        self.manager.retire();
    }
    /// Applies only assigned-project-admin decisions and automatic local ledger
    /// checks. This stage neither asks the resource owner for another vote nor
    /// starts a provider process.
    #[cfg(feature="agent-budget")]
    pub async fn poll_budget(&self,after:Option<String>)->Result<crate::console::native::AgentBudgetProgress,NativeError>{
        self.selection.check_executor_owner(&self.owner).await?;
        self.budget.poll(&self.selection.registry,after).await
    }
    #[cfg(feature="agent-budget")]
    pub async fn recover_budget(&self,id:String,digest:String)->Result<crate::console::native::AgentBudgetProgress,NativeError>{
        self.selection.check_executor_owner(&self.owner).await?;
        self.budget.recover(&self.selection.registry,id,digest).await
    }
    pub async fn poll_settlement(&self, after: Option<String>) -> Result<crate::console::native::AgentSettlementProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.settlement.poll(&self.selection.registry, after).await
    }
    pub async fn recover_settlement(&self, id: String, digest: String) -> Result<crate::console::native::AgentSettlementProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.settlement.recover(&self.selection.registry, id, digest).await
    }
    pub async fn poll_agent(
        &self,
        after: Option<String>,
    ) -> Result<ManagedAgentProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.agents.poll(&self.selection.registry, after).await
    }
    /// Attests the exact live local reservation for central Matrix provisioning.
    /// Matrix membership and process readiness remain separate observations.
    pub async fn poll_matrix(
        &self,
        after: Option<String>,
    ) -> Result<ManagedMatrixProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.matrix.poll(&self.selection.registry, after).await
    }
    pub async fn poll(
        &self,
        after: Option<String>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport.poll(&self.selection.registry, after).await
    }
    pub async fn recover(
        &self,
        id: String,
        digest: String,
        observation: Option<String>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .recover(&self.selection.registry, id, digest, observation)
            .await
    }
    pub async fn recover_work(
        &self,
        command_id: String,
        command_digest: String,
        work_id: String,
        observation: Option<String>,
    ) -> Result<SharedWorkRecoveryProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .recover_work(
                &self.selection.registry,
                command_id,
                command_digest,
                work_id,
                observation,
            )
            .await
    }
}
impl Drop for CurrentSharedExecutor {
    fn drop(&mut self) {
        self.manager.retire();
    }
}
