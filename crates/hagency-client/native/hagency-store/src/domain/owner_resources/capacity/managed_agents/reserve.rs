use super::*;

/// Octos pools bind one explicitly owner-selected local configuration. This
/// proves only the persisted mapping, not a third-party provider identity;
/// the original profile and private key are rechecked before process startup.
pub(super) fn admitted_account(db: &Connection, resource: &hagency_core::project::Resource) -> Result<bool, Error> {
    if resource.framework == "octos" {
        let Some(profile) = resource.octos_profile.as_deref() else { return Ok(false); };
        if resource.provider.as_deref() != Some(hagency_core::qualification::OWNER_ENDPOINT) {
            return Ok(false);
        }
        return Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_accounts a JOIN owner_octos_profiles p ON p.profile_digest=a.provider_digest JOIN owner_octos_model_resources m ON m.profile_digest=p.profile_digest JOIN owner_octos_key k ON k.singleton=1 WHERE a.seat_id=?1 AND a.identity_kind='explicit_owner_mapping' AND p.profile_id=?2 AND m.model=?3 AND m.resource_id=?4 AND length(m.configuration_digest)=64 AND length(k.commitment)=64)",
            params![resource.seat_id, profile, resource.model, resource.id()], |r| r.get(0))?);
    }
    Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_accounts WHERE seat_id=?1 AND identity_kind='verified_provider')",
        [&resource.seat_id], |r| r.get(0))?)
}

#[cfg(test)]
mod octos_account_tests {
    use super::*;
    #[test]
    fn octos_reservation_requires_its_own_explicit_profile_mapping_and_key_commitment() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE owner_resource_accounts(provider_digest TEXT, seat_id TEXT, identity_kind TEXT);
            CREATE TABLE owner_octos_profiles(profile_digest TEXT, profile_id TEXT, model TEXT, configuration_digest TEXT);
            CREATE TABLE owner_octos_model_resources(profile_digest TEXT, model TEXT, resource_id TEXT, configuration_digest TEXT);
            CREATE TABLE owner_octos_key(singleton INTEGER, commitment TEXT);").unwrap();
        let resource: hagency_core::project::Resource = serde_json::from_value(json!({"presetId":"preset", "seatId":"seat",
            "framework":"octos", "model":"local-model", "provider":"owner_endpoint", "octosProfile":"coding"})).unwrap();
        db.execute("INSERT INTO owner_resource_accounts VALUES('profile','seat','explicit_owner_mapping')", []).unwrap();
        db.execute("INSERT INTO owner_octos_profiles VALUES('profile','coding','local-model',?1)", [&"a".repeat(64)]).unwrap();
        db.execute("INSERT INTO owner_octos_model_resources VALUES('profile','local-model',?1,?2)", params![resource.id(), "a".repeat(64)]).unwrap();
        assert!(!admitted_account(&db, &resource).unwrap());
        db.execute("INSERT INTO owner_octos_key VALUES(1,?1)", [&"b".repeat(64)]).unwrap();
        assert!(admitted_account(&db, &resource).unwrap());
        let mut wrong = resource.clone();
        wrong.octos_profile = Some("other".into());
        assert!(!admitted_account(&db, &wrong).unwrap());
        wrong = resource.clone(); wrong.model = "other-model".into();
        assert!(!admitted_account(&db, &wrong).unwrap());
        wrong = resource.clone(); wrong.provider = None;
        assert!(!admitted_account(&db, &wrong).unwrap());
        wrong = resource.clone(); wrong.framework = "codex".into(); wrong.octos_profile = None;
        assert!(!admitted_account(&db, &wrong).unwrap());
    }
}

/// Write only an already checked original project command. Never overwrite an
/// existing authority row or reconstruct a missing native reservation.
pub(in super::super) fn save_project_authority(
    db: &Connection,
    c: &AllocationCommand,
    digest: &str,
) -> Result<(), Error> {
    let encoded = canonical::encode(&serde_json::to_value(c)?)?;
    let old: Option<(String,String)> = db.query_row(
        "SELECT reserve_digest,reserve_command FROM owner_project_authorities WHERE allocation_id=?1",
        [&c.allocation_id], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional()?;
    if let Some((old_digest, old_command)) = old {
        if old_digest != digest || old_command != encoded {
            return Err(Error::Conflict);
        }
    } else {
        db.execute(
            "INSERT INTO owner_project_authorities VALUES(?1,?2,?3,?4,?5)",
            params![
                c.allocation_id,
                encoded,
                digest,
                c.manager_mxid,
                c.project_definition_digest
            ],
        )?;
    }
    Ok(())
}

fn authority(
    db: &Connection,
    d: &VerifiedManagedAgentDelivery,
    now: u64,
) -> Result<Option<&'static str>, Error> {
    let e = &d.enrollment;
    let c = &d.command;
    if c.pool_grant.is_some() {
        return pool_authority(db, d, now);
    }
    let original = d.project_command.as_ref().ok_or(Error::LocalAuthority)?;
    if c.validate(&e.profile.account).is_err()
        || validate_command(original).is_err()
        || original.operation != "project.reserve"
    {
        return Ok(Some("invalid_request"));
    }
    let original_digest = canonical::digest(&serde_json::to_value(original)?)?;
    let grant = &original.grant_receipt;
    if !e.matches(grant)
        || c.enrollment_id != e.enrollment_id
        || c.executor_device_id != e.executor_device_id
        || c.executor_generation != e.executor_generation
        || c.resource_id != e.resource_id
        || c.resource_revision != e.resource_revision
        || c.grant_receipt_digest != canonical::digest(&serde_json::to_value(grant)?)?
        || c.manager_mxid != original.manager_mxid
        || c.project_id != original.project_id
        || c.project_revision != original.project_revision
        || c.allocation_id != original.allocation_id
        || c.period_key != grant.period_key
        || now < c.issued_at_ms
        || now >= c.expires_at_ms
    {
        return Ok(Some("authority_changed"));
    }
    // Native verifies original coordinator authority by its existing durable
    // command/receipt. Agent approval is the assigned project admin's decision;
    // do not reuse the coordinator self-approval policy for the new actor.
    let row: Option<(String,String,Option<String>,String,String)> = db.query_row(
        "SELECT grant_id,reserve_command_id,reserve_digest,binding_digest,state FROM owner_project_reservations WHERE id=?1",
        [&c.allocation_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)),
    ).optional()?;
    let Some((parent, original_id, saved_digest, binding, state)) = row else {
        return Ok(Some("project_unavailable"));
    };
    if parent != grant.grant_id
        || original_id != original.command_id
        || saved_digest.as_deref() != Some(&original_digest)
        || binding != child_binding(original)?
        || state != "reserved"
    {
        return Ok(Some("project_unavailable"));
    }
    let saved: Option<NativeAllocationReceipt> = receipt(
        db,
        &e.profile.digest,
        &e.executor_device_id,
        &original_id,
        &original_digest,
    )?;
    let Some(saved) = saved else {
        return Ok(Some("project_unavailable"));
    };
    if saved.state != "reserved"
        || saved.allocation_id != c.allocation_id
        || saved.reserved_tokens != original.requested_tokens
        || saved.retained_tokens != original.requested_tokens
        || c.allocation_receipt_digest != canonical::digest(&serde_json::to_value(&saved)?)?
    {
        return Ok(Some("project_unavailable"));
    }
    let row: Option<(String, String, bool, String)> = db
        .query_row(
            "SELECT profile,receipt,active,accounting_mode FROM owner_capacity_grants WHERE id=?1",
            [&grant.grant_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((profile, grant_json, active, mode)) = row else {
        return Ok(Some("authority_changed"));
    };
    let closed: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_parent_settlements WHERE grant_id=?1)",
        [&grant.grant_id],
        |r| r.get(0),
    )?;
    if profile != e.profile.digest
        || !active
        || closed
        || !matches!(mode.as_str(), parent::RESERVATION_ONLY | MANAGED)
        || canonical::digest(&serde_json::from_str::<serde_json::Value>(&grant_json)?)?
            != c.grant_receipt_digest
        || authorize_resource(db, &e.profile, &e.resource_id).is_err()
        || resource_revision(db, &e.resource_id)? != e.resource_revision
        || now < grant.period_start_ms
        || now >= grant.period_end_ms
        || now >= grant.delegation_expires_at_ms
    {
        return Ok(Some("authority_changed"));
    }
    let mode: String = db.query_row(
        "SELECT accounting_mode FROM owner_project_reservations WHERE id=?1",
        [&c.allocation_id],
        |r| r.get(0),
    )?;
    if !matches!(mode.as_str(), parent::RESERVATION_ONLY | MANAGED) {
        return Ok(Some("unsettled_usage"));
    }
    let resource = read_resource(db, &e.resource_id)?;
    if !admitted_account(db, &resource)?
        || !hagency_core::qualification::qualifies(
            &resource.profile(),
            &c.definition.capability,
            None,
        )
    {
        return Ok(Some("resource_unavailable"));
    }
    if unsettled_resource(db, &resource)? {
        return Ok(Some("unsettled_usage"));
    }
    if let Some(reason) = crate::domain::web_catalog::managed_offer_refusal(
        db, &e.profile.digest, &c.definition.capability, c.requested_tokens,
    )? {
        return Ok(Some(reason));
    }
    let duplicate: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM owner_managed_agents WHERE profile=?1 AND (request_id=?2 OR agent_id=?3))",params![e.profile.digest,c.request_id,c.proposed_agent_id],|r| r.get(0))?;
    if duplicate {
        return Ok(Some("request_conflict"));
    }
    let held = accounting::children(db, &c.allocation_id)?.retained;
    if c.requested_tokens > original.requested_tokens.saturating_sub(held) {
        return Ok(Some("insufficient_capacity"));
    }
    Ok(None)
}

fn pool_authority(db: &Connection, d: &VerifiedManagedAgentDelivery, now: u64) -> Result<Option<&'static str>, Error> {
    let e = &d.enrollment;
    let c = &d.command;
    if c.validate(&e.profile.account).is_err() || d.project_command.is_some() {
        return Ok(Some("invalid_request"));
    }
    let grant = c.pool_grant.as_ref().ok_or(Error::LocalAuthority)?;
    if !e.matches(grant) || now < c.issued_at_ms || now >= c.expires_at_ms
        || now < grant.period_start_ms || now >= grant.period_end_ms
        || now >= grant.delegation_expires_at_ms {
        return Ok(Some("authority_changed"));
    }
    let saved: Option<(String, String, bool, String)> = db.query_row(
        "SELECT profile,receipt,active,accounting_mode FROM owner_capacity_grants WHERE id=?1",
        [&grant.grant_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)),
    ).optional()?;
    let Some((profile, receipt, active, mode)) = saved else { return Ok(Some("authority_changed")) };
    // The command's grant must govern its chain natively now. A successor the
    // server accepted before this store observed it is not answered yet: the
    // server redelivers after its lease, once publication observation flips it.
    match super::super::succession::row(db, &grant.grant_id)? {
        Some(link) if link.state == super::super::succession::State::Pending => return Err(Error::Busy),
        Some(link) if link.state != super::super::succession::State::Effective || link.effective_from_ms > now => {
            return Ok(Some("authority_changed"));
        }
        _ => {}
    }
    let next = super::super::succession::live_successor(db, &grant.grant_id)?;
    if next.as_ref().is_some_and(|s| s.amendment() && s.state == super::super::succession::State::Effective && s.effective_from_ms <= now) {
        return Ok(Some("authority_changed"));
    }
    let closed: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM owner_parent_settlements WHERE grant_id=?1)",
        [&grant.grant_id], |r| r.get(0))?;
    if profile != e.profile.digest || !active || closed
        || !matches!(mode.as_str(), parent::RESERVATION_ONLY | MANAGED)
        || canonical::digest(&serde_json::from_str::<serde_json::Value>(&receipt)?)? != c.grant_receipt_digest
        || authorize_resource(db,&e.profile,&e.resource_id).is_err()
        || resource_revision(db,&e.resource_id)? != e.resource_revision {
        return Ok(Some("authority_changed"));
    }
    let resource = read_resource(db,&e.resource_id)?;
    if !admitted_account(db, &resource)? || !hagency_core::qualification::qualifies(&resource.profile(),&c.definition.capability,None) {
        return Ok(Some("resource_unavailable"));
    }
    let unsettled = if periods::recurring(c) {
        accounting::unsettled_resource_except(db,&resource,&super::super::succession::chain(db,&grant.grant_id)?)?
    } else { unsettled_resource(db,&resource)? };
    if unsettled { return Ok(Some("unsettled_usage")); }
    if let Some(reason) = crate::domain::web_catalog::managed_offer_refusal(db,&e.profile.digest,&c.definition.capability,c.requested_tokens)? {
        return Ok(Some(reason));
    }
    if let Some(reason) = generation_authority(db, &e.profile.digest, c)? { return Ok(Some(reason)); }
    // Every hold of this month's chain stays inside the governing grant.
    let floor = accounting::period_floor(db, &grant.grant_id, now)?;
    if c.requested_tokens > grant.request_limit()
        || c.requested_tokens > grant.allocated_tokens.saturating_sub(floor.total.retained) {
        return Ok(Some("insufficient_capacity"));
    }
    // With a pending amendment or a scheduled renewal the new Agent must also
    // fit what that successor will cover after the change.
    if let Some(next) = next {
        let successor: u64 = db.query_row(
            "SELECT allocated_tokens FROM owner_capacity_grants WHERE id=?1",
            [&next.grant_id],
            |r| r.get(0),
        )?;
        let covered = if next.amendment() {
            floor.total.retained
        } else {
            accounting::accounted_floor(db, &grant.grant_id, now)?.moving.retained
        };
        if c.requested_tokens > successor.saturating_sub(covered) {
            return Ok(Some("insufficient_capacity"));
        }
    }
    Ok(None)
}

impl DomainRepository {
    pub(super) fn owner_managed_agent_reserve(
        &mut self,
        d: VerifiedManagedAgentDelivery,
        fence: Arc<Inner>,
    ) -> Result<ManagedAgentReservationReceipt, Error> {
        self.owner_managed_agent_reserve_at(d,fence,graphs::now_ms()?)
    }
    pub(super) fn owner_managed_agent_reserve_at(
        &mut self, d:VerifiedManagedAgentDelivery, fence:Arc<Inner>, now:u64,
    ) -> Result<ManagedAgentReservationReceipt, Error> {
        d.check(&fence)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let e = &d.enrollment;
        let c = &d.command;
        if let Some(old) = receipt(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &c.command_id,
            &d.digest,
        )? {
            d.check(&fence)?;
            return Ok(old);
        }
        let fenced:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_managed_fences WHERE profile=?1 AND ((agent_id=?2 AND allocation_generation=?4) OR request_id=?3))",params![e.profile.digest,c.proposed_agent_id,c.request_id,c.allocation_generation()],|r|r.get(0))?;
        let reason = if fenced {
            Some("authority_changed")
        } else {
            authority(&tx, &d, now)?
        };
        let mut result = ManagedAgentReservationReceipt {
            version: 1,
            command_id: c.command_id.clone(),
            command_digest: d.digest.clone(),
            request_id: c.request_id.clone(),
            agent_id: c.proposed_agent_id.clone(),
            allocation_id: c.allocation_id.clone(),
            agent_reservation_id: None,
            definition_digest: c.definition_digest.clone(),
            state: "refused".into(),
            budget_revision: 0,
            ledger_revision: 0,
            allocated_tokens: 0,
            retained_tokens: 0,
            period_key: c.period_key.clone(),
            reason: reason.map(str::to_owned),
        };
        if reason.is_none() {
            let id = format!("mar_{}", &d.digest[..40]);
            bounded_row(&tx, "owner_managed_agents", "id", &id, 100_000)?;
            let (project, pool, grant) = if let Some(g) = &c.pool_grant {
                (None, Some(g.grant_id.as_str()), g)
            } else {
                let original = d.project_command.as_ref().ok_or(Error::LocalAuthority)?;
                let original_digest = canonical::digest(&serde_json::to_value(original)?)?;
                save_project_authority(&tx, original, &original_digest)?;
                (Some(c.allocation_id.as_str()), None, &original.grant_receipt)
            };
            tx.execute("INSERT INTO owner_managed_agents(id,profile,agent_id,request_id,project_reservation_id,pool_grant_id,original_command_id,original_command_digest,original_command,definition_digest,allocated_tokens,budget_revision,state,allocation_generation) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,'reserved',?12)",params![id,e.profile.digest,c.proposed_agent_id,c.request_id,project,pool,c.command_id,d.digest,canonical::encode(&serde_json::to_value(c)?)?,c.definition_digest,c.requested_tokens,c.allocation_generation()])?;
            tx.execute("INSERT INTO owner_managed_effects(id,child_id,kind,state,fence,payload_digest) VALUES(?1,?2,'provision','pending',0,?3)",params![format!("{id}_provision"),id,d.digest])?;
            if c.pool_grant.is_some() {
                crate::domain::exec_policy::resource_defaults::inherit(&tx, &e.profile.digest, &c.resource_id, &c.proposed_agent_id, &id)?;
            }
            tx.execute(
                "UPDATE owner_project_reservations SET accounting_mode=?2 WHERE id=?1",
                params![project, MANAGED],
            )?;
            tx.execute(
                "UPDATE owner_capacity_grants SET accounting_mode=?2 WHERE id=?1",
                params![grant.grant_id, MANAGED],
            )?;
            result.agent_reservation_id = Some(id);
            result.state = "reserved".into();
            result.budget_revision = 1;
            result.allocated_tokens = c.requested_tokens;
            result.retained_tokens = c.requested_tokens;
        }
        result.ledger_revision = revision(&tx)?;
        if let Some(child) = &result.agent_reservation_id {
            periods::initial(&tx, child, c, &d.digest, result.ledger_revision)?;
        }
        save(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &c.command_id,
            &d.digest,
            &result,
        )?;
        d.check(&fence)?;
        tx.commit()?;
        d.check(&fence).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}

// Recovery appends an allocation generation; old immutable rows, effects and
// fences remain untouched and continue to account for their actual liabilities.
fn generation_authority(db: &Connection, profile: &str, c: &ManagedAgentReserveCommand) -> Result<Option<&'static str>, Error> {
    let duplicate:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM owner_managed_agents WHERE profile=?1 AND request_id=?2)",params![profile,c.request_id],|r|r.get(0))?;
    if duplicate {return Ok(Some("request_conflict"));}
    let prior:Option<(u64,bool,String)>=db.query_row("SELECT allocation_generation,permanent_fence,original_command FROM owner_managed_agents WHERE profile=?1 AND agent_id=?2 ORDER BY allocation_generation DESC LIMIT 1",params![profile,c.proposed_agent_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    match prior {
        None if c.allocation_generation()==1 => Ok(None),
        Some((generation,true,raw)) if generation.checked_add(1)==Some(c.allocation_generation()) && periods::recurring(c) => {
            let old:ManagedAgentReserveCommand=serde_json::from_str(&raw)?;
            let grant=c.pool_grant.as_ref().ok_or(Error::LocalAuthority)?;
            let funded:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM owner_managed_periods p JOIN owner_managed_agents a ON a.id=p.child_id WHERE a.profile=?1 AND a.agent_id=?2 AND p.start_ms<?4 AND p.end_ms>?3)",params![profile,c.proposed_agent_id,grant.period_start_ms,grant.period_end_ms],|r|r.get(0))?;
            if funded {return Ok(Some("period_already_authorized"));}
            if old.manager_mxid!=c.manager_mxid || old.room_id!=c.room_id || old.resource_id!=c.resource_id
                || old.requested_tokens!=c.requested_tokens
                || old.definition_digest!=c.definition_digest || c.decision_actor_mxid!=c.pool_grant.as_ref().ok_or(Error::LocalAuthority)?.owner_mxid {
                return Ok(Some("authority_changed"));
            }
            Ok(None)
        }
        _=>Ok(Some("request_conflict")),
    }
}
