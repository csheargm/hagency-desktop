//! New canonical alerts on original granted resources. This does not import
//! quarantined populated legacy alert history or manufacture adoption evidence.
use super::*;
use crate::management::{CommandReceipt, ResourceAlert, ResourceAlertPage, ResourceAlertQuery, ResourceAlertDetail, ResourceAlertDetailQuery, TriageResourceAlert};
impl CurrentManagementStore {
    pub(crate) async fn web_managed_usage(&self, profile: &ProfileAuthority) -> Result<crate::management::WebManagedUsage, Error> {
        self.web_managed_usage_page(profile,None,256).await
    }
    pub(crate) async fn web_managed_usage_page(&self, profile: &ProfileAuthority, after: Option<String>, limit: usize) -> Result<crate::management::WebManagedUsage, Error> {
        self.check(profile)?;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self.registry.0.store.call(128 * 1024, move |db| {
            backing.check(&authority)?;
            let tx = db.db.unchecked_transaction()?;
            let result = db.management_web_managed_usage_page(&authority.digest,after.as_deref().unwrap_or(""),limit)?;
            for row in &result.items { authorize_resource(&tx, &authority, &row.resource_id)?; }
            backing.check(&authority)?;
            tx.commit()?;
            Ok(result)
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn web_workforce_links(&self, profile: &ProfileAuthority, ids: Vec<String>) -> Result<Vec<crate::management::WebWorkforceLink>, Error> {
        self.check(profile)?;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self.registry.0.store.call(32 * 1024, move |db| {
            backing.check(&authority)?;
            let tx = db.db.unchecked_transaction()?;
            let mut rows = Vec::new();
            for id in ids {
                if let Some(row) = db.management_web_managed_link(&authority.digest, &id)? {
                    authorize_resource(&tx, &authority, &row.resource_id)?;
                    rows.push(row);
                }
            }
            backing.check(&authority)?;
            tx.commit()?;
            Ok(rows)
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn web_alerts(&self, profile: &ProfileAuthority, query: crate::management::WebAlertsQuery) -> Result<crate::management::WebAlertsPage, Error> {
        self.check(profile)?;
        let authority = profile.clone();
        let backing = self.clone();
        let result = self.registry.0.store.call(128 * 1024, move |db| {
            db.management_web_alerts(Some(&authority.digest), &query, |_| backing.check(&authority))
        }).await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn observe_resource_alert(&self, profile: &ProfileAuthority, id: String, now: u64) -> Result<crate::SweepOutcome, Error> {
        self.check(profile)?;
        let authority = profile.clone(); let backing = self.clone();
        let result = self.registry.0.store.call(4096, move |db| db.observe_resource_alert(&id, now, |db| {
            backing.check(&authority)?;
            authorize_resource(db, &authority, &id)
        })).await?;
        self.check(profile)?;
        Ok(result)
    }

    pub(crate) async fn resource_alerts(&self,profile:&ProfileAuthority,query:ResourceAlertQuery)->Result<ResourceAlertPage,Error>{
        self.check(profile)?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(128*1024,move|db|db.management_resource_alerts(&query,|tx|{backing.check(&authority)?;authorize_resource(tx,&authority,&query.resource_id)})).await?;
        self.check(profile)?;Ok(result)
    }
    pub(crate) async fn resource_alert(&self,profile:&ProfileAuthority,query:ResourceAlertDetailQuery)->Result<ResourceAlertDetail,Error>{
        self.check(profile)?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(64*1024,move|db|db.management_resource_alert(&query,|tx|{backing.check(&authority)?;authorize_resource(tx,&authority,&query.resource_id)})).await?;
        self.check(profile)?;Ok(result)
    }
    pub(crate) async fn triage_resource_alert(&self,profile:&ProfileAuthority,input:TriageResourceAlert,actor:String,receipt:CommandReceipt)->Result<ResourceAlert,Error>{
        self.check(profile)?;let backing=self.clone();let authority=profile.clone();
        let result=self.registry.0.store.call(64*1024,move|db|db.management_triage_alert(&input,&actor,receipt,|tx|{backing.check(&authority)?;authorize_resource(tx,&authority,&input.resource_id)})).await?;
        self.check(profile).map_err(|_|Error::OutcomeUnknown)?;Ok(result)
    }
}
