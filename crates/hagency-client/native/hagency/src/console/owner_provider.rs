//! Dedicated owner provider onboarding. Codex owns OAuth tokens in its OS
//! keyring; this host only exposes a login URL and verified account status.
mod drain;
mod trust_config;
pub(in crate::console) mod managed;
pub(crate) mod external;
pub(super) mod identity;
mod claude_identity;
use super::{
    Console, body, console, cookie, current, owned_agents::journal_profile_directory, recheck,
    server_login::OwnerOperation,
};
use crate::console::lifecycle::TaskCustody;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::Mutex,
};

#[derive(Debug, thiserror::Error)]
#[error("{code}")]
pub(crate) struct ProviderError {
    pub status: u16,
    pub code: &'static str,
}
fn failure(status: u16, code: &'static str) -> ProviderError {
    ProviderError { status, code }
}
fn private_directory(path: &Path) -> Result<PathBuf, ProviderError> {
    hagency_store::private::directory(path)
        .map_err(|_| failure(503, "provider_directory_unavailable"))?;
    let canonical = path
        .canonicalize()
        .map_err(|_| failure(503, "provider_directory_unavailable"))?;
    if canonical != path {
        return Err(failure(503, "provider_directory_not_canonical"));
    }
    Ok(canonical)
}
/// Locator of Codex's keyring entry (Codex Auth / cli|<path hash>). This
/// neither reads credentials nor treats the locator as proof of login.
pub(crate) fn credential_reference(home: &Path) -> Result<String, ProviderError> {
    private_directory(home)?;
    if !matches!(std::fs::symlink_metadata(home.join("auth.json")), Err(e) if e.kind()==std::io::ErrorKind::NotFound)
        || !trust_config::accepted(home)
    {
        return Err(failure(409, "provider_file_credentials_or_config_rejected"));
    }
    let digest = Sha256::digest(home.to_string_lossy().as_bytes());
    Ok(format!(
        "keychain:codex-home:{}",
        &format!("{digest:x}")[..16]
    ))
}
pub(crate) fn executable() -> Result<PathBuf, ProviderError> {
    let candidates = if let Some(configured) = std::env::var_os("HAGENCY_CODEX_BINARY") {
        vec![PathBuf::from(configured)]
    } else {
        let mut paths = vec![PathBuf::from(
            "/Applications/ChatGPT.app/Contents/Resources/codex-cli/CodexCLI.app/Contents/MacOS/codex",
        )];
        if let Some(path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&path).map(|p| p.join("codex")));
        }
        paths
    };
    candidates
        .into_iter()
        .find_map(|p| {
            p.is_absolute()
                .then(|| p.canonicalize().ok())
                .flatten()
                .filter(|p| p.is_file())
        })
        .ok_or(failure(503, "codex_binary_unavailable"))
}
#[derive(Clone)]
pub(crate) struct ProviderPaths {
    pub home: PathBuf,
    pub codex_home: PathBuf,
    pub credential_ref: String,
    pub shared: bool,
}
fn trusted_local_home() -> Result<PathBuf, ProviderError> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".codex")))
        .ok_or(failure(503, "local_codex_home_unavailable"))?;
    let home = home
        .canonicalize()
        .map_err(|_| failure(503, "local_codex_home_unavailable"))?;
    if !home.is_dir() {
        return Err(failure(503, "local_codex_home_unavailable"));
    }
    Ok(home)
}
pub(crate) fn selected_reference(home: &Path, shared: bool) -> Result<String, ProviderError> {
    if !shared {
        return credential_reference(home);
    }
    if home != trusted_local_home()? {
        return Err(failure(409, "provider_profile_mismatch"));
    }
    Ok(format!(
        "codex-managed:shared-home:{:x}",
        Sha256::digest(home.to_string_lossy().as_bytes())
    ))
}
fn choice_path(paths: &ProviderPaths) -> PathBuf {
    paths.home.parent().unwrap().join("provider-choice.json")
}
pub(crate) fn paths(
    root: &Path,
    origin: &str,
    issuer: &str,
    subject: &str,
    owner: &str,
) -> Result<ProviderPaths, ProviderError> {
    // Provider metadata lives in the authenticated private namespace. Looking
    // it up must not open a personal budget ledger, which may be durably fenced
    // after canonical resource adoption. Personal execution opens its own ledger.
    let directory = journal_profile_directory(root, origin, issuer, subject, owner)
        .map_err(|_| failure(503, "provider_directory_unavailable"))?
        .canonicalize()
        .map_err(|_| failure(503, "provider_directory_unavailable"))?;
    let home = private_directory(&directory.join("provider-home"))?;
    let choice = directory.join("provider-choice.json");
    let shared = match std::fs::symlink_metadata(&choice) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => return Err(failure(503, "provider_choice_unavailable")),
        Ok(_) => {
            let file = hagency_store::private::open(&choice, false)
                .map_err(|_| failure(503, "provider_choice_unavailable"))?;
            use std::io::Read;
            let mut raw = Vec::new();
            file.take(8193)
                .read_to_end(&mut raw)
                .map_err(|_| failure(503, "provider_choice_unavailable"))?;
            if raw.len() > 8192 {
                return Err(failure(503, "provider_choice_unavailable"));
            }
            let v: Value = serde_json::from_slice(&raw)
                .map_err(|_| failure(503, "provider_choice_unavailable"))?;
            if v != json!({"version":1,"home":trusted_local_home()?.to_string_lossy()}) {
                return Err(failure(409, "provider_profile_mismatch"));
            }
            true
        }
    };
    let codex_home = if shared {
        trusted_local_home()?
    } else {
        private_directory(&directory.join("codex-home"))?
    };
    let credential_ref = selected_reference(&codex_home, shared)?;
    Ok(ProviderPaths {
        home,
        codex_home,
        credential_ref,
        shared,
    })
}
pub(super) fn prepare_workspace(
    paths: &ProviderPaths,
    agent: &str,
) -> Result<PathBuf, ProviderError> {
    if agent.is_empty()
        || agent.len() > 128
        || !agent
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
    {
        return Err(failure(400, "invalid_arguments"));
    }
    let owner_dir = paths
        .home
        .parent()
        .ok_or(failure(503, "provider_directory_unavailable"))?;
    let workspaces = private_directory(&owner_dir.join("workspaces"))?;
    private_directory(&workspaces.join(agent))
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProviderStatus {
    pub state: &'static str,
    pub authenticated: bool,
    pub credential_ref: String,
    pub credential_store: &'static str,
    pub auth_url: Option<String>,
    pub strict_token_cap: bool,
    pub native_tools: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ProviderModel>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub shared: bool,
    pub credential_source: &'static str,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProviderModel {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub is_default: bool,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ProviderEffort>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProviderEffort {
    pub reasoning_effort: String,
    pub description: String,
}
fn model_page(value: &Value) -> Result<Vec<ProviderModel>, ProviderError> {
    let data = value["data"]
        .as_array()
        .filter(|a| a.len() <= 100)
        .ok_or(failure(502, "codex_protocol_mismatch"))?;
    let mut models = Vec::new();
    for item in data {
        let hidden = item["hidden"]
            .as_bool()
            .ok_or(failure(502, "codex_protocol_mismatch"))?;
        if hidden {
            continue;
        }
        let m: ProviderModel = serde_json::from_value(item.clone())
            .map_err(|_| failure(502, "codex_protocol_mismatch"))?;
        if [&m.id, &m.model]
            .iter()
            .any(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control))
            || m.display_name.is_empty()
            || m.display_name.len() > 256
            || m.description.len() > 8192
            || m.supported_reasoning_efforts.len() > 16
            || m.supported_reasoning_efforts.is_empty()
            || m.supported_reasoning_efforts.iter().any(|e| {
                e.reasoning_effort.is_empty()
                    || e.reasoning_effort.len() > 32
                    || e.description.len() > 4096
            })
            || !m
                .supported_reasoning_efforts
                .iter()
                .any(|e| e.reasoning_effort == m.default_reasoning_effort)
        {
            return Err(failure(502, "codex_protocol_mismatch"));
        }
        models.push(m);
    }
    Ok(models)
}
fn rpc_rejection(value: &Value) -> ProviderError {
    let message = value["message"].as_str().unwrap_or("").to_ascii_lowercase();
    let code = if message.contains("keyring")
        || message.contains("keychain")
        || message.contains("credential store")
    {
        "provider_keyring_unavailable"
    } else if message.contains("bind") && (message.contains("address") || message.contains("port"))
    {
        "provider_login_callback_unavailable"
    } else if value["code"].as_i64() == Some(-32602) || value["code"].as_i64() == Some(-32601) {
        "codex_protocol_mismatch"
    } else {
        "codex_provider_request_rejected"
    };
    failure(502, code)
}
struct Process {
    child: drain::TrackedChild,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next: u64,
    login_id: Option<String>,
    auth_url: Option<String>,
    until: Option<Instant>,
    reference: String,
}
impl Process {
    #[cfg(test)]
    async fn spawn(paths: &ProviderPaths, executable: &Path) -> Result<Self, ProviderError> {
        Self::spawn_inner(paths, executable, false, &TaskCustody::default()).await
    }
    #[cfg(test)]
    async fn spawn_identity(
        paths: &ProviderPaths,
        executable: &Path,
    ) -> Result<Self, ProviderError> {
        Self::spawn_inner(paths, executable, true, &TaskCustody::default()).await
    }
    async fn spawn_inner(
        paths: &ProviderPaths,
        executable: &Path,
        identity: bool,
        tasks: &TaskCustody,
    ) -> Result<Self, ProviderError> {
        tasks.check_open().map_err(|_| failure(409, "provider_closing"))?;
        // These checks cannot start a child. A missing resolved executable or
        // rejected namespace must not poison later shutdown as an unknown
        // process lifetime. Keep checked custody around the shared probe and
        // actual spawn, whose cancellation/errors can require cleanup evidence.
        if executable.is_absolute() && !executable.is_file() {
            return Err(failure(503, "codex_provider_unavailable"));
        }
        selected_reference(&paths.codex_home, paths.shared)?;
        let creation = tasks
            .checked()
            .map_err(|_| failure(409, "provider_closing"))?;
        let shared_overrides = if paths.shared {
            hagency_agent_local::codex::sealed_shared_overrides(
                executable,
                &paths.home,
                &paths.codex_home,
            )
            .await
            .map_err(|_| failure(409, "local_codex_configuration_unsupported"))?
        } else {
            vec![]
        };
        let mut command = tokio::process::Command::new(executable);
        command
            .args([
                "app-server",
                "--listen",
                "stdio://",
                "-c",
                if paths.shared {
                    "cli_auth_credentials_store=\"auto\""
                } else {
                    "cli_auth_credentials_store=\"keyring\""
                },
            ])
            .args([
                "-c",
                "project_doc_max_bytes=0",
                "-c",
                "skills.include_instructions=false",
                "-c",
                "skills.bundled.enabled=false",
                "-c",
                "web_search=\"disabled\"",
                "-c",
                "developer_instructions=\"\"",
                "-c",
                "include_apps_instructions=false",
                "-c",
                "include_environment_context=false",
            ])
            .env_clear()
            .env("HOME", hagency_platform::codex_process_home(&paths.home).map_err(|_| failure(503, "provider_os_home_unavailable"))?)
            .env("CODEX_HOME", &paths.codex_home)
            .current_dir(&paths.home)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        if identity {
            command.args([
                "-c",
                "model_provider=\"openai\"",
                "-c",
                "chatgpt_base_url=\"https://chatgpt.com/backend-api\"",
            ]);
        }
        if paths.shared {
            for value in hagency_agent_local::codex::shared_base_overrides(&paths.home)
                .map_err(|_| failure(409, "local_codex_configuration_unsupported"))?
            {
                command.arg("-c").arg(value);
            }
        }
        for value in &shared_overrides {
            command.arg("-c").arg(value);
        }
        if let Some(path) = std::env::var_os("PATH") {
            command.env("PATH", path);
        }
        for feature in [
            "shell_tool",
            "unified_exec",
            "code_mode_host",
            "code_mode",
            "hooks",
            "plugins",
            "multi_agent",
            "skill_search",
            "skill_mcp_dependency_install",
            "shell_snapshot",
            "view_image",
            "browser_use",
            "browser_use_external",
            "browser_use_full_cdp_access",
            "browser_annotation_api",
            "computer_use",
            "in_app_browser",
            "goals",
            "image_generation",
            "apps",
            "multi_agent_v2",
            "tool_search",
            "tool_suggest",
            "web_search",
            "web_search_cached",
            "web_search_request",
            "standalone_web_search",
            "memory_tool",
        ] {
            command.arg("--disable").arg(feature);
        }
        let child = command
            .spawn()
            .map_err(|_| failure(503, "codex_provider_unavailable"))?;
        let mut child = drain::TrackedChild::new(child, tasks);
        creation.complete();
        let input = child
            .stdin
            .take()
            .ok_or(failure(503, "codex_provider_unavailable"))?;
        let output = BufReader::new(
            child
                .stdout
                .take()
                .ok_or(failure(503, "codex_provider_unavailable"))?,
        );
        let mut p = Self {
            child,
            input,
            output,
            next: 1,
            login_id: None,
            auth_url: None,
            until: None,
            reference: paths.credential_ref.clone(),
        };
        p.rpc("initialize",json!({"clientInfo":{"name":"hagency-client","title":"Hagency Client","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":false}})).await?;
        p.write(json!({"method":"initialized"})).await?;
        if paths.shared || identity {
            let c = p.rpc("config/read", json!({"includeLayers":false})).await?;
            if paths.shared {
                hagency_agent_local::codex::verify_shared_configuration(&c["config"], &paths.home)
                    .map_err(|_| failure(409, "local_codex_configuration_unsupported"))?;
            }
            if identity {
                identity::configuration(&c["config"])?;
            }
        }
        Ok(p)
    }
    async fn write(&mut self, value: Value) -> Result<(), ProviderError> {
        let mut line =
            serde_json::to_vec(&value).map_err(|_| failure(503, "codex_protocol_mismatch"))?;
        line.push(b'\n');
        self.input
            .write_all(&line)
            .await
            .map_err(|_| failure(503, "codex_provider_unavailable"))?;
        self.input
            .flush()
            .await
            .map_err(|_| failure(503, "codex_provider_unavailable"))
    }
    async fn frame(&mut self) -> Result<Value, ProviderError> {
        let mut frame = Vec::new();
        loop {
            let available = self
                .output
                .fill_buf()
                .await
                .map_err(|_| failure(503, "codex_provider_unavailable"))?;
            if available.is_empty() {
                return Err(failure(503, "codex_provider_unavailable"));
            }
            let end = available.iter().position(|b| *b == b'\n').map(|n| n + 1);
            let n = end.unwrap_or(available.len());
            if frame.len() + n > 262144 {
                return Err(failure(502, "codex_protocol_mismatch"));
            }
            frame.extend_from_slice(&available[..n]);
            self.output.consume(n);
            if end.is_some() {
                return serde_json::from_slice(&frame)
                    .map_err(|_| failure(502, "codex_protocol_mismatch"));
            }
        }
    }
    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value, ProviderError> {
        let id = self.next;
        self.next += 1;
        self.write(json!({"id":id,"method":method,"params":params}))
            .await?;
        tokio::time::timeout(Duration::from_secs(20),async{
            for _ in 0..128{
                let frame=self.frame().await?;
                if frame["id"]==id && frame.get("method").is_none(){
                    if let Some(error)=frame.get("error"){return Err(rpc_rejection(error));}
                    return frame.get("result").cloned().ok_or(failure(502,"codex_protocol_mismatch"));
                }
                if frame.get("id").is_some(){self.write(json!({"id":frame["id"],"error":{"code":-32601,"message":"Provider onboarding does not execute tools"}})).await?;}
                if frame["method"]=="account/login/completed" && frame["params"]["loginId"].as_str()==self.login_id.as_deref(){self.login_id=None;self.auth_url=None;self.until=None;}
            }Err(failure(502,"codex_protocol_mismatch"))
        }).await.map_err(|_|failure(504,"codex_provider_timeout"))?
    }
    async fn cancel(&mut self) -> Result<(), ProviderError> {
        if let Some(id) = self.login_id.take() {
            self.rpc("account/login/cancel", json!({"loginId":id}))
                .await?;
        }
        self.auth_url = None;
        self.until = None;
        Ok(())
    }
    async fn status(&mut self, paths: &ProviderPaths) -> Result<ProviderStatus, ProviderError> {
        selected_reference(&paths.codex_home, paths.shared)?;
        if self.until.is_some_and(|until| until <= Instant::now()) {
            self.cancel().await?;
        }
        let value = self
            .rpc("account/read", json!({"refreshToken":false}))
            .await?;
        let account = value["account"]["type"].as_str();
        let authenticated = value["requiresOpenaiAuth"] == true && account == Some("chatgpt");
        if value["account"].is_object() && !authenticated {
            return Err(failure(409, "provider_login_method_rejected"));
        }
        if authenticated {
            self.login_id = None;
            self.auth_url = None;
            self.until = None;
        }
        // A managed policy forcing file storage must not silently defeat keyring-only onboarding.
        selected_reference(&paths.codex_home, paths.shared)?;
        Ok(ProviderStatus {
            state: if authenticated {
                "authenticated"
            } else if self.login_id.is_some() {
                "signing_in"
            } else {
                "signed_out"
            },
            authenticated,
            credential_ref: self.reference.clone(),
            credential_store: if paths.shared {
                "codex_managed_local"
            } else {
                "codex_os_keyring"
            },
            auth_url: self.auth_url.clone(),
            strict_token_cap: false,
            native_tools: true,
            models: None,
            default_model: None,
            workspace: None,
            shared: paths.shared,
            credential_source: if paths.shared {
                "local_codex"
            } else {
                "dedicated"
            },
        })
    }
    async fn models(&mut self, paths: &ProviderPaths) -> Result<ProviderStatus, ProviderError> {
        let mut status = self.status(paths).await?;
        let mut cursor: Option<String> = None;
        let mut seen = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut models = Vec::new();
        for _ in 0..8 {
            let value = self
                .rpc(
                    "model/list",
                    json!({"cursor":cursor,"limit":100,"includeHidden":false}),
                )
                .await?;
            for model in model_page(&value)? {
                if !ids.insert(model.model.clone()) || models.len() >= 256 {
                    return Err(failure(502, "codex_model_catalog_unavailable"));
                }
                models.push(model);
            }
            match value.get("nextCursor") {
                Some(Value::Null) => {
                    status.default_model = models
                        .iter()
                        .find(|m| m.is_default)
                        .map(|m| m.model.clone());
                    status.models = Some(models);
                    return Ok(status);
                }
                Some(Value::String(next))
                    if !next.is_empty() && next.len() <= 1024 && seen.insert(next.clone()) =>
                {
                    cursor = Some(next.clone())
                }
                _ => return Err(failure(502, "codex_model_catalog_unavailable")),
            }
        }
        Err(failure(502, "codex_model_catalog_unavailable"))
    }
    async fn start(&mut self, paths: &ProviderPaths) -> Result<ProviderStatus, ProviderError> {
        let status = self.status(paths).await?;
        if paths.shared {
            return Err(failure(409, "shared_codex_login_is_managed_locally"));
        }
        if status.authenticated || self.login_id.is_some() {
            return Ok(status);
        }
        let value = self
            .rpc(
                "account/login/start",
                json!({"type":"chatgpt","useHostedLoginSuccessPage":true,"appBrand":"codex"}),
            )
            .await?;
        if value["type"] != "chatgpt" {
            return Err(failure(502, "codex_protocol_mismatch"));
        }
        let url = value["authUrl"]
            .as_str()
            .ok_or(failure(502, "codex_protocol_mismatch"))?;
        let parsed =
            reqwest::Url::parse(url).map_err(|_| failure(502, "codex_protocol_mismatch"))?;
        if parsed.scheme() != "https"
            || !matches!(parsed.host_str(), Some("auth.openai.com" | "chatgpt.com"))
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || url.len() > 8192
        {
            return Err(failure(502, "provider_authorization_url_rejected"));
        }
        self.login_id = Some(
            value["loginId"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 128)
                .ok_or(failure(502, "codex_protocol_mismatch"))?
                .to_owned(),
        );
        self.auth_url = Some(url.to_owned());
        self.until = Some(Instant::now() + Duration::from_secs(600));
        self.status(paths).await
    }
}
struct Entry {
    process: Mutex<Process>,
    paths: ProviderPaths,
}
#[derive(Default, Clone)]
pub(super) struct OwnerProvider {
    tasks: TaskCustody,
    entries: Arc<Mutex<BTreeMap<String, Arc<Entry>>>>,
}
#[derive(Clone)]
pub(super) enum Operation {
    Status,
    Login,
    Cancel,
    Logout,
    Models,
    UseLocal,
    Disconnect,
    PrepareWorkspace { agent: String },
}
impl OwnerProvider {
    pub async fn call(
        &self,
        console: &Console,
        cookie: &str,
        operation: Operation,
    ) -> Result<ProviderStatus, ProviderError> {
        let _activity = self
            .tasks
            .enter()
            .map_err(|_| failure(409, "provider_closing"))?;
        let reply = console
            .owner_api(cookie, OwnerOperation::Agents)
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let device = console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        if device.origin() != reply.origin
            || device.issuer() != reply.issuer
            || device.subject() != reply.subject
            || device.owner_mxid() != reply.owner
        {
            return Err(failure(401, "owner_authorization_required"));
        }
        device
            .bearer()
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let root = console
            .0
            .server_login
            .state_directory()
            .ok_or(failure(503, "provider_directory_unavailable"))?;
        let paths = paths(
            &root,
            &reply.origin,
            &reply.issuer,
            &reply.subject,
            &reply.owner,
        )?;
        if matches!(operation, Operation::UseLocal | Operation::Disconnect) {
            console
                .0
                .owned_runtime
                .stop_profile(&device)
                .await
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            if let Some(entry) = self.entries.lock().await.remove(&paths.credential_ref) {
                let mut p = entry.process.lock().await;
                let _ = p.cancel().await;
                let _ = p.child.kill().await;
            }
            device
                .bearer()
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            if matches!(operation, Operation::UseLocal) {
                let home = trusted_local_home()?;
                // Validate sealed child and current managed account before associating it.
                let candidate = ProviderPaths {
                    home: paths.home.clone(),
                    codex_home: home.clone(),
                    credential_ref: selected_reference(&home, true)?,
                    shared: true,
                };
                let mut p =
                    Process::spawn_inner(&candidate, &executable()?, false, &self.tasks).await?;
                let checked = p.status(&candidate).await;
                let _ = p.child.kill().await;
                let checked = checked?;
                if !checked.authenticated {
                    return Err(failure(409, "local_codex_account_not_signed_in"));
                }
                device
                    .bearer()
                    .map_err(|_| failure(401, "owner_authorization_required"))?;
                let verified = console
                    .owner_api(cookie, OwnerOperation::Agents)
                    .await
                    .map_err(|_| failure(401, "owner_authorization_required"))?;
                if verified.owner != reply.owner
                    || verified.origin != reply.origin
                    || verified.issuer != reply.issuer
                    || verified.subject != reply.subject
                {
                    return Err(failure(401, "owner_authorization_required"));
                }
                device
                    .bearer()
                    .map_err(|_| failure(401, "owner_authorization_required"))?;
                hagency_store::private::replace(
                    &choice_path(&paths),
                    &serde_json::to_vec(&json!({"version":1,"home":home.to_string_lossy()}))
                        .map_err(|_| failure(503, "provider_choice_unavailable"))?,
                )
                .map_err(|_| failure(503, "provider_choice_unavailable"))?;
                return Ok(checked);
            }
            match std::fs::remove_file(choice_path(&paths)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(failure(503, "provider_choice_unavailable")),
            }
            let paths = self::paths(
                &root,
                &reply.origin,
                &reply.issuer,
                &reply.subject,
                &reply.owner,
            )?;
            return Ok(ProviderStatus {
                state: "disconnected",
                authenticated: false,
                credential_ref: paths.credential_ref,
                credential_store: "codex_os_keyring",
                auth_url: None,
                strict_token_cap: false,
                native_tools: true,
                models: None,
                default_model: None,
                workspace: None,
                shared: false,
                credential_source: "dedicated",
            });
        }
        if let Operation::PrepareWorkspace { agent } = &operation {
            if !reply.value["agents"].as_array().is_some_and(|agents| {
                agents.iter().any(|a| {
                    a["id"] == *agent
                        && !matches!(a["state"].as_str(), Some("retiring" | "retired"))
                })
            }) {
                return Err(failure(403, "owner_scope_required"));
            }
            let workspace = prepare_workspace(&paths, agent)?;
            device
                .bearer()
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            let fresh = console
                .owner_api(cookie, OwnerOperation::Agents)
                .await
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            if fresh.origin != reply.origin
                || fresh.issuer != reply.issuer
                || fresh.subject != reply.subject
                || fresh.owner != reply.owner
            {
                return Err(failure(401, "owner_authorization_required"));
            }
            return Ok(ProviderStatus {
                state: "workspace_prepared",
                authenticated: false,
                credential_ref: paths.credential_ref,
                credential_store: "codex_os_keyring",
                auth_url: None,
                strict_token_cap: false,
                native_tools: true,
                models: None,
                default_model: None,
                workspace: Some(workspace.to_string_lossy().into_owned()),
                shared: paths.shared,
                credential_source: if paths.shared {
                    "local_codex"
                } else {
                    "dedicated"
                },
            });
        }
        if matches!(operation, Operation::Logout) {
            console
                .0
                .owned_runtime
                .stop_profile(&device)
                .await
                .map_err(|_| failure(401, "owner_authorization_required"))?;
        }
        let key = paths.credential_ref.clone();
        let entry = {
            let mut entries = self.entries.lock().await;
            device
                .bearer()
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            if let Some(entry) = entries.get(&key) {
                entry.clone()
            } else {
                self.tasks
                    .check_open()
                    .map_err(|_| failure(409, "provider_closing"))?;
                let mut process =
                    Process::spawn_inner(&paths, &executable()?, false, &self.tasks).await?;
                if device.bearer().is_err() {
                    let _ = process.child.kill().await;
                    return Err(failure(401, "owner_authorization_required"));
                }
                let entry = Arc::new(Entry {
                    process: Mutex::new(process),
                    paths,
                });
                entries.insert(key.clone(), entry.clone());
                entry
            }
        };
        let mut process = entry.process.lock().await;
        device
            .bearer()
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let result = match operation {
            Operation::Status => process.status(&entry.paths).await,
            Operation::Models => process.models(&entry.paths).await,
            Operation::UseLocal | Operation::Disconnect => unreachable!("handled above"),
            Operation::PrepareWorkspace { .. } => unreachable!("handled before provider process"),
            Operation::Login => process.start(&entry.paths).await,
            Operation::Cancel => {
                process.cancel().await?;
                process.status(&entry.paths).await
            }
            Operation::Logout => {
                if entry.paths.shared {
                    return Err(failure(409, "shared_codex_logout_is_managed_locally"));
                }
                process.cancel().await?;
                process.rpc("account/logout", json!({})).await?;
                process.status(&entry.paths).await
            }
        };
        // Recheck local/server capability before delivering account metadata or login URL.
        device
            .bearer()
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let verified = console
            .owner_api(cookie, OwnerOperation::Agents)
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        if verified.owner != reply.owner
            || verified.origin != reply.origin
            || verified.issuer != reply.issuer
            || verified.subject != reply.subject
        {
            return Err(failure(401, "owner_authorization_required"));
        }
        if result.is_err() {
            let _ = process.child.kill().await;
            drop(process);
            let mut entries = self.entries.lock().await;
            if entries
                .get(&key)
                .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                entries.remove(&key);
            }
        }
        result
    }
    pub fn request_stop_all(&self) {
        if let Ok(entries) = self.entries.try_lock() {
            for entry in entries.values() {
                if let Ok(mut process) = entry.process.try_lock() {
                    let _ = process.child.start_kill();
                }
            }
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let manager = self.clone();
            runtime.spawn(async move {
                manager.stop_all().await;
            });
        }
    }
    pub async fn stop_all(&self) {
        let mut entries = self.entries.lock().await;
        for entry in entries.values() {
            let mut process = entry.process.lock().await;
            let _ = process.cancel().await;
            if process.child.kill().await.is_err() {
                self.tasks.unknown();
            }
        }
        entries.clear();
    }
}
/// Typed provider entry shared by the browser adapter and embedded owner.
/// Browser-origin checks belong to the HTTP adapter; finite owner/device
/// authorization and response custody apply identically to both callers.
pub(super) async fn invoke(
    console: &Console,
    cookie: &str,
    runtime: Option<&str>,
    operation: Operation,
) -> Result<Value, ProviderError> {
    let session = console
        .0
        .authority
        .authenticate(cookie)
        .map_err(|_| failure(401, "sign_in_required"))?;
    console
        .0
        .server_login
        .validate(cookie)
        .await
        .map_err(|error| {
            if matches!(error, super::Error::Unavailable) {
                failure(503, "server_unavailable")
            } else {
                failure(401, "owner_authorization_required")
            }
        })?;
    let result = if let Some(runtime) = runtime {
        external::call(console, cookie, runtime, operation).await
    } else {
        console
            .0
            .owner_provider
            .call(console, cookie, operation)
            .await
            .and_then(|status| {
                serde_json::to_value(status).map_err(|_| failure(503, "provider_unavailable"))
            })
    };
    console
        .0
        .authority
        .check(&session)
        .map_err(|_| failure(401, "sign_in_required"))?;
    result
}

pub(super) fn router() -> Router {
    Router::with_path("owner-provider")
        .goal(dispatch)
        .push(Router::with_path("workspaces/{agent}").goal(dispatch))
        .push(
            Router::with_path("runtimes/{runtime}")
                .goal(dispatch)
                .push(Router::with_path("{action}").goal(dispatch)),
        )
        .push(Router::with_path("{action}").goal(dispatch))
}
#[handler]
async fn dispatch(req: &mut Request, depot: &Depot, res: &mut Response) {
    let result = async {
        let _guard = current(req, depot).map_err(|_| failure(401, "sign_in_required"))?;
        if req.uri().query().is_some() {
            return Err(failure(400, "invalid_arguments"));
        }
        let action = req.param::<String>("action").unwrap_or_default();
        let operation = match (req.method(), action.as_str()) {
            (&salvo::http::Method::POST, "") if req.param::<String>("agent").is_some() => {
                Operation::PrepareWorkspace {
                    agent: req.param::<String>("agent").unwrap(),
                }
            }
            (&salvo::http::Method::GET, "") => Operation::Status,
            (&salvo::http::Method::GET, "models") => Operation::Models,
            (&salvo::http::Method::POST, "use-local") => Operation::UseLocal,
            (&salvo::http::Method::POST, "disconnect") => Operation::Disconnect,
            (&salvo::http::Method::POST, "login") => Operation::Login,
            (&salvo::http::Method::POST, "cancel") => Operation::Cancel,
            (&salvo::http::Method::POST, "logout") => Operation::Logout,
            _ => return Err(failure(404, "not_found")),
        };
        if !body(req, 1)
            .await
            .map_err(|_| failure(400, "invalid_arguments"))?
            .is_empty()
        {
            return Err(failure(400, "invalid_arguments"));
        }
        let console = console(depot).map_err(|_| failure(503, "local_state_unavailable"))?;
        let runtime = req.param::<String>("runtime");
        let value = invoke(
            console,
            cookie(req).map_err(|_| failure(401, "sign_in_required"))?,
            runtime.as_deref(),
            operation,
        )
        .await?;
        recheck(depot).map_err(|_| failure(401, "sign_in_required"))?;
        Ok(value)
    }
    .await;
    match result {
        Ok(value) => res.render(Json(value)),
        Err(error) => {
            // Static protocol diagnostics only: never log provider payload,
            // account identity, configuration, auth URLs or credentials.
            eprintln!(
                "hagency owner-provider failure: status={} code={}",
                error.status, error.code
            );
            res.status_code(
                StatusCode::from_u16(error.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
            );
            res.render(Json(json!({"code":error.code})));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn model_catalog_projects_only_bounded_public_fields_and_errors_redact_credentials() {
        let page = json!({"data":[{"id":"m1","model":"real-model","displayName":"Real Model","description":"Catalog entry","isDefault":true,"hidden":false,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"Balanced"}],"token":"must-not-escape"}],"nextCursor":null});
        let models = model_page(&page).unwrap();
        let value = serde_json::to_value(models).unwrap();
        assert_eq!(value[0]["model"], "real-model");
        assert!(!value.to_string().contains("must-not-escape"));
        let mut invalid = page.clone();
        invalid["data"][0]["defaultReasoningEffort"] = "unsupported".into();
        assert!(model_page(&invalid).is_err());
        let mut hidden = page;
        hidden["data"][0]["hidden"] = true.into();
        assert!(model_page(&hidden).unwrap().is_empty());
        for (message, code) in [
            (
                "Failed to bind address: port occupied; secret=redact",
                "provider_login_callback_unavailable",
            ),
            (
                "Keychain refused credential store: redact",
                "provider_keyring_unavailable",
            ),
        ] {
            assert_eq!(
                rpc_rejection(&json!({"code":-32000,"message":message})).code,
                code
            );
        }
        assert_eq!(
            rpc_rejection(&json!({"code":-32602,"message":"invalid input bearer=redact"})).code,
            "codex_protocol_mismatch"
        );
    }
    #[test]
    fn prepared_workspaces_are_private_canonical_agent_and_full_owner_scoped() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("state");
        let a = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "sub-a",
            "@owner:example",
        )
        .unwrap();
        let b = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "sub-b",
            "@owner:example",
        )
        .unwrap();
        let workspace = prepare_workspace(&a, "agt_one").unwrap();
        assert_eq!(workspace, workspace.canonicalize().unwrap());
        assert_eq!(workspace, prepare_workspace(&a, "agt_one").unwrap());
        assert_ne!(workspace, prepare_workspace(&a, "agt_two").unwrap());
        assert_ne!(workspace, prepare_workspace(&b, "agt_one").unwrap());
        assert!(!a.codex_home.starts_with(&workspace));
        assert!(!a.home.starts_with(&workspace));
        assert!(prepare_workspace(&a, "../escape").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, symlink};
            assert_eq!(std::fs::metadata(&workspace).unwrap().mode() & 0o777, 0o700);
            symlink(
                &workspace,
                a.home.parent().unwrap().join("workspaces/agt_link"),
            )
            .unwrap();
            assert!(prepare_workspace(&a, "agt_link").is_err());
        }
    }
    #[tokio::test]
    #[ignore = "uses installed Codex only in a new empty private home; model catalog, no login or inference"]
    async fn installed_provider_model_catalog_without_login_or_inference() {
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap().join("state");
        let paths = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "fresh-subject",
            "@owner:example",
        )
        .unwrap();
        let mut process = Process::spawn(&paths, &executable().unwrap())
            .await
            .unwrap();
        let status = process.models(&paths).await.unwrap();
        assert!(
            !status.authenticated,
            "new private home must not inherit daily Codex account"
        );
        let models = status.models.unwrap();
        assert!(!models.is_empty());
        assert!(
            models
                .iter()
                .all(|m| !m.model.is_empty() && !m.supported_reasoning_efforts.is_empty())
        );
        process.child.kill().await.unwrap();
    }
    #[tokio::test]
    #[ignore = "explicit reuse of installed managed account; account/read and model/list only, no inference"]
    async fn installed_shared_managed_account_and_models_without_inference() {
        let temp = tempfile::Builder::new()
            .prefix(".hagency-shared-ancestry-probe-")
            .tempdir_in(std::env::var_os("HOME").unwrap())
            .unwrap();
        let root = temp.path().canonicalize().unwrap().join("state");
        let mut p = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "shared-probe",
            "@owner:example",
        )
        .unwrap();
        p.codex_home = trusted_local_home().unwrap();
        p.shared = true;
        p.credential_ref = selected_reference(&p.codex_home, true).unwrap();
        hagency_agent_local::codex::sealed_shared_overrides(
            &executable().unwrap(),
            &p.home,
            &p.codex_home,
        )
        .await
        .unwrap();
        let mut process = Process::spawn(&p, &executable().unwrap()).await.unwrap();
        let status = process.models(&p).await.unwrap();
        assert!(
            status.authenticated,
            "explicitly selected cached account must be signed in"
        );
        assert!(status.shared);
        assert!(!status.models.unwrap().is_empty());
        process.child.kill().await.unwrap();
    }
    use super::*;
    #[test]
    fn dedicated_provider_paths_are_private_canonical_and_owner_scoped() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = root.path().canonicalize().unwrap().join("state");
        let first = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "test-subject",
            "@alice:example",
        )
        .unwrap();
        let second = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "test-subject",
            "@bob:example",
        )
        .unwrap();
        let changed_subject = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "different-subject",
            "@alice:example",
        )
        .unwrap();
        assert_ne!(first.credential_ref, changed_subject.credential_ref);
        assert_ne!(first.codex_home, changed_subject.codex_home);
        assert_ne!(first.credential_ref, second.credential_ref);
        assert_ne!(first.codex_home, second.codex_home);
        let hash = format!(
            "{:x}",
            Sha256::digest(first.codex_home.to_string_lossy().as_bytes())
        );
        assert_eq!(
            first.credential_ref,
            format!("keychain:codex-home:{}", &hash[..16])
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, symlink};
            assert_eq!(
                std::fs::metadata(&first.codex_home).unwrap().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&first.home).unwrap().mode() & 0o777,
                0o700
            );
            let link = first.home.join("linked-login");
            symlink(&first.codex_home, &link).unwrap();
            assert!(credential_reference(&link).is_err());
        }
        std::fs::write(first.codex_home.join("auth.json"), "{}").unwrap();
        assert_eq!(
            credential_reference(&first.codex_home).unwrap_err().code,
            "provider_file_credentials_or_config_rejected"
        );
        std::fs::remove_file(first.codex_home.join("auth.json")).unwrap();
        std::fs::write(first.codex_home.join("config.toml"), "").unwrap();
        assert!(credential_reference(&first.codex_home).is_err());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn multi_profile_switch_stops_pending_provider_children_without_deleting_account_homes() {
        use std::os::unix::fs::PermissionsExt;
        let temp = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let exe = root.join("fake-codex");
        std::fs::write(&exe,r#"#!/usr/bin/env python3
import json,sys
for line in sys.stdin:
 r=json.loads(line);i=r.get('id');m=r.get('method')
 if i is None:continue
 if m=='account/read':v={'requiresOpenaiAuth':True,'account':None}
 elif m=='account/login/start':v={'type':'chatgpt','loginId':'pending','authUrl':'https://auth.openai.com/authorize?state=test'}
 elif m in ['initialize','account/login/cancel']:v={}
 else:raise Exception('no model or account credential mutation permitted')
 print(json.dumps({'id':i,'result':v}),flush=True)
"#).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let manager = OwnerProvider::default();
        let mut held = vec![];
        for sub in ["subject-a", "subject-b"] {
            let paths = paths(
                &root.join("state"),
                "https://server.example/",
                "https://server.example/auth/",
                sub,
                "@alice:example",
            )
            .unwrap();
            std::fs::write(paths.home.join("retain-account-data"), sub).unwrap();
            let mut process = Process::spawn(&paths, &exe).await.unwrap();
            assert_eq!(process.start(&paths).await.unwrap().state, "signing_in");
            let entry = Arc::new(Entry {
                process: Mutex::new(process),
                paths,
            });
            manager
                .entries
                .lock()
                .await
                .insert(entry.paths.credential_ref.clone(), entry.clone());
            held.push(entry);
        }
        manager.stop_all().await;
        assert!(manager.entries.lock().await.is_empty());
        for entry in held {
            let mut p = entry.process.lock().await;
            assert!(p.child.try_wait().unwrap().is_some());
            assert!(p.login_id.is_none());
            assert!(entry.paths.home.join("retain-account-data").is_file());
            assert!(entry.paths.codex_home.is_dir());
        }
    }
    #[tokio::test]
    async fn onboarding_managed_keyring_login_status_and_logout_never_execute_turns() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap();
        let paths = paths(
            &directory.join("state"),
            "https://server.example/",
            "https://server.example/auth/",
            "test-subject",
            "@owner:example",
        )
        .unwrap();
        let executable = directory.join("fake-codex");
        std::fs::write(&executable,r#"#!/usr/bin/env python3
import json,os,sys,pathlib
home=pathlib.Path.cwd(); marker=home/'signed-in'
(home/'process-metadata.json').write_text(json.dumps({'home':os.environ['HOME'],'codex_home':os.environ['CODEX_HOME'],'args':sys.argv[1:],'keys':list(os.environ.keys())}))
for line in sys.stdin:
 req=json.loads(line); method=req.get('method'); rid=req.get('id')
 if rid is None:continue
 if method=='initialize':result={}
 elif method=='account/read':result={'requiresOpenaiAuth':True,'account':({'type':'chatgpt','email':'private@example','planType':'plus'} if marker.exists() else None)}
 elif method=='account/login/start':
  assert req['params']['type']=='chatgpt'
  result={'type':'chatgpt','loginId':'login-1','authUrl':'https://auth.openai.com/authorize?state=test'}
 elif method=='account/login/cancel':result={}
 elif method=='account/logout':
  marker.unlink(missing_ok=True);result={}
 else:raise Exception('onboarding must not start inference')
 print(json.dumps({'id':rid,'result':result}),flush=True)
"#).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut process = Process::spawn(&paths, &executable).await.unwrap();
        assert!(!process.status(&paths).await.unwrap().authenticated);
        let pending = process.start(&paths).await.unwrap();
        assert_eq!(pending.state, "signing_in");
        assert!(pending.auth_url.is_some());
        process.cancel().await.unwrap();
        assert_eq!(process.status(&paths).await.unwrap().state, "signed_out");
        process.start(&paths).await.unwrap();
        std::fs::write(paths.home.join("signed-in"), "").unwrap();
        let status = process.status(&paths).await.unwrap();
        assert!(status.authenticated);
        assert!(status.auth_url.is_none());
        let public = serde_json::to_string(&status).unwrap();
        assert!(!public.contains("private@example"));
        assert!(!public.contains("token"));
        process.rpc("account/logout", json!({})).await.unwrap();
        assert!(!process.status(&paths).await.unwrap().authenticated);
        let metadata: Value = serde_json::from_slice(
            &std::fs::read(paths.home.join("process-metadata.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(metadata["home"], hagency_platform::codex_process_home(&paths.home).unwrap().to_string_lossy().as_ref());
        assert_eq!(
            metadata["codex_home"],
            paths.codex_home.to_string_lossy().as_ref()
        );
        assert!(
            metadata["args"]
                .as_array()
                .unwrap()
                .contains(&json!("cli_auth_credentials_store=\"keyring\""))
        );
        for flag in [
            "project_doc_max_bytes=0",
            "skills.include_instructions=false",
            "skills.bundled.enabled=false",
            "web_search=\"disabled\"",
            "developer_instructions=\"\"",
            "include_apps_instructions=false",
            "include_environment_context=false",
        ] {
            assert!(metadata["args"].as_array().unwrap().contains(&json!(flag)));
        }
        assert!(
            !metadata["keys"]
                .as_array()
                .unwrap()
                .iter()
                .any(|key| matches!(
                    key.as_str(),
                    Some("OPENAI_API_KEY" | "CODEX_ACCESS_TOKEN" | "HAGENCY_DEVICE_TOKEN")
                ))
        );
        assert!(!paths.codex_home.join("auth.json").exists());
        let _ = process.child.kill().await;
    }
    #[tokio::test]
    #[ignore = "requires installed Codex 0.160 and OS keyring; no login or inference"]
    async fn installed_codex_reads_only_its_dedicated_empty_owner_keyring() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let root = root.path().canonicalize().unwrap().join("state");
        let paths = paths(
            &root,
            "https://server.example/",
            "https://server.example/auth/",
            "test-subject",
            "@fresh-owner:example",
        )
        .unwrap();
        let mut process = Process::spawn(&paths, &executable().unwrap())
            .await
            .unwrap();
        let status = process.status(&paths).await.unwrap();
        assert!(!status.authenticated);
        assert_eq!(status.state, "signed_out");
        assert!(!paths.codex_home.join("auth.json").exists());
        assert!(!paths.codex_home.join("config.toml").exists());
        for ambient in ["skills", "AGENTS.md", "AGENTS.override.md"] {
            assert!(!paths.codex_home.join(ambient).exists());
        }
        let _ = process.child.kill().await;
        let mut reopened = Process::spawn(&paths, &executable().unwrap())
            .await
            .unwrap();
        assert!(!reopened.status(&paths).await.unwrap().authenticated);
        assert!(!paths.codex_home.join("skills").exists());
        let _ = reopened.child.kill().await;
    }
}
