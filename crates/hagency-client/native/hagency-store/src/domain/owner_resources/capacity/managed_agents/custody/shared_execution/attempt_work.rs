//! Successor calls use the original call ledger. Current actual process and
//! account conservation are rechecked in each writer; old incomplete calls
//! count at their known usage and cannot be turned into a refund or replay
//! permission.
use super::*;

pub(super) fn live(
    db: &Connection,
    a: &VerifiedSharedActivity,
    sink: &SharedAttemptProcessSink,
) -> Result<(), Error> {
    a.check()?;
    let c = &sink.custody;
    attempt_fence::require_unfenced(db, &c.command)?;
    demand::check_attempt_control(db, &a.enrollment.profile.digest, &c.command)?;
    let original = authority::original(db, &a.enrollment, &c.command.agent_reservation_id, false)?;
    let now = graphs::now_ms()?;
    if original.provider != c.provider
        || original.digest != c.command.original_runtime_command.reserve_command_digest
        || now >= authority::lifetime(db, &original, now)?
    {
        return Err(Error::LocalAuthority);
    }
    let bound: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_successor_attempts WHERE id=?1 AND child_id=?2 AND profile=?3 AND command_digest=?4 AND incarnation=?5 AND provider_digest=?6) AND NOT EXISTS(SELECT 1 FROM owner_shared_successor_attempts WHERE child_id=?2 AND process_attempt>?7)",params![c.command.command_id,c.command.agent_reservation_id,a.enrollment.profile.digest,c.digest,c.incarnation,c.provider,c.command.process_attempt],|r|r.get(0))?;
    let qualified:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_successor_observations WHERE attempt_id=?1 AND json_extract(payload,'$.state')='qualified') AND NOT EXISTS(SELECT 1 FROM owner_shared_successor_observations WHERE attempt_id=?1 AND json_extract(payload,'$.state')='stopped')",[&c.command.command_id],|r|r.get(0))?;
    let parent_closed:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM owner_parent_settlements s JOIN owner_project_reservations p ON p.grant_id=s.grant_id WHERE p.id=?1)",[&original.command.allocation_id],|r|r.get(0))?;
    if !bound || !qualified || parent_closed {
        return Err(Error::LocalAuthority);
    }
    // This is also the running call's renewal gate. Quota exhaustion may
    // reject fresh admission, never the existing process's authorization.
    Ok(())
}

pub(super) fn account_budget(db: &Connection, child: &str, binding: &Binding) -> Result<(), Error> {
    let resource = read_resource(db, &binding.command.resource_id)?;
    // Unsupported accounting anywhere in this shared Seat cannot be inferred
    // empty. Bounded sibling calls remain inside their original parent holds.
    let unsupported:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM owner_capacity_grants WHERE (preset_id=?1 OR seat_id=?2) AND accounting_mode NOT IN('reservation_only_v1','managed_agent_v1')) OR EXISTS(SELECT 1 FROM engagements WHERE preset_id=?1 OR seat_id=?2)",params![resource.preset_id,resource.seat_id],|r|r.get(0))?;
    if unsupported {
        return Err(Error::Capacity);
    }
    let mut q=db.prepare("SELECT a.id,a.original_command_digest,a.original_command,p.assignment_id,p.assignment_generation,p.binding_digest,p.provider_digest FROM owner_managed_agents a LEFT JOIN owner_project_reservations r ON r.id=a.project_reservation_id JOIN owner_capacity_grants g ON g.id=COALESCE(a.pool_grant_id,r.grant_id) LEFT JOIN owner_managed_assignment_pins p ON p.child_id=a.id WHERE (g.preset_id=?1 OR g.seat_id=?2) AND a.id<>?3 ORDER BY a.id LIMIT 100001")?;
    let mut rows = q.query(params![resource.preset_id, resource.seat_id, child])?;
    let mut count = 0;
    while let Some(row) = rows.next()? {
        count += 1;
        if count > 100_000 {
            return Err(Error::Capacity);
        }
        let id: String = row.get(0)?;
        let command_digest: String = row.get(1)?;
        let command: String = row.get(2)?;
        let command: ManagedAgentReserveCommand = serde_json::from_str(&command)?;
        if canonical::digest(&serde_json::to_value(&command)?)? != command_digest {
            return Err(Error::Capacity);
        }
        let assignment: Option<String> = row.get(3)?;
        let generation: Option<u64> = row.get(4)?;
        let saved: Option<String> = row.get(5)?;
        let provider: Option<String> = row.get(6)?;
        let has_calls: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_managed_calls WHERE child_id=?1)",
            [&id],
            |r| r.get(0),
        )?;
        if has_calls {
            let (Some(assignment), Some(saved), Some(provider)) = (assignment, saved, provider)
            else {
                return Err(Error::Capacity);
            };
            let expected = canonical::digest(
                &json!({"child":id,"assignmentId":assignment,"assignmentGeneration":1,"commandDigest":command_digest,"providerDigest":provider}),
            )?;
            if generation != Some(1)
                || saved != expected
                || successor::calls_all(db, &id, &saved)?.unrepresentable
            {
                return Err(Error::Capacity);
            }
        }
    }
    // This projection counts each parent once and includes all derived overrun
    // floors. We spend inside the already-held child, so zero external headroom
    // is valid; an overdrawn pool/Seat or mismatched period is not.
    let budget = crate::domain::budget(db, &resource, None, false)?;
    if budget
        .pool
        .ceiling
        .is_none_or(|n| budget.pool.committed > n)
        || budget.seat.quota.is_some_and(|n| budget.seat.committed > n)
        || budget.seat.status == allocation::SeatStatus::PeriodMismatch
        || (!periods::recurring(&binding.command) && budget.pool.period.as_deref() != Some("monthly"))
    {
        return Err(Error::Capacity);
    }
    Ok(())
}

fn budget(
    db: &Connection,
    a: &VerifiedSharedActivity,
) -> Result<(u64, String, successor::Calls), Error> {
    let b = authority::original(db, &a.enrollment, a.sink.child(), false)?;
    account_budget(db, a.sink.child(), &b)?;
    let binding = canonical::digest(
        &json!({"child":a.sink.child(),"assignmentId":a.sink.original().assignment_id,"assignmentGeneration":1,"commandDigest":b.digest,"providerDigest":b.provider}),
    )?;
    let pin:Option<(String,u64,String,String)>=db.query_row("SELECT assignment_id,assignment_generation,binding_digest,provider_digest FROM owner_managed_assignment_pins WHERE child_id=?1",[a.sink.child()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if pin
        != Some((
            a.sink.original().assignment_id.clone(),
            1,
            binding.clone(),
            b.provider.clone(),
        ))
    {
        return Err(Error::LocalAuthority);
    }
    // V3 resolves the exact applied native receipt inside this writer. It keeps
    // the original assignment pin and every old liability; v1/v2 stay bounded by
    // their original reservation even when a later top-up exists.
    let ceiling = match &a.sink {
        #[cfg(feature = "shared-budget-execution")]
        ProcessBinding::Budget(sink) => super::super::budget::execution::resolve(
            db, a.sink.child(), Some(&sink.command().execution_budget), true,
        )?.0.effective_ceiling_tokens,
        _ => b.allocated,
    };
    let calls = successor::calls(db, a.sink.child(), &binding)?;
    if calls.unrepresentable || calls.retained >= ceiling {
        return Err(Error::Capacity);
    }
    Ok((ceiling, binding, calls))
}

pub(super) async fn prepare(
    registry: &OwnerResourceRegistry,
    a: VerifiedSharedActivity,
    w: SharedWorkItem,
    input:Option<SharedPreparedInput>,
) -> Result<ManagedCallHistory, Error> {
    a.work(&w)?;
    if !Arc::ptr_eq(&registry.0, a.sink.registry()) {
        return Err(Error::LocalAuthority);
    }
    let profile = a.enrollment.profile.clone();
    let expires = a.expires;
    let result=registry.0.store.call(context::queue_weight(input.as_ref()),move|db|{
        let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        work::live(&tx,&a)?;
        context::require_input(&tx,&w,input.as_ref())?;
        if !work::exact_work(&tx,a.sink.child(),&w)? {return Err(Error::LocalAuthority)}
        let (_,binding,_)=budget(&tx,&a)?;
        let prior:Option<u64>=tx.query_row("SELECT reserved_tokens FROM owner_managed_calls WHERE child_id=?1 AND execution_id=?2",params![a.sink.child(),w.execution_id],|r|r.get(0)).optional()?;
        let tokens=prior.unwrap_or(0);
        let input=canonical::digest(&serde_json::to_value(&w)?)?;
        let digest=canonical::digest(&json!({"child":a.sink.child(),"assignmentDigest":binding,"execution":w.execution_id,"executionDigest":input,"estimatedTokens":tokens}))?;
        let id=format!("mac_{}",&canonical::digest(&json!({"child":a.sink.child(),"execution":w.execution_id}))?[..40]);
        let prior:Option<(String,String,String,u64)>=tx.query_row("SELECT c.immutable_digest,p.assignment_digest,p.execution_digest,c.assignment_generation FROM owner_managed_calls c JOIN owner_managed_call_pins p ON p.call_id=c.id WHERE c.id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if prior.as_ref().is_some_and(|p|p!=&(digest.clone(),binding.clone(),input.clone(),1)){return Err(Error::Conflict)}
        if prior.is_none() {
            bounded_row(&tx,"owner_managed_calls","id",&id,100_000)?;
            tx.execute("INSERT INTO owner_managed_calls(id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete) VALUES(?1,?2,?3,?4,1,'prepared',?5,0,0)",params![id,a.sink.child(),w.execution_id,digest,tokens])?;
            periods::pin_call(&tx,a.sink.child(),&id)?;
            let rev=revision(&tx)?;
            tx.execute("INSERT INTO owner_managed_call_pins VALUES(?1,?2,?3,?4,NULL)",params![id,binding,input,rev])?;
            tx.execute("INSERT INTO owner_shared_successor_call_pins VALUES(?1,?2,?3,?4)",params![id,a.sink.command_id(),w.id,input])?;
        }
        exact_pin(&tx,&a,&w,&id)?;
        let value=calls::history(&tx,a.sink.child(),&id)?;
        a.check()?;tx.commit()?;a.check().map_err(|_|Error::OutcomeUnknown)?;Ok(value)
    }).await?;
    after(&profile, &registry.0, expires)?;
    Ok(result)
}
fn exact_pin(
    db: &Connection,
    a: &VerifiedSharedActivity,
    w: &SharedWorkItem,
    id: &str,
) -> Result<(), Error> {
    let pin:Option<(String,String,String)>=db.query_row("SELECT attempt_id,work_id,execution_digest FROM owner_shared_successor_call_pins WHERE call_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if pin
        != Some((
            a.sink.command_id().into(),
            w.id.clone(),
            canonical::digest(&serde_json::to_value(w)?)?,
        ))
    {
        return Err(Error::Conflict);
    }
    Ok(())
}
pub(super) async fn issue(
    registry: &OwnerResourceRegistry,
    attempt: SharedStartAttempt,
    start: SharedWorkStart,
) -> Result<SharedCallCustody, Error> {
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
    if !Arc::ptr_eq(&registry.0, a.sink.registry()) {
        return Err(Error::LocalAuthority);
    }
    let profile = a.enrollment.profile.clone();
    let expires = a.expires;
    let r = registry.0.clone();
    let value = registry
        .0
        .store
        .call(context::queue_weight(attempt.input.as_ref()), move |db| {
            let tx = db
                .db
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            work::live(&tx, &a)?;
            if !work::exact_work(&tx, a.sink.child(), &w)? {
                return Err(Error::LocalAuthority);
            }
            exact_pin(&tx, &a, &w, &attempt.call.call_id)?;
            periods::require_call_current(&tx,a.sink.child(),&attempt.call.call_id)?;
            context::require_call(&tx,&w,&attempt.call.call_id,attempt.input.as_ref())?;
            let (_, binding, _) = budget(&tx, &a)?;
            let h = calls::history(&tx, a.sink.child(), &attempt.call.call_id)?;
            if h.state != "prepared" || h.prepare_revision != attempt.call.prepare_revision {
                return Err(Error::OutcomeUnknown);
            }
            work::event(&tx, &w, "start_confirmed", serde_json::to_value(&start)?)?;
            let rev = revision(&tx)?;
            tx.execute(
                "UPDATE owner_managed_calls SET state='issued' WHERE id=?1 AND state='prepared'",
                [&h.call_id],
            )?;
            tx.execute(
                "UPDATE owner_managed_call_pins SET issue_revision=?2 WHERE call_id=?1",
                params![h.call_id, rev],
            )?;
            let call = ManagedCallCustody {
                scope: Scope {
                    registry: r,
                    profile: a.enrollment.profile.clone(),
                    child: a.sink.child().into(),
                    target: h.call_id,
                    binding,
                    issued_revision: rev,
                    expires,
                },
            };
            call.scope.mark_live();
            a.check()?;
            tx.commit()?;
            a.check().map_err(|_| Error::OutcomeUnknown)?;
            Ok(SharedCallCustody {
                sink: a.sink,
                work: w,
                call,
                input:attempt.input,
            })
        })
        .await?;
    after(&profile, &registry.0, expires)?;
    Ok(value)
}
