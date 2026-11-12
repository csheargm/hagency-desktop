use super::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedCallHistory {
    pub call_id: String,
    pub execution_id: String,
    pub state: String,
    pub reserved_tokens: u64,
    pub known_consumed_tokens: u64,
    pub usage_complete: bool,
    pub prepare_revision: u64,
    pub issue_revision: Option<u64>,
}
pub struct ManagedCallCustody {
    pub(super) scope: Scope,
}
pub enum ManagedCallClaim {
    Issued(ManagedCallCustody),
    History(ManagedCallHistory),
}
impl ManagedCallCustody {
    pub fn call_id(&self) -> &str {
        &self.scope.target
    }
}
impl Drop for ManagedCallCustody {
    fn drop(&mut self) {
        self.scope.unmark_live();
    }
}
fn identity(a: &ManagedCallAuthority, binding: &str) -> Result<(String, String), Error> {
    let digest = canonical::digest(
        &json!({"child":a.assignment.child,"assignmentDigest":binding,"execution":a.execution,"executionDigest":a.execution_digest,"estimatedTokens":a.estimated_tokens}),
    )?;
    let id = format!(
        "mac_{}",
        &canonical::digest(&json!({"child":a.assignment.child,"execution":a.execution}))?[..40]
    );
    Ok((id, digest))
}
pub(super) fn history(db: &Connection, child: &str, id: &str) -> Result<ManagedCallHistory, Error> {
    Ok(db.query_row("SELECT c.id,c.execution_id,c.state,c.reserved_tokens,c.known_consumed_tokens,c.usage_complete,p.prepare_revision,p.issue_revision FROM owner_managed_calls c JOIN owner_managed_call_pins p ON p.call_id=c.id WHERE c.id=?1 AND c.child_id=?2",params![id,child],|r|Ok(ManagedCallHistory {call_id:r.get(0)?,execution_id:r.get(1)?,state:r.get(2)?,reserved_tokens:r.get(3)?,known_consumed_tokens:r.get(4)?,usage_complete:r.get(5)?,prepare_revision:r.get(6)?,issue_revision:r.get(7)?}))?)
}
fn same(
    db: &Connection,
    a: &ManagedCallAuthority,
    id: &str,
    digest: &str,
    binding: &str,
) -> Result<bool, Error> {
    let old:Option<(String,String,String,u64)>=db.query_row("SELECT c.immutable_digest,p.assignment_digest,p.execution_digest,c.assignment_generation FROM owner_managed_calls c JOIN owner_managed_call_pins p ON p.call_id=c.id WHERE c.id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((d, b, e, g)) = old {
        if d != digest || b != binding || e != a.execution_digest || g != a.assignment.generation {
            return Err(Error::Conflict);
        }
        Ok(true)
    } else {
        Ok(false)
    }
}
fn live_call(db: &Connection, b: &Binding, child: &str) -> Result<(), Error> {
    let p = provision::history(db, child)?;
    if p.state != "qualified" {
        return Err(Error::LocalAuthority);
    }
    shared_execution::check_period_account(db,child,b)
}
impl OwnerResourceRegistry {
    pub async fn prepare_managed_call(
        &self,
        a: ManagedCallAuthority,
    ) -> Result<ManagedCallHistory, Error> {
        a.check(&self.0)?;
        let p = a.assignment.enrollment.profile.clone();
        let expires = a.assignment.deadline();
        let r = self.0.clone();
        let result=self.0.store.call(16*1024,move |db| {
            a.check(&r)?;let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (b,binding)=assignment(&tx,&a.assignment,false)?;
            let (id,digest)=identity(&a,&binding)?;
            if !same(&tx,&a,&id,&digest,&binding)? {
                live_call(&tx,&b,&a.assignment.child)?;
                let (known,invalid)=postpaid_call_budget(&tx,&a.assignment.child)?;
                if invalid || known>=b.allocated {return Err(Error::Capacity);}
                bounded_row(&tx,"owner_managed_calls","id",&id,100_000)?;
                tx.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES(?1,?2,?3,?4,?5,'prepared',?6,0,0)",params![id,a.assignment.child,a.execution,digest,a.assignment.generation,a.estimated_tokens])?;
                periods::pin_call(&tx,&a.assignment.child,&id)?;
                let revision=revision(&tx)?;
                tx.execute("INSERT INTO owner_managed_call_pins VALUES(?1,?2,?3,?4,NULL)",params![id,binding,a.execution_digest,revision])?;
            }
            let value=history(&tx,&a.assignment.child,&id)?;
            a.check(&r)?;tx.commit()?;a.check(&r).map_err(|_|Error::OutcomeUnknown)?;Ok(value)
        }).await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
    pub async fn issue_managed_call(
        &self,
        a: ManagedCallAuthority,
    ) -> Result<ManagedCallClaim, Error> {
        a.check(&self.0)?;
        let p = a.assignment.enrollment.profile.clone();
        let expires = a.assignment.deadline();
        let r = self.0.clone();
        let result=self.0.store.call(16*1024,move |db| {
            a.check(&r)?;let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let (b,binding)=assignment(&tx,&a.assignment,false)?;
            let (id,digest)=identity(&a,&binding)?;
            if !same(&tx,&a,&id,&digest,&binding)? {return Err(Error::LocalAuthority);}
            let h=history(&tx,&a.assignment.child,&id)?;
            if h.state!="prepared" {a.check(&r)?;return Ok(ManagedCallClaim::History(h));}
            live_call(&tx,&b,&a.assignment.child)?;
            periods::require_call_current(&tx,&a.assignment.child,&id)?;
            let (known,invalid)=postpaid_call_budget(&tx,&a.assignment.child)?;
            if invalid || known>=b.allocated {return Err(Error::Capacity);}
            let revision=revision(&tx)?;
            tx.execute("UPDATE owner_managed_calls SET state='issued' WHERE id=?1 AND state='prepared'",[&id])?;
            tx.execute("UPDATE owner_managed_call_pins SET issue_revision=?2 WHERE call_id=?1",params![id,revision])?;
            let custody=ManagedCallCustody {scope:Scope {registry:r.clone(),profile:a.assignment.enrollment.profile.clone(),child:a.assignment.child.clone(),target:id,binding,issued_revision:revision,expires}};
            custody.scope.mark_live();
            a.check(&r)?;tx.commit()?;a.check(&r).map_err(|_|Error::OutcomeUnknown)?;Ok(ManagedCallClaim::Issued(custody))
        }).await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
    pub async fn managed_call_history(
        &self,
        a: ManagedCleanupAuthority,
        call: String,
    ) -> Result<ManagedCallHistory, Error> {
        a.check(&self.0)?;
        identifier(&call, 128)?;
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let r = self.0.clone();
        let result = self
            .0
            .store
            .call(8192, move |db| {
                a.check(&r)?;
                cleanup(&db.db, &a)?;
                let value = history(&db.db, &a.child, &call)?;
                a.check(&r)?;
                Ok(value)
            })
            .await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
    pub async fn record_managed_usage(
        &self,
        c: &ManagedCallCustody,
        o: ManagedUsageObservation,
    ) -> Result<ManagedObservationReceipt, Error> {
        c.scope.check(&self.0)?;
        let s = c.scope.clone();
        let p = s.profile.clone();
        let expires = s.expires;
        let r = self.0.clone();
        let result = self
            .0
            .store
            .call(32 * 1024, move |db| {
                s.check(&r)?;
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                observations::check_scope(&tx, &s, "usage")?;
                let value = observations::usage(&tx, &s.child, &s.target, o)?;
                s.check(&r)?;
                tx.commit()?;
                s.check(&r).map_err(|_| Error::OutcomeUnknown)?;
                Ok(value)
            })
            .await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
    pub async fn record_late_managed_usage(
        &self,
        a: ManagedCleanupAuthority,
        call: String,
        o: ManagedUsageObservation,
    ) -> Result<ManagedObservationReceipt, Error> {
        a.check(&self.0)?;
        identifier(&call, 128)?;
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let r = self.0.clone();
        let result = self
            .0
            .store
            .call(32 * 1024, move |db| {
                a.check(&r)?;
                let tx = db
                    .db
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                cleanup(&tx, &a)?;
                let value = observations::usage(&tx, &a.child, &call, o)?;
                a.check(&r)?;
                tx.commit()?;
                a.check(&r).map_err(|_| Error::OutcomeUnknown)?;
                Ok(value)
            })
            .await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
}
