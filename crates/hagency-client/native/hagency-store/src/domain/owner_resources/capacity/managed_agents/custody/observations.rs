use super::*;
use hagency_metering::{Framework, observation::UsageObservation};

/// Only a future native runtime adapter may construct these observations.
/// Numerical UsageObservation by itself is explicitly not provenance.
#[derive(Serialize)]
pub struct ManagedProvisionObservation {
    id: String,
    state: String,
    runtime_digest: Option<String>,
}
#[derive(Serialize)]
pub struct ManagedUsageObservation {
    id: String,
    usage: Option<UsageObservation>,
    terminal: bool,
    complete_capture: bool,
    unrepresentable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedObservationReceipt {
    pub observation_id: String,
    pub target_id: String,
    pub kind: String,
    pub state: String,
    pub known_consumed_tokens: u64,
    pub usage_complete: bool,
    pub ledger_revision: u64,
    pub content_digest: String,
}
impl ManagedProvisionObservation {
    pub(super) fn shared_text(
        id: String,
        observed: &hagency_runtime::owned::SharedTextProcessObservation,
        runtime_digest: String,
    ) -> Result<Self, Error> {
        if observed.state() != hagency_runtime::owned::ManagedProcessState::Qualified
            || !observed.source().is_live()
        {
            return Err(Error::LocalAuthority);
        }
        hash(&runtime_digest)?;
        Ok(Self {
            id,
            state: "qualified".into(),
            runtime_digest: Some(runtime_digest),
        })
    }

    #[cfg(test)]
    pub(crate) fn fixture(id: &str, state: &str, runtime_digest: Option<String>) -> Self {
        Self {
            id: id.into(),
            state: state.into(),
            runtime_digest,
        }
    }
}
impl ManagedUsageObservation {
    pub(super) fn shared_capture(
        id: String,
        evidence: Option<&hagency_runtime::codex::session::UsageEvidence>,
        terminal: bool,
        complete_capture: bool,
        unrepresentable: bool,
    ) -> Self {
        use hagency_metering::runtime_usage::{CodexUsage, CounterBreakdown, ProjectionDiagnostics};
        fn b(v: &hagency_runtime::codex::session::UsageBreakdown) -> CounterBreakdown {
            CounterBreakdown {
                total_tokens: v.total_tokens(),
                input_tokens: v.input_tokens(),
                cached_input_tokens: v.cached_input_tokens(),
                cache_write_input_tokens: v.cache_write_input_tokens(),
                output_tokens: v.output_tokens(),
                reasoning_output_tokens: v.reasoning_output_tokens(),
            }
        }
        let usage = evidence.map(|e| {
            let d = e.diagnostics();
            UsageObservation::codex_runtime(CodexUsage {
                total: b(e.total()),
                last: b(e.last()),
                context_window: e.model_context_window(),
                diagnostics: ProjectionDiagnostics {
                    missing: d.has_missing_fields(),
                    invalid: d.has_invalid_fields(),
                    unsupported: d.has_unsupported_fields(),
                },
            })
        });
        let failed = usage.as_ref().is_some_and(Result::is_err);
        Self {
            id,
            usage: usage.and_then(Result::ok),
            terminal,
            complete_capture,
            unrepresentable: unrepresentable || failed,
        }
    }

    pub(super) fn shared_claude_capture(
        id: String,
        evidence: &hagency_runtime::claude::session::UsageEvidence,
        terminal: bool,
        complete_capture: bool,
    ) -> Self {
        use hagency_metering::{
            TokenCounts,
            claude_usage::{ClaudeUsage, Coverage},
            runtime_usage::ProjectionDiagnostics,
        };
        use hagency_runtime::claude::session::UsageCoverage;
        let counts = evidence.counts();
        let diagnostics = evidence.diagnostics();
        let coverage = match evidence.coverage() {
            UsageCoverage::MainLoopSteps => Coverage::MainLoopSteps,
            UsageCoverage::MainLoopResult => Coverage::MainLoopResult,
            UsageCoverage::ReportedModelsResult => Coverage::ReportedModelsResult,
        };
        let usage = UsageObservation::claude_runtime(ClaudeUsage {
            counts: TokenCounts {
                input: counts.input(),
                output: counts.output(),
                cache_read: counts.cache_read(),
                cache_write: counts.cache_write(),
            },
            coverage,
            steps: evidence.steps(),
            models: evidence.models(),
            diagnostics: ProjectionDiagnostics {
                missing: diagnostics.has_missing_fields(),
                invalid: diagnostics.has_invalid_fields(),
                unsupported: diagnostics.has_unsupported_fields(),
            },
        });
        Self {
            id,
            terminal,
            complete_capture,
            unrepresentable: usage.is_err(),
            usage: usage.ok(),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture(
        id: &str,
        usage: Option<UsageObservation>,
        terminal: bool,
        complete_capture: bool,
        unrepresentable: bool,
    ) -> Self {
        Self {
            id: id.into(),
            usage,
            terminal,
            complete_capture,
            unrepresentable,
        }
    }
}
pub(super) fn evidence<T: Serialize>(
    kind: &str,
    target: &str,
    o: &T,
) -> Result<(String, String), Error> {
    let value = json!({"kind":kind,"target":target,"observation":o});
    if serde_json::to_vec(&value)?.len() > 16 * 1024 {
        return Err(Error::Capacity);
    }
    Ok((canonical::digest(&value)?, canonical::encode(&value)?))
}
pub(super) fn replay(
    db: &Connection,
    id: &str,
    child: &str,
    target: &str,
    kind: &str,
    digest: &str,
) -> Result<Option<ManagedObservationReceipt>, Error> {
    identifier(id, 128)?;
    let row:Option<(String,String,String,String,String)>=db.query_row("SELECT child_id,target_id,kind,content_digest,result FROM owner_managed_observations WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    if let Some((c, t, k, d, value)) = row {
        if c != child || t != target || k != kind || d != digest {
            return Err(Error::Conflict);
        }
        Ok(Some(serde_json::from_str(&value)?))
    } else {
        Ok(None)
    }
}
pub(super) fn save_observation(
    db: &Connection,
    child: &str,
    value: &ManagedObservationReceipt,
    evidence: &str,
) -> Result<(), Error> {
    bounded_row(
        db,
        "owner_managed_observations",
        "id",
        &value.observation_id,
        1_000_000,
    )?;
    db.execute(
        "INSERT INTO owner_managed_observations VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            value.observation_id,
            child,
            value.target_id,
            value.kind,
            value.content_digest,
            evidence,
            canonical::encode(&serde_json::to_value(value)?)?,
            value.ledger_revision
        ],
    )?;
    Ok(())
}
pub(super) fn check_scope(db: &Connection, s: &Scope, kind: &str) -> Result<(), Error> {
    let binding: String = db.query_row(
        "SELECT binding_digest FROM owner_managed_assignment_pins WHERE child_id=?1",
        [&s.child],
        |r| r.get(0),
    )?;
    let issued: u64 = if kind == "provision" {
        db.query_row(
            "SELECT fence FROM owner_managed_effects WHERE id=?1 AND child_id=?2",
            params![s.target, s.child],
            |r| r.get(0),
        )?
    } else {
        db.query_row("SELECT p.issue_revision FROM owner_managed_call_pins p JOIN owner_managed_calls c ON c.id=p.call_id WHERE c.id=?1 AND c.child_id=?2",params![s.target,s.child],|r|r.get(0))?
    };
    if binding != s.binding || issued != s.issued_revision {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}
pub(super) fn provision(
    db: &Connection,
    child: &str,
    target: &str,
    o: ManagedProvisionObservation,
) -> Result<ManagedObservationReceipt, Error> {
    if !matches!(o.state.as_str(), "qualified" | "failed" | "unknown") {
        return Err(Error::LocalAuthority);
    }
    if o.state == "qualified" {
        hash(o.runtime_digest.as_deref().ok_or(Error::LocalAuthority)?)?;
    } else if o.runtime_digest.is_some() {
        return Err(Error::LocalAuthority);
    }
    let (digest, evidence) = evidence("provision", target, &o)?;
    if let Some(old) = replay(db, &o.id, child, target, "provision", &digest)? {
        return Ok(old);
    }
    let old = provision::history(db, child)?;
    if old.effect_id != target
        || !matches!(old.state.as_str(), "issued" | "unknown")
        || old.fence_revision == 0
    {
        return Err(Error::LocalAuthority);
    }
    let result = ManagedObservationReceipt {
        observation_id: o.id,
        target_id: target.into(),
        kind: "provision".into(),
        state: o.state.clone(),
        known_consumed_tokens: 0,
        usage_complete: o.state != "unknown",
        ledger_revision: revision(db)?,
        content_digest: digest.clone(),
    };
    let value = canonical::encode(&serde_json::to_value(&result)?)?;
    db.execute("UPDATE owner_managed_effects SET state=?2,original_result=CASE WHEN ?2='unknown' THEN original_result ELSE ?3 END,original_result_digest=CASE WHEN ?2='unknown' THEN original_result_digest ELSE ?4 END WHERE id=?1",params![target,o.state,value,digest])?;
    save_observation(db, child, &result, &evidence)?;
    Ok(result)
}
/// The established codec splits Codex cache-inclusive input into fresh input,
/// cache read and cache write. The existing ceiling policy is fresh+write+output;
/// cache read is reported but never double-counted in that ceiling.
fn amounts(o: &ManagedUsageObservation) -> Result<(u64, bool, bool, bool), Error> {
    let mut overflow = o.unrepresentable;
    let mut known = 0_u64;
    let mut complete = false;
    let mut clean = false;
    if let Some(u) = &o.usage {
        if !matches!(u.framework(), Framework::Codex | Framework::Claude) {
            return Err(Error::LocalAuthority);
        }
        overflow |= matches!(
            u.failure(),
            Some(hagency_metering::observation::ParseFailure::Overflow)
        );
        if let Some(c) = u.counts() {
            for n in [c.input, c.output, c.cache_write].into_iter().flatten() {
                if let Some(sum) = known.checked_add(n).filter(|n| *n <= JSON_SAFE_MAX) {
                    known = sum;
                } else {
                    overflow = true;
                }
            }
            let valid = match u.framework() {
                Framework::Codex => u.runtime_evidence().is_some_and(|e| {
                    let d = e.usage().diagnostics;
                    let n = e.normalization();
                    !d.missing
                        && !d.invalid
                        && !d.unsupported
                        && !n.total_inconsistent
                        && !n.last_inconsistent
                }),
                Framework::Claude => u.claude_runtime_evidence().is_some_and(|e| {
                    let d = e.usage().diagnostics;
                    !d.missing
                        && !d.invalid
                        && !d.unsupported
                        && e.usage().coverage
                            == hagency_metering::claude_usage::Coverage::ReportedModelsResult
                }),
                _ => false,
            };
            clean = valid && u.failure().is_none();
            complete = o.terminal
                && o.complete_capture
                && !overflow
                && u.failure().is_none()
                && valid
                && [c.input, c.output, c.cache_write, c.cache_read]
                    .iter()
                    .all(Option::is_some);
        }
    }
    Ok((known, complete, overflow, clean))
}
pub(super) fn usage(
    db: &Connection,
    child: &str,
    target: &str,
    o: ManagedUsageObservation,
) -> Result<ManagedObservationReceipt, Error> {
    let (digest, evidence) = evidence("usage", target, &o)?;
    if let Some(old) = replay(db, &o.id, child, target, "usage", &digest)? {
        return Ok(old);
    }
    let h = calls::history(db, child, target)?;
    if h.issue_revision.is_none() || !matches!(h.state.as_str(), "issued" | "unknown" | "settled") {
        return Err(Error::LocalAuthority);
    }
    if let Some(usage) = &o.usage {
        // Use the immutable original command, not today's resource settings:
        // late usage must remain recordable after an owner edits a resource.
        let command: Option<String> = db
            .query_row(
                "SELECT command_payload FROM owner_shared_runtime_lifetimes WHERE child_id=?1",
                [child],
                |r| r.get(0),
            )
            .optional()?;
        let original = command
            .as_ref()
            .map(|raw| serde_json::from_str::<shared_execution::SharedRuntimeCommand>(raw))
            .transpose()?;
        // Pre-shared managed calls were Codex only; no historical row can
        // silently become a Claude source when this adapter is installed.
        let framework = original
            .as_ref()
            .map(|c| c.runtime())
            .transpose()?
            .unwrap_or("codex");
        if !matches!(
            (framework, usage.framework()),
            ("codex", Framework::Codex) | ("claude", Framework::Claude)
        ) {
            return Err(Error::LocalAuthority);
        }
    }
    let (observed, new_complete, overflow, clean) = amounts(&o)?;
    let old_overflow: bool = db.query_row(
        "SELECT unrepresentable_usage FROM owner_managed_calls WHERE id=?1",
        [target],
        |r| r.get(0),
    )?;
    let mut complete = !overflow
        && !old_overflow
        && ((new_complete && observed >= h.known_consumed_tokens)
            || (h.usage_complete
                && clean
                && observed <= h.known_consumed_tokens
                && !o.unrepresentable));
    let consumed = h.known_consumed_tokens.max(observed);
    // A physical stop prevents future spending; it does not prove the final
    // bill for work already dispatched. Keep missing/partial usage unknown.
    let mut state = if complete {
        "settled"
    } else if o.terminal || h.state != "issued" {
        "unknown"
    } else {
        "issued"
    };
    db.execute("UPDATE owner_managed_calls SET state=?2,known_consumed_tokens=?3,usage_complete=?4,unrepresentable_usage=?5 WHERE id=?1",params![target,state,consumed,complete,overflow||old_overflow])?;
    let grant:String=db.query_row("SELECT COALESCE(a.pool_grant_id,p.grant_id) FROM owner_managed_agents a LEFT JOIN owner_project_reservations p ON p.id=a.project_reservation_id WHERE a.id=?1",[child],|r|r.get(0))?;
    match grant_floor(db, &grant) {
        Ok(_) => {}
        Err(Error::Capacity) => {
            complete = false;
            state = "unknown";
            db.execute("UPDATE owner_managed_calls SET state='unknown',usage_complete=0,unrepresentable_usage=1 WHERE id=?1",[target])?;
        }
        Err(error) => return Err(error),
    }
    if complete {
        db.execute("UPDATE owner_managed_calls SET original_result_digest=?2 WHERE id=?1 AND original_result_digest IS NULL",params![target,digest])?;
    }
    let result = ManagedObservationReceipt {
        observation_id: o.id,
        target_id: target.into(),
        kind: "usage".into(),
        state: state.into(),
        known_consumed_tokens: consumed,
        usage_complete: complete,
        ledger_revision: revision(db)?,
        content_digest: digest.clone(),
    };
    save_observation(db, child, &result, &evidence)?;
    Ok(result)
}

/// Whether the process that ran this call left recorded stopped evidence.
fn process_stopped(db: &Connection, call: &str) -> Result<bool, Error> {
    Ok(db.query_row(
        "SELECT CASE WHEN p.attempt_id IS NOT NULL \
         THEN EXISTS(SELECT 1 FROM owner_shared_successor_observations o WHERE o.attempt_id=p.attempt_id AND json_extract(o.payload,'$.state')='stopped') \
         ELSE EXISTS(SELECT 1 FROM owner_shared_process_observations o WHERE o.child_id=c.child_id AND json_extract(o.payload,'$.state')='stopped') END \
         FROM owner_managed_calls c LEFT JOIN owner_shared_successor_call_pins p ON p.call_id=c.id WHERE c.id=?1",
        [call],
        |r| r.get(0),
    )?)
}

/// Mark stopped issued calls as awaiting accounting. Preserve their known
/// lower bound and all original receipts; a stop is never a zero-usage proof.
/// Postpaid successor admission is independent of usage completeness.
pub(super) fn close_stopped_calls(db: &Connection, child: &str) -> Result<(), Error> {
    let mut q = db.prepare(
        "SELECT id FROM owner_managed_calls WHERE child_id=?1 AND state='issued' ORDER BY rowid",
    )?;
    let open = q
        .query_map([child], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for call in open {
        if !process_stopped(db, &call)? {
            continue;
        }
        let h = calls::history(db, child, &call)?;
        let proof = json!({
            "source":"process_stopped_usage_incomplete",
            "knownConsumedTokens":h.known_consumed_tokens,
            "issueRevision":h.issue_revision,
        });
        let (digest, evidence) = evidence("usage", &call, &proof)?;
        db.execute(
            "UPDATE owner_managed_calls SET state='unknown',usage_complete=0 WHERE id=?1 AND state='issued'",
            params![call],
        )?;
        let receipt = ManagedObservationReceipt {
            observation_id: format!("{call}_stopped_usage_unknown"),
            target_id: call,
            kind: "usage".into(),
            state: "unknown".into(),
            known_consumed_tokens: h.known_consumed_tokens,
            usage_complete: false,
            ledger_revision: revision(db)?,
            content_digest: digest,
        };
        save_observation(db, child, &receipt, &evidence)?;
    }
    Ok(())
}

/// Called only after the retained runtime atomically sealed its unstarted
/// turn. Missing provider observations alone can never reach this path.
pub(super) fn not_dispatched(
    db: &Connection,
    scope: &Scope,
) -> Result<ManagedObservationReceipt, Error> {
    check_scope(db, scope, "usage")?;
    let id = format!("{}_not_dispatched", scope.target);
    let proof = json!({"source":"native_not_dispatched","issueRevision":scope.issued_revision,"dispatch":"sealed_before_turn"});
    let (digest, evidence) = evidence("usage", &scope.target, &proof)?;
    if let Some(old) = replay(db, &id, &scope.child, &scope.target, "usage", &digest)? {
        return Ok(old);
    }
    let h = calls::history(db, &scope.child, &scope.target)?;
    let observed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_managed_observations WHERE target_id=?1)",
        [&scope.target],
        |r| r.get(0),
    )?;
    if h.state != "issued" || h.known_consumed_tokens != 0 || h.usage_complete || observed {
        return Err(Error::Conflict);
    }
    db.execute("UPDATE owner_managed_calls SET state='settled',usage_complete=1,original_result_digest=?2 WHERE id=?1", params![scope.target,digest])?;
    let grant:String=db.query_row("SELECT COALESCE(a.pool_grant_id,p.grant_id) FROM owner_managed_agents a LEFT JOIN owner_project_reservations p ON p.id=a.project_reservation_id WHERE a.id=?1",[&scope.child],|r|r.get(0))?;
    grant_floor(db, &grant)?;
    let receipt = ManagedObservationReceipt {
        observation_id: id,
        target_id: scope.target.clone(),
        kind: "usage".into(),
        state: "settled".into(),
        known_consumed_tokens: 0,
        usage_complete: true,
        ledger_revision: revision(db)?,
        content_digest: digest,
    };
    save_observation(db, &scope.child, &receipt, &evidence)?;
    Ok(receipt)
}
impl OwnerResourceRegistry {
    /// Exact immutable observation recovery. This neither applies evidence nor
    /// reconstructs a provision/call custody token from stored history.
    pub async fn recover_managed_observation(
        &self,
        a: ManagedCleanupAuthority,
        id: String,
        digest: String,
    ) -> Result<Option<ManagedObservationReceipt>, Error> {
        a.check(&self.0)?;
        identifier(&id, 128)?;
        hash(&digest)?;
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let registry = self.0.clone();
        let result=self.0.store.call(8192,move |db| {
            a.check(&registry)?;cleanup(&db.db,&a)?;
            let row:Option<(String,String,String)>=db.db.query_row("SELECT child_id,content_digest,result FROM owner_managed_observations WHERE id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let result=if let Some((child,saved,value))=row {
                if child!=a.child {return Err(Error::LocalAuthority);}
                if saved!=digest {return Err(Error::Conflict);}
                Some(serde_json::from_str(&value)?)
            } else {None};
            a.check(&registry)?;Ok(result)
        }).await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
    /// A permanent execution fence is independent of consumed/unknown liability.
    /// This method never claims that an already issued provider process stopped.
    pub async fn fence_managed_agent(
        &self,
        a: ManagedCleanupAuthority,
        command: String,
    ) -> Result<ManagedObservationReceipt, Error> {
        a.check(&self.0)?;
        identifier(&command, 128)?;
        let p = a.enrollment.profile.clone();
        let expires = a.deadline();
        let r = self.0.clone();
        let result=self.0.store.call(8192,move |db| {
            a.check(&r)?;let tx=db.db.transaction_with_behavior(TransactionBehavior::Immediate)?;cleanup(&tx,&a)?;
            let original=json!({"operation":"agent.fence","child":a.child,"command":command});
            let digest=canonical::digest(&original)?;
            let evidence=canonical::encode(&original)?;
            if let Some(old)=replay(&tx,&command,&a.child,&a.child,"fence",&digest)? {a.check(&r)?;return Ok(old);}
            tx.execute("UPDATE owner_managed_agents SET state='fenced',permanent_fence=1 WHERE id=?1",[&a.child])?;
            tx.execute("UPDATE owner_managed_calls SET state='cancelled_before_issue' WHERE child_id=?1 AND state='prepared'",[&a.child])?;
            let (held,unknown)=match call_budget_all(&tx,&a.child) { Ok(v)=>v, Err(Error::Capacity)=>(JSON_SAFE_MAX,true), Err(error)=>return Err(error) };
            let (consumed,overflow)=known_consumed(&tx,&a.child)?;
            let provision=provision::history(&tx,&a.child)?;
            let provision_unknown=matches!(provision.state.as_str(),"issued"|"unknown");
            let result=ManagedObservationReceipt {observation_id:command,target_id:a.child.clone(),kind:"fence".into(),state:"fenced".into(),known_consumed_tokens:consumed,usage_complete:!unknown&&!overflow&&!provision_unknown&&held==consumed,ledger_revision:revision(&tx)?,content_digest:digest};
            save_observation(&tx,&a.child,&result,&evidence)?;a.check(&r)?;tx.commit()?;a.check(&r).map_err(|_|Error::OutcomeUnknown)?;Ok(result)
        }).await?;
        after(&p, &self.0, expires)?;
        Ok(result)
    }
}

fn known_consumed(db: &Connection, child: &str) -> Result<(u64, bool), Error> {
    let mut query =
        db.prepare("SELECT known_consumed_tokens FROM owner_managed_calls WHERE child_id=?1")?;
    let rows = query.query_map([child], |r| r.get::<_, u64>(0))?;
    let mut consumed = 0_u64;
    let mut overflow = false;
    for (count, row) in rows.enumerate() {
        if count >= 100_000 {
            return Err(Error::Capacity);
        }
        if let Some(value) = consumed.checked_add(row?).filter(|n| *n <= JSON_SAFE_MAX) {
            consumed = value;
        } else {
            consumed = JSON_SAFE_MAX;
            overflow = true;
        }
    }
    Ok((consumed, overflow))
}
