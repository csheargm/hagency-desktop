use super::{
    Call, Failure, Lease, Operation, Package, decode, encode, error,
    journal::{Admission, Journal},
    reviews::Reviews,
};
use crate::native_owner::*;
#[cfg(feature = "resource-adoption")]
use hagency_store::management as service;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

type BridgeFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, NativeError>> + Send + 'a>>;

/// Build only the selected operation's future in its own frame. Merely boxing
/// the old async match still constructs every large branch state in its caller
/// in debug builds, overflowing an ordinary Tokio worker on an inventory read.
#[inline(never)]
fn defer<'a, F, M>(make: M) -> BridgeFuture<'a>
where
    F: std::future::Future<Output = Result<Value, NativeError>> + Send + 'a,
    M: FnOnce() -> F + Send + 'a,
{
    Box::pin(async move { make().await })
}

const LEASE_TTL: Duration = Duration::from_secs(120);
const MAX_CONTROLLERS: usize = 16;
const MAX_MANAGERS: usize = 64;
struct Controller {
    epoch: u64,
    generation: u64,
    expires: Arc<Mutex<Instant>>,
    live: Arc<AtomicBool>,
}
struct Manager {
    controller: String,
    package: Package,
    native: Arc<NativeManagement>,
}
#[derive(Default)]
struct State {
    controllers: HashMap<String, Controller>,
    managers: HashMap<String, Manager>,
}
pub(crate) struct Bridge {
    #[cfg(feature = "resource-adoption")]
    pub(super) grants: Mutex<super::grants::GrantState>,
    state: Mutex<State>,
    journal: Journal,
    pub(super) reviews: Mutex<Reviews>,
}
impl Bridge {
    pub fn open(home: &Path) -> Result<Self, NativeError> {
        Ok(Self {
            #[cfg(feature = "resource-adoption")]
            grants: Mutex::new(super::grants::GrantState::default()),
            state: Mutex::new(State::default()),
            journal: Journal::open(&home.join("management-rpc.sqlite3"))?,
            reviews: Mutex::new(Reviews::default()),
        })
    }
    pub fn revoke_all(&self) {
        #[cfg(feature = "resource-adoption")]
        if let Ok(mut grants) = self.grants.lock() {
            grants.revoke_all();
        }
        if let Ok(mut state) = self.state.lock() {
            for (_, controller) in state.controllers.drain() {
                controller.live.store(false, Ordering::Release);
            }
            for (_, manager) in state.managers.drain() {
                manager.native.retire();
            }
        }
        if let Ok(mut reviews) = self.reviews.lock() {
            reviews.clear();
        }
    }
    /// A Desktop window's controller, as an attach leaves it.
    #[cfg(test)]
    pub(crate) fn attach_for_tests(&self, id: &str, generation: u64) -> Arc<AtomicBool> {
        let live = Arc::new(AtomicBool::new(true));
        self.state.lock().unwrap().controllers.insert(
            id.into(),
            Controller {
                epoch: 1,
                generation,
                expires: Arc::new(Mutex::new(Instant::now() + LEASE_TTL)),
                live: live.clone(),
            },
        );
        live
    }
    fn expire(state: &mut State, generation: u64) {
        let now = Instant::now();
        state.controllers.retain(|_, c| {
            let keep =
                c.generation == generation && c.expires.lock().is_ok_and(|until| *until > now);
            if !keep {
                c.live.store(false, Ordering::Release);
            }
            keep
        });
        let active: std::collections::HashSet<_> = state.controllers.keys().cloned().collect();
        state.managers.retain(|_, m| {
            let keep = active.contains(&m.controller) && !m.native.is_retired();
            if !keep {
                m.native.retire();
            }
            keep
        });
    }
    pub fn validate(&self, id: &str, generation: u64) -> Result<Arc<AtomicBool>, NativeError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| error(503, "daemon_bridge_unavailable"))?;
        Self::expire(&mut state, generation);
        let controller = state
            .controllers
            .get_mut(id)
            .ok_or_else(|| error(401, "daemon_bridge_lease_expired"))?;
        *controller
            .expires
            .lock()
            .map_err(|_| error(503, "daemon_bridge_unavailable"))? = Instant::now() + LEASE_TTL;
        Ok(controller.live.clone())
    }
    pub(super) fn controller_deadline(
        &self,
        id: &str,
        generation: u64,
    ) -> Result<Arc<dyn Fn() -> Option<Instant> + Send + Sync>, NativeError> {
        let live = self.validate(id, generation)?;
        let expires = self
            .state
            .lock()
            .map_err(|_| error(503, "daemon_bridge_unavailable"))?
            .controllers
            .get(id)
            .ok_or_else(|| error(401, "daemon_bridge_lease_expired"))?
            .expires
            .clone();
        Ok(Arc::new(move || {
            if !live.load(Ordering::Acquire) {
                return None;
            }
            expires
                .lock()
                .ok()
                .map(|until| *until)
                .filter(|until| *until > Instant::now())
        }))
    }
    fn package_epoch(&self, controller: &str, package: &Package) -> Result<(), NativeError> {
        let state = self
            .state
            .lock()
            .map_err(|_| error(503, "daemon_bridge_unavailable"))?;
        if state
            .controllers
            .get(controller)
            .is_none_or(|c| c.epoch != package.account_epoch)
        {
            return Err(error(401, "management_scope_changed"));
        }
        Ok(())
    }
    fn manager(
        &self,
        controller: &str,
        id: &str,
        generation: u64,
    ) -> Result<(Arc<NativeManagement>, Package), NativeError> {
        self.validate(controller, generation)?;
        let state = self
            .state
            .lock()
            .map_err(|_| error(503, "daemon_bridge_unavailable"))?;
        let manager = state
            .managers
            .get(id)
            .filter(|m| m.controller == controller)
            .ok_or_else(|| error(401, "daemon_bridge_lease_expired"))?;
        Ok((manager.native.clone(), manager.package.clone()))
    }
    #[cfg(test)]
    pub async fn handle(
        &self,
        request_id: &str,
        owner: &str,
        origin: &str,
        generation: u64,
        native: Option<NativeOwner>,
        owner_root: &Path,
        #[cfg(feature = "resource-adoption")] shared: Option<
            Arc<crate::native_owner::CurrentSharedExecutor>,
        >,
        call: Call,
    ) -> Result<Value, NativeError> {
        let recorded=AtomicBool::new(false);
        self.handle_inner(request_id,owner,origin,generation,native,owner_root,#[cfg(feature="resource-adoption")] shared,call,&recorded).await
    }
    pub async fn handle_recorded(
        &self,
        request_id: &str,
        owner: &str,
        origin: &str,
        generation: u64,
        native: Option<NativeOwner>,
        owner_root: &Path,
        #[cfg(feature = "resource-adoption")] shared: Option<
            Arc<crate::native_owner::CurrentSharedExecutor>,
        >,
        call: Call,
    ) -> Result<Value, Failure> {
        let recorded=AtomicBool::new(false);
        self.handle_inner(request_id,owner,origin,generation,native,owner_root,#[cfg(feature="resource-adoption")] shared,call,&recorded).await.map_err(|error|Failure{status:error.status,code:error.code,terminal:recorded.load(Ordering::Acquire)})
    }
    async fn handle_inner(
        &self,
        request_id: &str,
        owner: &str,
        origin: &str,
        generation: u64,
        native: Option<NativeOwner>,
        owner_root: &Path,
        #[cfg(feature = "resource-adoption")] shared: Option<
            Arc<crate::native_owner::CurrentSharedExecutor>,
        >,
        call: Call,
        receipt_recorded: &AtomicBool,
    ) -> Result<Value, NativeError> {
        match call {
            Call::Attach {
                origin: expected,
                epoch,
            } => {
                if !super::super::same_origin(&expected, &origin) || epoch == 0 {
                    return Err(error(401, "management_scope_changed"));
                }
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| error(503, "daemon_bridge_unavailable"))?;
                Self::expire(&mut state, generation);
                if state.controllers.len() >= MAX_CONTROLLERS {
                    return Err(error(503, "daemon_bridge_busy"));
                }
                let id = super::super::new_request_id();
                state.controllers.insert(
                    id.clone(),
                    Controller {
                        epoch,
                        generation,
                        expires: Arc::new(Mutex::new(Instant::now() + LEASE_TTL)),
                        live: Arc::new(AtomicBool::new(true)),
                    },
                );
                encode(Lease { id, generation })
            }
            Call::Detach { controller } => {
                #[cfg(feature = "resource-adoption")]
                self.grants
                    .lock()
                    .map_err(|_| error(503, "daemon_bridge_unavailable"))?
                    .revoke_controller(&controller);
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| error(503, "daemon_bridge_unavailable"))?;
                if let Some(c) = state.controllers.remove(&controller) {
                    c.live.store(false, Ordering::Release);
                }
                state.managers.retain(|_, m| {
                    if m.controller == controller {
                        m.native.retire();
                        false
                    } else {
                        true
                    }
                });
                Ok(Value::Null)
            }
            Call::Retire {
                controller,
                management,
            } => {
                self.validate(&controller, generation)?;
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| error(503, "daemon_bridge_unavailable"))?;
                if state
                    .managers
                    .get(&management)
                    .is_some_and(|m| m.controller == controller)
                {
                    if let Some(m) = state.managers.remove(&management) {
                        m.native.retire();
                    }
                }
                Ok(Value::Null)
            }
            Call::Management {
                controller,
                package,
            } => {
                let live = self.validate(&controller, generation)?;
                if !package.valid() {
                    return Err(error(400, "invalid_management_package"));
                }
                self.package_epoch(&controller, &package)?;
                let owner = native.ok_or_else(|| error(503, "owner_daemon_hosting_required"))?;
                let manager = owner.management(package.native()).await?;
                manager
                    .bind_controller_lifetime(self.controller_deadline(&controller, generation)?)?;
                if let Some(until) = package.deadline()? {
                    manager.restrict_to_deadline(until)?;
                }
                let mut state = self
                    .state
                    .lock()
                    .map_err(|_| error(503, "daemon_bridge_unavailable"))?;
                Self::expire(&mut state, generation);
                if !live.load(Ordering::Acquire) || !state.controllers.contains_key(&controller) {
                    manager.retire();
                    return Err(error(401, "management_scope_changed"));
                }
                if state.managers.len() >= MAX_MANAGERS {
                    manager.retire();
                    return Err(error(503, "daemon_bridge_busy"));
                }
                let id = super::super::new_request_id();
                state.managers.insert(
                    id.clone(),
                    Manager {
                        controller,
                        package,
                        native: manager,
                    },
                );
                encode(id)
            }
            Call::Invoke {
                controller,
                management,
                operation,
                arguments,
            } => {
                let mutation = operation.mutation_for(&arguments)?;
                let (manager, package) = self.manager(&controller, &management, generation)?;
                let call = Call::Invoke {
                    controller: controller.clone(),
                    management: management.clone(),
                    operation,
                    arguments: arguments.clone(),
                };
                if mutation {
                    let fingerprint =
                        super::client::fingerprint(owner, &package.receipt_scope()?, &call)?;
                    match self.journal.admit(request_id, &fingerprint)? {
                        Admission::Pending => return Err(error(503, "daemon_command_pending")),
                        Admission::RecoveryRequired => {
                            let Some(result) = recover_native_receipt(&self.journal, request_id, &manager, operation, &arguments).await? else {
                                return Err(error(409, "daemon_command_recovery_required"));
                            };
                            self.journal.finish(request_id, &result)?;
                    receipt_recorded.store(super::terminal_receipt(&result),Ordering::Release);
                            return result.map_err(Failure::native);
                        },
                        Admission::Result(result) => { receipt_recorded.store(true,Ordering::Release); return result.map_err(Failure::native); },
                        Admission::New => {}
                    }
                }
                let result = self
                    .invoke(request_id, &management, &manager, operation, arguments.clone())
                    .await;
                let result = match result {
                    Ok(value)
                        if serde_json::to_vec(&value).map_or(true, |v| {
                            v.len() > super::super::protocol::MAX_REPLY_FRAME - 1024
                        }) =>
                    {
                        Err(error(413, "daemon_bridge_result_too_large"))
                    }
                    other => other,
                };
                if mutation {
                    let mut recorded = result.map_err(Failure::from);
                    if recorded.as_ref().is_err_and(|error| !error.terminal) {
                        // A bridge error can follow an applied effect. Settle it
                        // only with this exact native operation, ID and payload.
                        if let Ok(Some(receipt)) = matching_native_receipt(&manager, operation, &arguments).await {
                            recorded = receipt;
                        }
                    }
                    self.journal.finish(request_id, &recorded)?;
                    receipt_recorded.store(super::terminal_receipt(&recorded),Ordering::Release);
                    return recorded.map_err(Failure::native);
                }
                self.validate(&controller, generation)?;
                result
            }
            Call::Execute {
                controller,
                command,
            } => {
                let live = self.validate(&controller, generation)?;
                let command: Command = decode(command.clone())?;
                if matches!(command, Command::BeginLogin | Command::Logout) {
                    return Err(error(403, "daemon_enrollment_command_required"));
                }
                let owner_handle =
                    native.ok_or_else(|| error(503, "owner_daemon_hosting_required"))?;
                let call = Call::Execute {
                    controller,
                    command: encode(&command)?,
                };
                let mutation = super::owner_mutation(&command);
                let fingerprint = super::client::fingerprint(owner, "owner", &call)?;
                if mutation {
                    match self.journal.admit(request_id, &fingerprint)? {
                        Admission::Pending => return Err(error(503, "daemon_command_pending")),
                        // A command that only states what should be runs
                        // again; every other one keeps its first outcome.
                        Admission::RecoveryRequired if super::repeatable_owner_command(&command) => {
                            if !live.load(Ordering::Acquire) {
                                return Err(error(401, "management_scope_changed"));
                            }
                            if !self.journal.reenter(request_id)? {
                                return Err(error(503, "daemon_command_pending"));
                            }
                        }
                        Admission::RecoveryRequired => return Err(error(409, "daemon_command_recovery_required")),
                        Admission::Result(r) => { receipt_recorded.store(true,Ordering::Release); return r.map_err(Failure::native); },
                        Admission::New => {}
                    }
                }
                if !live.load(Ordering::Acquire) {
                    return Err(error(401, "management_scope_changed"));
                }
                let until = Instant::now() + LEASE_TTL;
                let result = owner_handle
                    .execute_guarded(
                        command,
                        Arc::new(move || live.load(Ordering::Acquire) && Instant::now() < until),
                    )
                    .await
                    .map_err(Failure::from);
                if mutation {
                    self.journal.finish(request_id, &result)?;
                    receipt_recorded.store(super::terminal_receipt(&result),Ordering::Release);
                }
                result.map_err(Failure::native)
            }
            #[cfg(feature = "resource-adoption")]
            Call::OwnerAccess {
                controller,
                package,
                operation,
                arguments,
            } => {
                let live = self.validate(&controller, generation)?;
                if !package.valid() {
                    return Err(error(400, "invalid_management_package"));
                }
                self.package_epoch(&controller, &package)?;
                let native = native.ok_or_else(|| error(503, "owner_daemon_hosting_required"))?;
                let call = Call::OwnerAccess {
                    controller: controller.clone(),
                    package: package.clone(),
                    operation,
                    arguments: arguments.clone(),
                };
                if operation.is_mutation() {
                    match self.journal.admit(
                        request_id,
                        &super::client::fingerprint(owner, &package.receipt_scope()?, &call)?,
                    )? {
                        Admission::Pending => return Err(error(503, "daemon_command_pending")),
                        Admission::RecoveryRequired => return Err(error(409, "daemon_command_recovery_required")),
                        Admission::Result(result) => { receipt_recorded.store(true,Ordering::Release); return result.map_err(Failure::native); },
                        Admission::New => {}
                    }
                }
                let result = super::grants::dispatch(
                    &self.grants,
                    &controller,
                    &package,
                    owner_root,
                    origin,
                    owner,
                    &native,
                    shared,
                    live,
                    self.controller_deadline(&controller, generation)?,
                    operation,
                    arguments,
                )
                .await;
                if operation.is_mutation() {
                    let result = result.map_err(Failure::from);
                    self.journal.finish(request_id, &result)?;
                    receipt_recorded.store(super::terminal_receipt(&result),Ordering::Release);
                    return result.map_err(Failure::native);
                }
                self.validate(&controller, generation)?;
                result
            }
            #[cfg(feature = "resource-adoption")]
            Call::RetireOwnerAccess { controller, access } => {
                self.validate(&controller, generation)?;
                self.grants
                    .lock()
                    .map_err(|_| error(503, "daemon_bridge_unavailable"))?
                    .retire(&controller, &access);
                Ok(Value::Null)
            }
            Call::SignOut { .. } => Err(error(503, "daemon_sign_out_unavailable")),
        }
    }
    fn invoke<'a>(
        &'a self,
        request_id: &'a str,
        management: &'a str,
        manager: &'a Arc<NativeManagement>,
        operation: Operation,
        arguments: Value,
    ) -> BridgeFuture<'a> {
        macro_rules! deferred { ($($body:tt)*) => { defer(move || async move { $($body)* }) }; }
        match operation {
            Operation::project_setup => deferred! {
                let call = decode(arguments)?;
                if let super::project_setup::SetupOperation::Resume { project } = call {
                    let transport = crate::native_owner::ProjectSetupTransport::new(manager.clone());
                    encode(transport.resume_begin_pinned(project, |id| self.journal.pin_native(request_id,&id)).await?)
                } else { encode(super::project_setup::dispatch_setup(manager.clone(), call).await?) }
            },
            Operation::account_models => deferred! {
                let (input,): (AccountModelQuery,) = decode(arguments)?;
                encode(manager.account_models(input).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::operator_graph_page => deferred! {
                let (query,): (OperatorGraphQuery,) = decode(arguments)?;
                encode(manager.operator_graph_page(query).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::operator_graph_detail => deferred! {
                let (query,): (OperatorGraphDetailQuery,) = decode(arguments)?;
                encode(manager.operator_graph_detail(query).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::operator_graph_node => deferred! {
                let (query,): (OperatorGraphNodeQuery,) = decode(arguments)?;
                encode(manager.operator_graph_node(query).await?)
            },
            Operation::pool_agent_room => deferred! {
                let (command_id, name): (String, String) = decode(arguments)?;
                encode(manager.pool_agent_room(command_id, name).await?)
            },
            Operation::interrupted_calls => deferred! {
                let (query,): (InterruptedCallQuery,) = decode(arguments)?;
                encode(manager.interrupted_calls(query).await?)
            },
            Operation::reconcile_interrupted_call => deferred! {
                let (input,): (ReconcileInterruptedCall,) = decode(arguments)?;
                encode(manager.reconcile_interrupted_call(input).await?)
            },
            Operation::rename_resource => deferred! {
                let (input,): (RenameResource,) = decode(arguments)?;
                encode(manager.rename_resource(input).await?)
            },
            Operation::resource_alerts => deferred! {
                let (query,): (ResourceAlertQuery,) = decode(arguments)?;
                encode(manager.resource_alerts(query).await?)
            },
            Operation::resource_alert => deferred! {
                let (query,): (ResourceAlertDetailQuery,) = decode(arguments)?;
                encode(manager.resource_alert(query).await?)
            },
            Operation::triage_resource_alert => deferred! {
                let (input,): (TriageResourceAlert,) = decode(arguments)?;
                encode(manager.triage_resource_alert(input).await?)
            },
            Operation::saved_commands => deferred! {
                let (): () = decode(arguments)?;
                encode(manager.saved_commands().await?)
            },
            Operation::acknowledge_command => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.acknowledge_command(command_id).await?)
            },
            Operation::retry_saved_command => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.retry_saved_command(command_id).await?)
            },
            Operation::stopped_tasks => deferred! {
                let (query,): (StoppedListQuery,) = decode(arguments)?;
                encode(manager.stopped_tasks(query).await?)
            },
            Operation::stopped_inspection => deferred! {
                let (query,): (StoppedQuery,) = decode(arguments)?;
                encode(manager.stopped_inspection(query).await?)
            },
            Operation::review_stopped => deferred! {
                let (input,): (ReviewStopped,) = decode(arguments)?;
                encode(manager.review_stopped(input).await?)
            },
            Operation::resolve_stopped => deferred! {
                let (input,): (ResolveStopped,) = decode(arguments)?;
                encode(manager.resolve_stopped(input).await?)
            },
            Operation::web_catalog => deferred! {
                let (query,): (WebCatalogQuery,) = decode(arguments)?;
                encode(manager.web_catalog(query).await?)
            },
            Operation::web_offers => deferred! {
                let (resource_id,): (String,) = decode(arguments)?;
                encode(manager.web_offers(resource_id).await?)
            },
            Operation::web_trust => deferred! {
                let (query,): (WebCatalogQuery,) = decode(arguments)?;
                encode(manager.web_trust(query).await?)
            },
            Operation::prepare_web_catalog => deferred! {
                let (input,): (WebCatalogPrepare,) = decode(arguments)?;
                encode(manager.prepare_web_catalog(input).await?)
            },
            Operation::review_web_catalog_command => deferred! {
                let (id,): (String,) = decode(arguments)?;
                encode(manager.review_web_catalog_command(id).await?)
            },
            Operation::cancel_saved_web_catalog => deferred! {
                let (id,): (String,) = decode(arguments)?;
                encode(manager.cancel_saved_web_catalog(id).await?)
            },
            Operation::apply_saved_web_catalog => deferred! {
                let (id,): (String,) = decode(arguments)?;
                encode(manager.apply_saved_web_catalog(id).await?)
            },
            Operation::save_personal_policy => deferred! {
                let (input,): (SavePersonalPolicy,) = decode(arguments)?;
                encode(manager.save_personal_policy(input).await?)
            },
            Operation::operator_task_page => deferred! {
                let (input,): (service::OperatorTaskQuery,) = decode(arguments)?;
                encode(manager.operator_task_page(input).await?)
            },
            Operation::operator_task_detail => deferred! {
                let (input,): (service::OperatorTaskDetailQuery,) = decode(arguments)?;
                encode(manager.operator_task_detail(input).await?)
            },
            Operation::operator_board => deferred! {
                let (input,): (service::OperatorBoardQuery,) = decode(arguments)?;
                encode(manager.operator_board(input).await?)
            },
            Operation::review_resource_configuration => deferred! {
                let (input,): (CreateResourceConfiguration,) = decode(arguments)?;
                encode(manager.review_resource_configuration(&input).await?)
            },
            Operation::resource_workspaces => deferred! {
                let (query,): (WorkspaceListQuery,) = decode(arguments)?;
                encode(manager.resource_workspaces(query).await?)
            },
            Operation::resource_workspace => deferred! {
                let (query,): (WorkspaceQuery,) = decode(arguments)?;
                encode(manager.resource_workspace(query).await?)
            },
            Operation::clear_workspace_dirty => deferred! {
                let (input,): (ClearWorkspaceDirty,) = decode(arguments)?;
                encode(manager.clear_workspace_dirty(input).await?)
            },
            Operation::web_alerts => deferred! {
                let (query,): (WebAlertsQuery,) = decode(arguments)?;
                encode(manager.web_alerts(query).await?)
            },
            Operation::web_capability => deferred! {
                let (query,): (WebOverviewQuery,) = decode(arguments)?;
                encode(manager.web_capability(query).await?)
            },
            Operation::web_terminal => deferred! {
                let (query,): (WebTerminalQuery,) = decode(arguments)?;
                encode(manager.web_terminal(query).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::prepare_operator_graph_create => deferred! {
                let (input,): (OperatorGraphCreate,) = decode(arguments)?;
                encode(manager.prepare_operator_graph_create(input).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::prepare_operator_graph_update => deferred! {
                let (input,): (OperatorGraphUpdate,) = decode(arguments)?;
                encode(manager.prepare_operator_graph_update(input).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::prepare_operator_graph_cancel => deferred! {
                let (input,): (OperatorGraphCancel,) = decode(arguments)?;
                encode(manager.prepare_operator_graph_cancel(input).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::review_saved_operator_graph => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.review_saved_operator_graph(command_id).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::apply_saved_operator_graph => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.apply_saved_operator_graph(command_id).await?)
            },
            Operation::probe_web_connection => deferred! {
                let (input,): (WebConnectionProbeInput,) = decode(arguments)?;
                encode(manager.probe_web_connection(input).await?)
            },
            Operation::agent_execution_policies => deferred! {
                let (query,): (ExecutionPolicyListQuery,) = decode(arguments)?;
                encode(manager.agent_execution_policies(query).await?)
            },
            Operation::agent_execution_policy => deferred! {
                let (query,): (ExecutionPolicyQuery,) = decode(arguments)?;
                encode(manager.agent_execution_policy(query).await?)
            },
            Operation::edit_execution_policy => deferred! {
                let (input,): (EditExecutionPolicy,) = decode(arguments)?;
                encode(manager.edit_execution_policy(input).await?)
            },
            Operation::resource_templates => deferred! {
                let (resource_id, after): (String, Option<String>) = decode(arguments)?;
                encode(manager.resource_templates(resource_id, after).await?)
            },
            Operation::resource_template => deferred! {
                let (resource_id, id): (String, String) = decode(arguments)?;
                encode(manager.resource_template(resource_id, id).await?)
            },
            Operation::prepare_template_create => deferred! {
                let (input,): (TemplateCreateInput,) = decode(arguments)?;
                encode(manager.prepare_template_create(input).await?)
            },
            Operation::prepare_template_update => deferred! {
                let (input,): (TemplateUpdateInput,) = decode(arguments)?;
                encode(manager.prepare_template_update(input).await?)
            },
            Operation::prepare_template_delete => deferred! {
                let (input,): (TemplateDeleteInput,) = decode(arguments)?;
                encode(manager.prepare_template_delete(input).await?)
            },
            Operation::review_template_command => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.review_template_command(command_id).await?)
            },
            Operation::cancel_saved_template => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.cancel_saved_template(command_id).await?)
            },
            Operation::apply_saved_template => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.apply_saved_template(command_id).await?)
            },
            Operation::resource_approval_bindings => deferred! {
                let (query,): (ResourceApprovalBindingQuery,) = decode(arguments)?;
                encode(manager.resource_approval_bindings(query).await?)
            },
            Operation::retire_resource_approval_binding => deferred! {
                let (input,): (RetireResourceApprovalBinding,) = decode(arguments)?;
                encode(manager.retire_resource_approval_binding(input).await?)
            },
            Operation::prepare_operator_task => deferred! {
                let (change,): (OperatorTaskChange,) = decode(arguments)?;
                encode(manager.prepare_operator_task(change).await?)
            },
            Operation::review_saved_operator_task => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.review_saved_operator_task(command_id).await?)
            },
            Operation::apply_saved_operator_task => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.apply_saved_operator_task(command_id).await?)
            },
            Operation::prepare_account => deferred! {
                let (input,): (PrepareAccount,) = decode(arguments)?;
                encode(manager.prepare_account(input).await?)
            },
            Operation::personal_agents => deferred! {
                let (query,): (PersonalAgentQuery,) = decode(arguments)?;
                encode(manager.personal_agents(query).await?)
            },
            Operation::personal_agent_settings => deferred! {
                let (id,): (String,) = decode(arguments)?;
                encode(manager.personal_agent_settings(id).await?)
            },
            Operation::tool_approvals => deferred! {
                let (query,): (ToolApprovalQuery,) = decode(arguments)?;
                encode(manager.tool_approvals(query).await?)
            },
            Operation::tool_approval => deferred! {
                let (query,): (ToolApprovalLookup,) = decode(arguments)?;
                encode(manager.tool_approval(query).await?)
            },
            Operation::tool_grants => deferred! {
                let (query,): (ToolGrantQuery,) = decode(arguments)?;
                encode(manager.tool_grants(query).await?)
            },
            Operation::revoke_tool_grant => deferred! {
                let (input,): (RevokeToolGrant,) = decode(arguments)?;
                encode(manager.revoke_tool_grant(input).await?)
            },
            Operation::resources => deferred! {
                let (query,): (service::ResourceQuery,) = decode(arguments)?;
                encode(manager.resources(query).await?)
            },
            Operation::account_inventory => deferred! {
                let (query,): (service::AccountInventoryQuery,) = decode(arguments)?;
                encode(manager.account_inventory(query).await?)
            },
            Operation::resource_detail => deferred! {
                let (query,): (service::ResourceDetailQuery,) = decode(arguments)?;
                encode(manager.resource_detail(query).await?)
            },
            Operation::publish_resource => deferred! {
                let (input,): (service::PublishResource,) = decode(arguments)?;
                encode(manager.publish_resource(input).await?)
            },
            Operation::central => deferred! {
                let (command,): (ManagementCommand,) = decode(arguments)?;
                encode(manager.central(command).await?)
            },
            Operation::observe_personal_settings => deferred! {
                let (input,): (ObservePersonalSettings,) = decode(arguments)?;
                encode(manager.observe_personal_settings(input).await?)
            },
            Operation::personal_model_choices => deferred! {
                let (agent_id,): (String,) = decode(arguments)?;
                encode(manager.personal_model_choices(agent_id).await?)
            },
            Operation::review_local_resource_edit => deferred! {
                let (input,): (EditLocalResource,) = decode(arguments)?;
                encode(manager.review_local_resource_edit(&input).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::operator_graph_recipient_observe => deferred! {
                let (input,): (OperatorGraphRecipientObserve,) = decode(arguments)?;
                encode(manager.operator_graph_recipient_observe(input).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::prepare_operator_graph_bind_recipient => deferred! {
                let (input,): (OperatorGraphBindRecipient,) = decode(arguments)?;
                encode(manager.prepare_operator_graph_bind_recipient(input).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::operator_graph_recipient_get => deferred! {
                let (query,): (OperatorGraphNodeQuery,) = decode(arguments)?;
                encode(manager.operator_graph_recipient_get(query).await?)
            },
            #[cfg(feature = "resource-adoption")]
            Operation::operator_graph_recipient_options => deferred! {
                let (query,): (OperatorGraphRecipientOptionsQuery,) = decode(arguments)?;
                encode(manager.operator_graph_recipient_options(query).await?)
            },
            Operation::agent_budget => deferred! {
                let (id,): (String,) = decode(arguments)?;
                encode(manager.agent_budget(id).await?)
            },
            Operation::agent_status => deferred! {
                let (id,): (String,) = decode(arguments)?;
                encode(manager.agent_status(id).await?)
            },
            Operation::web_account_capacity => deferred! {
                let (query,): (WebAccountCapacityQuery,) = decode(arguments)?;
                encode(manager.web_account_capacity(query).await?)
            },
            Operation::web_workforce => deferred! {
                let (query,): (WebOverviewQuery,) = decode(arguments)?;
                encode(manager.web_workforce(query).await?)
            },
            Operation::web_usage => deferred! {
                let (query,): (WebOverviewQuery,) = decode(arguments)?;
                encode(manager.web_usage(query).await?)
            },
            Operation::web_connections => deferred! {
                let (after,): (Option<String>,) = decode(arguments)?;
                encode(manager.web_connections(after).await?)
            },
            Operation::web_connection => deferred! {
                let (id,): (String,) = decode(arguments)?;
                encode(manager.web_connection(id).await?)
            },
            Operation::web_connection_result => deferred! {
                let (command_id,): (String,) = decode(arguments)?;
                encode(manager.web_connection_result(command_id).await?)
            },
            Operation::verify_web_connection => deferred! {
                let (command_id, id, expected_revision): (String, String, u64) = decode(arguments)?;
                encode(
                    manager
                        .verify_web_connection(command_id, id, expected_revision)
                        .await?,
                )
            },
            Operation::login_account => deferred! {
                let (input,): (AccountLoginRequest,) = decode(arguments)?;
                encode(manager.login_account(input).await?)
            },
            Operation::account_login_status => deferred! {
                let (input,): (AccountLoginRequest,) = decode(arguments)?;
                encode(manager.account_login_status(input).await?)
            },
            Operation::cancel_account_login => deferred! {
                let (input,): (AccountLoginRequest,) = decode(arguments)?;
                encode(manager.cancel_account_login(input).await?)
            },
            _ => deferred! {
                self.special(management, manager, operation, arguments)
                    .await
            },
        }
    }
}

#[cfg(test)]
mod lease_tests {
    use super::*;
    #[test]
    fn bridge_dispatch_future_is_bounded_for_default_worker_stack() {
        fn bytes<F: std::future::Future>(_: impl FnOnce() -> F) -> usize {
            std::mem::size_of::<F>()
        }
        let temp = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(temp.path()).unwrap();
        let size = bytes(|| {
            bridge.handle(
                "size",
                "@owner:test",
                "https://test/",
                1,
                None,
                temp.path(),
                #[cfg(feature = "resource-adoption")]
                None,
                Call::Attach {
                    origin: "https://test/".into(),
                    epoch: 1,
                },
            )
        });
        println!("Bridge::handle future bytes={size}");
        assert!(
            size < 128 * 1024,
            "dispatch must not inline the complete management API: {size} bytes"
        );
    }

    #[test]
    fn renewed_controller_preserves_bound_lifetime_and_fences_expiry() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(temp.path()).unwrap();
        let expires = Arc::new(Mutex::new(Instant::now() + LEASE_TTL));
        let live = Arc::new(AtomicBool::new(true));
        bridge.state.lock().unwrap().controllers.insert(
            "same".into(),
            Controller {
                epoch: 1,
                generation: 1,
                expires: expires.clone(),
                live: live.clone(),
            },
        );
        let bound = bridge.controller_deadline("same", 1).unwrap();
        *expires.lock().unwrap() = Instant::now() + Duration::from_secs(25);
        let queued_writer_deadline = bound().unwrap();
        bridge.validate("same", 1).unwrap();
        assert!(bound().unwrap().duration_since(queued_writer_deadline) > Duration::from_secs(90));
        assert!(
            queued_writer_deadline < bound().unwrap(),
            "issued authorities keep their captured earlier deadline"
        );
        *expires.lock().unwrap() = Instant::now() - Duration::from_secs(1);
        assert!(bound().is_none());
        assert!(bridge.validate("same", 1).is_err());
        assert!(!live.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn controller_is_owner_generation_scoped_and_detach_revokes_queued_gate() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(temp.path()).unwrap();
        let attach = |origin: &str| Call::Attach {
            origin: origin.into(),
            epoch: 7,
        };
        let bad = bridge
            .handle(
                "one",
                "@o:s",
                "https://s/",
                3,
                None,
                temp.path(),
                #[cfg(feature = "resource-adoption")]
                None,
                attach("https://other/"),
            )
            .await;
        assert_eq!(bad.err().unwrap().code, "management_scope_changed");
        let lease: Lease = decode(
            bridge
                .handle(
                    "two",
                    "@o:s",
                    "https://s/",
                    3,
                    None,
                    temp.path(),
                    #[cfg(feature = "resource-adoption")]
                    None,
                    attach("https://s/"),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let queued = bridge.validate(&lease.id, 3).unwrap();
        assert!(queued.load(Ordering::Acquire));
        let package = Package {
            digest: "a".repeat(64),
            instance: 1,
            account_epoch: 8,
            permissions: vec![],
            expires_at_ms: None,
        };
        assert_eq!(
            bridge.package_epoch(&lease.id, &package).unwrap_err().code,
            "management_scope_changed"
        );
        bridge
            .handle(
                "three",
                "@o:s",
                "https://s/",
                3,
                None,
                temp.path(),
                #[cfg(feature = "resource-adoption")]
                None,
                Call::Detach {
                    controller: lease.id.clone(),
                },
            )
            .await
            .unwrap();
        assert!(!queued.load(Ordering::Acquire));
        assert_eq!(
            bridge.validate(&lease.id, 3).unwrap_err().code,
            "daemon_bridge_lease_expired"
        );
        let lease: Lease = decode(
            bridge
                .handle(
                    "four",
                    "@o:s",
                    "https://s/",
                    3,
                    None,
                    temp.path(),
                    #[cfg(feature = "resource-adoption")]
                    None,
                    attach("https://s/"),
                )
                .await
                .unwrap(),
        )
        .unwrap();
        let queued = bridge.validate(&lease.id, 3).unwrap();
        assert!(bridge.validate(&lease.id, 4).is_err());
        assert!(!queued.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn ttl_expiry_cannot_be_renewed_by_an_old_controller() {
        let temp = tempfile::tempdir().unwrap();
        let bridge = Bridge::open(temp.path()).unwrap();
        let id = "expired".to_owned();
        let live = Arc::new(AtomicBool::new(true));
        bridge.state.lock().unwrap().controllers.insert(
            id.clone(),
            Controller {
                epoch: 1,
                generation: 1,
                expires: Arc::new(Mutex::new(Instant::now() - Duration::from_secs(1))),
                live: live.clone(),
            },
        );
        assert!(bridge.validate(&id, 1).is_err());
        assert!(!live.load(Ordering::Acquire));
    }
}

/// Read original native journal receipts only. Missing receipts never prove
/// that an effect did not happen (and never authorize a generic retry).
async fn recover_native_receipt(journal: &Journal, request_id: &str, manager: &Arc<NativeManagement>, operation: Operation, arguments: &Value) -> Result<Option<Result<Value, Failure>>, NativeError> {
    if operation == Operation::project_setup {
        let transport = crate::native_owner::ProjectSetupTransport::new(manager.clone());
        let terminal=AtomicBool::new(false);
        let result = match decode(arguments.clone())? {
            super::project_setup::SetupOperation::Resume { project } => match journal.native_pin(request_id)? {
                Some(id) => transport.retry_command_terminal(id,&terminal).await,
                None => transport.resume_begin_pinned(project, |id| journal.pin_native(request_id,&id)).await,
            },
            super::project_setup::SetupOperation::Retry { command } => transport.retry_command_terminal(command,&terminal).await,
            super::project_setup::SetupOperation::Acknowledge { command } => transport.acknowledge_failure(command).await,
            _ => return Ok(None),
        };
        return Ok(Some(result.and_then(encode).map_err(|error| {let mut failure=Failure::from(error);failure.terminal|=terminal.load(Ordering::Acquire);failure})));
    }
    if operation == Operation::acknowledge_command {
        let (id,): (String,) = decode(arguments.clone())?;
        // Exact-ID acknowledgment is reduction-only and natively idempotent.
        return Ok(Some(manager.acknowledge_command(id).await.map(|_| Value::Null).map_err(Failure::from)));
    }
    if operation == Operation::retry_saved_command {
        let (id,): (String,) = decode(arguments.clone())?;
        let Some(saved) = manager.original_command_receipt(id).await? else { return Ok(None) };
        return Ok(saved.reply.map(|reply| match reply {
            service::IntentReply::Success(value) => Ok(serde_json::json!({"operation":saved.operation,"data":value})),
            service::IntentReply::Failure { status, code } => Err(Failure { status, code, terminal:true }),
        }));
    }
    if operation == Operation::observe_personal_settings {
        // Natively idempotent under its command ID: the saved request asks
        // the server again with its original key and gets the original row.
        // An observation only reads the device, so entering native code a
        // second time cannot apply anything twice. Without this the bridge
        // answered every retry with recovery_required and the request could
        // never be settled.
        let (input,): (ObservePersonalSettings,) = decode(arguments.clone())?;
        return Ok(Some(match manager.observe_personal_settings(input).await {
            Ok(value) => Ok(value),
            Err(error) => match matching_native_receipt(manager, operation, arguments).await? {
                Some(receipt) => receipt,
                None => Err(Failure::from(error)),
            },
        }));
    }
    matching_native_receipt(manager, operation, arguments).await
}

/// An observation's saved input wraps the original arguments with the
/// requesting device, under another operation name than the bridge's.
fn observation_reply(arguments: &Value, saved: &service::PendingIntent) -> Option<service::IntentReply> {
    let input = arguments.as_array()?.first()?;
    (saved.operation == service::IntentOperation::PersonalAgentObservation
        && input.get("commandId").and_then(Value::as_str) == Some(saved.command_id.as_str())
        && saved.input.get("input") == Some(input))
        .then(|| saved.reply.clone())
        .flatten()
}

fn original_intent(operation: Operation, arguments: &Value) -> Option<(String, service::IntentOperation, Value)> {
    let input = arguments.as_array()?.first()?.clone();
    let id = input.get("commandId").or_else(|| input.get("command_id"))?.as_str()?.to_owned();
    let kind = if operation == Operation::central {
        input.get("operation")?.clone()
    } else if operation == Operation::retire_resource_approval_binding {
        Value::String("retire_approval_binding".into())
    } else {
        serde_json::to_value(operation).ok()?
    };
    Some((id, serde_json::from_value(kind).ok()?, input))
}
fn matching_intent_reply(operation: Operation, arguments: &Value, saved: &service::PendingIntent) -> Option<service::IntentReply> {
    let (id, kind, input) = original_intent(operation, arguments)?;
    (saved.command_id == id && saved.operation == kind && saved.input == input).then(|| saved.reply.clone()).flatten()
}
async fn matching_native_receipt(manager: &Arc<NativeManagement>, operation: Operation, arguments: &Value) -> Result<Option<Result<Value, Failure>>, NativeError> {
    if operation == Operation::observe_personal_settings {
        let Some(id) = arguments.get(0).and_then(|input| input.get("commandId")).and_then(Value::as_str) else { return Ok(None) };
        let Some(saved) = manager.original_command_receipt(id.to_owned()).await? else { return Ok(None) };
        // Only a recorded refusal settles here. A recorded success is
        // returned by the native call itself under the same command ID.
        return Ok(match observation_reply(arguments, &saved) {
            Some(service::IntentReply::Failure { status, code }) => Some(Err(Failure { status, code, terminal: true })),
            _ => None,
        });
    }
    let Some((id, _, _)) = original_intent(operation, arguments) else { return Ok(None) };
    let Some(saved) = manager.original_command_receipt(id.clone()).await? else { return Ok(None) };
    Ok(match matching_intent_reply(operation, arguments, &saved) {
        Some(service::IntentReply::Failure { status, code }) => Some(Err(Failure { status, code, terminal: true })),
        Some(service::IntentReply::Success(_)) => {
            // Reuse the native original-command recovery path: it rechecks
            // current server role visibility before returning a success.
            let recovered = manager.retry_saved_command(id).await?;
            Some(Ok(recovered.get("data").cloned().ok_or_else(|| error(503, "management_saved_result_invalid"))?))
        },
        None => None,
    })
}

#[cfg(test)]
mod receipt_matching_tests {
    use super::*;
    #[test]
    fn native_refusal_settles_only_matching_original_payload_and_releases_bridge_id() {
        let args=serde_json::json!([{"commandId":"rename-1","resourceId":"resource-1","name":"new","expectedRevision":3}]);
        let mut saved:service::PendingIntent=serde_json::from_value(serde_json::json!({"commandId":"rename-1","operation":"rename_resource","input":args[0],"reply":{"kind":"failure","data":{"status":409,"code":"management_resource_name_changed"}}})).unwrap();
        let reply=matching_intent_reply(Operation::rename_resource,&args,&saved).unwrap();
        let service::IntentReply::Failure{status,code}=reply else {panic!("expected native refusal")};
        let result=Err(Failure{status,code,terminal:true});
        let temp=tempfile::tempdir().unwrap();let journal=Journal::open(&temp.path().join("journal")).unwrap();
        let id=journal.pending_for("owner","rename").unwrap();assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::New));
        journal.finish(&id,&result).unwrap();assert!(matches!(journal.admit(&id,"scope").unwrap(),Admission::Result(Err(Failure{terminal:true,..}))));
        if super::super::terminal_receipt(&result) {journal.clear("rename",&id).unwrap();}
        assert_ne!(journal.pending_for("owner","rename").unwrap(),id);
        saved.command_id="different".into();assert!(matching_intent_reply(Operation::rename_resource,&args,&saved).is_none());
        saved.command_id="rename-1".into();saved.input["name"]=Value::String("other".into());assert!(matching_intent_reply(Operation::rename_resource,&args,&saved).is_none());
        saved.input=args[0].clone();assert!(matching_intent_reply(Operation::publish_resource,&args,&saved).is_none());
    }
    #[test]
    fn closed_observation_settles_only_its_own_saved_request() {
        let args=serde_json::json!([{"commandId":"observe-1","agentId":"agent_a","expectedGeneration":3}]);
        let requester=serde_json::json!({"origin":"https://s/","userId":"@o:s","deviceId":"D"});
        let mut saved:service::PendingIntent=serde_json::from_value(serde_json::json!({"commandId":"observe-1","operation":"personal_agent_observation",
            "input":{"commandId":"observe-1","input":args[0],"requester":requester},
            "reply":{"kind":"failure","data":{"status":409,"code":"configuration_provider_unavailable"}}})).unwrap();
        // The generic matcher does not know this operation's saved shape.
        assert!(matching_intent_reply(Operation::observe_personal_settings,&args,&saved).is_none());
        let Some(service::IntentReply::Failure{status,code})=observation_reply(&args,&saved) else {panic!("expected the recorded refusal")};
        assert_eq!((status,code.as_str()),(409,"configuration_provider_unavailable"));
        let mut other=args.clone();other[0]["expectedGeneration"]=serde_json::json!(4);
        assert!(observation_reply(&other,&saved).is_none());
        other=args.clone();other[0]["commandId"]=serde_json::json!("observe-2");
        assert!(observation_reply(&other,&saved).is_none());
        saved.operation=service::IntentOperation::PersonalAgentPolicy;
        assert!(observation_reply(&args,&saved).is_none());
        saved.operation=service::IntentOperation::PersonalAgentObservation;saved.reply=None;
        assert!(observation_reply(&args,&saved).is_none());
    }
}
