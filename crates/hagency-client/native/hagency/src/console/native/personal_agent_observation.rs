//! Explicit device observation uses the existing environment request with a
//! durable original key. Plain settings GET remains an effect-free cache read.
use super::{Command, NativeError, NativeManagement, failure};
use crate::console::device_execution::DeviceIdentity;
use hagency_store::management::{Intent, IntentOperation, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservePersonalSettings {
    pub command_id: String,
    pub agent_id: String,
    pub expected_generation: i64,
}
impl ObservePersonalSettings {
    pub fn validate(&self) -> Result<(), NativeError> {
        super::key(&self.command_id)?;
        super::key(&self.agent_id)?;
        if !(1..=9_007_199_254_740_991).contains(&self.expected_generation) {
            return Err(failure(400, "invalid_personal_observation"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    command_id: String,
    input: ObservePersonalSettings,
    requester: DeviceIdentity,
}
pub(super) fn known_failure(e: &NativeError) -> bool {
    matches!(
        e.code.as_str(),
        "invalid_personal_observation"
            | "personal_agent_not_found"
            | "personal_agent_settings_changed"
            | "configuration_device_offline"
            | "configuration_device_unavailable"
            | "configuration_queue_full"
            | "configuration_request_busy"
            | "execution_device_required"
            | "agent_retired"
    ) || request_closed(e)
    // In particular, configuration_unsupported can follow a successful POST
    // when discovery changes during polling. It keeps original custody.
}
/// The state of the original request as the server keeps it under its
/// idempotency key: the execution device completed it as failed, no device
/// claimed it before it expired, or it was claimed and has not been answered
/// (this side stops waiting after about a minute). Asking again reads the
/// same row, so this is the original request's answer and not a later check.
/// An observation only reads the device: none of these can leave a change
/// behind, and one that is answered late only refreshes the stored settings.
/// Treating them as unknown left a request that no retry could ever settle.
/// Requester-side errors that share a code carry another status (400 before
/// the request is sent, 401 when the device authority changed) and are not
/// matched here.
pub(super) fn request_closed(e: &NativeError) -> bool {
    e.status == 409
        && matches!(
            e.code.as_str(),
            "invalid_arguments"
                | "configuration_runtime_busy"
                | "configuration_model_unavailable"
                | "configuration_provider_unavailable"
                | "configuration_local_state_unavailable"
                | "configuration_authority_changed"
                | "configuration_authority_invalidated"
                | "configuration_operation_unsupported"
                | "configuration_target_changed"
                | "configuration_request_failed"
                | "configuration_request_expired"
                | "configuration_outcome_unknown"
        )
}
impl NativeManagement {
    pub async fn observe_personal_settings(
        &self,
        input: ObservePersonalSettings,
    ) -> Result<Value, NativeError> {
        input.validate()?;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, Permission::RequestPersonalAgentSettings)
            .map_err(|_| failure(403, "management_permission_required"))?;
        let existing = self
            .saved_commands()
            .await?
            .into_iter()
            .find(|row| row.command_id == input.command_id);
        let recovering = existing.is_some();
        let saved = if let Some(existing) = existing {
            if existing.operation != IntentOperation::PersonalAgentObservation {
                return Err(failure(409, "management_command_conflict"));
            }
            let saved: Saved = serde_json::from_value(existing.input)
                .map_err(|_| failure(503, "management_saved_command_invalid"))?;
            if serde_json::to_value(&saved.input).ok() != serde_json::to_value(&input).ok() {
                return Err(failure(409, "management_command_conflict"));
            }
            saved
        } else {
            Saved {
                command_id: input.command_id.clone(),
                input,
                requester: super::configuration::pin(&self.owner).await?,
            }
        };
        self.apply_personal_observation(saved, recovering).await
    }
    pub(super) async fn retry_personal_observation(
        &self,
        input: Value,
    ) -> Result<Value, NativeError> {
        let saved: Saved = serde_json::from_value(input)
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        saved.input.validate()?;
        if saved.command_id != saved.input.command_id {
            return Err(failure(503, "management_saved_command_invalid"));
        }
        self.apply_personal_observation(saved, true).await
    }
    async fn apply_personal_observation(
        &self,
        saved: Saved,
        recovering: bool,
    ) -> Result<Value, NativeError> {
        let intent = Intent::new(
            IntentOperation::PersonalAgentObservation,
            saved.command_id.clone(),
            &saved,
        )
        .map_err(super::management::store_error)?;
        self.with_intent(intent, async {
            if super::configuration::pin(&self.owner).await? != saved.requester {
                return Err(failure(409, "personal_policy_original_device_required"));
            }
            let detail = self
                .personal_agent_settings(saved.input.agent_id.clone())
                .await?;
            if !recovering && detail["agent"]["generation"] != saved.input.expected_generation {
                return Err(failure(409, "personal_agent_settings_changed"));
            }
            // The transport validates the original result's shape and agent.
            // Its private provider/path metadata never crosses into the script.
            let _ = super::configuration::remote_pinned(
                &self.owner,
                Command::AgentConfiguration {
                    agent_id: saved.input.agent_id.clone(),
                    expected_generation: saved.input.expected_generation,
                    idempotency_key: saved.command_id.clone(),
                    command: Box::new(Command::ExecutionEnvironment {
                        agent_id: saved.input.agent_id.clone(),
                    }),
                },
                Some(&saved.requester),
            )
            .await?;
            Ok(
                json!({"protocol":"hagency-personal-settings-v1","agentId":saved.input.agent_id,
                "commandId":saved.command_id,"observed":true}),
            )
        })
        .await
    }
}
