//! One-click Agent creation. Desktop runs the same orchestration as
//! `hagency agent up` (hagency-client `simple_agent`), so "Create agent" means
//! Space, Agent, local provider login, model, policy, service start and the
//! private chat in one action.
use super::backend::Service;
use ::hagency::native_owner::{Command, NativeError};
use ::hagency::simple_agent::{self, CommandExecutor, CommandFuture, Runtime, SimpleAgentSpec};
use serde_json::{Value, json};

pub(super) struct ServiceExecutor(pub Service);
impl CommandExecutor for ServiceExecutor {
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        Box::pin(async move {
            match self.0.execute(command.clone()).await {
                // A retained request from an earlier failed run (same Agent,
                // same operation) blocks a fresh one until it is resumed:
                // resend it under the retained idempotency key, which returns
                // that record's outcome instead of a second creation.
                Err(code) if code == "agent_command_recovery_required" => {
                    let journal = self.0.execute(Command::AgentCommands).await.map_err(|code| NativeError { status: 0, code })?;
                    let resumed = retained_key(&journal, &command).and_then(|key| with_key(command.clone(), &key))
                        .ok_or_else(|| NativeError { status: 409, code: "agent_command_recovery_required".into() })?;
                    self.0.execute(resumed).await.map_err(|code| NativeError { status: 0, code })
                }
                Err(code) => Err(NativeError { status: 0, code }),
                Ok(value) => Ok(value),
            }
        })
    }
}

/// The idempotency key of the pending journal record that is this same
/// request (same operation, Agent and input apart from the key), if any. A
/// pending record for a different request never lends its key: that would
/// replay the other request's outcome under this one's name.
fn retained_key(journal: &Value, command: &Command) -> Option<String> {
    let (operation, agent, input) = match command {
        Command::CreateAgent { input } => ("agent.create", None, input),
        Command::BindAgent { agent_id, input } => ("agent.bind", Some(agent_id.as_str()), input),
        _ => return None, // the journal only retains agent.create and agent.bind
    };
    journal["commands"].as_array()?.iter().find(|record| {
        record["state"] == "pending" && record["operation"] == operation
            && agent.is_none_or(|id| record["agentId"] == id || record["input"]["agentId"] == id)
            && without_key(&record["input"]) == without_key(input)
    }).and_then(|record| record["input"]["idempotencyKey"].as_str().map(str::to_owned))
}
fn without_key(input: &Value) -> Value {
    let mut input = input.clone();
    if let Some(fields) = input.as_object_mut() { fields.remove("idempotencyKey"); }
    input
}
fn with_key(command: Command, key: &str) -> Option<Command> {
    let set = |mut input: Value| { input["idempotencyKey"] = json!(key); input };
    Some(match command {
        Command::CreateAgent { input } => Command::CreateAgent { input: set(input) },
        Command::BindAgent { agent_id, input } => Command::BindAgent { agent_id, input: set(input) },
        _ => return None,
    })
}

/// One-click defaults: first model, medium Thinking, daily token budget and
/// high-risk tools on Ask owner. The Create agent dialog starts from these.
pub(super) fn default_spec(name: String, runtime: Runtime) -> SimpleAgentSpec {
    SimpleAgentSpec { name, runtime, model: None, workspace_root: None, reservation: 0, daily_budget: simple_agent::PERSONAL_DAILY_TOKEN_BUDGET, high_risk: simple_agent::HIGH_RISK_ASK_OWNER, reasoning_effort: None }
}

/// Runs the shared setup and answers in the shape of a `CreateAgent` reply so
/// the completion path (refresh, private chat) applies; the spec carries the dialog's model and Thinking.
pub(super) async fn create_spec(service: Service, spec: SimpleAgentSpec) -> Result<Value, String> {
    let report = simple_agent::create_simple_agent(&ServiceExecutor(service), &spec)
        .await
        .map_err(|error| error.code)?;
    Ok(json!({
        "creation":{"agent":{"id":report.agent_id,"puppetMxid":report.puppet_mxid}},
        "commandState":"active",
        "quickSetup":report,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retained_pending_record_supplies_its_key() {
        let journal = json!({"commands":[{"state":"active","operation":"agent.create","input":{"idempotencyKey":"old"}},
            {"state":"pending","operation":"agent.create","input":{"displayName":"x","idempotencyKey":"keep"}}]});
        let command = Command::CreateAgent { input: json!({"displayName":"x","idempotencyKey":"fresh"}) };
        assert_eq!(retained_key(&journal, &command).as_deref(), Some("keep"));
        let Some(Command::CreateAgent { input }) = with_key(command, "keep") else { panic!("create agent") };
        assert_eq!(input["idempotencyKey"], "keep");
        assert!(retained_key(&journal, &Command::Agents).is_none());
    }
    #[test]
    fn another_requests_pending_key_is_never_reused() {
        let journal = json!({"commands":[
            {"state":"pending","operation":"agent.create","input":{"displayName":"alice","idempotencyKey":"alice-key"}},
            {"state":"pending","operation":"agent.bind","agentId":"agt","input":{"roomId":"!a:s","idempotencyKey":"bind-a"}}]});
        let bob = Command::CreateAgent { input: json!({"displayName":"bob","idempotencyKey":"fresh"}) };
        assert!(retained_key(&journal, &bob).is_none(), "Bob must not resume Alice's creation");
        let other_room = Command::BindAgent { agent_id: "agt".into(), input: json!({"roomId":"!b:s","idempotencyKey":"fresh"}) };
        assert!(retained_key(&journal, &other_room).is_none(), "a bind into another room is a different request");
        let same_room = Command::BindAgent { agent_id: "agt".into(), input: json!({"roomId":"!a:s","idempotencyKey":"fresh"}) };
        assert_eq!(retained_key(&journal, &same_room).as_deref(), Some("bind-a"));
    }
    #[test]
    fn one_click_defaults_ask_the_owner_and_leave_model_choices_to_the_runtime() {
        let spec = default_spec("ann-codex".into(), Runtime::Codex);
        assert_eq!(spec.high_risk, simple_agent::HIGH_RISK_ASK_OWNER);
        assert_eq!(spec.daily_budget, simple_agent::PERSONAL_DAILY_TOKEN_BUDGET);
        assert!(spec.model.is_none() && spec.reasoning_effort.is_none());
    }
}
