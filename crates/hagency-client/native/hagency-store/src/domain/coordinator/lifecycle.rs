use super::*;
type ProfileObservation = (String, Option<String>, Option<String>, Option<u64>);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentOperation {
    Stop,
    Start,
    Retire,
    RetryCleanup,
    Rename,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentControl {
    pub context: contract::CommandContext,
    pub agent_allocation_id: contract::AgentAllocationId,
    pub project_id: contract::ProjectId,
    pub project_revision: contract::Revision,
    pub resource_allocation_id: contract::ResourceAllocationId,
    pub operation: AgentOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

pub(super) fn authorize(db: &Connection, command: &AgentControl, now: u64) -> Result<(), Error> {
    let c = &command.context;
    let authority = binding(db, c.server_engagement_id.as_str())?.ok_or(Error::NotFound)?;
    let registration: String = db.query_row(
        "SELECT config FROM registrations WHERE fleet_id=?1",
        [c.server_engagement_id.as_str()],
        |r| r.get(0),
    )?;
    let registration: Registration = serde_json::from_str(&registration)?;
    if c.version != 1
        || !authority.coordinator_approval_v1
        || c.registration_generation != authority.registration_generation
        || u64::from(c.registration_generation) != registration.generation
        || c.delegation_revision != authority.delegation_revision
        || c.issued_at_ms > now
        || c.expires_at_ms <= now
        || c.expires_at_ms > contract::MAX_EXACT_JSON_INTEGER
        || !c.actor.belongs_to(&authority.server)
    {
        return Err(Error::Generation);
    }
    let (decision, grant_id): (String, String) = db
        .query_row(
            "SELECT decision,resource_allocation_id FROM coordinator_agents WHERE agent_id=?1",
            [command.agent_allocation_id.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let approved = audit::approved_request(&decision, matches!(command.operation, AgentOperation::Stop | AgentOperation::Retire | AgentOperation::RetryCleanup))?;
    if approved.server_engagement_id != c.server_engagement_id
        || approved.project_id != command.project_id
        || approved.project_revision != command.project_revision
        || grant_id != command.resource_allocation_id.as_str()
    {
        return Err(Error::Conflict);
    }
    let raw: String = db
        .query_row(
            "SELECT grant FROM coordinator_projects WHERE engagement_id=?1 AND id=?2",
            params![c.server_engagement_id.as_str(), command.project_id.as_str()],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    let project: ProjectGrant = serde_json::from_str(&raw)?;
    if project.revision != command.project_revision || project.owner != approved.project_owner {
        return Err(Error::Generation);
    }
    let delegated = c.actor == authority.coordinator && authority.delegation_expires_at_ms > now;
    if c.actor != authority.owner && c.actor != project.owner && !delegated {
        return Err(Error::LocalAuthority);
    }
    // Ending work remains possible after suspension. Only a verified, current
    // delegation/project may re-enable a serving allocation.
    if matches!(
        command.operation,
        AgentOperation::Start | AgentOperation::Rename
    ) {
        current(db, c.server_engagement_id.as_str(), now)?;
        if project.state != contract::ProjectState::Ready {
            return Err(Error::State);
        }
    }
    Ok(())
}

impl DomainRepository {
    pub fn control_coordinator_agent(
        &mut self,
        fleet: &str,
        command: &AgentControl,
        now: u64,
    ) -> Result<Value, Error> {
        if command.context.server_engagement_id.as_str() != fleet {
            return Err(Error::Conflict);
        }
        let payload = json!({"operation":"coordinator_agent_control","command":command});
        let digest = canonical::digest(&payload)?;
        let id = command.context.command_id.as_str();
        let agent = command.agent_allocation_id.as_str();
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(receipt) = refusals::result(&tx, id, &digest)? {
            return Ok(receipt);
        }
        authorize(&tx, command, now)?;
        if (command.operation == AgentOperation::Rename) != command.display_name.is_some() {
            return Err(InvalidInput("display name is only valid for rename").into());
        }
        match command.operation {
            AgentOperation::Rename => {
                if read_engagement(&tx, agent)?.state != EngagementState::Active {
                    return Err(Error::State);
                }
                let name = command.display_name.as_ref().unwrap();
                if name.trim() != name
                    || name.is_empty()
                    || name.chars().count() > 128
                    || name.chars().any(char::is_control)
                {
                    return Err(InvalidInput("invalid display name").into());
                }
                tx.execute("INSERT INTO coordinator_agent_profiles(agent_id,command_id,desired_name,updated_at) VALUES(?1,?2,?3,?4) ON CONFLICT(agent_id) DO UPDATE SET command_id=excluded.command_id,desired_name=excluded.desired_name,updated_at=excluded.updated_at,last_error=NULL",params![agent,id,name,now])?;
            }
            AgentOperation::Retire => {
                end_in_transaction(&tx, id, agent, true)?;
            }
            AgentOperation::RetryCleanup => {
                if read_engagement(&tx, agent)?.state != EngagementState::Revoked {
                    return Err(Error::State);
                }
                if tx.execute("UPDATE effects SET state='pending',outcome_digest=NULL WHERE engagement_id=?1 AND kind='retire' AND state='failed'",[agent])? != 1 { return Err(Error::State); }
            }
            AgentOperation::Stop => {
                if read_engagement(&tx, agent)?.state != EngagementState::Active {
                    return Err(Error::State);
                }
                agent_lifecycle::pause_in_transaction(
                    &tx,
                    agent,
                    command.context.actor.as_str(),
                    now,
                )?;
            }
            AgentOperation::Start => {
                if read_engagement(&tx, agent)?.state != EngagementState::Active {
                    return Err(Error::State);
                }
                agent_lifecycle::start_in_transaction(&tx, agent, now)?;
            }
        }
        let receipt = json!({"kind":"receipt","commandId":id,"commandDigest":digest,"agentId":agent,"operation":command.operation,"state":"applied"});
        bounded_row(&tx, "coordinator_commands", "id", id, 10000)?;
        tx.execute(
            "INSERT INTO coordinator_commands(id,engagement_id,digest,receipt) VALUES(?1,?2,?3,?4)",
            params![id, fleet, digest, serialize(&receipt)?],
        )?;
        publish(&tx, fleet, &format!("command_{id}"), receipt.clone())?;
        tx.commit()?;
        Ok(receipt)
    }

    pub fn coordinator_agent_lifecycle(&self, agent: &str) -> Result<Value, Error> {
        let record = read_engagement(&self.db, agent)?;
        let stopped: bool = self.db.query_row("SELECT EXISTS(SELECT 1 FROM agent_lifecycle WHERE engagement_id=?1 AND started_at IS NULL)",[agent],|r|r.get(0))?;
        let cleanup: Option<String> = self
            .db
            .query_row(
                "SELECT state FROM effects WHERE engagement_id=?1 AND kind='retire'",
                [agent],
                |r| r.get(0),
            )
            .optional()?;
        let provision: Option<String> = self
            .db
            .query_row(
                "SELECT state FROM effects WHERE engagement_id=?1 AND kind='provision'",
                [agent],
                |r| r.get(0),
            )
            .optional()?;
        let matrix_ready: bool = self.db.query_row(
            "SELECT COUNT(DISTINCT json_extract(r.config,'$.privacy.kind'))=2 \
             FROM current_matrix_routes c JOIN matrix_session_routes r ON r.session_id=c.session_id \
             JOIN runner_sessions s ON s.id=c.session_id WHERE s.engagement_id=?1 \
             AND json_extract(r.config,'$.privacy.kind') IN ('direct','group')",
            [agent], |r| r.get(0),
        )?;
        Ok(
            json!({"paused":stopped,"runtimeState":record.state,"provisionEffect":provision,"matrixReady":matrix_ready,"cleanup":record.cleanup,"cleanupEffect":cleanup,"settlement":self.coordinator_settlement(agent).ok(),"matrixProfile":self.matrix_agent_profile(agent)?}),
        )
    }

    /// Desired and observed public label. Canonical agent/resource identities
    /// and the immutable allocation request remain unchanged.
    pub fn matrix_agent_profile(&self, agent: &str) -> Result<Value, Error> {
        let record = read_engagement(&self.db, agent)?;
        let row:Option<ProfileObservation>=self.db.query_row("SELECT desired_name,confirmed_name,last_error,observed_at FROM coordinator_agent_profiles WHERE agent_id=?1",[agent],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        Ok(match row {
            Some((desired, confirmed, error, at)) => {
                json!({"desiredName":desired,"observedName":confirmed,"state":if error.is_some(){"failed"}else if confirmed.as_ref()==Some(&desired){"verified"}else{"pending"},"lastError":error,"observedAtMs":at})
            }
            None => json!({"desiredName":record.agent_name,"state":"default"}),
        })
    }
    /// The original native Matrix collector records only its exact attempted
    /// label. An old response cannot confirm or overwrite a newer request.
    pub fn observe_matrix_agent_profile(
        &mut self,
        agent: &str,
        desired: &str,
        verified: bool,
        now: u64,
    ) -> Result<(), Error> {
        self.db.execute("UPDATE coordinator_agent_profiles SET confirmed_name=CASE WHEN ?3 THEN ?2 ELSE confirmed_name END,last_error=CASE WHEN ?3 THEN NULL ELSE 'matrix_profile_unverified' END,observed_at=?4 WHERE agent_id=?1 AND desired_name=?2",params![agent,desired,verified,now])?;
        Ok(())
    }
}
