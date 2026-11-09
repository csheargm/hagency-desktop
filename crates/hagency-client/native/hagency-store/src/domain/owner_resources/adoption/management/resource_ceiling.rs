//! Append-only monthly-maximum amendments. They are allowed exactly where an
//! in-place edit is refused because the resource was offered, published or
//! retains commitments. An amendment never writes the definition or its
//! provenance, so no revision, grant receipt, reserve command or Agent custody
//! changes, and the live grant's per-request limit stays as published.
use super::resource_edit::blocker;
use super::*;
use crate::domain::resource_ceiling;
use crate::management::{
    CeilingAmendment, CeilingAmendmentBlocker, CeilingAmendmentRefusal, CommandReceipt,
    EditLocalResource, ResourceEditBlocker,
};
use crate::{
    CeilingAmendmentResult, CeilingChange, ResourceConfigurationResult,
    resource_publication_revision,
};

/// Detail readback. It is never authority for a later write: the writer
/// repeats every check in its own transaction.
pub(super) fn amendment_state(
    db: &Connection,
    registry: &str,
    resource: &Resource,
    now: u64,
) -> Result<CeilingAmendment, Error> {
    let sequence = resource_ceiling::latest_amendment(db, &resource.id())?.map_or(0, |(s, _)| s);
    let reason = eligibility(db, registry, resource)?;
    let minimum_tokens = if reason.is_none() {
        minimum(db, resource, now)?
    } else {
        None
    };
    Ok(CeilingAmendment {
        allowed: reason.is_none(),
        reason,
        sequence,
        minimum_tokens,
        per_request_tokens: per_request(db, &resource.id(), now)?,
    })
}

fn eligibility(
    db: &Connection,
    registry: &str,
    resource: &Resource,
) -> Result<Option<CeilingAmendmentBlocker>, Error> {
    match blocker(db, registry, resource)? {
        // Never offered, published or committed: change the definition instead.
        None => return Ok(Some(CeilingAmendmentBlocker::NotOffered)),
        Some(
            ResourceEditBlocker::PreviouslyOffered
            | ResourceEditBlocker::Published
            | ResourceEditBlocker::RetainedCommitments,
        ) => {}
        // Unknown history (the only other reason the blocker reports): neither
        // an edit nor an amendment can be proven safe.
        Some(_) => return Ok(Some(CeilingAmendmentBlocker::ContributionHistoryNotProven)),
    }
    let monthly = match resource_ceiling::effective_ceiling(db, resource)? {
        Some(ceiling) if serde_json::to_value(&ceiling.period)? == json!("monthly") || resource_ceiling::calendar(db, &resource.id())?.is_some() => {
            ceiling.tokens.is_some()
        }
        _ => false,
    };
    Ok((!monthly).then_some(CeilingAmendmentBlocker::MonthlyCeilingRequired))
}

/// The lowest maximum an amendment may set now: this month's commitments on
/// the resource (published grants, Agent holds, other allocations) and its
/// measured use. `None` while any of its usage is unsettled, because the
/// commitments are then not known: the maximum can only be raised.
fn minimum(db: &Connection, resource: &Resource, now: u64) -> Result<Option<u64>, Error> {
    if unsettled_exact_resource(db, &resource.id())? {
        return Ok(None);
    }
    // A grant whose month starts later (a scheduled renewal) holds capacity in
    // that month; evaluate there too so lowering never undercuts it.
    let mut points = vec![now];
    let mut later = db.prepare(
        "SELECT DISTINCT json_extract(receipt,'$.periodStartMs') FROM owner_capacity_grants WHERE resource_id=?1 AND json_valid(receipt) AND json_type(receipt,'$.periodStartMs')='integer' AND json_extract(receipt,'$.periodStartMs')>?2 LIMIT 24",
    )?;
    for start in later.query_map(params![resource.id(), now], |r| r.get::<_, u64>(0))? {
        points.push(start?);
    }
    let mut floor = 1u64;
    for at in points {
        let budget = crate::domain::budget_at(db, resource, None, false, at)?;
        floor = floor.max(u64::from(budget.pool.committed));
    }
    let report = crate::domain::usage::ceiling_report(db, &resource.id(), now)?;
    Ok(Some(floor.max(report.drawn)))
}

/// The live grant's per-request limit, when exactly one grant of this resource
/// is live now. An amendment never changes it.
fn per_request(db: &Connection, resource_id: &str, now: u64) -> Result<Option<u64>, Error> {
    let live = db
        .prepare(
            "SELECT COALESCE(json_extract(receipt,'$.perRequestTokens'),allocated_tokens) FROM owner_capacity_grants WHERE resource_id=?1 AND active=1 AND json_valid(receipt) AND json_extract(receipt,'$.periodStartMs')<=?2 AND json_extract(receipt,'$.periodEndMs')>?2 AND json_extract(receipt,'$.delegationExpiresAtMs')>?2 LIMIT 2",
        )?
        .query_map(params![resource_id, now], |r| r.get::<_, u64>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match live.as_slice() {
        [tokens] => Some(*tokens),
        _ => None,
    })
}

fn refused(reason: CeilingAmendmentRefusal) -> Error {
    Error::CeilingAmendmentRefused(reason)
}

impl CurrentManagementStore {
    /// `input` is a ceiling amendment request (`EditLocalResource::is_ceiling_amendment`).
    /// It shares the edit command's receipt, so recovery by command id is the
    /// same for both and the original result is returned on replay.
    pub(crate) async fn amend_resource_ceiling(
        &self,
        profile: &ProfileAuthority,
        input: EditLocalResource,
        receipt: CommandReceipt,
        confirmed_until: Instant,
    ) -> Result<ResourceConfigurationResult, Error> {
        self.check(profile)?;
        input.validate()?;
        let expected = input
            .expected_ceiling_sequence
            .ok_or(hagency_core::InvalidInput(
                "a ceiling amendment names its expected sequence",
            ))?;
        let backing = self.clone();
        let authority = profile.clone();
        let (result, replayed) = self
            .registry
            .0
            .store
            .call(16 * 1024, move |db| {
                backing.check(&authority)?;
                let tx = db
                    .db
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
                backing.check(&authority)?;
                authorize_resource(&tx, &authority, &input.resource_id)?;
                // The original receipt precedes every mutable check: recovering
                // a committed amendment never applies it a second time.
                if let Some(result) = receipt.lookup(&tx)? {
                    backing.check(&authority)?;
                    return Ok((result, true));
                }
                if Instant::now() >= confirmed_until {
                    return Err(Error::LocalAuthority);
                }
                let resource = read_resource(&tx, &input.resource_id)?;
                if resource_publication_revision(&resource)? != input.expected_revision {
                    return Err(Error::ResourceEditRevisionChanged);
                }
                db.accounts.check_resource(&tx, &resource)?;
                let now = graphs::now_ms()?;
                let state = amendment_state(&tx, backing.registry.id(), &resource, now)?;
                if state.sequence != expected {
                    return Err(Error::ResourceEditRevisionChanged);
                }
                if !state.allowed {
                    return Err(refused(CeilingAmendmentRefusal::NotAvailable));
                }
                let tokens = match input.ceiling {
                    CeilingChange::Monthly { tokens } => u64::from(tokens),
                    CeilingChange::Clear {} => {
                        return Err(refused(CeilingAmendmentRefusal::ClearRefused));
                    }
                    CeilingChange::Preserve {} => {
                        return Err(hagency_core::InvalidInput(
                            "a ceiling amendment sets a monthly maximum",
                        )
                        .into());
                    }
                };
                let previous = resource_ceiling::effective_ceiling_tokens(&tx, &resource)?
                    .ok_or_else(|| refused(CeilingAmendmentRefusal::NotAvailable))?;
                if tokens == previous {
                    return Err(refused(CeilingAmendmentRefusal::Unchanged));
                }
                if tokens < previous {
                    match state.minimum_tokens {
                        None => return Err(refused(CeilingAmendmentRefusal::UsageUnsettled)),
                        Some(minimum) if tokens < minimum => {
                            return Err(refused(CeilingAmendmentRefusal::BelowCommitments));
                        }
                        Some(_) => {}
                    }
                }
                let sequence = expected.checked_add(1).ok_or(Error::Capacity)?;
                bounded_row(
                    &tx,
                    "owner_resource_ceiling_amendments",
                    "command_id",
                    &input.command_id,
                    100_000,
                )?;
                tx.execute(
                    "INSERT INTO owner_resource_ceiling_amendments(resource_id,sequence,profile,command_id,base_revision,previous_tokens,tokens,period,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,'monthly',?8)",
                    params![
                        resource.id(),
                        sequence,
                        authority.digest,
                        input.command_id,
                        input.expected_revision,
                        previous,
                        tokens,
                        now
                    ],
                )?;
                let result = ResourceConfigurationResult {
                    resource_id: resource.id(),
                    revision: input.expected_revision.clone(),
                    published: resource.published,
                    ceiling_amendment: Some(CeilingAmendmentResult {
                        sequence,
                        previous_tokens: previous,
                        tokens,
                    }),
                };
                receipt.record(&tx, &result)?;
                backing.check(&authority)?;
                if Instant::now() >= confirmed_until {
                    return Err(Error::LocalAuthority);
                }
                tx.commit()?;
                Ok((result, false))
            })
            .await?;
        self.check(profile).map_err(|_| Error::OutcomeUnknown)?;
        if !replayed && Instant::now() >= confirmed_until {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }
}
