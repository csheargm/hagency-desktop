//! Native-generated IDs and original-command recovery for closed operator edits.
use super::{NativeError, NativeManagement, failure, management::store_error};
use hagency_store::management::IntentReply;
use hagency_store::management::{
    Intent, IntentOperation, OperatorTaskChange, OperatorTaskCommand, OperatorTaskResult,
};
use serde::Serialize;

#[derive(Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedOperatorTask {
    pub command_id: String,
    pub task_id: String,
    pub operation: IntentOperation,
    pub change: OperatorTaskChange,
    pub result: Option<OperatorTaskResult>,
    pub error: Option<String>,
}
fn id(prefix: &str) -> Result<String, NativeError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| failure(503, "native_random_unavailable"))?;
    Ok(format!(
        "{prefix}{}",
        bytes.iter().map(|v| format!("{v:02x}")).collect::<String>()
    ))
}
pub(super) fn is_operation(op: IntentOperation) -> bool {
    matches!(
        op,
        IntentOperation::TaskCreate
            | IntentOperation::TaskEdit
            | IntentOperation::TaskDelete
            | IntentOperation::TaskAccept
            | IntentOperation::TaskTransition
            | IntentOperation::TaskComment
    )
}
pub(super) fn known_failure(error: &NativeError) -> bool {
    matches!(
        (error.status, error.code.as_str()),
        (400, "invalid_management_arguments")
            | (404, "management_not_found")
            | (409, "management_task_revision_changed")
    )
}
impl NativeManagement {
    pub async fn operator_task_write_access(&self) -> Result<bool, NativeError> {
        let (context, session) = self.authorized().await?;
        let result = session.operator_task_write_access(&context).await;
        self.finish(&context, result, false).await
    }
    /// Durable preparation only: this never changes a task or dispatches work.
    /// If the reply is lost, list saved commands; do not prepare a replacement.
    pub async fn prepare_operator_task(
        &self,
        change: OperatorTaskChange,
    ) -> Result<PreparedOperatorTask, NativeError> {
        change.validate().map_err(store_error)?;
        let _mutation = self.owner.0.management.mutations.lock().await;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, change.permission())
            .map_err(store_error)?;
        if !session
            .operator_task_write_access(&context)
            .await
            .map_err(store_error)?
        {
            return Err(failure(409, "current_task_proof_required"));
        }
        let (fresh, session) = self.authorized().await?;
        if fresh != context {
            return Err(failure(401, "management_scope_changed"));
        }
        let command = OperatorTaskCommand {
            command_id: id("taskcmd_")?,
            task_id: match change.task_id() {
                Some(v) => v.into(),
                None => id("task_")?,
            },
            change,
        };
        let operation = command.change.operation();
        let intent =
            Intent::new(operation, command.command_id.clone(), &command).map_err(store_error)?;
        let saved = session.prepare_intent(&context, intent).await;
        self.finish(&context, saved, true).await?;
        Ok(PreparedOperatorTask {
            command_id: command.command_id,
            task_id: command.task_id,
            operation,
            change: command.change,
            result: None,
            error: None,
        })
    }
    /// Read the exact saved local task payload for review, never a replacement.
    pub async fn review_saved_operator_task(
        &self,
        command_id: String,
    ) -> Result<PreparedOperatorTask, NativeError> {
        super::key(&command_id)?;
        let saved = self
            .saved_commands()
            .await?
            .into_iter()
            .find(|v| v.command_id == command_id)
            .ok_or_else(|| failure(404, "management_saved_command_not_found"))?;
        if !is_operation(saved.operation) {
            return Err(failure(400, "invalid_management_arguments"));
        }
        let command: OperatorTaskCommand = serde_json::from_value(saved.input)
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        command
            .validate()
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        if command.command_id != command_id || command.change.operation() != saved.operation {
            return Err(failure(503, "management_saved_command_invalid"));
        }
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, command.change.permission())
            .map_err(store_error)?;
        let proof = session.operator_task_write_access(&context).await;
        if !self.finish(&context, proof, false).await? {
            return Err(failure(409, "current_task_proof_required"));
        }
        let (result, error) = match saved.reply {
            Some(IntentReply::Success(value)) => (
                Some(
                    serde_json::from_value(value)
                        .map_err(|_| failure(503, "management_saved_result_invalid"))?,
                ),
                None,
            ),
            Some(IntentReply::Failure { code, .. }) => (None, Some(code)),
            None => (None, None),
        };
        Ok(PreparedOperatorTask {
            command_id,
            task_id: command.task_id,
            operation: saved.operation,
            change: command.change,
            result,
            error,
        })
    }
    /// Only an existing native ID is accepted. The original full payload comes
    /// from private custody and retains its expected revision and target ID.
    pub async fn apply_saved_operator_task(
        &self,
        command_id: String,
    ) -> Result<OperatorTaskResult, NativeError> {
        super::key(&command_id)?;
        let saved = self
            .saved_commands()
            .await?
            .into_iter()
            .find(|v| v.command_id == command_id)
            .ok_or_else(|| failure(404, "management_saved_command_not_found"))?;
        if !is_operation(saved.operation) {
            return Err(failure(400, "invalid_management_arguments"));
        }
        let command: OperatorTaskCommand = serde_json::from_value(saved.input)
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        if command.command_id != command_id || command.change.operation() != saved.operation {
            return Err(failure(503, "management_saved_command_invalid"));
        }
        self.execute_operator_task(command).await
    }
    pub(super) async fn execute_operator_task(
        &self,
        command: OperatorTaskCommand,
    ) -> Result<OperatorTaskResult, NativeError> {
        command.validate().map_err(store_error)?;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, command.change.permission())
            .map_err(store_error)?;
        if !session
            .operator_task_write_access(&context)
            .await
            .map_err(store_error)?
        {
            return Err(failure(409, "current_task_proof_required"));
        }
        let intent = Intent::new(
            command.change.operation(),
            command.command_id.clone(),
            &command,
        )
        .map_err(store_error)?;
        self.with_intent(intent, async {
            let (context, session) = self.authorized().await?;
            let result = session.apply_operator_task(&context, command).await;
            self.finish(&context, result, true).await
        })
        .await
    }
}

#[cfg(all(test, feature = "resource-adoption"))]
#[path = "operator_task_command_tests.rs"]
mod tests;
