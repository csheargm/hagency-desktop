//! Direct native management services shared by desktop and console adapters.
//!
//! A session is constructed by the trusted host after package consent and Matrix
//! account validation. It is never deserialized from a script. The host supplies
//! its current context on every call, and retires the session on account/package
//! changes. Mutations retain the existing writer's finite, revocable authority.
use crate::{
    CeilingChange, CeilingReport, DomainRepository, DomainStore, Error, ProfileChange,
    ResourceConfigurationResult,
    ResourcePublicationAccess, ResourcePublicationResult, ResourcePublicationRetirement,
    resource_publication_revision,
};
use hagency_core::{allocation::Budget, project::identifier};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::BTreeSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const PROTOCOL: &str = "hagency-management-v1";
pub const MAX_PAGE: usize = 16;
const MAX_SCAN: usize = 100;
const MAX_SESSION: Duration = Duration::from_secs(300);

/// Host-derived identity. No Serialize/Deserialize: scripts cannot supply this.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostContext {
    account: String,
    account_epoch: u64,
    package_digest: String,
    instance: u64,
    device: String,
    device_generation: u64,
}
impl HostContext {
    pub fn new(
        account: String,
        account_epoch: u64,
        package_digest: String,
        instance: u64,
        device: String,
        device_generation: u64,
    ) -> Result<Self, Error> {
        // The shared coordinator contract validates Matrix identifiers; a
        // syntactically valid identifier alone does not grant any authority.
        palpo_hagency_contract::MatrixUserId::try_from(account.clone())
            .map_err(|_| Error::LocalAuthority)?;
        identifier(&device, 128)?;
        if account_epoch == 0
            || instance == 0
            || device_generation == 0
            || package_digest.len() != 64
            || !package_digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::LocalAuthority);
        }
        Ok(Self {
            account,
            account_epoch,
            package_digest,
            instance,
            device,
            device_generation,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Permission {
    ReadWebCatalog,
    ManageWebCatalog,
    ReadTasks,
    ReadOperatorGraphs,
    CreateOperatorGraphs,
    UpdateOperatorGraphNodes,
    CancelOperatorGraphs,
    ReadOperatorGraphRecipients,
    BindOperatorGraphRecipients,
    CreateTasks,
    EditTasks,
    DeleteTasks,
    AcceptTasks,
    TransitionTasks,
    CommentTasks,
    ReadTaskBoard,
    ReadAlerts,
    /// Trusted native background observation; not a mini-app service.
    ObserveResourceAlerts,
    TriageAlerts,
    ReadApprovalBindings,
    RetireApprovalBindings,
    ReadToolApprovals,
    InspectToolGrants,
    RevokeToolGrants,
    ReadExecutionPolicy,
    EditExecutionPolicy,
    ReadWorkspaces,
    ClearWorkspaceDirty,
    ReadStoppedTasks,
    InspectStoppedTasks,
    ResolveStoppedTasks,
    // Owner declaration that closes a managed call whose process is gone.
    ReconcileAgentCalls,
    ReadResources,
    ReadResourceTemplates,
    ManageResourceTemplates,
    RenameResources,
    ReadProviders,
    ReadAccounts,
    PrepareAccounts,
    RetireAccounts,
    LoginAccounts,
    ReadAccountModels,
    ConnectProviders,
    ReadConnections,
    ReadCatalog,
    ReadProjects,
    ReadAgentRequests,
    ReadAgentStatus,
    ReadPersonalAgentSettings,
    RequestPersonalAgentSettings,
    EditPersonalAgentPolicy,
    EditPersonalAgentModel,
    PreparePersonalAgentWorkspace,
    ReadAgentBudget,
    RequestAgentTopUps,
    DecideAgentTopUps,
    CancelAgentTopUps,
    ReadAgentRunControl,
    ChangeAgentRunControl,
    ReadAgentWork,
    ReadAgentCleanup,
    ReadAgentSettlement,
    RequestAgentSettlement,
    RevokeAgentRequests,
    ReadAllocations,
    DecideAllocations,
    ReleaseAllocations,
    RequestAgents,
    DecideAgentRequests,
    CancelAgentRequests,
    ReadProjectSetup,
    ManageProjectSetup,
    CreateProjectRooms,
    AssignProjectAdministrators,
    RequestProjects,
    DecideProjects,
    RevokeProjects,
    ReadInbox,
    DecideInbox,
    RevokeConnections,
    VerifyConnections,
    ResumeConnections,
    ReadIntents,
    AcknowledgeIntents,
    ConfigureResources,
    EnrollResources,
    ContributeResources,
    PublishResources,
    PublishGrants,
    // Native trusted confirmation only; no mini-app service maps to this grant.
    ConsentGrants,
    // Native migration review only; no mini-app capability maps to this grant.
    PrepareSourceQuiescence,
    // Native explicit history-copy confirmation; no mini-app service.
    ExecuteAllocations,
    // Native executor adapter only; no mini-app capability grants this.
    ExecuteManagedAgents,
    SettleGrants,
}

struct Scope {
    context: HostContext,
    permissions: BTreeSet<Permission>,
    expires: Instant,
    retired: AtomicBool,
    fence: ResourcePublicationRetirement,
    commands: tokio::sync::Mutex<()>,
    pending: tokio::sync::Semaphore,
}
impl Drop for Scope {
    fn drop(&mut self) {
        self.fence.retire();
    }
}

/// The store must be the account's enrolled resource ledger, selected by the
/// host, never a caller-supplied file path. Clones share revocation and expiry.
#[derive(Clone)]
pub struct Session {
    store: Option<SessionStore>,
    server_journal: Option<ServerManagementJournal>,
    scope: Arc<Scope>,
}
#[derive(Clone)]
enum SessionStore {
    Current(Box<crate::owner_resources::adoption::CurrentManagementStore>),
}
impl Session {
    fn local_store(&self) -> Result<&SessionStore, Error> {
        self.store.as_ref().ok_or(Error::ResourceSetupRequired)
    }
    pub fn new_server(
        journal: ServerManagementJournal,
        context: HostContext,
        permissions: impl IntoIterator<Item = Permission>,
        expires: Instant,
    ) -> Result<Self, Error> {
        if context.account != journal.owner() || !journal.is_live() {
            return Err(Error::LocalAuthority);
        }
        Ok(Self {
            store: None,
            server_journal: Some(journal),
            scope: ManagementAuthority::new(context, permissions, expires)?.scope,
        })
    }
    pub fn new_current(
        store: crate::owner_resources::adoption::CurrentManagementStore,
        context: HostContext,
        permissions: impl IntoIterator<Item = Permission>,
        expires: Instant,
    ) -> Result<Self, Error> {
        if store.identity().1 != context.account || !store.is_live() {
            return Err(Error::LocalAuthority);
        }
        Ok(Self {
            store: Some(SessionStore::Current(Box::new(store))),
            server_journal: None,
            scope: ManagementAuthority::new(context, permissions, expires)?.scope,
        })
    }
    fn current_profile(
        &self,
        store: &crate::owner_resources::adoption::CurrentManagementStore,
    ) -> Result<crate::owner_resources::ProfileAuthority, Error> {
        let authority = self.authority();
        let (profile, owner) = store.identity();
        crate::owner_resources::ProfileAuthority::new(
            profile.into(),
            owner.into(),
            self.scope.expires,
        )?
        .with_native_gate(move || authority.is_live())
    }
    /// Trusted native enrollment adapter; the mini app receives neither handle.
    pub fn namespace_enrollment_registry(&self,context:&HostContext)->Result<(crate::owner_resources::OwnerResourceRegistry,crate::owner_resources::ProfileAuthority),Error>{
        self.check(context,Permission::EnrollResources)?;
        let SessionStore::Current(store) = self.local_store()?;
        let profile=self.current_profile(store)?;
        let registry=store.enrollment_registry(&profile)?;
        self.check(context,Permission::EnrollResources)?;
        Ok((registry,profile))
    }
    /// Native reoffer preflight uses the already selected canonical registry.
    /// This grants no creation, editing, enrollment or provider authority.
    pub fn contribution_registry(
        &self, context: &HostContext,
    ) -> Result<crate::owner_resources::OwnerResourceRegistry, Error> {
        self.check(context, Permission::ContributeResources)?;
        self.check(context, Permission::ReadResources)?;
        let SessionStore::Current(store) = self.local_store()?;
        let profile = self.current_profile(store)?;
        let registry = store.enrollment_registry(&profile)?;
        self.check(context, Permission::ContributeResources)?;
        self.check(context, Permission::ReadResources)?;
        Ok(registry)
    }
    /// Shared finite authority without exposing the selected resource store.
    pub fn authority(&self) -> ManagementAuthority {
        ManagementAuthority {
            scope: self.scope.clone(),
        }
    }
    pub fn retire(&self) {
        self.authority().retire();
    }
    pub fn is_live(&self) -> bool {
        self.authority().is_live()
    }
    /// Original finite scope deadline. Reading it does not renew authority.
    pub fn valid_until(&self) -> Instant {
        self.scope.expires
    }
    fn check(&self, context: &HostContext, permission: Permission) -> Result<(), Error> {
        self.authority().authorize(context, permission)
    }
    /// Native adapters use the same finite package grant for central operations.
    pub fn authorize(&self, context: &HostContext, permission: Permission) -> Result<(), Error> {
        self.check(context, permission)
    }
    pub async fn resource(
        &self,
        context: &HostContext,
        id: String,
    ) -> Result<ResourceSummary, Error> {
        self.check(context, Permission::ReadResources)?;
        identifier(&id, 128)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store.resource(&self.current_profile(store)?, id).await?
            }
        };
        self.check(context, Permission::ReadResources)?;
        Ok(result)
    }
    pub async fn resources(
        &self,
        context: &HostContext,
        query: ResourceQuery,
    ) -> Result<ResourcePage, Error> {
        self.check(context, Permission::ReadResources)?;
        query.validate()?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .resources(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadResources)?;
        Ok(result)
    }
    /// Persist exposure before the native adapter sends an admission request.
    /// This is deliberately not a script operation or a remote-success receipt.
    pub async fn record_contribution_attempt(
        &self,
        context: &HostContext,
        resource_id: String,
        expected_revision: String,
        command_id: String,
    ) -> Result<(), Error> {
        self.check(context, Permission::ContributeResources)?;
        identifier(&resource_id, 128)?;
        identifier(&command_id, 128)?;
        match self.local_store()? {
            SessionStore::Current(store) => {
                store.record_contribution_attempt(
                    &self.current_profile(store)?, resource_id, expected_revision, command_id,
                ).await?;
            }
        }
        self.check(context, Permission::ContributeResources)
    }
    pub async fn publish_resource(&self, context:&HostContext, input:PublishResource)
        -> Result<ResourcePublicationResult,Error> {
        let _pending=self.scope.pending.try_acquire().map_err(|_|Error::Busy)?;
        let _command=self.scope.commands.lock().await;
        self.check(context,Permission::PublishResources)?;
        input.validate()?;
        {
            let SessionStore::Current(store)=self.local_store()?;
            let receipt=CommandReceipt::new(context,"resource.publish",&input.command_id,&input)?;
            let result=store.publish_resource(&self.current_profile(store)?,input,receipt).await?;
            self.check(context,Permission::PublishResources).map_err(|_|Error::OutcomeUnknown)?;
            return Ok(result);
        }

    }

}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourceQuery {
    #[serde(default)]
    pub after: String,
    #[serde(default = "default_page")]
    pub limit: usize,
    pub framework: Option<String>,
    pub capability: Option<String>,
    pub published: Option<bool>,
}
fn default_page() -> usize {
    MAX_PAGE
}
impl Default for ResourceQuery {
    fn default() -> Self {
        Self {
            after: String::new(),
            limit: MAX_PAGE,
            framework: None,
            capability: None,
            published: None,
        }
    }
}
impl ResourceQuery {
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if !self.after.is_empty() {
            identifier(&self.after, 128)?;
        }
        if !(1..=MAX_PAGE).contains(&self.limit)
            || self
                .framework
                .as_deref()
                .is_some_and(|f| !matches!(f, "codex" | "claude" | "octos"))
            || self
                .capability
                .as_deref()
                .is_some_and(|c| !hagency_core::qualification::roles().any(|r| r == c))
        {
            return Err(hagency_core::InvalidInput("invalid management resource filter").into());
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublishResource {
    pub command_id: String,
    pub resource_id: String,
    pub expected_revision: String,
    pub published: bool,
}
impl PublishResource {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        identifier(&self.resource_id, 128)?;
        if self.expected_revision.len() != 64 || !self.expected_revision.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err(hagency_core::InvalidInput("invalid resource revision").into());
        }
        Ok(())
    }
}

/// Native-prepared command identity. Receipts commit in the same transaction as
/// the original domain write. Package instances may restart; the durable scope
/// remains this account and installation, not an authorization generation.
/// Every operation still checks the current generation before and after work.
pub(crate) struct CommandReceipt {
    actor: String,
    key: String,
    operation: &'static str,
    digest: String,
}
impl CommandReceipt {
    pub(crate) fn new(
        context: &HostContext,
        operation: &'static str,
        key: &str,
        input: &impl Serialize,
    ) -> Result<Self, Error> {
        identifier(key, 128)?;
        let input = serde_json::to_value(input)?;
        if serde_json::to_vec(&input)?.len() > 16 * 1024 {
            return Err(Error::Capacity);
        }
        Ok(Self {
            actor: hagency_core::canonical::digest(&serde_json::json!({
                "account":context.account,"device":context.device
            }))?,
            key: key.into(),
            operation,
            digest: hagency_core::canonical::digest(&input)?,
        })
    }
    pub(crate) fn lookup<T: DeserializeOwned>(&self, db: &Connection) -> Result<Option<T>, Error> {
        let row: Option<(String, String, String)> = db.query_row(
            "SELECT operation,digest,result FROM management_commands WHERE actor_scope=?1 AND command_id=?2",
            params![self.actor, self.key], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?;
        match row {
            Some((op, digest, result)) if op == self.operation && digest == self.digest => {
                Ok(Some(serde_json::from_str(&result)?))
            }
            Some(_) => Err(Error::Conflict),
            None => Ok(None),
        }
    }
    pub(crate) fn record(&self, db: &Connection, result: &impl Serialize) -> Result<(), Error> {
        // Never evict a receipt and silently allow its command to run again.
        let count: i64 =
            db.query_row("SELECT COUNT(*) FROM management_commands", [], |r| r.get(0))?;
        if count >= 100_000 {
            return Err(Error::Capacity);
        }
        db.execute(
            "INSERT INTO management_commands(actor_scope,command_id,operation,digest,result) VALUES(?1,?2,?3,?4,?5)",
            params![self.actor, self.key, self.operation, self.digest, serde_json::to_string(result)?],
        )?;
        Ok(())
    }
}

/// Public configuration and ledger observations only. Account homes, credentials,
/// provider login URLs, bootstrap commands and filesystem paths are excluded.
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSummary {
    pub id: String,
    pub revision: String,
    pub framework: String,
    pub model: String,
    pub reasoning: Option<String>,
    pub capabilities: Vec<String>,
    pub published: bool,
    pub budget: Budget,
    pub usage: CeilingReport,
    /// Profile-local descriptive metadata; never part of effective configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_name: Option<ResourceLocalName>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourcePage {
    #[serde(deserialize_with = "crate::management::wire_static::one")]
    pub protocol: crate::management::wire_static::Label,
    pub observed_at_ms: u64,
    pub resources: Vec<ResourceSummary>,
    pub next_after: Option<String>,
}

fn resource_capabilities(resource: &hagency_core::project::Resource) -> Vec<String> {
    let mut profile = resource.clone();
    // Management must allow choosing capabilities before publication. The
    // public catalog still consults the original publication/availability gates.
    profile.published = true;
    profile.eligible_roles()
}

impl DomainRepository {
    pub(crate) fn management_resource(&self, id: &str, at: u64) -> Result<ResourceSummary, Error> {
        let config = self.resource_configuration(id)?;
        let (budget, usage) = self.resource_headroom(id, at)?;
        Ok(ResourceSummary {
            id: id.into(),
            revision: resource_publication_revision(&config)?,
            framework: config.framework.clone(),
            model: config.model.clone(),
            reasoning: config.reasoning.clone(),
            capabilities: resource_capabilities(&config),
            published: config.published,
            budget,
            usage,
            local_name: None,
        })
    }
    /// One bounded writer job: configuration, revision, reservations and usage
    /// are observed together. No per-row asynchronous reads can mix versions.
    pub(crate) fn management_resources(
        &self,
        query: &ResourceQuery,
        at: u64,
    ) -> Result<ResourcePage, Error> {
        query.validate()?;
        let scanned = self.resource_configurations(&query.after, MAX_SCAN)?;
        let has_more = scanned.len() == MAX_SCAN;
        let mut resources = Vec::with_capacity(query.limit);
        let mut next_after = None;
        for (i, row) in scanned.into_iter().enumerate() {
            let config = row.config;
            let capabilities = resource_capabilities(&config);
            let matches = query
                .framework
                .as_ref()
                .is_none_or(|f| *f == config.framework)
                && query
                    .capability
                    .as_ref()
                    .is_none_or(|r| capabilities.contains(r))
                && query.published.is_none_or(|p| p == config.published);
            if matches {
                if resources.len() == query.limit {
                    // Keep the previous cursor: this matching row must appear
                    // on the next page instead of being skipped.
                    break;
                }
                let (budget, usage) = self.resource_headroom(&row.id, at)?;
                resources.push(ResourceSummary {
                    revision: resource_publication_revision(&config)?,
                    id: row.id.clone(),
                    framework: config.framework,
                    model: config.model,
                    reasoning: config.reasoning,
                    capabilities,
                    published: config.published,
                    budget,
                    usage,
            local_name: None,
                });
            }
            next_after = Some(row.id);
            if i + 1 == MAX_SCAN && has_more {
                break;
            }
        }
        // A cursor may lead to an empty terminal page; it never silently drops
        // matches after a bounded scan, even for sparse capability filters.
        if resources.len() < query.limit && !has_more {
            next_after = None;
        }
        Ok(ResourcePage {
            protocol: PROTOCOL,
            observed_at_ms: at,
            resources,
            next_after,
        })
    }
}

mod alerts;
mod approval_bindings;
mod execution_policy;
pub use execution_policy::{ExecutionPolicyListQuery, AgentExecutionPolicyPage, ExecutionPolicyQuery, ExecutionPolicyValue, PolicyApplication, AgentExecutionPolicy, EditExecutionPolicy};
mod tool_approvals;
pub use tool_approvals::{ToolApprovalQuery, ToolApprovalLookup, ToolApproval, ToolApprovalSubject, ToolApprovalSnapshot, ToolApprovalPage, ToolGrantQuery, ToolGrantMode, ToolRuleDetails, ToolRule, ToolRulePermissions, ToolGrant, ToolGrantPage, RevokeToolGrant, RevokedToolGrant};
pub use approval_bindings::{ResourceApprovalBinding, ResourceApprovalBindingPage, ResourceApprovalBindingQuery, RetireResourceApprovalBinding, RetiredResourceApprovalBinding};
pub use alerts::{ResourceAlert, ResourceAlertPage, ResourceAlertQuery, ResourceAlertDetail, ResourceAlertDetailQuery, ResourceAlertNote, TriageResourceAlert};
mod resource_templates;
mod web_catalog;
pub use web_catalog::*;
pub use resource_templates::{ResourceTemplate,ResourceTemplatePage,ResourceTemplateDetail,TemplateCreateInput,TemplateUpdateInput,TemplateDeleteInput,ResourceTemplateChange,ResourceTemplateCommand,ResourceTemplateResult};
mod resource_name;
pub use resource_name::{RenameResource, ResourceLocalName, ResourceNameResult};
mod resource_edit;
pub use resource_edit::{CeilingAmendmentRefusal, EditLocalResource};
mod resource_create;
pub use resource_create::CreateResourceConfiguration;
pub(crate) mod accounts;
mod account_retirement;
pub use account_retirement::{RetireAccount, AccountRetirement};
mod account_login;
mod account_models;
pub use account_models::{AccountModelQuery, AccountInspection};
pub use account_login::{AccountLoginRequest, AccountLoginStatus, AccountLoginHandle, AccountLoginReservation};
pub use accounts::{AccountInventory, AccountInventoryQuery, AccountSummary, PrepareAccount, AccountPreparation};
mod resource_detail;
mod operator_graphs;
pub use operator_graphs::OPERATOR_GRAPH_SCOPE;
mod operator_tasks;
pub use operator_tasks::{
    OperatorBoardPage, OperatorBoardProject, OperatorBoardQuery, OperatorTaskDetail,
    OperatorTaskDetailQuery, OperatorTaskPage, OperatorTaskQuery, OperatorTaskSummary,
    TASK_COMMENT_PAGE, TASK_READ_PAGE,
};
pub use resource_detail::{
    CeilingAmendment, CeilingAmendmentBlocker, ResourceDetail, ResourceDetailQuery,
    ResourceEditBlocker, ResourceEditing,
};
mod connection;
pub(crate) mod intents;
pub use intents::{Intent, IntentOperation, IntentReply, PendingIntent};
pub(crate) use intents::{JournalCommand, execute_intent};


mod authority;
mod server_journal;
pub use server_journal::ServerManagementJournal;
pub use authority::{ManagementAuthority, WeakManagementAuthority};
mod journal_sql;

pub(crate) mod operator_task_commands;
pub use operator_task_commands::{TaskField, CreateOperatorTask, EditOperatorTask, OperatorTaskVersion, TransitionOperatorTask, CommentOperatorTask, OperatorTaskChange, OperatorTaskCommand, OperatorTaskResult};

mod workspaces;
pub use workspaces::{WorkspaceListQuery, WorkspaceQuery, WorkspaceReleaseBlock, WorkspaceStatus, WorkspacePage, ClearWorkspaceDirty, WorkspaceRelease};

mod stopped;
pub use stopped::{StoppedListQuery,StoppedQuery,StoppedAttempt,StoppedPage,StoppedInventoryEntry,StoppedInspection,ReviewStopped,StoppedReview,StoppedDecision,ResolveStopped,StoppedResolution};

mod web_overview;
pub use web_overview::{WebAlertsQuery, WebAlertsPage, WebAlertCounts, WebWorkforceLink, WebManagedUsage, WebManagedUsageRow};

mod web_connections;
pub use web_connections::{WebConnection, WebConnectionApplicability, WebConnectionPage, WebConnectionResult, WebConnectionCommand, WebConnectionChange, WebConnectionPrivate, web_connection_credential_digest};

#[doc(hidden)]
pub mod wire_static;
