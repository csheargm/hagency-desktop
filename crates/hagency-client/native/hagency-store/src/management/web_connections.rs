//! Owner-scoped copies of the web client's project-side configuration.
//! Secret-bearing inputs are native-only; no generic JSON or credential read is
//! exposed to scripts. Existing server admission and project approval stay separate.
use super::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebConnection {
    pub id: String,
    pub label: String,
    pub api_base_url: String,
    pub revision: u64,
    pub credential_kind: Option<String>,
    pub has_credential: bool,
    pub awaiting_install: bool,
    pub access_state: String,
    pub checked_at: Option<u64>,
    pub allocated_tokens: Option<u64>,
    pub registration_available: bool,
    /// Explicit compatibility limits, not permission or a grant observation.
    #[serde(default)]
    pub applicability: WebConnectionApplicability,
}
/// The original side budget excludes resource-pool requests
/// (backend-v2.js:9250-9263 and14713-14716). Desktop uses signed-in
/// resource grants; importing legacy fleet machine credentials does not
/// activate the retired poll/ack/update transport or change those grants.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebConnectionApplicability {
    pub allowance_scope: String,
    pub allowance_enforced: bool,
    pub resource_pool_allocations_affected: bool,
    pub management_authority: String,
    pub imported_transport_active: bool,
}
impl Default for WebConnectionApplicability {
    fn default() -> Self {
        Self {
            allowance_scope: "legacy_fleet_requests".into(),
            allowance_enforced: false,
            resource_pool_allocations_affected: false,
            management_authority: "signed_in_matrix".into(),
            imported_transport_active: false,
        }
    }
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebConnectionPage {
    pub items: Vec<WebConnection>,
    pub next_after: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebConnectionResult {
    pub command_id: String,
    pub operation: String,
    pub connection: WebConnection,
}

/// Trusted native command. Deliberately not Deserialize/Serialize/Debug: secret
/// material is neither a mini-app input type nor a saved UI projection.
pub struct WebConnectionCommand {
    pub command_id: String,
    pub id: String,
    pub expected_revision: u64,
    pub change: WebConnectionChange,
}
pub enum WebConnectionChange {
    Create {
        label: String,
        api_base_url: String,
    },
    SetAllowance {
        allocated_tokens: Option<u64>,
    },
    ReplaceCredential {
        credential: Value,
    },
    WithdrawCredential,
    IssueRegistration {
        request: crate::IssueSideRegistrationRequest,
    },
    ImportConfiguration {
        label: String,
        api_base_url: String,
        credential: Value,
        transport: Value,
    },
    /// Constructed only after native whoami validation, never from script input.
    Verification {
        state: String,
        credential_digest: String,
        staged: bool,
    },
}
/// Native-only snapshot for verification/download. No serialization or Debug.
pub struct WebConnectionPrivate {
    pub connection: WebConnection,
    pub credential: Option<crate::Credential>,
    pub pending: Option<crate::Credential>,
    pub registration: Option<String>,
}
pub(crate) fn web_server(value: &str) -> Result<(), Error> {
    if value.is_empty()
        || value.len() > 255
        || value.trim() != value
        || value != value.to_ascii_lowercase()
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
        || value.contains(['/', '?', '#', '@'])
    {
        return Err(hagency_core::InvalidInput("invalid connection server").into());
    }
    palpo_hagency_contract::MatrixUserId::try_from(format!("@hagency:{value}"))
        .map_err(|_| hagency_core::InvalidInput("invalid connection server"))?;
    Ok(())
}
fn label(value: &str) -> Result<(), Error> {
    if value.len() > 256 || value.trim() != value || value.chars().any(char::is_control) {
        return Err(hagency_core::InvalidInput("invalid connection label").into());
    }
    Ok(())
}
pub fn web_connection_credential_digest(credential: &crate::Credential) -> Result<String, Error> {
    use sha2::Digest;
    Ok(format!(
        "{:x}",
        sha2::Sha256::digest(serde_json::to_vec(credential)?)
    ))
}
impl WebConnectionCommand {
    pub fn validate(&self) -> Result<(), Error> {
        identifier(&self.command_id, 128)?;
        web_server(&self.id)?;
        if self.expected_revision > hagency_core::JSON_SAFE_MAX {
            return Err(Error::Capacity);
        }
        match &self.change {
            WebConnectionChange::Create {
                label: l,
                api_base_url,
            }
            | WebConnectionChange::ImportConfiguration {
                label: l,
                api_base_url,
                ..
            } => {
                label(l)?;
                if api_base_url.is_empty()
                    || api_base_url.len() > 2048
                    || api_base_url.chars().any(char::is_control)
                {
                    return Err(hagency_core::InvalidInput("invalid connection URL").into());
                }
            }
            WebConnectionChange::SetAllowance {
                allocated_tokens: Some(tokens),
            } if *tokens > hagency_core::JSON_SAFE_MAX => return Err(Error::Capacity),
            WebConnectionChange::ReplaceCredential { credential } => {
                crate::Credential::from_web_configuration(credential)?;
            }
            WebConnectionChange::IssueRegistration { request } => {
                if request.side != self.id || request.url.len() > 2048 || request.url.is_empty() {
                    return Err(hagency_core::InvalidInput("invalid registration target").into());
                }
            }
            WebConnectionChange::Verification {
                state,
                credential_digest,
                ..
            } => {
                if ![
                    "accepted",
                    "rejected",
                    "unreachable",
                    "unverified",
                    "blocked",
                ]
                .contains(&state.as_str())
                    || credential_digest.len() != 64
                    || !credential_digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(hagency_core::InvalidInput("invalid connection observation").into());
                }
            }
            _ => {}
        }
        if let WebConnectionChange::ImportConfiguration {
            credential,
            transport,
            ..
        } = &self.change
        {
            crate::Credential::from_web_configuration(credential)?;
            if !transport.is_object() || serde_json::to_vec(transport)?.len() > 16 * 1024 {
                return Err(Error::Capacity);
            }
        }
        Ok(())
    }
    pub(crate) fn digest(&self) -> Result<String, Error> {
        let change = match &self.change {
            WebConnectionChange::Create {
                label,
                api_base_url,
            } => serde_json::json!(["create", label, api_base_url]),
            WebConnectionChange::SetAllowance { allocated_tokens } => {
                serde_json::json!(["allowance", allocated_tokens])
            }
            WebConnectionChange::ReplaceCredential { credential } => {
                serde_json::json!(["credential", credential])
            }
            WebConnectionChange::WithdrawCredential => serde_json::json!(["withdraw"]),
            WebConnectionChange::IssueRegistration { request } => serde_json::json!([
                "issue",
                request.side,
                request.url,
                request.registration_id,
                request.sender_localpart,
                request.user_namespace,
                request.exclusive
            ]),
            WebConnectionChange::ImportConfiguration {
                label,
                api_base_url,
                credential,
                transport,
            } => serde_json::json!(["import", label, api_base_url, credential, transport]),
            WebConnectionChange::Verification {
                state,
                credential_digest,
                staged,
            } => serde_json::json!(["verify", state, credential_digest, staged]),
        };
        let bytes = serde_json::to_vec(&(
            self.command_id.as_str(),
            self.id.as_str(),
            self.expected_revision,
            change,
        ))?;
        if bytes.len() > 96 * 1024 {
            return Err(Error::Capacity);
        }
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}
impl Session {
    pub fn web_connection_access(&self, context: &HostContext, write: bool) -> Result<(), Error> {
        let permission = if write {
            Permission::ContributeResources
        } else {
            Permission::ReadConnections
        };
        self.check(context, permission)?;
        let SessionStore::Current(store) = self.local_store()?;
        {
            store.enrollment_registry(&self.current_profile(store)?)?;
            return self.check(context, permission);
        }
    }
    fn web_connection_profile(
        &self,
        context: &HostContext,
        write: bool,
        store: &crate::owner_resources::adoption::CurrentManagementStore,
    ) -> Result<crate::owner_resources::ProfileAuthority, Error> {
        self.web_connection_access(context, write)?;
        let authority = self.authority();
        let original = context.clone();
        let permission = if write {
            Permission::ContributeResources
        } else {
            Permission::ReadConnections
        };
        self.current_profile(store)?
            .with_native_gate(move || authority.authorize(&original, permission).is_ok())
    }
    pub async fn web_connections(
        &self,
        context: &HostContext,
        after: Option<String>,
    ) -> Result<WebConnectionPage, Error> {
        self.web_connection_access(context, false)?;
        if let Some(id) = &after {
            web_server(id)?
        }
        let SessionStore::Current(store) = self.local_store()?;
        {
            let result = store
                .web_connections(&self.web_connection_profile(context, false, store)?, after)
                .await?;
            self.check(context, Permission::ReadConnections)?;
            return Ok(result);
        }
    }
    pub async fn web_connection(
        &self,
        context: &HostContext,
        id: String,
    ) -> Result<WebConnection, Error> {
        self.web_connection_access(context, false)?;
        web_server(&id)?;
        let SessionStore::Current(store) = self.local_store()?;
        {
            let result = store
                .web_connection_private(&self.web_connection_profile(context, false, store)?, id)
                .await?
                .connection;
            self.check(context, Permission::ReadConnections)?;
            return Ok(result);
        }
    }
    pub async fn web_connection_private(
        &self,
        context: &HostContext,
        id: String,
    ) -> Result<WebConnectionPrivate, Error> {
        self.web_connection_access(context, true)?;
        web_server(&id)?;
        let SessionStore::Current(store) = self.local_store()?;
        {
            let result = store
                .web_connection_private(&self.web_connection_profile(context, true, store)?, id)
                .await?;
            self.check(context, Permission::ContributeResources)?;
            return Ok(result);
        }
    }
    pub async fn apply_web_connection(
        &self,
        context: &HostContext,
        input: WebConnectionCommand,
    ) -> Result<WebConnectionResult, Error> {
        let _pending = self.scope.pending.try_acquire().map_err(|_| Error::Busy)?;
        let _command = self.scope.commands.lock().await;
        self.web_connection_access(context, true)?;
        input.validate()?;
        let SessionStore::Current(store) = self.local_store()?;
        {
            let receipt = CommandReceipt::new(
                context,
                "web.connection",
                &input.command_id,
                &serde_json::json!({"profile":store.identity().0,"inputDigest":input.digest()?}),
            )?;
            let result = store
                .apply_web_connection(
                    &self.web_connection_profile(context, true, store)?,
                    input,
                    receipt,
                )
                .await?;
            self.check(context, Permission::ContributeResources)
                .map_err(|_| Error::OutcomeUnknown)?;
            return Ok(result);
        }
    }
    pub async fn web_connection_result(
        &self,
        context: &HostContext,
        command_id: String,
    ) -> Result<Option<WebConnectionResult>, Error> {
        self.web_connection_access(context, true)?;
        identifier(&command_id, 128)?;
        let SessionStore::Current(store) = self.local_store()?;
        {
            let scope = CommandReceipt::new(context, "web.connection", &command_id, &())?;
            let result = store
                .web_connection_result(
                    &self.web_connection_profile(context, true, store)?,
                    scope.actor,
                    command_id,
                )
                .await?;
            self.check(context, Permission::ContributeResources)?;
            return Ok(result);
        }
    }
}
