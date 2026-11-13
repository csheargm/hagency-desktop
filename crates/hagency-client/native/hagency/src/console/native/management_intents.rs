//! Native recovery keeps command identity when the mini-app draft is lost.
use super::{ManagementCommand, NativeError, NativeManagement, failure, management::store_error};
use hagency_store::management::{Intent, IntentOperation, IntentReply, PendingIntent};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::future::Future;

fn known_failure(error: &NativeError) -> bool {
    // Authentication can be rechecked after a committed server write. Neither
    // an authorization failure nor a transport failure proves non-application.
    error.status == 400
        || error.status == 404
        || matches!(
            error.code.as_str(),
            "management_model_unqualified"
                | "provider_model_changed"
                | "provider_connection_required"
                | "provider_profile_unavailable"
                | "provider_account_verification_required"
                | "management_revision_conflict"
                | "management_revision_changed"
                | "management_resource_unavailable"
                | "management_resource_in_use"
                | "management_executor_changed"
                | "contribution_executor_changed"
                | "management_action_closed"
                | "contribution_already_exists"
                | "contribution_resource_changed"
                | "contribution_not_admitted"
                | "contribution_executor_required"
                | "contribution_executor_current"
                | "project_request_capacity"
                | "project_resource_capacity"
                | "management_receipt_capacity"
                | "management_administrator_required"
                | "project_manager_required"
        )
}
fn is_atomic_graph_operation(operation: IntentOperation) -> bool {
    #[cfg(feature = "resource-adoption")]
    { super::operator_graph_commands::is_operation(operation) }
    #[cfg(not(feature = "resource-adoption"))]
    { let _ = operation; false }
}
fn known_failure_for(operation: IntentOperation, error: &NativeError) -> bool {
    if operation == IntentOperation::WebCatalog { return super::web_catalog::known_failure(error); }
    if super::resource_definitions::is_operation(operation) { return super::resource_definitions::known_failure(error); }
    #[cfg(feature = "resource-adoption")]
    if is_atomic_graph_operation(operation) { return super::operator_graph_commands::known_failure(error); }
    if matches!(operation, IntentOperation::ReviewStopped | IntentOperation::ResolveStopped) {
        return error.status == 400 || error.status == 404 || matches!(error.code.as_str(), "management_revision_conflict" | "management_stopped_not_resolvable" | "management_stopped_review_expired");
    }
    if operation == IntentOperation::ReconcileInterruptedCall {
        // A live call, a changed call or a declaration outside the observed
        // bounds leaves nothing applied; the owner looks again and retries.
        return error.status == 400 || error.status == 404 || matches!(error.code.as_str(), "management_revision_conflict" | "management_access_required" | "management_permission_required" | "owner_local_custody_required");
    }
    if operation == IntentOperation::ClearWorkspaceDirty {
        return error.status == 400 || error.status == 404 || matches!(error.code.as_str(), "management_revision_conflict" | "management_workspace_inspection_required" | "management_workspace_active");
    }
    if operation == IntentOperation::Reoffer && matches!(error.code.as_str(),
        "owner_grant_prior_intent_required" | "management_reoffer_local_custody_required") {
        return true;
    }
    if super::operator_task_commands::is_operation(operation) { return super::operator_task_commands::known_failure(error); }
    if super::management_server::agent_budget::is_operation(operation) { return super::management_server::agent_budget::known_failure(error); }
    if operation == IntentOperation::AgentSettlementRequest {
        return super::management_server::agent_settlement::known_failure(error);
    }
    if operation == IntentOperation::AgentRunControl {
        return super::management_server::agent_run_control::known_failure(error);
    }
    if matches!(operation, IntentOperation::PrepareAccount | IntentOperation::LoginAccount) {
        return error.status == 400 && error.code == "invalid_management_arguments";
    }
    if operation == IntentOperation::RetireAccount {
        return error.status == 400 || matches!(error.code.as_str(),"account_retirement_dependencies"|"account_retirement_revision_changed");
    }
    if operation == IntentOperation::EnrollAccountResource {
        return error.status==400 || error.status==404 || matches!(error.code.as_str(),"provider_connection_required"|"provider_identity_unavailable"|"provider_account_changed"|"provider_model_changed"|"management_model_unqualified"|"management_revision_conflict"|"management_resource_in_use");
    }
    if matches!(operation, IntentOperation::TriageResourceAlert | IntentOperation::RetireApprovalBinding | IntentOperation::RevokeToolGrant | IntentOperation::EditExecutionPolicy) {
        return error.status == 400 || error.code == "management_revision_conflict" || error.status == 404;
    }
    if operation == IntentOperation::RenameResource {
        return super::resource_name::known_failure(error);
    }
    if operation == IntentOperation::EditLocalResource {
        return super::resource_edit::known_failure(error);
    }
    if operation == IntentOperation::CreateResourceConfiguration {
        return super::resource_create::known_failure(error);
    }
    if operation == IntentOperation::PersonalAgentObservation {
        super::personal_agent_observation::known_failure(error)
    } else if matches!(operation, IntentOperation::PersonalAgentModel | IntentOperation::PersonalAgentWorkspace) {
        super::personal_agent_model::known_failure(error)
    } else if operation == IntentOperation::PersonalAgentRules {
        super::personal_agent_rules::known_failure(error)
    } else if operation == IntentOperation::PersonalAgentPolicy {
        super::personal_agent_policy::known_failure(error)
    } else if super::management_server::pools::is_operation(operation) {
        super::management_server::pools::known_failure(error)
    } else if super::management_server::agents::is_operation(operation) {
        super::management_server::agents::known_failure(operation, error)
    } else if super::management_server::allocations::is_operation(operation) {
        super::management_server::allocations::known_failure(operation,error)
    } else {
        known_failure(error)
    }
}
impl NativeManagement {
    pub(super) async fn with_intent<T: Serialize + DeserializeOwned>(
        &self,
        intent: Intent,
        operation: impl Future<Output = Result<T, NativeError>>,
    ) -> Result<T, NativeError> {
        // One account-wide lane spans custody, effect and reply recording. A
        // second package instance cannot race a failed reply with a later effect.
        let _mutation = self.owner.0.management.mutations.lock().await;
        let (context, session) = self.authorized().await?;
        let saved = session
            .prepare_intent(&context, intent.clone())
            .await
            .map_err(store_error)?;
        let (current, _) = self.authorized().await?;
        if current != context {
            return Err(failure(401, "management_scope_changed"));
        }
        let first_attempt = saved.is_first_attempt();
        if let Some(mut reply) = saved.reply {
            if matches!(saved.operation, IntentOperation::TriageResourceAlert | IntentOperation::RetireApprovalBinding | IntentOperation::RevokeToolGrant | IntentOperation::EditExecutionPolicy | IntentOperation::ClearWorkspaceDirty | IntentOperation::ReviewStopped | IntentOperation::ResolveStopped | IntentOperation::LoginAccount | IntentOperation::PrepareAccount | IntentOperation::RetireAccount | IntentOperation::AgentRunControl | IntentOperation::AgentSettlementRequest) && matches!(reply, IntentReply::Success(_)) {
                // Replay the exact original request for fresh role visibility and
                // latest descriptive control; do not overwrite its saved receipt.
                return operation.await;
            }
            if matches!(
                saved.operation,
                IntentOperation::Contribute
                    | IntentOperation::Reoffer
                    | IntentOperation::Decide
                    | IntentOperation::Revoke
                    | IntentOperation::Verify
                    | IntentOperation::Resume
            ) && let IntentReply::Success(value) = &reply
            {
                let id = value["admission"]["contribution"]["id"]
                    .as_str()
                    .ok_or_else(|| failure(503, "management_saved_result_invalid"))?;
                // A package capability is not a server role. Recheck central
                // visibility before returning an old administrator's result.
                self.central_now(ManagementCommand::Connection { id: id.into() })
                    .await?;
            }
            if matches!(
                saved.operation,
                IntentOperation::PoolAgentRequest
                    | IntentOperation::PoolAgentDecide
                    | IntentOperation::PoolAgentReallocate
                    | IntentOperation::PoolAgentRenew
                    | IntentOperation::PoolAgentStop
                    | IntentOperation::ProjectRequest
                    | IntentOperation::ProjectDecide
                    | IntentOperation::ProjectRevoke
                    | IntentOperation::AgentRequest
                    | IntentOperation::AgentDecide
                    | IntentOperation::AgentCancel
                    | IntentOperation::AgentRevoke
                    | IntentOperation::AgentTopUpRequest
                    | IntentOperation::AgentTopUpDecide
                    | IntentOperation::AgentTopUpCancel
                    | IntentOperation::AllocationDecide
                    | IntentOperation::AllocationRelease
            ) && let IntentReply::Success(value) = &reply
            {
                let result_key = if super::management_server::pools::is_operation(saved.operation) { "request" } else if super::management_server::agent_budget::is_operation(saved.operation) {
                    "topUp"
                } else if super::management_server::agents::is_operation(saved.operation)
                {
                    "agentRequest"
                } else if super::management_server::allocations::is_operation(saved.operation) {
                    "allocation"
                } else {
                    "project"
                };
                if !value[result_key]["id"].is_string() {
                    return Err(failure(503, "management_saved_result_invalid"));
                }
                // The receipt route checks current role visibility under the
                // original mutation grant; retry does not acquire a general
                // project read capability or expose a stale approval snapshot.
                let current = self
                    .central_now(ManagementCommand::Recover {
                        command_id: saved.command_id.clone(),
                        original_operation: saved.operation,
                    })
                    .await?;
                if current["recorded"] != true {
                    return Err(failure(503, "management_recovery_unavailable"));
                }
                reply = IntentReply::Success(current);
            }
            // Fresh account/package/device and exact operation grants were
            // checked by prepare_intent before exposing this saved result.
            return match reply {
                IntentReply::Success(value) => serde_json::from_value(value)
                    .map_err(|_| failure(503, "management_saved_result_invalid")),
                IntentReply::Failure { status, code } => Err(failure(status, &code)),
            };
        }
        let recovered = if !first_attempt
            && matches!(
                saved.operation,
                IntentOperation::Contribute
                    | IntentOperation::Reoffer
                    | IntentOperation::Decide
                    | IntentOperation::Revoke
                    | IntentOperation::Verify
                    | IntentOperation::Resume
                    | IntentOperation::ProjectRequest
                    | IntentOperation::ProjectDecide
                    | IntentOperation::ProjectRevoke
                    | IntentOperation::AgentRequest
                    | IntentOperation::AgentDecide
                    | IntentOperation::AgentCancel
                    | IntentOperation::AgentRevoke
                    | IntentOperation::AgentTopUpRequest
                    | IntentOperation::AgentTopUpDecide
                    | IntentOperation::AgentTopUpCancel
                    | IntentOperation::AllocationDecide
                    | IntentOperation::AllocationRelease
            ) {
            let value = self
                .central_now(ManagementCommand::Recover {
                    command_id: saved.command_id,
                    original_operation: saved.operation,
                })
                .await?;
            match value["recorded"].as_bool() {
                Some(true) => Some(
                    serde_json::from_value(value)
                        .map_err(|_| failure(503, "management_saved_result_invalid"))?,
                ),
                Some(false) => None,
                None => return Err(failure(503, "management_recovery_unavailable")),
            }
        } else {
            None
        };
        let result = match recovered {
            Some(value) => Ok(value),
            None => operation.await,
        };
        // A previous attempt may have committed before losing its reply. A
        // later provider/revision validation failure cannot prove it did not.
        // Local task failures below follow an atomic original-receipt lookup.
        // A current CAS/missing-task failure cannot hide an earlier commit.
        if !first_attempt
            && !super::operator_task_commands::is_operation(intent.operation())
            && !is_atomic_graph_operation(intent.operation())
            && !super::resource_definitions::is_operation(intent.operation())
            // This exact error follows the original canonical receipt lookup:
            // its cancellation tombstone permanently excludes an applied effect.
            && !(intent.operation() == IntentOperation::WebCatalog
                && result.as_ref().is_err_and(|error| error.code == "management_change_cancelled"))
            // Both publication writers check the exact receipt atomically
            // before current revision/retained-commitment eligibility.
            && !(intent.operation() == IntentOperation::PublishResource
                && result.as_ref().is_err_and(|error| matches!(error.code.as_str(), "management_resource_in_use" | "management_revision_conflict")))
            // The retry asks under the original key and reads the original
            // request's own row. That is its answer, and an observation leaves
            // nothing behind that an earlier attempt could have committed.
            && !(intent.operation() == IntentOperation::PersonalAgentObservation
                && result.as_ref().is_err_and(super::personal_agent_observation::request_closed))
            && result
                .as_ref()
                .is_err_and(|error| known_failure_for(intent.operation(), error))
        {
            return Err(failure(409, "management_outcome_unknown"));
        }
        let reply = match &result {
            Ok(value) => Some(IntentReply::Success(
                serde_json::to_value(value)
                    .map_err(|_| failure(409, "management_outcome_unknown"))?,
            )),
            Err(error) if known_failure_for(intent.operation(), error) => {
                Some(IntentReply::Failure {
                    status: error.status,
                    code: error.code.clone(),
                })
            }
            Err(_) => None,
        };
        if let Some(reply) = reply {
            let (current, live) = self
                .authorized()
                .await
                .map_err(|_| failure(409, "management_outcome_unknown"))?;
            if current != context {
                return Err(failure(409, "management_outcome_unknown"));
            }
            live.record_intent_reply(&current, intent, reply)
                .await
                .map_err(|_| failure(409, "management_outcome_unknown"))?;
        }
        let (current, _) = self.authorized().await.map_err(|error| {
            if result.is_ok() {
                failure(409, "management_outcome_unknown")
            } else {
                error
            }
        })?;
        if current != context {
            return Err(failure(409, "management_outcome_unknown"));
        }
        result
    }
    pub async fn saved_commands(&self) -> Result<Vec<PendingIntent>, NativeError> {
        let (context, session) = self.authorized().await?;
        let result = session.pending_intents(&context).await;
        self.finish(&context, result, false).await
    }
    /// Exact native custody lookup, retaining current scope and operation grants.
    #[cfg(any(unix, test))]
    pub(crate) async fn original_command_receipt(&self, command_id: String) -> Result<Option<PendingIntent>, NativeError> {
        let (context, session) = self.authorized().await?;
        let result = session.intent(&context, command_id).await;
        self.finish(&context, result, false).await
    }
    pub async fn acknowledge_command(&self, command_id: String) -> Result<(), NativeError> {
        let (context, session) = self.authorized().await?;
        let result = session.acknowledge_intent(&context, command_id).await;
        self.finish(&context, result, true).await
    }
    pub async fn retry_saved_command(&self, command_id: String) -> Result<Value, NativeError> {
        super::key(&command_id)?;
        let saved = self
            .saved_commands()
            .await?
            .into_iter()
            .find(|r| r.command_id == command_id)
            .ok_or_else(|| failure(404, "management_saved_command_not_found"))?;
        let parse = |input| {
            serde_json::from_value(input)
                .map_err(|_| failure(503, "management_saved_command_invalid"))
        };
        let value = match saved.operation {
            IntentOperation::WebCatalog => serde_json::to_value(Box::pin(self.apply_saved_web_catalog(saved.command_id)).await?),
            op if super::resource_definitions::is_operation(op) => serde_json::to_value(Box::pin(self.apply_saved_template(saved.command_id)).await?),
            #[cfg(feature = "resource-adoption")]
            op if super::operator_graph_commands::is_operation(op) => {
                serde_json::to_value(Box::pin(self.apply_saved_operator_graph(saved.command_id)).await?)
            },
            op if super::operator_task_commands::is_operation(op) => {
                let command: hagency_store::management::OperatorTaskCommand = serde_json::from_value(saved.input).map_err(|_| failure(503,"management_saved_command_invalid"))?;
                if command.command_id != saved.command_id || command.change.operation()!=op {return Err(failure(503,"management_saved_command_invalid"))}
                serde_json::to_value(Box::pin(self.execute_operator_task(command)).await?)
            },
            IntentOperation::LoginAccount => serde_json::to_value(Box::pin(self.account_login_status(serde_json::from_value(saved.input).map_err(|_| failure(503,"management_saved_command_invalid"))?)).await?),
            IntentOperation::RetireAccount => serde_json::to_value(Box::pin(self.retire_account(serde_json::from_value(saved.input).map_err(|_| failure(503,"management_saved_command_invalid"))?, std::time::Instant::now()+std::time::Duration::from_secs(30))).await?),
            IntentOperation::PrepareAccount => serde_json::to_value(Box::pin(self.prepare_account(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::TriageResourceAlert => serde_json::to_value(Box::pin(self.triage_resource_alert(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::RetireApprovalBinding => serde_json::to_value(Box::pin(self.retire_resource_approval_binding(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::RevokeToolGrant => serde_json::to_value(Box::pin(self.revoke_tool_grant(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::EditExecutionPolicy => serde_json::to_value(Box::pin(self.edit_execution_policy(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::ReviewStopped => serde_json::to_value(Box::pin(self.review_stopped(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::ResolveStopped => serde_json::to_value(Box::pin(self.resolve_stopped(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::ReconcileInterruptedCall => serde_json::to_value(Box::pin(self.reconcile_interrupted_call(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::ClearWorkspaceDirty => serde_json::to_value(Box::pin(self.clear_workspace_dirty(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::RenameResource => serde_json::to_value(Box::pin(self.rename_resource(serde_json::from_value(saved.input).map_err(|_| failure(503, "management_saved_command_invalid"))?)).await?),
            IntentOperation::EditLocalResource => serde_json::to_value(Box::pin(self.edit_local_resource(serde_json::from_value(saved.input).map_err(|_|failure(503,"management_saved_command_invalid"))?,std::time::Instant::now()+std::time::Duration::from_secs(30))).await?),
            IntentOperation::CreateResourceConfiguration => serde_json::to_value(Box::pin(self.create_resource_configuration(serde_json::from_value(saved.input).map_err(|_|failure(503,"management_saved_command_invalid"))?,std::time::Instant::now()+std::time::Duration::from_secs(30))).await?),
            IntentOperation::PersonalAgentObservation => Ok(Box::pin(self.retry_personal_observation(saved.input)).await?),
            IntentOperation::PersonalAgentPolicy => Ok(Box::pin(self.retry_personal_policy(saved.input)).await?),
            IntentOperation::PersonalAgentModel => Ok(Box::pin(self.retry_personal_model(saved.input)).await?),
            IntentOperation::PersonalAgentWorkspace => Ok(Box::pin(self.retry_personal_workspace(saved.input)).await?),
            IntentOperation::PersonalAgentRules => Ok(Box::pin(self.retry_personal_rules(saved.input)).await?),
            IntentOperation::EnrollAccountResource => serde_json::to_value(Box::pin(self.enroll_account_resource(
                serde_json::from_value(saved.input).map_err(|_|failure(503,"management_saved_command_invalid"))?,
                std::time::Instant::now()+std::time::Duration::from_secs(30),
            )).await?),
            // Retain the original saved receipt/input as historical custody.
            // Removed definition operations cannot be replayed into a new writer.
            IntentOperation::EnrollResource | IntentOperation::ConfigureResource => {
                return Err(failure(409,"management_operation_retired"));
            }
            IntentOperation::PublishResource => serde_json::to_value(
                Box::pin(self.publish_resource(
                    serde_json::from_value(saved.input)
                        .map_err(|_| failure(503, "management_saved_command_invalid"))?,
                ))
                .await?,
            ),
            operation => {
                let command: ManagementCommand = parse(saved.input)?;
                let matches = matches!(
                    (operation, &command),
                    (
                        IntentOperation::Contribute,
                        ManagementCommand::Contribute { .. }
                    ) | (IntentOperation::Reoffer, ManagementCommand::Reoffer { .. })
                        | (IntentOperation::Decide, ManagementCommand::Decide { .. })
                        | (IntentOperation::Revoke, ManagementCommand::Revoke { .. })
                        | (IntentOperation::Verify, ManagementCommand::Verify { .. })
                        | (IntentOperation::Resume, ManagementCommand::Resume { .. })
                        | (
                            IntentOperation::ProjectRequest,
                            ManagementCommand::ProjectRequest { .. }
                        )
                        | (
                            IntentOperation::ProjectDecide,
                            ManagementCommand::ProjectDecide { .. }
                        )
                        | (
                            IntentOperation::ProjectRevoke,
                            ManagementCommand::ProjectRevoke { .. }
                        )
                        | (
                            IntentOperation::AgentRequest,
                            ManagementCommand::AgentRequest { .. }
                        )
                        | (
                            IntentOperation::AgentDecide,
                            ManagementCommand::AgentDecide { .. }
                        )
                        | (
                            IntentOperation::AgentCancel,
                            ManagementCommand::AgentCancel { .. }
                        )
                        | (IntentOperation::AgentRevoke, ManagementCommand::AgentRevoke { .. })
                        | (IntentOperation::AgentTopUpRequest, ManagementCommand::AgentTopUpRequest { .. })
                        | (IntentOperation::AgentTopUpDecide, ManagementCommand::AgentTopUpDecide { .. })
                        | (IntentOperation::AgentTopUpCancel, ManagementCommand::AgentTopUpCancel { .. })
                        | (IntentOperation::AgentRunControl, ManagementCommand::AgentRunControl { .. })
                        | (IntentOperation::AgentSettlementRequest, ManagementCommand::AgentSettlementRequest { .. })
                        | (IntentOperation::AllocationDecide, ManagementCommand::AllocationDecide { .. })
                        | (IntentOperation::AllocationRelease, ManagementCommand::AllocationRelease { .. })
                );
                if !matches {
                    return Err(failure(503, "management_saved_command_invalid"));
                }
                Ok(Box::pin(self.central(command)).await?)
            }
        }
        .map_err(|_| failure(503, "management_saved_result_invalid"))?;
        Ok(json!({"operation":saved.operation,"data":value}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stopped_refusals_settle_but_uncertain_outcomes_keep_original_intents() {
        for operation in [IntentOperation::ReviewStopped, IntentOperation::ResolveStopped] {
            for error in [hagency_store::Error::StoppedNotResolvable, hagency_store::Error::StoppedReviewExpired, hagency_store::Error::Conflict] {
                assert!(known_failure_for(operation, &store_error(error)));
            }
            for error in [hagency_store::Error::LocalAuthority, hagency_store::Error::OutcomeUnknown, hagency_store::Error::Unavailable] {
                assert!(!known_failure_for(operation, &store_error(error)));
            }
        }
    }

    #[test]
    fn workspace_refusals_settle_but_authority_loss_keeps_original_intent() {
        for error in [
            store_error(hagency_store::Error::WorkspaceActive),
            store_error(hagency_store::Error::WorkspaceInspectionRequired),
            store_error(hagency_store::Error::Conflict),
        ] {
            assert!(known_failure_for(IntentOperation::ClearWorkspaceDirty, &error));
        }
        for error in [
            store_error(hagency_store::Error::LocalAuthority),
            store_error(hagency_store::Error::OutcomeUnknown),
            failure(503, "transport_failed"),
        ] {
            assert!(!known_failure_for(IntentOperation::ClearWorkspaceDirty, &error));
        }
    }
    #[test]
    fn uncertain_and_post_write_authorization_failures_keep_native_custody_pending() {
        for (status, code) in [
            (401, "management_scope_changed"),
            (403, "management_permission_required"),
            (409, "management_outcome_unknown"),
            (409, "management_command_conflict"),
            (500, "server_failure"),
            (503, "transport_failed"),
        ] {
            assert!(!known_failure(&failure(status, code)));
        }
        for (status, code) in [
            (400, "invalid_management_arguments"),
            (409, "provider_connection_required"),
            (409, "management_revision_changed"),
            (401, "management_administrator_required"),
            (401, "project_manager_required"),
            (409, "project_resource_capacity"),
            (409, "management_receipt_capacity"),
        ] {
            assert!(known_failure(&failure(status, code)));
        }
    }
}
