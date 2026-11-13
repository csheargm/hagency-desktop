//! Resource-scoped observations and reviewed revocation of saved tool rules.
//! Raw tool requests and Matrix approval verdicts use their private transport.
use super::*;
use hagency_core::approvals::ApprovalChoice;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolApprovalQuery {
    pub resource_id: String,
    #[serde(default)]
    pub engagement_id: String,
    #[serde(default)]
    pub after: String,
}
impl ToolApprovalQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        for value in [&self.engagement_id, &self.after] {
            if !value.is_empty() {
                identifier(value, 128)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolApprovalLookup {
    pub resource_id: String,
    pub approval_id: String,
}
impl ToolApprovalLookup {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        identifier(&self.approval_id, 128)?;
        Ok(())
    }
}

/// Exactly the retained console's seven descriptive fields. No command input,
/// private approval room, credentials, application or provider observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolApproval {
    pub id: String,
    pub state: String,
    pub choice: Option<ApprovalChoice>,
    pub reusable_scope: bool,
    pub expires_at: u64,
    pub engagement_id: String,
    pub project_room_id: Option<String>,
}

/// Human context for an authorized engagement, separate from the retained
/// seven-field status row. No private approval room or tool parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolApprovalSubject {
    pub engagement_id: String,
    pub agent: String,
    pub project_id: String,
    pub project_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApprovalSnapshot {
    pub resource_id: String,
    pub approval: ToolApproval,
    pub subject: ToolApprovalSubject,
    pub observed_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolApprovalPage {
    pub resource_id: String,
    pub engagement_id: String,
    pub approvals: Vec<ToolApproval>,
    pub subjects: Vec<ToolApprovalSubject>,
    pub next_after: Option<String>,
    pub observed_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ToolGrantQuery {
    pub resource_id: String,
    pub engagement_id: String,
    #[serde(default)]
    pub after: String,
}
impl ToolGrantQuery {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.resource_id, 128)?;
        identifier(&self.engagement_id, 128)?;
        if !self.after.is_empty() {
            identifier(&self.after, 128)?;
        }
        Ok(())
    }
}

/// Descriptive rule metadata requires its own consented permission, beyond
/// reading approval status. It never contains the raw request parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRuleDetails {
    pub description: String,
    pub project_id: String,
    pub owner_mxid: String,
    /// Structured form of `description` for localized display. None when the
    /// saved scope can no longer be re-derived exactly; show `description`.
    #[serde(default)]
    pub rule: Option<ToolRule>,
}

/// The facts a saved rule's English `description` states, as fields. They are
/// derived again from the original request by the same pure function that
/// derived the saved scope, and exposed only when the recomputed scope key and
/// description equal the stored ones. They never include the workspace, the
/// task or any other request parameter. Which fields are set depends on the
/// grant's `kind`:
/// - `exact_command`: `command`, `cwd`, and `permissions` when the command
///   asked for extra sandbox permissions;
/// - `network_host`: `host`, `protocol`;
/// - `permission_profile`: `cwd`, `permissions`;
/// - `exact_tool_call`: `tool`, `input`.
/// The rule applies to the grant's `mode` (`task` or `always`) for this Agent
/// and project; that scope is already on the grant itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRule {
    /// `codex`, `claude` or `octos`: the runtime whose request was approved.
    pub runtime: String,
    /// The Claude Code or Octos tool name. Codex requests have none; the
    /// grant `kind` says what they ask for.
    pub tool: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub host: Option<String>,
    /// `http`, `https`, `socks5Tcp` or `socks5Udp`.
    pub protocol: Option<String>,
    /// The exact tool input object of a Claude Code tool call.
    pub input: Option<serde_json::Value>,
    pub permissions: Option<ToolRulePermissions>,
}

/// Extra sandbox permissions a rule allows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolRulePermissions {
    /// `true` for network access to all destinations; null when not asked.
    pub network: Option<bool>,
    /// Paths the Agent may read, sorted.
    pub read: Vec<String>,
    /// Paths the Agent may write, sorted.
    pub write: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolGrantMode {
    Task,
    Always,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolGrant {
    pub id: String,
    pub resource_id: String,
    pub engagement_id: String,
    pub agent: String,
    pub kind: String,
    pub mode: ToolGrantMode,
    pub task_id: Option<String>,
    pub revoked: bool,
    /// Observation only: live execution still validates all original custody.
    pub binding_current: bool,
    /// None means the original scoped description cannot be established.
    pub details: Option<ToolRuleDetails>,
    pub revision: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolGrantPage {
    pub resource_id: String,
    pub engagement_id: String,
    pub grants: Vec<ToolGrant>,
    pub next_after: Option<String>,
    pub observed_at_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RevokeToolGrant {
    pub command_id: String,
    pub resource_id: String,
    pub engagement_id: String,
    pub grant_id: String,
    pub expected_revision: String,
}
impl RevokeToolGrant {
    pub fn validate(&self) -> Result<(), Error> {
        for value in [
            &self.command_id,
            &self.resource_id,
            &self.engagement_id,
            &self.grant_id,
        ] {
            identifier(value, 128)?;
        }
        if self.expected_revision.len() != 64
            || !self
                .expected_revision
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(hagency_core::InvalidInput("invalid tool grant revision").into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokedToolGrant {
    /// Identify the exact reviewed revision without copying private rule text
    /// into the command journal. This receipt is not a fresh observation.
    pub resource_id: String,
    pub engagement_id: String,
    pub grant_id: String,
    pub reviewed_revision: String,
    pub revoked: bool,
}

impl Session {
    pub async fn tool_approvals(
        &self,
        context: &HostContext,
        query: ToolApprovalQuery,
    ) -> Result<ToolApprovalPage, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadToolApprovals)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .tool_approvals(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadToolApprovals)?;
        Ok(result)
    }
    pub async fn tool_approval(
        &self,
        context: &HostContext,
        query: ToolApprovalLookup,
    ) -> Result<ToolApprovalSnapshot, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::ReadToolApprovals)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .tool_approval(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::ReadToolApprovals)?;
        Ok(result)
    }
    pub async fn tool_grants(
        &self,
        context: &HostContext,
        query: ToolGrantQuery,
    ) -> Result<ToolGrantPage, Error> {
        query.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        self.check(context, Permission::InspectToolGrants)?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .tool_grants(&self.current_profile(store)?, query)
                    .await?
            }
        };
        self.check(context, Permission::InspectToolGrants)?;
        Ok(result)
    }
    pub async fn revoke_tool_grant(
        &self,
        context: &HostContext,
        input: RevokeToolGrant,
    ) -> Result<RevokedToolGrant, Error> {
        input.validate()?;
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.check(context, Permission::RevokeToolGrants)?;
        let profile = match self.local_store()? {
            SessionStore::Current(store) => store.identity().0,
        };
        let receipt = CommandReceipt::new(
            context,
            "tool_grant.revoke",
            &input.command_id,
            &serde_json::json!({"profile":profile,"input":input}),
        )?;
        let result = match self.local_store()? {
            SessionStore::Current(store) => {
                store
                    .revoke_tool_grant(&self.current_profile(store)?, input, receipt)
                    .await?
            }
        };
        self.check(context, Permission::RevokeToolGrants)
            .map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }
}
