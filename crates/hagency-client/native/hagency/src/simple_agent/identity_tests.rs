//! Which Agent setup reuses, and that a missing owner chat fails setup.
use super::*;
use std::sync::Mutex;

/// Lists one active Agent `a9` named "dan-agent"; `owner_direct` is the
/// reply to `EnsureAgentOwnerDirect`.
struct Server {
    log: Mutex<Vec<Command>>,
    owner_direct: Result<Value, &'static str>,
}
impl CommandExecutor for Server {
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        let reply = match &command {
            Command::Projects => Ok(json!({"projects":[{"id":"p1","spaceId":"!s:x","active":true}]})),
            Command::Agents => Ok(json!({"agents":[{"id":"a9","puppetMxid":"@_hagency_agt_9:x","displayName":"dan-agent","state":"active"}]})),
            Command::CreateAgent { .. } => Ok(json!({"creation":{"agent":{"id":"a1","puppetMxid":"@_hagency_agt_1:x"}}})),
            Command::Provider { action: ProviderAction::UseLocal } => Ok(json!({"credentialRef":"codex-shared:1"})),
            Command::Provider { action: ProviderAction::Models } => Ok(json!({"models":[{"model":"gpt-6.1"}]})),
            Command::EnsureAgentOwnerDirect { .. } => self.owner_direct.clone().map_err(failure),
            _ => Ok(json!({})),
        };
        self.log.lock().unwrap().push(command);
        Box::pin(async move { reply })
    }
}
fn server(owner_direct: Result<Value, &'static str>) -> Server {
    Server { log: Mutex::new(vec![]), owner_direct }
}
fn spec() -> SimpleAgentSpec {
    SimpleAgentSpec { name: "dan-agent".into(), runtime: Runtime::Codex, model: None, workspace_root: None, reservation: 0, daily_budget: DAILY_TOKEN_BUDGET, high_risk: HIGH_RISK_DENY, reasoning_effort: None }
}
fn created(server: &Server) -> bool {
    server.log.lock().unwrap().iter().any(|c| matches!(c, Command::CreateAgent { .. }))
}

/// Regression: `@dan:alpha` and `@dan:beta` both asked for "dan-agent" and
/// the second requester was handed the first one's Agent by display name.
#[tokio::test]
async fn known_identity_never_reuses_an_agent_by_display_name() {
    let server = server(Ok(json!({"roomId":"!dm:x"})));
    let report = create_simple_agent_as(&server, &spec(), AgentIdentity::Known(None)).await.unwrap();
    assert_eq!(report.agent_id, "a1");
    assert!(created(&server));
}

#[tokio::test]
async fn known_identity_reuses_exactly_the_stored_agent_id() {
    let server = server(Ok(json!({"roomId":"!dm:x"})));
    let report = create_simple_agent_as(&server, &spec(), AgentIdentity::Known(Some("a9"))).await.unwrap();
    assert_eq!(report.agent_id, "a9");
    assert!(!created(&server));
}

/// Regression: an `EnsureAgentOwnerDirect` failure was dropped with `.ok()`
/// and setup reported a ready Agent with no owner chat for approvals.
#[tokio::test]
async fn owner_chat_failure_fails_setup() {
    let failing = server(Err("matrix_unavailable"));
    let error = create_simple_agent(&failing, &spec()).await.unwrap_err();
    assert_eq!(error.code, "matrix_unavailable");
    let missing = server(Ok(json!({})));
    assert_eq!(create_simple_agent(&missing, &spec()).await.unwrap_err().code, "owner_direct_room_missing");
}
