//! The model profile `configure` saves for a simple Agent.
use super::{DEFAULT_REASONING_EFFORT, SimpleAgentSpec};
use serde_json::{Value, json};

/// The model profile to save: the chosen model and login plus the spec's settings.
pub(super) fn model_input(spec: &SimpleAgentSpec, model: &str, credential: &str) -> Value {
    json!({
        "model": model,
        "credential_ref": credential,
        "workspace_root": spec.workspace_root.clone().unwrap_or_default(),
        "reasoning_effort": spec.reasoning_effort.as_deref().unwrap_or(DEFAULT_REASONING_EFFORT),
        // Web search would leave the machine; a local model runs without it.
        "web_search": credential != hagency_agent_local::codex::ollama::LOCAL_REFERENCE,
    })
}
