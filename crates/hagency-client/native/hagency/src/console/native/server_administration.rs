//! Current owner administration uses the server ownership/Matrix policy and
//! the profile-bound native mutation journal.
use super::Command;

pub(super) fn allows(command: &Command) -> bool {
    matches!(
        command,
        Command::Projects
            | Command::ProjectRooms { .. }
            | Command::SpaceCandidates { .. }
            | Command::RoomRoster { .. }
            | Command::AdoptProject { .. }
            | Command::AdoptRoom { .. }
            | Command::ScopeServiceState { .. }
            | Command::SetProjectCreationPolicy { .. }
            | Command::SetRoomCreationPolicy { .. }
            | Command::SetScopeServicePause { .. }
            | Command::Agents
            | Command::RoomAgents
            | Command::AgentDetails { .. }
            | Command::Devices
            | Command::AgentOwnerDirectStatus { .. }
            | Command::Bindings { .. }
            | Command::BindAgent { .. }
            | Command::CreateAgent { .. }
            | Command::Agent { .. }
            | Command::Binding { .. }
            | Command::AgentCommands
            | Command::ResumeAgentCommand { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::native::{AgentAction, BindingAction, LocalAction, ProviderAction};
    use serde_json::json;

    #[test]
    fn server_administration_is_closed_to_local_execution_and_wrappers() {
        for command in [
            Command::AgentConfigurationStatus {
                agent_id: "a".into(),
            },
            Command::ExecutionEnvironment {
                agent_id: "a".into(),
            },
            Command::AgentConfiguration {
                agent_id: "a".into(),
                expected_generation: 1,
                idempotency_key: "original".into(),
                command: Box::new(Command::Projects),
            },
            Command::AssignAgentToCurrentDevice {
                agent_id: "a".into(),
                expected_generation: 1,
            },
            Command::EnsureAgentOwnerDirect {
                agent_id: "a".into(),
            },
            Command::AgentOwnerDirect {
                agent_id: "a".into(),
                room_id: "!r:test".into(),
            },
            Command::CreateMatrix { input: json!({}) },
            Command::Creations,
            Command::Creation {
                id: "original".into(),
            },
            Command::ResumeCreation {
                id: "original".into(),
                room_id: None,
            },
            Command::Local {
                agent_id: "a".into(),
                action: LocalAction::StartAgentService,
                input: None,
                binding_id: None,
                requester: None,
            },
            Command::Provider {
                action: ProviderAction::Status,
            },
            Command::RuntimeProvider {
                runtime: "codex".into(),
                action: ProviderAction::UseLocal,
            },
            Command::PrepareAgentWorkspace {
                agent_id: "a".into(),
            },
            Command::LoginStatus,
            Command::BeginLogin,
            Command::Logout,
        ] {
            assert!(!allows(&command), "{command:?}");
        }
        for command in [
            Command::Projects,
            Command::ProjectRooms {
                project_id: "p".into(),
            },
            Command::AdoptProject {
                space_id: "!s:test".into(),
            },
            Command::AdoptRoom {
                project_id: "p".into(),
                room_id: "!r:test".into(),
            },
            Command::SpaceCandidates { cursor: None },
            Command::RoomRoster {
                project_id: "p".into(),
                room_id: "!r:test".into(),
            },
            Command::Agents,
            Command::RoomAgents,
            Command::Devices,
            Command::AgentDetails {
                agent_id: "a".into(),
            },
            Command::AgentOwnerDirectStatus {
                agent_id: "a".into(),
            },
            Command::Bindings {
                agent_id: "a".into(),
            },
            Command::BindAgent {
                agent_id: "a".into(),
                input: json!({}),
            },
            Command::CreateAgent { input: json!({}) },
            Command::Agent {
                agent_id: "a".into(),
                action: AgentAction::Pause,
            },
            Command::Agent {
                agent_id: "a".into(),
                action: AgentAction::Resume,
            },
            Command::Agent {
                agent_id: "a".into(),
                action: AgentAction::Retire,
            },
            Command::Binding {
                agent_id: "a".into(),
                binding_id: "b".into(),
                action: BindingAction::Leave,
            },
            Command::AgentCommands,
            Command::ResumeAgentCommand {
                id: "original".into(),
            },
        ] {
            assert!(allows(&command), "{command:?}");
        }
    }
}
