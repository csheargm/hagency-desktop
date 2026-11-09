//! Rinx ADR 0011: a delegated coordinator decision is the capacity verdict.
//! Native Matrix request verification and resource provisioning remain required.
//! All authority, resource reservations and receipts use the domain writer.
use super::*;
use palpo_hagency_contract as contract;
type StoredResourceGrant = (
    String,
    String,
    String,
    String,
    u64,
    u64,
    String,
    String,
    String,
);
mod delegations;
mod deliveries;
mod lifecycle;
mod audit;
mod project_setup;
mod refusals;
mod settlements;
pub use delegations::{DelegationChange, DelegationCommand, DelegationState};
pub use deliveries::terminal_reason as coordinator_refusal_reason;
pub use lifecycle::{AgentControl, AgentOperation};
pub use project_setup::{ProjectSetupCommand, ProjectSetupWork};
pub use settlements::{FinalUsage, SettlementCommand};

pub use contract::{
    AgentApproval, ProjectApproval, ProjectGrant, ServerEngagement, TokenTopUpApproval,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceGrant {
    pub id: contract::ResourceAllocationId,
    pub server_engagement_id: contract::ServerEngagementId,
    pub resource_id: String,
    pub revision: contract::Revision,
    pub allocated_tokens: contract::Tokens,
    pub eligible_managers: Vec<contract::MatrixUserId>,
}

/// Owned, non-serializable console permission retained through the writer commit.
pub struct ResourceContributionCommand {
    pub(super) grant: ResourceGrant,
    pub(super) gate: crate::ResourcePublicationCommand,
}
impl ResourceContributionCommand {
    pub(crate) fn weight(&self) -> Result<u32, Error> {
        u32::try_from(serialize(&self.grant)?.len() + 256).map_err(|_| Error::Capacity)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProjectDefinition {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    pub room_id: String,
    pub owner_dm_room_id: String,
}

/// Native Matrix observations, never deserialized from an incoming command.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectReadiness {
    pub registration: Registration,
    pub project_id: String,
    pub observed_at_ms: u64,
    pub project: hagency_core::authority::RoomObservation,
    pub owner_room: hagency_core::authority::RoomObservation,
}

fn publish(db: &Connection, fleet: &str, id: &str, mut payload: Value) -> Result<(), Error> {
    let authority = binding(db, fleet)?.ok_or(Error::LocalAuthority)?;
    payload["registrationGeneration"] = json!(authority.registration_generation);
    payload["delegationRevision"] = json!(authority.delegation_revision);
    let digest = canonical::digest(&payload)?;
    db.execute("INSERT INTO coordinator_publications(engagement_id,id,digest,payload) VALUES(?1,?2,?3,?4) ON CONFLICT(engagement_id,id) DO UPDATE SET digest=excluded.digest,payload=excluded.payload", params![fleet,id,digest,serialize(&payload)?])?;
    Ok(())
}

fn publish_project(db: &Connection, grant: &ProjectGrant) -> Result<(), Error> {
    publish(
        db,
        grant.server_engagement_id.as_str(),
        &format!("project_{}", grant.project_id.as_str()),
        json!({"kind":"project","project":grant}),
    )
}

fn policy(error: contract::Error) -> Error {
    match error {
        contract::Error::Forbidden | contract::Error::SelfApproval => Error::LocalAuthority,
        contract::Error::BindingMismatch
        | contract::Error::Expired
        | contract::Error::EngagementUnavailable => Error::Generation,
        contract::Error::ProjectUnavailable => Error::State,
        contract::Error::ResourceNotGranted => Error::Unqualified,
        contract::Error::InsufficientCapacity | contract::Error::Unallocated => {
            Error::InsufficientCapacity
        }
        _ => Error::Invalid(InvalidInput("coordinator command refused")),
    }
}

pub(super) fn binding(db: &Connection, fleet: &str) -> Result<Option<ServerEngagement>, Error> {
    db.query_row(
        "SELECT authority FROM coordinator_engagements WHERE id=?1",
        [fleet],
        |r| r.get::<_, String>(0),
    )
    .optional()?
    .map(|raw| Ok(serde_json::from_str(&raw)?))
    .transpose()
}

fn current(db: &Connection, fleet: &str, now: u64) -> Result<ServerEngagement, Error> {
    let value = binding(db, fleet)?.ok_or(Error::LocalAuthority)?;
    let raw: String = db.query_row(
        "SELECT config FROM registrations WHERE fleet_id=?1",
        [fleet],
        |r| r.get(0),
    )?;
    let registration: Registration = serde_json::from_str(&raw)?;
    if value.state != contract::EngagementState::Verified
        || !value.coordinator_approval_v1
        || value.delegation_expires_at_ms <= now
        || u64::from(value.registration_generation) != registration.generation
        || value.server.as_str() != registration.server_name
        || registration.reception_room_id.is_empty()
    {
        return Err(Error::Generation);
    }
    Ok(value)
}

pub(super) fn verified_after_probe(
    db: &Connection,
    fleet: &str,
    generation: u64,
) -> Result<(), Error> {
    if let Some(mut engagement) = binding(db, fleet)?
        && u64::from(engagement.registration_generation) == generation
        && matches!(
            engagement.state,
            contract::EngagementState::Approved
                | contract::EngagementState::Configuring
                | contract::EngagementState::Verifying
        )
    {
        engagement.state = contract::EngagementState::Verified;
        db.execute(
            "UPDATE coordinator_engagements SET authority=?2 WHERE id=?1",
            params![fleet, serialize(&engagement)?],
        )?;
    }
    Ok(())
}

fn period(db: &Connection, resource: &Resource, now: u64) -> Result<(String, String), Error> {
    let millis = i64::try_from(now).map_err(|_| InvalidInput("clock exceeds storage"))?;
    let date = time::OffsetDateTime::from_unix_timestamp(millis / 1000)
        .map_err(|_| InvalidInput("invalid budget clock"))?
        .date();
    let daily = super::resource_ceiling::effective_ceiling(db, resource)?
        .as_ref()
        .map(|c| serde_json::to_value(&c.period))
        .transpose()?
        == Some(json!("daily"));
    Ok(if daily {
        (
            "daily".into(),
            format!(
                "{:04}-{:02}-{:02}",
                date.year(),
                date.month() as u8,
                date.day()
            ),
        )
    } else {
        (
            "monthly".into(),
            format!("{:04}-{:02}", date.year(), date.month() as u8),
        )
    })
}

// Unmeasured and unsettled agents retain their allocation. A terminal state is
// not permission to refund its spend or an in-flight turn's unused reservation.
fn held(db: &Connection, grant: &str) -> Result<(u64, u64), Error> {
    let mut query = db.prepare("SELECT c.agent_id,c.retained_tokens,e.state,COALESCE(e.allocated_tokens,e.tokens) FROM coordinator_agents c JOIN engagements e ON e.id=c.agent_id WHERE c.resource_allocation_id=?1")?;
    let mut retained = 0u64;
    let mut active = 0u64;
    for row in query.query_map([grant], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, u64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u64>(3)?,
        ))
    })? {
        let (agent, amount, state, allocated) = row?;
        let amount = amount.max(quota_holds::spend(db, &agent)?.unwrap_or(0));
        retained = retained.checked_add(amount).ok_or(Error::Capacity)?;
        if state == "reserved" || state == "active" {
            active = active.checked_add(allocated).ok_or(Error::Capacity)?;
        }
    }
    Tokens::try_from(retained)?;
    Ok((retained, active))
}

/// Existing agent commitments already count active child allocations. Add only
/// the uncounted part of every contribution so the parent reserves it once.
/// A coordinator may spend its own unused grant; other entry points may not.
pub(super) fn additional_commitments(
    db: &Connection,
    resource: &Resource,
    within: Option<&str>,
) -> Result<Vec<allocation::Commitment>, Error> {
    let mut query = db.prepare("SELECT id,preset_id,seat_id,allocated_tokens FROM coordinator_resources WHERE preset_id=?1 OR seat_id=?2")?;
    let rows = query.query_map(params![resource.preset_id, resource.seat_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u64>(3)?,
        ))
    })?;
    let mut result = Vec::new();
    for row in rows {
        let (id, preset, seat, allocated) = row?;
        let (retained, active) = held(db, &id)?;
        let credit = if within == Some(id.as_str()) {
            allocated.saturating_sub(retained)
        } else {
            0
        };
        let additional = allocated
            .max(retained)
            .saturating_sub(active)
            .saturating_sub(credit);
        if additional > 0 {
            result.push(allocation::Commitment {
                id: format!("contribution_{id}"),
                preset_id: Some(preset),
                seat_id: Some(seat),
                allocated_tokens: Some(additional.try_into()?),
                state: "active".into(),
                fulfillment: None,
            });
        }
    }
    Ok(result)
}

pub(super) fn unused_grant(db: &Connection, id: &str, resource: &Resource) -> Result<u64, Error> {
    let amount: u64 = db.query_row("SELECT allocated_tokens FROM coordinator_resources WHERE id=?1 AND resource_id=?2 AND preset_id=?3 AND seat_id=?4", params![id,resource.id(),resource.preset_id,resource.seat_id], |r|r.get(0)).optional()?.ok_or(Error::LocalAuthority)?;
    amount
        .checked_sub(held(db, id)?.0)
        .ok_or(Error::InsufficientCapacity)
}

pub(super) fn check_agent(
    db: &Connection,
    command: &AgentApproval,
    proof: &VerifiedRequest,
    now: u64,
) -> Result<(), Error> {
    if db
        .query_row(
            "SELECT state FROM coordinator_deliveries WHERE id=?1",
            [command.context.command_id.as_str()],
            |r| r.get::<_, String>(0),
        )
        .optional()?
        .as_deref()
        == Some("refused")
    {
        return Err(Error::State);
    }
    let legacy = proof.request();
    let authority = current(db, &legacy.fleet_id, now)?;
    let raw: String = db
        .query_row(
            "SELECT grant FROM coordinator_projects WHERE id=?1 AND engagement_id=?2",
            params![legacy.target_project_id, legacy.fleet_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let project: ProjectGrant = serde_json::from_str(&raw)?;
    contract::authorize_agent_approval(
        command,
        &command.request,
        &authority,
        &project,
        &command.context.actor,
        now,
    )
    .map_err(policy)?;
    let expected_digest: String = command.request.definition_digest.clone().into();
    if command.request.id.as_str() != legacy.request_id
        || command.request.project_id.as_str() != legacy.target_project_id
        || command.request.project_owner.as_str() != legacy.owner_mxid
        || command.request.requester.as_str() != legacy.requester_mxid
        || u64::from(command.request.requested_tokens) != u64::from(legacy.requested_tokens)
        || expected_digest != canonical::digest(&serde_json::to_value(legacy)?)?
    {
        return Err(Error::Conflict);
    }
    let grant = command.request.resource_allocation_id.as_str();
    let (fleet, parent, preset, seat, allocated, granularity, key, managers): (String,String,String,String,u64,String,String,String) = db.query_row(
        "SELECT engagement_id,resource_id,preset_id,seat_id,allocated_tokens,period,period_key,managers FROM coordinator_resources WHERE id=?1", [grant],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).optional()?.ok_or(Error::NotFound)?;
    let managers: Vec<contract::MatrixUserId> = serde_json::from_str(&managers)?;
    let resource = read_resource(db, &parent)?;
    if fleet != legacy.fleet_id
        || parent != legacy.agent_definition.resource_id
        || resource.preset_id != preset
        || resource.seat_id != seat
        || period(db, &resource, now)? != (granularity, key)
        || !managers.contains(&command.request.project_owner)
    {
        return Err(Error::LocalAuthority);
    }
    // On replay the already-held reservation is validated by the immutable
    // receipt, not charged a second time.
    if replay(
        db,
        command.context.command_id.as_str(),
        &command_digest(command)?,
    )?
    .is_some()
    {
        return Ok(());
    }
    let remaining = allocated
        .checked_sub(held(db, grant)?.0)
        .ok_or(Error::InsufficientCapacity)?;
    if u64::from(command.allocated_tokens) > remaining {
        return Err(Error::InsufficientCapacity);
    }
    Ok(())
}

pub(super) fn command_digest(command: &AgentApproval) -> Result<String, Error> {
    Ok(canonical::digest(
        &json!({"operation":"coordinator_agent_approval","command":command}),
    )?)
}
pub(super) fn replay(db: &Connection, id: &str, digest: &str) -> Result<Option<Engagement>, Error> {
    let row: Option<(String, String)> = db
        .query_row(
            "SELECT digest,receipt FROM coordinator_commands WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    row.map(|(stored, receipt)| {
        if stored != digest {
            Err(Error::Conflict)
        } else {
            Ok(serde_json::from_str(&receipt)?)
        }
    })
    .transpose()
}
pub(super) fn commit_agent(
    tx: &Transaction<'_>,
    command: &AgentApproval,
    value: &Engagement,
) -> Result<(), Error> {
    let id = command.context.command_id.as_str();
    bounded_row(tx, "coordinator_commands", "id", id, 10000)?;
    tx.execute("INSERT INTO coordinator_agents(agent_id,resource_allocation_id,allocated_tokens,retained_tokens,command_id,decision) VALUES(?1,?2,?3,?3,?4,?5)", params![value.id,command.request.resource_allocation_id.as_str(),u64::from(command.allocated_tokens),id,serialize(command)?])?;
    tx.execute(
        "INSERT INTO coordinator_commands(id,engagement_id,digest,receipt) VALUES(?1,?2,?3,?4)",
        params![
            id,
            command.context.server_engagement_id.as_str(),
            command_digest(command)?,
            serialize(value)?
        ],
    )?;
    publish(
        tx,
        command.context.server_engagement_id.as_str(),
        &format!("command_{id}"),
        json!({"kind":"receipt","commandId":id,"commandDigest":command_digest(command)?,"agentId":value.id,"state":"applied"}),
    )?;
    tx.execute(
        "UPDATE coordinator_deliveries SET state='applied',agent_id=?2,reason=NULL WHERE id=?1",
        params![id, value.id],
    )?;
    Ok(())
}

impl DomainRepository {
    pub fn server_engagements(&self, after: &str, limit: usize) -> Result<Vec<Value>, Error> {
        if limit == 0 || limit > 50 {
            return Err(Error::Capacity);
        }
        let mut query = self.db.prepare(
            "SELECT authority FROM coordinator_engagements WHERE id>?1 ORDER BY id LIMIT ?2",
        )?;
        let mut result = Vec::new();
        for row in query.query_map(params![after, limit], |r| r.get::<_, String>(0))? {
            let policy: ServerEngagement = serde_json::from_str(&row?)?;
            let exports: Option<String> = self.db.query_row("SELECT change FROM coordinator_delegations WHERE engagement_id=?1 ORDER BY revision DESC LIMIT 1", [policy.id.as_str()], |r| r.get(0)).optional()?;
            let exports: Value = exports
                .map(|s| serde_json::from_str::<Value>(&s))
                .transpose()?
                .map_or(json!([]), |v| v["exportMxids"].clone());
            let queued: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM coordinator_publications WHERE engagement_id=?1 AND id='engagement_authority')",[policy.id.as_str()],|r|r.get(0))?;
            result.push(json!({"id":policy.id,"serverName":policy.server,"ownerMxid":policy.owner,"coordinatorMxid":policy.coordinator,
                "state":policy.state,"registrationGeneration":policy.registration_generation,"delegationRevision":policy.delegation_revision,
                "delegationExpiresAtMs":policy.delegation_expires_at_ms,"allowSelfApproval":policy.allow_self_approval,"exportMxids":exports,"delegationPublication":if queued {"queued"}else{"acknowledged"}}));
        }
        Ok(result)
    }
    pub fn server_engagement_resources(
        &self,
        fleet: &str,
        after: &str,
        limit: usize,
    ) -> Result<Vec<Value>, Error> {
        if limit == 0 || limit > 50 {
            return Err(Error::Capacity);
        }
        binding(&self.db, fleet)?.ok_or(Error::NotFound)?;
        let mut query=self.db.prepare("SELECT id,resource_id,revision,allocated_tokens,period,period_key,managers FROM coordinator_resources WHERE engagement_id=?1 AND id>?2 ORDER BY id LIMIT ?3")?;
        let mut result = Vec::new();
        let mut bytes = 0;
        for row in query.query_map(params![fleet, after, limit], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
                r.get::<_, u64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
            ))
        })? {
            let (id, resource, revision, allocated, period, key, managers) = row?;
            let managers: Vec<contract::MatrixUserId> = serde_json::from_str(&managers)?;
            let retained = held(&self.db, &id)?.0;
            let row = json!({"id":id,"serverEngagementId":fleet,"resourceId":resource,"revision":revision,"allocatedTokens":allocated,
                "retainedTokens":retained,"remainingTokens":allocated.saturating_sub(retained),"overdrawn":retained>allocated,
                "period":period,"periodKey":key,"eligibleManagers":managers});
            bytes += serialize(&row)?.len() + 1;
            if bytes > 60 * 1024 {
                if result.is_empty() {
                    return Err(Error::Capacity);
                }
                break;
            }
            result.push(row);
        }
        Ok(result)
    }
    pub fn resource_engagements(&self, resource: &str) -> Result<Vec<ResourceGrant>, Error> {
        resource_grants(&self.db, resource)
    }
    pub fn coordinator_agent_usage(&self, id: &str) -> Result<Value, Error> {
        let quota = self.quota_status(id)?;
        let observed:Option<u64>=self.db.query_row("SELECT MIN(observed_at) FROM usage_sources WHERE engagement_id=?1 AND observed_at IS NOT NULL",[id],|r|r.get(0))?;
        if let Ok(settlement) = self.coordinator_settlement(id)
            && let Some(final_tokens) = settlement["consumedTokens"].as_u64()
        {
            return Ok(
                json!({"consumedTokens":final_tokens.max(quota.spent_tokens.unwrap_or(0)),
                "usageObservedAtMs":settlement["observedAtMs"],"usageEvidence":"owner_account_reconciliation",
                "usageComplete":settlement["state"]=="settled","quotaPaused":quota.paused}),
            );
        }
        Ok(
            json!({"consumedTokens":quota.spent_tokens,"usageObservedAtMs":observed,"usageEvidence":"host_attributed_lower_bound",
            "usageComplete":false,
            "quotaPaused":quota.paused}),
        )
    }

    /// Apply the Rinx coordinator's token decision inside the same writer as
    /// parent/child accounting, quota resumption, receipt and publication.
    pub fn approve_coordinator_top_up(
        &mut self,
        command: &TokenTopUpApproval,
        proof: &VerifiedRequest,
        now: u64,
    ) -> Result<Engagement, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let r = &command.request;
        let fleet = r.server_engagement_id.as_str();
        let fingerprint =
            canonical::digest(&json!({"operation":"coordinator_token_top_up","command":command}))?;
        if let Some(value) =
            refusals::result(&tx, command.context.command_id.as_str(), &fingerprint)?
        {
            return Ok(serde_json::from_value(value)?);
        }
        let current = current(&tx, fleet, now)?;
        authority(&tx, proof, now)?;
        project_authority(&tx, proof)?;
        let original = proof.request();
        if original.fleet_id != fleet || original.engagement_id()? != r.agent_allocation_id.as_str()
        {
            return Err(Error::Conflict);
        }
        let raw: String = tx
            .query_row(
                "SELECT grant FROM coordinator_projects WHERE id=?1 AND engagement_id=?2",
                params![r.project_id.as_str(), fleet],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let project: ProjectGrant = serde_json::from_str(&raw)?;
        contract::authorize_token_top_up(
            command,
            &command.request,
            &current,
            &project,
            &command.context.actor,
            now,
        )
        .map_err(policy)?;
        let fingerprint =
            canonical::digest(&json!({"operation":"coordinator_token_top_up","command":command}))?;
        if let Some(existing) = replay(&tx, command.context.command_id.as_str(), &fingerprint)? {
            return Ok(existing);
        }
        let id = r.agent_allocation_id.as_str();
        let mut agent = read_engagement(&tx, id)?;
        let (grant,decision,retained):(String,String,u64)=tx.query_row("SELECT resource_allocation_id,decision,retained_tokens FROM coordinator_agents WHERE agent_id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?.ok_or(Error::NotFound)?;
        let decision = audit::approved_request(&decision, false)?;
        if r.resource_allocation_id.as_str() != grant
            || decision.server_engagement_id != r.server_engagement_id
            || decision.project_id != r.project_id
            || decision.project_owner != r.project_owner
            || r.project_owner.as_str() != original.owner_mxid
            || r.requester != r.project_owner
            || u64::from(r.expected_allocated_tokens) != u64::from(agent.allocation())
        {
            return Err(Error::Conflict);
        }
        let definition = json!({"agentAllocationId":r.agent_allocation_id,"expectedAllocatedTokens":r.expected_allocated_tokens,"requestedAdditionalTokens":r.requested_additional_tokens});
        let expected: String = r.definition_digest.clone().into();
        if canonical::digest(&definition)? != expected {
            return Err(Error::Conflict);
        }
        if !matches!(
            agent.state,
            EngagementState::Reserved | EngagementState::Active
        ) {
            return Err(Error::State);
        }
        let resource = read_resource(&tx, &agent.resource_id)?;
        let (granularity, key) = period(&tx, &resource, now)?;
        let (stored_period,stored_key,managers):(String,String,String)=tx.query_row("SELECT period,period_key,managers FROM coordinator_resources WHERE id=?1 AND engagement_id=?2",params![grant,fleet],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        if granularity != stored_period
            || key != stored_key
            || !serde_json::from_str::<Vec<contract::MatrixUserId>>(&managers)?
                .contains(&r.project_owner)
        {
            return Err(Error::LocalAuthority);
        }
        let add = u64::from(command.additional_tokens);
        if add > unused_grant(&tx, &grant, &resource)? {
            return Err(Error::InsufficientCapacity);
        }
        check_grant_within(
            &tx,
            &resource,
            agent.agent_name.as_str(),
            add,
            now,
            Some(&grant),
        )?;
        let raised = u64::from(agent.allocation())
            .checked_add(add)
            .ok_or(Error::Capacity)?;
        agent.allocated_tokens = Some(raised.try_into()?);
        write_engagement(&tx, &agent)?;
        tx.execute("UPDATE coordinator_agents SET allocated_tokens=?2,retained_tokens=?3 WHERE agent_id=?1",params![id,raised,retained.checked_add(add).ok_or(Error::Capacity)?])?;
        quota_holds::lift(&tx, id, now)?;
        record_decision(
            &tx,
            command.context.command_id.as_str(),
            &fingerprint,
            &agent,
            Some("engagement.allocation_raised"),
        )?;
        bounded_row(
            &tx,
            "coordinator_commands",
            "id",
            command.context.command_id.as_str(),
            10000,
        )?;
        tx.execute(
            "INSERT INTO coordinator_commands(id,engagement_id,digest,receipt) VALUES(?1,?2,?3,?4)",
            params![
                command.context.command_id.as_str(),
                fleet,
                fingerprint,
                serialize(&agent)?
            ],
        )?;
        publish(
            &tx,
            fleet,
            &format!("command_{}", command.context.command_id.as_str()),
            json!({"kind":"receipt","commandId":command.context.command_id,"commandDigest":fingerprint,"agentId":id,"state":"applied"}),
        )?;
        tx.commit()?;
        Ok(agent)
    }

    pub fn coordinator_updates(
        &self,
        identity: &crate::outbound::RegistrationIdentity,
    ) -> Result<Vec<Value>, Error> {
        self.check_publication_registration(identity)?;
        let mut rows = self.db.prepare("SELECT id,digest,payload FROM coordinator_publications WHERE engagement_id=?1 ORDER BY CASE WHEN id='engagement_authority' THEN 0 WHEN id LIKE 'resource_%' THEN 1 WHEN id LIKE 'project_%' THEN 2 ELSE 3 END,id LIMIT 100")?;
        let mut result = Vec::new();
        let mut bytes = 0usize;
        for row in rows.query_map([&identity.fleet_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (id, digest, raw) = row?;
            bytes += raw.len();
            if bytes > 256 * 1024 {
                break;
            }
            let payload: Value = serde_json::from_str(&raw)?;
            result.push(json!({"id":id,"digest":digest,"payload":payload}));
        }
        Ok(result)
    }

    pub fn acknowledge_coordinator_updates(
        &mut self,
        identity: &crate::outbound::RegistrationIdentity,
        updates: &[Value],
    ) -> Result<(), Error> {
        self.check_publication_registration(identity)?;
        if updates.len() > 100 {
            return Err(Error::Capacity);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        for update in updates {
            let id = update["id"]
                .as_str()
                .ok_or(InvalidInput("missing publication id"))?;
            let digest = update["digest"]
                .as_str()
                .ok_or(InvalidInput("missing publication digest"))?;
            tx.execute("DELETE FROM coordinator_publications WHERE engagement_id=?1 AND id=?2 AND digest=?3",params![identity.fleet_id,id,digest])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Called only by authenticated local resource-owner setup, never by a
    /// command received from Palpo. The owner explicitly delegates this role.
    pub fn configure_coordinator(&mut self, value: &ServerEngagement) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::configure_coordinator_transaction(&tx, value)?;
        tx.commit()?;
        Ok(())
    }
    fn configure_coordinator_transaction(
        tx: &Transaction<'_>,
        value: &ServerEngagement,
    ) -> Result<(), Error> {
        let raw: String = tx
            .query_row(
                "SELECT config FROM registrations WHERE fleet_id=?1",
                [value.id.as_str()],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let registration: Registration = serde_json::from_str(&raw)?;
        if value.server.as_str() != registration.server_name
            || u64::from(value.registration_generation) != registration.generation
            || !value.owner.belongs_to(&value.server)
            || !value.coordinator.belongs_to(&value.server)
            || value.delegation_expires_at_ms > JSON_SAFE_MAX
            || value.state == contract::EngagementState::Verified
                && registration.reception_room_id.is_empty()
        {
            return Err(Error::Generation);
        }
        if let Some(old) = binding(tx, value.id.as_str())?
            && (old.owner != value.owner
                || old.server != value.server
                || value.registration_generation < old.registration_generation
                || value.delegation_revision < old.delegation_revision
                || ((value.coordinator != old.coordinator
                    || value.allow_self_approval != old.allow_self_approval
                    || value.delegation_expires_at_ms != old.delegation_expires_at_ms)
                    && value.delegation_revision == old.delegation_revision)
                || old.state == contract::EngagementState::Revoked
                    && value.state != old.state
                    && value.registration_generation == old.registration_generation)
        {
            return Err(Error::Generation);
        }
        tx.execute("INSERT INTO coordinator_engagements(id,authority) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET authority=excluded.authority", params![value.id.as_str(),serialize(value)?])?;
        Ok(())
    }

    pub fn import_coordinator_registration(
        &mut self,
        registration: &Registration,
        policy: Option<&ServerEngagement>,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if policy.is_some_and(|p| p.id.as_str() != registration.fleet_id) {
            return Err(Error::Generation);
        }
        Self::register_transaction(&tx, registration)?;
        if let Some(policy) = policy {
            Self::configure_coordinator_transaction(&tx, policy)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Reserve or top up a contribution at its parent. Shrinking cannot discard
    /// an agent's spent/unknown reservation. IDs never resolve through hostname.
    pub fn put_coordinator_resource(
        &mut self,
        grant: &ResourceGrant,
        now: u64,
    ) -> Result<(), Error> {
        self.put_coordinator_resource_inner(grant, now, None)
    }

    pub fn contribute_resource(
        &mut self,
        command: ResourceContributionCommand,
        now: u64,
    ) -> Result<(), Error> {
        self.put_coordinator_resource_inner(&command.grant, now, Some(&command.gate))
    }

    fn put_coordinator_resource_inner(
        &mut self,
        grant: &ResourceGrant,
        now: u64,
        gate: Option<&crate::ResourcePublicationCommand>,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revoked = gate
            .map(|g| {
                g.access
                    .revoked
                    .try_lock()
                    .map_err(resource_publication::lock_error)
            })
            .transpose()?;
        let check = || {
            if let (Some(g), Some(revoked)) = (gate, revoked.as_ref()) {
                g.access
                    .check_at(**revoked, g.deadline, std::time::Instant::now())?;
            }
            Ok::<_, Error>(())
        };
        check()?;
        put_resource_transaction(&tx, &self.accounts, grant, now)?;
        check()?;
        tx.commit()?;
        check().map_err(|_| Error::OutcomeUnknown)
    }

    /// The project command comes only through the configured authenticated
    /// Palpo transport. Its actor was authenticated there; local delegation is
    /// checked again here. Room readiness is a later verified transition.
    pub fn approve_coordinator_project(
        &mut self,
        command: &ProjectApproval,
        definition: &Value,
        now: u64,
    ) -> Result<ProjectGrant, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fingerprint = canonical::digest(
            &json!({"operation":"coordinator_project_approval","command":command,"definition":definition}),
        )?;
        if let Some(value) =
            refusals::result(&tx, command.context.command_id.as_str(), &fingerprint)?
        {
            return Ok(serde_json::from_value(value)?);
        }
        let authority = current(&tx, command.context.server_engagement_id.as_str(), now)?;
        contract::authorize_project_approval(
            command,
            &command.request,
            &authority,
            &command.context.actor,
            now,
        )
        .map_err(policy)?;
        let expected: String = command.request.definition_digest.clone().into();
        if canonical::digest(definition)? != expected {
            return Err(Error::Conflict);
        }
        let detail: ProjectDefinition = serde_json::from_value(definition.clone())?;
        let room = |id: &str| {
            id.starts_with('!')
                && id.len() <= 255
                && !id.chars().any(|c| c.is_control() || c.is_whitespace())
                && id
                    .split_once(':')
                    .is_some_and(|(_, s)| s == authority.server.as_str())
        };
        if detail.name.trim().is_empty()
            || detail.name.len() > 256
            || detail.name.chars().any(char::is_control)
            || detail.reason.chars().count() > 2000
            || detail.reason.chars().any(char::is_control)
            || !room(&detail.room_id)
            || !room(&detail.owner_dm_room_id)
            || detail.room_id == detail.owner_dm_room_id
        {
            return Err(InvalidInput("invalid project definition").into());
        }
        for id in &command.request.resource_allocations {
            let (fleet,allocated,managers): (String,u64,String) = tx.query_row("SELECT engagement_id,allocated_tokens,managers FROM coordinator_resources WHERE id=?1", [id.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.ok_or(Error::NotFound)?;
            let managers: Vec<contract::MatrixUserId> = serde_json::from_str(&managers)?;
            if fleet != authority.id.as_str()
                || allocated == 0
                || !managers.contains(&command.request.owner)
            {
                return Err(Error::LocalAuthority);
            }
        }
        let mut grant = ProjectGrant {
            project_id: command.request.project_id.clone(),
            server_engagement_id: authority.id,
            revision: command.request.revision,
            owner: command.request.owner.clone(),
            resource_allocations: command.request.resource_allocations.clone(),
            state: contract::ProjectState::Approved,
        };
        let existing: Option<(String,String)> = tx.query_row("SELECT grant,definition FROM coordinator_projects WHERE id=?1 AND engagement_id=?2", params![grant.project_id.as_str(),grant.server_engagement_id.as_str()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((old, def)) = &existing {
            let old: ProjectGrant = serde_json::from_str(old)?;
            let mut compare = old.clone();
            compare.state = contract::ProjectState::Approved;
            if compare != grant || serde_json::from_str::<Value>(def)? != *definition {
                return Err(Error::Conflict);
            }
            if matches!(
                old.state,
                contract::ProjectState::Rejected | contract::ProjectState::Revoked
            ) {
                return Err(Error::State);
            }
            grant = old;
        }
        let fingerprint = canonical::digest(
            &json!({"operation":"coordinator_project_approval","command":command,"definition":definition}),
        )?;
        let command_id = command.context.command_id.as_str();
        let prior: Option<String> = tx
            .query_row(
                "SELECT digest FROM coordinator_commands WHERE id=?1",
                [command_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = prior {
            if old != fingerprint {
                return Err(Error::Conflict);
            }
            return Ok(grant);
        }
        bounded_row(&tx, "coordinator_commands", "id", command_id, 10000)?;
        if existing.is_none() {
            bounded_row(
                &tx,
                "coordinator_projects",
                "id",
                grant.project_id.as_str(),
                1000,
            )?;
            tx.execute("INSERT INTO coordinator_projects(id,engagement_id,grant,definition) VALUES(?1,?2,?3,?4)", params![grant.project_id.as_str(),grant.server_engagement_id.as_str(),serialize(&grant)?,serialize(definition)?])?;
        }
        tx.execute(
            "INSERT INTO coordinator_commands(id,engagement_id,digest,receipt) VALUES(?1,?2,?3,?4)",
            params![
                command_id,
                grant.server_engagement_id.as_str(),
                fingerprint,
                serialize(&grant)?
            ],
        )?;
        publish_project(&tx, &grant)?;
        publish(
            &tx,
            grant.server_engagement_id.as_str(),
            &format!("command_{command_id}"),
            json!({"kind":"receipt","commandId":command_id,"commandDigest":fingerprint,"projectId":grant.project_id,"state":"applied"}),
        )?;
        tx.commit()?;
        Ok(grant)
    }

    pub fn coordinator_project_ready(
        &mut self,
        observed: &ProjectReadiness,
        now: u64,
    ) -> Result<ProjectGrant, Error> {
        if now < observed.observed_at_ms || now - observed.observed_at_ms > 30000 {
            return Err(Error::Generation);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let fleet = &observed.registration.fleet_id;
        current(&tx, fleet, now)?;
        let stored: String = tx.query_row(
            "SELECT config FROM registrations WHERE fleet_id=?1",
            [fleet],
            |r| r.get(0),
        )?;
        if serde_json::from_str::<Registration>(&stored)? != observed.registration {
            return Err(Error::Generation);
        }
        let (raw,definition):(String,String)=tx.query_row("SELECT grant,definition FROM coordinator_projects WHERE engagement_id=?1 AND id=?2",params![fleet,observed.project_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Error::NotFound)?;
        let mut grant: ProjectGrant = serde_json::from_str(&raw)?;
        let definition: ProjectDefinition = serde_json::from_str(&definition)?;
        let p = &observed.project;
        let dm = &observed.owner_room;
        let owner = grant.owner.as_str();
        let reg = &observed.registration;
        let expected = json!({"v":1,"fleetId":fleet,"purpose":"project","projectId":observed.project_id,"ownerMxid":owner,"authVersion":1});
        if !matches!(
            grant.state,
            contract::ProjectState::Approved | contract::ProjectState::Ready
        ) || p.room_id != definition.room_id
            || dm.room_id != definition.owner_dm_room_id
            || !p.invite_only
            || p.encryption.is_some()
            || p.binding.as_ref() != Some(&expected)
            || !p.joined.contains(owner)
            || !p.joined.contains(&reg.representative_mxid)
            || p.powers.get(owner).copied().unwrap_or(p.default_power) < 100.max(p.invite_power)
            || !dm.invite_only
            || dm.encryption.as_deref() != Some("m.megolm.v1.aes-sha2")
            || dm.joined
                != std::collections::BTreeSet::from([
                    owner.to_owned(),
                    reg.approval_bot_mxid.clone(),
                ])
        {
            return Err(Error::LocalAuthority);
        }
        grant.state = contract::ProjectState::Ready;
        tx.execute(
            "UPDATE coordinator_projects SET grant=?3 WHERE engagement_id=?1 AND id=?2",
            params![fleet, observed.project_id, serialize(&grant)?],
        )?;
        publish_project(&tx, &grant)?;
        tx.commit()?;
        Ok(grant)
    }

    /// Fresh Matrix request evidence binds the project's real room/owner before
    /// it becomes usable. This does not itself grant or provision an agent.
    pub fn verify_coordinator_project(
        &mut self,
        proof: &VerifiedRequest,
        now: u64,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        authority(&tx, proof, now)?;
        project_authority(&tx, proof)?;
        let request = proof.request();
        current(&tx, &request.fleet_id, now)?;
        let (raw, definition): (String,String) = tx.query_row("SELECT grant,definition FROM coordinator_projects WHERE id=?1 AND engagement_id=?2", params![request.target_project_id,request.fleet_id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(Error::NotFound)?;
        let mut grant: ProjectGrant = serde_json::from_str(&raw)?;
        let definition: ProjectDefinition = serde_json::from_str(&definition)?;
        if definition.room_id != request.target_room_id
            || definition.owner_dm_room_id != request.owner_dm_room_id
        {
            return Err(Error::Conflict);
        }
        if grant.owner.as_str() != request.owner_mxid
            || !matches!(
                grant.state,
                contract::ProjectState::Approved | contract::ProjectState::Ready
            )
        {
            return Err(Error::LocalAuthority);
        }
        grant.state = contract::ProjectState::Ready;
        tx.execute(
            "UPDATE coordinator_projects SET grant=?3 WHERE id=?1 AND engagement_id=?2",
            params![
                grant.project_id.as_str(),
                grant.server_engagement_id.as_str(),
                serialize(&grant)?
            ],
        )?;
        publish_project(&tx, &grant)?;
        tx.commit()?;
        Ok(())
    }
    pub fn approve_coordinated_agent(
        &mut self,
        command: &AgentApproval,
        proof: &VerifiedRequest,
        now: u64,
    ) -> Result<Engagement, Error> {
        self.approve_allocating_inner(
            command.context.command_id.as_str(),
            proof,
            now,
            Some(u64::from(command.allocated_tokens)),
            Some(command),
        )
    }
}

/// Shared by the legacy contribution command and atomic resource configuration.
pub(super) fn put_resource_transaction(
    tx: &Transaction<'_>,
    accounts: &accounts::Registry,
    grant: &ResourceGrant,
    now: u64,
) -> Result<(), Error> {
    let authority = current(tx, grant.server_engagement_id.as_str(), now)?;
    bounded_row(tx, "coordinator_resources", "id", grant.id.as_str(), 2000)?;
    if grant.eligible_managers.len() > 64
        || grant
            .eligible_managers
            .iter()
            .any(|u| !u.belongs_to(&authority.server))
    {
        return Err(Error::LocalAuthority);
    }
    let resource = read_resource(tx, &grant.resource_id)?;
    accounts.check_resource(tx, &resource)?;
    let (granularity, key) = period(tx, &resource, now)?;
    let old: Option<StoredResourceGrant> = tx.query_row("SELECT engagement_id,resource_id,preset_id,seat_id,revision,allocated_tokens,period,period_key,managers FROM coordinator_resources WHERE id=?1", [grant.id.as_str()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).optional()?;
    let amount = u64::from(grant.allocated_tokens);
    let managers = serialize(&grant.eligible_managers)?;
    let mut before = 0;
    if let Some((
        fleet,
        parent,
        preset,
        seat,
        revision,
        allocated,
        period,
        period_key,
        previous_managers,
    )) = old
    {
        if fleet != grant.server_engagement_id.as_str()
            || parent != grant.resource_id
            || preset != resource.preset_id
            || seat != resource.seat_id
            || period != granularity
            || period_key != key
        {
            return Err(Error::Generation);
        }
        if u64::from(grant.revision) == revision
            && amount == allocated
            && managers == previous_managers
        {
            return Ok(());
        }
        if u64::from(grant.revision) <= revision {
            return Err(Error::Conflict);
        }
        before = allocated;
    }
    if amount < held(tx, grant.id.as_str())?.0 {
        return Err(Error::InsufficientCapacity);
    }
    if amount > before {
        check_grant(
            tx,
            &resource,
            "engagement contribution",
            amount - before,
            now,
        )?;
    }
    tx.execute("INSERT INTO coordinator_resources(id,engagement_id,resource_id,preset_id,seat_id,revision,allocated_tokens,period,period_key,managers) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10) ON CONFLICT(id) DO UPDATE SET revision=excluded.revision,allocated_tokens=excluded.allocated_tokens,managers=excluded.managers", params![grant.id.as_str(),grant.server_engagement_id.as_str(),grant.resource_id,resource.preset_id,resource.seat_id,u64::from(grant.revision),amount,granularity,key,managers])?;
    publish(
        tx,
        grant.server_engagement_id.as_str(),
        &format!("resource_{}", grant.id.as_str()),
        json!({"kind":"resource","resource":{
            "id":grant.id,"serverEngagementId":grant.server_engagement_id,"revision":grant.revision,"allocatedTokens":grant.allocated_tokens,"eligibleManagers":grant.eligible_managers
        },"resourceId":grant.resource_id,"period":granularity,"periodKey":key}),
    )?;
    Ok(())
}

pub(super) fn resource_grants(
    db: &Connection,
    resource: &str,
) -> Result<Vec<ResourceGrant>, Error> {
    let mut query = db.prepare("SELECT id,engagement_id,revision,allocated_tokens,managers FROM coordinator_resources WHERE resource_id=?1 ORDER BY id")?;
    let rows = query.query_map([resource], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u64>(2)?,
            r.get::<_, u64>(3)?,
            r.get::<_, String>(4)?,
        ))
    })?;
    rows.map(|row| {
        let (id, engagement, revision, tokens, managers) = row?;
        serde_json::from_value(json!({"id":id,"serverEngagementId":engagement,"resourceId":resource,"revision":revision,"allocatedTokens":tokens,"eligibleManagers":serde_json::from_str::<Value>(&managers)?})).map_err(Into::into)
    }).collect()
}
