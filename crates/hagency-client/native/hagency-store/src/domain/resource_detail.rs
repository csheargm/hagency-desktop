use super::{DomainRepository, resource_ceiling};
use crate::{
    Error,
    management::{
        CeilingAmendment, CeilingAmendmentBlocker, PROTOCOL, ResourceDetail,
        ResourceEditBlocker, ResourceEditing,
    },
};
use hagency_core::qualification;

impl DomainRepository {
    #[cfg(test)]
    pub(crate) fn management_resource_detail(
        &self,
        id: &str,
        at: u64,
    ) -> Result<ResourceDetail, Error> {
        // Pin configuration, native budget and measured usage to one SQLite
        // snapshot, including when another legitimate writer is active.
        let tx = self.db.unchecked_transaction()?;
        let detail = self.management_resource_detail_in_snapshot(id, at)?;
        tx.commit()?;
        Ok(detail)
    }

    /// The caller owns the read transaction, including any canonical name and
    /// eligibility reads. Do not begin or commit a second snapshot here.
    pub(crate) fn management_resource_detail_in_snapshot(
        &self,
        id: &str,
        at: u64,
    ) -> Result<ResourceDetail, Error> {
        let config = self.resource_configuration(id)?;
        let profile = config.profile();
        let detail = ResourceDetail {
            calendar: resource_ceiling::calendar(&self.db, id)?,
            protocol: PROTOCOL,
            observed_at_ms: at,
            resource: self.management_resource(id, at)?,
            // The maximum in force; the definition keeps its own for the revision.
            ceiling: resource_ceiling::effective_ceiling(&self.db, &config)?,
            execution_policy: config.execution_policy.unwrap_or(hagency_core::execution::ExecutionPolicy { yolo: false }),
            choices: qualification::configuration_choices(&profile)?,
            model_tier: qualification::model(&profile).0,
            model_family: qualification::model(&profile).1.map(str::to_owned),
            // Neither the current page nor absent/acknowledged local commands
            // establish that this configuration was never offered elsewhere.
            editing: ResourceEditing {
                allowed: false,
                reason: Some(ResourceEditBlocker::ContributionHistoryNotProven),
            },
            // Only the canonical owner ledger proves offer history and amends.
            ceiling_amendment: CeilingAmendment {
                allowed: false,
                reason: Some(CeilingAmendmentBlocker::ContributionHistoryNotProven),
                sequence: resource_ceiling::latest_amendment(&self.db, id)?.map_or(0, |(s, _)| s),
                minimum_tokens: None,
                per_request_tokens: None,
            },
        };
        if serde_json::to_vec(&detail)?.len() > 64 * 1024 {
            return Err(Error::Capacity);
        }
        Ok(detail)
    }
}
