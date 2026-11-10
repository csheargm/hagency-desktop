//! Exact central management operations; no script-selected URLs or credentials.
pub(super) mod agents;
pub(super) mod pools;
pub(super) mod agent_status;
pub(super) mod agent_work;
pub(super) mod agent_cleanup;
pub(in crate::console::native) mod agent_settlement;
pub(in crate::console::native) mod agent_budget;
pub(super) mod allocations;
pub(in crate::console::native) mod agent_run_control;
pub use agent_run_control::AgentRunMode;
use super::{NativeError, NativeManagement, failure};
use crate::console::{
    device_execution::DeviceIdentity,
    server_login::{OwnerError, OwnerOperation},
};
use hagency_store::management::{Permission, ResourceSummary};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagementCommand {
    SharedApprovals {room_id:Option<String>},
    SharedApprovalGet {id:String},
    SharedApprovalDecide {id:String,digest:String,decision:String},
    Pools { after: Option<String>, capability: Option<String> },
    PoolAgents { after: Option<String>, pool_id: Option<String>, needs_my_action: bool },
    PoolAgent { id: String },
    PoolAgentRequest { command_id: String, pool_id: String, request: pools::PoolAgentInput },
    PoolAgentDecide { command_id: String, request_id: String, expected_revision: i64, approve: bool },
    PoolAgentReallocate { command_id: String, request_id: String, expected_revision: i64, expected_grant_revision: i64, renewal_mode: hagency_core::recurring_quota::RenewalMode },
    PoolAgentRenew { command_id: String, request_id: String, expected_revision: i64, expected_period_key: String },
    PoolAgentStop { command_id: String, request_id: String, expected_revision: i64 },
    Session {},
    Recover {
        command_id: String,
        original_operation: hagency_store::management::IntentOperation,
    },
    Connections {
        after: Option<String>,
    },
    Connection {
        id: String,
    },
    ConnectionHistory {
        id: String,
        after: Option<String>,
    },
    Inbox {
        after: Option<String>,
        needs_my_action: bool,
        #[serde(default)]
        include_projects: bool,
        #[serde(default)]
        include_allocations: bool,
    },
    Request {
        id: String,
    },
    Catalog {
        after: Option<String>,
        capability: Option<String>,
    },
    Projects {
        after: Option<String>,
        needs_my_action: bool,
    },
    Project {
        id: String,
    },
    ProjectRequest {
        command_id: String,
        contribution_id: String,
        name: String,
        purpose: String,
        requested_tokens: i64,
    },
    ProjectDecide {
        command_id: String,
        request_id: String,
        expected_revision: i64,
        approve: bool,
    },
    ProjectRevoke {
        command_id: String,
        project_id: String,
        expected_revision: i64,
    },
    Allocations { project_id: String, after: Option<String>, limit: Option<u16> },
    Allocation { id: String },
    AllocationDecide { command_id: String, request_id: String, expected_revision: i64, approve: bool },
    AllocationRelease { command_id: String, allocation_id: String, expected_revision: i64 },
    AgentBudget { id: String },
    AgentTopUp { id: String },
    AgentTopUps { id: String, after: Option<String>, limit: Option<u16> },
    AgentTopUpRequest { #[serde(default, skip_serializing_if = "std::ops::Not::not")] approve_immediately: bool, #[serde(default, skip_serializing_if = "std::ops::Not::not")] owner_funded: bool, command_id: String, request_id: String, expected_request_revision: i64, expected_assignment_generation: i64, expected_budget_revision: i64, expected_budget_digest: String, add_tokens: i64, purpose: String },
    AgentTopUpDecide { command_id: String, top_up_id: String, expected_revision: i64, expected_authority_revision: i64, approve: bool },
    AgentTopUpCancel { command_id: String, top_up_id: String, expected_revision: i64 },
    AgentRunReadiness { assignment_id: String },
    AgentRunControl { command_id: String, assignment_id: String, expected_revision: i64, mode: AgentRunMode },
    AgentReadiness {
        project_id: String,
    },
    AgentRequests {
        project_id: Option<String>,
        after: Option<String>,
        needs_my_action: bool,
    },
    Agent {
        id: String,
    },
    AgentAssignment { id: String },
    AgentMembership { id: String },
    AgentRuntime { id: String },
    AgentExecution { id: String },
    AgentWork { id: String, after: Option<String>, limit: Option<u16> },
    AgentWorkList { project_id: Option<String>, after: Option<String>, limit: Option<u16> },
    AgentSettlement { id: String },
    AgentSettlementRequest { command_id: String, request_id: String, expected_request_revision: i64, expected_fence_receipt_digest: String },
    AgentRemoval { id: String },
    AgentRemovals { project_id: Option<String>, after: Option<String>, limit: Option<u16> },
    AgentRevoke { command_id: String, request_id: String, expected_revision: i64 },
    AgentRequest {
        command_id: String,
        project_id: String,
        request: agents::AgentRequestInput,
    },
    AgentDecide {
        command_id: String,
        request_id: String,
        expected_revision: i64,
        expected_authority_revision: i64,
        approve: bool,
    },
    AgentCancel {
        command_id: String,
        request_id: String,
        expected_revision: i64,
    },
    Contribute {
        #[serde(default, skip_serializing_if = "publication_first_disabled")]
        publication_first: bool,
        command_id: String,
        resource_id: String,
        expected_revision: String,
        label: String,
        coordinator_mxid: String,
    },
    Reoffer {
        command_id: String,
        contribution_id: String,
        expected_contribution_revision: i64,
        resource_id: String,
        expected_revision: String,
        label: String,
        coordinator_mxid: String,
    },
    Decide {
        command_id: String,
        request_id: String,
        expected_revision: i64,
        approve: bool,
    },
    Revoke {
        command_id: String,
        contribution_id: String,
        expected_revision: i64,
    },
    Verify {
        command_id: String,
        contribution_id: String,
        expected_revision: i64,
        resource_id: String,
        resource_revision: String,
    },
    Resume {
        command_id: String,
        contribution_id: String,
        expected_revision: i64,
        resource_id: String,
        resource_revision: String,
    },
}
fn publication_first_disabled(value: &bool) -> bool { !*value }
impl ManagementCommand {
    pub(super) fn intent(&self) -> Result<Option<hagency_store::management::Intent>, NativeError> {
        use hagency_store::management::{Intent, IntentOperation as Op};
        let (kind, id) = match self {
            Self::Contribute { command_id, .. } => (Op::Contribute, command_id),
            Self::Reoffer { command_id, .. } => (Op::Reoffer, command_id),
            Self::Decide { command_id, .. } => (Op::Decide, command_id),
            Self::Revoke { command_id, .. } => (Op::Revoke, command_id),
            Self::Verify { command_id, .. } => (Op::Verify, command_id),
            Self::Resume { command_id, .. } => (Op::Resume, command_id),
            Self::ProjectRequest { command_id, .. } => (Op::ProjectRequest, command_id),
            Self::ProjectDecide { command_id, .. } => (Op::ProjectDecide, command_id),
            Self::ProjectRevoke { command_id, .. } => (Op::ProjectRevoke, command_id),
            Self::PoolAgentRequest { command_id, .. } => (Op::PoolAgentRequest, command_id),
            Self::PoolAgentDecide { command_id, .. } => (Op::PoolAgentDecide, command_id),
            Self::PoolAgentReallocate { command_id, .. } => (Op::PoolAgentReallocate, command_id),
            Self::PoolAgentRenew { command_id, .. } => (Op::PoolAgentRenew, command_id),
            Self::PoolAgentStop { command_id, .. } => (Op::PoolAgentStop, command_id),
            Self::AgentRequest { command_id, .. } => (Op::AgentRequest, command_id),
            Self::AgentDecide { command_id, .. } => (Op::AgentDecide, command_id),
            Self::AgentCancel { command_id, .. } => (Op::AgentCancel, command_id),
            Self::AgentRevoke { command_id, .. } => (Op::AgentRevoke, command_id),
            Self::AgentSettlementRequest { command_id, .. } => (Op::AgentSettlementRequest, command_id),
            Self::AgentTopUpRequest { command_id, .. } => (Op::AgentTopUpRequest, command_id),
            Self::AgentTopUpDecide { command_id, .. } => (Op::AgentTopUpDecide, command_id),
            Self::AgentTopUpCancel { command_id, .. } => (Op::AgentTopUpCancel, command_id),
            Self::AgentRunControl { command_id, .. } => (Op::AgentRunControl, command_id),
            Self::AllocationDecide { command_id, .. } => (Op::AllocationDecide, command_id),
            Self::AllocationRelease { command_id, .. } => (Op::AllocationRelease, command_id),
            _ => return Ok(None),
        };
        Ok(Some(
            Intent::new(kind, id.clone(), self).map_err(super::management::store_error)?,
        ))
    }
    fn authorize(&self, session: &hagency_store::management::Session, context: &hagency_store::management::HostContext) -> Result<(), NativeError> {
        session.authorize(context, self.permission()).map_err(|_| failure(403, "management_permission_required"))?;
        if matches!(self, Self::AgentTopUpRequest { approve_immediately: true, .. }) {
            session.authorize(context, Permission::DecideAgentTopUps).map_err(|_| failure(403, "management_permission_required"))?;
        }
        Ok(())
    }
    fn permission(&self) -> Permission {
        match self {
            Self::SharedApprovals{..}|Self::SharedApprovalGet{..}=>Permission::ReadAgentWork,
            Self::SharedApprovalDecide{..}=>Permission::DecideAgentRequests,
            Self::Session {} | Self::Connections { .. } | Self::Connection { .. } | Self::ConnectionHistory { .. } => {
                Permission::ReadConnections
            }
            Self::Recover {
                original_operation, ..
            } => match original_operation {
                hagency_store::management::IntentOperation::Reoffer => Permission::ContributeResources,
                hagency_store::management::IntentOperation::ProjectRequest => {
                    Permission::RequestProjects
                }
                hagency_store::management::IntentOperation::ProjectDecide => {
                    Permission::DecideProjects
                }
                hagency_store::management::IntentOperation::ProjectRevoke => {
                    Permission::RevokeProjects
                }
                hagency_store::management::IntentOperation::PoolAgentRequest => Permission::RequestAgents,
                hagency_store::management::IntentOperation::PoolAgentDecide => Permission::DecideAgentRequests,
                hagency_store::management::IntentOperation::PoolAgentReallocate => Permission::DecideAgentRequests,
                hagency_store::management::IntentOperation::PoolAgentRenew => Permission::DecideAgentRequests,
                hagency_store::management::IntentOperation::PoolAgentStop => Permission::CancelAgentRequests,
                hagency_store::management::IntentOperation::AgentRequest => {
                    Permission::RequestAgents
                }
                hagency_store::management::IntentOperation::AgentDecide => {
                    Permission::DecideAgentRequests
                }
                hagency_store::management::IntentOperation::AgentCancel => {
                    Permission::CancelAgentRequests
                }
                hagency_store::management::IntentOperation::AgentRevoke => Permission::RevokeAgentRequests,
                hagency_store::management::IntentOperation::AgentSettlementRequest => Permission::RequestAgentSettlement,
                hagency_store::management::IntentOperation::AgentTopUpRequest => Permission::RequestAgentTopUps,
                hagency_store::management::IntentOperation::AgentTopUpDecide => Permission::DecideAgentTopUps,
                hagency_store::management::IntentOperation::AgentTopUpCancel => Permission::CancelAgentTopUps,
                hagency_store::management::IntentOperation::AgentRunControl => Permission::ChangeAgentRunControl,
                hagency_store::management::IntentOperation::AllocationDecide => Permission::DecideAllocations,
                hagency_store::management::IntentOperation::AllocationRelease => Permission::ReleaseAllocations,
                _ => Permission::ReadConnections,
            },
            Self::Pools { .. } => Permission::ReadCatalog,
            Self::PoolAgent { .. } | Self::PoolAgents { .. } => Permission::ReadAgentRequests,
            Self::PoolAgentRequest { .. } => Permission::RequestAgents,
            Self::PoolAgentDecide { .. } => Permission::DecideAgentRequests,
            Self::PoolAgentReallocate { .. } => Permission::DecideAgentRequests,
            Self::PoolAgentRenew { .. } => Permission::DecideAgentRequests,
            Self::PoolAgentStop { .. } => Permission::CancelAgentRequests,
            Self::AgentBudget { .. } | Self::AgentTopUp { .. } | Self::AgentTopUps { .. } => Permission::ReadAgentBudget,
            Self::AgentTopUpRequest { .. } => Permission::RequestAgentTopUps,
            Self::AgentTopUpDecide { .. } => Permission::DecideAgentTopUps,
            Self::AgentTopUpCancel { .. } => Permission::CancelAgentTopUps,
            Self::AgentRunReadiness { .. } => Permission::ReadAgentRunControl,
            Self::AgentRunControl { .. } => Permission::ChangeAgentRunControl,
            Self::Allocations { .. } | Self::Allocation { .. } => Permission::ReadAllocations,
            Self::AllocationDecide { .. } => Permission::DecideAllocations,
            Self::AllocationRelease { .. } => Permission::ReleaseAllocations,
            Self::AgentReadiness { .. } | Self::AgentRequests { .. } | Self::Agent { .. } => {
                Permission::ReadAgentRequests
            }
            Self::AgentAssignment { .. } | Self::AgentMembership { .. } | Self::AgentRuntime { .. } | Self::AgentExecution { .. } => Permission::ReadAgentStatus,
            Self::AgentWork { .. } | Self::AgentWorkList { .. } => Permission::ReadAgentWork,
            Self::AgentRequest { .. } => Permission::RequestAgents,
            Self::AgentDecide { .. } => Permission::DecideAgentRequests,
            Self::AgentCancel { .. } => Permission::CancelAgentRequests,
            Self::AgentRevoke { .. } => Permission::RevokeAgentRequests,
            Self::AgentSettlement { .. } => Permission::ReadAgentSettlement,
            Self::AgentSettlementRequest { .. } => Permission::RequestAgentSettlement,
            Self::AgentRemoval { .. } | Self::AgentRemovals { .. } => Permission::ReadAgentCleanup,
            Self::Catalog { .. } => Permission::ReadCatalog,
            Self::Projects { .. } | Self::Project { .. } => Permission::ReadProjects,
            Self::ProjectRequest { .. } => Permission::RequestProjects,
            Self::ProjectDecide { .. } => Permission::DecideProjects,
            Self::ProjectRevoke { .. } => Permission::RevokeProjects,
            Self::Inbox { .. } | Self::Request { .. } => Permission::ReadInbox,
            Self::Contribute { .. } | Self::Reoffer { .. } => Permission::ContributeResources,
            Self::Decide { .. } => Permission::DecideInbox,
            Self::Revoke { .. } => Permission::RevokeConnections,
            Self::Verify { .. } => Permission::VerifyConnections,
            Self::Resume { .. } => Permission::ResumeConnections,
        }
    }
    pub(crate) fn mutation(&self) -> bool {
        matches!(
            self,
            Self::SharedApprovalDecide {..} | Self::Contribute { .. }
                | Self::Reoffer { .. }
                | Self::Decide { .. }
                | Self::Revoke { .. }
                | Self::Verify { .. }
                | Self::Resume { .. }
                | Self::ProjectRequest { .. }
                | Self::ProjectDecide { .. }
                | Self::ProjectRevoke { .. }
                | Self::PoolAgentRequest { .. }
                | Self::PoolAgentDecide { .. }
                | Self::PoolAgentReallocate { .. }
                | Self::PoolAgentRenew { .. }
                | Self::PoolAgentStop { .. }
                | Self::AgentRequest { .. }
                | Self::AgentDecide { .. }
                | Self::AgentCancel { .. }
                | Self::AgentRevoke { .. }
                | Self::AgentSettlementRequest { .. }
                | Self::AgentTopUpRequest { .. }
                | Self::AgentTopUpDecide { .. }
                | Self::AgentTopUpCancel { .. }
                | Self::AgentRunControl { .. }
                | Self::AllocationDecide { .. }
                | Self::AllocationRelease { .. }
        )
    }
}
/// Created only after package authorization. Only this module can construct it.
pub(in crate::console) struct ServerOperation {
    method: reqwest::Method,
    suffix: String,
    input: Option<Value>,
}
impl ServerOperation {
    pub(in crate::console) fn request(
        self,
    ) -> Result<(reqwest::Method, String, Option<Value>), OwnerError> {
        Ok((
            self.method,
            format!("/hagency/api/v1/management/{}", self.suffix),
            self.input,
        ))
    }
    fn prepare(
        command: &ManagementCommand,
        resource: Option<&ResourceSummary>,
    ) -> Result<Self, NativeError> {
        use reqwest::Method;
        let key = |value: &str| super::key(value);
        let (method, suffix, input) = match command {
            ManagementCommand::SharedApprovals{room_id}=>{
                if room_id.as_ref().is_some_and(|s|s.len()>255||!s.starts_with('!')) {return Err(failure(400,"invalid_room"));}
                (Method::POST,"shared-execution/approvals/query".into(),Some(json!({"roomId":room_id})))
            },
            ManagementCommand::SharedApprovalGet{id}=>{key(id)?;(Method::POST,"shared-execution/approvals/get".into(),Some(json!({"id":id})))},
            ManagementCommand::SharedApprovalDecide{id,digest,decision}=>{
                key(id)?;if digest.len()!=64 || !digest.bytes().all(|b|b.is_ascii_hexdigit()) || !matches!(decision.as_str(),"approve_once"|"approve_session"|"deny"){return Err(failure(400,"invalid_permission_decision"));}
                (Method::POST,"shared-execution/approvals/decide".into(),Some(json!({"id":id,"digest":digest,"decision":decision})))
            },
            ManagementCommand::Pools { .. } | ManagementCommand::PoolAgents { .. } | ManagementCommand::PoolAgent { .. }
            | ManagementCommand::PoolAgentRequest { .. } | ManagementCommand::PoolAgentDecide { .. } | ManagementCommand::PoolAgentStop { .. } | ManagementCommand::PoolAgentRenew { .. } | ManagementCommand::PoolAgentReallocate { .. } => return pools::prepare(command),
            ManagementCommand::AgentSettlement { .. } | ManagementCommand::AgentSettlementRequest { .. } => return agent_settlement::prepare(command),
            ManagementCommand::AgentBudget { .. } | ManagementCommand::AgentTopUp { .. } | ManagementCommand::AgentTopUps { .. }
            | ManagementCommand::AgentTopUpRequest { .. } | ManagementCommand::AgentTopUpDecide { .. } | ManagementCommand::AgentTopUpCancel { .. } => return agent_budget::prepare(command),
            ManagementCommand::Allocations { .. } | ManagementCommand::Allocation { .. }
            | ManagementCommand::AllocationDecide { .. } | ManagementCommand::AllocationRelease { .. } => return allocations::prepare(command),
            ManagementCommand::AgentRunReadiness { .. } | ManagementCommand::AgentRunControl { .. } => return Err(failure(400, "native_run_control_path_required")),
            ManagementCommand::AgentReadiness { .. }
            | ManagementCommand::AgentRequests { .. }
            | ManagementCommand::Agent { .. }
            | ManagementCommand::AgentAssignment { .. }
            | ManagementCommand::AgentMembership { .. }
            | ManagementCommand::AgentRuntime { .. }
            | ManagementCommand::AgentExecution { .. }
            | ManagementCommand::AgentWork { .. }
            | ManagementCommand::AgentWorkList { .. }
            | ManagementCommand::AgentRequest { .. }
            | ManagementCommand::AgentDecide { .. }
            | ManagementCommand::AgentCancel { .. }
            | ManagementCommand::AgentRevoke { .. }
            | ManagementCommand::AgentRemoval { .. }
            | ManagementCommand::AgentRemovals { .. } => return agents::prepare(command),
            ManagementCommand::Session {} => (Method::GET, "session".into(), None),
            ManagementCommand::Recover {
                command_id,
                original_operation,
            } => {
                use hagency_store::management::IntentOperation as Op;
                key(command_id)?;
                let operation = match original_operation {
                    Op::Contribute => "contribution.create",
                    Op::Reoffer => "contribution.reoffer",
                    Op::Decide => "request.decide",
                    Op::Revoke => "contribution.revoke",
                    Op::Verify => "contribution.verify",
                    Op::Resume => "contribution.resume",
                    Op::ProjectRequest => "project.request",
                    Op::ProjectDecide => "project.decide",
                    Op::ProjectRevoke => "project.revoke",
                    Op::PoolAgentRequest => "pool_agent.request",
                    Op::PoolAgentDecide => "pool_agent.decide",
                    Op::PoolAgentReallocate => "pool_agent.reallocate",
                    Op::PoolAgentRenew => "pool_agent.renew",
                    Op::PoolAgentStop => "pool_agent.stop",
                    Op::AgentRequest => "agent.request",
                    Op::AgentDecide => "agent.decide",
                    Op::AgentCancel => "agent.cancel",
                    Op::AgentRevoke => "agent.revoke",
                    Op::AgentTopUpRequest => "agent.topup.request",
                    Op::AgentTopUpDecide => "agent.topup.decide",
                    Op::AgentTopUpCancel => "agent.topup.cancel",
                    Op::AllocationDecide => "allocation.decide",
                    Op::AllocationRelease => "allocation.release",
                    _ => return Err(failure(400, "invalid_management_operation")),
                };
                (
                    Method::POST,
                    if matches!(
                        original_operation,
                        Op::ProjectRequest | Op::ProjectDecide | Op::ProjectRevoke
                    ) {
                        format!("project-commands/{command_id}/recover")
                    } else if pools::is_operation(*original_operation) {
                        format!("pool-agent-commands/{command_id}/recover")
                    } else if agent_budget::is_operation(*original_operation) {
                        format!("agent-topup-commands/{command_id}/recover")
                    } else if allocations::is_operation(*original_operation) {
                        format!("allocation-commands/{command_id}/recover")
                    } else if agents::is_operation(*original_operation) {
                        format!("agent-commands/{command_id}/recover")
                    } else {
                        format!("commands/{command_id}/recover")
                    },
                    Some(json!({"operation":operation})),
                )
            }
            ManagementCommand::Connections { after } => {
                if let Some(after) = after {
                    key(after)?;
                }
                (
                    Method::POST,
                    "contributions/list".into(),
                    Some(json!({"after":after})),
                )
            }
            ManagementCommand::Connection { id } => {
                key(id)?;
                (Method::GET, format!("contributions/{id}"), None)
            }
            ManagementCommand::ConnectionHistory { id, after } => {
                key(id)?;
                if let Some(after)=after {key(after)?;}
                (Method::POST,format!("contributions/{id}/history"),Some(json!({"after":after})))
            }
            ManagementCommand::Inbox {
                after,
                needs_my_action,
                include_projects,
                include_allocations,
            } => {
                if let Some(after) = after {
                    key(after)?;
                }
                (
                    Method::POST,
                    "inbox".into(),
                    Some({
                        let mut input=json!({"after":after,"needsMyAction":needs_my_action});
                        if *include_projects {input["includeProjects"]=json!(true);}
                        if *include_allocations {input["includeAllocations"]=json!(true);}
                        input
                    }),
                )
            }
            ManagementCommand::Request { id } => {
                key(id)?;
                (Method::GET, format!("requests/{id}"), None)
            }
            ManagementCommand::Catalog { after, capability } => {
                if let Some(after) = after {
                    key(after)?;
                }
                if capability.as_deref().is_some_and(|value| {
                    value.is_empty()
                        || value.len() > 80
                        || !value.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-/".contains(&b)
                        })
                }) {
                    return Err(failure(400, "invalid_management_capability"));
                }
                (
                    Method::POST,
                    "catalog".into(),
                    Some(json!({"after":after,"capability":capability})),
                )
            }
            ManagementCommand::Projects {
                after,
                needs_my_action,
            } => {
                if let Some(after) = after {
                    key(after)?;
                }
                (
                    Method::POST,
                    "projects/list".into(),
                    Some(json!({"after":after,"needsMyAction":needs_my_action})),
                )
            }
            ManagementCommand::Project { id } => {
                key(id)?;
                (Method::GET, format!("projects/{id}"), None)
            }
            ManagementCommand::ProjectRequest {
                command_id,
                contribution_id,
                name,
                purpose,
                requested_tokens,
            } => {
                key(command_id)?;
                key(contribution_id)?;
                if name.trim().is_empty()
                    || name.len() > 160
                    || name.chars().any(char::is_control)
                    || purpose.trim().is_empty()
                    || purpose.len() > 2000
                    || purpose.chars().any(|ch| ch.is_control() && ch != '\n')
                    || !(1..=9_007_199_254_740_991).contains(requested_tokens)
                {
                    return Err(failure(400, "invalid_management_arguments"));
                }
                (
                    Method::POST,
                    "projects".into(),
                    Some(
                        json!({"commandId":command_id,"contributionId":contribution_id,"name":name,"purpose":purpose,"requestedTokens":requested_tokens}),
                    ),
                )
            }
            ManagementCommand::ProjectDecide {
                command_id,
                request_id,
                expected_revision,
                approve,
            } => {
                key(command_id)?;
                key(request_id)?;
                if *expected_revision <= 0 {
                    return Err(failure(400, "invalid_management_revision"));
                }
                (
                    Method::POST,
                    format!("project-requests/{request_id}/decide"),
                    Some(
                        json!({"commandId":command_id,"expectedRevision":expected_revision,"decision":if *approve { "approve" } else { "reject" }}),
                    ),
                )
            }
            ManagementCommand::ProjectRevoke {
                command_id,
                project_id,
                expected_revision,
            } => {
                key(command_id)?;
                key(project_id)?;
                if *expected_revision <= 0 {
                    return Err(failure(400, "invalid_management_revision"));
                }
                (
                    Method::POST,
                    format!("projects/{project_id}/revoke"),
                    Some(json!({"commandId":command_id,"expectedRevision":expected_revision})),
                )
            }
            ManagementCommand::Contribute {
                publication_first,
                command_id,
                resource_id,
                expected_revision,
                label,
                coordinator_mxid,
            } => {
                key(command_id)?;
                key(resource_id)?;
                let resource =
                    resource.ok_or_else(|| failure(404, "management_resource_not_found"))?;
                if resource.id != *resource_id || resource.revision != *expected_revision {
                    return Err(failure(409, "management_revision_conflict"));
                }
                // The draft is not actionable until the owner confirms a native
                // capacity receipt. Keep legacy wire bytes and replay IDs intact.
                let mut body = json!({"commandId":command_id,"resourceId":resource.id,"resourceRevision":resource.revision,"label":label,"runtime":resource.framework,"capabilities":resource.capabilities,"coordinatorMxid":coordinator_mxid});
                if *publication_first { body["publicationFirst"] = json!(true); }
                (Method::POST, "contributions".into(), Some(body))
            }
            ManagementCommand::Reoffer {
                command_id, contribution_id, expected_contribution_revision,
                resource_id, expected_revision, label, coordinator_mxid,
            } => {
                key(command_id)?;
                key(contribution_id)?;
                key(resource_id)?;
                if !(1..9_007_199_254_740_991).contains(expected_contribution_revision) {
                    return Err(failure(400,"invalid_management_revision"));
                }
                let resource=resource.ok_or_else(||failure(404,"management_resource_not_found"))?;
                if resource.id!=*resource_id || resource.revision!=*expected_revision {
                    return Err(failure(409,"management_revision_conflict"));
                }
                // Capabilities and framework are taken from the canonical
                // native definition; the script cannot assert native capacity.
                (Method::POST,format!("contributions/{contribution_id}/reoffer"),Some(json!({
                    "expectedRevision":expected_contribution_revision,
                    "offer":{"commandId":command_id,"resourceId":resource.id,"resourceRevision":resource.revision,
                        "label":label,"runtime":resource.framework,"capabilities":resource.capabilities,"coordinatorMxid":coordinator_mxid}
                })))
            }
            ManagementCommand::Decide {
                command_id,
                request_id,
                expected_revision,
                approve,
            } => {
                key(command_id)?;
                key(request_id)?;
                if *expected_revision <= 0 {
                    return Err(failure(400, "invalid_management_revision"));
                }
                (
                    Method::POST,
                    format!("requests/{request_id}/decide"),
                    Some(
                        json!({"commandId":command_id,"expectedRevision":expected_revision,"decision":if *approve{"approve"}else{"reject"}}),
                    ),
                )
            }
            ManagementCommand::Revoke {
                command_id,
                contribution_id,
                expected_revision,
            } => {
                key(command_id)?;
                key(contribution_id)?;
                if *expected_revision <= 0 {
                    return Err(failure(400, "invalid_management_revision"));
                }
                (
                    Method::POST,
                    format!("contributions/{contribution_id}/revoke"),
                    Some(json!({"commandId":command_id,"expectedRevision":expected_revision})),
                )
            }
            ManagementCommand::Verify {
                command_id,
                contribution_id,
                expected_revision,
                resource_id,
                resource_revision,
            }
            | ManagementCommand::Resume {
                command_id,
                contribution_id,
                expected_revision,
                resource_id,
                resource_revision,
            } => {
                key(command_id)?;
                key(contribution_id)?;
                key(resource_id)?;
                let resource =
                    resource.ok_or_else(|| failure(404, "management_resource_not_found"))?;
                if *expected_revision <= 0
                    || resource.id != *resource_id
                    || resource.revision != *resource_revision
                {
                    return Err(failure(409, "management_revision_conflict"));
                }
                (
                    Method::POST,
                    format!(
                        "contributions/{contribution_id}/{}",
                        if matches!(command, ManagementCommand::Resume { .. }) {
                            "resume"
                        } else {
                            "verify"
                        }
                    ),
                    Some(
                        json!({"commandId":command_id,"expectedRevision":expected_revision,
                        "resourceId":resource_id,"resourceRevision":resource_revision}),
                    ),
                )
            }
        };
        if serde_json::to_vec(&input)
            .map_err(|_| failure(400, "invalid_management_arguments"))?
            .len()
            > 16 * 1024
        {
            return Err(failure(400, "management_payload_too_large"));
        }
        Ok(Self {
            method,
            suffix,
            input,
        })
    }
}
impl NativeManagement {
    pub async fn central(&self, command: ManagementCommand) -> Result<Value, NativeError> {
        if !cfg!(feature="shared-intake") && matches!(&command,ManagementCommand::AgentRunReadiness{..}|ManagementCommand::AgentRunControl{..}) {return Err(failure(409,"shared_intake_disabled"))}

        if let Some(intent) = command.intent()? {
            let (context, session) = self.authorized().await?;
            command.authorize(&session, &context)?;
            self.with_intent(intent, self.central_now(command)).await
        } else {
            self.central_now(command).await
        }
    }
    pub(super) async fn central_now(
        &self,
        command: ManagementCommand,
    ) -> Result<Value, NativeError> {
        if matches!(&command, ManagementCommand::AgentRunReadiness { .. } | ManagementCommand::AgentRunControl { .. }) {
            return agent_run_control::execute(self, command).await;
        }
        self.central_now_with_connection_observer(command, |context, session, resource, resume| async move {
            self.verify_local_resource(&context, &session, &resource, resume).await
        }).await
    }
    async fn central_now_with_connection_observer<F, Fut>(
        &self,
        command: ManagementCommand,
        observe: F,
    ) -> Result<Value, NativeError>
    where
        F: FnOnce(hagency_store::management::HostContext, hagency_store::management::Session, ResourceSummary, bool) -> Fut,
        Fut: std::future::Future<Output = Result<ResourceSummary, NativeError>>,
    {
        let (context, session) = self.authorized().await?;
        command.authorize(&session, &context)?;
        // Opting into project inbox records requires a separate read grant;
        // existing resource inbox packages retain their original scope.
        if matches!(
            &command,
            ManagementCommand::Inbox {
                include_projects: true,
                ..
            }
        ) {
            session
                .authorize(&context, Permission::ReadProjects)
                .map_err(|_| failure(403, "management_permission_required"))?;
        }
        if matches!(&command, ManagementCommand::Inbox { include_allocations: true, .. }) {
            session.authorize(&context, Permission::ReadAllocations)
                .map_err(|_| failure(403, "management_permission_required"))?;
        }
        let resource = if let ManagementCommand::Contribute { resource_id, .. }
        | ManagementCommand::Reoffer { resource_id, .. }
        | ManagementCommand::Verify { resource_id, .. }
        | ManagementCommand::Resume { resource_id, .. } = &command
        {
            Some(
                session
                    .resource(&context, resource_id.clone())
                    .await
                    .map_err(|_| failure(409, "management_resource_unavailable"))?,
            )
        } else {
            None
        };
        // The script supplies only stable object/revision identifiers. Native
        // code observes the provider and validates its original local binding.
        let resource = if matches!(
            command,
            ManagementCommand::Verify { .. } | ManagementCommand::Resume { .. }
        ) {
            Some(
                observe(
                    context.clone(),
                    session.clone(),
                    resource
                        .ok_or_else(|| failure(404, "management_resource_not_found"))?,
                    matches!(command, ManagementCommand::Resume { .. }),
                )
                .await?,
            )
        } else {
            resource
        };
        let operation = ServerOperation::prepare(&command, resource.as_ref())?;
        if let ManagementCommand::Reoffer { contribution_id, .. } = &command {
            // with_intent already holds the account mutation lane. Reconcile
            // local originals before exposure recording or any mutation HTTP.
            Box::pin(super::owner_grant::reoffer::check(self, contribution_id)).await?;
        }

        let console = &self.owner.0.host.console;
        let device = console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let pin = DeviceIdentity::from(&device);
        if matches!(
            &command,
            ManagementCommand::Contribute { .. } | ManagementCommand::Reoffer { .. }
        ) {
            // Nothing of the offer has been sent or recorded yet, so a failed
            // declaration is reported as itself.
            self.declare_ledger_before_offer(&device).await?;
        }
        // Provider observation can outlive the original finite session. Refresh
        // only the local capability, retaining the exact owner/package/device
        // context and the command's original permission before sending.
        let (before_send, current_session) = self.authorized().await?;
        if before_send != context {
            return Err(failure(409, "management_scope_changed"));
        }
        command.authorize(&current_session, &context)?;
        if let ManagementCommand::Contribute { command_id, resource_id, expected_revision, .. }
        | ManagementCommand::Reoffer { command_id, resource_id, expected_revision, .. } = &command {
            // Commit the irreversible installation-wide fence before HTTP.
            // Rejection, lost responses and another profile cannot make an
            // exposed definition locally editable again.
            current_session.record_contribution_attempt(
                &context, resource_id.clone(), expected_revision.clone(), command_id.clone(),
            ).await.map_err(super::management::store_error)?;
            let (before_send, current_session) = self.authorized().await?;
            if before_send != context {
                return Err(failure(409, "management_scope_changed"));
            }
            command.authorize(&current_session, &context)?;
        }
        let reply = console
            .owner_api_pinned(&pin, OwnerOperation::Management(operation))
            .await;
        // A successful write followed by scope loss has an unknown UI outcome;
        // its stable command receipt is recovered after fresh authentication.
        let (current, current_session) = self.authorized().await.map_err(|error| {
            if command.mutation() && reply.is_ok() {
                failure(409, "management_outcome_unknown")
            } else {
                error
            }
        })?;
        if current != context {
            return Err(failure(
                409,
                if command.mutation() {
                    "management_outcome_unknown"
                } else {
                    "management_scope_changed"
                },
            ));
        }
        command.authorize(&current_session, &current)?;
        let reply = reply.map_err(|error| {
            // The script bridge exposes bounded codes, so distinguish the one
            // documented disabled-policy response before discarding HTTP status.
            if matches!(command, ManagementCommand::AgentWork { .. } | ManagementCommand::AgentWorkList { .. })
                && error.code == "shared_execution_unavailable" && error.status != 503 {
                failure(error.status, "management_agent_work_invalid")
            } else {
                failure(error.status, &error.code)
            }
        })?;
        if reply.owner != device.owner_mxid()
            || reply.origin != device.origin()
            || reply.subject != device.subject()
            || reply.issuer != device.issuer()
        {
            return Err(failure(401, "management_scope_changed"));
        }
        if matches!(
            &command,
            ManagementCommand::Inbox {
                include_projects: true,
                ..
            }
        ) || matches!(&command, ManagementCommand::Request { .. })
            && reply.value["request"]["kind"] == "project_capacity"
        {
            current_session
                .authorize(&current, Permission::ReadProjects)
                .map_err(|_| failure(403, "management_permission_required"))?;
        }
        if matches!(&command, ManagementCommand::Inbox { include_allocations: true, .. })
            || matches!(&command, ManagementCommand::Request { .. })
                && reply.value["request"]["kind"] == "project_allocation" {
            current_session.authorize(&current, Permission::ReadAllocations)
                .map_err(|_| failure(403, "management_permission_required"))?;
        }
        let value=pools::project_reply(&command, reply.value)?;
        let value=agents::project_reply(&command, value)?;
        let value = allocations::project_reply(&command, value)?;
        let value = agent_budget::project_reply(&command, value)?;
        agent_settlement::project_reply(&command, value)
    }
    /// Isolated tests replace only the external provider observation. Canonical
    /// storage, native authority, HTTP and original command journals stay real.
    #[cfg(all(test, feature = "resource-adoption"))]
    pub(super) async fn central_with_connection_fixture(
        &self,
        command: ManagementCommand,
        provider: hagency_store::owner_resources::VerifiedProviderAccount,
    ) -> Result<Value, NativeError> {
        let intent = command.intent()?.ok_or_else(|| failure(400, "invalid_management_operation"))?;
        let (context, session) = self.authorized().await?;
        command.authorize(&session, &context)?;
        self.with_intent(intent, self.central_now_with_connection_observer(
            command, |context, session, resource, resume| async move {
                // Deterministic finite-session rollover while the external
                // observer is pending. The native canonical observer obtains
                // a fresh same-context grant; the outer write must do so too.
                session.retire();
                let observed=self.verify_canonical_connection(&context, &resource, resume, provider).await;
                assert!(!session.is_live(), "refresh must never revive captured authority");
                observed
            },
        )).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projects_use_separate_routes_grants_and_receipts_without_capacity_claims() {
        use hagency_store::management::IntentOperation as Op;
        for (op, name, permission) in [
            (
                Op::ProjectRequest,
                "project.request",
                Permission::RequestProjects,
            ),
            (
                Op::ProjectDecide,
                "project.decide",
                Permission::DecideProjects,
            ),
            (
                Op::ProjectRevoke,
                "project.revoke",
                Permission::RevokeProjects,
            ),
        ] {
            let command = ManagementCommand::Recover {
                command_id: "original".into(),
                original_operation: op,
            };
            assert_eq!(command.permission(), permission);
            let (_, route, body) = ServerOperation::prepare(&command, None)
                .unwrap()
                .request()
                .unwrap();
            assert_eq!(
                route,
                "/hagency/api/v1/management/project-commands/original/recover"
            );
            assert_eq!(body.unwrap(), json!({"operation":name}));
        }
        let command = ManagementCommand::ProjectDecide {
            command_id: "decision".into(),
            request_id: "req_1".into(),
            expected_revision: 1,
            approve: true,
        };
        assert_eq!(command.permission(), Permission::DecideProjects);
        let (_, route, body) = ServerOperation::prepare(&command, None)
            .unwrap()
            .request()
            .unwrap();
        assert_eq!(
            route,
            "/hagency/api/v1/management/project-requests/req_1/decide"
        );
        assert_eq!(
            body.unwrap(),
            json!({"commandId":"decision","expectedRevision":1,"decision":"approve"})
        );
        for (tokens, valid) in [
            (0, false),
            (1, true),
            (9_007_199_254_740_991, true),
            (9_007_199_254_740_992, false),
        ] {
            let command = ManagementCommand::ProjectRequest {
                command_id: "request".into(),
                contribution_id: "con_1".into(),
                name: "Project".into(),
                purpose: "Build documentation".into(),
                requested_tokens: tokens,
            };
            assert_eq!(ServerOperation::prepare(&command, None).is_ok(), valid);
        }
        for id in ["../contributions", "project?owner=other", "a/b"] {
            assert!(
                ServerOperation::prepare(&ManagementCommand::Project { id: id.into() }, None)
                    .is_err()
            );
        }
        for command in [
            json!({"operation":"project_request","command_id":"a","contribution_id":"c","name":"n","purpose":"p","requested_tokens":1,"manager_mxid":"@other:test"}),
            json!({"operation":"project_decide","command_id":"a","request_id":"r","expected_revision":1,"approve":true,"allocated_tokens":1}),
            json!({"operation":"catalog","after":null,"capability":null,"url":"https://other.test"}),
        ] {
            assert!(serde_json::from_value::<ManagementCommand>(command).is_err());
        }
        let (_, _, legacy_inbox) = ServerOperation::prepare(
            &ManagementCommand::Inbox {
                after: None,
                needs_my_action: true,
                include_projects: false,
                include_allocations: false,
            },
            None,
        )
        .unwrap()
        .request()
        .unwrap();
        assert!(
            legacy_inbox.unwrap().get("includeProjects").is_none(),
            "old server input remains compatible"
        );
    }

    #[test]
    fn management_routes_cannot_be_supplied_by_script_or_cross_object_path() {
        for id in ["../secrets", "x?token=secret", "https://other.test", "a/b"] {
            assert!(
                ServerOperation::prepare(&ManagementCommand::Connection { id: id.into() }, None)
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<ManagementCommand>(
                json!({"operation":"session","url":"https://other.test","token":"secret"})
            )
            .is_err()
        );
        let r = ServerOperation::prepare(
            &ManagementCommand::Decide {
                command_id: "one".into(),
                request_id: "req_1".into(),
                expected_revision: 2,
                approve: false,
            },
            None,
        )
        .unwrap();
        let (method, path, body) = r.request().unwrap();
        assert_eq!(method, reqwest::Method::POST);
        assert_eq!(path, "/hagency/api/v1/management/requests/req_1/decide");
        assert_eq!(
            body.unwrap(),
            json!({"commandId":"one","expectedRevision":2,"decision":"reject"})
        );
        assert!(
            ServerOperation::prepare(
                &ManagementCommand::Contribute {
                    publication_first: false,
                    command_id: "one".into(),
                    resource_id: "r1".into(),
                    expected_revision: "a".repeat(64),
                    label: "Coding".into(),
                    coordinator_mxid: "@c:example.test".into()
                },
                None
            )
            .is_err()
        );
    }
}

#[cfg(test)]
#[path = "management_server/resource_offer_tests.rs"]
mod resource_offer_tests;
