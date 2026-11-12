use super::*;

/// A fresh authenticated delivery. Historical responses cannot construct this
/// proof and saved journal bytes can never reconstruct its effect custody.
pub struct VerifiedSharedRuntimeDelivery {
    enrollment: VerifiedManagementEnrollment,
    command: SharedRuntimeCommand,
    digest: String,
    expires: Instant,
}
impl VerifiedSharedRuntimeDelivery {
    pub fn new(
        enrollment: VerifiedManagementEnrollment,
        command: SharedRuntimeCommand,
        digest: String,
        execution_allowed: bool,
        expires: Instant,
    ) -> Result<Self, Error> {
        enrollment.check()?;
        command.validate(&enrollment.profile.account)?;
        if !execution_allowed
            || digest != canonical::digest(&serde_json::to_value(&command)?)?
            || serde_json::to_vec(&command)?.len() > 16 * 1024
            || expires > Instant::now() + Duration::from_secs(30)
        {
            return Err(Error::LocalAuthority);
        }
        let remaining = command.expires_at_ms.saturating_sub(graphs::now_ms()?);
        let expires = expires
            .min(enrollment.expires)
            .min(Instant::now() + Duration::from_millis(remaining));
        let p = Self {
            enrollment,
            command,
            digest,
            expires,
        };
        p.check()?;
        Ok(p)
    }
    fn check(&self) -> Result<(), Error> {
        self.enrollment.check()?;
        let now = graphs::now_ms()?;
        if Instant::now() >= self.expires
            || now < self.command.issued_at_ms
            || now >= self.command.expires_at_ms
            || self.enrollment.executor_generation
                != self.command.reserve_command.executor_generation
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SharedRuntimeHistory {
    pub command: SharedRuntimeCommand,
    pub command_digest: String,
    pub issued_revision: u64,
}
/// Original one-shot process right. No Clone or deserialization. Dropping it
/// leaves conservative issued history; a later read never returns another.
pub struct SharedRuntimeCustody {
    pub(super) scope: Scope,
    pub(super) command: SharedRuntimeCommand,
    pub(super) digest: String,
    pub(super) incarnation: String,
    pub(super) provider: String,
    pub(super) enrollment: Arc<VerifiedManagementEnrollment>,
}
impl SharedRuntimeCustody {
    pub fn command(&self) -> &SharedRuntimeCommand {
        &self.command
    }
    pub fn command_digest(&self) -> &str {
        &self.digest
    }
    pub fn provider_digest(&self) -> &str {
        &self.provider
    }
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    pub fn deadline(&self) -> Instant {
        self.scope.expires
    }
    pub fn check(&self) -> Result<(), Error> {
        self.scope.check(&self.scope.registry)?;
        self.enrollment.check()
    }
}
pub enum SharedRuntimeClaim {
    Issued(Box<SharedRuntimeCustody>),
    History(Box<SharedRuntimeHistory>),
}
fn original(
    db: &Connection,
    p: &VerifiedSharedRuntimeDelivery,
) -> Result<(Binding, String), Error> {
    original_command(db, &p.enrollment, &p.command)
}
pub(super) fn original_command(
    db: &Connection,
    e: &VerifiedManagementEnrollment,
    c: &SharedRuntimeCommand,
) -> Result<(Binding, String), Error> {
    let saved: ManagedAgentReservationReceipt = receipt(
        db,
        &e.profile.digest,
        &e.executor_device_id,
        &c.reserve_command.command_id,
        &c.reserve_command_digest,
    )?
    .ok_or(Error::LocalAuthority)?;
    if saved.state != "reserved"
        || saved.agent_reservation_id.as_deref() != Some(&c.agent_reservation_id)
        || canonical::digest(&serde_json::to_value(&saved)?)? != c.reserve_receipt_digest
    {
        return Err(Error::LocalAuthority);
    }
    let b = authority::original(db, e, &c.agent_reservation_id, true)?;
    if b.digest != c.reserve_command_digest
        || canonical::digest(&serde_json::to_value(&b.command)?)?
            != canonical::digest(&serde_json::to_value(&c.reserve_command)?)?
    {
        return Err(Error::Conflict);
    }
    let binding = canonical::digest(
        &json!({"child":c.agent_reservation_id,"assignmentId":c.assignment_id,"assignmentGeneration":c.assignment_generation,"commandDigest":b.digest,"providerDigest":b.provider}),
    )?;
    let old:Option<(String,u64,String,String)>=db.query_row("SELECT assignment_id,assignment_generation,binding_digest,provider_digest FROM owner_managed_assignment_pins WHERE child_id=?1",[&c.agent_reservation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if old.is_some_and(|(a, g, h, provider)| {
        a != c.assignment_id
            || g != c.assignment_generation
            || h != binding
            || provider != b.provider
    }) {
        return Err(Error::Conflict);
    }
    Ok((b, binding))
}
fn history(
    db: &Connection,
    c: &SharedRuntimeCommand,
) -> Result<Option<SharedRuntimeHistory>, Error> {
    let row:Option<(String,String,u64)>=db.query_row("SELECT command_payload,command_digest,issued_revision FROM owner_shared_runtime_lifetimes WHERE child_id=?1",[&c.agent_reservation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let Some((payload, digest, issued_revision)) = row else {
        return Ok(None);
    };
    let command: SharedRuntimeCommand = serde_json::from_str(&payload)?;
    if canonical::digest(&serde_json::to_value(&command)?)? != digest
        || canonical::digest(&serde_json::to_value(c)?)? != digest
    {
        return Err(Error::Conflict);
    }
    Ok(Some(SharedRuntimeHistory {
        command,
        command_digest: digest,
        issued_revision,
    }))
}
fn preflight(db: &Connection, p: &VerifiedSharedRuntimeDelivery) -> Result<(), Error> {
    let c = &p.command;
    initial_unstarted::require_not_void(db, c)?;
    authority::original(db, &p.enrollment, &c.agent_reservation_id, false)?;
    if read_resource(db, &c.reserve_command.resource_id)?.framework != c.runtime()? {
        return Err(Error::LocalAuthority);
    }
    if db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_managed_runtime_lifetimes WHERE child_id=?1)",
        [&c.agent_reservation_id],
        |r| r.get::<_, bool>(0),
    )? {
        return Err(Error::Conflict);
    }
    if provision::history(db, &c.agent_reservation_id)?.state != "pending" {
        return Err(Error::OutcomeUnknown);
    }
    Ok(())
}
impl OwnerResourceRegistry {
    pub async fn prepare_shared_runtime_delivery(
        &self,
        p: VerifiedSharedRuntimeDelivery,
    ) -> Result<Option<VerifiedSharedRuntimeDelivery>, Error> {
        p.check()?;
        let expires = p.expires;
        let profile = p.enrollment.profile.clone();
        let registry = self.0.clone();
        let result = self
            .0
            .store
            .call(32 * 1024, move |db| {
                p.check()?;
                if registry.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                original(&db.db, &p)?;
                if history(&db.db, &p.command)?.is_some() {
                    p.check()?;
                    return Ok(None);
                }
                preflight(&db.db, &p)?;
                p.check()?;
                Ok(Some(p))
            })
            .await?;
        after(&profile, &self.0, expires)?;
        Ok(result)
    }
    pub async fn claim_shared_runtime(
        &self,
        p: VerifiedSharedRuntimeDelivery,
    ) -> Result<SharedRuntimeClaim, Error> {
        p.check()?;
        let expires = p.expires;
        let profile = p.enrollment.profile.clone();
        let registry = self.0.clone();
        let result=self.0.store.call(32*1024,move|db|{
            p.check()?;if registry.closed.load(Ordering::Acquire){return Err(Error::LocalAuthority)}
            let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (b,binding)=original(&tx,&p)?;let c=&p.command;
            if let Some(h)=history(&tx,c)?{p.check()?;return Ok(SharedRuntimeClaim::History(Box::new(h)))}
            preflight(&tx,&p)?;
            let old=provision::history(&tx,&c.agent_reservation_id)?;
            tx.execute("INSERT OR IGNORE INTO owner_managed_assignment_pins VALUES(?1,?2,?3,?4,?5)",params![c.agent_reservation_id,c.assignment_id,c.assignment_generation,binding,b.provider])?;
            let rev=revision(&tx)?;let incarnation=format!("shared_{rev}_{}",c.agent_reservation_id);
            tx.execute("INSERT INTO owner_shared_runtime_lifetimes VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",params![c.agent_reservation_id,p.enrollment.profile.digest,c.assignment_id,c.command_id,canonical::encode(&serde_json::to_value(c)?)?,p.digest,binding,b.provider,rev,incarnation])?;
            tx.execute("UPDATE owner_managed_effects SET state='issued',fence=?2 WHERE id=?1 AND state='pending'",params![old.effect_id,rev])?;
            let scope=Scope{registry:registry.clone(),profile:p.enrollment.profile.clone(),child:c.agent_reservation_id.clone(),target:old.effect_id,binding,issued_revision:rev,expires};
            p.check()?;tx.commit()?;p.check().map_err(|_|Error::OutcomeUnknown)?;
            Ok(SharedRuntimeClaim::Issued(Box::new(SharedRuntimeCustody{scope,command:p.command,digest:p.digest,incarnation,provider:b.provider,enrollment:Arc::new(p.enrollment)})))
        }).await?;
        after(&profile, &self.0, expires)?;
        Ok(result)
    }
    /// Exact history only under fresh same-installation authorization. An
    /// expired original spawn window does not hide saved command custody.
    pub async fn shared_runtime_history(
        &self,
        a: ManagedCleanupAuthority,
        c: SharedRuntimeCommand,
    ) -> Result<Option<SharedRuntimeHistory>, Error> {
        a.check(&self.0)?;
        c.validate(&a.enrollment.profile.account)?;
        if a.child != c.agent_reservation_id {
            return Err(Error::LocalAuthority);
        }
        let profile = a.enrollment.profile.clone();
        let expires = a.deadline();
        let registry = self.0.clone();
        let result = self
            .0
            .store
            .call(32 * 1024, move |db| {
                a.check(&registry)?;
                authority::cleanup(&db.db, &a)?;
                let value = history(&db.db, &c)?;
                a.check(&registry)?;
                Ok(value)
            })
            .await?;
        after(&profile, &self.0, expires)?;
        Ok(result)
    }
}
