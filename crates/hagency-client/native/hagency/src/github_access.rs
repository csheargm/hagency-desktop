//! One GitHub token per Agent, kept on this device only and never sent to the
//! Hagency server. A product build keeps it in the OS credential store
//! (Keychain on macOS), one item per Agent. A developer's demo build (made
//! with `development-credential-files`, which the desktop's `demo` feature
//! turns on) run with `HAGENCY_DESKTOP_DATA_DIR` keeps it in a 0600 file under
//! that directory instead, so rebuilds do not ask for the Keychain again.
//!
//! The token is never logged: `Token`'s `Debug` hides it and `redact` removes
//! it from any text (an HTTP error, say) before that text is shown.
pub mod check;
#[cfg(test)]
mod tests;

use hagency_agent_local::agent_env::AgentEnvironment;
use hagency_store::credential_vault::{
    Error as StoreError, NativeStore, Purpose, Scope, SecretRef, SecretStore, development,
};
use std::path::PathBuf;
use std::sync::Arc;

/// The data directory override a demo run sets.
pub const DATA_DIR_VARIABLE: &str = "HAGENCY_DESKTOP_DATA_DIR";
/// Where a demo build keeps the files, under the data directory.
pub const DEMO_DIRECTORY: &str = "github-tokens";
/// Stands in for a token in any text that is shown or logged.
pub const REDACTED: &str = "[redacted]";
/// GitHub tokens are about 40–95 characters; anything this long is not one.
const MAX_TOKEN_LEN: usize = 255;
/// Agent tokens are not tied to one app data root: the Agent id is unique.
const INSTALLATION: &str = "00000000000000000000000000000000";

/// A GitHub token. `Debug` never shows it; `expose` is for the HTTP header
/// and the runtime environment only.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);
impl Token {
    /// Checks a pasted token: trimmed, non-empty, printable ASCII, no spaces.
    pub fn parse(pasted: &str) -> Result<Self, &'static str> {
        let token = pasted.trim();
        if token.is_empty() {
            return Err("Paste a GitHub token first.");
        }
        if token.len() > MAX_TOKEN_LEN || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("That does not look like a GitHub token.");
        }
        Ok(Self(token.to_owned()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token([redacted])")
    }
}

/// `text` with every copy of the token replaced by `REDACTED`.
pub fn redact(text: &str, token: &str) -> String {
    if token.is_empty() { text.to_owned() } else { text.replace(token, REDACTED) }
}

/// Where tokens are kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Backend {
    Keychain,
    DemoFile(PathBuf),
}
/// The backend of this build and run.
pub fn backend() -> Backend {
    backend_for(development::COMPILED, std::env::var_os(DATA_DIR_VARIABLE).map(PathBuf::from))
}
/// The file backend needs both the demo build and a demo data directory;
/// a product build (`compiled` false) always uses the Keychain.
pub fn backend_for(compiled: bool, data_dir: Option<PathBuf>) -> Backend {
    match data_dir.filter(|dir| compiled && !dir.as_os_str().is_empty()) {
        Some(dir) => Backend::DemoFile(dir.join(DEMO_DIRECTORY)),
        None => Backend::Keychain,
    }
}

/// The store a demo file backend falls back to: none, so a demo run never
/// asks for the Keychain.
struct NoEarlierStore;
impl SecretStore for NoEarlierStore {
    fn get(&self, _: &SecretRef) -> Result<hagency_store::Zeroizing<Vec<u8>>, StoreError> {
        Err(StoreError::Missing)
    }
    fn put(&self, _: &SecretRef, _: &[u8]) -> Result<(), StoreError> {
        Err(StoreError::Unsupported)
    }
    fn remove(&self, _: &SecretRef) -> Result<(), StoreError> {
        Err(StoreError::Missing)
    }
}

/// The Agents' GitHub tokens on this device.
#[derive(Clone)]
pub struct TokenStore {
    store: Arc<dyn SecretStore>,
}
impl TokenStore {
    pub fn current() -> Self {
        Self::for_backend(&backend())
    }
    pub fn for_backend(backend: &Backend) -> Self {
        match backend {
            Backend::Keychain => Self::with_store(Arc::new(NativeStore::new())),
            Backend::DemoFile(dir) => Self::with_store(Arc::new(development::DevelopmentFileStore::new(
                dir.clone(),
                Arc::new(NoEarlierStore),
            ))),
        }
    }
    pub fn with_store(store: Arc<dyn SecretStore>) -> Self {
        Self { store }
    }
    /// The Agent's token, `None` when it has none.
    pub fn load(&self, agent_id: &str) -> Result<Option<Token>, String> {
        match self.store.get(&reference(agent_id)?) {
            Ok(bytes) => {
                let text = std::str::from_utf8(&bytes).map_err(|_| store_error(StoreError::Invalid))?;
                Token::parse(text).map(Some).map_err(|_| store_error(StoreError::Invalid))
            }
            Err(StoreError::Missing) => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }
    pub fn save(&self, agent_id: &str, token: &Token) -> Result<(), String> {
        self.store.put(&reference(agent_id)?, token.expose().as_bytes()).map_err(store_error)
    }
    /// Removes the Agent's token; removing a missing one is not an error.
    pub fn remove(&self, agent_id: &str) -> Result<(), String> {
        match self.store.remove(&reference(agent_id)?) {
            Ok(()) | Err(StoreError::Missing) => Ok(()),
            Err(error) => Err(store_error(error)),
        }
    }
    /// What the Agent's own runtime starts with: its token as `GH_TOKEN`.
    pub fn runtime_environment(&self, agent_id: &str) -> Result<AgentEnvironment, String> {
        Ok(self.load(agent_id)?.map(|t| AgentEnvironment::github_token(t.expose())).unwrap_or_default())
    }
}

fn reference(agent_id: &str) -> Result<SecretRef, String> {
    if agent_id.is_empty() {
        return Err(store_error(StoreError::Invalid));
    }
    let identity = format!("hagency-agent-github-token-v1\n{agent_id}");
    Scope::new(INSTALLATION, Purpose::GithubToken, identity.as_bytes())
        .and_then(SecretRef::single)
        .map_err(store_error)
}
/// Store errors are fixed codes and carry no secret.
fn store_error(error: StoreError) -> String {
    format!("GitHub token store: {error}")
}

/// The Agent's runtime environment for a start, read off the async threads
/// (the OS store may block). A store failure starts the Agent without a
/// token instead of refusing the start: GitHub access is optional for a run,
/// and the failure is logged (a fixed code, never the token) so the owner's
/// "Check" in the Agent's GitHub access settings shows the same problem.
pub async fn start_environment(agent_id: &str) -> AgentEnvironment {
    let agent = agent_id.to_owned();
    let read = tokio::task::spawn_blocking(move || TokenStore::current().runtime_environment(&agent)).await;
    match read {
        Ok(Ok(environment)) => environment,
        Ok(Err(error)) => {
            tracing::warn!(agent_id, %error, "starting Agent without its GitHub token");
            AgentEnvironment::default()
        }
        Err(error) => {
            tracing::warn!(agent_id, %error, "starting Agent without its GitHub token");
            AgentEnvironment::default()
        }
    }
}
