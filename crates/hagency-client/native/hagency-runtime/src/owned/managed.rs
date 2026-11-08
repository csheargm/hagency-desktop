//! Evidence from this retained supervisor and its actual Codex stream. No
//! Deserialize/JSON constructor, model input, domain permission or capacity math.
use super::{Cleanup, OwnedSession};
use crate::codex::session::{Error, Phase, QualificationRead};
use hagency_core::canonical;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

pub const MANAGED_DISABLED_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "code_mode_host",
    "code_mode",
    "hooks",
    "plugins",
    "multi_agent",
    "skill_search",
    "skill_mcp_dependency_install",
    "shell_snapshot",
    "view_image",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "browser_annotation_api",
    "computer_use",
    "in_app_browser",
    "goals",
    "image_generation",
    "apps",
    "multi_agent_v2",
    "tool_search",
    "tool_suggest",
    "web_search",
    "web_search_cached",
    "web_search_request",
    "standalone_web_search",
    "memory_tool",
    "remote_plugin",
    "remote_control",
    "memories",
];
pub const MANAGED_INSTRUCTIONS_FILE: &str = "hagency-managed-no-tools-v1.md";
pub const MANAGED_INSTRUCTIONS: &str = "Hagency managed qualification. No tools, workspace actions, web search, personal instructions, or model turns are authorized.\n";
#[derive(Clone)]
pub struct ManagedProcessSource(Arc<AtomicBool>);
impl ManagedProcessSource {
    pub(super) fn new() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
    pub(super) fn retire(&self) {
        self.0.store(false, Ordering::Release);
    }
    pub fn is_live(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ManagedProcessState {
    Qualified,
    Stopped,
    Unknown,
}
pub struct ManagedProcessObservation {
    source: ManagedProcessSource,
    state: ManagedProcessState,
    provider_digest: Option<String>,
    policy_digest: Option<String>,
}
impl ManagedProcessObservation {
    pub fn source(&self) -> &ManagedProcessSource {
        &self.source
    }
    pub fn state(&self) -> ManagedProcessState {
        self.state
    }
    pub fn provider_digest(&self) -> Option<&str> {
        self.provider_digest.as_deref()
    }
    pub fn policy_digest(&self) -> Option<&str> {
        self.policy_digest.as_deref()
    }
}
pub(super) fn account(v: &Value) -> Result<Value, Error> {
    if v["requiresOpenaiAuth"] != true || v["account"]["type"] != "chatgpt" {
        return Err(Error::Policy);
    }
    let plan = v["account"]["planType"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
        .ok_or(Error::Policy)?;
    let email = match &v["account"]["email"] {
        Value::Null if v["account"].get("email").is_some() => None,
        Value::String(s) if !s.is_empty() && s.len() <= 320 && !s.chars().any(char::is_control) => {
            Some(s)
        }
        _ => return Err(Error::Policy),
    };
    Ok(json!({"email":email,"plan":plan}))
}
pub(super) fn quota(v: &Value) -> Result<String, Error> {
    if !v["rateLimits"].is_object() {
        return Err(Error::Policy);
    }
    let subject = v["accountId"]
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        })
        .ok_or(Error::Policy)?;
    canonical::digest(&json!({"provider":"openai.chatgpt","subject":subject}))
        .map_err(|_| Error::Policy)
}
pub(super) fn base_configuration_for_web(
    c: &Value,
    instructions: &Path,
    expected: &str,
    web: bool,
) -> Result<(), Error> {
    let metadata = std::fs::symlink_metadata(instructions).map_err(|_| Error::Policy)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != expected.len() as u64
        || std::fs::read(instructions).map_err(|_| Error::Policy)? != expected.as_bytes()
    {
        return Err(Error::Policy);
    }
    // Only fixed field identifiers are diagnostic; never project effective
    // values, MCP arguments/environment, paths, credentials or account details.
    let checks = [
        ("model_provider", c["model_provider"] == "openai"),
        (
            "chatgpt_base_url",
            c["chatgpt_base_url"] == "https://chatgpt.com/backend-api",
        ),
        (
            "model_providers.openai",
            c["model_providers"]
                .get("openai")
                .is_none_or(Value::is_null),
        ),
        (
            "model_instructions_file",
            c["model_instructions_file"].as_str() == instructions.to_str(),
        ),
        ("instructions", c["instructions"] == ""),
        ("notify", c["notify"].as_array().is_some_and(Vec::is_empty)),
        (
            "model_catalog_json",
            c.get("model_catalog_json").is_none_or(Value::is_null),
        ),
        (
            "web_search",
            c["web_search"] == if web { "live" } else { "disabled" },
        ),
        ("project_doc_max_bytes", c["project_doc_max_bytes"] == 0),
        (
            "skills.include_instructions",
            c["skills"]["include_instructions"] == false,
        ),
        (
            "skills.bundled.enabled",
            c["skills"]["bundled"]["enabled"] == false,
        ),
        ("developer_instructions", c["developer_instructions"] == ""),
        (
            "include_apps_instructions",
            c["include_apps_instructions"] == false,
        ),
        (
            "include_environment_context",
            c["include_environment_context"] == false,
        ),
    ];
    let mut valid = true;
    for (field, matches) in checks {
        if !matches {
            tracing::debug!(field, "managed effective configuration mismatch");
            valid = false;
        }
    }
    if valid { Ok(()) } else { Err(Error::Policy) }
}
fn configuration(c: &Value, instructions: &Path) -> Result<(), Error> {
    configuration_for(c, instructions, MANAGED_INSTRUCTIONS)
}
pub(super) fn configuration_for(
    c: &Value,
    instructions: &Path,
    expected: &str,
) -> Result<(), Error> {
    configuration_for_web(c, instructions, expected, false)
}
pub(super) fn configuration_for_web(
    c: &Value,
    instructions: &Path,
    expected: &str,
    web: bool,
) -> Result<(), Error> {
    configuration_for_tools(c, instructions, expected, web, false)
}
pub(super) fn feature_enabled(name: &str, web: bool, tools: bool) -> bool {
    (web && matches!(name, "standalone_web_search" | "code_mode_host"))
        || (tools && matches!(name, "shell_tool" | "unified_exec" | "code_mode_host"))
}
pub(super) fn configuration_for_tools(
    c: &Value,
    instructions: &Path,
    expected: &str,
    web: bool,
    tools: bool,
) -> Result<(), Error> {
    if tools {
        return crate::codex::validate_native_configuration(c, true, Some(web));
    }
    base_configuration_for_web(c, instructions, expected, web)?;
    if c.get("mcp_servers").is_some_and(|v| {
        !v.is_null()
            && v.as_object()
                .is_none_or(|m| m.len() > 128 || m.values().any(|s| s["enabled"] != false))
    }) {
        return Err(Error::Policy);
    }
    let features = c["features"]
        .as_object()
        .filter(|m| m.len() <= 256)
        .ok_or(Error::Policy)?;
    if features
        .iter()
        .any(|(k, v)| *v != Value::Bool(feature_enabled(k, web, tools)))
    {
        return Err(Error::Policy);
    }
    for key in MANAGED_DISABLED_FEATURES {
        if features.get(*key) != Some(&Value::Bool(feature_enabled(key, web, tools))) {
            return Err(Error::Policy);
        }
    }
    Ok(())
}
/// Bounded policy names observed on this exact metadata stream. These strings
/// provide no account identity, process qualification or domain authority.
pub struct ManagedPolicyOverrides {
    source: ManagedProcessSource,
    values: Vec<String>,
}
impl ManagedPolicyOverrides {
    pub fn source(&self) -> &ManagedProcessSource {
        &self.source
    }
    pub fn values(&self) -> &[String] {
        &self.values
    }
}
fn override_name(name: &str) -> Result<(), Error> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(Error::Policy);
    }
    Ok(())
}
impl OwnedSession {
    /// Inspection only: no account request or model/thread operation. The caller
    /// must observe this original process stopped, apply the restrictive names,
    /// then qualify the final retained process independently.
    pub async fn discover_managed_policy(
        &mut self,
        instructions: &Path,
    ) -> Result<ManagedPolicyOverrides, Error> {
        self.discover_policy_for(instructions, MANAGED_INSTRUCTIONS)
            .await
    }
    pub(super) async fn discover_policy_for(
        &mut self,
        instructions: &Path,
        expected: &str,
    ) -> Result<ManagedPolicyOverrides, Error> {
        self.discover_policy_for_web(instructions, expected, false)
            .await
    }
    pub(super) async fn discover_policy_for_web(
        &mut self,
        instructions: &Path,
        expected: &str,
        web: bool,
    ) -> Result<ManagedPolicyOverrides, Error> {
        self.discover_policy_for_tools(instructions, expected, web, false)
            .await
    }
    pub(super) async fn discover_policy_for_tools(
        &mut self,
        instructions: &Path,
        expected: &str,
        web: bool,
        tools: bool,
    ) -> Result<ManagedPolicyOverrides, Error> {
        if self.phase() != Phase::Ready || !self.managed_source.is_live() {
            return Err(Error::State);
        }
        let result = async {
            let response = self
                .qualification_read(QualificationRead::Configuration)
                .await?;
            let c = &response["config"];
            if tools {
                crate::codex::validate_native_configuration(c, true, Some(web))?;
                self.qualify_ready_owner(Duration::from_millis(100))?;
                return Ok(ManagedPolicyOverrides {
                    source: self.managed_source.clone(),
                    values: Vec::new(),
                });
            }
            base_configuration_for_web(c, instructions, expected, web)?;
            tracing::debug!("managed fixed configuration accepted");
            let mut values = Vec::new();
            if let Some(servers) = c.get("mcp_servers").filter(|v| !v.is_null()) {
                let servers = servers
                    .as_object()
                    .filter(|m| m.len() <= 128)
                    .ok_or(Error::Policy)?;
                for (name, value) in servers {
                    override_name(name).inspect_err(|_| {
                        tracing::debug!("managed MCP name rejected");
                    })?;
                    if !value.is_object() {
                        return Err(Error::Policy);
                    }
                    values.push(format!("mcp_servers.{name}.enabled=false"));
                }
            }
            let features = c["features"]
                .as_object()
                .filter(|m| m.len() <= 256)
                .ok_or(Error::Policy)?;
            for (name, value) in features {
                override_name(name).inspect_err(|_| {
                    tracing::debug!("managed feature name rejected");
                })?;
                // Codex FeatureToml supports bool/configuration tables, and
                // optional entries can serialize as null. Reduce those observed
                // entries to the literal false override;
                // the final process must independently report the resulting
                // boolean false. This never treats a table as already disabled.
                if !value.is_boolean() && !value.is_object() && !value.is_null() {
                    tracing::debug!(is_null = value.is_null(), "managed feature shape rejected");
                    return Err(Error::Policy);
                }
                if value != &Value::Bool(feature_enabled(name, web, tools)) {
                    values.push(format!(
                        "features.{name}={}",
                        feature_enabled(name, web, tools)
                    ));
                }
            }
            for key in MANAGED_DISABLED_FEATURES {
                if features.get(*key) != Some(&Value::Bool(feature_enabled(key, web, tools))) {
                    tracing::debug!(field = *key, "managed fixed feature is not disabled");
                    return Err(Error::Policy);
                }
            }
            self.qualify_ready_owner(Duration::from_millis(100))?;
            Ok(ManagedPolicyOverrides {
                source: self.managed_source.clone(),
                values,
            })
        }
        .await;
        if result.is_err() {
            self.stop();
        }
        result
    }
    pub fn managed_process_source(&self) -> ManagedProcessSource {
        self.managed_source.clone()
    }
    /// Qualifies the actual retained stream, never a prior metadata subprocess.
    /// The caller must still match provider_digest against its canonical ledger.
    pub async fn qualify_managed_no_tools(
        &mut self,
        instructions: &Path,
    ) -> Result<ManagedProcessObservation, Error> {
        // Metadata reads support tool sessions, but a no-tools qualification
        // still requires the original read-only execution policy.
        if !self.shared_settings_match(None) {
            self.stop();
            return Err(Error::Policy);
        }
        if self.phase() != Phase::Ready || !self.managed_source.is_live() {
            return Err(Error::State);
        }
        let observed = async {
            let c = self
                .qualification_read(QualificationRead::Configuration)
                .await?;
            configuration(&c["config"], instructions)?;
            let first = account(&self.qualification_read(QualificationRead::Account).await?)?;
            let quota1 = quota(
                &self
                    .qualification_read(QualificationRead::QuotaAccount)
                    .await?,
            )?;
            let middle = account(&self.qualification_read(QualificationRead::Account).await?)?;
            let quota2 = quota(
                &self
                    .qualification_read(QualificationRead::QuotaAccount)
                    .await?,
            )?;
            let last = account(&self.qualification_read(QualificationRead::Account).await?)?;
            let repeated = self
                .qualification_read(QualificationRead::Configuration)
                .await?;
            configuration(&repeated["config"], instructions)?;
            if first != middle || first != last || quota1 != quota2 {
                return Err(Error::Scope);
            }
            self.qualify_ready_owner(Duration::from_millis(100))?;
            Ok(ManagedProcessObservation {
                source: self.managed_source.clone(),
                state: ManagedProcessState::Qualified,
                provider_digest: Some(quota1),
                policy_digest: Some(
                    canonical::digest(&json!({"policy":"codex_no_tools_v1"}))
                        .map_err(|_| Error::Policy)?,
                ),
            })
        }
        .await;
        if observed.is_err() {
            self.stop();
        }
        observed
    }
    /// Only the original retained supervisor can mint a stopped observation.
    /// Protocol completion, leader PID disappearance or a closed pipe is not it.
    pub fn stop_managed_observation(&mut self) -> ManagedProcessObservation {
        let cleanup = self.stop();
        let stopped = matches!(cleanup,Cleanup::Observed(r) if r.scope.whole_tree_stopped&&r.scope.leader_exited);
        ManagedProcessObservation {
            source: self.managed_source.clone(),
            state: if stopped {
                ManagedProcessState::Stopped
            } else {
                ManagedProcessState::Unknown
            },
            provider_digest: None,
            policy_digest: None,
        }
    }
}
