use super::*;
use sha2::{Digest, Sha256};

pub(in super::super) const MANAGED: &str = "managed_agent_v1";
pub(in super::super) struct Liability {
    pub retained: u64,
    pub consumed: u64,
    pub unresolved: u64,
    pub complete: bool,
    pub digest: String,
}
fn add(a: u64, b: u64) -> Result<u64, Error> {
    a.checked_add(b)
        .filter(|v| *v <= JSON_SAFE_MAX)
        .ok_or(Error::Capacity)
}
pub(super) struct ChildLiability {
    pub retained: u64,
    pub consumed: u64,
    pub complete: bool,
    pub process_state: &'static str,
    pub manifest: serde_json::Value,
}
/// The same bounded accounting calculation backs project and agent settlement.
/// A stopped process never substitutes for complete provider usage.
pub(super) fn child(db: &Connection, id: &str) -> Result<ChildLiability, Error> {
    child_at(db,id,graphs::now_ms()?)
}
fn child_at(db: &Connection, id: &str, _at: u64) -> Result<ChildLiability, Error> {
    let (original, state, fenced): (u64, String, bool) = db.query_row(
        "SELECT allocated_tokens,state,permanent_fence FROM owner_managed_agents WHERE id=?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let allocated = custody::budget::effective_ceiling(db, id, original)?;
    let effect: Option<String> = db
        .query_row(
            "SELECT state FROM owner_managed_effects WHERE child_id=?1",
            [&id],
            |r| r.get(0),
        )
        .optional()?;
    let mut unknown = effect
        .as_deref()
        .is_none_or(|s| matches!(s, "issued" | "unknown"));
    // A qualified text process is still live capacity. Only an observation
    // from its original retained supervisor can prove it stopped; a task
    // result or expired central activity does not release this liability.
    unknown |= fenced && db.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_runtime_lifetimes l WHERE l.child_id=?1 AND NOT EXISTS(SELECT 1 FROM owner_shared_process_observations o WHERE o.child_id=l.child_id AND json_extract(o.payload,'$.state')='stopped'))",[&id],|r|r.get::<_,bool>(0))?;
    // A successor is separate one-shot custody, never a reset of the old
    // stopped observation. Until its own actual supervisor proves exit,
    // parent/project settlement must retain this child's full uncertainty.
    unknown |= db.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_successor_attempts a WHERE a.child_id=?1 AND NOT EXISTS(SELECT 1 FROM owner_shared_successor_observations o WHERE o.attempt_id=a.id AND json_extract(o.payload,'$.state')='stopped'))",[&id],|r|r.get::<_,bool>(0))?;
    unknown |= db.query_row("SELECT EXISTS(SELECT 1 FROM owner_managed_runtime_lifetimes l WHERE l.child_id=?1 AND l.issued_revision IS NOT NULL AND NOT EXISTS(SELECT 1 FROM owner_managed_runtime_observations o WHERE o.child_id=l.child_id AND json_extract(o.payload,'$.record.state')='stopped'))",[id],|r|r.get::<_,bool>(0))?;
    let process_unknown = unknown;
    let mut calls = db.prepare("SELECT state,reserved_tokens,known_consumed_tokens,usage_complete,unrepresentable_usage FROM owner_managed_calls WHERE child_id=?1 ORDER BY id")?;
    let mut call_rows = calls.query([&id])?;
    let mut calls_count = 0;
    let mut spent = 0;
    let mut issued = false;
    let mut ever_issued = false;
    while let Some(call) = call_rows.next()? {
        if calls_count >= 100_000 {
            return Err(Error::Capacity);
        }
        calls_count += 1;
        let state: String = call.get(0)?;
        let known: u64 = call.get(2)?;
        let reported: bool = call.get(3)?;
        let overflow: bool = call.get(4)?;
        spent = add(spent, known)?;
        let running = matches!(state.as_str(), "issued" | "unknown");
        issued |= running;
        ever_issued |=
            !matches!(state.as_str(), "prepared" | "cancelled_before_issue") || known > 0;
        unknown |= (fenced && state == "prepared")
            || overflow
            || running
            || (state == "settled" && !reported)
            || (matches!(state.as_str(), "prepared" | "cancelled_before_issue") && known > 0);
    }
    let held = if fenced && state == "fenced" && !unknown && !issued {
        spent
    } else {
        allocated.max(spent)
    };
    let never_started = effect.as_deref() == Some("pending") && !ever_issued
        && !db.query_row("SELECT EXISTS(SELECT 1 FROM owner_shared_runtime_lifetimes WHERE child_id=?1 UNION ALL SELECT 1 FROM owner_managed_runtime_lifetimes WHERE child_id=?1 AND issued_revision IS NOT NULL)",[id],|r|r.get::<_,bool>(0))?;
    Ok(ChildLiability {
        retained: held,
        consumed: spent,
        complete: !unknown,
        process_state: if process_unknown || issued {
            "unknown"
        } else if never_started {
            "not_started"
        } else {
            "stopped"
        },
        manifest: json!({"id":id,"allocatedTokens":allocated,"state":state,"fenced":fenced,"retainedTokens":held,"knownConsumedTokens":spent,"unknown":unknown,"issued":issued,"calls":calls_count}),
    })
}
pub(super) fn children(db: &Connection, allocation: &str) -> Result<Liability, Error> {
    let mut query = db.prepare("SELECT id,allocated_tokens,state,permanent_fence FROM owner_managed_agents WHERE project_reservation_id=?1 ORDER BY id")?;
    let mut rows = query.query([allocation])?;
    let mut retained = 0;
    let mut consumed = 0;
    let mut complete = true;
    let mut count = 0;
    let mut manifest = Sha256::new();
    manifest.update(b"[");
    while let Some(row) = rows.next()? {
        if count >= 100_000 {
            return Err(Error::Capacity);
        }
        let id: String = row.get(0)?;
        let item = child(db, &id)?;
        if count > 0 {
            manifest.update(b",");
        }
        manifest.update(canonical::encode(&item.manifest)?.as_bytes());
        count += 1;
        retained = add(retained, item.retained)?;
        consumed = add(consumed, item.consumed)?;
        complete &= item.complete;
    }
    manifest.update(b"]");
    Ok(Liability {
        retained,
        consumed,
        unresolved: retained.saturating_sub(consumed),
        complete,
        digest: format!("{:x}", manifest.finalize()),
    })
}

pub(in super::super) fn project_liability(
    db: &Connection,
    allocation: &str,
) -> Result<Liability, Error> {
    let (requested, state, mode, retained): (u64, String, String, u64) = db.query_row(
        "SELECT requested_tokens,state,accounting_mode,retained_tokens FROM owner_project_reservations WHERE id=?1",
        [allocation],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut value = children(db, allocation)?;
    value.retained = value.retained.max(retained);
    let has_children: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_managed_agents WHERE project_reservation_id=?1)",
        [allocation],
        |r| r.get(0),
    )?;
    let supported = mode == MANAGED || (mode == parent::RESERVATION_ONLY && !has_children);
    if state == "reserved" || !supported {
        value.retained = value.retained.max(requested);
    }
    value.complete &= supported;
    value.unresolved = value.retained.saturating_sub(value.consumed);
    value.digest = canonical::digest(
        &json!({"allocationId":allocation,"state":state,"mode":mode,"requestedTokens":requested,"childrenDigest":value.digest,"retainedTokens":value.retained,"knownConsumedTokens":value.consumed,"complete":value.complete}),
    )?;
    Ok(value)
}
pub(in super::super) fn grant_floor(db: &Connection, grant: &str) -> Result<Liability, Error> {
    let mut q =
        db.prepare("SELECT id FROM owner_project_reservations WHERE grant_id=?1 ORDER BY id")?;
    let rows = q.query_map([grant], |r| r.get::<_, String>(0))?;
    let mut value = Liability {
        retained: 0,
        consumed: 0,
        unresolved: 0,
        complete: true,
        digest: String::new(),
    };
    let mut digest = Sha256::new();
    for (count, row) in rows.enumerate() {
        if count >= 100_000 {
            return Err(Error::Capacity);
        }
        let child = project_liability(db, &row?)?;
        value.retained = add(value.retained, child.retained)?;
        value.consumed = add(value.consumed, child.consumed)?;
        value.complete &= child.complete;
        digest.update(child.digest.as_bytes());
    }
    let mut direct = db.prepare("SELECT id FROM owner_managed_agents WHERE pool_grant_id=?1 UNION SELECT child_id AS id FROM owner_managed_periods WHERE grant_id=?1 ORDER BY id")?;
    let children = direct.query_map([grant], |r| r.get::<_,String>(0))?;
    for (count, id) in children.enumerate() {
        if count >= 100_000 { return Err(Error::Capacity); }
        let id=id?;
        let child = child_for_grant(db, &id, grant, graphs::now_ms()?)?;
        value.retained = add(value.retained,child.retained)?;
        value.consumed = add(value.consumed,child.consumed)?;
        value.complete &= child.complete;
        digest.update(canonical::encode(&child.manifest)?.as_bytes());
    }
    value.unresolved = value.retained.saturating_sub(value.consumed);
    value.digest = format!("{:x}", digest.finalize());
    Ok(value)
}

/// Direct pool children whose accounting grant at `at` is `grant`, in id
/// order: children of this grant or of a predecessor that the chain carried
/// here (section 3.3). For a grant without succession links these are exactly
/// the children whose original grant is `grant`.
pub(in super::super) fn accounted_children(
    db: &Connection,
    grant: &str,
    at: u64,
) -> Result<Vec<(String, bool)>, Error> {
    let mut out = Vec::new();
    for original in super::super::succession::lineage(db, grant)? {
        let mut q = db.prepare(
            "SELECT id FROM owner_managed_agents WHERE pool_grant_id=?1 ORDER BY id LIMIT 100001",
        )?;
        let ids = q
            .query_map([&original], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for id in ids {
            if out.len() >= 100_000 {
                return Err(Error::Capacity);
            }
            if periods::enabled(db,&id)? {continue;}
            let fenced = super::super::succession::fenced_at(db, &id)?;
            if super::super::succession::accounting_grant(db, &original, fenced, at)? == grant {
                out.push((id, fenced.is_some()));
            }
        }
    }
    out.extend(periods::grant_children(db,grant,at)?);
    out.sort();
    out.dedup();
    Ok(out)
}
/// A grant's hold split for section 3.5: what stays with it when its chain
/// moves on (project reservations and permanently fenced children) and what an
/// amendment or renewal carries (live direct children).
pub(in super::super) struct AccountedFloor {
    pub total: Liability,
    pub staying: Liability,
    pub moving: Liability,
    pub moving_children: u64,
}
fn empty() -> Liability {
    Liability {
        retained: 0,
        consumed: 0,
        unresolved: 0,
        complete: true,
        digest: String::new(),
    }
}
fn include(into: &mut Liability, retained: u64, consumed: u64, complete: bool) -> Result<(), Error> {
    into.retained = add(into.retained, retained)?;
    into.consumed = add(into.consumed, consumed)?;
    into.complete &= complete;
    into.unresolved = into.retained.saturating_sub(into.consumed);
    Ok(())
}
/// `grant_floor` by accounting grant at `at`. For a grant without succession
/// links it is the same set, so the same numbers.
pub(in super::super) fn accounted_floor(
    db: &Connection,
    grant: &str,
    at: u64,
) -> Result<AccountedFloor, Error> {
    let mut floor = AccountedFloor {
        total: empty(),
        staying: empty(),
        moving: empty(),
        moving_children: 0,
    };
    let mut digest = Sha256::new();
    let mut q =
        db.prepare("SELECT id FROM owner_project_reservations WHERE grant_id=?1 ORDER BY id")?;
    let rows = q.query_map([grant], |r| r.get::<_, String>(0))?;
    for (count, row) in rows.enumerate() {
        if count >= 100_000 {
            return Err(Error::Capacity);
        }
        let child = project_liability(db, &row?)?;
        include(&mut floor.total, child.retained, child.consumed, child.complete)?;
        include(&mut floor.staying, child.retained, child.consumed, child.complete)?;
        digest.update(child.digest.as_bytes());
    }
    for (id, fenced) in accounted_children(db, grant, at)? {
        let child = child_for_grant(db, &id, grant, at)?;
        include(&mut floor.total, child.retained, child.consumed, child.complete)?;
        if fenced {
            include(&mut floor.staying, child.retained, child.consumed, child.complete)?;
        } else {
            include(&mut floor.moving, child.retained, child.consumed, child.complete)?;
            floor.moving_children += 1;
        }
        digest.update(canonical::encode(&child.manifest)?.as_bytes());
    }
    floor.total.digest = format!("{:x}", digest.finalize());
    Ok(floor)
}
fn merge(into: &mut Liability, part: &Liability) -> Result<(), Error> {
    include(into, part.retained, part.consumed, part.complete)
}
/// The same-period floor of `grant` at `at`: children accounted to it or to a
/// predecessor it superseded within this UTC month (an amendment chain). Fenced
/// children stay in it, so an amendment never moves a hold outside the cap of
/// the grant now heading the month. For a root it is `accounted_floor`.
pub(in super::super) fn period_floor(
    db: &Connection,
    grant: &str,
    at: u64,
) -> Result<AccountedFloor, Error> {
    let key = |id: &str| -> Result<String, Error> {
        let raw: String = db.query_row(
            "SELECT receipt FROM owner_capacity_grants WHERE id=?1",
            [id],
            |r| r.get(0),
        )?;
        Ok(serde_json::from_str::<NativeGrantReceipt>(&raw)?.period_key)
    };
    let period = key(grant)?;
    let mut floor = AccountedFloor {
        total: empty(),
        staying: empty(),
        moving: empty(),
        moving_children: 0,
    };
    let mut digest = Sha256::new();
    for (index, id) in super::super::succession::lineage(db, grant)?.into_iter().enumerate() {
        if index > 0 && key(&id)? != period {
            break;
        }
        let part = accounted_floor(db, &id, at)?;
        merge(&mut floor.total, &part.total)?;
        merge(&mut floor.staying, &part.staying)?;
        merge(&mut floor.moving, &part.moving)?;
        floor.moving_children = floor
            .moving_children
            .checked_add(part.moving_children)
            .ok_or(Error::Capacity)?;
        digest.update(part.total.digest.as_bytes());
    }
    floor.total.digest = format!("{:x}", digest.finalize());
    Ok(floor)
}
/// The direct pool children a closure or settlement of `grant` must cover:
/// those whose accounting grant at `at` is `grant`.
pub(in super::super) fn accounted_pool_liability(
    db: &Connection,
    grant: &str,
    at: u64,
) -> Result<(Liability, u64), Error> {
    let mut value = empty();
    let mut digest = Sha256::new();
    let mut count = 0;
    for (id, _) in accounted_children(db, grant, at)? {
        let child = child_for_grant(db, &id, grant, at)?;
        include(&mut value, child.retained, child.consumed, child.complete)?;
        digest.update(canonical::encode(&child.manifest)?.as_bytes());
        count += 1;
    }
    value.digest = format!("{:x}", digest.finalize());
    Ok((value, count))
}
fn child_for_grant(db:&Connection,id:&str,grant:&str,at:u64)->Result<ChildLiability,Error>{
    if !periods::enabled(db,id)? {return child_at(db,id,at);}
    let (retained,consumed,complete)=periods::grant_usage(db,id,grant,at)?;
    Ok(ChildLiability{retained,consumed,complete,process_state:if complete{"stopped"}else{"unknown"},
        manifest:json!({"id":id,"grantId":grant,"retainedTokens":retained,"knownConsumedTokens":consumed,"complete":complete})})
}
/// E20 for a successor: a carried child's in-flight usage is bounded by its
/// hold, so it counts as held instead of blocking. Usage that cannot be
/// represented still blocks, as does any unsettled child outside `chain`.
pub(in crate::domain) fn unsettled_resource_except(
    db: &Connection,
    resource: &Resource,
    chain: &std::collections::BTreeSet<String>,
) -> Result<bool, Error> {
    let shared = crate::domain::account_ceiling::shared(db, &resource.seat_id)?;
    let mut q=db.prepare("SELECT id FROM owner_capacity_grants WHERE accounting_mode=?1 AND (preset_id=?2 OR (?4 AND seat_id=?3))")?;
    let rows = q
        .query_map(params![MANAGED, resource.preset_id, resource.seat_id, shared], |r| {
            r.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for grant in rows {
        if !chain.contains(&grant) {
            if !grant_floor(db, &grant)?.complete {
                return Ok(true);
            }
            continue;
        }
        let projects: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_project_reservations WHERE grant_id=?1)",
            [&grant],
            |r| r.get(0),
        )?;
        if projects && !grant_floor(db, &grant)?.complete {
            return Ok(true);
        }
        let overflow: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_managed_calls c JOIN owner_managed_agents a ON a.id=c.child_id WHERE a.pool_grant_id=?1 AND c.unrepresentable_usage=1)",
            [&grant],
            |r| r.get(0),
        )?;
        if overflow {
            return Ok(true);
        }
        // Bounded sums only: an overflowing hold cannot be counted as held.
        grant_floor(db, &grant)?;
    }
    Ok(false)
}
pub(in crate::domain) fn unsettled_resource(
    db: &Connection,
    resource: &Resource,
) -> Result<bool, Error> {
    let shared = crate::domain::account_ceiling::shared(db, &resource.seat_id)?;
    let mut q=db.prepare("SELECT id FROM owner_capacity_grants WHERE accounting_mode=?1 AND (preset_id=?2 OR (?4 AND seat_id=?3))")?;
    let rows = q.query_map(
        params![MANAGED, resource.preset_id, resource.seat_id, shared],
        |r| r.get::<_, String>(0),
    )?;
    for row in rows {
        if !grant_floor(db, &row?)?.complete {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Definition visibility/retirement does not change an account or seat budget.
/// Inspect only this exact resource's retained children, including inactive
/// parent grants whose issued work has not yet been fully accounted for.
pub(in crate::domain) fn unsettled_exact_resource(db: &Connection, resource_id: &str) -> Result<bool, Error> {
    let mut q = db.prepare("SELECT id FROM owner_capacity_grants WHERE accounting_mode=?1 AND resource_id=?2")?;
    let rows = q.query_map(params![MANAGED, resource_id], |r| r.get::<_, String>(0))?;
    for row in rows {
        if !grant_floor(db, &row?)?.complete { return Ok(true); }
    }
    Ok(false)
}

/// Fence first, then release only the exact derived child liability. An overrun
/// cannot be encoded by the existing v1 project release receipt: retain it and
/// return an explicit refusal, never clamp it into a successful refund.
pub(in super::super) fn release_project(
    db: &Connection,
    c: &AllocationCommand,
    result: &mut NativeAllocationReceipt,
) -> Result<(), Error> {
    db.execute("UPDATE owner_managed_agents SET permanent_fence=1,state='fenced' WHERE project_reservation_id=?1",[&c.allocation_id])?;
    db.execute("UPDATE owner_managed_calls SET state='cancelled_before_issue' WHERE state='prepared' AND child_id IN(SELECT id FROM owner_managed_agents WHERE project_reservation_id=?1)",[&c.allocation_id])?;
    let liability = children(db, &c.allocation_id)?;
    if !liability.complete || liability.retained > c.requested_tokens {
        result.reason = Some("unsettled_usage".into());
        return Ok(());
    }
    db.execute(
        "UPDATE owner_project_reservations SET state='released',retained_tokens=?2 WHERE id=?1",
        params![c.allocation_id, liability.retained],
    )?;
    result.state = "released".into();
    result.reserved_tokens = c.requested_tokens;
    result.retained_tokens = liability.retained;
    Ok(())
}
