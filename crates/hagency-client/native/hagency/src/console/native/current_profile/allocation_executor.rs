//! Original-device allocation worker access. Neither registry custody nor these
//! permissions are available through a mini-app service request.
use super::*;
use crate::console::native::allocation_transport::{AllocationProgress, AllocationTransport};

pub struct CurrentAllocationExecutor {
    selection: CurrentProfileSelection,
    owner: NativeOwner,
    manager: Arc<NativeManagement>,
    transport: AllocationTransport,
}

impl CurrentProfileSelection {
    /// The trusted desktop host creates a dedicated native worker session. The
    /// original owner grant is checked by the transport before any reservation;
    /// this factory does not grant tokens or require another human approval.
    pub async fn allocation_executor(
        &self,
        owner: &NativeOwner,
        mut package: ManagementPackage,
        host_gate: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<CurrentAllocationExecutor, NativeError> {
        if !host_gate() {
            return Err(failure(401, "management_scope_changed"));
        }
        self.check_executor_owner(owner).await?;
        package.permissions = vec![
            ManagementPermission::ExecuteAllocations,
            ManagementPermission::SettleGrants,
        ];
        let manager = owner.management(package).await?;
        if let Err(error) = self.check_executor_owner(owner).await {
            manager.retire();
            return Err(error);
        }
        if !host_gate() {
            manager.retire();
            return Err(failure(401, "management_scope_changed"));
        }
        Ok(CurrentAllocationExecutor {
            selection: self.clone(),
            owner: owner.clone(),
            transport: AllocationTransport::new(manager.clone()).restrict_to_host(host_gate),
            manager,
        })
    }

    pub(super) async fn check_executor_owner(&self, owner: &NativeOwner) -> Result<(), NativeError> {
        self.check_owner(owner).await?;
        let device = owner
            .0
            .host
            .console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        if device.device_id() != self.device {
            return Err(failure(
                409,
                "current_executor_device_reauthorization_required",
            ));
        }
        Ok(())
    }
}

impl CurrentAllocationExecutor {
    pub fn retire(&self) {
        self.manager.retire();
    }

    pub async fn poll_allocation(
        &self,
        after: Option<String>,
    ) -> Result<AllocationProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .poll_allocation(&self.selection.registry, after)
            .await
    }

    pub async fn poll_parent_settlement(
        &self,
        after: Option<String>,
    ) -> Result<AllocationProgress, NativeError> {
        self.selection.check_executor_owner(&self.owner).await?;
        self.transport
            .poll_parent_settlement(&self.selection.registry, after)
            .await
    }
}

impl Drop for CurrentAllocationExecutor {
    fn drop(&mut self) {
        self.manager.retire();
    }
}
