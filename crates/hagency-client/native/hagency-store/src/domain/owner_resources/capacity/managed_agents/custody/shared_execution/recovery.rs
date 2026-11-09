use super::*;
/// Private original work and saved evidence. This type never contains execution
/// or provider custody; a restart may upload evidence but cannot repeat a turn.
#[derive(Clone)]
pub struct SharedSavedWork {
    pub work: SharedWorkItem,
    pub receipt: Option<SharedWorkReceipt>,
}
impl OwnerResourceRegistry {
    /// Current same-installation cleanup proof. It validates the exact original
    /// reservation; it never permits new runtime or call effects.
    pub async fn shared_cleanup_authority(
        &self,
        enrollment: VerifiedManagementEnrollment,
        c: SharedRuntimeCommand,
    ) -> Result<ManagedCleanupAuthority, Error> {
        self.find_shared_cleanup_authority(enrollment, c)
            .await?
            .ok_or(Error::LocalAuthority)
    }
    /// An authenticated historical delivery may outlive all local business
    /// records. Missing original custody is distinct from a failed gate or a
    /// mismatched existing row, and cannot authorize any write or execution.
    pub async fn find_shared_cleanup_authority(
        &self,
        enrollment: VerifiedManagementEnrollment,
        c: SharedRuntimeCommand,
    ) -> Result<Option<ManagedCleanupAuthority>, Error> {
        enrollment.check()?;
        c.validate(&enrollment.profile.account)?;
        if let Some(grant) = &c.reserve_command.pool_grant
            && (!enrollment.matches_cleanup(grant)
                || enrollment.executor_generation < grant.executor_generation)
        {
            return Err(Error::LocalAuthority);
        }
        let a = ManagedCleanupAuthority {
            expires: enrollment.expires,
            child: c.agent_reservation_id.clone(),
            enrollment,
        };
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let registry = self.0.clone();
        let a = self
            .0
            .store
            .call(32 * 1024, move |db| {
                a.check(&registry)?;
                let exists: bool = db.db.query_row(
                    "SELECT EXISTS(SELECT 1 FROM owner_managed_agents WHERE id=?1)",
                    [&a.child],
                    |r| r.get(0),
                )?;
                if !exists {
                    a.check(&registry)?;
                    return Ok(None);
                }
                let b = authority::cleanup(&db.db, &a)?;
                if b.digest != c.reserve_command_digest
                    || canonical::digest(&serde_json::to_value(&b.command)?)?
                        != canonical::digest(&serde_json::to_value(&c.reserve_command)?)?
                {
                    return Err(Error::Conflict);
                }
                a.check(&registry)?;
                Ok(Some(a))
            })
            .await?;
        after(&p, &self.0, expires)?;
        Ok(a)
    }
    /// Bounded original inbox discovery for restart evidence upload. Identifiers
    /// confer no call authority; missing or unreported observations stay held.
    pub async fn shared_original_work_ids(
        &self,
        a: ManagedCleanupAuthority,
        c: SharedRuntimeCommand,
    ) -> Result<Vec<String>, Error> {
        a.check(&self.0)?;
        c.validate(&a.enrollment.profile.account)?;
        if a.child != c.agent_reservation_id {
            return Err(Error::LocalAuthority);
        }
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let registry = self.0.clone();
        let result=self.0.store.call(8*1024,move|db|{
            a.check(&registry)?;authority::cleanup(&db.db,&a)?;
            let digest=canonical::digest(&serde_json::to_value(&c)?)?;
            let saved:Option<String>=db.db.query_row("SELECT command_digest FROM owner_shared_runtime_lifetimes WHERE child_id=?1 AND profile=?2",params![a.child,a.enrollment.profile.digest],|r|r.get(0)).optional()?;
            if saved.is_none(){a.check(&registry)?;return Ok(Vec::new())}
            if saved.as_deref()!=Some(&digest){return Err(Error::LocalAuthority)}
            let mut q=db.db.prepare("SELECT id FROM owner_shared_work_inbox WHERE child_id=?1 AND json_extract(payload,'$.runtimeCommandDigest')=?2 ORDER BY id LIMIT 33")?;
            let ids=q.query_map(params![a.child,digest],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            if ids.len()>32{return Err(Error::Capacity)}
            for id in &ids {identifier(id,128)?;}
            a.check(&registry)?;Ok(ids)
        }).await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
    pub async fn recover_shared_work(
        &self,
        a: ManagedCleanupAuthority,
        c: SharedRuntimeCommand,
        work_id: String,
        observation_id: Option<String>,
    ) -> Result<Option<SharedSavedWork>, Error> {
        a.check(&self.0)?;
        c.validate(&a.enrollment.profile.account)?;
        identifier(&work_id, 128)?;
        if let Some(id) = &observation_id {
            identifier(id, 128)?;
        }
        if a.child != c.agent_reservation_id {
            return Err(Error::LocalAuthority);
        }
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let registry = self.0.clone();
        let result=self.0.store.call(48*1024,move|db|{
            a.check(&registry)?;authority::cleanup(&db.db,&a)?;
            let command_digest=canonical::digest(&serde_json::to_value(&c)?)?;
            let original:Option<(String,String)>=db.db.query_row("SELECT command_digest,incarnation FROM owner_shared_runtime_lifetimes WHERE child_id=?1 AND profile=?2",params![a.child,a.enrollment.profile.digest],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if original.is_none(){a.check(&registry)?;return Ok(None)}
            if original.as_ref().is_some_and(|v|v.0!=command_digest){return Err(Error::LocalAuthority)}
            let payload:Option<String>=db.db.query_row("SELECT payload FROM owner_shared_work_inbox WHERE id=?1 AND child_id=?2",params![work_id,a.child],|r|r.get(0)).optional()?;
            let result=if let Some(payload)=payload {
                let work:SharedWorkItem=serde_json::from_str(&payload)?;work.validate()?;
                if work.runtime_command_digest!=command_digest||work.assignment_id!=c.assignment_id{return Err(Error::LocalAuthority)}
                let receipt:Option<String>=db.db.query_row("SELECT payload FROM owner_shared_work_observations WHERE work_id=?1 AND (?2 IS NULL OR id=?2) ORDER BY ledger_revision DESC LIMIT 1",params![work.id,observation_id],|r|r.get(0)).optional()?;
                let receipt=receipt.map(|v|serde_json::from_str::<SharedWorkReceipt>(&v)).transpose()?;
                if let Some(r)=&receipt {
                    let provider:String=db.db.query_row("SELECT provider_digest FROM owner_shared_runtime_lifetimes WHERE child_id=?1",[&a.child],|r|r.get(0))?;
                    let process=canonical::digest(&json!({"commandDigest":command_digest,"incarnation":original.unwrap().1,"providerDigest":provider}))?;
                    r.validate_for(&work,&process)?;
                }
                Some(SharedSavedWork{work,receipt})
            }else {None};
            a.check(&registry)?;Ok(result)
        }).await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
}
