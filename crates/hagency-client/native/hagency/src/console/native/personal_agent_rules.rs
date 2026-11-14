//! Custom tool/directory policy is reviewed only in the trusted native editor.
//! The original complete policy and requester are durable; retries never rebuild
//! approved rules from a later snapshot or expose local paths to the mini-app.
use super::{Command, LocalAction, NativeError, NativeManagement, failure};
use crate::console::device_execution::DeviceIdentity;
use hagency_agent_local::{Policy, ToolPolicy};
use hagency_store::management::{HostContext, Intent, IntentOperation, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const MAX: i64 = 9_007_199_254_740_991;
fn invalid() -> NativeError {
    failure(400, "invalid_personal_rules")
}
/// Native-only: paths and the original policy never cross the script boundary.
pub struct PersonalRulesReview {
    pub agent_id: String,
    pub expected_generation: i64,
    pub policy_revision: i64,
    pub tools: Vec<String>,
    pub directories: Vec<PathBuf>,
    pub can_edit_directories: bool,
    original: Policy,
    requester: DeviceIdentity,
    context: HostContext,
    expires: Instant,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    command_id: String,
    agent_id: String,
    expected_generation: i64,
    expected_revision: i64,
    policy: Policy,
    requester: DeviceIdentity,
}
fn replacement(
    mut original: Policy,
    tools: Vec<String>,
    directories: Vec<PathBuf>,
) -> Result<Policy, NativeError> {
    if tools.is_empty() || tools.len() > 64 || directories.is_empty() || directories.len() > 64 {
        return Err(invalid());
    }
    if tools
        .iter()
        .any(|t| t.is_empty() || t.len() > 128 || t.trim() != t || t.chars().any(char::is_control))
    {
        return Err(invalid());
    }
    let directories = directories
        .into_iter()
        .map(|path| {
            if !path.is_absolute() {
                return Err(invalid());
            }
            let canonical = std::fs::canonicalize(path).map_err(|_| invalid())?;
            if !canonical.is_dir() {
                return Err(invalid());
            }
            let text = canonical.to_str().ok_or_else(invalid)?;
            if text.len() > 4096 || text.chars().any(char::is_control) {
                return Err(invalid());
            }
            Ok(text.to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    original.high_risk = ToolPolicy::AllowWithRules { tools, directories };
    // Full trust is saved only as its own choice, under its warning.
    if original.high_risk.is_full_trust() {
        return Err(invalid());
    }
    Ok(original)
}
fn request(saved: &Saved) -> Command {
    Command::AgentConfiguration {
        agent_id: saved.agent_id.clone(),
        expected_generation: saved.expected_generation,
        idempotency_key: saved.command_id.clone(),
        command: Box::new(Command::Local {
            agent_id: saved.agent_id.clone(),
            action: LocalAction::SaveAgentPolicy,
            input: Some(json!({"expectedRevision":saved.expected_revision,"policy":saved.policy})),
            binding_id: None,
            requester: None,
        }),
    }
}
fn validate_saved(saved: &Saved) -> Result<(), NativeError> {
    super::key(&saved.command_id)?;
    super::key(&saved.agent_id)?;
    if !(1..=MAX).contains(&saved.expected_generation)
        || !(0..MAX).contains(&saved.expected_revision)
    {
        return Err(invalid());
    }
    let ToolPolicy::AllowWithRules { tools, directories } = &saved.policy.high_risk else {
        return Err(invalid());
    };
    if tools.is_empty()
        || tools.len() > 64
        || directories.is_empty()
        || directories.len() > 64
        || tools.iter().any(|t| {
            t.is_empty() || t.len() > 128 || t.trim() != t || t.chars().any(char::is_control)
        })
        || directories.iter().any(|p| {
            p.len() > 4096
                || p.chars().any(char::is_control)
                || !std::path::Path::new(p).is_absolute()
        })
    {
        return Err(invalid());
    }
    // This is the existing remote command byte limit, not a truncation policy.
    // Do not re-canonicalize saved paths: recovery must replay the original data.
    if serde_json::to_vec(&request(saved))
        .map_err(|_| invalid())?
        .len()
        > 65536
    {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn known_failure(error: &NativeError) -> bool {
    error.code == "invalid_personal_rules" || super::personal_agent_model::known_failure(error)
}
impl NativeManagement {
    pub async fn review_personal_rules(
        &self,
        agent_id: String,
        expected_generation: i64,
    ) -> Result<PersonalRulesReview, NativeError> {
        let snapshot = self
            .personal_edit_snapshot(&agent_id, Permission::EditPersonalAgentPolicy)
            .await?;
        if snapshot.generation != expected_generation {
            return Err(failure(409, "personal_agent_settings_changed"));
        }
        if !snapshot.local {
            return Err(failure(409, "personal_execution_device_required"));
        }
        let (tools, directories) = match &snapshot.policy.policy.high_risk {
            // Full trust is not a list the owner wrote; rules start empty.
            trusted if trusted.is_full_trust() => (Vec::new(), Vec::new()),
            ToolPolicy::AllowWithRules { tools, directories } => (
                tools.clone(),
                directories.iter().map(PathBuf::from).collect(),
            ),
            _ => (Vec::new(), Vec::new()),
        };
        Ok(PersonalRulesReview {
            agent_id,
            expected_generation,
            policy_revision: snapshot.policy.revision,
            tools,
            directories,
            can_edit_directories: snapshot.local,
            original: snapshot.policy.policy,
            requester: snapshot.requester,
            context: snapshot.context,
            expires: Instant::now() + Duration::from_secs(120),
        })
    }
    pub async fn save_personal_rules(
        &self,
        review: PersonalRulesReview,
        command_id: String,
        tools: Vec<String>,
        directories: Vec<PathBuf>,
        confirmed_until: Instant,
    ) -> Result<Value, NativeError> {
        super::personal_agent_model::deadline(confirmed_until)?;
        if Instant::now() >= review.expires {
            return Err(failure(403, "personal_confirmation_expired"));
        }
        let (context, session) = self.authorized().await?;
        if context != review.context {
            return Err(failure(401, "management_scope_changed"));
        }
        session
            .authorize(&context, Permission::EditPersonalAgentPolicy)
            .map_err(super::management::store_error)?;
        if !review.can_edit_directories {
            return Err(failure(409, "personal_execution_device_required"));
        }
        let saved = Saved {
            command_id,
            agent_id: review.agent_id,
            expected_generation: review.expected_generation,
            expected_revision: review.policy_revision,
            policy: replacement(review.original, tools, directories)?,
            requester: review.requester,
        };
        self.apply_personal_rules(saved, false).await
    }
    pub(super) async fn retry_personal_rules(&self, input: Value) -> Result<Value, NativeError> {
        let saved = serde_json::from_value(input)
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        self.apply_personal_rules(saved, true).await
    }
    async fn apply_personal_rules(
        &self,
        saved: Saved,
        recovering: bool,
    ) -> Result<Value, NativeError> {
        validate_saved(&saved)?;
        let intent = Intent::new(
            IntentOperation::PersonalAgentRules,
            saved.command_id.clone(),
            &saved,
        )
        .map_err(super::management::store_error)?;
        self.with_intent(intent,async {
            if super::configuration::pin(&self.owner).await? != saved.requester {
                return Err(failure(409,"personal_policy_original_device_required"));
            }
            let detail = self.personal_agent_settings(saved.agent_id.clone()).await?;
            if !recovering && detail["agent"]["generation"] != saved.expected_generation {
                return Err(failure(409,"personal_agent_settings_changed"));
            }
            let result = super::configuration::remote_pinned(&self.owner,request(&saved),Some(&saved.requester)).await?;
            let revision = result["policy"]["revision"].as_i64().filter(|r| *r>saved.expected_revision && *r<=MAX)
                .ok_or_else(|| failure(409,"management_outcome_unknown"))?;
            if result["policy"]["policy"] != json!(saved.policy) { return Err(failure(409,"management_outcome_unknown")); }
            Ok(json!({"protocol":"hagency-personal-settings-v1","agentId":saved.agent_id,"commandId":saved.command_id,"policyRevision":revision,"saved":true}))
        }).await
    }
}
