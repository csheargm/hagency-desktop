use super::super::allocation_transport::{AllocationTransport, Operation, Scope};
use super::*;
use crate::console::server_login::OwnerError;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Weak;

pub(in crate::console::native) struct ServerOperation(Box<Action>, bool);
pub(super) enum Action {
    SessionRoute { id: String, input: SharedSessionRouteInput },
    Crypto(super::e2ee::CryptoAction),
    ApprovalRegister {
        authority: SharedActivityRef,
        work: SharedWorkItem,
        id: String,
        request: Value,
        remaining_ms: u64,
    },
    ApprovalTake {
        authority: SharedActivityRef,
        id: String,
        digest: String,
    },
    #[cfg(feature = "shared-budget-execution")]
    BudgetDemand(Box<super::budget_demand_io::BudgetDemandAction>),
    #[cfg(feature = "shared-budget-execution")]
    Budget(Box<super::budget_io::AttemptAction>),
    #[cfg(feature = "shared-budget-execution")]
    BudgetWork(Box<super::budget_work_io::WorkAction>),
    Context(Box<super::context_io::ContextAction>),
    Demand(Box<super::demand_io::DemandAction>),
    Attempt(Box<super::attempt_io::AttemptAction>),
    Poll {
        after: Option<String>,
    },
    Recover {
        id: String,
        digest: String,
        observation: Option<String>,
    },
    Process {
        id: String,
        input: SharedProcessInput,
    },
    Renew(SharedActivityRenewInput),
    WorkPoll(SharedActivityRef),
    WorkAck(SharedWorkInput),
    WorkStart(SharedWorkInput),
    WorkRecover {
        id: String,
        input: SharedWorkRecoverInput,
    },
    WorkResult {
        id: String,
        input: Box<SharedWorkResultUpload>,
    },
}
impl ServerOperation {
    #[cfg(test)]
    pub(in crate::console::native) fn protocol_test_poll() -> Self {
        Self::new(Action::Poll { after: None }).expect("valid sealed worker poll")
    }

    fn new(action: Action) -> Result<Self, NativeError> {
        match &action {
            Action::SessionRoute { id, input } => {
                super::super::key(id)?;
                input.validate().map_err(store)?;
            }
            Action::Crypto(_) => {}
            Action::ApprovalRegister {
                authority,
                work,
                id,
                request,
                remaining_ms,
            } => {
                authority.validate().map_err(store)?;
                work.validate().map_err(store)?;
                super::super::key(id)?;
                if *remaining_ms == 0
                    || *remaining_ms > 120_000
                    || serde_json::to_vec(request).map_or(true, |v| v.len() > 64 * 1024)
                {
                    return Err(invalid());
                }
            }
            Action::ApprovalTake {
                authority,
                id,
                digest,
            } => {
                authority.validate().map_err(store)?;
                super::super::key(id)?;
                hash(digest)?;
            }
            #[cfg(feature = "shared-budget-execution")]
            Action::BudgetDemand(a) => a.validate()?,
            #[cfg(feature = "shared-budget-execution")]
            Action::Budget(a) => a.validate()?,
            #[cfg(feature = "shared-budget-execution")]
            Action::BudgetWork(a) => a.validate()?,
            Action::Context(a) => a.validate()?,
            Action::Attempt(a) => a.validate()?,
            Action::Demand(a) => a.validate()?,
            Action::Poll { after: Some(v) } => super::super::key(v)?,
            Action::Poll { after: None } => {}
            Action::Recover {
                id,
                digest,
                observation,
            } => {
                super::super::key(id)?;
                hash(digest)?;
                if let Some(v) = observation {
                    super::super::key(v)?;
                }
            }
            Action::Process { id, input } => {
                super::super::key(id)?;
                hash(&input.command_digest)?;
                hash(&input.token)?;
            }
            Action::Renew(v) => {
                v.authority.validate().map_err(store)?;
                super::super::key(&v.renewal_id)?;
            }
            Action::WorkPoll(v) => v.validate().map_err(store)?,
            Action::WorkAck(v) | Action::WorkStart(v) => {
                v.authority.validate().map_err(store)?;
                super::super::key(&v.work_id)?;
                hash(&v.work_digest)?;
            }
            Action::WorkRecover { id, input } => {
                super::super::key(id)?;
                hash(&input.work_digest)?;
                hash(&input.runtime_command_digest)?;
                if let Some(v) = &input.observation_id {
                    super::super::key(v)?;
                }
            }
            Action::WorkResult { id, input } => {
                super::super::key(id)?;
                hash(&input.work_digest)?;
                hash(&input.token)?;
            }
        }
        Ok(Self(Box::new(action), false))
    }
    pub(in crate::console::native) fn request(
        self,
    ) -> Result<(reqwest::Method, String, Option<Value>), OwnerError> {
        if matches!(&*self.0,Action::Demand(a) if matches!(&**a,super::demand_io::DemandAction::Discovery))
        {
            return Ok((
                reqwest::Method::GET,
                "/hagency/api/v1/discovery".into(),
                None,
            ));
        }
        let read = matches!(&*self.0,Action::Demand(a) if matches!(&**a,super::demand_io::DemandAction::Control{..}));
        let (mut path, body) = match *self.0 {
            Action::SessionRoute { id, input } => (format!("runtime/{id}/session-route"), json!(input)),
            Action::Crypto(v) => v.body(),
            Action::ApprovalRegister {
                authority,
                work,
                id,
                request,
                remaining_ms,
            } => (
                "approvals/register".into(),
                json!({"authority":authority,"workId":work.id,"workDigest":digest(&work).map_err(|_|OwnerError{status:400,code:"invalid_shared_work".into()})?,"id":id,"request":request,"remainingMs":remaining_ms}),
            ),
            Action::ApprovalTake {
                authority,
                id,
                digest,
            } => (
                "approvals/take".into(),
                json!({"authority":authority,"id":id,"digest":digest}),
            ),
            #[cfg(feature = "shared-budget-execution")]
            Action::BudgetDemand(a) => a.body(),
            #[cfg(feature = "shared-budget-execution")]
            Action::Budget(a) => a.body(),
            #[cfg(feature = "shared-budget-execution")]
            Action::BudgetWork(a) => a.body(),
            Action::Context(a) => a.body(),
            Action::Attempt(a) => a.body(),
            Action::Demand(a) => a.body(),
            Action::Poll { after } => ("runtime/poll".into(), json!({"after":after})),
            Action::Recover {
                id,
                digest,
                observation,
            } => (
                format!("runtime/{id}/recover"),
                json!({"commandDigest":digest,"observationId":observation}),
            ),
            Action::Process { id, input } => (format!("runtime/{id}/result"), json!(input)),
            Action::Renew(input) => ("activity/renew".into(), json!(input)),
            Action::WorkPoll(authority) => ("work/poll".into(), json!({"authority":authority})),
            Action::WorkAck(input) => ("work/ack".into(), json!(input)),
            Action::WorkStart(input) => ("work/start".into(), json!(input)),
            Action::WorkRecover { id, input } => (format!("work/{id}/recover"), json!(input)),
            Action::WorkResult { id, input } => (format!("work/{id}/result"), json!(input)),
        };
        if serde_json::to_vec(&body).map_or(true, |v| {
            v.len()
                > if path.starts_with("approvals/") {
                    96 * 1024
                } else if path.contains("work/") && path.ends_with("/result") {
                    64 * 1024
                } else {
                    16 * 1024
                }
        }) {
            return Err(OwnerError {
                status: 400,
                code: "invalid_shared_execution_payload".into(),
            });
        }
        if self.1
            && matches!(
                path.as_str(),
                "runtime/poll"
                    | "attempts/poll"
                    | "budget-attempts/poll"
                    | "demands/poll"
                    | "budget-demands/poll"
            )
        {
            path.push_str("-v4");
        }
        Ok((
            if read {
                reqwest::Method::GET
            } else {
                reqwest::Method::POST
            },
            format!("/hagency/api/v1/management/shared-execution/{path}"),
            if read { None } else { Some(body) },
        ))
    }
}
/// Current SDK-owned scopes are reacquired per operation; a retained runtime
/// does not retain a strong NativeOwner cycle or stale authentication lease.
#[derive(Clone)]
pub(super) struct Io {
    permission: Permission,
    manager: Weak<NativeManagement>,
    host_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    namespace: Option<Arc<hagency_store::owner_resources::ResourceNamespace>>,
    provider_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    claude_discovery: Arc<tokio::sync::Mutex<Option<(Instant, bool)>>>,
    #[cfg(test)]
    pub(super) crypto_fixture: Option<Arc<super::e2ee::CryptoFixture>>,
}
pub(super) struct Bound {
    permission: Permission,
    pub transport: AllocationTransport,
    pub scope: Scope,
    namespace: Option<Arc<hagency_store::owner_resources::ResourceNamespace>>,
    provider_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    claude_discovery: Arc<tokio::sync::Mutex<Option<(Instant, bool)>>>,
}
impl Io {
    pub(super) fn hosting_admission(
        &self,
    ) -> Result<Arc<crate::console::hosting::Admission>, NativeError> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| failure(409, "shared_execution_closed"))?;
        Ok(manager.owner.0.host.console.hosting_admission())
    }
    pub(super) fn crypto_directory(&self) -> Result<std::path::PathBuf, NativeError> {
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| failure(409, "shared_execution_closed"))?;
        let root = manager
            .owner
            .0
            .host
            .console
            .0
            .server_login
            .state_directory()
            .ok_or_else(|| failure(409, "shared_crypto_storage_unavailable"))?
            .join("shared-matrix-devices");
        hagency_store::private::directory(&root).map_err(store)?;
        Ok(root)
    }

    pub fn new(manager: &Arc<NativeManagement>) -> Self {
        Self {
            manager: Arc::downgrade(manager),
            permission: Permission::ExecuteManagedAgents,
            host_gate: None,
            namespace: None,
            provider_gate: None,
            claude_discovery: Arc::default(),
            #[cfg(test)]
            crypto_fixture: None,
        }
    }
    #[cfg(feature = "shared-intake")]
    pub fn for_owner_stop(manager: &Arc<NativeManagement>) -> Self {
        Self {
            permission: Permission::ChangeAgentRunControl,
            ..Self::new(manager)
        }
    }
    pub fn with_provider_gate(&self, gate: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            provider_gate: Some(gate),
            ..self.clone()
        }
    }
    pub fn with_namespace(
        &self,
        namespace: Option<Arc<hagency_store::owner_resources::ResourceNamespace>>,
    ) -> Self {
        Self {
            namespace,
            ..self.clone()
        }
    }
    pub fn restrict_to_host(&mut self, gate: Arc<dyn Fn() -> bool + Send + Sync>) {
        let prior = self.host_gate.take();
        self.host_gate = Some(Arc::new(move || {
            gate() && prior.as_ref().is_none_or(|p| p())
        }));
    }
    pub async fn bound(&self) -> Result<Bound, NativeError> {
        if self.host_gate.as_ref().is_some_and(|gate| !gate()) {
            return Err(failure(401, "management_scope_changed"));
        }
        let manager = self
            .manager
            .upgrade()
            .ok_or_else(|| failure(409, "shared_execution_closed"))?;
        let mut transport = AllocationTransport::new(manager);
        if let Some(gate) = &self.host_gate {
            transport = transport.restrict_to_host(gate.clone());
        }
        let scope = transport.scope(self.permission).await?;
        Ok(Bound {
            transport,
            scope,
            permission: self.permission,
            namespace: self.namespace.clone(),
            provider_gate: self.provider_gate.clone(),
            claude_discovery: self.claude_discovery.clone(),
        })
    }
    pub async fn check(&self, c: &SharedRuntimeCommand) -> Result<(), NativeError> {
        let b = self.bound().await?;
        binding(&b.scope, c, true)?;
        b.check_namespace(c).await?;
        Ok(())
    }
    /// The same check from inside a running process. Around each of the
    /// daemon's SDK credential refreshes (every few minutes) the owner's
    /// management boundary briefly reports a changed scope, a lost
    /// authorization or an unknown outcome. Worker steps retry on their next
    /// pass, but a run ended on the first one and its message went unanswered.
    /// Such codes are retried for a short while; a real revocation still ends
    /// the run once they persist. Callers keep the run's cancellation and
    /// activity deadline around this.
    pub async fn check_running(&self, c: &SharedRuntimeCommand) -> Result<(), NativeError> {
        let settle = std::time::Instant::now() + RUNNING_CHECK_SETTLE;
        loop {
            match self.check(c).await {
                Err(error) if self.transient(&error) && std::time::Instant::now() < settle => {
                    tracing::info!(code = %error.code, "Shared run authority check retrying");
                    tokio::time::sleep(RETRY_GAP).await;
                }
                result => return result,
            }
        }
    }
    /// Hosting stopping, or this management package retired or expired, is a
    /// real revocation: no retry, the run stops now.
    fn revoked(&self) -> bool {
        self.host_gate.as_ref().is_some_and(|gate| !gate())
            || self
                .manager
                .upgrade()
                .is_none_or(|manager| manager.is_retired())
    }
    /// A boundary failure worth waiting out: one of the shared codes, and not a
    /// real revocation.
    pub(super) fn transient(&self, error: &NativeError) -> bool {
        transient_boundary(error) && !self.revoked()
    }
    /// Uploads one activity renewal. If its reply is lost or a boundary check
    /// fails, the SAME renewal (id and authority) is resubmitted under a fresh
    /// scope until the lease ends: the server returns an already applied
    /// renewal again instead of renewing twice, so the run keeps its lease.
    pub(super) async fn upload_renewal<T: DeserializeOwned>(
        &self,
        mut b: Bound,
        action: impl Fn() -> Action,
        until: Instant,
    ) -> Result<(Bound, Timed<T>), NativeError> {
        loop {
            let error = match b.upload(action(), until).await {
                Ok(reply) => return Ok((b, reply)),
                Err(error) => error,
            };
            if !self.transient(&error) || Instant::now() + RETRY_GAP >= until {
                return Err(error);
            }
            tracing::info!(code = %error.code, "Shared activity renewal resubmitting the same renewal");
            tokio::time::sleep(RETRY_GAP).await;
            b = match self.bound().await {
                Ok(fresh) => fresh,
                Err(error) if self.transient(&error) && Instant::now() + RETRY_GAP < until => {
                    tokio::time::sleep(RETRY_GAP).await;
                    self.bound().await?
                }
                Err(error) => return Err(error),
            };
        }
    }
}

const RETRY_GAP: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a running process waits out a transient authority failure.
const RUNNING_CHECK_SETTLE: std::time::Duration = std::time::Duration::from_secs(20);

/// Codes the owner's management boundary reports while its SDK credentials or
/// management session are being renewed.
fn transient_boundary(error: &NativeError) -> bool {
    matches!(
        error.code.as_str(),
        "management_scope_changed"
            | "management_outcome_unknown"
            | "management_access_required"
            | "owner_authorization_required"
            | "matrix_authorization_unavailable"
            | "management_unavailable"
    )
}
impl Bound {
    async fn check_namespace(&self, c: &SharedRuntimeCommand) -> Result<(), NativeError> {
        if self.provider_gate.as_ref().is_some_and(|gate| !gate()) {
            return Err(failure(409, "provider_account_changed"));
        }
        if let Some(namespace) = &self.namespace {
            if !namespace.matches_resource(
                &c.reserve_command.resource_id,
                &c.reserve_command.resource_revision,
            ) {
                return Err(invalid());
            }
            namespace
                .check_with(self.scope.profile.clone())
                .await
                .map_err(|_| failure(409, "shared_execution_namespace_unavailable"))?;
        }
        Ok(())
    }
    pub async fn demand_discovery(&self) -> Result<super::demand_io::Discovery, NativeError> {
        self.transport
            .request(
                &self.scope,
                Operation::SharedExecution(ServerOperation::new(
                    super::demand_io::DemandAction::Discovery.into(),
                )?),
                false,
            )
            .await
    }

    async fn operation(&self, action: Action) -> Result<ServerOperation, NativeError> {
        let poll = match &action {
            Action::Poll { .. } => true,
            Action::Attempt(a) => matches!(**a, super::attempt_io::AttemptAction::Poll { .. }),
            Action::Demand(a) => matches!(**a, super::demand_io::DemandAction::Poll { .. }),
            #[cfg(feature = "shared-budget-execution")]
            Action::Budget(a) => matches!(**a, super::budget_io::AttemptAction::Poll { .. }),
            #[cfg(feature = "shared-budget-execution")]
            Action::BudgetDemand(a) => matches!(
                **a,
                super::budget_demand_io::BudgetDemandAction::Poll { .. }
            ),
            _ => false,
        };
        let mut operation = ServerOperation::new(action)?;
        if poll {
            let mut cached = self.claude_discovery.lock().await;
            let supported = if let Some((at, value)) = *cached
                && at.elapsed() < Duration::from_secs(30)
            {
                value
            } else {
                let supported = match self.demand_discovery().await {
                    Ok(discovery) => {
                        if discovery.capabilities.len() > 64
                            || discovery.capabilities.iter().any(|c| c.len() > 128)
                        {
                            return Err(invalid());
                        }
                        discovery
                            .capabilities
                            .iter()
                            .any(|c| c == "shared-claude-execution-v1")
                    }
                    Err(error) if error.status == 404 => false,
                    Err(error) => return Err(error),
                };
                *cached = Some((Instant::now(), supported));
                supported
            };
            operation.1 = supported;
        }
        Ok(operation)
    }
    fn check_action(&self, action: &Action) -> Result<(), NativeError> {
        if self.permission == Permission::ChangeAgentRunControl
            && !matches!(action,Action::Demand(a) if matches!(&**a,super::demand_io::DemandAction::Stop{..}|super::demand_io::DemandAction::Control{..}|super::demand_io::DemandAction::Discovery))
        {
            return Err(failure(403, "management_permission_required"));
        }
        Ok(())
    }
    pub async fn request<T: DeserializeOwned>(
        &self,
        a: Action,
        uncertain: bool,
    ) -> Result<Timed<T>, NativeError> {
        self.check_action(&a)?;
        self.transport
            .request(
                &self.scope,
                Operation::SharedExecution(self.operation(a).await?),
                uncertain,
            )
            .await
    }
    pub async fn upload<T: DeserializeOwned>(
        &self,
        a: Action,
        until: Instant,
    ) -> Result<Timed<T>, NativeError> {
        self.check_action(&a)?;
        self.transport
            .upload(
                &self.scope,
                Operation::SharedExecution(self.operation(a).await?),
                until,
            )
            .await
    }
    pub async fn enrollment(
        &self,
        r: &OwnerResourceRegistry,
        c: &SharedRuntimeCommand,
        fresh: bool,
    ) -> Result<VerifiedManagementEnrollment, NativeError> {
        binding(&self.scope, c, fresh)?;
        if fresh {
            self.check_namespace(c).await?;
        }
        let grant = if let Some(grant) = &c.reserve_command.pool_grant {
            grant.clone()
        } else {
            r.managed_agent_project_history(
                self.scope.profile.clone(),
                self.scope.device.device_id().into(),
                c.reserve_command.allocation_id.clone(),
                c.reserve_command.allocation_receipt_digest.clone(),
            )
            .await
            .map_err(store)?
            .grant_receipt
        };
        if digest(&grant)? != c.reserve_command.grant_receipt_digest {
            return Err(invalid());
        }
        let e = self
            .transport
            .enrollment(&self.scope, &grant, fresh)
            .await?;
        self.transport.check_scope(&self.scope, false).await?;
        if fresh {
            self.check_namespace(c).await?;
        }
        Ok(e)
    }
    pub async fn cleanup(
        &self,
        r: &OwnerResourceRegistry,
        c: &SharedRuntimeCommand,
    ) -> Result<ManagedCleanupAuthority, NativeError> {
        let e = self.enrollment(r, c, false).await?;
        let a = r
            .shared_cleanup_authority(e, c.clone())
            .await
            .map_err(store)?;
        self.transport.check_scope(&self.scope, false).await?;
        Ok(a)
    }
    /// Read-only lookup used only for non-executable successor deliveries.
    /// Current scope and immutable command checks still bracket missing history.
    pub async fn find_cleanup(
        &self,
        r: &OwnerResourceRegistry,
        c: &SharedRuntimeCommand,
    ) -> Result<Option<ManagedCleanupAuthority>, NativeError> {
        binding(&self.scope, c, false)?;
        self.transport.check_scope(&self.scope, false).await?;
        let grant = if let Some(grant) = &c.reserve_command.pool_grant {
            grant.clone()
        } else {
            let project = r.find_managed_agent_project_history(
                self.scope.profile.clone(),
                self.scope.device.device_id().into(),
                c.reserve_command.allocation_id.clone(),
                c.reserve_command.allocation_receipt_digest.clone(),
            ).await.map_err(store)?;
            let Some(project) = project else {
                self.transport.check_scope(&self.scope, false).await?;
                return Ok(None);
            };
            project.grant_receipt
        };
        if digest(&grant)? != c.reserve_command.grant_receipt_digest {
            return Err(invalid());
        }
        let enrollment = self.transport.enrollment(&self.scope, &grant, false).await?;
        let result = r.find_shared_cleanup_authority(enrollment, c.clone()).await.map_err(store)?;
        self.transport.check_scope(&self.scope, false).await?;
        Ok(result)
    }
}
pub(super) fn binding(s: &Scope, c: &SharedRuntimeCommand, fresh: bool) -> Result<(), NativeError> {
    c.validate(s.device.owner_mxid()).map_err(store)?;
    if c.reserve_command.executor_device_id != s.device.device_id()
        || c.reserve_command.executor_generation > s.device.generation()
        || (fresh && c.reserve_command.executor_generation != s.device.generation())
    {
        return Err(failure(409, "shared_execution_enrollment_changed"));
    }
    Ok(())
}
/// Extract only the mandatory authenticated server timestamp, then decode the
/// remainder into a closed DTO. No arbitrary server fields survive this step.
pub(super) struct Timed<T> {
    pub value: T,
    pub observed: u64,
}
impl<'de, T: DeserializeOwned> Deserialize<'de> for Timed<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(d)?;
        let observed = value
            .as_object_mut()
            .and_then(|o| o.remove("observedAtMs"))
            .and_then(|v| v.as_u64())
            .filter(|v| *v > 0 && *v <= hagency_core::JSON_SAFE_MAX)
            .ok_or_else(|| serde::de::Error::custom("invalid observation"))?;
        let value = serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        Ok(Self { value, observed })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DeliveryReply {
    pub delivery: Option<SharedRuntimeDelivery>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ActivityReply {
    pub activity: SharedActivityLease,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct WorkReply {
    pub work: Option<SharedWorkItem>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AckReply {
    pub acknowledged: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct TicketReply {
    pub ticket: SharedWorkResultTicket,
}
pub(super) fn window(
    token: &str,
    until: u64,
    observed: u64,
    started: Instant,
    scope: &Scope,
) -> Result<Instant, NativeError> {
    hash(token)?;
    let remaining = until
        .checked_sub(observed)
        .filter(|v| *v > 0 && *v <= 30_000)
        .ok_or_else(invalid)?;
    let deadline = (started + Duration::from_millis(remaining)).min(scope.deadline);
    if Instant::now() >= deadline {
        return Err(failure(409, "shared_execution_ticket_expired"));
    }
    Ok(deadline)
}

#[cfg(test)]
mod claude_protocol_tests {
    use super::*;

    #[test]
    fn shared_claude_poll_opt_in_keeps_legacy_bodies_and_effect_routes() {
        fn poll(kind: usize) -> Action {
            match kind {
                0 => Action::Poll {
                    after: Some("original_cursor".into()),
                },
                1 => super::super::attempt_io::AttemptAction::Poll {
                    after: Some("original_cursor".into()),
                }
                .into(),
                2 => super::super::demand_io::DemandAction::Poll {
                    after: Some("original_cursor".into()),
                    retries: true,
                }
                .into(),
                #[cfg(feature = "shared-budget-execution")]
                3 => super::super::budget_io::AttemptAction::Poll {
                    after: Some("original_cursor".into()),
                }
                .into(),
                #[cfg(feature = "shared-budget-execution")]
                4 => super::super::budget_demand_io::BudgetDemandAction::Poll {
                    after: Some("original_cursor".into()),
                }
                .into(),
                _ => unreachable!(),
            }
        }
        let count = if cfg!(feature = "shared-budget-execution") {
            5
        } else {
            3
        };
        for kind in 0..count {
            let legacy = ServerOperation::new(poll(kind)).unwrap().request().unwrap();
            let mut opted = ServerOperation::new(poll(kind)).unwrap();
            opted.1 = true;
            let opted = opted.request().unwrap();
            assert_eq!(legacy.0, opted.0);
            assert_eq!(legacy.2, opted.2);
            assert_eq!(format!("{}-v4", legacy.1), opted.1);
        }
        fn recovery() -> Action {
            Action::Recover {
                id: "immutable_original".into(),
                digest: "a".repeat(64),
                observation: Some("original_receipt".into()),
            }
        }
        let legacy = ServerOperation::new(recovery()).unwrap().request().unwrap();
        let mut opted = ServerOperation::new(recovery()).unwrap();
        opted.1 = true;
        assert_eq!(
            legacy,
            opted.request().unwrap(),
            "discovery cannot rewrite original recovery paths or IDs"
        );
    }
}
