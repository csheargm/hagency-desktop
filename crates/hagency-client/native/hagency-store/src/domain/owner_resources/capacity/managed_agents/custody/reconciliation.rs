//! Explicit owner reconciliation of a managed call whose original process is
//! gone. The host never invents usage: the resource owner declares the final
//! consumption, at least what was already observed, including any overrun,
//! and the declaration is retained as audited evidence like any observation.
use super::*;

/// Calls of one Matrix agent identity whose final usage is still unknown.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterruptedCallQuery {
    pub agent_id: String,
}
impl InterruptedCallQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.agent_id, 128)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InterruptedCall {
    pub call_id: String,
    pub reservation_id: String,
    pub execution_id: String,
    /// `issued` or `unknown`.
    pub state: String,
    pub reserved_tokens: u64,
    pub known_consumed_tokens: u64,
    /// A usage observation exists, so a model request may have been sent.
    pub usage_observed: bool,
    pub period_key: Option<String>,
    /// A custody object in this process still owns the call; it cannot be
    /// reconciled until that process ends.
    pub live: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InterruptedCallPage {
    pub agent_id: String,
    pub fenced: bool,
    pub calls: Vec<InterruptedCall>,
    pub ledger_revision: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconcileInterruptedCall {
    pub command_id: String,
    pub agent_id: String,
    pub call_id: String,
    /// The reservation the owner saw; a changed call is refused.
    pub expected_reserved_tokens: u64,
    /// Final consumption verified by the owner, at least what was observed.
    /// An old estimate or a zero postpaid reservation never caps actual usage.
    pub consumed_tokens: u64,
    /// Bounded, non-secret reference to the owner's evidence (for example the
    /// provider usage page figure), retained in the private audit.
    pub evidence_reference: String,
}
impl ReconcileInterruptedCall {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.agent_id, 128)?;
        identifier(&self.call_id, 128)?;
        let reference = self.evidence_reference.trim();
        if reference.is_empty()
            || self.evidence_reference.chars().count() > 256
            || self.evidence_reference.chars().any(char::is_control)
        {
            return Err(Error::Conflict);
        }
        if self.consumed_tokens > JSON_SAFE_MAX
            || self.expected_reserved_tokens > JSON_SAFE_MAX
        {
            return Err(Error::Capacity);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InterruptedCallReconciliation {
    pub call_id: String,
    pub reservation_id: String,
    /// Always `settled`.
    pub state: String,
    pub consumed_tokens: u64,
    /// Reservation returned to the Agent's allowance by this declaration.
    pub released_tokens: u64,
    pub observation_id: String,
    pub ledger_revision: u64,
    /// The same declaration was already recorded; nothing changed.
    pub replayed: bool,
}

const OPEN_STATES: &str = "('issued','unknown')";

fn check_owner(profile: &ProfileAuthority, registry: &Inner) -> Result<(), Error> {
    profile.check()?;
    if registry.closed.load(Ordering::Acquire) {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}

fn live(registry: &Inner, call: &str) -> bool {
    registry
        .live_calls
        .lock()
        .map(|live| live.contains(call))
        .unwrap_or(true)
}

fn children(db: &Connection, profile: &str, agent: &str) -> Result<Vec<(String, bool)>, Error> {
    let mut q = db.prepare(
        "SELECT id,permanent_fence FROM owner_managed_agents WHERE profile=?1 AND agent_id=?2 ORDER BY allocation_generation,rowid",
    )?;
    let rows = q
        .query_map(params![profile, agent], |r| Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn open_calls(db: &Connection, registry: &Inner, child: &str) -> Result<Vec<InterruptedCall>, Error> {
    let mut q = db.prepare(&format!(
        "SELECT c.id,c.execution_id,c.state,c.reserved_tokens,c.known_consumed_tokens,p.period_id,\
         EXISTS(SELECT 1 FROM owner_managed_observations o WHERE o.target_id=c.id AND o.kind='usage') \
         FROM owner_managed_calls c LEFT JOIN owner_managed_call_periods p ON p.call_id=c.id \
         WHERE c.child_id=?1 AND c.state IN {OPEN_STATES} ORDER BY c.rowid LIMIT 1001"
    ))?;
    let rows = q
        .query_map([child], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, u64>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, bool>(6)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    if rows.len() > 1000 {
        return Err(Error::Capacity);
    }
    let mut out = Vec::with_capacity(rows.len());
    for (id, execution, state, reserved, known, period, observed) in rows {
        let period_key = match period {
            Some(period) => db
                .query_row(
                    "SELECT period_key FROM owner_managed_periods WHERE id=?1",
                    [&period],
                    |r| r.get::<_, String>(0),
                )
                .optional()?,
            None => None,
        };
        out.push(InterruptedCall {
            live: live(registry, &id),
            call_id: id,
            reservation_id: child.into(),
            execution_id: execution,
            state,
            reserved_tokens: reserved,
            known_consumed_tokens: known,
            usage_observed: observed,
            period_key,
        });
    }
    Ok(out)
}

impl OwnerResourceRegistry {
    pub async fn interrupted_calls(
        &self,
        profile: ProfileAuthority,
        query: InterruptedCallQuery,
    ) -> Result<InterruptedCallPage, Error> {
        query.validate()?;
        check_owner(&profile, &self.0)?;
        let registry = self.0.clone();
        let after = profile.clone();
        let result = self
            .0
            .store
            .call(32 * 1024, move |db| {
                check_owner(&profile, &registry)?;
                let mut page = InterruptedCallPage {
                    agent_id: query.agent_id.clone(),
                    fenced: false,
                    calls: Vec::new(),
                    ledger_revision: revision(&db.db)?,
                };
                let children = children(&db.db, &profile.digest, &query.agent_id)?;
                if children.is_empty() {
                    return Err(Error::Conflict);
                }
                for (child, fenced) in children {
                    page.fenced |= fenced;
                    page.calls.extend(open_calls(&db.db, &registry, &child)?);
                }
                Ok(page)
            })
            .await?;
        check_owner(&after, &self.0)?;
        Ok(result)
    }

    /// Settles one open call at the owner-declared consumption. Refused while
    /// a custody object in this process still owns the call, when the call
    /// changed since the owner looked, or when the declaration is below what
    /// was already observed. Replaying the identical declaration is a no-op.
    pub async fn reconcile_interrupted_call(
        &self,
        profile: ProfileAuthority,
        input: ReconcileInterruptedCall,
    ) -> Result<InterruptedCallReconciliation, Error> {
        input.validate()?;
        check_owner(&profile, &self.0)?;
        let registry = self.0.clone();
        let after = profile.clone();
        let result = self
            .0
            .store
            .call(64 * 1024, move |db| {
                check_owner(&profile, &registry)?;
                if live(&registry, &input.call_id) {
                    return Err(Error::LocalAuthority);
                }
                let tx = db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let child: String = tx
                    .query_row(
                        "SELECT a.id FROM owner_managed_agents a JOIN owner_managed_calls c ON c.child_id=a.id \
                         WHERE a.profile=?1 AND a.agent_id=?2 AND c.id=?3",
                        params![profile.digest, input.agent_id, input.call_id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .ok_or(Error::Conflict)?;
                let h = calls::history(&tx, &child, &input.call_id)?;
                let unrepresentable: bool = tx.query_row(
                    "SELECT unrepresentable_usage FROM owner_managed_calls WHERE id=?1",
                    [&input.call_id],
                    |r| r.get(0),
                )?;
                let proof = json!({
                    "source":"owner_reconciliation",
                    "consumedTokens":input.consumed_tokens,
                    "expectedReservedTokens":input.expected_reserved_tokens,
                    "evidenceReference":input.evidence_reference,
                    "commandId":input.command_id,
                    "issueRevision":h.issue_revision,
                });
                let (digest, evidence) = observations::evidence("usage", &input.call_id, &proof)?;
                let id = format!("{}_owner_reconciliation", input.call_id);
                if let Some(old) =
                    observations::replay(&tx, &id, &child, &input.call_id, "usage", &digest)?
                {
                    return Ok(InterruptedCallReconciliation {
                        call_id: input.call_id,
                        reservation_id: child,
                        state: old.state,
                        consumed_tokens: old.known_consumed_tokens,
                        released_tokens: input.expected_reserved_tokens.saturating_sub(old.known_consumed_tokens),
                        observation_id: old.observation_id,
                        ledger_revision: old.ledger_revision,
                        replayed: true,
                    });
                }
                if !matches!(h.state.as_str(), "issued" | "unknown")
                    || h.usage_complete
                    || unrepresentable
                    || h.reserved_tokens != input.expected_reserved_tokens
                    || input.consumed_tokens < h.known_consumed_tokens
                {
                    return Err(Error::Conflict);
                }
                let changed = tx.execute(
                    &format!(
                        "UPDATE owner_managed_calls SET state='settled',known_consumed_tokens=?2,usage_complete=1,\
                         original_result_digest=COALESCE(original_result_digest,?3) WHERE id=?1 AND state IN {OPEN_STATES}"
                    ),
                    params![input.call_id, input.consumed_tokens, digest],
                )?;
                if changed != 1 {
                    return Err(Error::Conflict);
                }
                let grant: String = tx.query_row(
                    "SELECT COALESCE(a.pool_grant_id,p.grant_id) FROM owner_managed_agents a \
                     LEFT JOIN owner_project_reservations p ON p.id=a.project_reservation_id WHERE a.id=?1",
                    [&child],
                    |r| r.get(0),
                )?;
                grant_floor(&tx, &grant)?;
                let receipt = ManagedObservationReceipt {
                    observation_id: id,
                    target_id: input.call_id.clone(),
                    kind: "usage".into(),
                    state: "settled".into(),
                    known_consumed_tokens: input.consumed_tokens,
                    usage_complete: true,
                    ledger_revision: revision(&tx)?,
                    content_digest: digest,
                };
                observations::save_observation(&tx, &child, &receipt, &evidence)?;
                tx.commit()?;
                Ok(InterruptedCallReconciliation {
                    call_id: input.call_id,
                    reservation_id: child,
                    state: receipt.state,
                    consumed_tokens: input.consumed_tokens,
                    released_tokens: h.reserved_tokens.saturating_sub(input.consumed_tokens),
                    observation_id: receipt.observation_id,
                    ledger_revision: receipt.ledger_revision,
                    replayed: false,
                })
            })
            .await?;
        check_owner(&after, &self.0)?;
        Ok(result)
    }
}
