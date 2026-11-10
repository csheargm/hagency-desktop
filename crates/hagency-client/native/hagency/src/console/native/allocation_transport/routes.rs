use super::*;
use crate::console::server_login::OwnerError;

pub(in crate::console) struct ServerOperation(Box<Operation>);
pub(in crate::console::native) enum Operation {
    Session,
    /// The owner database this device now reports from. `fresh`: it was
    /// created empty, so what the server holds for the device is not its own.
    DeclareLedger { id: String, fresh: bool },
    #[cfg(feature="agent-budget")]
    AgentBudget(super::super::managed_agent_budget_transport::ServerOperation),
    AgentSettlement(super::super::managed_agent_settlement_transport::ServerOperation),
    SharedExecution(super::super::shared_execution::ServerOperation),
    ManagedAgent(super::super::managed_agent_transport::ServerOperation),
    ManagedMatrix(super::super::managed_agent_matrix_transport::ServerOperation),
    ManagedRuntime(super::super::managed_runtime_transport::ServerOperation),
    Publish(Publication),
    RecoverPublication(String),
    PublishSuccessor(SuccessorPublication),
    RecoverSuccession(String),
    ExpirePublication { command_id: String, operation: String, target_id: String, publication: Value },
    Grant(String),
    Contribution(String),
    Poll {
        parent: bool,
        after: Option<String>,
    },
    Recover {
        parent: bool,
        id: String,
        digest: String,
    },
    AllocationResult {
        id: String,
        digest: String,
        token: String,
        receipt: NativeAllocationReceipt,
    },
    ParentResult {
        id: String,
        digest: String,
        token: String,
        receipt: NativeGrantSettlementReceipt,
    },
}
impl ServerOperation {
    pub(super) fn new(operation: Operation) -> Result<Self, NativeError> {
        match &operation {
            Operation::Session => {},
            Operation::DeclareLedger { id, .. } => hash(id)?,
            #[cfg(feature="agent-budget")]
            Operation::AgentBudget(_) => {},
            Operation::AgentSettlement(_)
            | Operation::SharedExecution(_)
            | Operation::ManagedAgent(_)
            | Operation::ManagedMatrix(_)
            | Operation::ManagedRuntime(_) => {}
            Operation::ExpirePublication { command_id, operation, target_id, publication } => {
                key(command_id)?; key(target_id)?;
                if !matches!(operation.as_str(),"grant.publish"|"grant.succeed") || publication["commandId"].as_str()!=Some(command_id) {return Err(super::invalid());}
            }
            Operation::Publish(p) => {
                key(&p.command_id)?;
                key(&p.receipt.contribution_id)?;
                positive(p.expected_contribution_revision)?;
            }
            Operation::PublishSuccessor(p) => {
                key(&p.command_id)?;
                key(&p.receipt.contribution_id)?;
                key(
                    &p.receipt
                        .succession
                        .as_ref()
                        .ok_or_else(super::invalid)?
                        .predecessor_grant_id,
                )?;
                positive(p.expected_contribution_revision)?;
                positive(p.expected_predecessor_revision)?;
            }
            Operation::RecoverPublication(id)
            | Operation::RecoverSuccession(id)
            | Operation::Grant(id)
            | Operation::Contribution(id) => key(id)?,
            Operation::Poll { after, .. } => {
                if let Some(id) = after {
                    key(id)?;
                }
            }
            Operation::Recover { id, digest, .. }
            | Operation::AllocationResult { id, digest, .. }
            | Operation::ParentResult { id, digest, .. } => {
                key(id)?;
                hash(digest)?;
            }
        }
        Ok(Self(Box::new(operation)))
    }
    pub(in crate::console) fn request(
        self,
    ) -> Result<(reqwest::Method, String, Option<Value>), OwnerError> {
        #[cfg(feature="agent-budget")]
        if let Operation::AgentBudget(operation) = *self.0 { return operation.request(); }
        if let Operation::AgentSettlement(operation) = *self.0 { return operation.request(); }
        if let Operation::SharedExecution(operation) = *self.0 {
            return operation.request();
        }
        if let Operation::ManagedAgent(operation) = *self.0 {
            return operation.request();
        }
        if let Operation::ManagedMatrix(operation) = *self.0 {
            return operation.request();
        }
        if let Operation::ManagedRuntime(operation) = *self.0 {
            return operation.request();
        }
        let prefix = |parent| {
            if parent {
                "grant-executor-commands"
            } else {
                "executor-commands"
            }
        };
        let (method, suffix, body) = match *self.0 {
            Operation::Session => (reqwest::Method::GET, "session".into(), None),
            // `fresh` is sent only when true: a server that knows
            // declarations but not this field refuses an unknown one.
            Operation::DeclareLedger { id, fresh } => (
                reqwest::Method::POST,
                "executor-ledger/declare".into(),
                Some(if fresh { json!({"ledgerId":id,"fresh":true}) } else { json!({"ledgerId":id}) }),
            ),
            #[cfg(feature="agent-budget")]
            Operation::AgentBudget(_) => unreachable!("budget handled above"),
            Operation::AgentSettlement(_) => unreachable!("settlement handled above"),
            Operation::SharedExecution(_) => unreachable!("shared execution handled above"),
            Operation::ManagedAgent(_) => unreachable!("managed agent handled above"),
            Operation::ManagedMatrix(_) => unreachable!("managed Matrix handled above"),
            Operation::ManagedRuntime(_) => unreachable!("managed runtime handled above"),
            Operation::ExpirePublication { command_id, operation, target_id, publication } => (
                reqwest::Method::POST, format!("grant-publications/{command_id}/expire"),
                Some(json!({"operation":operation,"targetId":target_id,"publication":publication})),
            ),
            Operation::Publish(p) => (
                reqwest::Method::POST,
                format!("contributions/{}/grants", p.receipt.contribution_id),
                Some(serde_json::to_value(p).map_err(|_| OwnerError {
                    status: 400,
                    code: "invalid_allocation_payload".into(),
                })?),
            ),
            Operation::RecoverPublication(id) => (
                reqwest::Method::POST,
                format!("allocation-commands/{id}/recover"),
                Some(json!({"operation":"grant.publish"})),
            ),
            Operation::PublishSuccessor(p) => (
                reqwest::Method::POST,
                format!(
                    "grants/{}/successors",
                    p.receipt
                        .succession
                        .as_ref()
                        .map(|s| s.predecessor_grant_id.clone())
                        .unwrap_or_default()
                ),
                Some(serde_json::to_value(p).map_err(|_| OwnerError {
                    status: 400,
                    code: "invalid_allocation_payload".into(),
                })?),
            ),
            Operation::RecoverSuccession(id) => (
                reqwest::Method::POST,
                format!("allocation-commands/{id}/recover"),
                Some(json!({"operation":"grant.succeed"})),
            ),
            Operation::Grant(id) => (reqwest::Method::GET, format!("grants/{id}"), None),
            Operation::Contribution(id) => {
                (reqwest::Method::GET, format!("contributions/{id}"), None)
            }
            Operation::Poll { parent, after } => (
                reqwest::Method::POST,
                format!("{}/poll", prefix(parent)),
                Some(json!({"after":after})),
            ),
            Operation::Recover { parent, id, digest } => (
                reqwest::Method::POST,
                format!("{}/{id}/recover", prefix(parent)),
                Some(json!({"commandDigest":digest})),
            ),
            Operation::AllocationResult {
                id,
                digest,
                token,
                receipt,
            } => (
                reqwest::Method::POST,
                format!("executor-commands/{id}/result"),
                Some(json!({"commandDigest":digest,"token":token,"receipt":receipt})),
            ),
            Operation::ParentResult {
                id,
                digest,
                token,
                receipt,
            } => (
                reqwest::Method::POST,
                format!("grant-executor-commands/{id}/result"),
                Some(json!({"commandDigest":digest,"token":token,"receipt":receipt})),
            ),
        };
        if body
            .as_ref()
            .is_some_and(|v| serde_json::to_vec(v).map_or(true, |b| b.len() > 16 * 1024))
        {
            return Err(OwnerError {
                status: 400,
                code: "invalid_allocation_payload".into(),
            });
        }
        Ok((method, format!("/hagency/api/v1/management/{suffix}"), body))
    }
}
