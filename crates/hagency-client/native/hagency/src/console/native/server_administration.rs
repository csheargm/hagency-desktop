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

/// The owner's own local Agents: runtime, provider connection, workspace,
/// configuration through this device, and the Agent's direct room. These run
/// beside pool hosting. Their state is the profile's own local execution
/// journal, apart from the pool registry.
pub(super) fn allows_local(command: &Command) -> bool {
    matches!(
        command,
        Command::AgentConfigurationStatus { .. }
            | Command::ExecutionEnvironment { .. }
            | Command::AgentConfiguration { .. }
            | Command::AssignAgentToCurrentDevice { .. }
            | Command::EnsureAgentOwnerDirect { .. }
            | Command::AgentOwnerDirect { .. }
            | Command::Local { .. }
            | Command::PrepareAgentWorkspace { .. }
            | Command::Provider { .. }
            | Command::RuntimeProvider { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::console::native::{AgentAction, BindingAction, LocalAction, ProviderAction};
    use serde_json::json;

    /// The fixture opens process-wide owner state, so it runs in a child.
    #[test]
    fn pooled_owner_runs_local_agent_commands_and_keeps_login_and_creation_closed() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "console::native::server_administration::tests::pooled_owner_local_commands_process_child",
                "--exact",
                "--nocapture",
            ])
            .env("HAGENCY_POOLED_LOCAL_FIXTURE", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[tokio::test]
    async fn pooled_owner_local_commands_process_child() {
        use crate::console::native::reconciliation_sources::tests::Fixture;
        use hagency_store::owner_resources::OwnerResourceRegistry;
        if std::env::var("HAGENCY_POOLED_LOCAL_FIXTURE").as_deref() != Ok("1") {
            return;
        }
        let f = Fixture::new().await;
        let registry =
            OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("canonical")).unwrap();
        let f = f.initialize_current(registry.clone()).await;
        let f = f
            .restart_current(registry.clone(), "current_fixture".into())
            .await;
        assert!(f.owner.0.current_profile.get().is_some());
        for command in [
            Command::Provider {
                action: ProviderAction::Status,
            },
            Command::Local {
                agent_id: "a".into(),
                action: LocalAction::StartAgentService,
                input: None,
                binding_id: None,
                requester: None,
            },
            Command::PrepareAgentWorkspace {
                agent_id: "a".into(),
            },
            Command::Agents,
        ] {
            assert!(
                f.owner.admit_owner_command(&command).await.unwrap(),
                "{command:?}"
            );
        }
        for command in [
            Command::LoginStatus,
            Command::BeginLogin,
            Command::Logout,
            Command::Creations,
            Command::CreateMatrix { input: json!({}) },
        ] {
            assert_eq!(
                f.owner.admit_owner_command(&command).await.unwrap_err().code,
                "source_current_profile_only",
                "{command:?}"
            );
        }
        // The start-up recovery of saved local runs reaches the local
        // execution journal in pool mode. Nothing is saved here.
        assert_eq!(
            f.owner.restore_runtime_intents().await.unwrap(),
            json!({"recoveries": []})
        );
        // A local command runs end to end, past admission and authorization.
        let status = f
            .owner
            .execute(Command::Provider {
                action: ProviderAction::Status,
            })
            .await;
        assert!(status.as_ref().is_ok_and(|s| s["state"].is_string()), "{status:?}");
        f.owner.shutdown().await;
        registry.shutdown().await.unwrap();
    }

    #[test]
    fn server_administration_and_local_execution_are_separate_closed_lists() {
        let local = |command: &Command| {
            !matches!(
                command,
                Command::CreateMatrix { .. }
                    | Command::Creations
                    | Command::Creation { .. }
                    | Command::ResumeCreation { .. }
                    | Command::LoginStatus
                    | Command::BeginLogin
                    | Command::Logout
            )
        };
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
            // Local Agents pass beside pools; Matrix login and room creation
            // stay with their own controllers.
            assert_eq!(allows_local(&command), local(&command), "{command:?}");
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
