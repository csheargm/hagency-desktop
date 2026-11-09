//! Current canonical definitions and the profile-bound command journal.
use super::*;
mod resource_templates;
mod resource_publication;
mod web_catalog;
mod resource_name;
mod resource_edit;
mod resource_ceiling;
mod resource_create;
mod alerts;
mod approval_bindings;
mod tool_approvals;
mod execution_policy;
mod workspaces;
mod stopped;
mod accounts;
mod operator_tasks;
use crate::management::{
    JournalCommand, PROTOCOL, PendingIntent,
    ResourceDetail, ResourcePage, ResourceQuery, ResourceSummary,
};

/// Produced from a freshly authorized current profile and its command journal.
#[derive(Clone)]
pub struct CurrentManagementStore {
    registry: OwnerResourceRegistry,
    profile: String,
    owner: String,
    journal: crate::management::ServerManagementJournal,
    task_history: Option<operator_tasks::TaskBinding>,
}
impl CurrentManagementStore {
    pub(super) fn from_current(registry: OwnerResourceRegistry, plan: current::CurrentPlan,
        journal: crate::management::ServerManagementJournal) -> Self {
        Self { registry, profile: plan.profile.clone(), owner: plan.owner.clone(),
            journal,
            task_history: Some(operator_tasks::TaskBinding(Arc::new(plan))) }
    }
    pub(crate) async fn record_contribution_attempt(
        &self,
        profile: &ProfileAuthority,
        resource_id: String,
        expected_revision: String,
        command_id: String,
    ) -> Result<(), Error> {
        self.check(profile)?;
        let inherited = profile.clone();
        let backing = self.clone();
        let authority = profile.clone().with_native_gate(move || backing.check(&inherited).is_ok())?;
        self.registry.record_contribution_attempt(authority, resource_id, expected_revision, command_id).await?;
        self.check(profile)
    }
    pub(crate) fn enrollment_registry(&self,profile:&ProfileAuthority)->Result<OwnerResourceRegistry,Error>{
        self.check(profile)?;
        Ok(self.registry.clone())
    }
    pub fn native_owner_cleanup_registry(&self, profile: &ProfileAuthority) -> Result<OwnerResourceRegistry, Error> {
        self.check(profile)?;
        Ok(self.registry.clone())
    }
    pub fn matches_registry(&self, registry: &OwnerResourceRegistry) -> bool {
        Arc::ptr_eq(&self.registry.0, &registry.0)
    }
    pub fn matches_profile(&self, profile: &str, account: &str) -> bool {
        self.profile == profile && self.owner == account
    }
    pub(crate) fn identity(&self) -> (&str, &str) {
        (&self.profile, &self.owner)
    }
    pub fn is_live(&self) -> bool {
        !self.registry.0.closed.load(Ordering::Acquire) && self.journal.is_live()
    }
    pub async fn shutdown(&self) -> Result<(), Error> {
        self.journal.shutdown().await
    }
    fn check(&self, profile: &ProfileAuthority) -> Result<(), Error> {
        profile.check()?;
        if !self.is_live() || !self.matches_profile(&profile.digest, &profile.account) {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    pub(crate) async fn journal(
        &self,
        command: JournalCommand,
    ) -> Result<Vec<PendingIntent>, Error> {
        if !self.is_live() {
            return Err(Error::LocalAuthority);
        }
        self.journal.command(command).await
    }
    pub(crate) async fn resource(
        &self,
        profile: &ProfileAuthority,
        id: String,
    ) -> Result<ResourceSummary, Error> {
        self.check(profile)?;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self.registry.0.store.call(4096, move |db| {
            backing.check(&authority)?;
            let tx = db.db.unchecked_transaction()?;
            authorize_resource(&tx, &authority, &id)?;
            let mut value = db.management_resource(&id, graphs::now_ms()?)?;
            value.local_name = Some(backing.name_for(&tx, &id)?);
            crate::domain::web_catalog::filter_capabilities(&tx, &authority.digest, &mut value.capabilities)?;
            backing.check(&authority)?;
            tx.commit()?;
            Ok(value)
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn verify_connection(
        &self,
        profile: &ProfileAuthority,
        id: String,
        revision: String,
        provider: VerifiedProviderAccount,
    ) -> Result<ResourceSummary, Error> {
        self.check(profile)?;
        let binding = self
            .registry
            .verify_provider_resource(profile.clone(), id.clone(), provider)
            .await?;
        if binding.resource_revision() != revision {
            return Err(Error::Conflict);
        }
        let expires = binding.expires;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self
            .registry
            .0
            .store
            .call(4096, move |db| {
                backing.check(&authority)?;
                binding.check_canonical(&db.db, &backing.registry.0.id)?;
                let result = db.management_resource(&id, graphs::now_ms()?)?;
                if result.revision != revision {
                    return Err(Error::Conflict);
                }
                binding.check()?;
                backing.check(&authority)?;
                Ok(result)
            })
            .await?;
        self.check(profile)?;
        if Instant::now() >= expires {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }
    pub(crate) async fn detail(
        &self,
        profile: &ProfileAuthority,
        id: String,
    ) -> Result<ResourceDetail, Error> {
        self.check(profile)?;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self
            .registry
            .0
            .store
            .call(4096, move |db| {
                backing.check(&authority)?;
                let tx = db.db.unchecked_transaction()?;
                authorize_resource(&tx, &authority, &id)?;
                let mut result = db.management_resource_detail_in_snapshot(&id, graphs::now_ms()?)?;
                result.resource.local_name = Some(backing.name_for(&tx, &id)?);
                crate::domain::web_catalog::filter_capabilities(&tx, &authority.digest, &mut result.resource.capabilities)?;
                result.editing = resource_edit::editing(&tx, backing.registry.id(), &id)?;
                result.ceiling_amendment = resource_ceiling::amendment_state(
                    &tx,
                    backing.registry.id(),
                    &read_resource(&tx, &id)?,
                    result.observed_at_ms,
                )?;
                if serde_json::to_vec(&result)?.len() > 64 * 1024 {
                    return Err(Error::Capacity);
                }
                backing.check(&authority)?;
                tx.commit()?;
                Ok(result)
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn resources(
        &self,
        profile: &ProfileAuthority,
        query: ResourceQuery,
    ) -> Result<ResourcePage, Error> {
        self.check(profile)?;
        query.validate()?;
        let authority = profile.clone();
        let backing = self.clone();
        let result=self.registry.0.store.call(4096,move|db|{
            backing.check(&authority)?;
            let tx=db.db.unchecked_transaction()?;
            // Query only authorized IDs. A page cursor cannot enumerate another
            // profile's definitions, model labels or resource cardinality.
            let ids=tx.prepare("SELECT r.id FROM resources r JOIN owner_resource_grants g ON g.resource_id=r.id WHERE g.profile=?1 AND g.active=1 AND r.id>?2 ORDER BY r.id LIMIT 101")?
                .query_map(params![authority.digest,query.after],|r|r.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            let has_more=ids.len()>100;
            let at=graphs::now_ms()?;let mut resources=Vec::new();let mut next_after=None;
            for id in ids.into_iter().take(100){
                authorize_resource(&tx,&authority,&id)?;
                let mut value=db.management_resource(&id,at)?;
                value.local_name=Some(backing.name_for(&tx,&id)?);
                crate::domain::web_catalog::filter_capabilities(&tx,&authority.digest,&mut value.capabilities)?;
                let matches=query.framework.as_ref().is_none_or(|v|v==&value.framework)
                    && query.capability.as_ref().is_none_or(|v|value.capabilities.contains(v))
                    && query.published.is_none_or(|v|v==value.published);
                if matches {
                    if resources.len()==query.limit{break;}
                    resources.push(value);
                }
                next_after=Some(id);
            }
            if resources.len()<query.limit && !has_more{next_after=None;}
            let result=ResourcePage{protocol:PROTOCOL,observed_at_ms:at,resources,next_after};
            if serde_json::to_vec(&result)?.len()>64*1024{return Err(Error::Capacity);}
            backing.check(&authority)?;tx.commit()?;Ok(result)
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
}
impl CurrentManagementStore {
    pub(crate) async fn resource_namespace(
        &self, profile: &ProfileAuthority, id: String, revision: String,
    ) -> Result<Option<ResourceNamespace>, Error> {
        self.check(profile)?;
        let result=self.registry.resource_namespace(profile.clone(),id,revision).await?;
        self.check(profile)?;
        Ok(result)
    }
}


mod web_connections;
