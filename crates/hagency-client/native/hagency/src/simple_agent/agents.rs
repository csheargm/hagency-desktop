//! Agent lookup and creation.
use super::{CommandExecutor, failure, step, text};
use crate::native_owner::{Command, NativeError};
use serde_json::json;

/// How setup picks an existing Agent to reuse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentIdentity<'a> {
    /// The owner's own Agent with this display name (Desktop, `agent up`):
    /// one owner, so the name is theirs alone.
    ByName,
    /// A host serving many people keeps Agent ids itself (the concierge):
    /// reuse exactly this id while it is active, otherwise create one.
    /// A display name never selects an Agent here.
    Known(Option<&'a str>),
}

fn active(agent: &serde_json::Value) -> bool {
    matches!(agent["state"].as_str(), Some("active" | "creating"))
}

async fn find_where(
    exec: &dyn CommandExecutor,
    wanted: impl Fn(&serde_json::Value) -> bool,
) -> Result<Option<(String, String)>, NativeError> {
    let listed = step(exec, "agents", Command::Agents).await?;
    Ok(listed["agents"].as_array().and_then(|agents| {
        agents.iter().find(|a| wanted(a) && active(a)).map(|a| (text(a, "id"), text(a, "puppetMxid")))
    }))
}

/// An existing Agent with this display name, so reruns are idempotent.
pub async fn find_agent(exec: &dyn CommandExecutor, name: &str) -> Result<Option<(String, String)>, NativeError> {
    find_where(exec, |a| a["displayName"] == name).await
}

/// `(id, puppet, reused)`: the Agent `identity` selects, or a new one.
pub(super) async fn resolve_agent(
    exec: &dyn CommandExecutor,
    name: &str,
    identity: AgentIdentity<'_>,
) -> Result<(String, String, bool), NativeError> {
    let existing = match identity {
        AgentIdentity::ByName => find_agent(exec, name).await?,
        AgentIdentity::Known(Some(id)) => find_where(exec, |a| a["id"] == id).await?,
        AgentIdentity::Known(None) => None,
    };
    match existing {
        Some((id, puppet)) => Ok((id, puppet, true)),
        None => create_agent(exec, name).await.map(|(id, puppet)| (id, puppet, false)),
    }
}

pub async fn create_agent(exec: &dyn CommandExecutor, name: &str) -> Result<(String, String), NativeError> {
    let input = json!({"displayName":name,"idempotencyKey":crate::daemon::new_request_id()});
    let reply = step(exec, "create-agent", Command::CreateAgent { input }).await?;
    let agent = &reply["creation"]["agent"];
    let id = text(agent, "id");
    if id.is_empty() {
        return Err(failure("invalid_server_response"));
    }
    Ok((id, text(agent, "puppetMxid")))
}
