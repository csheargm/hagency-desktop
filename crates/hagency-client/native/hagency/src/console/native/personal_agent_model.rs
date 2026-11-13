//! Native-resolved personal model/workspace editing over existing device custody.
use super::{Command, LocalAction, NativeError, NativeManagement, failure};
use crate::console::device_execution::DeviceIdentity;
use hagency_agent_local::{ModelProfile, PolicyVersion, model_profile_digest};
use hagency_store::management::{HostContext, Intent, IntentOperation, Permission};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

const MAX: i64 = 9_007_199_254_740_991;
fn invalid() -> NativeError {
    failure(400, "invalid_personal_model")
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
pub(super) fn deadline(until: Instant) -> Result<(), NativeError> {
    let now = Instant::now();
    if until <= now || until > now + Duration::from_secs(120) {
        Err(failure(403, "personal_confirmation_expired"))
    } else {
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavePersonalModel {
    pub command_id: String,
    pub agent_id: String,
    pub expected_generation: i64,
    pub expected_profile_digest: String,
    pub runtime: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub reasoning_effort: String,
    pub web_search: bool,
    pub workspace: String,
}
impl SavePersonalModel {
    pub fn validate(&self) -> Result<(), NativeError> {
        super::key(&self.command_id)?;
        super::key(&self.agent_id)?;
        if !(1..=MAX).contains(&self.expected_generation)
            || !digest(&self.expected_profile_digest)
            || !matches!(self.runtime.as_str(), "codex" | "claude" | "octos")
            || !text(&self.model, 512)
            || self.profile_id.as_ref().is_some_and(|id| self.runtime != "octos" || !hagency_runtime::octos::profile_id(id))
            || self.reasoning_effort.len() > 32
            || self.reasoning_effort.chars().any(char::is_control)
            || !matches!(self.workspace.as_str(), "preserve" | "default")
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalModelView {
    pub runtime: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub reasoning_effort: String,
    pub web_search: bool,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalModelChoice {
    pub runtime: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    pub display_name: String,
    pub reasoning_efforts: Vec<String>,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalModelChoices {
    pub agent_id: String,
    pub generation: i64,
    pub profile_digest: String,
    pub current: Option<PersonalModelView>,
    pub models: Vec<PersonalModelChoice>,
    pub workspace_configured: bool,
    pub can_choose_workspace: bool,
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PersonalModelSummary {
    pub agent_id: String,
    pub expected_generation: i64,
    pub profile_digest: String,
    pub model: PersonalModelView,
    pub workspace: String,
    pub can_choose_workspace: bool,
}
/// Only the trusted native dialog sees this handle; no path/credential is serializable.
pub struct PersonalModelReview {
    pub summary: PersonalModelSummary,
    saved: Saved,
    context: HostContext,
    expires: Instant,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    command_id: String,
    input: SavePersonalModel,
    requester: DeviceIdentity,
    profile: ModelProfile,
}
pub(super) struct Snapshot {
    pub context: HostContext,
    pub requester: DeviceIdentity,
    pub generation: i64,
    pub local: bool,
    pub model: Option<ModelProfile>,
    pub policy: PolicyVersion,
    pub providers: Vec<Value>,
}
pub(super) fn view(profile: &ModelProfile) -> PersonalModelView {
    PersonalModelView {
        runtime: hagency_agent_local::runtime::kind(&profile.credential_ref)
            .name()
            .into(),
        model: profile.model.clone(),
        profile_id: hagency_agent_local::runtime::managed_reference(&profile.credential_ref)
            .and_then(|(kind,id)| (kind == hagency_agent_local::runtime::Kind::Octos).then_some(id).flatten())
            .map(str::to_owned),
        reasoning_effort: profile.reasoning_effort.clone(),
        web_search: profile.web_search,
    }
}
/// The public ID must match the native, owner-connected credential reference.
/// Only the ID is projected to the miniapp, never the reference itself.
fn choice_profile<'a>(runtime: &str, row: &'a Value) -> Result<Option<&'a str>, NativeError> {
    if runtime != "octos" { return Ok(None); }
    let reference = row["credentialRef"].as_str().ok_or_else(invalid)?;
    let (kind, id) = hagency_agent_local::runtime::managed_reference(reference).ok_or_else(invalid)?;
    let id = id.filter(|_| kind == hagency_agent_local::runtime::Kind::Octos).ok_or_else(invalid)?;
    if row.get("profileId").is_some_and(|value| value.as_str() != Some(id)) { return Err(invalid()); }
    Ok(Some(id))
}
fn selected_model<'a>(provider: &'a Value, input: &SavePersonalModel) -> Result<&'a Value, NativeError> {
    let rows = provider["models"].as_array().ok_or_else(|| failure(409, "configuration_model_unavailable"))?;
    let mut selected = None;
    for row in rows {
        if row["model"] != input.model || !row["supportedReasoningEfforts"].as_array().is_some_and(|e| e.iter().any(|e| e["reasoningEffort"] == input.reasoning_effort)) { continue; }
        let profile = choice_profile(&input.runtime, row)?;
        if input.profile_id.as_deref().is_some_and(|expected| profile != Some(expected)) { continue; }
        // A legacy request without a profile must never guess between two
        // Octos configurations with the same model and reasoning effort.
        if selected.is_some() && input.runtime == "octos" { return Err(failure(409, "configuration_model_unavailable")); }
        if selected.is_none() { selected = Some(row); }
    }
    selected.ok_or_else(|| failure(409, "configuration_model_unavailable"))
}
pub(super) fn known_failure(e: &NativeError) -> bool {
    super::personal_agent_policy::known_failure(e)
        || matches!(
            e.code.as_str(),
            "invalid_personal_model"
                | "personal_model_observation_required"
                | "personal_model_changed"
                | "personal_workspace_required"
                | "personal_execution_device_required"
                | "configuration_model_unavailable"
                | "configuration_runtime_busy"
        )
}
impl NativeManagement {
    pub(super) async fn personal_edit_snapshot(
        &self,
        agent: &str,
        permission: Permission,
    ) -> Result<Snapshot, NativeError> {
        super::key(agent)?;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, permission)
            .map_err(super::management::store_error)?;
        let detail = self.personal_agent_settings(agent.into()).await?;
        let requester = super::configuration::pin(&self.owner).await?;
        let raw = super::configuration::remote_pinned(
            &self.owner,
            Command::AgentConfigurationStatus {
                agent_id: agent.into(),
            },
            Some(&requester),
        )
        .await?;
        let c = &raw["configuration"];
        let generation = c["agentGeneration"]
            .as_i64()
            .filter(|v| (1..=MAX).contains(v))
            .ok_or_else(invalid)?;
        if detail["agent"]["generation"] != generation {
            return Err(failure(409, "personal_agent_settings_changed"));
        }
        let env = &c["environment"];
        if env.is_null() {
            return Err(failure(409, "personal_model_observation_required"));
        }
        let model: Option<ModelProfile> =
            serde_json::from_value(env["configuration"]["modelProfile"].clone())
                .map_err(|_| failure(503, "invalid_configuration_response"))?;
        let policy = serde_json::from_value(env["configuration"]["policy"].clone())
            .map_err(|_| failure(503, "invalid_configuration_response"))?;
        let providers = env["providers"]
            .as_array()
            .cloned()
            .ok_or_else(|| failure(503, "invalid_configuration_response"))?;
        let (fresh, session) = self.authorized().await?;
        if fresh != context || super::configuration::pin(&self.owner).await? != requester {
            return Err(failure(401, "management_scope_changed"));
        }
        session
            .authorize(&fresh, permission)
            .map_err(super::management::store_error)?;
        let local = c["executionDeviceId"] == requester.id
            && c["deviceGeneration"].as_u64() == Some(requester.generation);
        Ok(Snapshot {
            context,
            requester,
            generation,
            local,
            model,
            policy,
            providers,
        })
    }
    pub async fn personal_model_choices(
        &self,
        agent_id: String,
    ) -> Result<PersonalModelChoices, NativeError> {
        let snapshot = self
            .personal_edit_snapshot(&agent_id, Permission::ReadPersonalAgentSettings)
            .await?;
        let mut models = Vec::new();
        for provider in &snapshot.providers {
            if provider["authenticated"] != true {
                continue;
            }
            if let Some(rows) = provider["models"].as_array() {
                for row in rows {
                    if models.len() >= 256 {
                        return Err(failure(503, "management_capacity"));
                    }
                    let model = row["model"]
                        .as_str()
                        .filter(|s| text(s, 512))
                        .ok_or_else(invalid)?;
                    let runtime = provider["runtime"]
                        .as_str()
                        .filter(|s| matches!(*s, "codex" | "claude" | "octos"))
                        .ok_or_else(invalid)?;
                    let efforts = row["supportedReasoningEfforts"]
                        .as_array()
                        .filter(|v| v.len() <= 16)
                        .ok_or_else(invalid)?
                        .iter()
                        .map(|v| {
                            v["reasoningEffort"]
                                .as_str()
                                .filter(|s| s.len() <= 32 && !s.chars().any(char::is_control))
                                .map(str::to_owned)
                                .ok_or_else(invalid)
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    models.push(PersonalModelChoice {
                        runtime: runtime.into(),
                        model: model.into(),
                        profile_id: choice_profile(runtime, row)?.map(str::to_owned),
                        display_name: row["displayName"].as_str().unwrap_or(model).into(),
                        reasoning_efforts: efforts,
                    });
                }
            }
        }
        Ok(PersonalModelChoices {
            agent_id,
            generation: snapshot.generation,
            profile_digest: model_profile_digest(snapshot.model.as_ref()).map_err(|_| invalid())?,
            current: snapshot.model.as_ref().map(view),
            workspace_configured: snapshot
                .model
                .as_ref()
                .is_some_and(|p| !p.workspace_root.is_empty()),
            models,
            can_choose_workspace: snapshot.local,
        })
    }
    pub async fn review_personal_model(
        &self,
        input: &SavePersonalModel,
        selected_workspace: Option<PathBuf>,
    ) -> Result<PersonalModelReview, NativeError> {
        input.validate()?;
        let snapshot = self
            .personal_edit_snapshot(&input.agent_id, Permission::EditPersonalAgentModel)
            .await?;
        if snapshot.generation != input.expected_generation {
            return Err(failure(409, "personal_agent_settings_changed"));
        }
        if model_profile_digest(snapshot.model.as_ref()).map_err(|_| invalid())?
            != input.expected_profile_digest
        {
            return Err(failure(409, "personal_model_changed"));
        }
        let provider = snapshot
            .providers
            .iter()
            .find(|p| p["runtime"] == input.runtime && p["authenticated"] == true)
            .ok_or_else(|| failure(409, "configuration_provider_unavailable"))?;
        let row = selected_model(provider, input)?;
        let reference = row
            .get("credentialRef")
            .unwrap_or(&provider["credentialRef"])
            .as_str()
            .ok_or_else(invalid)?
            .to_owned();
        let workspace_mode = if selected_workspace.is_some() {
            "selected"
        } else {
            &input.workspace
        }
        .to_owned();
        let workspace_root = if let Some(path) = selected_workspace {
            if !snapshot.local {
                return Err(failure(409, "personal_execution_device_required"));
            }
            let path = std::fs::canonicalize(path).map_err(|_| invalid())?;
            if !path.is_dir() {
                return Err(invalid());
            }
            path.to_str().ok_or_else(invalid)?.to_owned()
        } else if input.workspace == "preserve" {
            snapshot
                .model
                .as_ref()
                .map(|p| p.workspace_root.clone())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| failure(409, "personal_workspace_required"))?
        } else {
            String::new()
        };
        let profile = ModelProfile {
            model: input.model.clone(),
            credential_ref: reference,
            workspace_root,
            reasoning_effort: input.reasoning_effort.clone(),
            web_search: input.web_search,
        };
        let summary = PersonalModelSummary {
            agent_id: input.agent_id.clone(),
            expected_generation: input.expected_generation,
            profile_digest: input.expected_profile_digest.clone(),
            model: view(&profile),
            workspace: workspace_mode,
            can_choose_workspace: snapshot.local,
        };
        Ok(PersonalModelReview {
            summary,
            saved: Saved {
                command_id: input.command_id.clone(),
                input: input.clone(),
                requester: snapshot.requester,
                profile,
            },
            context: snapshot.context,
            expires: Instant::now() + Duration::from_secs(120),
        })
    }
    pub async fn save_personal_model(
        &self,
        review: PersonalModelReview,
        confirmed_until: Instant,
    ) -> Result<Value, NativeError> {
        deadline(confirmed_until)?;
        if Instant::now() >= review.expires {
            return Err(failure(403, "personal_confirmation_expired"));
        }
        let (context, session) = self.authorized().await?;
        if context != review.context {
            return Err(failure(401, "management_scope_changed"));
        }
        session
            .authorize(&context, Permission::EditPersonalAgentModel)
            .map_err(super::management::store_error)?;
        self.apply_personal_model(review.saved, false).await
    }
    pub(super) async fn retry_personal_model(&self, value: Value) -> Result<Value, NativeError> {
        let saved: Saved = serde_json::from_value(value)
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        self.apply_personal_model(saved, true).await
    }
    async fn apply_personal_model(
        &self,
        saved: Saved,
        recovering: bool,
    ) -> Result<Value, NativeError> {
        saved.input.validate()?;
        if saved.command_id != saved.input.command_id {
            return Err(failure(503, "management_saved_command_invalid"));
        }
        let intent = Intent::new(
            IntentOperation::PersonalAgentModel,
            saved.command_id.clone(),
            &saved,
        )
        .map_err(super::management::store_error)?;
        self.with_intent(intent,async{
            if super::configuration::pin(&self.owner).await?!=saved.requester{return Err(failure(409,"personal_policy_original_device_required"))}
            let detail=self.personal_agent_settings(saved.input.agent_id.clone()).await?;
            if !recovering&&detail["agent"]["generation"]!=saved.input.expected_generation{return Err(failure(409,"personal_agent_settings_changed"))}
            let result=super::configuration::remote_pinned(&self.owner,Command::AgentConfiguration{agent_id:saved.input.agent_id.clone(),expected_generation:saved.input.expected_generation,idempotency_key:saved.command_id.clone(),command:Box::new(Command::Local{agent_id:saved.input.agent_id.clone(),action:LocalAction::SaveAgentModel,input:Some(json!({"profile":saved.profile,"expectedProfileDigest":saved.input.expected_profile_digest})),binding_id:None,requester:None})},Some(&saved.requester)).await?;
            let recorded:ModelProfile=serde_json::from_value(result["modelProfile"].clone()).map_err(|_|failure(409,"management_outcome_unknown"))?;
            if recorded.model!=saved.profile.model||recorded.credential_ref!=saved.profile.credential_ref||recorded.reasoning_effort!=saved.profile.reasoning_effort||recorded.web_search!=saved.profile.web_search||(!saved.profile.workspace_root.is_empty()&&recorded.workspace_root!=saved.profile.workspace_root)||recorded.workspace_root.is_empty(){return Err(failure(409,"management_outcome_unknown"))}
            Ok(json!({"protocol":"hagency-personal-settings-v1","agentId":saved.input.agent_id,"commandId":saved.command_id,"profileDigest":model_profile_digest(Some(&recorded)).map_err(|_|failure(409,"management_outcome_unknown"))?,"saved":true}))
        }).await
    }
}

#[cfg(test)]
mod octos_profile_tests {
    use super::*;

    fn row(id: &str) -> Value {
        json!({"model":"local-model","profileId":id,
            "credentialRef":format!("octos-managed:shared-home:{}:{id}","a".repeat(64)),
            "supportedReasoningEfforts":[{"reasoningEffort":"none"}]})
    }
    fn input() -> Value {
        json!({"commandId":"original","agentId":"agent_a","expectedGeneration":3,
            "expectedProfileDigest":"b".repeat(64),"runtime":"octos","model":"local-model",
            "reasoningEffort":"none","webSearch":false,"workspace":"preserve"})
    }
    #[test]
    fn same_model_selects_exact_profile_and_old_ambiguous_request_fails_closed() {
        let provider=json!({"models":[row("first"),row("second")]});
        let legacy: SavePersonalModel=serde_json::from_value(input()).unwrap();
        legacy.validate().unwrap();
        assert_eq!(serde_json::to_value(&legacy).unwrap(),input(),"old saved request must remain byte-compatible");
        assert_eq!(selected_model(&provider,&legacy).unwrap_err().code,"configuration_model_unavailable");
        let mut selected=legacy.clone();selected.profile_id=Some("second".into());
        selected.validate().unwrap();
        assert_eq!(selected_model(&provider,&selected).unwrap()["profileId"],"second");
        selected.profile_id=Some("missing".into());
        assert_eq!(selected_model(&provider,&selected).unwrap_err().code,"configuration_model_unavailable");
        let one=json!({"models":[row("first")]});
        assert_eq!(selected_model(&one,&legacy).unwrap()["profileId"],"first");
    }
    #[test]
    fn public_profile_id_must_match_the_native_reference() {
        let mut altered=row("first");altered["profileId"]="second".into();
        assert!(choice_profile("octos",&altered).is_err());
        altered["credentialRef"]="provider-secret".into();
        assert!(choice_profile("octos",&altered).is_err());
        let mut input: SavePersonalModel=serde_json::from_value(input()).unwrap();
        input.profile_id=Some("../other".into());assert!(input.validate().is_err());
        input.runtime="codex".into();input.profile_id=Some("first".into());assert!(input.validate().is_err());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparePersonalWorkspace {
    pub command_id: String,
    pub agent_id: String,
    pub expected_generation: i64,
}
impl PreparePersonalWorkspace {
    pub fn validate(&self) -> Result<(), NativeError> {
        super::key(&self.command_id)?;
        super::key(&self.agent_id)?;
        if !(1..=MAX).contains(&self.expected_generation) {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedWorkspace {
    command_id: String,
    input: PreparePersonalWorkspace,
    requester: DeviceIdentity,
}
impl NativeManagement {
    pub async fn prepare_personal_workspace(
        &self,
        input: PreparePersonalWorkspace,
        confirmed_until: Instant,
    ) -> Result<Value, NativeError> {
        input.validate()?;
        deadline(confirmed_until)?;
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, Permission::PreparePersonalAgentWorkspace)
            .map_err(super::management::store_error)?;
        let saved = SavedWorkspace {
            command_id: input.command_id.clone(),
            input,
            requester: super::configuration::pin(&self.owner).await?,
        };
        self.apply_personal_workspace(saved, false).await
    }
    pub(super) async fn retry_personal_workspace(
        &self,
        value: Value,
    ) -> Result<Value, NativeError> {
        let saved = serde_json::from_value(value)
            .map_err(|_| failure(503, "management_saved_command_invalid"))?;
        self.apply_personal_workspace(saved, true).await
    }
    async fn apply_personal_workspace(
        &self,
        saved: SavedWorkspace,
        recovering: bool,
    ) -> Result<Value, NativeError> {
        saved.input.validate()?;
        if saved.command_id != saved.input.command_id {
            return Err(failure(503, "management_saved_command_invalid"));
        }
        let intent = Intent::new(
            IntentOperation::PersonalAgentWorkspace,
            saved.command_id.clone(),
            &saved,
        )
        .map_err(super::management::store_error)?;
        self.with_intent(intent,async{
            if super::configuration::pin(&self.owner).await?!=saved.requester{return Err(failure(409,"personal_policy_original_device_required"))}
            let detail=self.personal_agent_settings(saved.input.agent_id.clone()).await?;
            if !recovering&&detail["agent"]["generation"]!=saved.input.expected_generation{return Err(failure(409,"personal_agent_settings_changed"))}
            let result=super::configuration::remote_pinned(&self.owner,Command::AgentConfiguration{agent_id:saved.input.agent_id.clone(),expected_generation:saved.input.expected_generation,idempotency_key:saved.command_id.clone(),command:Box::new(Command::PrepareAgentWorkspace{agent_id:saved.input.agent_id.clone()})},Some(&saved.requester)).await?;
            if result["agentId"]!=saved.input.agent_id||!result["workspace"].as_str().is_some_and(|v|!v.is_empty()){return Err(failure(409,"management_outcome_unknown"))}
            Ok(json!({"protocol":"hagency-personal-settings-v1","agentId":saved.input.agent_id,"commandId":saved.command_id,"workspacePrepared":true}))
        }).await
    }
}
