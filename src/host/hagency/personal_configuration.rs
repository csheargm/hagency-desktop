//! Trusted personal configuration review. Paths and full rules stay native.
use super::*;
use ::hagency::native_owner::{
    PreparePersonalWorkspace, SavePersonalModel,
};
use ::hagency::daemon::bridge::{PersonalModelReview, PersonalRulesReview};
use serde::Deserialize;
use std::path::PathBuf;

pub(crate) const CHOICES: &str = "hagency.personal_agents.model.choices";
pub(crate) const MODEL: &str = "hagency.personal_agents.model.save";
pub(crate) const WORKSPACE: &str = "hagency.personal_agents.workspace.prepare";
pub(crate) const RULES: &str = "hagency.personal_agents.rules.edit";

pub(super) fn permissions(service: &str) -> &'static [ManagementPermission] {
    use ManagementPermission::*;
    match service {
        CHOICES => &[ReadPersonalAgentSettings],
        MODEL => &[
            ReadPersonalAgentSettings,
            EditPersonalAgentModel,
            ReadIntents,
        ],
        WORKSPACE => &[
            ReadPersonalAgentSettings,
            PreparePersonalAgentWorkspace,
            ReadIntents,
        ],
        RULES => &[
            ReadPersonalAgentSettings,
            EditPersonalAgentPolicy,
            ReadIntents,
        ],
        _ => &[],
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RulesInput {
    command_id: String,
    agent_id: String,
    expected_generation: i64,
}
pub(super) enum Input {
    Model(SavePersonalModel),
    Workspace(PreparePersonalWorkspace),
    Rules(RulesInput),
}
pub(super) fn parse_input(service: &str, args: Value) -> Result<Input, String> {
    Ok(match service {
        MODEL => {
            let input: SavePersonalModel = parse(args)?;
            input.validate().map_err(|e| e.code)?;
            Input::Model(input)
        }
        WORKSPACE => {
            let input: PreparePersonalWorkspace = parse(args)?;
            input.validate().map_err(|e| e.code)?;
            Input::Workspace(input)
        }
        RULES => {
            let input: RulesInput = parse(args)?;
            let valid_id = |v: &str| {
                !v.is_empty()
                    && v.len() <= 128
                    && v.bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            };
            if !valid_id(&input.command_id)
                || !valid_id(&input.agent_id)
                || !(1..=9_007_199_254_740_991).contains(&input.expected_generation)
            {
                return Err("invalid_management_arguments".into());
            }
            Input::Rules(input)
        }
        _ => return Err("unknown_hagency_service".into()),
    })
}

enum Review {
    Model(PersonalModelReview),
    Rules(PersonalRulesReview),
    Workspace,
}

pub(crate) struct PersonalConsent {
    host: Host,
    lease: Lease,
    service: &'static str,
    input: Input,
    expires: Instant,
    review: Option<Review>,
    selected_workspace: Option<PathBuf>,
    tools: Vec<String>,
    directories: Vec<PathBuf>,
}
impl Host {
    pub fn personal_configuration_consent(
        &self,
        lease: Lease,
        service: &str,
        args: Value,
    ) -> Result<PersonalConsent, String> {
        let input = parse_input(service, args)?;
        let service = match service {
            MODEL => MODEL,
            WORKSPACE => WORKSPACE,
            RULES => RULES,
            _ => unreachable!(),
        };
        let account = self.check(&lease)?;
        lease.authorize(&account, service, None)?;
        Ok(PersonalConsent {
            host: self.clone(),
            lease,
            service,
            input,
            expires: Instant::now() + Duration::from_secs(120),
            review: None,
            selected_workspace: None,
            tools: vec![],
            directories: vec![],
        })
    }
}
impl PersonalConsent {
    pub fn account(&self) -> &str {
        &self.lease.identity().account
    }
    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }
    pub fn is_rules(&self) -> bool {
        self.service == RULES
    }
    pub fn is_model(&self) -> bool {
        self.service == MODEL
    }
    pub fn ready(&self) -> bool {
        self.review.is_some()
            && !self.expired()
            && (!self.is_rules()
                || ((1..=64).contains(&self.tools.len())
                    && (1..=64).contains(&self.directories.len())
                    && self.tools.iter().all(|v| {
                        !v.is_empty() && v.len() <= 128 && !v.chars().any(char::is_control)
                    })))
    }
    pub fn can_choose_directory(&self) -> bool {
        match &self.review {
            Some(Review::Model(v)) => v.summary.can_choose_workspace,
            Some(Review::Rules(v)) => v.can_edit_directories,
            _ => false,
        }
    }
    pub fn tools(&self) -> &[String] {
        &self.tools
    }
    pub fn directories(&self) -> &[PathBuf] {
        &self.directories
    }
    pub fn set_tools(&mut self, text: &str) {
        self.tools = text
            .lines()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .collect();
    }
    pub fn remove_directory(&mut self, index: usize) {
        if self.is_rules() && self.can_choose_directory() && index < self.directories.len() {
            self.directories.remove(index);
        }
    }
    fn authorize(&self) -> Result<(), String> {
        if self.expired() {
            return Err("provider_confirmation_expired".into());
        }
        let account = self.host.check(&self.lease)?;
        self.lease.authorize(&account, self.service, None)
    }
    pub async fn load(mut self) -> Result<Self, String> {
        self.authorize()?;
        let manager = self.host.manager(&self.lease).await?;
        self.review = Some(match &self.input {
            Input::Model(input) => Review::Model(
                manager
                    .review_personal_model(input, self.selected_workspace.clone())
                    .await
                    .map_err(|e| e.code)?,
            ),
            Input::Rules(input) => {
                let review = manager
                    .review_personal_rules(input.agent_id.clone(), input.expected_generation)
                    .await
                    .map_err(|e| e.code)?;
                self.tools = review.tools.clone();
                self.directories = review.directories.clone();
                Review::Rules(review)
            }
            Input::Workspace(input) => {
                let detail = manager
                    .personal_agent_settings(input.agent_id.clone())
                    .await
                    .map_err(|e| e.code)?;
                if detail["agent"]["generation"] != input.expected_generation
                    || detail["settings"]["online"] != true
                {
                    return Err("personal_agent_settings_changed".into());
                }
                Review::Workspace
            }
        });
        self.authorize()?;
        Ok(self)
    }
    pub fn picker_identity(&self) -> (Lease, String) {
        (self.lease.clone(), self.account().to_owned())
    }
    pub async fn choose_directory(mut self, path: PathBuf) -> Result<Self, String> {
        self.authorize()?;
        if !self.can_choose_directory() {
            return Err("personal_execution_device_required".into());
        }
        if self.is_rules() {
            if !self.directories.contains(&path) {
                self.directories.push(path);
            }
        } else {
            self.selected_workspace = Some(path);
            self.review = None;
            self = self.load().await?;
        }
        Ok(self)
    }
    pub async fn preserve_workspace(mut self) -> Result<Self, String> {
        self.authorize()?;
        if !self.is_model() {
            return Err("invalid_management_arguments".into());
        }
        self.selected_workspace = None;
        self.review = None;
        self.load().await
    }
    pub fn details(&self) -> String {
        let tr = crate::i18n::tr;
        match &self.review {
            Some(Review::Model(v)) => {
                let s = &v.summary;
                let folder = self
                    .selected_workspace
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| {
                        tr(if s.workspace == "default" {
                            "Use the execution device’s default workspace"
                        } else {
                            "Keep the current workspace"
                        })
                        .into()
                    });
                let runtime = match &s.model.profile_id {
                    Some(id) => format!("{} · {}", s.model.runtime, id),
                    None => s.model.runtime.clone(),
                };
                let reasoning = if s.model.runtime == "octos" && s.model.reasoning_effort == "none" {
                    tr("Use profile settings")
                } else {
                    &s.model.reasoning_effort
                };
                format!(
                    "{}\n{} · {}\n{}: {}\n{}: {}\n{}: {}",
                    s.agent_id,
                    runtime,
                    s.model.model,
                    tr("Reasoning"),
                    reasoning,
                    tr("Web search"),
                    tr(if s.model.web_search {
                        "Enabled"
                    } else {
                        "Disabled"
                    }),
                    tr("Workspace"),
                    folder
                )
            }
            Some(Review::Rules(v)) => {
                format!(
                    "{}\n{}: {}\n{}: {}\n{}: {}",
                    v.agent_id,
                    tr("Policy revision"),
                    v.policy_revision,
                    tr("Tool rules"),
                    self.tools.len(),
                    tr("Allowed directories"),
                    self.directories.len()
                ) + if self.ready() {
                    ""
                } else {
                    tr(
                        "Choose 1–64 exact tool names and 1–64 existing directories before saving. Tool names must be at most 128 bytes.",
                    )
                }
            }
            Some(Review::Workspace) => match &self.input {
                Input::Workspace(v) => format!(
                    "{}\n{}",
                    v.agent_id,
                    tr(
                        "Prepare the workspace currently configured on this agent’s execution device."
                    )
                ),
                _ => String::new(),
            },
            None => tr("Reading current personal-agent configuration…").into(),
        }
    }
    pub fn words(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self.service {
            MODEL => (
                "Review personal-agent model",
                "Verify this agent’s model and workspace before saving.",
                "Credentials and directory paths remain native. Saving settings does not start an agent or submit a task.",
                "Save model settings",
            ),
            RULES => (
                "Edit personal-agent tool rules",
                "Keep or edit each allowed tool and directory. Existing budget and request-approval settings are preserved.",
                "Allow only the tools and directories this agent needs. The exact rules remain native and are checked again before saving.",
                "Save custom rules",
            ),
            _ => (
                "Prepare personal-agent workspace",
                "Create the configured workspace on this agent’s execution device if needed.",
                "This prepares a directory only. It does not start an agent or submit a task.",
                "Prepare workspace",
            ),
        }
    }
    pub async fn accept(self) -> Result<Value, String> {
        self.authorize()?;
        let manager = self.host.manager(&self.lease).await?;
        self.authorize()?;
        let result = match (self.input, self.review) {
            (Input::Model(_), Some(Review::Model(review))) => {
                manager.save_personal_model(review, self.expires).await
            }
            (Input::Rules(input), Some(Review::Rules(review))) => {
                manager
                    .save_personal_rules(
                        review,
                        input.command_id,
                        self.tools,
                        self.directories,
                        self.expires,
                    )
                    .await
            }
            (Input::Workspace(input), Some(Review::Workspace)) => {
                manager
                    .prepare_personal_workspace(input, self.expires)
                    .await
            }
            _ => return Err("personal_configuration_review_required".into()),
        }
        .map_err(|e| e.code);
        self.host
            .check(&self.lease)
            .and_then(|account| self.lease.authorize(&account, self.service, None))
            .map_err(|_| "management_outcome_unknown".to_owned())?;
        rinx_miniapp_core::bounded_reply(result?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn personal_native_review_rejects_script_paths_rules_and_authority() {
        let model = json!({"commandId":"original","agentId":"agent_a","expectedGeneration":3,"expectedProfileDigest":"a".repeat(64),"runtime":"codex","model":"gpt-6.1-sol","reasoningEffort":"low","webSearch":true,"workspace":"preserve"});
        let rules = json!({"commandId":"original","agentId":"agent_a","expectedGeneration":3});
        assert!(parse_input(MODEL, model.clone()).is_ok());
        assert!(parse_input(RULES, rules.clone()).is_ok());
        assert!(parse_input(WORKSPACE, rules.clone()).is_ok());
        for field in [
            "path",
            "directory",
            "directories",
            "tools",
            "credentialRef",
            "requester",
            "owner",
            "confirmedUntil",
            "approved",
        ] {
            for (service, mut args) in [
                (MODEL, model.clone()),
                (RULES, rules.clone()),
                (WORKSPACE, rules.clone()),
            ] {
                args[field] = json!("injected");
                assert!(parse_input(service, args).is_err(), "{service} {field}");
            }
        }
        let mut bad = model;
        bad["workspace"] = json!("/private/path");
        assert!(parse_input(MODEL, bad).is_err());
        assert!(!permissions(CHOICES).contains(&ManagementPermission::EditPersonalAgentModel));
        assert!(permissions(MODEL).contains(&ManagementPermission::EditPersonalAgentModel));
        assert!(permissions(RULES).contains(&ManagementPermission::EditPersonalAgentPolicy));
    }
}
