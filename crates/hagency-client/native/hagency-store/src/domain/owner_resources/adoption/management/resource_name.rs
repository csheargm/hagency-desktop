//! Separate metadata CAS with an atomic original effect receipt.
use super::*;
use crate::management::{CommandReceipt, RenameResource, ResourceLocalName, ResourceNameResult};

fn local_name(db: &Connection, profile: &str, id: &str) -> Result<ResourceLocalName, Error> {
    Ok(db.query_row(
        "SELECT name,revision FROM owner_resource_display_names WHERE profile=?1 AND resource_id=?2",
        params![profile,id], |row| Ok(ResourceLocalName{name:row.get(0)?,revision:row.get(1)?})
    ).optional()?.unwrap_or(ResourceLocalName{name:String::new(),revision:0}))
}
impl CurrentManagementStore {
    pub(super) fn name_for(&self, db: &Connection, id: &str) -> Result<ResourceLocalName, Error> {
        local_name(db, &self.profile, id)
    }
    pub(crate) async fn rename_resource(
        &self,
        profile: &ProfileAuthority,
        input: RenameResource,
        receipt: CommandReceipt,
    ) -> Result<ResourceNameResult, Error> {
        self.check(profile)?;
        input.validate()?;
        let authority = profile.clone();
        let backing = self.clone();
        let result=self.registry.0.store.call(8192,move|db| {
            backing.check(&authority)?;
            let tx=db.db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            // SQLite writer waits cannot carry a retired package into a write.
            backing.check(&authority)?;
            authorize_resource(&tx,&authority,&input.resource_id)?;
            if let Some(result)=receipt.lookup::<ResourceNameResult>(&tx)? {
                backing.check(&authority)?;
                return Ok(result);
            }
            let current=local_name(&tx,&authority.digest,&input.resource_id)?;
            if current.revision != input.expected_name_revision {
                return Err(Error::ResourceNameRevisionChanged);
            }
            let revision=current.revision.checked_add(1).filter(|v|*v<=9_007_199_254_740_991).ok_or(Error::Capacity)?;
            tx.execute("INSERT INTO owner_resource_display_names(profile,resource_id,name,revision) VALUES(?1,?2,?3,?4) ON CONFLICT(profile,resource_id) DO UPDATE SET name=excluded.name,revision=excluded.revision",params![authority.digest,input.resource_id,input.name,revision])?;
            let result=ResourceNameResult{resource_id:input.resource_id,local_name:ResourceLocalName{name:input.name,revision}};
            receipt.record(&tx,&result)?;
            backing.check(&authority)?;
            tx.commit()?;
            Ok(result)
        }).await?;
        // A committed effect remains recoverable through its original receipt.
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
