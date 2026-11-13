//! Explicitly enabled first shared text turn on a retained actual provider.
//! Original central commands, local holds and process evidence remain distinct.
mod context_io;
mod launch;
mod claude_run;
mod octos_capture;
mod octos_run;
mod claude_workspace;
mod e2ee;
pub(in crate::console) mod history;
pub(in crate::console) mod files;
mod approvals;
pub(in crate::console) mod questions;
mod demand_io;
mod demands;
mod io;
mod guarded;
mod stop_capture;
mod run_controls;
mod cooperative;
use guarded::{guarded, reply_settled};
pub use demands::{
    SharedDemandPoll, SharedDemandProgress, SharedDemandResume, SharedDemandStopReceipt,
    SharedDemandStopResume,
};
mod accepted_start;
mod attempt_io;
mod attempt_run;
mod attempts;
mod successor_inventory;
pub use accepted_start::{
    PreparedSharedSuccessor, SharedSuccessorResume, SharedSuccessorReview,
    SharedSuccessorAssessment, SharedSuccessorBudgetReview, SharedSuccessorBlockReason,
    SharedSuccessorDescription,
};
pub use successor_inventory::{SharedSuccessorPage, SharedSuccessorSummary};
mod manager;
mod run;
use super::{NativeError, NativeManagement, failure};
use hagency_store::{
    management::Permission,
    owner_resources::{shared_execution::*, *},
};
pub(in crate::console::native) use io::ServerOperation;
use io::{Action, Bound, DeliveryReply, Io, Timed};
pub(super) use manager::Manager;
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
fn invalid() -> NativeError {
    failure(502, "invalid_shared_execution_response")
}
fn store(e: hagency_store::Error) -> NativeError {
    super::management::store_error(e)
}
fn digest(v: &impl Serialize) -> Result<String, NativeError> {
    hagency_core::canonical::digest(&serde_json::to_value(v).map_err(|_| invalid())?)
        .map_err(|_| invalid())
}
fn hash(v: &str) -> Result<(), NativeError> {
    if v.len() == 64
        && v.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(invalid())
    }
}
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SharedExecutionProgress {
    Idle,
    Running {
        command_id: String,
    },
    Stopping {
        command_id: String,
    },
    HistoricalEvidenceMissing {
        command_id: String,
    },
    Recorded {
        command_id: String,
        observation_id: String,
        /// Exact saved receipt state, absent when no receipt exists yet.
        #[serde(skip_serializing_if = "Option::is_none")]
        process_state: Option<String>,
    },
}
#[derive(Clone, Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SharedWorkRecoveryProgress {
    EvidenceMissing {
        work_id: String,
    },
    Recorded {
        work_id: String,
        observation_id: String,
        reply_state: Option<String>,
    },
}
pub struct SharedExecutionTransport {
    manager: Arc<NativeManagement>,
    io: Io,
}
impl SharedExecutionTransport {
    #[cfg(feature = "shared-intake")]
    pub(super) fn owner_stop_only(
        manager: Arc<NativeManagement>,
        gate: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Self {
        let mut io = Io::for_owner_stop(&manager);
        io.restrict_to_host(gate);
        Self { manager, io }
    }

    pub fn new(manager: Arc<NativeManagement>) -> Self {
        Self {
            io: Io::new(&manager),
            manager,
        }
    }
    #[cfg(feature = "resource-adoption")]
    pub(super) fn restrict_to_host(mut self, gate: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.io.restrict_to_host(gate);
        self
    }
    pub async fn poll(
        &self,
        r: &OwnerResourceRegistry,
        after: Option<String>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.process(r, Action::Poll { after }, None, None).await
    }
    pub async fn recover(
        &self,
        r: &OwnerResourceRegistry,
        id: String,
        command_digest: String,
        observation: Option<String>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.process(
            r,
            Action::Recover {
                id: id.clone(),
                digest: command_digest.clone(),
                observation: observation.clone(),
            },
            Some((id, command_digest, observation)),
            None,
        )
        .await
    }
    #[cfg(all(test, feature = "shared-execution"))]
    pub(super) async fn poll_fixture(
        &self,
        r: &OwnerResourceRegistry,
        launch: super::managed_runtime::PreparedLaunch,
    ) -> Result<SharedExecutionProgress, NativeError> {
        self.process(r, Action::Poll { after: None }, None, Some(launch.into()))
            .await
    }
    #[cfg(all(test, feature = "shared-execution"))]
    pub(super) async fn poll_claude_fixture(
        &self, r: &OwnerResourceRegistry, resource_id: String, root: std::path::PathBuf,
        binding: hagency_runtime::claude::account::AccountBinding, mode: &str,
    ) -> Result<SharedExecutionProgress, NativeError> {
        let b = self.io.bound().await?;
        let resource = r.resource(&b.scope.profile, resource_id).await.map_err(store)?;
        let launch = launch::ClaudeLaunch::fixture(r.clone(), b.scope.profile, &resource, root, binding, mode);
        self.process(r, Action::Poll { after: None }, None, Some(launch::Launch::Claude(launch))).await
    }
    #[cfg(all(test, feature = "shared-execution"))]
    pub(super) async fn poll_octos_fixture(
        &self, r: &OwnerResourceRegistry, resource_id: String, home: std::path::PathBuf,
        root: std::path::PathBuf, profile_id: String,
    ) -> Result<SharedExecutionProgress, NativeError> {
        let b = self.io.bound().await?;
        let resource = r.resource(&b.scope.profile, resource_id).await.map_err(store)?;
        let launch = launch::OctosLaunch::fixture(r.clone(), b.scope.profile, &resource, home, root, profile_id);
        self.process(r, Action::Poll { after: None }, None, Some(launch::Launch::Octos(launch))).await
    }
    /// Historical accounting upload only. Neither missing evidence nor a server
    /// recovery response can authorize another process or model turn.
    pub async fn recover_work(
        &self,
        r: &OwnerResourceRegistry,
        command_id: String,
        command_digest: String,
        work_id: String,
        observation: Option<String>,
    ) -> Result<SharedWorkRecoveryProgress, NativeError> {
        if !cfg!(feature = "shared-execution") {
            return Err(failure(409, "shared_execution_disabled"));
        }
        super::key(&work_id)?;
        if let Some(id) = &observation {
            super::key(id)?;
        }
        let b = self.io.bound().await?;
        let reply: Timed<DeliveryReply> = b
            .request(
                Action::Recover {
                    id: command_id.clone(),
                    digest: command_digest.clone(),
                    observation: None,
                },
                false,
            )
            .await?;
        let d = reply.value.delivery.ok_or_else(invalid)?;
        io::binding(&b.scope, &d.command, false)?;
        if d.execution_allowed
            || d.command.command_id != command_id
            || d.command_digest != command_digest
            || digest(&d.command)? != command_digest
        {
            return Err(invalid());
        }
        let a = b.cleanup(r, &d.command).await?;
        let local = r
            .recover_shared_work(a, d.command.clone(), work_id.clone(), observation)
            .await
            .map_err(store)?;
        b.transport.check_scope(&b.scope, false).await?;
        let Some(saved) = local else {
            return Ok(SharedWorkRecoveryProgress::EvidenceMissing { work_id });
        };
        let Some(receipt) = saved.receipt else {
            return Ok(SharedWorkRecoveryProgress::EvidenceMissing { work_id });
        };
        let reply_state = run::report_work(&self.io, r, &d.command, &saved.work, &receipt).await?;
        Ok(SharedWorkRecoveryProgress::Recorded {
            work_id,
            observation_id: receipt.observation_id,
            reply_state,
        })
    }
    async fn process(
        &self,
        r: &OwnerResourceRegistry,
        action: Action,
        historical: Option<(String, String, Option<String>)>,
        prepared: Option<launch::Launch>,
    ) -> Result<SharedExecutionProgress, NativeError> {
        if !cfg!(feature = "shared-execution") {
            return Err(failure(409, "shared_execution_disabled"));
        }
        let b = self.io.bound().await?;
        let started = Instant::now();
        let reply: Timed<DeliveryReply> = b.request(action, false).await?;
        let Some(d) = reply.value.delivery else {
            return if historical.is_none() {
                Ok(SharedExecutionProgress::Idle)
            } else {
                Err(invalid())
            };
        };
        io::binding(&b.scope, &d.command, d.execution_allowed)?;
        if d.command_digest != digest(&d.command)?
            || historical
                .as_ref()
                .is_some_and(|(id, h, _)| id != &d.command.command_id || h != &d.command_digest)
            || historical.is_some() && d.execution_allowed
            || d.recorded.is_some() && d.execution_allowed
            || d.execution_allowed && d.cleanup_allowed
            || historical.is_some() && d.cleanup_allowed
        {
            return Err(invalid());
        }
        let until = io::window(
            &d.token,
            d.lease_until_ms,
            reply.observed,
            started,
            &b.scope,
        )?;
        let c = &d.command;
        if historical.is_none()
            && d.cleanup_allowed
            && self
                .manager
                .owner
                .0
                .shared_execution
                .request_stop(&c.command_id, &d.command_digest)?
        {
            return Ok(SharedExecutionProgress::Stopping {
                command_id: c.command_id.clone(),
            });
        }
        if historical.is_none()
            && self
                .manager
                .owner
                .0
                .shared_execution
                .running(&c.command_id, &d.command_digest)?
        {
            return Ok(SharedExecutionProgress::Running {
                command_id: c.command_id.clone(),
            });
        }
        let a = b.cleanup(r, c).await?;
        let mut local = r
            .recover_shared_process(a, c.clone(), historical.as_ref().and_then(|v| v.2.clone()))
            .await
            .map_err(store)?;
        b.transport.check_scope(&b.scope, false).await?;
        if local.is_none()
            && !d.execution_allowed
            && d.cleanup_allowed
            && historical.is_none()
            && c.reserve_command
                .pool_grant
                .as_ref()
                .is_some_and(|grant| grant.calendar.is_some())
        {
            // Only the native atomic no-issuance proof can retire an expired
            // original. A missing receipt or elapsed lease alone never proves
            // that a process did not run.
            let authority = b.cleanup(r, c).await?;
            local = r
                .record_unstarted_shared_runtime(authority, c.clone())
                .await
                .map_err(store)?;
            b.transport.check_scope(&b.scope, false).await?;
        }
        if local
            .as_ref()
            .is_some_and(|receipt| receipt.state != "not_started")
        {
            // Process history alone is insufficient: durable work observations
            // may still need reporting after a lost reply or process restart.
            run::recover_process_work(&self.io, r, c).await.map_err(|error| {
                tracing::debug!(status = error.status, code = %error.code, "Shared process historical work recovery pending");
                error
            })?;
        }
        if let Some(saved) = &d.recorded {
            saved.validate_for(c).map_err(store)?;
            if historical
                .as_ref()
                .and_then(|v| v.2.as_deref())
                .is_some_and(|id| id != saved.observation_id)
            {
                return Err(invalid());
            }
            if let Some(latest) = &local {
                if latest.ledger_revision > saved.ledger_revision {
                    run::report_process(&self.io, r, c, latest).await.map_err(|error| {
                        tracing::debug!(status = error.status, code = %error.code, "Shared process latest receipt recovery pending");
                        error
                    })?;
                    return Ok(SharedExecutionProgress::Recorded {
                        command_id: c.command_id.clone(),
                        observation_id: latest.observation_id.clone(),
                        process_state: Some(latest.state.clone()),
                    });
                }
                if latest.ledger_revision == saved.ledger_revision
                    && digest(latest)? != digest(saved)?
                {
                    return Err(invalid());
                }
            }
            if local.as_ref().is_some_and(|l| {
                l.observation_id == saved.observation_id && digest(l).ok() != digest(saved).ok()
            }) {
                return Err(invalid());
            }
            return Ok(SharedExecutionProgress::Recorded {
                command_id: c.command_id.clone(),
                observation_id: saved.observation_id.clone(),
                process_state: Some(saved.state.clone()),
            });
        }
        if let Some(local) = local {
            run::upload_process(&b, &d, until, &local).await?;
            return Ok(SharedExecutionProgress::Recorded {
                command_id: c.command_id.clone(),
                observation_id: local.observation_id,
                process_state: Some(local.state.clone()),
            });
        }
        if !d.execution_allowed {
            return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                command_id: c.command_id.clone(),
            });
        }
        let e = b.enrollment(r, c, true).await?;
        let proof =
            VerifiedSharedRuntimeDelivery::new(e, c.clone(), d.command_digest.clone(), true, until)
                .map_err(store)?;
        let Some(proof) = r
            .prepare_shared_runtime_delivery(proof)
            .await
            .map_err(store)?
        else {
            return Ok(SharedExecutionProgress::HistoricalEvidenceMissing {
                command_id: c.command_id.clone(),
            });
        };
        let resource = r
            .resource(&b.scope.profile, c.reserve_command.resource_id.clone())
            .await
            .map_err(store)?;
        if resource.framework != c.runtime().map_err(store)?
            || resource.revision != c.reserve_command.resource_revision
        {
            return Err(failure(409, "shared_execution_resource_changed"));
        }
        let launch = if let Some(prepared) = prepared {
            prepared
        } else {
            let root = self
                .manager
                .owner
                .0
                .host
                .console
                .0
                .server_login
                .state_directory()
                .ok_or_else(|| failure(409, "shared_execution_launch_unavailable"))?;
            launch::Launch::for_resource(&root, &b.scope.device, r, &b.scope.profile, &resource)
                .await?
        };
        let launch = launch.with_agent_workspace(&c.reserve_command.proposed_agent_id)?;
        let process_io = launch.io(&self.io);
        let io = process_io.clone();
        let command = c.clone();
        let check: super::managed_runtime::LiveCheck = Arc::new(move || {
            let io = io.clone();
            let c = command.clone();
            Box::pin(async move { io.check(&c).await })
        });
        let launch = launch.prepare(&self.manager, until, check).await?;
        b.transport.check_scope(&b.scope, false).await?;
        self.manager
            .owner
            .0
            .shared_execution
            .start(
                r.clone(),
                proof,
                launch,
                process_io,
                d,
                reply.observed,
                started,
                until,
            )?
            .await
            .map_err(|_| failure(409, "shared_execution_outcome_unknown"))?
    }
}

#[cfg(feature = "shared-budget-execution")]
mod budget_io;
#[cfg(feature = "shared-budget-execution")]
mod budget_attempts;
#[cfg(feature = "shared-budget-execution")]
mod budget_run;
#[cfg(feature = "shared-budget-execution")]
mod budget_work_io;
#[cfg(feature = "shared-budget-execution")]
mod budget_context_io;

#[cfg(feature = "shared-budget-execution")]
mod budget_demand_io;
#[cfg(feature = "shared-budget-execution")]
mod budget_demands;
