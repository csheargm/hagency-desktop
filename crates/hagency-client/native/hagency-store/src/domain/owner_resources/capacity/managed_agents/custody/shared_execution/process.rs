use super::*;
use hagency_runtime::owned::{
    ManagedProcessState, SharedTextProcessObservation, SharedTextProcessSource,
};
/// Original supervisor custody. This can report late evidence after logout, but
/// cannot mint a new process or extend a business/activity lease.
pub struct SharedProcessSink {
    pub(super) custody: Arc<SharedRuntimeCustody>,
    pub(super) source: SharedTextProcessSource,
}
fn bound(db: &Connection, c: &SharedRuntimeCustody) -> Result<(), Error> {
    let (profile,digest,incarnation,issued,binding):(String,String,String,u64,String)=db.query_row("SELECT profile,command_digest,incarnation,issued_revision,binding_digest FROM owner_shared_runtime_lifetimes WHERE child_id=?1",[&c.scope.child],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    if profile != c.scope.profile.digest
        || digest != c.digest
        || incarnation != c.incarnation
        || issued != c.scope.issued_revision
        || binding != c.scope.binding
    {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}
impl SharedRuntimeCustody {
    /// Binding preserves original actual-process cleanup custody even if the
    /// issuance lease expired while spawn was in flight. New qualification
    /// and activity still require all current authority gates below.
    pub fn bind_process(self, source: SharedTextProcessSource) -> Result<SharedProcessSink, Error> {
        source
            .claim_managed_custody(&self.digest, &self.incarnation)
            .map_err(|_| Error::LocalAuthority)?;
        Ok(SharedProcessSink {
            custody: Arc::new(self),
            source,
        })
    }
}
impl SharedProcessSink {
    pub fn process_digest(&self) -> Result<String, Error> {
        canonical::digest(&json!({"commandDigest":self.custody.digest,"incarnation":self.custody.incarnation,"providerDigest":self.custody.provider})).map_err(Into::into)
    }
    pub fn source(&self) -> &SharedTextProcessSource {
        &self.source
    }
    pub fn command(&self) -> &SharedRuntimeCommand {
        &self.custody.command
    }
    pub async fn record(
        &self,
        o: SharedTextProcessObservation,
    ) -> Result<SharedProcessReceipt, Error> {
        if !self.source.same(o.source())
            || !self
                .source
                .has_managed_custody(&self.custody.digest, &self.custody.incarnation)
        {
            return Err(Error::LocalAuthority);
        }
        let qualified = o.state() == ManagedProcessState::Qualified;
        if qualified {
            self.custody.check()?;
            if !self.source.is_live()
                || o.provider_digest() != Some(&self.custody.provider)
                || o.policy_digest()
                    != Some(
                        canonical::digest(&json!({"policy":self.custody.command.policy}))?.as_str(),
                    )
            {
                return Err(Error::LocalAuthority);
            }
        }
        let state = match o.state() {
            ManagedProcessState::Qualified => "qualified",
            ManagedProcessState::Stopped => "stopped",
            ManagedProcessState::Unknown => "unknown",
        }
        .to_owned();
        let stopping = state == "stopped";
        let policy = o.policy_digest().map(str::to_owned);
        let process = self.process_digest()?;
        let id = format!(
            "spro_{}",
            canonical::digest(
                &json!({"processDigest":process,"state":state,"policyDigest":policy})
            )?
        );
        let c = self.custody.clone();
        let source = self.source.clone();
        let registry = c.scope.registry.clone();
        let result=registry.store.call(32*1024,move|db|{
   if qualified && c.scope.registry.closed.load(Ordering::Acquire){return Err(Error::LocalAuthority)}
   let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;bound(&tx,&c)?;
   if qualified {demand::require_no_owner_stop(&tx,&c.enrollment.profile.digest,&c.scope.child)?;}
   if let Some(payload)=tx.query_row("SELECT payload FROM owner_shared_process_observations WHERE id=?1",[&id],|r|r.get::<_,String>(0)).optional()?{if qualified{c.check()?;if !source.is_live(){return Err(Error::LocalAuthority)}}return Ok(serde_json::from_str(&payload)?)}
   let stopped:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_process_observations WHERE child_id=?1 AND json_extract(payload,'$.state')='stopped')",[&c.scope.child],|r|r.get(0))?;
   if stopped && state!="stopped" {return Err(Error::Conflict)}
   if qualified{
    c.check()?;let b=authority::original(&tx,&c.enrollment,&c.scope.child,false)?;
    if b.provider!=c.provider||!source.is_live(){return Err(Error::LocalAuthority)}
    observations::check_scope(&tx,&c.scope,"provision")?;
    observations::provision(&tx,&c.scope.child,&c.scope.target,ManagedProvisionObservation::shared_text(format!("{id}_effect"),&o,process.clone())?)?;
   }
   let rev=revision(&tx)?;
   let receipt=SharedProcessReceipt{version:1,observation_id:id.clone(),command_id:c.command.command_id.clone(),command_digest:c.digest.clone(),assignment_id:c.command.assignment_id.clone(),agent_reservation_id:c.scope.child.clone(),ledger_revision:rev,process_digest:Some(process),state,policy_digest:policy};
   receipt.validate_for(&c.command)?;
   let payload=canonical::encode(&serde_json::to_value(&receipt)?)?;let digest=canonical::digest(&serde_json::to_value(&receipt)?)?;
   bounded_row(&tx,"owner_shared_process_observations","id",&id,100_000)?;
   tx.execute("INSERT INTO owner_shared_process_observations VALUES(?1,?2,?3,?4,?5)",params![id,c.scope.child,payload,digest,rev])?;
   if stopping {observations::close_stopped_calls(&tx,&c.scope.child)?;}
   if qualified {c.check()?;if !source.is_live(){return Err(Error::LocalAuthority)}}
   tx.commit()?;if qualified{c.check().map_err(|_|Error::OutcomeUnknown)?;if !source.is_live(){return Err(Error::OutcomeUnknown)}}Ok(receipt)
  }).await?;
        if qualified {
            self.custody.check().map_err(|_| Error::OutcomeUnknown)?;
            if !self.source.is_live() {
                return Err(Error::OutcomeUnknown);
            }
        }
        Ok(result)
    }
}
impl OwnerResourceRegistry {
    pub async fn recover_shared_process(
        &self,
        a: ManagedCleanupAuthority,
        c: SharedRuntimeCommand,
        observation_id: Option<String>,
    ) -> Result<Option<SharedProcessReceipt>, Error> {
        a.check(&self.0)?;
        c.validate(&a.enrollment.profile.account)?;
        if a.child != c.agent_reservation_id {
            return Err(Error::LocalAuthority);
        }
        if let Some(id) = &observation_id {
            identifier(id, 128)?;
        }
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let registry = self.0.clone();
        let value=self.0.store.call(32*1024,move|db|{
   a.check(&registry)?;authority::cleanup(&db.db,&a)?;
   if let Some(value)=initial_unstarted::saved(&db.db,&a.enrollment.profile.digest,&c)? {
    if observation_id.as_ref().is_none_or(|id|id==&value.observation_id) {a.check(&registry)?;return Ok(Some(value));}
    return Ok(None);
   }
   let digest=canonical::digest(&serde_json::to_value(&c)?)?;
   let saved:Option<String>=db.db.query_row("SELECT command_digest FROM owner_shared_runtime_lifetimes WHERE child_id=?1 AND profile=?2",params![a.child,a.enrollment.profile.digest],|r|r.get(0)).optional()?;
   if saved.is_none(){a.check(&registry)?;return Ok(None)}
   if saved.as_deref()!=Some(&digest){return Err(Error::LocalAuthority)}
   let payload:Option<String>=db.db.query_row("SELECT payload FROM owner_shared_process_observations WHERE child_id=?1 AND (?2 IS NULL OR id=?2) ORDER BY ledger_revision DESC LIMIT 1",params![a.child,observation_id],|r|r.get(0)).optional()?;
   let value=payload.map(|p|serde_json::from_str::<SharedProcessReceipt>(&p)).transpose()?;
   if let Some(r)=&value{r.validate_for(&c)?;}a.check(&registry)?;Ok(value)
  }).await?;
        after(&p, &self.0, expires)?;
        Ok(value)
    }
}
