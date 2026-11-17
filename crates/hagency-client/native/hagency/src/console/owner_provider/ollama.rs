//! "Ollama (local)" provider paths: a dedicated, owner-only Codex home with no
//! login. It never reads or shares the owner's ChatGPT or Claude login.
use super::{ProviderError, ProviderPaths, private_directory};
use std::path::Path;

const FOLDER: &str = "ollama-local";

/// The paths an Ollama-backed Agent runs with, under the owner's directory.
pub(super) fn paths(directory: &Path) -> Result<ProviderPaths, ProviderError> {
    let base = private_directory(&directory.join(FOLDER))?;
    Ok(ProviderPaths {
        home: private_directory(&base.join("home"))?,
        codex_home: private_directory(&base.join("codex"))?,
        credential_ref: hagency_agent_local::codex::ollama::LOCAL_REFERENCE.into(),
        shared: false,
    })
}

/// Before an Ollama-backed Agent starts: the model must be stored on the
/// local Ollama with tool support (not a cloud model under a local alias).
pub(crate) async fn verify_local_model(credential_ref: &str, model: &str) -> Result<(), ProviderError> {
    let Some(local) = hagency_agent_local::codex::ollama::from_reference(credential_ref) else {
        return Ok(());
    };
    hagency_agent_local::codex::ollama_verify::verify_model(&local, model)
        .await
        .map_err(|_| super::failure(409, "local_model_unavailable"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn ollama_paths_are_private_and_outside_any_login() {
        let temp = tempfile::tempdir().unwrap();
        let dir = super::private_directory(&temp.path().canonicalize().unwrap().join("owner")).unwrap();
        let paths = super::paths(&dir).unwrap();
        assert!(!paths.shared);
        assert!(paths.codex_home.starts_with(dir.join("ollama-local")));
        assert_eq!(paths.credential_ref, hagency_agent_local::codex::ollama::LOCAL_REFERENCE);
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&paths.codex_home).unwrap().permissions().mode() & 0o077, 0);
    }
}
