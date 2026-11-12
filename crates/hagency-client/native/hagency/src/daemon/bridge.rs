//! Typed native management across the daemon custody boundary.
//!
//! Native reviews and execution handles stay in the writer. Socket peer UID
//! authenticates the OS user; owner/profile selection and revocable controller
//! and package leases are checked separately on every operation.
mod client;
mod journal;
mod methods;
mod project_setup;
mod reviews;
mod server;
pub use project_setup::ProjectSetupTransport;
#[cfg(feature = "resource-adoption")]
mod grants;
use crate::native_owner::ManagementPermission;
pub use client::{ControllerClient, ManagementClient};
pub use journal::PendingCommand;
#[cfg(feature = "resource-adoption")]
pub use grants::{
    CurrentSelection, DeviceSetupReview, OwnerGrantAccess, OwnerGrantCancellationResult,
    OwnerGrantCancellationReview, OwnerGrantPublicationResult, OwnerGrantPublicationReview,
    OwnerGrantReview, OwnerGrantReviewResult, OwnerOperation, PreparedSharedSuccessor,
    SuccessorAssessment,
};
pub use reviews::{
    AccountLoginAuthorization, PersonalModelReview, PersonalRulesReview, WebConnectionReview,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub(crate) use server::Bridge;

macro_rules! operations {
    ($( $(#[$meta:meta])* $name:ident($($arg:ident: $ty:ty),* $(,)?) -> $result:ty; $mutation:literal; )*) => {
        #[allow(non_camel_case_types)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        pub enum Operation { $($(#[$meta])* $name,)*
            project_setup, review_personal_model, save_personal_model, review_personal_rules, save_personal_rules,
            review_web_connection, review_web_connection_import, review_web_connection_credential,
            apply_web_connection, download_web_connection_registration, account_login_authorization,
            retire_account, create_resource_configuration, connect_provider, prepare_personal_workspace,
            edit_local_resource, enroll_account_resource,
        }
        impl Operation {
            pub fn is_mutation(self) -> bool { match self {
                $($(#[$meta])* Self::$name => $mutation,)*
                Self::review_personal_model | Self::review_personal_rules | Self::review_web_connection
                | Self::review_web_connection_import | Self::review_web_connection_credential
                | Self::download_web_connection_registration | Self::account_login_authorization => false,
                _ => true,
            }}
        }
    }
}
methods::management_methods!(operations);

impl Operation {
    /// Wrapper operations contain both queries and mutations. Decode the same
    /// typed arguments on each side before deciding whether an uncertain write
    /// needs a durable receipt. Queries must remain freely retryable.
    fn mutation_for(self, arguments: &Value) -> Result<bool, crate::native_owner::NativeError> {
        match self {
            Self::central => {
                let (command,): (crate::native_owner::ManagementCommand,) = decode(arguments.clone())?;
                Ok(command.mutation())
            }
            Self::project_setup => {
                let command: project_setup::SetupOperation = decode(arguments.clone())?;
                Ok(command.is_mutation())
            }
            _ => Ok(self.is_mutation()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Package {
    pub digest: String,
    pub instance: u64,
    pub account_epoch: u64,
    pub permissions: Vec<ManagementPermission>,
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
}
impl From<crate::native_owner::ManagementPackage> for Package {
    fn from(p: crate::native_owner::ManagementPackage) -> Self {
        Self {
            digest: p.digest,
            instance: p.instance,
            account_epoch: p.account_epoch,
            permissions: p.permissions,
            expires_at_ms: None,
        }
    }
}
impl Package {
    pub fn with_deadline(
        mut self,
        until: std::time::Instant,
    ) -> Result<Self, crate::native_owner::NativeError> {
        let left = until
            .checked_duration_since(std::time::Instant::now())
            .ok_or_else(|| error(401, "management_scope_changed"))?;
        self.expires_at_ms = Some(
            super::control::now_ms()
                .checked_add(
                    left.as_millis()
                        .try_into()
                        .map_err(|_| error(400, "invalid_management_package"))?,
                )
                .ok_or_else(|| error(400, "invalid_management_package"))?,
        );
        Ok(self)
    }
    pub fn deadline(&self) -> Result<Option<std::time::Instant>, crate::native_owner::NativeError> {
        self.expires_at_ms
            .map(|at| {
                let ms = at
                    .checked_sub(super::control::now_ms())
                    .filter(|v| *v > 0)
                    .ok_or_else(|| error(401, "management_scope_changed"))?;
                std::time::Instant::now()
                    .checked_add(std::time::Duration::from_millis(ms))
                    .ok_or_else(|| error(400, "invalid_management_package"))
            })
            .transpose()
    }
    fn receipt_scope(&self) -> Result<String, crate::native_owner::NativeError> {
        let mut permissions = self.permissions.clone();
        permissions.sort();
        permissions.dedup();
        serde_json::to_string(&(&self.digest, permissions))
            .map_err(|_| error(400, "invalid_management_package"))
    }
    fn native(&self) -> crate::native_owner::ManagementPackage {
        crate::native_owner::ManagementPackage {
            digest: self.digest.clone(),
            instance: self.instance,
            account_epoch: self.account_epoch,
            permissions: self.permissions.clone(),
        }
    }
    fn valid(&self) -> bool {
        self.digest.len() == 64
            && self.digest.bytes().all(|c| c.is_ascii_hexdigit())
            && self.instance > 0
            && self.account_epoch > 0
            && self.permissions.len() <= 128
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum Call {
    Attach {
        origin: String,
        epoch: u64,
    },
    Detach {
        controller: String,
    },
    Management {
        controller: String,
        package: Package,
    },
    Retire {
        controller: String,
        management: String,
    },
    Invoke {
        controller: String,
        management: String,
        operation: Operation,
        arguments: Value,
    },
    Execute {
        controller: String,
        command: Value,
    },
    SignOut {
        controller: String,
    },
    #[cfg(feature = "resource-adoption")]
    OwnerAccess {
        controller: String,
        package: Package,
        operation: OwnerOperation,
        arguments: Value,
    },
    #[cfg(feature = "resource-adoption")]
    RetireOwnerAccess {
        controller: String,
        access: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lease {
    pub id: String,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Failure {
    pub status: u16,
    pub code: String,
    /// Proven terminal native outcome, not merely a persisted bridge error.
    /// Missing metadata, gates and unknown native effects remain unresolved.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}
impl From<crate::native_owner::NativeError> for Failure {
    fn from(e: crate::native_owner::NativeError) -> Self {
        let terminal=e.status==400 && e.code=="invalid_management_arguments";
        Self {
            status: e.status,
            code: e.code,
            terminal,
        }
    }
}
impl Failure {
    fn native(self) -> crate::native_owner::NativeError {
        crate::native_owner::NativeError {
            status: self.status,
            code: self.code,
        }
    }
}
pub(crate) fn terminal_receipt(result:&Result<Value,Failure>)->bool {
    match result {Ok(_)=>true,Err(error)=>error.terminal}
}
pub(crate) fn error(status: u16, code: &str) -> crate::native_owner::NativeError {
    crate::native_owner::NativeError {
        status,
        code: code.into(),
    }
}
pub(crate) fn encode<T: Serialize>(value: T) -> Result<Value, crate::native_owner::NativeError> {
    serde_json::to_value(value).map_err(|_| error(503, "daemon_bridge_encoding_failed"))
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    value: Value,
) -> Result<T, crate::native_owner::NativeError> {
    serde_json::from_value(value).map_err(|_| error(400, "invalid_management_arguments"))
}

fn owner_mutation(command: &crate::native_owner::Command) -> bool {
    use crate::native_owner::{BindingAction, Command, LocalAction, ProviderAction};
    !matches!(
        command,
        Command::Projects
            | Command::ProjectRooms { .. }
            | Command::SpaceCandidates { .. }
            | Command::RoomRoster { .. }
            | Command::Agents
            | Command::RoomAgents
            | Command::Bindings { .. }
            | Command::Devices
            | Command::AgentDetails { .. }
            | Command::AgentConfigurationStatus { .. }
            | Command::AgentOwnerDirectStatus { .. }
            | Command::ScopeServiceState { .. }
            | Command::LoginStatus
            | Command::AgentCommands
            | Command::Creations
            | Command::Creation { .. }
            | Command::Binding {
                action: BindingAction::Status,
                ..
            }
            | Command::Provider {
                action: ProviderAction::Status | ProviderAction::Models
            }
            | Command::RuntimeProvider {
                action: ProviderAction::Status | ProviderAction::Models,
                ..
            }
            | Command::Local {
                action: LocalAction::AgentModel
                    | LocalAction::AgentPolicy
                    | LocalAction::Policy
                    | LocalAction::Runtime
                    | LocalAction::AgentServiceStatus
                    | LocalAction::Approvals,
                input: None,
                ..
            }
    )
}

/// Owner commands that say what should be, not a step to take: this Agent's
/// service runs or is stopped, a coding tool's local account is used or no
/// longer used, the private chat with an Agent exists. Each one reads the
/// current state first and leaves the same state however often it runs (a
/// second start finds the service running; the private chat keeps its own
/// record of how far it got).
///
/// Only these may run again after a refusal that is not final, or after a
/// daemon restart cut the first run short. Without that, a start refused
/// because no model was chosen yet stayed refused for a day under the same
/// request: the Desktop reuses the request for 24 hours and the daemon
/// answered every reuse with `daemon_command_recovery_required`. Commands that
/// create something or decide something (a new Agent, a room binding, a saved
/// policy, an approval) are not listed and keep the original rule.
fn repeatable_owner_command(command: &crate::native_owner::Command) -> bool {
    use crate::native_owner::{Command, LocalAction, ProviderAction};
    matches!(
        command,
        Command::Local {
            action: LocalAction::StartAgentService | LocalAction::StopAgentService,
            ..
        } | Command::Provider {
            action: ProviderAction::UseLocal | ProviderAction::Disconnect
        } | Command::RuntimeProvider {
            action: ProviderAction::UseLocal | ProviderAction::Disconnect,
            ..
        } | Command::EnsureAgentOwnerDirect { .. }
    )
}

#[cfg(test)]
mod mutation_tests {
    use super::*;
    #[test]
    fn only_desired_state_owner_commands_may_run_again() {
        use crate::native_owner::{AgentAction, Command, LocalAction, ProviderAction};
        let local = |action| Command::Local { agent_id: "a".into(), action, input: None, binding_id: None, requester: None };
        for command in [
            local(LocalAction::StartAgentService),
            local(LocalAction::StopAgentService),
            Command::Provider { action: ProviderAction::UseLocal },
            Command::Provider { action: ProviderAction::Disconnect },
            Command::RuntimeProvider { runtime: "claude".into(), action: ProviderAction::UseLocal },
            Command::RuntimeProvider { runtime: "claude".into(), action: ProviderAction::Disconnect },
            Command::EnsureAgentOwnerDirect { agent_id: "a".into() },
        ] {
            assert!(owner_mutation(&command), "{command:?}");
            assert!(repeatable_owner_command(&command), "{command:?}");
        }
        for command in [
            Command::CreateAgent { input: serde_json::json!({}) },
            Command::BindAgent { agent_id: "a".into(), input: serde_json::json!({}) },
            Command::Agent { agent_id: "a".into(), action: AgentAction::Retire },
            Command::AssignAgentToCurrentDevice { agent_id: "a".into(), expected_generation: 1 },
            Command::AgentOwnerDirect { agent_id: "a".into(), room_id: "!r:s".into() },
            Command::PrepareAgentWorkspace { agent_id: "a".into() },
            local(LocalAction::SaveAgentModel),
            local(LocalAction::SaveAgentPolicy),
            local(LocalAction::ResetAgentPolicy),
            local(LocalAction::RuntimeStart),
            local(LocalAction::RuntimeStop),
            local(LocalAction::Decision { approval_id: "p".into() }),
            Command::Provider { action: ProviderAction::Login },
            Command::Provider { action: ProviderAction::Logout },
            Command::RuntimeProvider { runtime: "claude".into(), action: ProviderAction::Logout },
        ] {
            assert!(owner_mutation(&command), "{command:?}");
            assert!(!repeatable_owner_command(&command), "{command:?}");
        }
    }
    #[test]
    fn nested_queries_are_repeatable_and_only_real_writes_need_receipts() {
        for operation in ["session","pools","inbox","agent_budget"] {
            let value=match operation {
                "session"=>serde_json::json!([{"operation":operation}]),
                "pools"=>serde_json::json!([{"operation":operation,"after":null,"capability":null}]),
                "inbox"=>serde_json::json!([{"operation":operation,"after":null,"needs_my_action":false}]),
                _=>serde_json::json!([{"operation":operation,"id":"agent"}]),
            };
            assert!(!Operation::central.mutation_for(&value).unwrap(),"{operation}");
        }
        assert!(!Operation::project_setup.mutation_for(&serde_json::json!({"operation":"progress","project":"project"})).unwrap());
        assert!(Operation::project_setup.mutation_for(&serde_json::json!({"operation":"resume","project":"project"})).unwrap());
        assert!(Operation::central.mutation_for(&serde_json::json!([{"operation":"shared_approval_decide","id":"approval","digest":"digest","decision":"deny"}])).unwrap());
        assert!(Operation::central.mutation_for(&serde_json::json!([{"operation":"future_unknown_operation"}])).is_err());
    }
}
