//! Provider-free initialization of a current profile. No source ledger is
//! opened, imported, fenced, or assigned capacity by this protocol.
use super::*;
use crate::management::ServerManagementJournal;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct CurrentPlan {
    version: u16,
    kind: String,
    pub(super) id: String,
    registry: String,
    pub(super) profile: String,
    pub(super) owner: String,
}
impl CurrentPlan {
    fn new(authority: &ProfileInitializationAuthority, id: String) -> Result<Self, Error> {
        authority.check()?;
        identifier(&id, 128)?;
        Ok(Self {
            version: 1,
            kind: "current_profile".into(),
            id,
            registry: authority.registry.id().into(),
            profile: authority.profile.digest.clone(),
            owner: authority.profile.account.clone(),
        })
    }
    fn digest(&self) -> Result<String, Error> {
        Ok(canonical::digest(&serde_json::to_value(self)?)?)
    }
    fn evidence(&self) -> Result<String, Error> {
        Ok(canonical::digest(
            &json!({"kind":"current_profile_journal","plan":self.digest()?}),
        )?)
    }
}
pub(super) fn read(
    db: &Connection,
    authority: &ProfileInitializationAuthority,
    id: &str,
) -> Result<CurrentPlan, Error> {
    authority.check()?;
    let row: (String, String, String, i64, String, String, String) = db.query_row(
        "SELECT profile,plan_digest,CASE WHEN length(CAST(plan AS BLOB))<=4096 THEN plan END,phase,source_evidence,custody_evidence,result FROM owner_resource_adoptions WHERE id=?1",
        [id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
    let plan: CurrentPlan = serde_json::from_str(&row.2)?;
    if plan != CurrentPlan::new(authority, id.into())?
        || row.0 != plan.profile
        || row.1 != plan.digest()?
        || row.3 != 4
        || row.4 != plan.evidence()?
        || row.5 != plan.evidence()?
        || row.6 != plan.digest()?
    {
        return Err(Error::LocalAuthority);
    }
    authority.check()?;
    Ok(plan)
}
pub(super) fn task_history(
    db: &Connection,
    authority: &ProfileInitializationAuthority,
    plan: &CurrentPlan,
) -> Result<bool, Error> {
    if &read(db, authority, &plan.id)? != plan {
        return Err(Error::LocalAuthority);
    }
    let row: Option<(String,String,String)> = db.query_row(
        "SELECT migration_id,receipt_digest,receipt FROM owner_operator_task_histories WHERE profile=?1",
        [&plan.profile], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    Ok(row
        == Some((
            plan.id.clone(),
            plan.digest()?,
            serde_json::to_string(plan)?,
        )))
}
impl OwnerResourceRegistry {
    /// The native host retains the current owner root and explicit setup consent.
    /// The private current journal is independently owner/profile-bound.
    pub async fn initialize_current_profile(
        &self,
        authority: ProfileInitializationAuthority,
        id: String,
        journal: ServerManagementJournal,
    ) -> Result<(), Error> {
        authority.check()?;
        if !Arc::ptr_eq(&self.0, &authority.registry.0)
            || !journal.is_live()
            || journal.owner() != authority.profile.account
            || journal.profile() != authority.profile.digest
        {
            return Err(Error::LocalAuthority);
        }
        let plan = CurrentPlan::new(&authority, id)?;
        let after = authority.clone();
        self.0.store.call(4096, move |db| {
            let tx = db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            authority.check()?;
            let existing: Option<String> = tx.query_row("SELECT id FROM owner_resource_adoptions WHERE profile=?1",
                [&plan.profile], |r| r.get(0)).optional()?;
            if let Some(id) = existing {
                if read(&tx,&authority,&id)? != plan || !task_history(&tx,&authority,&plan)? {
                    return Err(Error::Conflict);
                }
            } else {
                // Never reinterpret detached financial/operational history as
                // an empty profile, even when no resource grant remains active.
                for table in ["owner_resource_grants","management_account_profiles","owner_resource_namespaces",
                    "owner_operator_task_histories","owner_operator_tasks","owner_operator_task_comments",
                    "owner_operator_graphs","owner_operator_graph_commands","owner_operator_graph_outbox","owner_operator_graph_recipients"] {
                    let column = "profile";
                    let present: bool = tx.query_row(&format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {column}=?1)"),
                        [&plan.profile], |r| r.get(0))?;
                    if present { return Err(Error::ResourceSetupRequired); }
                }
                let raw = serde_json::to_string(&plan)?;
                tx.execute("INSERT INTO owner_resource_adoptions(id,profile,plan_digest,plan,phase,source_evidence,custody_evidence,result) VALUES(?1,?2,?3,?4,4,?5,?5,?3)",
                    params![plan.id,plan.profile,plan.digest()?,raw,plan.evidence()?])?;
                tx.execute("INSERT INTO owner_operator_task_histories(profile,migration_id,receipt_digest,receipt) VALUES(?1,?2,?3,?4)",
                    params![plan.profile,plan.id,plan.digest()?,raw])?;
            }
            authority.check()?;
            tx.commit()?;
            authority.check().map_err(|_|Error::OutcomeUnknown)
        }).await?;
        after.check().map_err(|_| Error::OutcomeUnknown)?;
        if !journal.is_live() {
            return Err(Error::OutcomeUnknown);
        }
        Ok(())
    }
    pub async fn current_profile_active(
        &self,
        authority: ProfileInitializationAuthority,
        id: String,
    ) -> Result<(), Error> {
        if !Arc::ptr_eq(&self.0, &authority.registry.0) {
            return Err(Error::LocalAuthority);
        }
        self.0
            .store
            .call(4096, move |db| {
                let tx = db.db.unchecked_transaction()?;
                let plan = read(&tx, &authority, &id)?;
                if !task_history(&tx, &authority, &plan)? {
                    return Err(Error::LocalAuthority);
                }
                tx.commit()?;
                authority.check()
            })
            .await
    }
    pub async fn current_management(
        &self,
        authority: ProfileInitializationAuthority,
        id: String,
        journal: ServerManagementJournal,
    ) -> Result<CurrentManagementStore, Error> {
        authority.check()?;
        if !Arc::ptr_eq(&self.0, &authority.registry.0)
            || !journal.is_live()
            || journal.owner() != authority.profile.account
            || journal.profile() != authority.profile.digest
        {
            return Err(Error::LocalAuthority);
        }
        let check = authority.clone();
        let plan = self
            .0
            .store
            .call(4096, move |db| {
                let tx = db.db.unchecked_transaction()?;
                let plan = read(&tx, &check, &id)?;
                if !task_history(&tx, &check, &plan)? {
                    return Err(Error::LocalAuthority);
                }
                tx.commit()?;
                check.check()?;
                Ok(plan)
            })
            .await?;
        authority.check()?;
        Ok(CurrentManagementStore::from_current(
            self.clone(),
            plan,
            journal,
        ))
    }
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TestCurrentManagement {
    registry: OwnerResourceRegistry,
    pub(crate) backing: CurrentManagementStore,
}
#[cfg(test)]
impl TestCurrentManagement {
    pub(crate) async fn open(path: &Path, owner: &str) -> Self {
        crate::private::directory(path).unwrap();
        let registry = OwnerResourceRegistry::open_root(&path.join("canonical")).unwrap();
        let journal =
            ServerManagementJournal::open(&path.join("server"), &"a".repeat(64), owner).unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        let authority = ProfileInitializationAuthority::new(
            registry.clone(),
            ProfileAuthority::new("a".repeat(64), owner.into(), until).unwrap(),
            until,
            || true,
        )
        .unwrap();
        registry
            .initialize_current_profile(authority.clone(), "current_test".into(), journal.clone())
            .await
            .unwrap();
        let backing = registry
            .current_management(authority, "current_test".into(), journal)
            .await
            .unwrap();
        Self { registry, backing }
    }
    pub(crate) async fn shutdown(&self) -> Result<(), Error> {
        self.backing.shutdown().await?;
        self.registry.shutdown().await
    }
}
#[cfg(test)]
impl std::ops::Deref for TestCurrentManagement {
    type Target = DomainStore;
    fn deref(&self) -> &Self::Target {
        &self.registry.0.store
    }
}

#[cfg(test)]
#[path = "current_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "current_management_tests.rs"]
mod management_tests;
