//! Exact original receipts and edit/offer exclusion share the canonical writer.
use super::*;
use crate::{CeilingChange, ProfileChange, ResourceConfigurationResult, resource_publication_revision};
use crate::management::{CommandReceipt, EditLocalResource, ResourceEditBlocker, ResourceEditing};

/// Why the definition cannot be edited in place. An amended monthly maximum
/// is retained history too: the definition keeps the maximum it started from.
pub(super) fn blocker(db: &Connection, registry: &str, resource: &Resource) -> Result<Option<ResourceEditBlocker>, Error> {
    match provenance::history(db, registry, &resource.id())? {
        provenance::History::Unknown => return Ok(Some(ResourceEditBlocker::ContributionHistoryNotProven)),
        provenance::History::Offered => return Ok(Some(ResourceEditBlocker::PreviouslyOffered)),
        provenance::History::NeverOffered => {}
    }
    if resource.published {
        return Ok(Some(ResourceEditBlocker::Published));
    }
    // Even a contradictory historical hold must not be erased or reinterpreted.
    // Offered resources require amendment, including after their grant closes.
    let retained: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_capacity_grants WHERE resource_id=?1 UNION ALL SELECT 1 FROM engagements WHERE resource_id=?1 UNION ALL SELECT 1 FROM owner_resource_ceiling_amendments WHERE resource_id=?1)",
        [resource.id()], |r| r.get(0))?;
    if retained || unsettled_resource(db, resource)? {
        return Ok(Some(ResourceEditBlocker::RetainedCommitments));
    }
    Ok(None)
}

/// UI eligibility additionally requires the managed Codex namespace supported
/// by the native adapter. This read does not claim current provider sign-in or
/// model availability; those are observed afresh after native confirmation.
pub(super) fn editing(db: &Connection, registry: &str, id: &str) -> Result<ResourceEditing, Error> {
    let resource = read_resource(db, id)?;
    let mut reason = blocker(db, registry, &resource)?;
    if reason.is_none() {
        let namespace: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_resource_namespaces WHERE resource_id=?1)",
            [id], |row| row.get(0))?;
        if resource.framework != "codex" || !resource.preset_id.starts_with("owner_ns_") || !namespace {
            reason = Some(ResourceEditBlocker::ManagedNamespaceRequired);
        } else {
            let current: bool = db.query_row(
                "SELECT EXISTS(SELECT 1 FROM owner_resource_namespaces n JOIN managed_accounts a ON a.id=n.account_id JOIN management_account_profiles p ON p.account_id=a.id AND p.profile=n.profile WHERE n.resource_id=?1 AND a.generation=n.generation AND a.state='active' AND NOT EXISTS(SELECT 1 FROM account_login_attempts l WHERE l.account_id=a.id AND l.state='attempting'))",
                [id], |row| row.get(0))?;
            if !current { reason = Some(ResourceEditBlocker::AccountUnavailable); }
        }
    }
    Ok(ResourceEditing { allowed: reason.is_none(), reason })
}

fn require_editable(db: &Connection, registry: &str, resource: &Resource) -> Result<(), Error> {
    if blocker(db, registry, resource)?.is_some() {
        return Err(Error::ResourceEditNotAvailable);
    }
    Ok(())
}

impl CurrentManagementStore {
    pub(crate) async fn recover_local_resource_edit(
        &self,
        profile: &ProfileAuthority,
        id: String,
        receipt: CommandReceipt,
    ) -> Result<Option<ResourceConfigurationResult>, Error> {
        self.check(profile)?;
        let backing = self.clone();
        let authority = profile.clone();
        let result = self
            .registry
            .0
            .store
            .call(4096, move |db| {
                backing.check(&authority)?;
                let tx = db.db.unchecked_transaction()?;
                authorize_resource(&tx, &authority, &id)?;
                let result = receipt.lookup(&tx)?;
                backing.check(&authority)?;
                tx.commit()?;
                Ok(result)
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }
    pub(crate) async fn edit_local_resource(
        &self,
        profile: &ProfileAuthority,
        input: EditLocalResource,
        binding: ResourceEditBinding,
        receipt: CommandReceipt,
    ) -> Result<ResourceConfigurationResult, Error> {
        self.check(profile)?;
        input.validate()?;
        // An amendment request is never applied as a definition edit.
        if input.is_ceiling_amendment() {
            return Err(hagency_core::InvalidInput("a ceiling amendment is not an in-place edit").into());
        }
        let backing = self.clone();
        let authority = profile.clone();
        let expires = binding.expires();
        let (result, replayed) = self
            .registry
            .0
            .store
            .call(16 * 1024, move |db| {
                backing.check(&authority)?;
                let tx = db
                    .db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                backing.check(&authority)?;
                authorize_resource(&tx, &authority, &input.resource_id)?;
                // Original receipt precedes mutable eligibility and stale binding:
                // recovering a committed edit never writes it a second time.
                if let Some(result) = receipt.lookup(&tx)? {
                    backing.check(&authority)?;
                    return Ok((result, true));
                }
                let mut resource = read_resource(&tx, &input.resource_id)?;
                if resource_publication_revision(&resource)? != input.expected_revision {
                    return Err(Error::ResourceEditRevisionChanged);
                }
                require_editable(&tx, backing.registry.id(), &resource)?;
                if let Some(policy) = input.execution_policy {
                    policy.validate_for(&resource.framework)?;
                    resource.execution_policy = Some(policy);
                }
                binding.check_on(
                    &tx,
                    backing.registry.id(),
                    &authority,
                    &input.resource_id,
                    &input.expected_revision,
                )?;
                if let ProfileChange::Select { model, reasoning } = input.profile {
                    if !hagency_core::qualification::configuration_choices(&resource.profile())?
                        .iter()
                        .any(|c| c.model == model && c.reasoning == reasoning)
                    {
                        return Err(Error::Unqualified);
                    }
                    resource.model = model;
                    resource.reasoning = reasoning;
                }
                match input.ceiling {
                    CeilingChange::Preserve {} => {}
                    CeilingChange::Clear {} => resource.ceiling = None,
                    CeilingChange::Monthly { tokens } => {
                        resource.ceiling = Some(serde_json::from_value(
                            json!({"tokens":tokens,"period":"monthly"}),
                        )?)
                    }
                }
                let resource = prepare_resource_write(&tx, &resource, Some(false), false)?;
                db.accounts.check_resource(&tx, &resource)?;
                let result = ResourceConfigurationResult {
                    resource_id: resource.id(),
                    revision: resource_publication_revision(&resource)?,
                    published: false,
                    ceiling_amendment: None,
                };
                write_resource_configuration(&tx, &resource, false)?;
                receipt.record(&tx, &result)?;
                binding.check_live()?;
                backing.check(&authority)?;
                tx.commit()?;
                Ok((result, false))
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        if !replayed && Instant::now() >= expires {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }
}
