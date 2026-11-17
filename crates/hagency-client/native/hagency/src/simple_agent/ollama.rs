//! "Ollama (local)" for one-click and dialog creation: models come from the
//! local Ollama catalogue, refused when cloud-backed or without tool support.
use super::{NativeError, failure};
use serde_json::{Value, json};

/// The local Ollama's tool-capable, locally stored models, in the provider
/// `Models` reply shape the Create agent dialog reads.
pub async fn ollama_models() -> Result<Value, String> {
    use hagency_agent_local::codex::{ollama, ollama_verify};
    let local = ollama::LocalModel::new(ollama::DEFAULT_ENDPOINT).map_err(|_| "ollama_unavailable".to_owned())?;
    let models = ollama_verify::list_models(&local).await.map_err(|_| "ollama_unavailable".to_owned())?;
    Ok(json!({"models": models.iter().map(|m| json!({"model":m,"displayName":m})).collect::<Vec<_>>()}))
}
pub(super) async fn local_model(wanted: Option<&str>) -> Result<(String, String), NativeError> {
    use hagency_agent_local::codex::{ollama, ollama_verify};
    let local = ollama::LocalModel::new(ollama::DEFAULT_ENDPOINT).map_err(|_| failure("ollama_unavailable"))?;
    let models = ollama_verify::list_models(&local).await.map_err(|_| failure("ollama_unavailable"))?;
    let model = match wanted {
        Some(w) => models.iter().find(|m| *m == w).cloned().ok_or_else(|| failure("local_model_unavailable"))?,
        None => models.first().cloned().ok_or_else(|| failure("provider_models_unavailable"))?,
    };
    Ok((model, ollama::LOCAL_REFERENCE.into()))
}

