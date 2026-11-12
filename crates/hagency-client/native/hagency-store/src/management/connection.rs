//! A fresh native observation of the provider bound to an enrolled resource.
//! This read grants no execution, publication, reservation or allocation rights.
use super::*;

impl Session {
    /// Explicit reconnect/verify against the adopted canonical resource. The
    /// original central command still performs owner/device/generation/CAS
    /// checks; this read never changes a grant, capacity or the legacy fence.
    pub async fn verify_canonical_connection(
        &self,
        context: &HostContext,
        id: String,
        expected_revision: String,
        provider: crate::owner_resources::VerifiedProviderAccount,
        resume: bool,
    ) -> Result<ResourceSummary, Error> {
        let permission = if resume {
            Permission::ResumeConnections
        } else {
            Permission::VerifyConnections
        };
        self.check(context, Permission::ReadResources)?;
        self.check(context, permission)?;
        identifier(&id, 128)?;
        let SessionStore::Current(store) = self.local_store()?;
        let result = store
            .verify_connection(
                &self.current_profile(store)?,
                id,
                expected_revision,
                provider,
            )
            .await?;
        self.check(context, Permission::ReadResources)?;
        self.check(context, permission)?;
        Ok(result)
    }
}

impl Session {
    /// Host-only original namespace for the explicit connection operation.
    /// A resource ID never selects a filesystem path or a default profile.
    pub async fn connection_namespace(
        &self, context: &HostContext, resource: &ResourceSummary, resume: bool,
    ) -> Result<Option<crate::owner_resources::ResourceNamespace>, Error> {
        let permission=if resume {Permission::ResumeConnections} else {Permission::VerifyConnections};
        self.check(context, Permission::ReadResources)?;
        self.check(context, permission)?;
        let SessionStore::Current(store) = self.local_store()?;
        let result=store.resource_namespace(&self.current_profile(store)?,resource.id.clone(),resource.revision.clone()).await?;
        self.check(context, permission)?;
        Ok(result)
    }
}
