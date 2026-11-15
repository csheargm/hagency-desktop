//! The one-call setup against a scripted owner engine.
use super::*;
use std::sync::Mutex;
struct Script(Mutex<Vec<(String, Value)>>);
impl CommandExecutor for Script {
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        let label = format!("{command:?}");
        let mut log = self.0.lock().unwrap();
        let reply = match &command {
            Command::Projects => json!({"projects":[{"id":"p1","spaceId":"!s:x","active":true}]}),
            Command::Agents => json!({"agents":[]}),
            Command::CreateAgent { .. } => json!({"creation":{"agent":{"id":"a1","puppetMxid":"@_hagency_agt_1:x"}},"commandState":"active"}),
            Command::Provider { action: ProviderAction::UseLocal } => json!({"credentialRef":"codex-shared:1"}),
            Command::Provider { action: ProviderAction::Models } => json!({"models":[{"model":"gpt-6.1","credentialRef":"codex-shared:1"}]}),
            Command::Local { action: LocalAction::AgentPolicy, .. } => json!({"policy":{"revision":1,"policy":{"budget":{"limit":"Unlimited","period":"Lifetime"},"requests":"AskOwner","high_risk":"Deny"}}}),
            Command::EnsureAgentOwnerDirect { .. } => json!({"roomId":"!dm:x"}),
            _ => json!({}),
        };
        log.push((label, reply.clone()));
        Box::pin(async move { Ok(reply) })
    }
}
#[tokio::test]
async fn creates_agent_with_existing_project_and_allow_policy() {
    let script = Script(Mutex::new(vec![]));
    let spec = SimpleAgentSpec { name: "bobbot".into(), runtime: Runtime::Codex, model: None, workspace_root: None, reservation: 0, daily_budget: DAILY_TOKEN_BUDGET, high_risk: HIGH_RISK_ASK_OWNER, reasoning_effort: None };
    let report = create_simple_agent(&script, &spec).await.unwrap();
    assert_eq!(report.agent_id, "a1");
    assert_eq!(report.project_id, "p1");
    assert_eq!(report.model, "gpt-6.1");
    assert!(report.started);
    assert_eq!(report.owner_direct_room_id.as_deref(), Some("!dm:x"));
    let log = script.0.lock().unwrap();
    assert!(log.iter().any(|(label, _)| label.contains("SaveAgentPolicy")));
    assert!(log.iter().any(|(label, _)| label.contains("StartAgentService")));
    assert!(!log.iter().any(|(label, _)| label.contains("CreateMatrix")));
}
#[test]
fn runtime_parses_aliases() {
    assert_eq!("Claude-Code".parse::<Runtime>().unwrap(), Runtime::Claude);
    assert_eq!("codex".parse::<Runtime>().unwrap(), Runtime::Codex);
    assert!("octos".parse::<Runtime>().is_err());
}
