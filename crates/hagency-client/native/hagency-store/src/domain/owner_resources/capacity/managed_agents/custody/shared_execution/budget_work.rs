//! V3 uses the same call ledger and source-bound capture as earlier attempts.
//! Only its fresh-work ceiling changes, and only through an exact applied delta.
use super::*;

pub(super) fn live(
    db: &Connection,
    a: &VerifiedSharedActivity,
    sink: &SharedBudgetAttemptProcessSink,
    current_head: bool,
) -> Result<(), Error> {
    a.check()?;
    let c = &sink.custody;
    budget_fence::require_unfenced(db, &c.command)?;
    if budget_attempt::control_revision(db, &a.enrollment.profile.digest, &c.command)?
        != c.demand_control_revision()
    {
        return Err(Error::LocalAuthority);
    }
    // Verifies the complete append-only delta chain, not a cached UI balance.
    // Renewal may finish an old issued call. Every fresh work writer requires
    // the current head and will refuse to spend through that historical lease.
    super::super::budget::execution::resolve(
        db,
        &c.command.agent_reservation_id,
        Some(&c.command.execution_budget),
        current_head,
    )?;
    let original = authority::original(db, &a.enrollment, &c.command.agent_reservation_id, false)?;
    if original.provider != c.provider
        || original.digest != c.command.original_runtime_command.reserve_command_digest
        || {
            let now = graphs::now_ms()?;
            now >= authority::lifetime(db, &original, now)?
        }
    {
        return Err(Error::LocalAuthority);
    }
    let bound: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_shared_successor_attempts WHERE id=?1 AND child_id=?2 AND profile=?3 AND command_digest=?4 AND incarnation=?5 AND provider_digest=?6 AND issued_revision=?7) AND NOT EXISTS(SELECT 1 FROM owner_shared_successor_attempts WHERE child_id=?2 AND process_attempt>?8)",
        params![c.command.command_id,c.command.agent_reservation_id,a.enrollment.profile.digest,c.digest,c.incarnation,c.provider,c.issued_revision,c.command.process_attempt],
        |r| r.get(0),
    )?;
    let qualified: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_shared_successor_observations WHERE attempt_id=?1 AND json_extract(payload,'$.state')='qualified') AND NOT EXISTS(SELECT 1 FROM owner_shared_successor_observations WHERE attempt_id=?1 AND json_extract(payload,'$.state')='stopped')",
        [&c.command.command_id], |r| r.get(0),
    )?;
    let parent_closed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_parent_settlements s JOIN owner_project_reservations p ON p.grant_id=s.grant_id WHERE p.id=?1)",
        [&original.command.allocation_id], |r| r.get(0),
    )?;
    if !bound || !qualified || parent_closed {
        return Err(Error::LocalAuthority);
    }
    // Budget availability belongs to prepare/issue. In particular, a sibling
    // call's late overrun must not abort this call's renewal or evidence.
    Ok(())
}
