//! Additional configurations retain the source's actual private account association.
use super::{
    DomainRepository, prepare_resource_write, read_resource, resource_publication::lock_error,
    write_resource_configuration,
};
use crate::{
    Error, ResourcePublicationAccess, ResourcePublicationCommand, ResourcePublicationRetirement,
    resource_publication_revision,
};
use hagency_core::{
    allocation::{Ceiling, Tokens},
    qualification,
};
use palpo_hagency_contract as contract;
use rusqlite::TransactionBehavior;
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProfileChange {
    Preserve {},
    Select {
        model: String,
        reasoning: Option<String>,
    },
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CeilingChange {
    Preserve {},
    Clear {},
    Monthly { tokens: Tokens },
}
/// One engagement resource, configured with its model and budget in one commit.
/// Allocation IDs remain stable for existing projects and agents.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ResourceEngagementChange {
    pub server_engagement_id: contract::ServerEngagementId,
    pub allocation_id: Option<contract::ResourceAllocationId>,
    pub expected_revision: Option<contract::Revision>,
    pub eligible_managers: Vec<contract::MatrixUserId>,
}
/// Trusted-host constructor only. Never owned by a read-only/publication grant.
/// The inner access supplies the original finite gate, not publication authority.
pub struct ResourceConfigurationAccess(ResourcePublicationAccess);
impl ResourceConfigurationAccess {
    pub fn prepare_settlement(
        &self,
        usage: super::coordinator::FinalUsage,
        deadline: Instant,
    ) -> Result<super::coordinator::SettlementCommand, Error> {
        let digest = hagency_core::canonical::digest(&serde_json::to_value(&usage)?)?;
        let gate = self.0.prepare(
            usage.agent_allocation_id.as_str().into(),
            digest,
            false,
            deadline,
        )?;
        Ok(super::coordinator::SettlementCommand { usage, gate })
    }
    pub fn prepare_delegation(
        &self,
        change: super::coordinator::DelegationChange,
        deadline: Instant,
    ) -> Result<super::coordinator::DelegationCommand, Error> {
        let digest = hagency_core::canonical::digest(&serde_json::to_value(&change)?)?;
        let gate = self.0.prepare(
            change.server_engagement_id.as_str().into(),
            digest,
            false,
            deadline,
        )?;
        Ok(super::coordinator::DelegationCommand { change, gate })
    }
    /// The same finite, revocable console authority also governs contributions.
    pub fn prepare_contribution(
        &self,
        grant: super::coordinator::ResourceGrant,
        deadline: Instant,
    ) -> Result<super::coordinator::ResourceContributionCommand, Error> {
        let digest = hagency_core::canonical::digest(&serde_json::to_value(&grant)?)?;
        let gate = self
            .0
            .prepare(grant.resource_id.clone(), digest, false, deadline)?;
        Ok(super::coordinator::ResourceContributionCommand { grant, gate })
    }
    pub fn new(expires: Instant, retirement: ResourcePublicationRetirement) -> Self {
        Self(ResourcePublicationAccess::new(expires, retirement))
    }
    pub fn revoke(&self) -> Result<(), Error> {
        self.0.revoke()
    }
    pub fn prepare(
        &self,
        resource: String,
        revision: String,
        create: bool,
        profile: ProfileChange,
        ceiling: CeilingChange,
        deadline: Instant,
    ) -> Result<ResourceConfigurationCommand, Error> {
        if let ProfileChange::Select { model, reasoning } = &profile
            && (model.len() > 256 || reasoning.as_ref().is_some_and(|v| v.len() > 128))
        {
            return Err(hagency_core::InvalidInput("configuration field too long").into());
        }
        let gate = self.0.prepare(resource, revision, false, deadline)?;
        let preset = if create {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
            Some(format!(
                "preset_{}",
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            ))
        } else {
            None
        };
        Ok(ResourceConfigurationCommand {
            gate,
            preset,
            profile,
            ceiling,
            execution_policy: None,
            engagement: None,
            receipt: None,
        })
    }
}
/// One consuming command; data unions cannot construct or recreate this authority.
pub struct ResourceConfigurationCommand {
    gate: ResourcePublicationCommand,
    preset: Option<String>,
    profile: ProfileChange,
    ceiling: CeilingChange,
    execution_policy: Option<hagency_core::execution::ExecutionPolicy>,
    engagement: Option<ResourceEngagementChange>,
    receipt: Option<crate::management::CommandReceipt>,
}
impl ResourceConfigurationCommand {
    /// Set the default for subsequently created agents. Leaving this omitted
    /// preserves the original resource default, including when cloning it.
    pub fn with_execution_policy(mut self, policy: hagency_core::execution::ExecutionPolicy) -> Self {
        self.execution_policy = Some(policy);
        self
    }
    pub fn with_legacy_lease(
        mut self,
        lease: crate::installation_authority::LegacyResourceLease,
    ) -> Result<Self, Error> {
        self.gate = self.gate.with_legacy_lease(lease)?;
        Ok(self)
    }
    pub fn with_engagement(mut self, change: ResourceEngagementChange) -> Result<Self, Error> {
        if change.eligible_managers.is_empty()
            || change.eligible_managers.len() > 64
            || change.allocation_id.is_some() != change.expected_revision.is_some()
            || (self.preset.is_some() && change.allocation_id.is_some())
            || !matches!(self.ceiling, CeilingChange::Monthly { .. })
        {
            return Err(
                hagency_core::InvalidInput("invalid engagement resource configuration").into(),
            );
        }
        self.engagement = Some(change);
        Ok(self)
    }
    pub(crate) fn weight(&self) -> u32 {
        24 * 1024
    }
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceConfigurationResult {
    pub resource_id: String,
    pub revision: String,
    pub published: bool,
    /// Present only when the command amended an offered resource's monthly
    /// maximum; the definition and `revision` were then left unchanged.
    /// Omission keeps every earlier recorded result byte-identical.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ceiling_amendment: Option<CeilingAmendmentResult>,
}
/// The amendment a command recorded: `sequence` is its position in the
/// resource's chain, `previous_tokens` the maximum it replaced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CeilingAmendmentResult {
    pub sequence: u64,
    pub previous_tokens: u64,
    pub tokens: u64,
}
impl DomainRepository {
    pub fn configure_resource(
        &mut self,
        command: ResourceConfigurationCommand,
    ) -> Result<ResourceConfigurationResult, Error> {
        self.configure_resource_clock(command, Instant::now)
    }
    fn configure_resource_clock(
        &mut self,
        command: ResourceConfigurationCommand,
        mut clock: impl FnMut() -> Instant,
    ) -> Result<ResourceConfigurationResult, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let gate = &command.gate;
        let revoked = gate.access.revoked.try_lock().map_err(lock_error)?;
        let check = |now| gate.check_at(*revoked, now);
        check(clock())?;
        if let Some(receipt) = &command.receipt
            && let Some(result) = receipt.lookup(&tx)?
        {
            check(clock())?;
            return Ok(result);
        }
        let mut resource = read_resource(&tx, &gate.resource)?;
        self.accounts.check_resource(&tx, &resource)?;
        let source = resource.clone();
        if resource_publication_revision(&resource)? != gate.revision {
            return Err(Error::Conflict);
        }
        let create = command.preset.is_some();
        if let Some(policy) = command.execution_policy {
            policy.validate_for(&resource.framework)?;
            resource.execution_policy = Some(policy);
        }
        if let ProfileChange::Select { model, reasoning } = command.profile {
            if !qualification::configuration_choices(&resource.profile())?
                .iter()
                .any(|c| c.model == model && c.reasoning == reasoning)
            {
                return Err(hagency_core::InvalidInput(
                    "profile is not a selectable configuration",
                )
                .into());
            }
            resource.model = model;
            resource.reasoning = reasoning;
        }
        // A new configuration must use a supported framework and qualified pair.
        // Existing unsupported configurations can preserve their exact profile.
        if create
            && !qualification::configuration_choices(&resource.profile())?
                .iter()
                .any(|c| c.model == resource.model && c.reasoning == resource.reasoning)
        {
            return Err(
                hagency_core::InvalidInput("source profile cannot create a configuration").into(),
            );
        }
        match command.ceiling {
            CeilingChange::Preserve {} => {}
            CeilingChange::Clear {} => resource.ceiling = None,
            CeilingChange::Monthly { tokens } => {
                resource.ceiling = Some(serde_json::from_value::<Ceiling>(
                    serde_json::json!({"tokens":tokens,"period":"monthly"}),
                )?);
            }
        }
        if let Some(preset) = command.preset {
            resource.preset_id = preset;
        }
        if create {
            super::accounts::copy_association(&tx, &source, &resource)?;
        }
        let grant = if let Some(change) = command.engagement {
            let existing = super::coordinator::resource_grants(&tx, &resource.id())?;
            let amount = resource
                .ceiling
                .as_ref()
                .and_then(|c| c.tokens)
                .ok_or(Error::NoCeiling)?;
            let (id, revision) = if let Some(id) = change.allocation_id {
                let previous = existing
                    .iter()
                    .find(|g| g.id == id)
                    .ok_or(Error::Conflict)?;
                if Some(previous.revision) != change.expected_revision
                    || previous.server_engagement_id != change.server_engagement_id
                {
                    return Err(Error::Conflict);
                }
                (
                    id,
                    u64::from(previous.revision)
                        .checked_add(1)
                        .ok_or(Error::Capacity)?,
                )
            } else {
                if !existing.is_empty() {
                    return Err(Error::Conflict);
                }
                (
                    serde_json::from_value(serde_json::json!(format!("grant_{}", resource.id())))?,
                    1,
                )
            };
            // Older installations may share one profile between several grants.
            // Its compatibility ceiling is derived from those budgets, never a
            // second user-controlled pool; identities and held agents stay put.
            let total =
                existing
                    .iter()
                    .filter(|g| g.id != id)
                    .try_fold(u64::from(amount), |n, g| {
                        n.checked_add(u64::from(g.allocated_tokens))
                            .ok_or(Error::Capacity)
                    })?;
            resource.ceiling = Some(serde_json::from_value(
                serde_json::json!({"tokens":total,"period":"monthly"}),
            )?);
            Some(super::coordinator::ResourceGrant {
                id,
                server_engagement_id: change.server_engagement_id,
                resource_id: resource.id(),
                revision: serde_json::from_value(serde_json::json!(revision))?,
                allocated_tokens: serde_json::from_value(serde_json::json!(u64::from(amount)))?,
                eligible_managers: change.eligible_managers,
            })
        } else {
            None
        };
        let resource = prepare_resource_write(&tx, &resource, create.then_some(true), create)?;
        let result = ResourceConfigurationResult {
            resource_id: resource.id(),
            revision: resource_publication_revision(&resource)?,
            published: resource.published,
            ceiling_amendment: None,
        };
        check(clock())?;
        self.accounts.check_resource(&tx, &resource)?;
        write_resource_configuration(&tx, &resource, create)?;
        if let Some(grant) = &grant {
            super::coordinator::put_resource_transaction(
                &tx,
                &self.accounts,
                grant,
                super::graphs::now_ms()?,
            )?;
        }
        check(clock())?;
        self.accounts.check_resource(&tx, &resource)?;
        if let Some(receipt) = &command.receipt {
            receipt.record(&tx, &result)?;
            check(clock())?;
        }
        tx.commit()?;
        self.accounts
            .check_resource(&self.db, &resource)
            .map_err(|_| Error::OutcomeUnknown)?;
        check(clock()).map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DomainStore;
    use hagency_core::project::Resource;
    use std::{sync::TryLockError, time::Duration};
    #[tokio::test]
    async fn native_resource_configuration_current() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        let mut db = DomainRepository::open(&state).unwrap();
        let resource: Resource = serde_json::from_value(serde_json::json!({"presetId":"configuration_clock","seatId":"original_clock_account","framework":"codex","model":"gpt-5.6-sol","reasoning":"medium"})).unwrap();
        db.put_resource(&resource).unwrap();
        let revision = resource_publication_revision(&resource).unwrap();
        let prepare = |access: &ResourceConfigurationAccess, deadline| {
            access
                .prepare(
                    resource.id(),
                    revision.clone(),
                    false,
                    ProfileChange::Preserve {},
                    CeilingChange::Monthly {
                        tokens: 1u64.try_into().unwrap(),
                    },
                    deadline,
                )
                .unwrap()
        };
        let inspect = rusqlite::Connection::open(state.join("domain.sqlite3")).unwrap();
        inspect.busy_timeout(Duration::ZERO).unwrap();
        for retire_at in [0, 2, 4] {
            let fence = ResourcePublicationRetirement::default();
            let access = ResourceConfigurationAccess::new(
                Instant::now() + Duration::from_secs(60),
                fence.clone(),
            );
            let command = prepare(&access, Instant::now() + Duration::from_secs(2));
            let original = command.gate.access.clone();
            let mut samples = 0;
            let result = db.configure_resource_clock(command, || {
                samples += 1;
                assert!(matches!(
                    original.revoked.try_lock(),
                    Err(TryLockError::WouldBlock)
                ));
                assert!(matches!(access.revoke(), Err(Error::Busy)));
                let probe = inspect.execute_batch("BEGIN IMMEDIATE");
                if samples < 4 {
                    assert!(probe.is_err());
                } else {
                    probe.unwrap();
                    inspect.execute_batch("ROLLBACK").unwrap();
                }
                if samples == retire_at {
                    fence.retire();
                }
                Instant::now()
            });
            match retire_at {
                0 => assert!(result.is_ok()),
                2 => assert!(matches!(result, Err(Error::LocalAuthority))),
                _ => assert!(matches!(result, Err(Error::OutcomeUnknown))),
            }
            assert_eq!(
                db.resource_configuration(&resource.id())
                    .unwrap()
                    .ceiling
                    .is_some(),
                retire_at != 2
            );
            db.put_resource(&resource).unwrap();
        }
        // Creation collision refuses without replacing the existing resource.
        let access = ResourceConfigurationAccess::new(
            Instant::now() + Duration::from_secs(60),
            ResourcePublicationRetirement::default(),
        );
        let mut command = prepare(&access, Instant::now() + Duration::from_secs(2));
        command.preset = Some(resource.preset_id.clone());
        assert!(matches!(
            db.configure_resource(command),
            Err(Error::Conflict)
        ));
        assert!(
            db.resource_configuration(&resource.id())
                .unwrap()
                .ceiling
                .is_none()
        );
        let store = DomainStore::start(db, 8).unwrap();
        for mode in ["expiry", "deadline", "retirement", "caller_loss"] {
            let now = Instant::now();
            let fence = ResourcePublicationRetirement::default();
            let access = ResourceConfigurationAccess::new(
                now + if mode == "expiry" {
                    Duration::from_millis(60)
                } else {
                    Duration::from_secs(60)
                },
                fence.clone(),
            );
            let deadline = now
                + if mode == "deadline" {
                    Duration::from_millis(60)
                } else {
                    Duration::from_secs(2)
                };
            let command = prepare(&access, deadline);
            assert!(matches!(access.revoke(), Err(Error::Busy)));
            let other = ResourceConfigurationAccess::new(
                now + Duration::from_secs(60),
                ResourcePublicationRetirement::default(),
            );
            drop(prepare(&other, deadline));
            other.revoke().unwrap();
            inspect.execute_batch("BEGIN IMMEDIATE").unwrap();
            let mut ahead = Box::pin(store.put_resource(resource.clone()));
            assert!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(ahead.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            let mut pending = Box::pin(store.configure_resource(command));
            assert!(
                std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            if mode == "retirement" {
                fence.retire();
            }
            if mode == "caller_loss" {
                drop(pending);
            } else {
                if mode != "retirement" {
                    tokio::time::sleep(Duration::from_millis(80)).await;
                }
                inspect.execute_batch("COMMIT").unwrap();
                ahead.await.unwrap();
                let result = pending.await;
                assert!(if mode == "deadline" {
                    matches!(result, Err(Error::OutcomeUnknown))
                } else {
                    matches!(result, Err(Error::LocalAuthority))
                });
                assert!(
                    store
                        .resource_configuration(resource.id())
                        .await
                        .unwrap()
                        .ceiling
                        .is_none()
                );
                access.revoke().unwrap();
                continue;
            }
            inspect.execute_batch("COMMIT").unwrap();
            ahead.await.unwrap();
            assert!(
                store
                    .resource_configuration(resource.id())
                    .await
                    .unwrap()
                    .ceiling
                    .is_none()
            );
            access.revoke().unwrap();
        }
        // Per-session queue custody is finite and dropped preparation frees it.
        let access = ResourceConfigurationAccess::new(
            Instant::now() + Duration::from_secs(60),
            ResourcePublicationRetirement::default(),
        );
        let commands = (0..8)
            .map(|_| prepare(&access, Instant::now() + Duration::from_secs(2)))
            .collect::<Vec<_>>();
        assert!(matches!(
            access.prepare(
                resource.id(),
                revision,
                false,
                ProfileChange::Preserve {},
                CeilingChange::Preserve {},
                Instant::now() + Duration::from_secs(2)
            ),
            Err(Error::Busy)
        ));
        drop(commands);
        access.revoke().unwrap();
        store.shutdown().await.unwrap();
    }
}
