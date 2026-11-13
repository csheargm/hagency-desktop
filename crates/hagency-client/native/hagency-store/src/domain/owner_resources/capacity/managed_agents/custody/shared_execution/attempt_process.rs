//! Actual-process observations for an immutable successor. Old v1 provision
//! evidence is never reset, and recovered history cannot construct this sink.
use super::*;
use hagency_runtime::owned::{
    ManagedProcessState, SharedTextProcessObservation, SharedTextProcessSource,
};

pub struct SharedAttemptProcessSink {
    pub(super) custody: Arc<SharedAttemptCustody>,
    pub(super) source: SharedTextProcessSource,
}

fn bound(db: &Connection, c: &SharedAttemptCustody) -> Result<(), Error> {
    let row: (String,String,String,String,u64) = db.query_row(
        "SELECT profile,command_digest,incarnation,provider_digest,issued_revision FROM owner_shared_successor_attempts WHERE id=?1 AND child_id=?2",
        params![c.command.command_id,c.command.agent_reservation_id],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    )?;
    if row
        != (
            c.enrollment.profile.digest.clone(),
            c.digest.clone(),
            c.incarnation.clone(),
            c.provider.clone(),
            c.issued_revision,
        )
    {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}

impl SharedAttemptCustody {
    /// Bind the actual retained process once. Qualification is still checked
    /// inside the canonical writer; this sink alone grants no model-call right.
    /// If cancellation won after spawn, the sink can retain actual stop evidence
    /// while qualification fails closed against the permanent fence.
    pub fn bind_process(
        self,
        source: SharedTextProcessSource,
    ) -> Result<SharedAttemptProcessSink, Error> {
        source
            .claim_managed_custody(&self.digest, &self.incarnation)
            .map_err(|_| Error::LocalAuthority)?;
        Ok(SharedAttemptProcessSink {
            custody: Arc::new(self),
            source,
        })
    }
}
impl SharedAttemptProcessSink {
    pub fn command(&self) -> &SharedAttemptCommand {
        &self.custody.command
    }
    pub fn process_digest(&self) -> Result<String, Error> {
        canonical::digest(&json!({"commandDigest":self.custody.digest,"incarnation":self.custody.incarnation,"providerDigest":self.custody.provider})).map_err(Into::into)
    }
    pub async fn record(
        &self,
        observation: SharedTextProcessObservation,
    ) -> Result<SharedAttemptProcessReceipt, Error> {
        if !self.source.same(observation.source())
            || !self
                .source
                .has_managed_custody(&self.custody.digest, &self.custody.incarnation)
        {
            return Err(Error::LocalAuthority);
        }
        let qualified = observation.state() == ManagedProcessState::Qualified;
        if qualified {
            self.custody.check()?;
            if !self.source.is_live()
                || !provider_accepted(
                    self.custody.command.original_runtime_command.runtime()?,
                    &self.custody.provider,
                    observation.provider_digest(),
                )
                || observation.policy_digest()
                    != Some(canonical::digest(&json!({"policy":self.custody.command.original_runtime_command.execution_policy(self.custody.command.web_search,self.custody.command.tool_policy.as_ref().map(|p|p.mode.as_str()))?}))?.as_str())
            {
                return Err(Error::LocalAuthority);
            }
        }
        let state = match observation.state() {
            ManagedProcessState::Qualified => "qualified",
            ManagedProcessState::Stopped => "stopped",
            ManagedProcessState::Unknown => "unknown",
        }
        .to_owned();
        let stopping = state == "stopped";
        let policy = observation.policy_digest().map(str::to_owned);
        let process = self.process_digest()?;
        let id = format!(
            "sapo_{}",
            canonical::digest(
                &json!({"processDigest":process,"state":state,"policyDigest":policy})
            )?
        );
        let custody = self.custody.clone();
        let source = self.source.clone();
        let registry = custody.registry.clone();
        let value=registry.store.call(32*1024,move|db|{
            if qualified && custody.registry.closed.load(Ordering::Acquire) {return Err(Error::LocalAuthority)}
            let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            bound(&tx,&custody)?;
            if qualified {
                custody.check()?;attempt_fence::require_unfenced(&tx,&custody.command)?;
                demand::check_attempt_control(&tx,&custody.enrollment.profile.digest,&custody.command)?;
                let binding=authority::original(&tx,&custody.enrollment,&custody.command.agent_reservation_id,false)?;
                if binding.provider!=custody.provider || !source.is_live() {return Err(Error::LocalAuthority)}
            }
            if let Some(payload)=tx.query_row("SELECT payload FROM owner_shared_successor_observations WHERE id=?1",[&id],|r|r.get::<_,String>(0)).optional()? {
                if qualified {custody.check()?;if !source.is_live(){return Err(Error::LocalAuthority)}}
                return Ok(serde_json::from_str(&payload)?);
            }
            let stopped:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_successor_observations WHERE attempt_id=?1 AND json_extract(payload,'$.state')='stopped')",[&custody.command.command_id],|r|r.get(0))?;
            if stopped && state!="stopped" {return Err(Error::Conflict)}
            let revision=revision(&tx)?;
            let receipt=SharedAttemptProcessReceipt{
                version:2,observation_id:id.clone(),command_id:custody.command.command_id.clone(),command_digest:custody.digest.clone(),assignment_id:custody.command.assignment_id.clone(),agent_reservation_id:custody.command.agent_reservation_id.clone(),process_attempt:custody.command.process_attempt,original_runtime_digest:custody.command.original_runtime_digest.clone(),ledger_revision:revision,process_digest:Some(process),state,policy_digest:policy,reason:None,
            };
            receipt.validate_for(&custody.command)?;
            bounded_row(&tx,"owner_shared_successor_observations","id",&id,100_000)?;
            tx.execute("INSERT INTO owner_shared_successor_observations VALUES(?1,?2,?3,?4,?5)",params![id,custody.command.command_id,canonical::encode(&serde_json::to_value(&receipt)?)?,canonical::digest(&serde_json::to_value(&receipt)?)?,revision])?;
            if stopping {observations::close_stopped_calls(&tx,&custody.command.agent_reservation_id)?;}
            if qualified {custody.check()?;if !source.is_live(){return Err(Error::LocalAuthority)}}
            tx.commit()?;
            if qualified {custody.check().map_err(|_|Error::OutcomeUnknown)?;if !source.is_live(){return Err(Error::OutcomeUnknown)}}
            Ok(receipt)
        }).await?;
        if qualified {
            self.custody.check().map_err(|_| Error::OutcomeUnknown)?;
            if !self.source.is_live() {
                return Err(Error::OutcomeUnknown);
            }
        }
        Ok(value)
    }
}

impl OwnerResourceRegistry {
    pub async fn recover_shared_attempt_process(
        &self,
        authority: ManagedCleanupAuthority,
        command: SharedAttemptCommand,
        observation_id: Option<String>,
    ) -> Result<Option<SharedAttemptProcessReceipt>, Error> {
        authority.check(&self.0)?;
        command.validate(&authority.enrollment.profile.account)?;
        if authority.child != command.agent_reservation_id {
            return Err(Error::LocalAuthority);
        }
        if let Some(id) = &observation_id {
            identifier(id, 128)?;
        }
        let profile = authority.enrollment.profile.clone();
        let expires = authority.deadline();
        let registry = self.0.clone();
        let result=self.0.store.call(64*1024,move|db|{
            authority.check(&registry)?;authority::cleanup(&db.db,&authority)?;
            let digest=canonical::digest(&serde_json::to_value(&command)?)?;
            let saved:Option<(String,String)>=db.db.query_row("SELECT profile,command_digest FROM owner_shared_successor_attempts WHERE id=?1 AND child_id=?2",params![command.command_id,command.agent_reservation_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            // Never claimed: only an unclaimed permanent fence is evidence, of non-issuance.
            if saved.is_none(){let value=attempt_unstarted::load(&db.db,&authority.enrollment.profile.digest,&command)?;if observation_id.as_ref().is_some_and(|id|value.as_ref().is_none_or(|r|&r.observation_id!=id)){return Err(Error::Conflict)}authority.check(&registry)?;return Ok(value)}
            if saved!=Some((authority.enrollment.profile.digest.clone(),digest)){return Err(Error::Conflict)}
            let payload:Option<String>=db.db.query_row("SELECT payload FROM owner_shared_successor_observations WHERE attempt_id=?1 AND (?2 IS NULL OR id=?2) ORDER BY ledger_revision DESC LIMIT 1",params![command.command_id,observation_id],|r|r.get(0)).optional()?;
            let value=payload.map(|s|serde_json::from_str::<SharedAttemptProcessReceipt>(&s)).transpose()?;
            if let Some(value)=&value{value.validate_for(&command)?;}
            authority.check(&registry)?;Ok(value)
        }).await?;
        after(&profile, &self.0, expires)?;
        Ok(result)
    }
}
