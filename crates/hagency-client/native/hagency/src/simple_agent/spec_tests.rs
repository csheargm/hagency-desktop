//! The Create agent dialog's choices (model, Thinking) reach the saved model
//! profile; callers that leave them out keep today's defaults.
use super::*;
use std::sync::Mutex;

struct Recorder(Mutex<Vec<Command>>);
impl CommandExecutor for Recorder {
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        let reply = match &command {
            Command::Projects => json!({"projects":[{"id":"p1","spaceId":"!s:x","active":true}]}),
            Command::Agents => json!({"agents":[]}),
            Command::CreateAgent { .. } => json!({"creation":{"agent":{"id":"a1","puppetMxid":"@_hagency_agt_1:x"}},"commandState":"active"}),
            Command::RuntimeProvider { action: ProviderAction::UseLocal, .. } => json!({"credentialRef":"claude-managed:1"}),
            Command::RuntimeProvider { action: ProviderAction::Models, .. } => json!({"models":[{"model":"sonnet"},{"model":"opus"}]}),
            Command::Local { action: LocalAction::AgentModel, .. } => json!({"modelProfile":null}),
            Command::Local { action: LocalAction::AgentPolicy, .. } => json!({"policy":{"revision":1,"policy":{"requests":"AskOwner"}}}),
            Command::EnsureAgentOwnerDirect { .. } => json!({"ownerDirect":{"roomId":"!dm:x"}}),
            _ => json!({}),
        };
        self.0.lock().unwrap().push(command);
        Box::pin(async move { Ok(reply) })
    }
}
fn spec(model: Option<&str>, effort: Option<&str>) -> SimpleAgentSpec {
    SimpleAgentSpec { name: "ann-claude".into(), runtime: Runtime::Claude, model: model.map(str::to_owned), workspace_root: None, reservation: 0, daily_budget: DAILY_TOKEN_BUDGET, high_risk: HIGH_RISK_ASK_OWNER, reasoning_effort: effort.map(str::to_owned) }
}
async fn saved_profile(spec: &SimpleAgentSpec) -> Value {
    let recorder = Recorder(Mutex::new(vec![]));
    create_simple_agent(&recorder, spec).await.unwrap();
    let log = recorder.0.lock().unwrap();
    log.iter().find_map(|command| match command {
        Command::Local { action: LocalAction::SaveAgentModel, input: Some(input), .. } => Some(input["profile"].clone()),
        _ => None,
    }).expect("model profile saved")
}

#[tokio::test]
async fn chosen_model_and_thinking_reach_the_profile() {
    let profile = saved_profile(&spec(Some("opus"), Some("high"))).await;
    assert_eq!(profile["model"], "opus");
    assert_eq!(profile["reasoning_effort"], "high");
    assert_eq!(profile["credential_ref"], "claude-managed:1");
}
#[tokio::test]
async fn callers_without_choices_keep_first_model_and_medium() {
    let profile = saved_profile(&spec(None, None)).await;
    assert_eq!(profile["model"], "sonnet");
    assert_eq!(profile["reasoning_effort"], DEFAULT_REASONING_EFFORT);
}
#[test]
fn provider_commands_follow_the_runtime() {
    assert!(matches!(provider_command(&Runtime::Codex, ProviderAction::Models), Some(Command::Provider { action: ProviderAction::Models })));
    assert!(matches!(provider_command(&Runtime::Claude, ProviderAction::Status), Some(Command::RuntimeProvider { runtime, .. }) if runtime == "claude"));
    assert!(provider_command(&Runtime::OpenAiCompatible { endpoint: "http://x".into() }, ProviderAction::Models).is_none());
}
