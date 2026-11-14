//! The model profile `configure` saves for a simple Agent.
use super::{DEFAULT_REASONING_EFFORT, SimpleAgentSpec};
use serde_json::{Value, json};

/// `SaveAgentModel` input: the chosen model and login plus the spec's settings.
pub(super) fn model_input(spec: &SimpleAgentSpec, model: &str, credential: &str) -> Value {
    json!({"profile":{
        "model": model,
        "credential_ref": credential,
        "workspace_root": spec.workspace_root.clone().unwrap_or_default(),
        "reasoning_effort": spec.reasoning_effort.as_deref().unwrap_or(DEFAULT_REASONING_EFFORT),
        "web_search": true,
    }})
}
