//! Bind an explicitly connected native Claude OAuth login to the provider's
//! authenticated account UUID. Public CLI email/org metadata only detects a
//! switch; it is never hashed into an account identity. The native credential
//! is read transiently for the fixed provider endpoint and an isolated native
//! child. It is never serialized, logged or persisted by Hagency.
//!
//! An owner whose Claude Code runs on an API key, or on a bearer token for
//! another endpoint, has no account UUID. That credential is read from the
//! `env` block of the connected config folder's settings file, where the CLI
//! itself reads it, and is bound as the endpoint plus a digest of the
//! credential. It is held as briefly and never persisted either.
use super::{ProviderError, ProviderPaths, failure};
use hagency_agent_local::runtime::{ClaudeAccountStatus, Kind, claude_account_status};
use hagency_runtime::claude::account::{AccountBinding, SharedCredential};
use serde_json::{Value, json};
use std::{
    path::Path,
    time::{Duration, Instant},
};

const PROFILE: &str = "https://api.anthropic.com/api/oauth/profile";
const MAX_BYTES: usize = 64 * 1024;

fn unavailable() -> ProviderError {
    failure(409, "provider_identity_unavailable")
}

fn uuid(value: &str) -> Option<String> {
    (value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        }))
    .then(|| value.to_ascii_lowercase())
}

fn subject(account: &ClaudeAccountStatus, profile: &Value) -> Result<String, ProviderError> {
    let (email, organization) = account.oauth_metadata().ok_or_else(unavailable)?;
    let account_id = profile["account"]["uuid"]
        .as_str()
        .and_then(uuid)
        .ok_or_else(unavailable)?;
    let organization_id = profile["organization"]["uuid"]
        .as_str()
        .and_then(uuid)
        .ok_or_else(unavailable)?;
    if profile["account"]["email"].as_str() != Some(email)
        || uuid(organization).as_ref() != Some(&organization_id)
    {
        return Err(failure(409, "provider_account_changed"));
    }
    // Organization is part of the identity: switching the billing organization
    // must not preserve a resource bound to another subscription.
    hagency_core::canonical::encode(&json!({"account":account_id,"organization":organization_id}))
        .map_err(|_| unavailable())
}

// No Debug/Serialize/Clone: this is neither a credential locator nor a proof.
struct Token(String);
impl Token {
    fn parse(bytes: &[u8]) -> Result<Self, ProviderError> {
        if bytes.len() > MAX_BYTES {
            return Err(unavailable());
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
        let oauth = &value["claudeAiOauth"];
        let scopes = oauth["scopes"].as_array().ok_or_else(unavailable)?;
        if !scopes
            .iter()
            .any(|scope| scope.as_str() == Some("user:profile"))
        {
            return Err(unavailable());
        }
        let token = oauth["accessToken"]
            .as_str()
            .filter(|token| {
                !token.is_empty()
                    && token.len() <= 8192
                    && token.bytes().all(|byte| byte.is_ascii_graphic())
            })
            .ok_or_else(unavailable)?;
        Ok(Self(token.into()))
    }
}

/// What the settings file selects in place of a claude.ai sign-in.
// No Debug/Serialize/Clone, like Token.
struct ApiSettings {
    base_url: Option<String>,
    credential: String,
    models: Vec<(String, String)>,
}
impl ApiSettings {
    /// `key` is the CLI's own verdict: an API key, else a bearer token. The
    /// variable for that verdict must be here; one that exists only in the
    /// owner's shell, a helper script or a cloud provider is not taken.
    fn parse(bytes: &[u8], key: bool) -> Result<Self, ProviderError> {
        let value: Value = serde_json::from_slice(bytes).map_err(|_| unavailable())?;
        let env = value["env"].as_object().ok_or_else(unavailable)?;
        let text = |name: &str| -> Result<Option<String>, ProviderError> {
            match env.get(name) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) if !text.is_empty() => Ok(Some(text.clone())),
                _ => Err(unavailable()),
            }
        };
        // The CLI prefers the bearer token when both are set.
        if key && text("ANTHROPIC_AUTH_TOKEN")?.is_some() {
            return Err(unavailable());
        }
        let credential = text(if key {
            "ANTHROPIC_API_KEY"
        } else {
            "ANTHROPIC_AUTH_TOKEN"
        })?
        .ok_or_else(unavailable)?;
        let mut models = Vec::new();
        for name in hagency_runtime::claude::account::MODEL_VARIABLES
            .into_iter()
            .chain([hagency_runtime::claude::account::MAIN_MODEL_VARIABLE])
        {
            if let Some(model) = text(name)? {
                models.push((name.to_owned(), model));
            }
        }
        Ok(Self {
            base_url: text("ANTHROPIC_BASE_URL")?,
            credential,
            models,
        })
    }
}

/// The settings file is the owner's own and often world-readable. It must
/// be a regular file of this user that nobody else can write.
fn settings_file(config: &Path) -> Result<Vec<u8>, ProviderError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
    use std::io::Read;
    const MAX_SETTINGS: usize = 1024 * 1024;
    let directory = cap_std::fs::Dir::open_ambient_dir(config, cap_std::ambient_authority())
        .map_err(|_| unavailable())?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = directory
        .open_with("settings.json", &options)
        .map_err(|_| unavailable())?
        .into_std();
    let meta = file.metadata().map_err(|_| unavailable())?;
    if !meta.is_file() {
        return Err(unavailable());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o022 != 0 || meta.uid() != rustix::process::geteuid().as_raw() {
            return Err(unavailable());
        }
    }
    let mut bytes = Vec::new();
    file.take((MAX_SETTINGS + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    if bytes.len() > MAX_SETTINGS {
        return Err(unavailable());
    }
    Ok(bytes)
}

fn api_credential(
    config: &Path,
    key: bool,
    deadline: Instant,
) -> Result<SharedCredential, ProviderError> {
    let settings = ApiSettings::parse(&settings_file(config)?, key)?;
    SharedCredential::from_api_settings(
        settings.base_url.as_deref(),
        settings.credential,
        key,
        &settings.models,
        deadline,
    )
    .map_err(|_| unavailable())
}

fn file_token(config: &Path) -> Result<Token, ProviderError> {
    use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt, OpenOptionsSyncExt};
    use std::io::Read;
    let directory = cap_std::fs::Dir::open_ambient_dir(config, cap_std::ambient_authority())
        .map_err(|_| unavailable())?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    // Native credentials can be read-only. Opening them must not require
    // write access, follow a replacement symlink or wait on a FIFO.
    let file = directory
        .open_with(".credentials.json", &options)
        .map_err(|_| unavailable())?
        .into_std();
    hagency_store::private::check_handle(&file).map_err(|_| unavailable())?;
    if !file.metadata().map_err(|_| unavailable())?.is_file() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    file.take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| unavailable())?;
    Token::parse(&bytes)
}

#[cfg(target_os = "macos")]
fn keychain_service(config: &Path) -> Result<String, ProviderError> {
    use sha2::{Digest, Sha256};
    use unicode_normalization::UnicodeNormalization;
    // Explicit config selection uses the CLI's NFC-normalized path suffix.
    let path: String = config.to_str().ok_or_else(unavailable)?.nfc().collect();
    let hash = format!("{:x}", Sha256::digest(path.as_bytes()));
    Ok(format!("Claude Code-credentials-{}", &hash[..8]))
}

#[cfg(target_os = "macos")]
async fn token(home: &Path, config: &Path) -> Result<Token, ProviderError> {
    use tokio::io::AsyncReadExt;
    let native_user = match std::env::var("USER") {
        Ok(user) if !user.is_empty() => user,
        _ => {
            let output = tokio::time::timeout(
                Duration::from_secs(2),
                tokio::process::Command::new("/usr/bin/id")
                    .arg("-un")
                    .env_clear()
                    .stdin(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            .map_err(|_| unavailable())?
            .map_err(|_| unavailable())?;
            if !output.status.success() || output.stdout.len() > 512 {
                return Err(unavailable());
            }
            String::from_utf8(output.stdout)
                .map_err(|_| unavailable())?
                .trim()
                .to_owned()
        }
    };
    let user = Some(native_user)
        .filter(|user| {
            !user.is_empty()
                && user
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
        .unwrap_or_else(|| "claude-code-user".into());
    let mut child = tokio::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-a",
            &user,
            "-w",
            "-s",
            &if hagency_agent_local::runtime::claude_uses_default_config(home, config) {
                "Claude Code-credentials".into()
            } else {
                keychain_service(config)?
            },
        ])
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| unavailable())?;
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(unavailable)?
            .take((MAX_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| unavailable())?;
        let status = child.wait().await.map_err(|_| unavailable())?;
        if status.success() {
            return Token::parse(&bytes);
        }
        // Only item-not-found permits the native plaintext fallback. A locked
        // keychain or a denied access must not select a different account.
        if status.code() == Some(44) {
            file_token(config)
        } else {
            Err(unavailable())
        }
    })
    .await
    .map_err(|_| unavailable())?
}

#[cfg(not(target_os = "macos"))]
async fn token(_home: &Path, config: &Path) -> Result<Token, ProviderError> {
    file_token(config)
}

fn client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|_| unavailable())
}

async fn profile(
    client: &reqwest::Client,
    endpoint: &str,
    token: &Token,
) -> Result<Value, ProviderError> {
    let mut response = client
        .get(endpoint)
        .bearer_auth(&token.0)
        .header("Cache-Control", "no-cache")
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|n| n > MAX_BYTES as u64)
    {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if chunk.len() > MAX_BYTES.saturating_sub(bytes.len()) {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

pub(super) async fn observe(
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<AccountBinding, ProviderError> {
    Ok(observe_for_launch(paths, deadline).await?.binding().clone())
}

async fn selected_status(
    executable: &Path,
    paths: &ProviderPaths,
) -> Result<ClaudeAccountStatus, ProviderError> {
    if hagency_agent_local::runtime::claude_uses_default_config(&paths.home, &paths.codex_home) {
        hagency_agent_local::runtime::claude_default_account_status(executable, &paths.home).await
    } else {
        claude_account_status(executable, &paths.home, &paths.codex_home).await
    }
    .map_err(|_| unavailable())
}
pub(in crate::console) async fn observe_for_launch(
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<SharedCredential, ProviderError> {
    let executable = super::external::executable(Kind::Claude)?;
    super::external::verify_reference(&paths.codex_home, &paths.credential_ref)?;
    let proof = observe_credential(
        paths,
        &executable,
        deadline,
        PROFILE,
        token(&paths.home, &paths.codex_home),
    )
    .await?;
    super::external::verify_reference(&paths.codex_home, &paths.credential_ref)?;
    Ok(proof)
}

/// The claude.ai credential is not read at all on this path.
async fn observe_api_settings(
    paths: &ProviderPaths,
    executable: &Path,
    before: &ClaudeAccountStatus,
    key: bool,
    deadline: Instant,
) -> Result<SharedCredential, ProviderError> {
    let observed = api_credential(&paths.codex_home, key, deadline)?;
    if *before != selected_status(executable, paths).await? {
        return Err(failure(409, "provider_account_changed"));
    }
    // A settings file changed between the two reads is another account.
    let again = api_credential(&paths.codex_home, key, deadline)?;
    if again.binding().provider_digest() != observed.binding().provider_digest() {
        return Err(failure(409, "provider_account_changed"));
    }
    Ok(again)
}

/// What a pool is made from: the API key or endpoint token of the connected
/// configuration. A claude.ai sign-in is refused before its credential is
/// looked for, and so is a configuration that is not signed in at all.
pub(in crate::console) async fn observe_api_credential(
    paths: &ProviderPaths,
    deadline: Instant,
) -> Result<SharedCredential, ProviderError> {
    let executable = super::external::executable(Kind::Claude)?;
    super::external::verify_reference(&paths.codex_home, &paths.credential_ref)?;
    let observed = observe_api_credential_with(paths, &executable, deadline).await?;
    super::external::verify_reference(&paths.codex_home, &paths.credential_ref)?;
    Ok(observed)
}
async fn observe_api_credential_with(
    paths: &ProviderPaths,
    executable: &Path,
    deadline: Instant,
) -> Result<SharedCredential, ProviderError> {
    if Instant::now() >= deadline {
        return Err(unavailable());
    }
    let before = selected_status(executable, paths).await?;
    let Some(key) = before.api_credential() else {
        return Err(failure(
            409,
            if before.oauth_metadata().is_some() {
                "provider_login_method_rejected"
            } else {
                "provider_connection_required"
            },
        ));
    };
    observe_api_settings(paths, executable, &before, key, deadline).await
}

/// For display only, never a proof: whether the connected configuration has
/// an API credential (true), a claude.ai sign-in (false) or neither.
pub(in crate::console) async fn sign_in(
    paths: &ProviderPaths,
) -> Result<Option<bool>, ProviderError> {
    let executable = super::external::executable(Kind::Claude)?;
    let status = selected_status(&executable, paths).await?;
    Ok(match status.api_credential() {
        Some(_) => Some(true),
        None if status.oauth_metadata().is_some() => Some(false),
        None => None,
    })
}

#[cfg(test)]
async fn observe_selected(
    paths: &ProviderPaths,
    executable: &Path,
    deadline: Instant,
    endpoint: &str,
    credential: impl std::future::Future<Output = Result<Token, ProviderError>>,
) -> Result<AccountBinding, ProviderError> {
    Ok(
        observe_credential(paths, executable, deadline, endpoint, credential)
            .await?
            .binding()
            .clone(),
    )
}
async fn observe_credential(
    paths: &ProviderPaths,
    executable: &Path,
    deadline: Instant,
    endpoint: &str,
    credential: impl std::future::Future<Output = Result<Token, ProviderError>>,
) -> Result<SharedCredential, ProviderError> {
    if Instant::now() >= deadline {
        return Err(unavailable());
    }
    let before = selected_status(executable, paths).await?;
    if let Some(key) = before.api_credential() {
        return observe_api_settings(paths, executable, &before, key, deadline).await;
    }
    if before.oauth_metadata().is_none() {
        return Err(unavailable());
    }
    let credential = credential.await?;
    // No endpoint, token or subject is accepted from a resource, Matrix event,
    // environment override, model output or a public command argument.
    let observed = profile(&client()?, endpoint, &credential).await?;
    let subject = subject(&before, &observed)?;
    let after = selected_status(executable, paths).await?;
    if before != after {
        return Err(failure(409, "provider_account_changed"));
    }
    let binding = AccountBinding::from_authenticated_profile(
        observed["account"]["uuid"]
            .as_str()
            .ok_or_else(unavailable)?,
        observed["organization"]["uuid"]
            .as_str()
            .ok_or_else(unavailable)?,
        before.oauth_metadata().ok_or_else(unavailable)?.0,
        deadline,
    )
    .map_err(|_| unavailable())?;
    if binding.subject() != subject {
        return Err(unavailable());
    }
    SharedCredential::from_authenticated_login(binding, credential.0).map_err(|_| unavailable())
}

#[cfg(test)]
mod tests;
