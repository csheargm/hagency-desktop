use super::*;

pub(super) struct Binding {
    pub command: ManagedAgentReserveCommand,
    pub digest: String,
    pub provider: String,
    pub allocated: u64,
}
pub(super) fn original(
    db: &Connection,
    e: &VerifiedManagementEnrollment,
    child: &str,
    cleanup: bool,
) -> Result<Binding, Error> {
    let row:Option<(String,String,String,u64,bool)>=db.query_row("SELECT profile,original_command,original_command_digest,allocated_tokens,permanent_fence FROM owner_managed_agents WHERE id=?1",[child],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let (profile, command, digest, allocated, fenced) = row.ok_or(Error::LocalAuthority)?;
    let c: ManagedAgentReserveCommand = serde_json::from_str(&command)?;
    let original;
    let g = if let Some(grant) = &c.pool_grant {
        c.validate(&e.profile.account)?;
        let stored: Option<String> = db.query_row("SELECT pool_grant_id FROM owner_managed_agents WHERE id=?1", [child], |r| r.get(0))?;
        if stored.as_deref() != Some(&grant.grant_id) { return Err(Error::LocalAuthority); }
        grant
    } else {
    let project: String = db.query_row(
        "SELECT reserve_command FROM owner_project_authorities WHERE allocation_id=?1",
        [&c.allocation_id],
        |r| r.get(0),
    )?;
    original = serde_json::from_str::<AllocationCommand>(&project)?;
    &original.grant_receipt
    };
    if profile != e.profile.digest
        || canonical::digest(&serde_json::to_value(&c)?)? != digest
        || !e.matches_cleanup(g)
        || e.executor_generation < g.executor_generation
    {
        return Err(Error::LocalAuthority);
    }
    let (stored, active, parent_mode): (String, bool, String) = db.query_row(
        "SELECT receipt,active,accounting_mode FROM owner_capacity_grants WHERE id=?1",
        [&g.grant_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if canonical::digest(&serde_json::from_str::<serde_json::Value>(&stored)?)?
        != c.grant_receipt_digest
    {
        return Err(Error::Conflict);
    }
    let resource = read_resource(db, &c.resource_id)?;
    // Retain the original seat identity for late cleanup, even if the owner
    // has since withdrawn consent. New execution separately checks the full
    // current mapping and re-observes the profile before qualification.
    let identity_kind = if resource.framework == "octos" { "explicit_owner_mapping" } else { "verified_provider" };
    let provider:String=db.query_row("SELECT provider_digest FROM owner_resource_accounts WHERE seat_id=?1 AND identity_kind=?2",params![resource.seat_id,identity_kind],|r|r.get(0))?;
    if !cleanup {
        if !super::super::reserve::admitted_account(db, &resource)? { return Err(Error::LocalAuthority); }
        let (state, mode): (String, String) = if c.pool_grant.is_some() {
            ("reserved".into(), MANAGED.into())
        } else { db.query_row(
            "SELECT state,accounting_mode FROM owner_project_reservations WHERE id=?1",
            [&c.allocation_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )? };
        let now = graphs::now_ms()?;
        // A pool child's authority is the grant governing its chain now: the
        // original receipt digest is checked above, the live grant here.
        let head = if c.pool_grant.is_some() {
            super::super::super::succession::effective_at(db, &g.grant_id, now)?
        } else {
            g.grant_id.clone()
        };
        let (governing, active, parent_mode) = if head == g.grant_id {
            (g.clone(), active, parent_mode)
        } else {
            let (raw, active, mode): (String, bool, String) = db.query_row(
                "SELECT receipt,active,accounting_mode FROM owner_capacity_grants WHERE id=?1",
                [&head],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            (serde_json::from_str::<NativeGrantReceipt>(&raw)?, active, mode)
        };
        let g = &governing;
        if fenced
            || !active
            || state != "reserved"
            || mode != MANAGED
            || parent_mode != MANAGED
            || !e.matches(g)
            || resource_revision(db, &c.resource_id)? != c.resource_revision
            || now < g.period_start_ms
            || now >= g.period_end_ms
            || now >= g.delegation_expires_at_ms
            || !hagency_core::qualification::qualifies(
                &resource.profile(),
                &c.definition.capability,
                None,
            )
        {
            return Err(Error::LocalAuthority);
        }
        authorize_resource(db, &e.profile, &c.resource_id)?;
        if periods::recurring(&c) {
            let period = periods::current(db, child, now)?.ok_or(Error::LocalAuthority)?;
            if period.tokens != allocated || period.period.key != g.period_key
                || period.period.start_ms != g.period_start_ms || period.period.end_ms != g.period_end_ms {
                return Err(Error::LocalAuthority);
            }
        }
    }
    Ok(Binding {
        command: c,
        digest,
        provider,
        allocated,
    })
}
/// When a child's authority ends. A pool child lives while its capacity chain
/// continues (phase 1b): the chain lifetime replaces its reserve command's
/// expiry. A project child keeps its original command's expiry.
pub(super) fn lifetime(db: &Connection, b: &Binding, now: u64) -> Result<u64, Error> {
    if periods::recurring(&b.command) {
        let child = format!("mar_{}", &b.digest[..40]);
        return Ok(periods::current(db, &child, now)?.ok_or(Error::LocalAuthority)?.period.end_ms);
    }
    match &b.command.pool_grant {
        Some(grant) => super::super::super::succession::chain_until(db, &grant.grant_id, now),
        None => Ok(b.command.expires_at_ms),
    }
}
pub(super) fn cleanup(db: &Connection, a: &ManagedCleanupAuthority) -> Result<Binding, Error> {
    original(db, &a.enrollment, &a.child, true)
}
pub(super) fn assignment(
    db: &Connection,
    a: &ManagedAssignmentAuthority,
    insert: bool,
) -> Result<(Binding, String), Error> {
    let b = original(db, &a.enrollment, &a.child, false)?;
    pin_assignment(db,b,&a.child,&a.assignment,a.generation,insert)
}
pub(super) fn assignment_for_period(db:&Connection,e:&VerifiedManagementEnrollment,child:&str,assignment:&str,generation:u64)->Result<(),Error>{
    let b=original(db,e,child,false)?;
    pin_assignment(db,b,child,assignment,generation,true)?;
    Ok(())
}
fn pin_assignment(db:&Connection,b:Binding,child:&str,assignment:&str,generation:u64,insert:bool)->Result<(Binding,String),Error>{
    let digest = canonical::digest(
        &json!({"child":child,"assignmentId":assignment,"assignmentGeneration":generation,"commandDigest":b.digest,"providerDigest":b.provider}),
    )?;
    let stored:Option<(String,u64,String,String)>=db.query_row("SELECT assignment_id,assignment_generation,binding_digest,provider_digest FROM owner_managed_assignment_pins WHERE child_id=?1",[&child],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((id, saved_generation, binding, provider)) = stored {
        if id != assignment
            || saved_generation != generation
            || binding != digest
            || provider != b.provider
        {
            return Err(Error::Conflict);
        }
    } else if insert {
        db.execute(
            "INSERT INTO owner_managed_assignment_pins VALUES(?1,?2,?3,?4,?5)",
            params![child, assignment, generation, digest, b.provider],
        )?;
    } else {
        return Err(Error::LocalAuthority);
    }
    Ok((b, digest))
}
pub(super) fn postpaid_call_budget(db: &Connection, child: &str) -> Result<(u64, bool), Error> {
    let period = periods::current_id(db, child)?;
    call_budget_scoped(db,child,period.as_deref(),false)
}
pub(super) fn call_budget_all(db: &Connection, child: &str) -> Result<(u64, bool), Error> {
    call_budget_scoped(db,child,None,true)
}
fn call_budget_scoped(db:&Connection,child:&str,period:Option<&str>,require_complete:bool)->Result<(u64,bool),Error>{
    // An open call holds only what it is known to have spent, never its reservation.
    let mut q=db.prepare("SELECT state,known_consumed_tokens,usage_complete,unrepresentable_usage FROM owner_managed_calls WHERE child_id=?1 AND (?2 IS NULL OR id IN(SELECT call_id FROM owner_managed_call_periods WHERE period_id=?2))")?;
    let mut rows = q.query(params![child,period])?;
    let mut held = 0_u64;
    let mut unknown = false;
    let mut count = 0;
    while let Some(r) = rows.next()? {
        if count >= 100_000 {
            return Err(Error::Capacity);
        }
        count += 1;
        let state: String = r.get(0)?;
        let consumed: u64 = r.get(1)?;
        let complete: bool = r.get(2)?;
        let overflow: bool = r.get(3)?;
        held = held
            .checked_add(consumed)
            .filter(|n| *n <= JSON_SAFE_MAX)
            .ok_or(Error::Capacity)?;
        unknown |= overflow || require_complete && (
            matches!(state.as_str(), "issued" | "unknown")
            || (state == "settled" && !complete));
    }
    Ok((held, unknown))
}
