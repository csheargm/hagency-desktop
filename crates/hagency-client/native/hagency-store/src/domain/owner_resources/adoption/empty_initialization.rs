//! Durable preparation for a genuinely empty original device. Version four is
//! a separate plan kind, not a resource adoption with a fake provider or quota.
//! Preparation alone never fences sources or activates management.
use super::*;
mod sources;
mod activation;
pub(crate) use activation::EmptyDeviceCustodyRequest;
pub use activation::VerifiedEmptyDeviceJournal;
pub use sources::{EmptyDeviceTransfer, VerifiedEmptyDeviceArchives};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    EmptyDevice,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct EmptyPlan {
    pub(super) version: u16,
    kind: Kind,
    pub(super) initialization_id: String,
    pub(super) registry_id: String,
    pub(super) source_binding: String,
    pub(super) inventory_digest: String,
    pub(super) inventory: empty_device::Descriptor,
}

/// Display-only. No provider, account quota, native path or private receipt is
/// exposed; receiving this summary does not authorize a subsequent mutation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyDeviceInitializationSummary {
    pub initialization_id: String,
    pub phase: &'static str,
    pub personal_database_present: bool,
    pub completed_private_records: u32,
}

pub struct PreparedEmptyDeviceInitialization {
    authority: AdoptionAuthority,
    plan: EmptyPlan,
}
impl PreparedEmptyDeviceInitialization {
    pub fn plan_digest(&self) -> Result<String, Error> {
        self.plan.digest()
    }
    pub fn inventory_digest(&self) -> &str {
        &self.plan.inventory_digest
    }
    pub fn review_summary(&self) -> EmptyDeviceInitializationSummary {
        self.plan.summary()
    }
}
impl EmptyPlan {
    pub(super) fn digest(&self) -> Result<String, Error> {
        Ok(canonical::digest(&serde_json::to_value(self)?)?)
    }
    fn summary_at(&self, phase: i64) -> Result<EmptyDeviceInitializationSummary, Error> {
        let mut result = self.summary();
        result.phase = match phase {
            0 => "prepared",
            1 => "sources_fenced",
            2 => "registry_ready",
            3 => "custody_ready",
            4 => "active",
            _ => return Err(Error::Quarantined),
        };
        Ok(result)
    }
    fn summary(&self) -> EmptyDeviceInitializationSummary {
        EmptyDeviceInitializationSummary {
            initialization_id: self.initialization_id.clone(),
            phase: "prepared",
            personal_database_present: self.inventory.personal_present,
            completed_private_records: self.inventory.completed_private_records,
        }
    }
    fn check(&self, authority: &AdoptionAuthority) -> Result<(), Error> {
        authority.check()?;
        identifier(&self.initialization_id, 128)?;
        hash(&self.source_binding)?;
        hash(&self.inventory_digest)?;
        hash(&self.inventory.custody_digest)?;
        if self.version != 4
            || self.inventory.version != 1
            || !matches!(self.inventory.source_schema, 77..=111)
            || self.registry_id != authority.registry.id()
            || self.inventory.profile != authority.profile.digest
            || self.inventory.owner != authority.profile.account
            || self.inventory.personal_present != self.inventory.personal_digest.is_some()
            || canonical::digest(&serde_json::to_value(&self.inventory)?)? != self.inventory_digest
        {
            return Err(Error::Quarantined);
        }
        if let Some(digest) = &self.inventory.personal_digest {
            hash(digest)?;
        }
        if let Some(seat) = &self.inventory.original_empty_seat {
            seat.validate()?;
            if seat.declaration.is_some() {
                return Err(Error::Quarantined);
            }
        }
        Ok(())
    }
}

fn read(db: &Connection, authority: &AdoptionAuthority, id: &str) -> Result<EmptyPlan, Error> {
    let (plan, phase) = read_state(db, authority, id)?;
    if phase != 0 {
        return Err(Error::Conflict);
    }
    Ok(plan)
}
pub(super) fn read_state(
    db: &Connection,
    authority: &AdoptionAuthority,
    id: &str,
) -> Result<(EmptyPlan, i64), Error> {
    authority.check()?;
    let (profile, digest, raw, phase): (String, String, String, i64) = db.query_row(
        "SELECT CASE WHEN length(profile)=64 THEN profile END,CASE WHEN length(plan_digest)=64 THEN plan_digest END,CASE WHEN length(CAST(plan AS BLOB))<=16384 THEN plan END,phase FROM owner_resource_adoptions WHERE id=?1",
        [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    )?;
    if profile != authority.profile.digest {
        return Err(Error::LocalAuthority);
    }
    let plan: EmptyPlan = serde_json::from_str(&raw)?;
    plan.check(authority)?;
    // Source fencing is separate from original private custody and activation.
    // Later phases cannot be fabricated by changing a saved integer.
    if !matches!(phase, 0..=4) || plan.initialization_id != id || plan.digest()? != digest {
        return Err(Error::Quarantined);
    }
    if phase >= 1 {
        let actual: String = db.query_row(
            "SELECT source_evidence FROM owner_resource_adoptions WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        if actual != sources::source_evidence(&plan)? {
            return Err(Error::Quarantined);
        }
    }
    if phase >= 3 {
        let actual: String = db.query_row(
            "SELECT custody_evidence FROM owner_resource_adoptions WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        if actual != custody_evidence(&plan)? {
            return Err(Error::Quarantined);
        }
    }
    if phase == 4 {
        let actual: String = db.query_row("SELECT CASE WHEN length(CAST(result AS BLOB))<=4096 THEN result END FROM owner_resource_adoptions WHERE id=?1", [id], |r| r.get(0))?;
        if serde_json::from_str::<Value>(&actual)? != serde_json::to_value(plan.summary_at(4)?)? {
            return Err(Error::Quarantined);
        }
    }
    authority.check()?;
    Ok((plan, phase))
}

fn preflight(
    db: &Connection,
    authority: &AdoptionAuthority,
    plan: &EmptyPlan,
) -> Result<(), Error> {
    plan.check(authority)?;
    let existing: Option<String> = db
        .query_row(
            "SELECT id FROM owner_resource_adoptions WHERE profile=?1 OR id=?2",
            params![plan.inventory.profile, plan.initialization_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing
        && (id != plan.initialization_id || read(db, authority, &id)?.digest()? != plan.digest()?)
    {
        return Err(Error::Conflict);
    }
    ensure_profile_empty(db, plan)?;
    authority.check()
}
fn ensure_profile_empty(db: &Connection, plan: &EmptyPlan) -> Result<(), Error> {
    // The canonical registry may contain other owners. It must not already
    // contain account or resource history belonging to this supposedly new one.
    for table in [
        "owner_resource_grants",
        "management_account_profiles",
        "owner_resource_namespaces",
        "owner_operator_tasks",
        "owner_operator_task_comments",
        "owner_operator_task_histories",
        "owner_operator_graphs",
        "owner_operator_graph_commands",
        "owner_operator_graph_outbox",
        "owner_operator_graph_recipients",
    ] {
        if db.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE profile=?1)"),
            [&plan.inventory.profile],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Error::Conflict);
        }
    }
    Ok(())
}

pub(super) fn custody_evidence(plan: &EmptyPlan) -> Result<String, Error> {
    Ok(canonical::digest(
        &json!({"plan":plan.digest()?,"custody":plan.inventory.custody_digest,"count":plan.inventory.completed_private_records}),
    )?)
}

#[cfg(feature = "resource-adoption")]
pub(super) use sources::source_evidence;

impl OwnerResourceRegistry {
    /// Construct only from native inventory and finite authority. The host must
    /// persist its exact original selection before committing this preparation.
    pub fn empty_device_preparation(
        &self,
        authority: AdoptionAuthority,
        initialization_id: String,
        inventory: EmptyDeviceInventory,
        source_binding: String,
    ) -> Result<PreparedEmptyDeviceInitialization, Error> {
        authority.check()?;
        if !Arc::ptr_eq(&self.0, &authority.registry.0) {
            return Err(Error::LocalAuthority);
        }
        let plan = EmptyPlan {
            version: 4,
            kind: Kind::EmptyDevice,
            initialization_id,
            registry_id: self.id().into(),
            source_binding,
            inventory_digest: inventory.digest()?,
            inventory: inventory.0,
        };
        plan.check(&authority)?;
        if serde_json::to_vec(&plan)?.len() > 16384 {
            return Err(Error::Capacity);
        }
        Ok(PreparedEmptyDeviceInitialization { authority, plan })
    }
    pub async fn validate_empty_device_preparation(
        &self,
        prepared: &PreparedEmptyDeviceInitialization,
    ) -> Result<(), Error> {
        if !Arc::ptr_eq(&self.0, &prepared.authority.registry.0) {
            return Err(Error::LocalAuthority);
        }
        prepared.authority.check()?;
        let authority = prepared.authority.clone();
        let plan = prepared.plan.clone();
        self.0
            .store
            .call(16384, move |db| {
                let tx = db.db.unchecked_transaction()?;
                preflight(&tx, &authority, &plan)?;
                tx.commit()?;
                authority.check()
            })
            .await?;
        prepared.authority.check()
    }
    /// Persist the original prepared phase only. No account, seat, resource,
    /// allocation, provider call or source mutation is created by this write.
    pub async fn commit_empty_device_preparation(
        &self,
        prepared: PreparedEmptyDeviceInitialization,
    ) -> Result<EmptyDeviceInitializationSummary, Error> {
        let PreparedEmptyDeviceInitialization { authority, plan } = prepared;
        if !Arc::ptr_eq(&self.0, &authority.registry.0) {
            return Err(Error::LocalAuthority);
        }
        plan.check(&authority)?;
        let after = authority.clone();
        let result=self.0.store.call(16384,move |db| {
            let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            preflight(&tx,&authority,&plan)?;
            if !tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_adoptions WHERE id=?1)",[&plan.initialization_id],|r|r.get::<_,bool>(0))? {
                bounded_row(&tx,"owner_resource_adoptions","id",&plan.initialization_id,128)?;
                tx.execute("INSERT INTO owner_resource_adoptions(id,profile,plan_digest,plan,phase) VALUES(?1,?2,?3,?4,0)",params![plan.initialization_id,plan.inventory.profile,plan.digest()?,serde_json::to_string(&plan)?])?;
            }
            authority.check()?;
            tx.commit()?;
            authority.check().map_err(|_| Error::OutcomeUnknown)?;
            Ok(plan.summary())
        }).await?;
        after.check().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
    /// Recover only the original saved preparation with current native scope
    /// and the exact digest retained by the host. This never creates a row.
    pub async fn recover_empty_device_preparation(
        &self,
        authority: AdoptionAuthority,
        initialization_id: String,
        expected_digest: String,
    ) -> Result<PreparedEmptyDeviceInitialization, Error> {
        authority.check()?;
        if !Arc::ptr_eq(&self.0, &authority.registry.0) {
            return Err(Error::LocalAuthority);
        }
        identifier(&initialization_id, 128)?;
        hash(&expected_digest)?;
        let check = authority.clone();
        let plan = self
            .0
            .store
            .call(16384, move |db| {
                let tx = db.db.unchecked_transaction()?;
                let plan = read(&tx, &check, &initialization_id)?;
                if plan.digest()? != expected_digest {
                    return Err(Error::Conflict);
                }
                preflight(&tx, &check, &plan)?;
                tx.commit()?;
                check.check()?;
                Ok(plan)
            })
            .await?;
        authority.check()?;
        Ok(PreparedEmptyDeviceInitialization { authority, plan })
    }
}

#[cfg(test)]
mod tests;
