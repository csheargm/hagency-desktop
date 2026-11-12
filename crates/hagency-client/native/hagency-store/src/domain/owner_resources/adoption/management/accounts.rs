use super::*;
use crate::management::{AccountInventory, AccountInventoryQuery};

impl CurrentManagementStore {
    pub(crate) fn account_login_store(&self, profile: &ProfileAuthority) -> Result<crate::DomainStore, Error> {
        self.check(profile)?;
        Ok(self.registry.0.store.clone())
    }

    pub(crate) async fn account_inventory(
        &self,
        profile: &ProfileAuthority,
        query: AccountInventoryQuery,
    ) -> Result<AccountInventory, Error> {
        self.check(profile)?;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self
            .registry
            .0
            .store
            .call(4096, move |db| {
                backing.check(&authority)?;
                let mut result = db.management_account_inventory(Some(&authority.digest), query)?;
                for resource in &mut result.resources {
                    authorize_resource(&db.db, &authority, &resource.id)?;
                    resource.local_name = Some(backing.name_for(&db.db, &resource.id)?);
                }
                backing.check(&authority)?;
                Ok(result)
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
}

impl CurrentManagementStore {
    pub(crate) async fn prepare_account(
        &self,
        profile: &ProfileAuthority,
        receipt: crate::management::CommandReceipt,
        session: crate::management::ManagementAuthority,
        context: crate::management::HostContext,
    ) -> Result<crate::management::AccountPreparation, Error> {
        self.check(profile)?;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self.registry.0.store.call(4096, move |db| {
            db.management_prepare_account(&authority.digest, receipt, || {
                backing.check(&authority)?;
                session.authorize(&context, crate::management::Permission::PrepareAccounts)
            })
        }).await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
