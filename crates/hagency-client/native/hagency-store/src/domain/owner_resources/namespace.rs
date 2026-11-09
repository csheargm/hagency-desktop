//! Canonical quota identity is independent of the original private execution home.
use super::*;
use crate::management::AccountModelQuery;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NamespaceEnrollment {
    pub command_id: String,
    pub account: AccountModelQuery,
    pub model: String,
    pub reasoning: Option<String>,
    pub monthly_tokens: u64,
}
impl NamespaceEnrollment {
    pub fn validate(&self) -> Result<(), Error> {
        hagency_core::project::identifier(&self.command_id, 128)?;
        self.account.validate()?;
        if self.model.is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
            || self
                .reasoning
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.len() > 64 || v.chars().any(char::is_control))
            || self.monthly_tokens == 0
            || self.monthly_tokens > JSON_SAFE_MAX
        {
            return Err(InvalidInput("invalid namespace resource selection").into());
        }
        Ok(())
    }
}
/// Host-only selection from the original registry. No serialized path or handle.
pub struct ResourceAccountSelection {
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    pub(super) query: AccountModelQuery,
    generation: u64,
    launch: crate::ManagedLaunch,
}
impl ResourceAccountSelection {
    pub fn launch(&self) -> &crate::ManagedLaunch {
        &self.launch
    }
    pub(super) fn check(&self) -> Result<(), Error> {
        self.profile.check()?;
        if self.registry.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        self.launch.check()
    }
    pub(super) fn key(&self) -> serde_json::Value {
        json!({"account":self.query.account_id,"generation":self.generation,"revision":self.query.expected_revision})
    }
    pub(super) fn check_on(&self, db: &Connection) -> Result<(), Error> {
        self.check()?;
        let current:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM managed_accounts a JOIN management_account_profiles p ON p.account_id=a.id WHERE a.id=?1 AND a.generation=?2 AND a.state='active' AND p.profile=?3) AND NOT EXISTS(SELECT 1 FROM account_login_attempts WHERE account_id=?1 AND state='attempting')",params![self.query.account_id,self.generation,self.profile.digest],|r|r.get(0))?;
        if !current {
            return Err(Error::LocalAuthority);
        }
        self.check()
    }
    pub async fn check_current(&self) -> Result<(), Error> {
        self.check()?;
        let fresh = self
            .registry
            .select_resource_account(self.profile.clone(), self.query.clone())
            .await?;
        if fresh.generation != self.generation {
            return Err(Error::LocalAuthority);
        }
        self.check()
    }
}
/// Original namespace retained through execution; each live check also checks
/// resource revision and current profile access. A saved association is not login.
pub struct ResourceNamespace {
    registry: OwnerResourceRegistry,
    profile: ProfileAuthority,
    resource: String,
    revision: String,
    access_revision: u64,
    account: String,
    owner_profile: String,
    generation: u64,
    launch: crate::ManagedLaunch,
}
impl ResourceNamespace {
    pub(super) fn matches_edit_binding(&self, binding: &VerifiedOwnerResourceBinding) -> Result<(), Error> {
        self.profile.check()?;
        self.launch.check()?;
        if self.registry.id()!=binding.registry_id || self.registry.0.closed.load(Ordering::Acquire)
            || self.profile.digest!=binding.profile.digest || self.profile.account!=binding.profile.account
            || !self.matches_resource(&binding.resource_id,&binding.resource_revision) {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    pub(super) fn check_edit_on(&self, db: &Connection, binding: &VerifiedOwnerResourceBinding) -> Result<(), Error> {
        self.matches_edit_binding(binding)?;
        let current: bool=db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_resource_namespaces n JOIN managed_accounts a ON a.id=n.account_id JOIN management_account_profiles p ON p.account_id=a.id AND p.profile=n.profile JOIN owner_resource_grants g ON g.resource_id=n.resource_id WHERE n.resource_id=?1 AND n.account_id=?2 AND n.generation=?3 AND a.generation=n.generation AND a.state='active' AND g.profile=?4 AND g.revision=?5 AND g.active=1 AND n.profile=?6) AND NOT EXISTS(SELECT 1 FROM account_login_attempts WHERE account_id=?2 AND state='attempting')",
            params![self.resource,self.account,self.generation,self.profile.digest,self.access_revision,self.owner_profile],|r|r.get(0))?;
        if !current {return Err(Error::LocalAuthority);}
        Ok(())
    }
    pub fn launch(&self) -> &crate::ManagedLaunch {
        &self.launch
    }
    pub async fn check(&self) -> Result<(), Error> {
        self.check_with(self.profile.clone()).await
    }
    pub fn matches_resource(&self, resource: &str, revision: &str) -> bool {
        self.resource == resource && self.revision == revision
    }
    /// Execution renewal supplies current authority for the same profile;
    /// retaining an original namespace never renews the startup session.
    pub async fn check_with(&self, profile: ProfileAuthority) -> Result<(), Error> {
        if profile.digest != self.profile.digest || profile.account != self.profile.account {
            return Err(Error::LocalAuthority);
        }
        self.launch.check()?;
        let fresh = self
            .registry
            .resource_namespace(profile, self.resource.clone(), self.revision.clone())
            .await?
            .ok_or(Error::LocalAuthority)?;
        if fresh.account != self.account
            || fresh.owner_profile != self.owner_profile
            || fresh.generation != self.generation
            || fresh.access_revision != self.access_revision
        {
            return Err(Error::LocalAuthority);
        }
        self.launch.check()
    }
}
impl OwnerResourceRegistry {
    pub async fn select_resource_account(
        &self,
        profile: ProfileAuthority,
        query: AccountModelQuery,
    ) -> Result<ResourceAccountSelection, Error> {
        profile.check()?;
        query.validate()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let registry = self.clone();
        let caller = profile.clone();
        let result = self
            .0
            .store
            .call(4096, move |db| {
                profile.check()?;
                let launch = db.management_inspect_account(&profile.digest, &query)?;
                let generation = db.db.query_row(
                    "SELECT generation FROM managed_accounts WHERE id=?1",
                    [&query.account_id],
                    |r| r.get(0),
                )?;
                let selected = ResourceAccountSelection {
                    registry,
                    profile,
                    query,
                    generation,
                    launch,
                };
                selected.check_on(&db.db)?;
                Ok(selected)
            })
            .await?;
        caller.check()?;
        result.check()?;
        Ok(result)
    }
    pub fn prepare_namespace_enrollment(
        &self,
        profile: ProfileAuthority,
        input: NamespaceEnrollment,
        selected: ResourceAccountSelection,
        proof: VerifiedProviderAccount,
    ) -> Result<OwnerResourceConsent, Error> {
        input.validate()?;
        selected.check()?;
        if proof.runtime != "codex"
            || selected.registry.id() != self.id()
            || selected.profile.digest != profile.digest
            || selected.profile.account != profile.account
            || selected.query.account_id != input.account.account_id
            || selected.query.expected_revision != input.account.expected_revision
        {
            return Err(Error::LocalAuthority);
        }
        let expires = proof.expires;
        let mut consent = self.prepare_enrollment(
            profile,
            input.command_id,
            proof,
            input.model,
            input.reasoning,
            input.monthly_tokens,
            expires,
        )?;
        match &mut consent.action {
            Action::Enroll { namespace, .. } => *namespace = Some(Box::new(selected)),
            _ => return Err(Error::State),
        }
        consent.check()?;
        Ok(consent)
    }
    /// Recover the exact original result before provider observation. No new proof,
    /// declaration, namespace or grant is created when the previous reply was lost.
    pub async fn recover_namespace_enrollment(
        &self,
        profile: ProfileAuthority,
        input: NamespaceEnrollment,
    ) -> Result<Option<OwnerResourceReceipt>, Error> {
        profile.check()?;
        input.validate()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let digest = canonical::digest(&serde_json::to_value(&input)?)?;
        let fence = self.0.clone();
        let caller = profile.clone();
        let result=self.0.store.call(4096,move |db|{
            profile.check()?;
            let old:Option<(String,String,String)>=db.db.query_row("SELECT n.profile,n.input_digest,r.result FROM owner_namespace_enrollments n JOIN owner_resource_receipts r ON r.command_id=n.command_id WHERE n.command_id=?1",[&input.command_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let result=match old {
                Some((owner,original,value))=>{
                    if owner!=profile.digest {return Err(Error::LocalAuthority);}
                    if original!=digest {return Err(Error::Conflict);}
                    Some(serde_json::from_str(&value)?)
                }
                None=>{
                    if db.db.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_receipts WHERE command_id=?1)",[&input.command_id],|r|r.get::<_,bool>(0))? {return Err(Error::Conflict);}
                    None
                }
            };
            profile.check()?;if fence.closed.load(Ordering::Acquire) {return Err(Error::LocalAuthority);}
            Ok(result)
        }).await?;
        caller.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }
    pub async fn resource_namespace(
        &self,
        profile: ProfileAuthority,
        resource: String,
        revision: String,
    ) -> Result<Option<ResourceNamespace>, Error> {
        profile.check()?;
        hash(&revision)?;
        hagency_core::project::identifier(&resource, 128)?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let registry = self.clone();
        let caller = profile.clone();
        let result=self.0.store.call(4096,move |db|{
            profile.check()?;authorize_resource(&db.db,&profile,&resource)?;
            let definition=read_resource(&db.db,&resource)?;
            if crate::resource_publication_revision(&definition)?!=revision {return Err(Error::Conflict);}
            let row:Option<(String,u64,String)>=db.db.query_row("SELECT account_id,generation,profile FROM owner_resource_namespaces WHERE resource_id=?1",[&resource],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let result=match row {
                Some((account,generation,owner))=>{
                    if definition.framework!="codex" || !definition.preset_id.starts_with("owner_ns_") {return Err(Error::Quarantined);}
                    let current:bool=db.db.query_row("SELECT EXISTS(SELECT 1 FROM managed_accounts a JOIN management_account_profiles p ON p.account_id=a.id WHERE a.id=?1 AND a.generation=?2 AND a.state='active' AND p.profile=?3) AND NOT EXISTS(SELECT 1 FROM account_login_attempts WHERE account_id=?1 AND state='attempting')",params![account,generation,owner],|r|r.get(0))?;
                    if !current {return Err(Error::LocalAuthority);}
                    let launch=db.managed_account(&account)?.prepare_login()?;
                    let access_revision=db.db.query_row("SELECT revision FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2 AND active=1",params![profile.digest,resource],|r|r.get(0))?;
                    Some(ResourceNamespace{registry:registry.clone(),profile:profile.clone(),resource,revision,access_revision,account,owner_profile:owner,generation,launch})
                }
                None=>{
                    if definition.preset_id.starts_with("owner_ns_") {return Err(Error::Quarantined);}
                    None
                }
            };
            profile.check()?;if registry.0.closed.load(Ordering::Acquire) {return Err(Error::LocalAuthority);}
            Ok(result)
        }).await?;
        caller.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }
}
pub(super) fn bind(
    db: &Connection,
    resource: &str,
    selection: &ResourceAccountSelection,
) -> Result<(), Error> {
    selection.check_on(db)?;
    let row:Option<(String,u64,String)>=db.query_row("SELECT account_id,generation,profile FROM owner_resource_namespaces WHERE resource_id=?1",[resource],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let wanted = (
        selection.query.account_id.clone(),
        selection.generation,
        selection.profile.digest.clone(),
    );
    match row {
        Some(old) if old == wanted => {}
        Some(_) => return Err(Error::Conflict),
        None => {
            db.execute(
                "INSERT INTO owner_resource_namespaces VALUES(?1,?2,?3,?4)",
                params![resource, wanted.0, wanted.1, wanted.2],
            )?;
        }
    }
    selection.check_on(db)
}
#[cfg(test)]
mod tests;
