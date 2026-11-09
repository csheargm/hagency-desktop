//! Catalogue visibility changes do not rewrite admitted grants or running agents.
use super::*;
use crate::management::{CommandReceipt, PublishResource};
use crate::ResourcePublicationResult;

impl CurrentManagementStore {
    pub(crate) async fn publish_resource(
        &self,
        profile: &ProfileAuthority,
        input: PublishResource,
        receipt: CommandReceipt,
    ) -> Result<ResourcePublicationResult, Error> {
        self.check(profile)?;
        input.validate()?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self.registry.0.store.call(4096, move |db| {
            backing.check(&authority)?;
            let tx = db.db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            authorize_resource(&tx, &authority, &input.resource_id)?;
            // Replay precedes mutable revision/account state, but never ACLs.
            if let Some(result) = receipt.lookup(&tx)? {
                backing.check(&authority)?;
                return Ok(result);
            }
            let resource = read_resource(&tx, &input.resource_id)?;
            db.accounts.check_resource(&tx, &resource)?;
            if resource.published != input.published {
                // Publication participates in the immutable resource revision.
                // Do not invalidate an admitted grant/child or mutate another
                // owner's shared configuration through a catalogue action.
                let retained: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM owner_capacity_grants WHERE resource_id=?1 AND active=1 UNION ALL SELECT 1 FROM engagements WHERE resource_id=?1 AND state IN('pending','reserved','active') UNION ALL SELECT 1 FROM owner_resource_grants WHERE resource_id=?1 AND active=1 AND profile<>?2)",
                    params![input.resource_id, authority.digest], |r| r.get(0),
                )?;
                if retained || unsettled_exact_resource(&tx, &input.resource_id)? {
                    return Err(Error::State);
                }
            }
            let (resource, result) = crate::domain::resource_publication::publication_change(
                &resource, &input.expected_revision, input.published,
            )?;
            backing.check(&authority)?;
            tx.execute("UPDATE resources SET config=?1 WHERE id=?2", params![serde_json::to_string(&resource)?, input.resource_id])?;
            receipt.record(&tx, &result)?;
            db.accounts.check_resource(&tx, &resource)?;
            backing.check(&authority)?;
            tx.commit()?;
            Ok(result)
        }).await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
