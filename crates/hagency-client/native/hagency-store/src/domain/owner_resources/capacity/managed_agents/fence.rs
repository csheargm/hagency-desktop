//! Exact central cleanup, including a durable semantic fence before reservation.
//! A stored fence is not evidence that an issued provider process stopped.
use super::*;

pub const AGENT_FENCE_CAPABILITY: &str = "hagency.managed_agents.fence.v1";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedAgentFenceCommand {
    pub version: u16,
    pub capability: String,
    pub operation: String,
    pub command_id: String,
    pub reserve_command: ManagedAgentReserveCommand,
    pub reserve_command_digest: String,
    pub issued_at_ms: u64,
}
impl ManagedAgentFenceCommand {
    pub fn validate(&self, owner: &str) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        exact(self.issued_at_ms)?;
        self.reserve_command.validate(owner)?;
        if self.version != 1
            || self.capability != AGENT_FENCE_CAPABILITY
            || self.operation != "agent.fence"
            || canonical::digest(&serde_json::to_value(&self.reserve_command)?)?
                != self.reserve_command_digest
            || serde_json::to_vec(self)?.len() > 16 * 1024
        {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManagedAgentFenceReceipt {
    pub version: u16,
    pub command_id: String,
    pub command_digest: String,
    pub reserve_command_digest: String,
    pub request_id: String,
    pub agent_id: String,
    pub agent_reservation_id: Option<String>,
    pub state: String,
    pub fence_revision: u64,
    pub ledger_revision: u64,
    pub known_consumed_tokens: u64,
    pub retained_tokens: Option<u64>,
    pub usage_complete: bool,
    pub process_state: String,
}
/// Native transport owns this current authenticated lease. Recovery never
/// constructs it; no script/provider/legacy-registration authority is accepted.
pub struct VerifiedManagedAgentFence {
    enrollment: VerifiedManagementEnrollment,
    command: ManagedAgentFenceCommand,
    digest: String,
    project: Option<AllocationCommand>,
    expires: Instant,
}
impl VerifiedManagedAgentFence {
    pub fn new(
        enrollment: VerifiedManagementEnrollment,
        command: ManagedAgentFenceCommand,
        digest: String,
        project: AllocationCommand,
        execution_allowed: bool,
        expires: Instant,
    ) -> Result<Self, Error> {
        enrollment.check()?;
        command.validate(&enrollment.profile.account)?;
        hash(&digest)?;
        if !execution_allowed
            || expires <= Instant::now()
            || expires > Instant::now() + Duration::from_secs(60)
            || canonical::digest(&serde_json::to_value(&command)?)? != digest
            || serde_json::to_vec(&project)?.len() > 32 * 1024
        {
            return Err(Error::LocalAuthority);
        }
        Ok(Self {
            enrollment,
            command,
            digest,
            project: Some(project),
            expires,
        })
    }
    pub fn for_pool(
        enrollment: VerifiedManagementEnrollment,
        command: ManagedAgentFenceCommand,
        digest: String,
        execution_allowed: bool,
        expires: Instant,
    ) -> Result<Self, Error> {
        enrollment.check()?;
        command.validate(&enrollment.profile.account)?;
        if command.reserve_command.pool_grant.is_none()
            || !execution_allowed
            || expires <= Instant::now()
            || expires > Instant::now() + Duration::from_secs(60)
            || canonical::digest(&serde_json::to_value(&command)?)? != digest
        {
            return Err(Error::LocalAuthority);
        }
        Ok(Self {
            enrollment,
            command,
            digest,
            project: None,
            expires,
        })
    }
    fn check(&self, r: &Inner) -> Result<(), Error> {
        self.enrollment.check()?;
        if r.closed.load(Ordering::Acquire) || Instant::now() >= self.expires {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
fn check_read(p: &ProfileAuthority, r: &Inner) -> Result<(), Error> {
    p.check()?;
    if r.closed.load(Ordering::Acquire) {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}

/// Original acknowledged project history only; no reservation or fresh grant.
pub(super) fn original_project(
    db: &Connection,
    p: &ProfileAuthority,
    device: &str,
    id: &str,
    expected: &str,
) -> Result<AllocationCommand, Error> {
    find_original_project(db, p, device, id, expected)?.ok_or(Error::LocalAuthority)
}
fn find_original_project(
    db: &Connection,
    p: &ProfileAuthority,
    device: &str,
    id: &str,
    expected: &str,
) -> Result<Option<AllocationCommand>, Error> {
    let row:Option<(String,String,String,Option<String>,String)>=db.query_row("SELECT g.profile,a.reserve_command,a.reserve_digest,r.reserve_digest,r.reserve_command_id FROM owner_project_authorities a JOIN owner_project_reservations r ON r.id=a.allocation_id JOIN owner_capacity_grants g ON g.id=r.grant_id WHERE a.allocation_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    let Some((profile, raw, digest, saved, command_id)) = row else {
        // A clean current installation may have no original allocation. A
        // partial/broken join is not that absence and must remain a refusal.
        let partial: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_project_authorities WHERE allocation_id=?1 UNION ALL SELECT 1 FROM owner_project_reservations WHERE id=?1)",
            [id], |r| r.get(0),
        )?;
        return if partial {
            Err(Error::LocalAuthority)
        } else {
            Ok(None)
        };
    };
    let c: AllocationCommand = serde_json::from_str(&raw)?;
    validate_command(&c)?;
    if profile != p.digest
        || c.grant_receipt.owner_mxid != p.account
        || c.grant_receipt.executor_device_id != device
        || c.operation != "project.reserve"
        || c.command_id != command_id
        || saved.as_deref() != Some(&digest)
        || canonical::digest(&serde_json::to_value(&c)?)? != digest
    {
        return Err(Error::LocalAuthority);
    }
    let r: NativeAllocationReceipt =
        receipt(db, &p.digest, device, &command_id, &digest)?.ok_or(Error::LocalAuthority)?;
    if r.state != "reserved"
        || r.allocation_id != id
        || r.reserved_tokens != c.requested_tokens
        || r.retained_tokens != c.requested_tokens
        || canonical::digest(&serde_json::to_value(&r)?)? != expected
    {
        return Err(Error::Conflict);
    }
    Ok(Some(c))
}
impl OwnerResourceRegistry {
    pub async fn managed_agent_project_history(
        &self,
        profile: ProfileAuthority,
        device: String,
        allocation: String,
        receipt_digest: String,
    ) -> Result<AllocationCommand, Error> {
        self.find_managed_agent_project_history(profile, device, allocation, receipt_digest)
            .await?
            .ok_or(Error::LocalAuthority)
    }
    /// Read-only discovery for historical transport deliveries. None means the
    /// original allocation is absent, never that current authority was refused.
    pub async fn find_managed_agent_project_history(
        &self,
        profile: ProfileAuthority,
        device: String,
        allocation: String,
        receipt_digest: String,
    ) -> Result<Option<AllocationCommand>, Error> {
        check_read(&profile, &self.0)?;
        identifier(&device, 128)?;
        identifier(&allocation, 128)?;
        hash(&receipt_digest)?;
        let p = profile.clone();
        let registry = self.0.clone();
        let result = self
            .0
            .store
            .call(32 * 1024, move |db| {
                check_read(&profile, &registry)?;
                let result =
                    find_original_project(&db.db, &profile, &device, &allocation, &receipt_digest)?;
                check_read(&profile, &registry)?;
                Ok(result)
            })
            .await?;
        check_read(&p, &self.0)?;
        Ok(result)
    }
    pub async fn apply_managed_agent_fence(
        &self,
        d: VerifiedManagedAgentFence,
    ) -> Result<ManagedAgentFenceReceipt, Error> {
        d.check(&self.0)?;
        let p = d.enrollment.profile.clone();
        let expires = d.expires.min(d.enrollment.expires);
        let registry = self.0.clone();
        let result = self
            .0
            .store
            .call(64 * 1024, move |db| {
                db.owner_managed_agent_fence(d, registry)
            })
            .await?;
        if check_read(&p, &self.0).is_err() || Instant::now() >= expires {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }
    pub async fn recover_managed_agent_fence(
        &self,
        profile: ProfileAuthority,
        device: String,
        command: String,
        digest: String,
    ) -> Result<Option<ManagedAgentFenceReceipt>, Error> {
        check_read(&profile, &self.0)?;
        identifier(&device, 128)?;
        identifier(&command, 128)?;
        hash(&digest)?;
        let p = profile.clone();
        let registry = self.0.clone();
        let result = self
            .0
            .store
            .call(8192, move |db| {
                check_read(&profile, &registry)?;
                let r = receipt(&db.db, &profile.digest, &device, &command, &digest)?;
                check_read(&profile, &registry)?;
                Ok(r)
            })
            .await?;
        check_read(&p, &self.0)?;
        Ok(result)
    }
}
impl DomainRepository {
    fn owner_managed_agent_fence(
        &mut self,
        d: VerifiedManagedAgentFence,
        registry: Arc<Inner>,
    ) -> Result<ManagedAgentFenceReceipt, Error> {
        d.check(&registry)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let e = &d.enrollment;
        let c = &d.command;
        let original = &c.reserve_command;
        if let Some(old) = receipt(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &c.command_id,
            &d.digest,
        )? {
            d.check(&registry)?;
            return Ok(old);
        }
        verify_original(&tx, e, original, d.project.as_ref())?;
        let old:Option<(String,String,u64)>=tx.query_row("SELECT original_command_digest,original_command,fence_revision FROM owner_managed_fences WHERE profile=?1 AND (agent_id=?2 AND request_id=?3)",params![e.profile.digest,original.proposed_agent_id,original.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let encoded = canonical::encode(&serde_json::to_value(original)?)?;
        if old
            .as_ref()
            .is_some_and(|(digest, raw, _)| digest != &c.reserve_command_digest || raw != &encoded)
        {
            return Err(Error::Conflict);
        }
        let child:Option<(String,String,String)>=tx.query_row("SELECT id,original_command_digest,original_command FROM owner_managed_agents WHERE profile=?1 AND (agent_id=?2 AND request_id=?3)",params![e.profile.digest,original.proposed_agent_id,original.request_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        if child
            .as_ref()
            .is_some_and(|(_, digest, raw)| digest != &c.reserve_command_digest || raw != &encoded)
        {
            return Err(Error::Conflict);
        }
        let revision = revision(&tx)?;
        let fence_revision = old.as_ref().map(|r| r.2).unwrap_or(revision);
        if old.is_none() {
            bounded_row(
                &tx,
                "owner_managed_fences",
                "original_command_id",
                &original.command_id,
                100_000,
            )?;
            tx.execute(
                "INSERT INTO owner_managed_fences(profile,agent_id,request_id,original_command_id,original_command_digest,original_command,project_reservation_id,executor_device_id,fence_revision,pool_grant_id,allocation_generation) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    e.profile.digest,
                    original.proposed_agent_id,
                    original.request_id,
                    original.command_id,
                    c.reserve_command_digest,
                    encoded,
                    original.pool_grant.is_none().then_some(&original.allocation_id),
                    e.executor_device_id,
                    fence_revision,
                    original.pool_grant.as_ref().map(|g| &g.grant_id),
                    original.allocation_generation()
                ],
            )?;
        }
        let mut result = ManagedAgentFenceReceipt {
            version: 1,
            command_id: c.command_id.clone(),
            command_digest: d.digest.clone(),
            reserve_command_digest: c.reserve_command_digest.clone(),
            request_id: original.request_id.clone(),
            agent_id: original.proposed_agent_id.clone(),
            agent_reservation_id: None,
            state: "absent_fenced".into(),
            fence_revision,
            ledger_revision: revision,
            known_consumed_tokens: 0,
            retained_tokens: Some(0),
            usage_complete: true,
            process_state: "not_started".into(),
        };
        if let Some((child, _, _)) = child {
            tx.execute(
                "UPDATE owner_managed_agents SET state='fenced',permanent_fence=1 WHERE id=?1",
                [&child],
            )?;
            tx.execute("UPDATE owner_managed_calls SET state='cancelled_before_issue' WHERE child_id=?1 AND state='prepared'",[&child])?;
            let (consumed, never_issued) = snapshot(&tx, &child)?;
            result.agent_reservation_id = Some(child);
            result.state = "fenced".into();
            result.known_consumed_tokens = consumed;
            result.retained_tokens = never_issued.then_some(0);
            result.usage_complete = never_issued;
            result.process_state = if never_issued {
                "not_started"
            } else {
                "unknown"
            }
            .into();
        }
        save(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &c.command_id,
            &d.digest,
            &result,
        )?;
        d.check(&registry)?;
        tx.commit()?;
        d.check(&registry).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
fn snapshot(db: &Connection, child: &str) -> Result<(u64, bool), Error> {
    let state: String = db.query_row(
        "SELECT state FROM owner_managed_effects WHERE child_id=?1",
        [child],
        |r| r.get(0),
    )?;
    let mut never_issued = state == "pending";
    let mut consumed = 0_u64;
    let mut q=db.prepare("SELECT state,known_consumed_tokens,unrepresentable_usage FROM owner_managed_calls WHERE child_id=?1 ORDER BY id")?;
    for (index, row) in q
        .query_map([child], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, u64>(1)?,
                r.get::<_, bool>(2)?,
            ))
        })?
        .enumerate()
    {
        if index >= 100_000 {
            return Err(Error::Capacity);
        }
        let (state, known, overflow) = row?;
        if let Some(n) = consumed.checked_add(known).filter(|n| *n <= JSON_SAFE_MAX) {
            consumed = n;
        } else {
            consumed = JSON_SAFE_MAX;
            never_issued = false;
        }
        never_issued &= state == "cancelled_before_issue" && known == 0 && !overflow;
    }
    Ok((consumed, never_issued))
}

pub(super) fn verify_original(
    db: &Connection,
    e: &VerifiedManagementEnrollment,
    original: &ManagedAgentReserveCommand,
    expected_project: Option<&AllocationCommand>,
) -> Result<(), Error> {
    if let Some(grant) = &original.pool_grant {
        original.validate(&e.profile.account)?;
        if expected_project.is_some()
            || !e.matches_cleanup(grant)
            || e.executor_generation < grant.executor_generation
        {
            return Err(Error::LocalAuthority);
        }
        let (profile, stored): (String, String) = db.query_row(
            "SELECT profile,receipt FROM owner_capacity_grants WHERE id=?1",
            [&grant.grant_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if profile != e.profile.digest
            || canonical::digest(&serde_json::from_str::<serde_json::Value>(&stored)?)?
                != original.grant_receipt_digest
        {
            return Err(Error::LocalAuthority);
        }
        return Ok(());
    }
    let expected_project = expected_project.ok_or(Error::LocalAuthority)?;
    let project = original_project(
        db,
        &e.profile,
        &e.executor_device_id,
        &original.allocation_id,
        &original.allocation_receipt_digest,
    )?;
    let g = &project.grant_receipt;
    if canonical::digest(&serde_json::to_value(&project)?)?
        != canonical::digest(&serde_json::to_value(expected_project)?)?
        || !e.matches_cleanup(g)
        || e.executor_generation < g.executor_generation
        || original.enrollment_id != g.enrollment_id
        || original.executor_device_id != g.executor_device_id
        || original.executor_generation != g.executor_generation
        || original.resource_id != g.resource_id
        || original.resource_revision != g.resource_revision
        || original.grant_receipt_digest != canonical::digest(&serde_json::to_value(g)?)?
        || original.manager_mxid != project.manager_mxid
        || original.project_id != project.project_id
        || original.project_revision != project.project_revision
        || original.period_key != g.period_key
    {
        return Err(Error::LocalAuthority);
    }
    let stored: String = db.query_row(
        "SELECT receipt FROM owner_capacity_grants WHERE id=?1",
        [&g.grant_id],
        |r| r.get(0),
    )?;
    if canonical::digest(&serde_json::from_str::<serde_json::Value>(&stored)?)?
        != original.grant_receipt_digest
    {
        return Err(Error::Conflict);
    }
    Ok(())
}
