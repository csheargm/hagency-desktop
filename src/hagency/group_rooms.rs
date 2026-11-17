//! A new Agent is private: it serves only its owner's private chat, and the
//! server refuses to bind it into a group Room with a stable code. Only the
//! owner's explicit choice ("Allow in group rooms") changes that; nothing here
//! publishes an Agent on its own, and allowing it is not a pool contribution.
use super::{backend::Service, model::{AgentAction, Command}};
use serde_json::Value;

/// The server's stable refusal for binding a private Agent into a group Room.
pub(crate) const PRIVATE_AGENT_REFUSAL: &str = "private_agent_owner_direct_only";

/// The Agent a bind command names, when it is one.
pub(crate) fn bound_agent(command: &Command) -> Option<&str> {
    match command { Command::BindAgent { agent_id, .. } => Some(agent_id), _ => None }
}

impl Service {
    /// The owner allowed this Agent in group Rooms: record that, then retry the
    /// original bind with its original command key, so the refused attempt and
    /// the retry are one request and never a second Agent or binding.
    pub(crate) async fn allow_group_rooms_then_bind(&self, command: Command) -> Result<Value, String> {
        let agent = bound_agent(&command).ok_or("invalid_arguments")?.to_owned();
        self.execute(Command::Agent { agent_id: agent, action: AgentAction::AllowGroupRooms }).await?;
        self.bind_agent_to_room(command).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_bind_names_the_agent_to_allow() {
        let bind = Command::BindAgent { agent_id: "agt_1".into(), input: serde_json::json!({"idempotencyKey":"k"}) };
        assert_eq!(bound_agent(&bind), Some("agt_1"));
        assert_eq!(bound_agent(&Command::Agents), None);
    }
    #[test]
    fn private_refusal_is_actionable_not_an_uncertain_outcome() {
        let explain = crate::home::mobile_chat_info::agent_binding_error;
        assert_ne!(explain(PRIVATE_AGENT_REFUSAL), explain("server_unavailable"));
        assert!(explain(PRIVATE_AGENT_REFUSAL).contains("Allow in group rooms"));
        assert!(explain(PRIVATE_AGENT_REFUSAL).contains("pool"));
    }
}
