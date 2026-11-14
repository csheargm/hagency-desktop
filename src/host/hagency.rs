//! Built-in Hagency services. The SDK owns identity; Splash gets no credentials,
//! paths, provider URLs or generic engine/HTTP entry point.
use ::hagency::native_owner::{
    ManagementCommand, ManagementPackage, ManagementPermission, };
use ::hagency::daemon::bridge::ManagementClient as NativeManagement;
use crate::miniapps::{InstanceId, Lease};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

mod setup;
mod resources;
mod alerts;
mod approval_bindings;
pub(crate) mod tool_approvals;
mod execution_policy;
mod workspaces;
mod stopped;
mod interrupted_calls;
pub(crate) mod accounts;
pub(crate) mod enrollment;
pub(crate) mod resource_edit;
pub(crate) mod resource_create;
pub(crate) mod retirement;
mod tasks;
pub(crate) mod web_connections;
mod web_catalog;
mod web_overview;
mod templates;
mod graphs;
pub(crate) mod reauthentication;
pub(crate) mod errors;
mod local;
mod personal;
pub(crate) mod personal_configuration;
mod agents;
mod pools;
mod control;
mod budget;
mod allocations;
pub(crate) mod grants;
pub(crate) mod grant_people;
pub(crate) mod execution;
pub(crate) mod local_zone;
pub(crate) mod people;

#[cfg(feature = "resource-adoption")]
pub(crate) mod adoption;

struct Inner {
    digest: String,
    identity: InstanceId,
    epoch: u64,
    retired: AtomicBool,
    reauthentication_required: AtomicBool,
    initialising: tokio::sync::Mutex<()>,
    manager: Mutex<Option<Arc<NativeManagement>>>,
    grant_access: Mutex<Option<::hagency::daemon::bridge::OwnerGrantAccess>>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        if let Ok(access) = self.grant_access.get_mut()
            && let Some(access) = access.take() { access.retire(); }
        if let Ok(manager) = self.manager.get_mut()
            && let Some(manager) = manager.take()
        {
            manager.retire();
        }
    }
}
#[derive(Clone)]
pub(crate) struct Host(Arc<Inner>);
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    after: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectionHistory {
    id: String,
    after: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reoffer {
    command_id: String,
    contribution_id: String,
    expected_contribution_revision: i64,
    resource_id: String,
    expected_revision: String,
    label: String,
    coordinator_mxid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CommandId {
    command_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Inbox {
    after: Option<String>,
    #[serde(default)]
    needs_my_action: bool,
    #[serde(default)]
    include_projects: bool,
    #[serde(default)]
    include_allocations: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    after: Option<String>,
    capability: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Projects {
    after: Option<String>,
    #[serde(default)]
    needs_my_action: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectRequest {
    command_id: String,
    contribution_id: String,
    name: String,
    purpose: String,
    requested_tokens: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProjectRevoke {
    command_id: String,
    project_id: String,
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Contribute {
    #[serde(default)]
    publication_first: bool,
    command_id: String,
    resource_id: String,
    expected_revision: String,
    label: String,
    coordinator_mxid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Decide {
    command_id: String,
    request_id: String,
    expected_revision: i64,
    approve: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Revoke {
    command_id: String,
    contribution_id: String,
    expected_revision: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Verify {
    command_id: String,
    contribution_id: String,
    expected_revision: i64,
    resource_id: String,
    resource_revision: String,
}
fn parse<T: for<'de> Deserialize<'de>>(args: Value) -> Result<T, String> {
    if !args.is_object() || args.to_string().len() > 16 * 1024 {
        return Err("invalid_management_arguments".into());
    }
    serde_json::from_value(args).map_err(|_| "invalid_management_arguments".into())
}
fn permissions(lease: &Lease) -> Vec<ManagementPermission> {
    use ManagementPermission::*;
    let mut grants = Vec::new();
    for cap in lease.services() {
        grants.extend(match cap.as_str() {
            "hagency.shared_approvals.list"=>vec![ReadAgentWork],
            "hagency.shared_approvals.decide"=>vec![DecideAgentRequests],
            "hagency.intent.pending" | "hagency.intent.retry" => vec![ReadIntents],
            "hagency.intent.acknowledge" => vec![AcknowledgeIntents],
            cap if !personal_configuration::permissions(cap).is_empty() => personal_configuration::permissions(cap).to_vec(),
            cap if !web_connections::permissions(cap).is_empty() => web_connections::permissions(cap).to_vec(),
            cap if !web_catalog::permissions(cap).is_empty() => web_catalog::permissions(cap).to_vec(),
            cap if !web_overview::permissions(cap).is_empty() => web_overview::permissions(cap).to_vec(),
            cap if !templates::permissions(cap).is_empty() => templates::permissions(cap).to_vec(),
            cap if !graphs::permissions(cap).is_empty() => graphs::permissions(cap).to_vec(),
            tasks::LIST | tasks::GET => vec![ReadTasks],
            tasks::BOARD => vec![ReadTaskBoard],
            tasks::CREATE => vec![ReadTasks, CreateTasks],
            tasks::EDIT => vec![ReadTasks, EditTasks],
            tasks::DELETE => vec![ReadTasks, DeleteTasks],
            tasks::ACCEPT => vec![ReadTasks, AcceptTasks],
            tasks::TRANSITION => vec![ReadTasks, TransitionTasks],
            tasks::COMMENT => vec![ReadTasks, CommentTasks],
            tasks::REVIEW | tasks::APPLY => vec![ReadTasks, ReadIntents],
            "hagency.resources.list" | resources::GET => vec![ReadResources],
            alerts::LIST | alerts::GET => vec![ReadAlerts],
            alerts::TRIAGE => vec![TriageAlerts],
            tool_approvals::LIST | tool_approvals::GET | tool_approvals::OPEN => vec![ReadToolApprovals],
            tool_approvals::GRANTS => vec![InspectToolGrants],
            tool_approvals::REVOKE => vec![RevokeToolGrants],
            stopped::LIST | stopped::GET => vec![ReadStoppedTasks],
            stopped::REVIEW => vec![InspectStoppedTasks],
            stopped::RESOLVE => vec![ResolveStoppedTasks],
            interrupted_calls::LIST | interrupted_calls::RECONCILE => vec![ReconcileAgentCalls],
            workspaces::LIST | workspaces::GET => vec![ReadWorkspaces],
            workspaces::CLEAR => vec![ClearWorkspaceDirty],
            execution_policy::LIST | execution_policy::GET => vec![ReadExecutionPolicy],
            execution_policy::EDIT => vec![EditExecutionPolicy],
            approval_bindings::LIST => vec![ReadApprovalBindings],
            approval_bindings::RETIRE => vec![RetireApprovalBindings],
            resources::RENAME => vec![ReadResources, RenameResources],
            resource_edit::EDIT | resource_create::CREATE => vec![ReadResources, ConfigureResources],
            "hagency.resources.catalog" => vec![ReadCatalog],
            "hagency.projects.list" | "hagency.projects.get" => vec![ReadProjects],
            "hagency.projects.request" => vec![RequestProjects],
            "hagency.projects.decide" => vec![DecideProjects],
            "hagency.projects.revoke" => vec![RevokeProjects],
            accounts::LIST | accounts::GET => vec![ReadAccounts],
            accounts::PREPARE => vec![PrepareAccounts],
            retirement::RETIRE => vec![RetireAccounts],
            accounts::LOGIN => vec![LoginAccounts],
            accounts::MODELS => vec![ReadAccountModels],
            enrollment::ENROLL => vec![EnrollResources],
            "hagency.resources.publish" => vec![PublishResources],
            "hagency.connections.create" | "hagency.connections.reoffer" => vec![ReadResources, ContributeResources],
            "hagency.session" | "hagency.connections.list" | "hagency.connections.get" | "hagency.connections.history" => {
                vec![ReadConnections]
            }
            "hagency.connections.revoke" => vec![RevokeConnections],
            "hagency.connections.verify" => vec![ReadResources, VerifyConnections],
            "hagency.connections.resume" => vec![ReadResources, ResumeConnections],
            "hagency.inbox.list" | "hagency.inbox.get" => vec![ReadInbox],
            "hagency.inbox.decide" => vec![DecideInbox],
            _ if !personal::permissions(cap).is_empty() => personal::permissions(cap).to_vec(),
            _ if !local::permissions(cap).is_empty() => local::permissions(cap).to_vec(),
            _ if !allocations::permissions(cap).is_empty() => allocations::permissions(cap).to_vec(),
            _ if !budget::permissions(cap).is_empty() => budget::permissions(cap).to_vec(),
            _ if !control::permissions(cap).is_empty() => control::permissions(cap).to_vec(),
            _ if !agents::permissions(cap).is_empty() => agents::permissions(cap).to_vec(),
            _ if !pools::permissions(cap).is_empty() => pools::permissions(cap).to_vec(),
            _ => setup::permissions(cap).to_vec(),
        });
    }
    grants.sort();
    grants.dedup();
    grants
}
impl Host {
    pub fn new(digest: String, lease: &Lease) -> Result<Self, String> {
        if lease.identity().app != octosense_app_contract::hagency::APP_ID
            || digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("management_package_required".into());
        }
        let host = Self(Arc::new(Inner {
            digest,
            identity: lease.identity().clone(),
            epoch: crate::account_session::epoch(),
            retired: AtomicBool::new(false),
            reauthentication_required: AtomicBool::new(false),
            initialising: Default::default(),
            manager: Default::default(),
            grant_access: Default::default(),
        }));
        host.check(lease)?;
        Ok(host)
    }
    fn check(&self, lease: &Lease) -> Result<String, String> {
        if self.0.retired.load(Ordering::Acquire)
            || self.0.identity != *lease.identity()
            || self.0.epoch != crate::account_session::epoch()
            || crate::sliding_sync::account_changing()
            || crate::sliding_sync::token_expired()
            || crate::logout::logout_state_machine::is_logout_in_progress()
            || crate::account_session::is_closing()
        {
            return Err("management_scope_changed".into());
        }
        let account = crate::sliding_sync::current_user_id()
            .ok_or("matrix_sign_in_required")?
            .to_string();
        lease.check(&account)?;
        Ok(account)
    }
    pub fn retire(&self) {
        self.0.retired.store(true, Ordering::Release);
        if let Ok(mut access) = self.0.grant_access.lock()
            && let Some(access) = access.take() { access.retire(); }
        if let Ok(mut manager) = self.0.manager.lock()
            && let Some(manager) = manager.take()
        {
            manager.retire();
        }
    }
    fn cached_manager(&self) -> Result<Option<Arc<NativeManagement>>, String> {
        let mut current = self
            .0
            .manager
            .lock()
            .map_err(|_| "management_unavailable")?;
        if current.as_ref().is_some_and(|manager| manager.is_retired()) {
            // An aborted logout can leave this account's package lease valid,
            // but its native manager is permanently fenced. Drop its owner
            // handle before a new service tries to acquire the profile lock.
            drop(current.take());
        }
        Ok(current.clone())
    }
    async fn manager(&self, lease: &Lease) -> Result<Arc<NativeManagement>, String> {
        self.check(lease)?;
        let _initialising = self.0.initialising.lock().await;
        self.check(lease)?;
        if let Some(manager) = self.cached_manager()? {
            return Ok(manager);
        }
        let service = crate::hagency::backend::Service::for_current()?;
        let manager = service
            .management_until(ManagementPackage {
                digest: self.0.digest.clone(),
                instance: self.0.identity.generation,
                account_epoch: self.0.epoch + 1,
                permissions: permissions(lease),
            }, lease.expires())
            .await.map_err(|error| {self.note_reauthentication_error(&error);error})?;
        self.0.reauthentication_required.store(false,Ordering::Release);
        #[cfg(feature = "resource-adoption")]
        let grant_access = if lease.authorize(&self.0.identity.account, grants::REVIEW_GRANT, None).is_ok() {
            // Read the actual adopted inventory before enabling native consent.
            match service.current_grant_access_until(ManagementPackage {
                digest: self.0.digest.clone(), instance: self.0.identity.generation,
                account_epoch: self.0.epoch + 1, permissions: Vec::new(),
            }, lease.expires()).await {
                Ok(access) => {
                    if let Err(error) = manager.resources(Default::default()).await {
                        access.retire(); manager.retire(); return Err(error.code);
                    }
                    Some(access)
                }
                Err(error) if matches!(error.as_str(), "current_profile_required" | "adopted_grant_device_reauthorization_required") => None,
                Err(error) => { manager.retire(); return Err(error); }
            }
        } else { None };
        let mut current = self
            .0
            .manager
            .lock()
            .map_err(|_| "management_unavailable")?;
        if let Err(error) = self.check(lease) {
            #[cfg(feature = "resource-adoption")]
            if let Some(access) = &grant_access { access.retire(); }
            manager.retire();
            return Err(error);
        }
        if manager.is_retired() {
            #[cfg(feature = "resource-adoption")]
            if let Some(access) = &grant_access { access.retire(); }
            return Err("management_scope_changed".into());
        }
        #[cfg(feature = "resource-adoption")]
        if let Some(access) = grant_access {
            let mut cached = self.0.grant_access.lock().map_err(|_| "management_unavailable")?;
            if let Some(old) = cached.replace(access) { old.retire(); }
        }
        *current = Some(manager.clone());
        Ok(manager)
    }
    pub async fn request(&self, lease: Lease, service: &str, args: Value) -> Result<Value, String> {
        let account = self.check(&lease)?;
        lease.authorize(&account, service, None)?;
        if !octosense_app_contract::hagency::SERVICES.contains(&service) {
            return Err("unknown_management_service".into());
        }
        if service == reauthentication::SERVICE {
            let _:Empty=parse(args)?;
            return Err("native_account_confirmation_required".into());
        }
        #[cfg(not(feature = "shared-intake"))]
        if service == control::READINESS || service == control::SET {
            let _ = control::command(service, args)?;
            return Err("shared_intake_disabled".into());
        }
        if service == execution::LIST { return self.execution_assignments(&lease,args).await; }
        if service == grants::APPLICANTS { return rinx_miniapp_core::bounded_reply(self.grant_applicants(&lease, args).await?); }
        if service == grants::FLOOR { return rinx_miniapp_core::bounded_reply(self.grant_floor(&lease, args).await?); }
        if service == people::SEARCH { return rinx_miniapp_core::bounded_reply(self.people_search(&lease, args).await?); }
        if service == execution::REVIEW || service == execution::RESUME {let _ = execution::Selection::parse(args)?;return Err("native_execution_confirmation_required".into());}
        if service == "hagency.onboarding.status" {
            let _:Empty=parse(args)?;
            #[cfg(feature = "resource-adoption")]
            return self.onboarding_status(&lease).await;
            #[cfg(not(feature = "resource-adoption"))]
            return Ok(json!({"state":"unavailable","canReview":false,"grantReviewAvailable":false,"grantIntents":[],"reason":"onboarding_unavailable"}));
        }
        if service == "hagency.onboarding.review" {
            return Err("native_adoption_confirmation_required".into());
        }
        if service == accounts::LOGIN {let _=accounts::login_args(args)?;return Err("native_account_login_confirmation_required".into());}
        if service == retirement::RETIRE {
            let input: ::hagency::native_owner::RetireAccount=parse(args)?;
            input.validate().map_err(|_|"invalid_management_arguments".to_owned())?;
            return Err("native_account_retirement_confirmation_required".into());
        }
        if [personal_configuration::MODEL, personal_configuration::WORKSPACE, personal_configuration::RULES].contains(&service) {
            let _ = personal_configuration::parse_input(service, args)?;
            return Err("native_personal_configuration_confirmation_required".into());
        }
        if service == personal_configuration::CHOICES {
            let query: Id = parse(args)?;
            let manager = self.manager(&lease).await?;
            let result = manager.personal_model_choices(query.id).await.map_err(|e| e.code);
            self.check(&lease)?;
            lease.authorize(&account, service, None)?;
            return rinx_miniapp_core::bounded_reply(value(result?)?);
        }
        if service == resource_create::CREATE {
            let _ = resource_create::parse_create(args)?;
            return Err("native_resource_create_confirmation_required".into());
        }
        if service == resource_edit::EDIT {
            let _ = resource_edit::parse_edit(args)?;
            return Err("native_resource_edit_confirmation_required".into());
        }
        if service == "hagency.intent.new" {
            let _: Empty = parse(args)?;
            return Ok(json!({"commandId":uuid::Uuid::new_v4().to_string()}));
        }
        // Parse before lazy initialization, so malformed requests cannot create
        // owner enrollment or private ledger state as a side effect.
        if let Some(call) = web_connections::call(service, args.clone())? {
            let mutates = call.mutation();
            let manager = self.manager(&lease).await?;
            let result = call.execute(&manager).await;
            let changed = |error: String| if mutates { "management_outcome_unknown".to_owned() } else { error };
            self.check(&lease).map_err(changed)?;
            lease.authorize(&account, service, None).map_err(changed)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = web_catalog::call(service, args.clone())? {
            let mutates = call.mutates();
            let manager = self.manager(&lease).await?;
            let result = call.execute(&manager).await;
            let changed = |error: String| if mutates { "management_outcome_unknown".to_owned() } else { error };
            self.check(&lease).map_err(changed)?;
            lease.authorize(&account, service, None).map_err(changed)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = web_overview::call(service, args.clone())? {
            let mutates = call.mutates();
            let manager = self.manager(&lease).await?;
            let result = call.execute(&manager).await;
            let changed = |error: String| if mutates { "management_outcome_unknown".to_owned() } else { error };
            self.check(&lease).map_err(changed)?;
            lease.authorize(&account, service, None).map_err(changed)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = templates::call(service, args.clone())? {
            let mutates = call.mutates();
            let manager = self.manager(&lease).await?;
            let result = call.execute(&manager).await;
            let changed = |error: String| if mutates { "management_outcome_unknown".to_owned() } else { error };
            self.check(&lease).map_err(changed)?;
            lease.authorize(&account, service, None).map_err(changed)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = graphs::call(service, args.clone())? {
            let mutates = call.mutates();
            let manager = self.manager(&lease).await?;
            let result = call.execute(&manager).await;
            let changed = |error: String| if mutates { "management_outcome_unknown".to_owned() } else { error };
            self.check(&lease).map_err(changed)?;
            lease.authorize(&account, service, None).map_err(changed)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call)=tasks::write(service,args.clone())? {
            let mutates=call.mutates();
            let manager=self.manager(&lease).await?;
            let result=call.execute(&manager).await;
            let changed=|error:String| if mutates {"management_outcome_unknown".to_owned()} else {error};
            self.check(&lease).map_err(changed)?;
            lease.authorize(&account,service,None).map_err(changed)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = tasks::call(service,args.clone())? {
            let manager=self.manager(&lease).await?;
            let result=call.execute(&manager).await;
            self.check(&lease)?;
            lease.authorize(&account,service,None)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = setup::parse_call(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let result = call.execute(manager).await;
            self.check(&lease)?;
            lease.authorize(&account, service, None)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(input) = accounts::preparation(service, args.clone())? {
            #[cfg(feature = "resource-adoption")]
            if crate::hagency::backend::Service::for_current()?.activation_state().await? != "active" {
                return Err("device_setup_required".into());
            }
            if !cfg!(feature = "resource-adoption") {
                return Err("onboarding_unavailable".into());
            }
            let manager = self.manager(&lease).await?;
            let result = manager.prepare_account(input).await.map_err(|error| error.code);
            self.check(&lease).map_err(|_| "management_outcome_unknown".to_owned())?;
            lease.authorize(&account, service, None).map_err(|_| "management_outcome_unknown".to_owned())?;
            return rinx_miniapp_core::bounded_reply(value(result?)?);
        }
        if let Some(query) = accounts::models(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let result = manager.account_models(query).await.map_err(|error| error.code);
            self.check(&lease)?;
            lease.authorize(&account, service, None)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(query) = accounts::query(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let result = manager.account_inventory(query).await.map_err(|error| error.code);
            self.check(&lease)?;
            lease.authorize(&account, service, None)?;
            return rinx_miniapp_core::bounded_reply(value(result?)?);
        }
        if let Some(call) = stopped::call(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let mutation = matches!(call, stopped::Call::Review(_) | stopped::Call::Resolve(_));
            let encode = |v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_outcome_unknown".into()});
            let result = match call {
                stopped::Call::List(q) => manager.stopped_tasks(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                stopped::Call::Get(q) => manager.stopped_inspection(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                stopped::Call::Review(q) => manager.review_stopped(q).await.and_then(encode),
                stopped::Call::Resolve(q) => manager.resolve_stopped(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_outcome_unknown".into()})),
            }.map_err(|error| error.code);
            self.check(&lease).and_then(|_| lease.authorize(&account, service, None))
                .map_err(|error| if mutation {"management_outcome_unknown".into()} else {error})?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = interrupted_calls::call(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let mutation = matches!(call, interrupted_calls::Call::Reconcile(_));
            let encode = |v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_outcome_unknown".into()});
            let result = match call {
                interrupted_calls::Call::List(q) => manager.interrupted_calls(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                interrupted_calls::Call::Reconcile(q) => manager.reconcile_interrupted_call(q).await.and_then(encode),
            }.map_err(|error| error.code);
            self.check(&lease).and_then(|_| lease.authorize(&account, service, None))
                .map_err(|error| if mutation {"management_outcome_unknown".into()} else {error})?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = workspaces::call(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let mutation = matches!(call, workspaces::Call::Clear(_));
            let result = match call {
                workspaces::Call::List(q) => manager.resource_workspaces(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                workspaces::Call::Get(q) => manager.resource_workspace(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                workspaces::Call::Clear(q) => manager.clear_workspace_dirty(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_outcome_unknown".into()})),
            }.map_err(|error| error.code);
            self.check(&lease).and_then(|_| lease.authorize(&account, service, None))
                .map_err(|error| if mutation {"management_outcome_unknown".into()} else {error})?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = execution_policy::call(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let mutation = matches!(call, execution_policy::Call::Edit(_));
            let result = match call {
                execution_policy::Call::List(q) => manager.agent_execution_policies(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                execution_policy::Call::Get(q) => manager.agent_execution_policy(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                execution_policy::Call::Edit(q) => manager.edit_execution_policy(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_outcome_unknown".into()})),
            }.map_err(|error| error.code);
            self.check(&lease).and_then(|_| lease.authorize(&account, service, None))
                .map_err(|error| if mutation {"management_outcome_unknown".into()} else {error})?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = tool_approvals::call(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let mutation = matches!(call, tool_approvals::Call::Revoke(_));
            let result = match call {
                tool_approvals::Call::List(q) => manager.tool_approvals(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                tool_approvals::Call::Get(q) => manager.tool_approval(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                tool_approvals::Call::Open(q) => manager.tool_approval(q).await.and_then(|v| tool_approvals::destination(v,&account).map_err(|code| ::hagency::native_owner::NativeError {status:503,code})),
                tool_approvals::Call::Grants(q) => manager.tool_grants(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                tool_approvals::Call::Revoke(q) => manager.revoke_tool_grant(q).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_outcome_unknown".into()})),
            }.map_err(|error| error.code);
            self.check(&lease).and_then(|_| lease.authorize(&account, service, None))
                .map_err(|error| if mutation {"management_outcome_unknown".into()} else {error})?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = approval_bindings::call(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let mutation = matches!(call, approval_bindings::Call::Retire(_));
            let result = match call {
                approval_bindings::Call::List(query) => manager.resource_approval_bindings(query).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_invalid_result".into()})),
                approval_bindings::Call::Retire(input) => manager.retire_resource_approval_binding(input).await.and_then(|v| serde_json::to_value(v).map_err(|_| ::hagency::native_owner::NativeError {status:503,code:"management_outcome_unknown".into()})),
            }.map_err(|error| error.code);
            self.check(&lease).and_then(|_| lease.authorize(&account, service, None))
                .map_err(|error| if mutation {"management_outcome_unknown".into()} else {error})?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call) = alerts::call(service, args.clone())? {
            let manager=self.manager(&lease).await?;
            let mutation=matches!(call,alerts::Call::Triage(_));
            let result=match call {
                alerts::Call::List(q)=>manager.resource_alerts(q).await.and_then(|v|serde_json::to_value(v).map_err(|_|::hagency::native_owner::NativeError{status:503,code:"management_invalid_result".into()})),
                alerts::Call::Get(q)=>manager.resource_alert(q).await.and_then(|v|serde_json::to_value(v).map_err(|_|::hagency::native_owner::NativeError{status:503,code:"management_invalid_result".into()})),
                alerts::Call::Triage(q)=>manager.triage_resource_alert(q).await.and_then(|v|serde_json::to_value(v).map_err(|_|::hagency::native_owner::NativeError{status:503,code:"management_outcome_unknown".into()})),
            }.map_err(|e|e.code);
            self.check(&lease).and_then(|_|lease.authorize(&account,service,None)).map_err(|e|if mutation{"management_outcome_unknown".into()}else{e})?;
            return rinx_miniapp_core::bounded_reply(alerts::display(result?));
        }
        if let Some(input) = resources::parse_rename(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let result = manager.rename_resource(input).await.map_err(|error| error.code);
            // An effect can commit before package retirement; preserve custody.
            self.check(&lease).map_err(|_| "management_outcome_unknown".to_owned())?;
            lease.authorize(&account, service, None).map_err(|_| "management_outcome_unknown".to_owned())?;
            return rinx_miniapp_core::bounded_reply(value(result?)?);
        }
        if let Some(query) = resources::parse_get(service, args.clone())? {
            let manager = self.manager(&lease).await?;
            let result = manager.resource_detail(query).await.map_err(|error| error.code);
            self.check(&lease)?;
            lease.authorize(&account, service, None)?;
            return rinx_miniapp_core::bounded_reply(value(result?)?);
        }
        if let Some(id) = budget::get_id(service,args.clone())? {
            let manager=self.manager(&lease).await?;
            let result=manager.agent_budget(id).await.map_err(|error|error.code);
            self.check(&lease)?;
            lease.authorize(&account,service,None)?;
            return rinx_miniapp_core::bounded_reply(budget::display_reply(service,result?)?);
        }
        if service==pools::ROOM {
            let a:pools::Room=parse(args)?;
            let manager=self.manager(&lease).await?;
            let result=manager.pool_agent_room(a.command_id,a.name).await.map_err(|e|e.code);
            self.check(&lease).and_then(|_|lease.authorize(&account,service,None)).map_err(|_|"management_outcome_unknown".to_owned())?;
            return rinx_miniapp_core::bounded_reply(value(result?)?);
        }
        if let Some(id) = agents::status_id(service,args.clone())? {
            let manager=self.manager(&lease).await?;
            let result=manager.agent_status(id).await.map_err(|error|error.code);
            self.check(&lease)?;
            lease.authorize(&account,service,None)?;
            return rinx_miniapp_core::bounded_reply(agents::status_display(value(result?)?)?);
        }
        if let Some(call)=local::parse_call(service,args.clone())? {
            let result=call.execute(&account).await;
            self.check(&lease)?;
            lease.authorize(&account,service,None)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        if let Some(call)=personal::parse_call(service,args.clone())? {
            let manager=self.manager(&lease).await?;
            let result=call.execute(manager).await;
            self.check(&lease)?;
            lease.authorize(&account,service,None)?;
            return rinx_miniapp_core::bounded_reply(result?);
        }
        let command = central_command(service, args.clone())?;
        let resource = match service {
            "hagency.intent.pending" => {
                let _: Empty = parse(args)?;
                Some(ResourceCall::Pending)
            }
            "hagency.intent.retry" => {
                Some(ResourceCall::Retry(parse::<CommandId>(args)?.command_id))
            }
            "hagency.intent.acknowledge" => Some(ResourceCall::Acknowledge(
                parse::<CommandId>(args)?.command_id,
            )),
            "hagency.resources.list" => Some(ResourceCall::List(parse(args)?)),
            "hagency.resources.publish" => Some(ResourceCall::Publish(parse(args)?)),
            _ => None,
        };
        let manager = self.manager(&lease).await?;
        let result = match (command, resource) {
            (None, Some(ResourceCall::Pending)) => manager.saved_commands().await.map_err(|e| e.code)
                .map(|rows| json!({"commands":rows.into_iter().map(|r| {
                    let reply=json!(r.reply);
                    json!({"commandId":r.command_id,"operation":r.operation,"hasReply":!reply.is_null(),
                        "error":if reply["kind"]=="failure" {reply["data"]["code"].clone()} else {Value::Null}})
                }).collect::<Vec<_>>()})),
            (None, Some(ResourceCall::Retry(id))) => manager.retry_saved_command(id).await.map_err(|e|e.code),
            (None, Some(ResourceCall::Acknowledge(id))) => manager.acknowledge_command(id).await.map(|()|json!({"acknowledged":true})).map_err(|e|e.code),
            (Some(command @ (ManagementCommand::SharedApprovals{..}|ManagementCommand::SharedApprovalDecide{..})),None) => Box::pin(crate::hagency::backend::Service::for_current()?.shared_permissions_with(manager.clone(),command)).await,
            (Some(command), None) => manager.central(command).await.map_err(|e| e.code),
            (None, Some(ResourceCall::List(input))) => manager
                .resources(input)
                .await
                .map_err(|e| e.code)
                .and_then(value),
            (None, Some(ResourceCall::Publish(input))) => manager
                .publish_resource(input)
                .await
                .map_err(|e| e.code)
                .and_then(value),
            _ => Err("unknown_management_service".into()),
        };
        self.check(&lease)?;
        lease.authorize(&account, service, None)?;
        rinx_miniapp_core::bounded_reply(budget::display_reply(service, control::display_reply(service, allocations::display_reply(service, agents::display_reply(service, local::display(service, local_zone::display(service, pools::display(result?))))?)?)?)?)
    }
}
fn value(input: impl serde::Serialize) -> Result<Value, String> {
    serde_json::to_value(input).map_err(|_| "management_response_invalid".into())
}
enum ResourceCall {
    Pending,
    Retry(String),
    Acknowledge(String),
    List(::hagency::native_owner::ResourceQuery),
    Publish(::hagency::native_owner::PublishResource),
}
fn central_command(service: &str, args: Value) -> Result<Option<ManagementCommand>, String> {
    if service=="hagency.shared_approvals.list" {
        #[derive(Deserialize)] #[serde(rename_all="camelCase",deny_unknown_fields)] struct Query {room_id:Option<String>}
        let a:Query=parse(args)?;return Ok(Some(ManagementCommand::SharedApprovals{room_id:a.room_id}));
    }
    if service=="hagency.shared_approvals.decide" {
        #[derive(Deserialize)] #[serde(rename_all="camelCase",deny_unknown_fields)] struct Decision {id:String,digest:String,decision:String}
        let a:Decision=parse(args)?;return Ok(Some(ManagementCommand::SharedApprovalDecide{id:a.id,digest:a.digest,decision:a.decision}));
    }
    if let Some(command)=budget::command(service,args.clone())? {return Ok(Some(command));}
    if let Some(command)=control::command(service,args.clone())? {return Ok(Some(command));}
    if let Some(command)=agents::command(service,args.clone())? {return Ok(Some(command));}
    if let Some(command)=pools::command(service,args.clone())? {return Ok(Some(command));}
    if let Some(command)=allocations::command(service,args.clone())? {return Ok(Some(command));}
    Ok(Some(match service {
        "hagency.session" => {
            let _: Empty = parse(args)?;
            ManagementCommand::Session {}
        }
        "hagency.connections.list" => {
            let a: Cursor = parse(args)?;
            ManagementCommand::Connections { after: a.after }
        }
        "hagency.connections.get" => {
            let a: Id = parse(args)?;
            ManagementCommand::Connection { id: a.id }
        }
        "hagency.inbox.list" => {
            let a: Inbox = parse(args)?;
            ManagementCommand::Inbox {
                after: a.after,
                needs_my_action: a.needs_my_action,
                include_projects: a.include_projects,
                include_allocations: a.include_allocations,
            }
        }
        "hagency.inbox.get" => {
            let a: Id = parse(args)?;
            ManagementCommand::Request { id: a.id }
        }
        "hagency.resources.catalog" => {
            let a: Catalog = parse(args)?;
            ManagementCommand::Catalog {
                after: a.after,
                capability: a.capability,
            }
        }
        "hagency.projects.list" => {
            let a: Projects = parse(args)?;
            ManagementCommand::Projects {
                after: a.after,
                needs_my_action: a.needs_my_action,
            }
        }
        "hagency.projects.get" => {
            let a: Id = parse(args)?;
            ManagementCommand::Project { id: a.id }
        }
        "hagency.projects.request" => {
            let a: ProjectRequest = parse(args)?;
            ManagementCommand::ProjectRequest {
                command_id: a.command_id,
                contribution_id: a.contribution_id,
                name: a.name,
                purpose: a.purpose,
                requested_tokens: a.requested_tokens,
            }
        }
        "hagency.projects.decide" => {
            let a: Decide = parse(args)?;
            ManagementCommand::ProjectDecide {
                command_id: a.command_id,
                request_id: a.request_id,
                expected_revision: a.expected_revision,
                approve: a.approve,
            }
        }
        "hagency.projects.revoke" => {
            let a: ProjectRevoke = parse(args)?;
            ManagementCommand::ProjectRevoke {
                command_id: a.command_id,
                project_id: a.project_id,
                expected_revision: a.expected_revision,
            }
        }
        "hagency.connections.history" => {
            let a: ConnectionHistory = parse(args)?;
            ManagementCommand::ConnectionHistory { id:a.id, after:a.after }
        }
        "hagency.connections.reoffer" => {
            let a: Reoffer = parse(args)?;
            ManagementCommand::Reoffer {
                command_id:a.command_id, contribution_id:a.contribution_id,
                expected_contribution_revision:a.expected_contribution_revision,
                resource_id:a.resource_id, expected_revision:a.expected_revision,
                label:a.label, coordinator_mxid:a.coordinator_mxid,
            }
        }
        "hagency.connections.create" => {
            let a: Contribute = parse(args)?;
            ManagementCommand::Contribute {
                publication_first: a.publication_first,
                command_id: a.command_id,
                resource_id: a.resource_id,
                expected_revision: a.expected_revision,
                label: a.label,
                coordinator_mxid: a.coordinator_mxid,
            }
        }
        "hagency.inbox.decide" => {
            let a: Decide = parse(args)?;
            ManagementCommand::Decide {
                command_id: a.command_id,
                request_id: a.request_id,
                expected_revision: a.expected_revision,
                approve: a.approve,
            }
        }
        "hagency.connections.revoke" => {
            let a: Revoke = parse(args)?;
            ManagementCommand::Revoke {
                command_id: a.command_id,
                contribution_id: a.contribution_id,
                expected_revision: a.expected_revision,
            }
        }
        "hagency.connections.verify" => {
            let a: Verify = parse(args)?;
            ManagementCommand::Verify {
                command_id: a.command_id,
                contribution_id: a.contribution_id,
                expected_revision: a.expected_revision,
                resource_id: a.resource_id,
                resource_revision: a.resource_revision,
            }
        }
        "hagency.connections.resume" => {
            let a: Verify = parse(args)?;
            ManagementCommand::Resume {
                command_id: a.command_id,
                contribution_id: a.contribution_id,
                expected_revision: a.expected_revision,
                resource_id: a.resource_id,
                resource_revision: a.resource_revision,
            }
        }
        "hagency.intent.pending"
        | "hagency.intent.retry"
        | "hagency.intent.acknowledge"
        | "hagency.resources.list"
        | "hagency.resources.publish" => return Ok(None),
        _ => return Err("unknown_management_service".into()),
    }))
}
#[cfg(test)]
mod cache_tests;

#[cfg(test)]
mod tests {
    #[test]
    fn resource_reoffer_host_contract_rejects_script_authority_and_keeps_revisions_separate() {
        let input=serde_json::json!({"commandId":"reoffer_once","contributionId":"contribution_one","expectedContributionRevision":3,"resourceId":"resource_one","expectedRevision":"a".repeat(64),"label":"Updated resource","coordinatorMxid":"@coordinator:test"});
        assert!(matches!(super::central_command("hagency.connections.reoffer",input.clone()).unwrap(),Some(super::ManagementCommand::Reoffer{expected_contribution_revision:3,..})));
        for key in ["runtime","capabilities","ownerMxid","nativeFence","allocatedTokens","url","token"] {
            let mut forged=input.clone();forged[key]=serde_json::json!("forged");
            assert!(super::central_command("hagency.connections.reoffer",forged).is_err());
        }
        assert!(matches!(super::central_command("hagency.connections.history",serde_json::json!({"id":"contribution_one","after":"request_one"})).unwrap(),Some(super::ManagementCommand::ConnectionHistory{..})));
        assert!(super::central_command("hagency.connections.history",serde_json::json!({"id":"contribution_one","ownerMxid":"@someone:test"})).is_err());
    }

    use super::*;
    #[test]
    fn project_host_inputs_are_closed_and_grants_never_alias_resource_approval() {
        let request = json!({"commandId":"once","contributionId":"con_1","name":"Documentation","purpose":"Write manuals","requestedTokens":1000});
        assert!(matches!(
            central_command("hagency.projects.request", request.clone()),
            Ok(Some(ManagementCommand::ProjectRequest {
                requested_tokens: 1000,
                ..
            }))
        ));
        for field in [
            "owner",
            "managerMxid",
            "isAdmin",
            "approved",
            "allocatedTokens",
            "workspace",
            "token",
            "url",
        ] {
            let mut injected = request.clone();
            injected[field] = json!("injected");
            assert!(central_command("hagency.projects.request", injected).is_err());
        }
        for tokens in [json!(1.5), json!("1000"), json!(null)] {
            let mut invalid = request.clone();
            invalid["requestedTokens"] = tokens;
            assert!(central_command("hagency.projects.request", invalid).is_err());
        }
        assert!(
            central_command(
                "hagency.projects.decide",
                json!({"commandId":"one","requestId":"req_1","expectedRevision":2,"approve":true})
            )
            .is_ok()
        );
        assert!(central_command("hagency.projects.decide", json!({"commandId":"one","requestId":"req_1","expectedRevision":2,"approve":true,"reserve":true})).is_err());
        let authority = rinx_miniapp_core::SessionAuthority::default();
        let identity = InstanceId {
            app: octosense_app_contract::hagency::APP_ID.into(),
            account: "@manager:test".into(),
            room: None,
            generation: 1,
        };
        let lease = |services: &[&str]| {
            authority.issue(
                identity.clone(),
                services.iter().map(|s| s.to_string()).collect(),
                Default::default(),
                Instant::now() + Duration::from_secs(60),
            )
        };
        let legacy = permissions(&lease(&[
            "hagency.inbox.decide",
            "hagency.connections.create",
            "hagency.connections.revoke",
            "hagency.intent.retry",
        ]));
        for grant in [
            ManagementPermission::RequestProjects,
            ManagementPermission::DecideProjects,
            ManagementPermission::RevokeProjects,
            ManagementPermission::ReadCatalog,
            ManagementPermission::ReadProjects,
        ] {
            assert!(!legacy.contains(&grant));
        }
        assert_eq!(
            permissions(&lease(&["hagency.projects.decide"])),
            vec![ManagementPermission::DecideProjects]
        );
        assert_eq!(
            permissions(&lease(&["hagency.projects.request"])),
            vec![ManagementPermission::RequestProjects]
        );
    }

    #[test]
    fn connection_verification_accepts_identifiers_but_no_claimed_provider_or_authority() {
        let input = json!({"commandId":"verify_once","contributionId":"con_1","expectedRevision":2,
            "resourceId":"resource_1","resourceRevision":"a".repeat(64)});
        assert!(matches!(
            central_command("hagency.connections.verify", input.clone()),
            Ok(Some(ManagementCommand::Verify { .. }))
        ));
        assert!(matches!(
            central_command("hagency.connections.resume", input.clone()),
            Ok(Some(ManagementCommand::Resume { .. }))
        ));
        for field in [
            "verified",
            "provider",
            "credentialRef",
            "owner",
            "device",
            "generation",
            "freshUntilMs",
            "url",
        ] {
            let mut injected = input.clone();
            injected[field] = json!(true);
            assert!(
                central_command("hagency.connections.verify", injected.clone()).is_err(),
                "accepted {field}"
            );
            assert!(central_command("hagency.connections.resume", injected).is_err());
        }
    }
    #[test]
    fn removed_resource_services_are_not_dispatchable_or_capabilities() {
        for service in ["hagency.providers.list", "hagency.providers.connect", "hagency.resources.enroll", "hagency.resources.configure"] {
            assert!(central_command(service, json!({})).is_err());
            assert!(!octosense_app_contract::hagency::SERVICES.contains(&service));
            assert!(!octosense_app_contract::KNOWN_CAPABILITIES.contains(&service));
        }
    }
    #[test]
    fn recovery_accepts_only_an_opaque_command_id() {
        assert!(parse::<CommandId>(json!({"commandId":"original-command"})).is_ok());
        assert!(parse::<CommandId>(json!({})).is_err());
        for field in [
            "actor",
            "input",
            "operation",
            "reply",
            "token",
            "url",
            "deviceId",
        ] {
            let mut args = json!({"commandId":"original-command"});
            args[field] = json!("injected");
            assert!(parse::<CommandId>(args).is_err());
        }
        for service in [
            "hagency.intent.pending",
            "hagency.intent.retry",
            "hagency.intent.acknowledge",
        ] {
            assert!(central_command(service, json!({})).unwrap().is_none());
            assert!(octosense_app_contract::hagency::SERVICES.contains(&service));
        }
    }

    #[test]
    fn closed_service_arguments_reject_actor_credentials_urls_and_unknown_operations() {
        for service in [
            "hagency.session",
            "hagency.connections.list",
            "hagency.inbox.list",
        ] {
            assert!(central_command(service, json!({})).is_ok());
            for field in ["actor", "token", "url", "deviceId", "isAdmin", "operation"] {
                let mut args = json!({});
                args[field] = json!("injected");
                assert!(central_command(service, args).is_err());
            }
        }
        assert!(central_command("hagency.shell", json!({})).is_err());
        assert!(
            central_command(
                "hagency.inbox.decide",
                json!({"requestId":"r","approve":true})
            )
            .is_err()
        );
    }
}
