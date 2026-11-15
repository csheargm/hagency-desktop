//! The policy save fails closed on an unexpected reply.
use super::*;

/// Replies like a healthy server except for the policy read.
struct Server(Value, std::sync::atomic::AtomicBool);
impl CommandExecutor for Server {
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        if matches!(command, Command::Local { action: LocalAction::StartAgentService, .. }) {
            self.1.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let reply = match &command {
            Command::Projects => json!({"projects":[{"id":"p1","spaceId":"!s:x","active":true}]}),
            Command::Agents => json!({"agents":[]}),
            Command::CreateAgent { .. } => json!({"creation":{"agent":{"id":"a1","puppetMxid":"@_hagency_agt_1:x"}}}),
            Command::Provider { action: ProviderAction::UseLocal } => json!({"credentialRef":"codex-shared:1"}),
            Command::Provider { action: ProviderAction::Models } => json!({"models":[{"model":"gpt-6.1"}]}),
            Command::Local { action: LocalAction::AgentPolicy, .. } => self.0.clone(),
            Command::EnsureAgentOwnerDirect { .. } => json!({"roomId":"!dm:x"}),
            _ => json!({}),
        };
        Box::pin(async move { Ok(reply) })
    }
}
fn spec() -> SimpleAgentSpec {
    SimpleAgentSpec { name: "pol".into(), runtime: Runtime::Codex, model: None, workspace_root: None, reservation: 0, daily_budget: DAILY_TOKEN_BUDGET, high_risk: HIGH_RISK_DENY, reasoning_effort: None }
}

#[tokio::test]
async fn unexpected_policy_reply_fails_setup_before_starting() {
    for reply in [json!({}), json!({"policy":{"revision":1,"policy":null}}), json!({"policy":{"policy":{"requests":"AskOwner"}}})] {
        let server = Server(reply, Default::default());
        let error = create_simple_agent(&server, &spec()).await.unwrap_err();
        assert_eq!(error.code, "invalid_server_response");
        assert!(!server.1.load(std::sync::atomic::Ordering::Relaxed), "the Agent must not start");
    }
}
