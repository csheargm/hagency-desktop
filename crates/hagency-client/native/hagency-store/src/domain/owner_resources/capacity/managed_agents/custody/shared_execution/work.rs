use super::*;

/// A finite current central lease on the original actual text process. This is
/// not constructible from a recovered result ticket or a serialized journal.
pub struct VerifiedSharedActivity {
    pub(super) sink: ProcessBinding,
    pub(super) enrollment: VerifiedManagementEnrollment,
    pub(super) epoch: u64,
    pub(super) expires: Instant,
}
impl SharedProcessSink {
    pub fn authorize_activity(
        self: &Arc<Self>,
        enrollment: VerifiedManagementEnrollment,
        lease: &SharedActivityLease,
        expires: Instant,
    ) -> Result<VerifiedSharedActivity, Error> {
        ProcessBinding::Original(self.clone()).authorize_activity(enrollment, lease, expires)
    }
}
impl SharedAttemptProcessSink {
    pub fn authorize_activity(
        self: &Arc<Self>,
        enrollment: VerifiedManagementEnrollment,
        lease: &SharedActivityLease,
        expires: Instant,
    ) -> Result<VerifiedSharedActivity, Error> {
        ProcessBinding::Attempt(self.clone()).authorize_activity(enrollment, lease, expires)
    }
    pub fn source(&self) -> &hagency_runtime::owned::SharedTextProcessSource {
        &self.source
    }
}
#[cfg(feature = "shared-budget-execution")]
impl SharedBudgetAttemptProcessSink {
    pub fn authorize_activity(
        self: &Arc<Self>,
        enrollment: VerifiedManagementEnrollment,
        lease: &SharedActivityLease,
        expires: Instant,
    ) -> Result<VerifiedSharedActivity, Error> {
        ProcessBinding::Budget(self.clone()).authorize_activity(enrollment, lease, expires)
    }
    pub fn source(&self) -> &hagency_runtime::owned::SharedTextProcessSource {
        &self.source
    }
}
impl ProcessBinding {
    pub fn authorize_activity(
        &self,
        mut enrollment: VerifiedManagementEnrollment,
        lease: &SharedActivityLease,
        expires: Instant,
    ) -> Result<VerifiedSharedActivity, Error> {
        enrollment.check()?;
        lease.authority.validate()?;
        let original = self.enrollment();
        let checked_at = Instant::now();
        let remaining = lease.expires_at_ms.saturating_sub(graphs::now_ms()?);
        if enrollment.profile.digest != original.profile.digest
            || enrollment.executor_device_id != original.executor_device_id
            || enrollment.executor_generation != original.executor_generation
            || enrollment.enrollment_id != original.enrollment_id
            || lease.authority.runtime_command_id != self.command_id()
            || lease.command_digest != self.command_digest()
            || lease.process_digest != self.process_digest()?
            || expires <= checked_at
            || remaining == 0
            || remaining > 30_000
            || expires > checked_at + Duration::from_secs(30)
            || self
                .source()
                .activity_deadline(lease.authority.epoch)
                .is_none_or(|d| expires > d)
        {
            return Err(Error::LocalAuthority);
        }
        // The authenticated transport already pins a send-anchored monotonic
        // deadline. A fresh millisecond wall-clock sample can be slightly shorter
        // because of rounding (or a forward clock correction). Restrict to both;
        // never reject a live source for sub-millisecond comparison jitter and
        // never extend the original deadline from a later wall-clock observation.
        let expires = expires.min(checked_at + Duration::from_millis(remaining));
        // Restrict (never replace) the existing SDK/profile epoch fence for queued
        // canonical commits. A later renewal uses a new proof; old ones expire.
        let prior = enrollment.profile.clone();
        let source = self.source().clone();
        let epoch = lease.authority.epoch;
        enrollment.profile.native_gate = Some(Arc::new(move || {
            prior.check().is_ok()
                && Instant::now() < expires
                && source
                    .activity_deadline(epoch)
                    .is_some_and(|d| Instant::now() < d)
        }));
        let value = VerifiedSharedActivity {
            sink: self.clone(),
            enrollment,
            epoch,
            expires,
        };
        value.check()?;
        Ok(value)
    }
}
impl VerifiedSharedActivity {
    pub(super) fn check(&self) -> Result<(), Error> {
        self.enrollment.check()?;
        if Instant::now() >= self.expires
            || self
                .sink
                .source()
                .activity_deadline(self.epoch)
                .is_none_or(|d| Instant::now() >= d)
            || self.sink.registry().closed.load(Ordering::Acquire)
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    pub(super) fn work(&self, w: &SharedWorkItem) -> Result<(), Error> {
        self.check()?;
        w.validate()?;
        let c = self.sink.original();
        // Any member of the assignment's Room may send work; the server
        // verified that sender's fresh membership and mention before issuing it.
        if w.assignment_id != c.assignment_id
            || w.runtime_command_digest != self.sink.command_digest()
            || (w.room_id != c.reserve_command.room_id
                && (c.reserve_command.pool_grant.is_none() || w.room_binding.is_none()))
            || (w.room_id == c.reserve_command.room_id && w.room_binding.is_some())
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    pub(super) fn assignment(self) -> ManagedAssignmentAuthority {
        ManagedAssignmentAuthority {
            enrollment: self.enrollment,
            child: self.sink.child().to_owned(),
            assignment: self.sink.original().assignment_id.clone(),
            generation: 1,
            expires: self.expires,
        }
    }
}
pub(super) fn live(db: &Connection, a: &VerifiedSharedActivity) -> Result<(), Error> {
    a.check()?;
    if let ProcessBinding::Attempt(sink) = &a.sink {
        return attempt_work::live(db, a, sink);
    }
    #[cfg(feature = "shared-budget-execution")]
    if let ProcessBinding::Budget(sink) = &a.sink {
        return budget_work::live(db, a, sink, true);
    }
    demand::require_no_owner_stop(db, &a.enrollment.profile.digest, a.sink.child())?;
    let b = authority::original(db, &a.enrollment, a.sink.child(), false)?;
    if b.provider != a.sink.provider() || a.sink.process_digest()?.len() != 64 {
        return Err(Error::LocalAuthority);
    }
    let qualified:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_process_observations WHERE child_id=?1 AND json_extract(payload,'$.state')='qualified') AND NOT EXISTS(SELECT 1 FROM owner_shared_process_observations WHERE child_id=?1 AND json_extract(payload,'$.state')='stopped')",[a.sink.child()],|r|r.get(0))?;
    if !qualified {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}
pub(super) fn exact_work(db: &Connection, child: &str, w: &SharedWorkItem) -> Result<bool, Error> {
    if periods::enabled(db,child)? {
        let period=periods::current(db,child,graphs::now_ms()?)?.ok_or(Error::LocalAuthority)?;
        if w.created_at_ms<period.period.start_ms || w.created_at_ms>=period.period.end_ms {return Err(Error::LocalAuthority);}
    }
    demand::check_work_control(db, child, w)?;
    let digest = canonical::digest(&serde_json::to_value(w)?)?;
    let saved: Option<(String, String)> = db
        .query_row(
            "SELECT child_id,digest FROM owner_shared_work_inbox WHERE id=?1",
            [&w.id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((c, d)) = saved {
        if c != child || d != digest {
            return Err(Error::Conflict);
        }
        Ok(true)
    } else {
        Ok(false)
    }
}
impl OwnerResourceRegistry {
    /// V1 bootstrap only qualifies the provider. Owner tool or network policy
    /// requires a versioned successor before the first model call can run.
    pub async fn shared_bootstrap_requires_successor(&self, a: VerifiedSharedActivity) -> Result<bool, Error> {
        a.check()?;
        let registry = self.0.clone();
        if !Arc::ptr_eq(&registry, a.sink.registry()) || a.sink.is_successor() {
            return Err(Error::LocalAuthority);
        }
        self.0.store.call(4096, move |db| {
            live(&db.db, &a)?;
            let original = a.sink.original();
            let required = owner_tool_policy(&db.db, original)?.is_some()
                || owner_live_web(&db.db, original);
            a.check()?;
            Ok(required)
        }).await
    }
    /// Persist the exact private event before acknowledging it centrally. This
    /// changes no capacity and cannot by itself authorize a model request.
    pub async fn receive_shared_work(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
    ) -> Result<(), Error> {
        a.work(&w)?;
        let profile = a.enrollment.profile.clone();
        let expires = a.expires;
        let registry = self.0.clone();
        if !Arc::ptr_eq(&registry, a.sink.registry()) {
            return Err(Error::LocalAuthority);
        }
        self.0
            .store
            .call(32 * 1024, move |db| {
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                live(&tx, &a)?;
                if !exact_work(&tx, a.sink.child(), &w)? {
                    bounded_row(&tx, "owner_shared_work_inbox", "id", &w.id, 100_000)?;
                    let revision = revision(&tx)?;
                    tx.execute(
                        "INSERT INTO owner_shared_work_inbox VALUES(?1,?2,?3,?4,?5,?6)",
                        params![
                            w.id,
                            a.sink.child(),
                            w.execution_id,
                            canonical::encode(&serde_json::to_value(&w)?)?,
                            canonical::digest(&serde_json::to_value(&w)?)?,
                            revision
                        ],
                    )?;
                }
                a.check()?;
                tx.commit()?;
                a.check().map_err(|_| Error::OutcomeUnknown)
            })
            .await?;
        after(&profile, &self.0, expires)
    }
    /// The first text turn holds all currently available child headroom in the
    /// existing call ledger. No parallel budget or provider hard-cap is invented.
    pub async fn prepare_shared_call(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
    ) -> Result<ManagedCallHistory, Error> {
        self.prepare_shared_call_input(a,w,None).await
    }
    pub async fn prepare_shared_input_call(&self,a:VerifiedSharedActivity,w:SharedWorkItem,input:SharedPreparedInput)->Result<ManagedCallHistory,Error>{
        self.prepare_shared_call_input(a,w,Some(input)).await
    }
    async fn prepare_shared_call_input(&self,a:VerifiedSharedActivity,w:SharedWorkItem,input:Option<SharedPreparedInput>)->Result<ManagedCallHistory,Error>{
        a.work(&w)?;
        if a.sink.is_successor() {
            return Box::pin(attempt_work::prepare(self, a, w, input)).await;
        }
        let profile = a.enrollment.profile.clone();
        let expires = a.expires;
        let registry = self.0.clone();
        if !Arc::ptr_eq(&registry, a.sink.registry()) {
            return Err(Error::LocalAuthority);
        }
        let authority = self
            .0
            .store
            .call(context::queue_weight(input.as_ref()), move |db| {
                live(&db.db, &a)?;
                context::require_input(&db.db,&w,input.as_ref())?;
                if !exact_work(&db.db, a.sink.child(), &w)? {
                    return Err(Error::LocalAuthority);
                }
                let b =
                    authority::original(&db.db, &a.enrollment, a.sink.child(), false)?;
                let prior:Option<u64>=db.db.query_row("SELECT reserved_tokens FROM owner_managed_calls WHERE child_id=?1 AND execution_id=?2",params![a.sink.child(),w.execution_id],|r|r.get(0)).optional()?;
                let estimated_tokens=if let Some(n)=prior{n}else{let (known,invalid)=postpaid_call_budget(&db.db,a.sink.child())?;if invalid||known>=b.allocated{return Err(Error::Capacity)}0};
                let execution_digest=canonical::digest(&serde_json::to_value(&w)?)?;
                a.check()?;
                Ok(ManagedCallAuthority {assignment:a.assignment(),execution:w.execution_id,execution_digest,estimated_tokens})
            })
            .await?;
        after(&profile, &self.0, expires)?;
        self.prepare_managed_call(authority).await
    }
}

/// Only the original pre-send writer response owns this attempt. The durable
/// start_requested row is an uncertainty fence, never a retry permission.
pub struct SharedStartAttempt {
    pub(super) activity: VerifiedSharedActivity,
    pub(super) work: SharedWorkItem,
    pub(super) call: ManagedCallHistory,
    pub(super) input: Option<SharedPreparedInput>,
}
pub struct SharedCallCustody {
    pub(super) sink: ProcessBinding,
    pub(super) work: SharedWorkItem,
    pub(super) call: ManagedCallCustody,
    pub(super) input: Option<SharedPreparedInput>,
}
impl SharedCallCustody {
    pub fn model_input(&self)->&str { self.input.as_ref().map_or(&self.work.body,SharedPreparedInput::text) }
    pub fn context_input_digest(&self)->Option<String> { self.input.as_ref().filter(|i|self.work.encrypted || i.context().is_some()).map(SharedPreparedInput::input_digest) }
    /// Send only this issued call's exact pinned input to its original actual
    /// process. A caller never supplies replacement text at the effect boundary.
    pub async fn start_turn(&self,process:&mut hagency_runtime::owned::OwnedSession)->Result<(),Error>{
        self.check()?;
        if !self.sink.source().same(&process.shared_text_process_source()){return Err(Error::LocalAuthority)}
        if let Some(digest)=self.context_input_digest(){
            process.start_shared_context_turn(self.model_input().into(),&digest).await
        }else{
            process.start_shared_text_turn(self.model_input().into()).await
        }.map_err(|_|Error::OutcomeUnknown)?;
        self.check_capture(process).map_err(|_|Error::OutcomeUnknown)
    }
    /// Recording the original process is distinct from admitting another call.
    /// Its renewable live lease may outlast the proof used before turn/start.
    pub(super) fn check_capture(&self, process: &hagency_runtime::owned::OwnedSession) -> Result<(), Error> {
        // The old profile proof deliberately pins its old activity epoch. It
        // cannot authorize another dispatch after renewal, but must not reject
        // observations from this already-issued call and its same live source.
        if self.call.scope.registry.closed.load(Ordering::Acquire)
            || !self.sink.source().same(&process.shared_text_process_source())
            || !self.sink.source().has_live_activity()
        { return Err(Error::LocalAuthority); }
        Ok(())
    }
    pub fn work(&self) -> &SharedWorkItem {
        &self.work
    }
    pub fn call_id(&self) -> &str {
        self.call.call_id()
    }
    pub fn process_digest(&self) -> Result<String, Error> {
        self.sink.process_digest()
    }
    pub fn check(&self) -> Result<(), Error> {
        self.call.scope.check(&self.call.scope.registry)
    }
}
pub(super) fn event(
    db: &Connection,
    w: &SharedWorkItem,
    kind: &str,
    payload: serde_json::Value,
) -> Result<(), Error> {
    let id = format!(
        "swe_{}",
        canonical::digest(&json!({"workId":w.id,"kind":kind}))?
    );
    let digest = canonical::digest(&payload)?;
    let prior: Option<String> = db
        .query_row(
            "SELECT digest FROM owner_shared_work_events WHERE id=?1",
            [&id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(d) = prior {
        if d != digest {
            return Err(Error::Conflict);
        }
        return Ok(());
    }
    let rev = revision(db)?;
    db.execute(
        "INSERT INTO owner_shared_work_events VALUES(?1,?2,?3,?4,?5,?6)",
        params![id, w.id, kind, canonical::encode(&payload)?, digest, rev],
    )?;
    Ok(())
}
impl OwnerResourceRegistry {
    pub async fn begin_shared_start(
        &self,
        a: VerifiedSharedActivity,
        w: SharedWorkItem,
        call_id: String,
    ) -> Result<SharedStartAttempt, Error> {
        self.begin_shared_start_input(a,w,call_id,None).await
    }
    pub async fn begin_shared_input_start(&self,a:VerifiedSharedActivity,w:SharedWorkItem,call_id:String,input:SharedPreparedInput)->Result<SharedStartAttempt,Error>{
        self.begin_shared_start_input(a,w,call_id,Some(input)).await
    }
    async fn begin_shared_start_input(&self,a:VerifiedSharedActivity,w:SharedWorkItem,call_id:String,input:Option<SharedPreparedInput>)->Result<SharedStartAttempt,Error>{
        a.work(&w)?;
        identifier(&call_id, 128)?;
        if !Arc::ptr_eq(&self.0, a.sink.registry()) {
            return Err(Error::LocalAuthority);
        }
        let p = a.enrollment.profile.clone();
        let expires = a.expires;
        let result=self.0.store.call(context::queue_weight(input.as_ref()),move|db|{
   let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;live(&tx,&a)?;if !exact_work(&tx,a.sink.child(),&w)?{return Err(Error::LocalAuthority)}
   if tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_work_events WHERE work_id=?1 AND kind='start_requested')",[&w.id],|r|r.get::<_,bool>(0))?{return Err(Error::OutcomeUnknown)}
   let call=calls::history(&tx,a.sink.child(),&call_id)?;
   let execution_digest:String=tx.query_row("SELECT execution_digest FROM owner_managed_call_pins WHERE call_id=?1",[&call_id],|r|r.get(0))?;
   if call.state!="prepared"||call.execution_id!=w.execution_id||execution_digest!=canonical::digest(&serde_json::to_value(&w)?)?{return Err(Error::LocalAuthority)}
   context::pin_call(&tx,&w,&call_id,input.as_ref())?;
   event(&tx,&w,"start_requested",json!({"callId":call_id,"activityEpoch":a.epoch,"workDigest":execution_digest}))?;
   a.check()?;tx.commit()?;a.check().map_err(|_|Error::OutcomeUnknown)?;Ok(SharedStartAttempt{activity:a,work:w,call,input})
  }).await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
    pub async fn issue_shared_call(
        &self,
        attempt: SharedStartAttempt,
        start: SharedWorkStart,
    ) -> Result<SharedCallCustody, Error> {
        if attempt.input.as_ref().is_some_and(|i|i.context().is_some()){return Err(Error::LocalAuthority)}
        self.issue_shared_input_inner(attempt,start).await
    }
    pub async fn issue_shared_context_call(&self,attempt:SharedStartAttempt,start:SharedContextStart)->Result<SharedCallCustody,Error>{
        let c=attempt.input.as_ref().and_then(|i|i.context()).ok_or(Error::LocalAuthority)?;
        if !cfg!(feature="shared-context") || c.context_digest!=start.context_digest || c.input_digest!=start.input_digest {return Err(Error::LocalAuthority)}
        self.issue_shared_input_inner(attempt,start.start).await
    }
    async fn issue_shared_input_inner(&self,attempt:SharedStartAttempt,start:SharedWorkStart)->Result<SharedCallCustody,Error>{
        if attempt.activity.sink.is_successor() {
            return Box::pin(attempt_work::issue(self, attempt, start)).await;
        }
        let a = attempt.activity;
        let w = attempt.work;
        a.work(&w)?;
        if !start.newly_started
            || start.activity_epoch != a.epoch
            || start.work_digest != canonical::digest(&serde_json::to_value(&w)?)?
            || canonical::digest(&serde_json::to_value(&start.work)?)? != start.work_digest
        {
            return Err(Error::OutcomeUnknown);
        }
        if !Arc::ptr_eq(&self.0, a.sink.registry()) {
            return Err(Error::LocalAuthority);
        }
        let p = a.enrollment.profile.clone();
        let expires = a.expires;
        let sink = a.sink.clone();
        let work = w.clone();
        let input=attempt.input.clone();
        let authority = self
            .0
            .store
            .call(context::queue_weight(attempt.input.as_ref()), move |db| {
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                live(&tx, &a)?;
                if !exact_work(&tx, a.sink.child(), &w)? {
                    return Err(Error::LocalAuthority);
                }
                context::require_call(&tx,&w,&attempt.call.call_id,attempt.input.as_ref())?;
                let h = calls::history(&tx, a.sink.child(), &attempt.call.call_id)?;
                if h.state != "prepared" || h.prepare_revision != attempt.call.prepare_revision {
                    return Err(Error::OutcomeUnknown);
                }
                event(&tx, &w, "start_confirmed", serde_json::to_value(&start)?)?;
                let execution_digest = canonical::digest(&serde_json::to_value(&w)?)?;
                a.check()?;
                tx.commit()?;
                a.check().map_err(|_| Error::OutcomeUnknown)?;
                Ok(ManagedCallAuthority {
                    assignment: a.assignment(),
                    execution: w.execution_id,
                    execution_digest,
                    estimated_tokens: h.reserved_tokens,
                })
            })
            .await?;
        after(&p, &self.0, expires)?;
        match self.issue_managed_call(authority).await? {
            ManagedCallClaim::Issued(call) => Ok(SharedCallCustody { sink, work, call, input }),
            ManagedCallClaim::History(_) => Err(Error::OutcomeUnknown),
        }
    }
}

impl OwnerResourceRegistry {
    /// Durable renewal intent before the network request. Its ID can retry only
    /// the exact prior epoch; it never extends the local process by itself.
    pub async fn prepare_shared_renewal(
        &self,
        a: VerifiedSharedActivity,
        id: String,
    ) -> Result<(), Error> {
        identifier(&id, 128)?;
        a.check()?;
        if !Arc::ptr_eq(&self.0, a.sink.registry()) {
            return Err(Error::LocalAuthority);
        }
        let p = a.enrollment.profile.clone();
        let expires = a.expires;
        self.0.store.call(4096,move|db|{
            let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;renewal_live(&tx,&a)?;
            let old:Option<(String,u64)>=tx.query_row("SELECT child_id,requested_epoch FROM owner_shared_activity_renewals WHERE id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((child,epoch))=old {
                if child!=a.sink.child()||epoch!=a.epoch{return Err(Error::Conflict)}
            }else {
                bounded_row(&tx,"owner_shared_activity_renewals","id",&id,1_000_000)?;
                let rev=revision(&tx)?;tx.execute("INSERT INTO owner_shared_activity_renewals VALUES(?1,?2,?3,?4)",params![id,a.sink.child(),a.epoch,rev])?;
            }
            a.check()?;tx.commit()?;a.check().map_err(|_|Error::OutcomeUnknown)
        }).await?;
        after(&p, &self.0, expires)
    }
}

// A later budget head prevents fresh work, but cannot rewrite or interrupt the
// evidence collection lease of an already issued call on the prior exact head.
fn renewal_live(db: &Connection, a: &VerifiedSharedActivity) -> Result<(), Error> {
    #[cfg(feature = "shared-budget-execution")]
    if let ProcessBinding::Budget(sink) = &a.sink {
        return budget_work::live(db, a, sink, false);
    }
    live(db, a)
}
