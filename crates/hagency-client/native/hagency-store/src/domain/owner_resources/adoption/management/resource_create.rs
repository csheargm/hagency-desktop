//! Additional configurations share their source's actual account and seat.
//! Published/offered sources remain valid: no source amendment is performed.
use super::*;
use crate::{CeilingChange, ProfileChange, ResourceConfigurationResult, resource_publication_revision};
use crate::management::{CommandReceipt, CreateResourceConfiguration};
use crate::domain::accounts as private_accounts;

/// The original account namespace is an execution association, not extra quota
/// or evidence of current provider sign-in. Check both its canonical generation
/// and retained filesystem custody without invoking or logging into a provider.
struct NamespaceCopy {
    account: String,
    generation: u64,
    owner: String,
    launch: crate::ManagedLaunch,
}

fn namespace_source(
    db: &Connection,
    accounts: &private_accounts::Registry,
    source: &Resource,
) -> Result<Option<NamespaceCopy>, Error> {
    let row: Option<(String, u64, String)> = db.query_row(
        "SELECT account_id,generation,profile FROM owner_resource_namespaces WHERE resource_id=?1",
        [source.id()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).optional()?;
    let Some((account, generation, owner)) = row else {
        if source.preset_id.starts_with("owner_ns_") {
            return Err(Error::Quarantined);
        }
        return Ok(None);
    };
    if source.framework != "codex" || !source.preset_id.starts_with("owner_ns_") {
        return Err(Error::Quarantined);
    }
    let current: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM managed_accounts a JOIN management_account_profiles p ON p.account_id=a.id WHERE a.id=?1 AND a.generation=?2 AND a.state='active' AND p.profile=?3) AND NOT EXISTS(SELECT 1 FROM account_login_attempts WHERE account_id=?1 AND state='attempting')",
        params![account, generation, owner], |r| r.get(0),
    )?;
    if !current {
        return Err(Error::LocalAuthority);
    }
    let launch = accounts.managed_account(db, &account)?.prepare_login()?;
    launch.check()?;
    Ok(Some(NamespaceCopy {
        account,
        generation,
        owner,
        launch,
    }))
}

impl CurrentManagementStore {
    pub(crate) async fn recover_resource_configuration(
        &self,
        profile: &ProfileAuthority,
        source: String,
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
                authorize_resource(&tx, &authority, &source)?;
                let result = receipt.lookup(&tx)?;
                backing.check(&authority)?;
                tx.commit()?;
                Ok(result)
            })
            .await?;
        self.check(profile)?;
        Ok(result)
    }

    pub(crate) async fn create_resource_configuration(
        &self,
        profile: &ProfileAuthority,
        input: CreateResourceConfiguration,
        receipt: CommandReceipt,
        confirmed_until: Instant,
    ) -> Result<ResourceConfigurationResult, Error> {
        self.check(profile)?;
        input.validate()?;
        let backing = self.clone();
        let authority = profile.clone();
        let (result, replayed) = self.registry.0.store.call(16 * 1024, move |db| {
            backing.check(&authority)?;
            let tx = db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            backing.check(&authority)?;
            authorize_resource(&tx, &authority, &input.source_resource_id)?;
            if let Some(result) = receipt.lookup(&tx)? {
                backing.check(&authority)?;
                return Ok((result, true));
            }
            let occupied: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM owner_resource_receipts WHERE command_id=?1)",
                [&input.command_id], |r| r.get(0),
            )?;
            if occupied { return Err(Error::Conflict); }
            if Instant::now() >= confirmed_until { return Err(Error::LocalAuthority); }
            let source = read_resource(&tx, &input.source_resource_id)?;
            if resource_publication_revision(&source)? != input.expected_revision { return Err(Error::ResourceConfigurationSourceChanged); }
            db.accounts.check_resource(&tx, &source)?;
            let namespace = namespace_source(&tx, &db.accounts, &source)?;
            let mut resource = source.clone();
            if let Some(policy) = input.execution_policy {
                policy.validate_for(&resource.framework)?;
                resource.execution_policy = Some(policy);
            }
            if let ProfileChange::Select { model, reasoning } = &input.profile {
                resource.model = model.clone();
                resource.reasoning = reasoning.clone();
            }
            if !hagency_core::qualification::configuration_choices(&source.profile())?.iter()
                .any(|choice| choice.model == resource.model && choice.reasoning == resource.reasoning)
            { return Err(Error::Unqualified); }
            match input.ceiling {
                // A copy starts from the maximum in force, including an amendment.
                CeilingChange::Preserve {} => {
                    resource.ceiling = crate::domain::resource_ceiling::effective_ceiling(&tx, &source)?
                }
                CeilingChange::Clear {} => resource.ceiling = None,
                CeilingChange::Monthly { tokens } => resource.ceiling = Some(serde_json::from_value(
                    json!({"tokens":tokens,"period":"monthly"}),
                )?),
            }
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
            let prefix = if namespace.is_some() { "owner_ns_" } else { "preset_" };
            resource.preset_id = format!("{prefix}{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
            private_accounts::copy_association(&tx, &source, &resource)?;
            // Original POST /api/resources publishes the local configuration;
            // this flag is not server admission, delegation or remote capacity.
            let resource = prepare_resource_write(&tx, &resource, Some(true), true)?;
            db.accounts.check_resource(&tx, &resource)?;
            write_resource_configuration(&tx, &resource, true)?;
            if let Some(namespace) = &namespace {
                tx.execute("INSERT INTO owner_resource_namespaces(resource_id,account_id,generation,profile) VALUES(?1,?2,?3,?4)",
                    params![resource.id(), namespace.account, namespace.generation, namespace.owner])?;
            }
            // Only the current profile gains the new resource. Source access,
            // provider identity, account quota and existing grants stay intact.
            tx.execute("INSERT INTO owner_resource_grants(profile,resource_id,revision,active) VALUES(?1,?2,1,1)",
                params![authority.digest, resource.id()])?;
            tx.execute("UPDATE owner_resource_identity SET revision=revision+1 WHERE singleton=1", [])?;
            let ledger_revision = tx.query_row("SELECT revision FROM owner_resource_identity WHERE singleton=1", [], |r| r.get(0))?;
            let owner_receipt = OwnerResourceReceipt {
                command_id: input.command_id.clone(), registry_id: backing.registry.id().into(),
                resource_id: resource.id(), grant_revision: 1, ledger_revision, active: true,
            };
            let digest = canonical::digest(&json!({"operation":"resource.create_configuration","registry":backing.registry.id(),"profile":authority.digest,"input":input}))?;
            bounded_row(&tx, "owner_resource_receipts", "command_id", &input.command_id, 100_000)?;
            tx.execute("INSERT INTO owner_resource_receipts(command_id,digest,result) VALUES(?1,?2,?3)",
                params![input.command_id, digest, serde_json::to_string(&owner_receipt)?])?;
            provenance::created(&tx, &owner_receipt, &digest, &authority.digest)?;
            let result = ResourceConfigurationResult {
                resource_id: resource.id(), revision: resource_publication_revision(&resource)?, published: true,
                ceiling_amendment: None,
            };
            receipt.record(&tx, &result)?;
            if let Some(namespace) = &namespace { namespace.launch.check()?; }
            db.accounts.check_resource(&tx, &source)?;
            db.accounts.check_resource(&tx, &resource)?;
            backing.check(&authority)?;
            if Instant::now() >= confirmed_until { return Err(Error::LocalAuthority); }
            tx.commit()?;
            Ok((result, false))
        }).await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        if !replayed && Instant::now() >= confirmed_until {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }
}
