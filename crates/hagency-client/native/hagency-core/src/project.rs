//! Project-owned names never become filesystem paths or authorization identities.
use crate::{
    InvalidInput,
    allocation::{Ceiling, Declaration, Tokens},
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{L}[\p{L}\p{M}\p{N}_-]*$").expect("static pattern"));
static ASCII_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9_-]{0,63}$").expect("static pattern"));
static NON_STEM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-z0-9_]+").expect("static pattern"));

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentName(String);
impl TryFrom<String> for AgentName {
    type Error = InvalidInput;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        // Bound the input before normalization as well as the normalized output.
        if value.len() > 1024 {
            return Err(InvalidInput("Agent name is too long"));
        }
        let name: String = value.nfc().collect();
        if name.encode_utf16().count() > 64 || !NAME.is_match(&name) {
            return Err(InvalidInput("invalid project Agent name"));
        }
        Ok(Self(name))
    }
}
impl From<AgentName> for String {
    fn from(value: AgentName) -> Self {
        value.0
    }
}
impl AgentName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn identifier(value: &str, max: usize) -> Result<(), InvalidInput> {
    if value.is_empty()
        || value.len() > max
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(InvalidInput("invalid opaque identifier"));
    }
    Ok(())
}
pub fn role(value: &str) -> Result<(), InvalidInput> {
    if !ASCII_NAME.is_match(value) {
        return Err(InvalidInput("invalid role"));
    }
    Ok(())
}
pub fn hash(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}
pub fn public_resource_id(preset: &str) -> String {
    format!("resource_{}", &hash(preset.as_bytes())[..24])
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AgentDefinition {
    pub name: AgentName,
    pub resource_id: String,
    /// Per-agent workspace settings (board #78; the TS agent record,
    /// backend-v2.js:2994): `workspaceMode` is `worktree` or `shared`
    /// (normalize, backend-v2.js:512), `worktreesDir` the agent's own
    /// worktrees root, `worktreeBootstrap` its bootstrap argv (backend-v2.js:516).
    /// TS kept them on an agent record its operator wrote. Here they arrive in
    /// a requester's event, and they name a folder the host creates and a
    /// command it runs outside any sandbox, so a request carrying any of them
    /// is refused (`verify_request`). Kept only so a stored request still reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub worktree_bootstrap: Vec<String>,
}
impl AgentDefinition {
    /// Whether this definition names a workspace mode, worktrees folder or
    /// bootstrap command: the machine owner's settings, never a requester's.
    pub fn carries_workspace_settings(&self) -> bool {
        self.workspace_mode.is_some()
            || self.worktrees_dir.is_some()
            || !self.worktree_bootstrap.is_empty()
    }
    pub fn validate(&self) -> Result<(), InvalidInput> {
        let suffix = self
            .resource_id
            .strip_prefix("resource_")
            .ok_or(InvalidInput("invalid resource ID"))?;
        if suffix.len() != 24
            || !suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvalidInput("invalid resource ID"));
        }
        Ok(())
    }
    pub fn runtime_name(
        &self,
        fleet: &str,
        project: &str,
        request: &str,
    ) -> Result<String, InvalidInput> {
        self.validate()?;
        let suffix = hash(
            &serde_json::to_vec(&[fleet, project, request])
                .map_err(|_| InvalidInput("invalid identity"))?,
        );
        let name = self.name.as_str();
        let stem = if ASCII_NAME.is_match(name) {
            name.chars().take(32).collect::<String>().replace('-', "_")
        } else {
            let normalized = name.nfkd().collect::<String>().to_lowercase();
            NON_STEM
                .replace_all(&normalized, "_")
                .trim_matches('_')
                .chars()
                .take(32)
                .collect::<String>()
        };
        Ok(format!(
            "pa_{}_{}",
            if stem.is_empty() { "agent" } else { &stem },
            &suffix[..16]
        ))
    }
}

/// Provider configuration. `roles` is a derived cache, never an eligibility grant. Execution
/// adapters must independently verify runtime readiness before provisioning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Resource {
    pub preset_id: String,
    pub seat_id: String,
    pub framework: String,
    pub model: String,
    pub provider: Option<String>,
    pub reasoning: Option<String>,
    /// The user's Octos profile an `octos` resource runs. Its model is an
    /// owner-selected configured route, pinned in a private process snapshot.
    /// No other framework names an Octos profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub octos_profile: Option<String>,
    /// Inherited once when a new agent is provisioned. Absence keeps the
    /// original serialized resource and its existing revision byte-compatible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_policy: Option<crate::execution::ExecutionPolicy>,
    #[serde(default)]
    pub roles: Vec<String>,
    pub ceiling: Option<Ceiling>,
    #[serde(default = "yes")]
    pub published: bool,
}
fn yes() -> bool {
    true
}
impl Resource {
    pub fn validate(&self) -> Result<(), InvalidInput> {
        identifier(&self.preset_id, 128)?;
        identifier(&self.seat_id, 128)?;
        identifier(&self.framework, 64)?;
        if let Some(policy) = self.execution_policy {
            policy.validate_for(&self.framework)?;
        }
        if self.model.is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
            || self.roles.len() > 64
        {
            return Err(InvalidInput("invalid resource qualification"));
        }
        for r in &self.roles {
            role(r)?;
        }
        for field in [&self.provider, &self.reasoning].into_iter().flatten() {
            if field.len() > 128 || field.chars().any(char::is_control) {
                return Err(InvalidInput("invalid model profile"));
            }
        }
        if let Some(profile) = &self.octos_profile
            && (self.framework != "octos"
                || profile.is_empty()
                || profile.len() > 64
                || profile.starts_with('-')
                || !profile
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        {
            return Err(InvalidInput("invalid Octos profile"));
        }
        Ok(())
    }
    pub fn id(&self) -> String {
        public_resource_id(&self.preset_id)
    }
    pub fn qualifies(&self, requested_role: &str) -> bool {
        self.published
            && self.provisionable()
            && self.ceiling.as_ref().and_then(|c| c.tokens).is_some()
            && crate::qualification::qualifies(&self.profile(), requested_role, None)
    }
    pub fn profile(&self) -> crate::qualification::ModelProfile {
        crate::qualification::ModelProfile {
            framework: self.framework.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            reasoning: self.reasoning.clone(),
        }
    }
    pub fn provisionable(&self) -> bool {
        matches!(self.framework.as_str(), "claude" | "codex")
            || (self.framework == "octos" && self.octos_profile.is_some())
    }
    pub fn eligible_roles(&self) -> Vec<String> {
        crate::qualification::roles()
            .filter(|role| self.qualifies(role))
            .map(str::to_owned)
            .collect()
    }
    pub fn catalog(&self) -> CatalogResource {
        CatalogResource {
            id: self.id(),
            framework: self.framework.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            reasoning: self.reasoning.clone(),
            tier: crate::qualification::model(&self.profile()).0,
            roles: self.eligible_roles(),
            ceiling: self.ceiling.clone(),
        }
    }
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogResource {
    pub id: String,
    pub framework: String,
    pub model: String,
    pub provider: Option<String>,
    pub reasoning: Option<String>,
    pub tier: Option<crate::qualification::Tier>,
    pub roles: Vec<String>,
    pub ceiling: Option<Ceiling>,
}
/// Operator-only configuration, including resources explicitly withdrawn.
#[derive(Debug, Serialize)]
pub struct ConfiguredResource {
    pub id: String,
    /// The stored definition; its revision never covers an amended maximum.
    pub config: Resource,
    /// The monthly maximum in force: the latest native amendment of an offered
    /// resource, else `config.ceiling`. Display this, never `config.ceiling`.
    #[serde(skip)]
    pub effective_ceiling: Option<Ceiling>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Seat {
    pub id: String,
    pub declaration: Option<Declaration>,
}
impl Seat {
    pub fn validate(&self) -> Result<(), InvalidInput> {
        identifier(&self.id, 128)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngagementState {
    Pending,
    Reserved,
    Active,
    Rejected,
    Revoked,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupState {
    NotRequired,
    Pending,
    Uncertain,
    Complete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Engagement {
    pub id: String,
    pub request_id: String,
    pub project_id: String,
    pub project_room_id: String,
    pub project_name: Option<String>,
    pub agent_name: AgentName,
    pub runtime_name: String,
    pub resource_id: String,
    pub role: String,
    pub requested_tokens: Tokens,
    pub state: EngagementState,
    pub cleanup: CleanupState,
    /// Per-agent workspace settings (board #78; the TS agent record,
    /// backend-v2.js:2994): `workspaceMode` normalizes to `worktree` or
    /// `shared` (backend-v2.js:512), `worktreesDir` is the agent's own
    /// worktrees root, `worktreeBootstrap` its bootstrap argv. Absent in
    /// older projections, which deserialize as the shared defaults.
    #[serde(default)]
    pub workspace_mode: String,
    #[serde(default)]
    pub worktrees_dir: Option<String>,
    #[serde(default)]
    pub worktree_bootstrap: Vec<String>,
    /// TS routeRequest verdict at admission (lib/engagement-store.js:178-232):
    /// why this request did — or did not — auto-join. The routing is recorded,
    /// never used to refuse: the request still lands in the queue.
    #[serde(default)]
    pub route: Option<String>,
    #[serde(default)]
    pub auto_joined: bool,
    /// ADR-186 §A4: the amount the operator granted when it differs from the
    /// ask, or raised by a top-up (§C). `None` means the requested amount is
    /// the allocation — every engagement decided before ADR-186 reads that
    /// way, and the projection then serializes exactly as it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_tokens: Option<Tokens>,
}
impl Engagement {
    /// The tokens this engagement holds: the granted amount when one is set,
    /// otherwise the requested one (`COALESCE(allocated_tokens, tokens)`).
    pub fn allocation(&self) -> Tokens {
        self.allocated_tokens.unwrap_or(self.requested_tokens)
    }
}
