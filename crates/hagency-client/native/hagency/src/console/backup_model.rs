//! Per-Agent backup model. When the main provider fails a turn for its own
//! availability (usage limit, overload, outage) before any tool ran, the
//! same request is answered once by the Agent's backup. Today the backup is
//! an Ollama model reached through the local Ollama API only, and it answers
//! text-only: no tools, shell, files or GitHub, so it adds no permission.
//! Off unless the owner picks one.
// TODO(backup-model): a tool-using backup (Claude Code pointed at Ollama's
// Anthropic-compatible API) needs run_turn split first; see docs/adr/0014-backup-model.md.
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::OnceLock, time::Duration};

/// The local Ollama API. Cloud models are served through it as well, so no
/// Hagency credential or other host is ever involved.
pub const OLLAMA_ORIGIN: &str = "http://127.0.0.1:11434";
/// Stored value prefix: `ollama:<model>|<place>`.
pub const OLLAMA_PREFIX: &str = "ollama:";
/// Separates the model from where Ollama said it runs; never in a model name.
const PLACE_SEPARATOR: char = '|';
const PLACE_LOCAL: &str = "local";
const PLACE_CLOUD: &str = "cloud";
const STORE_FILE: &str = "backup-models.json";
const ANSWER_TIMEOUT: Duration = Duration::from_secs(180);
const LIST_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_ANSWER_BYTES: usize = 60_000;
const SWITCHED_NOTICE: &str = "ℹ️ Main model unavailable ({reason}); answered by the backup model {label}, without tools.";

static STORE_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Called once by the app with its data directory.
pub fn set_store_dir(dir: &Path) {
    let _ = STORE_DIR.set(dir.to_path_buf());
}
fn store_path() -> Option<PathBuf> {
    STORE_DIR.get().map(|dir| dir.join(STORE_FILE))
}
fn read_store(path: &Path) -> BTreeMap<String, String> {
    // A missing or unreadable file means no backup is configured: off is the safe default.
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}
/// The Agent's saved backup (`ollama:<model>`), if any.
pub fn load(agent_id: &str) -> Option<String> {
    load_from(&store_path()?, agent_id)
}
pub fn load_from(path: &Path, agent_id: &str) -> Option<String> {
    read_store(path).remove(agent_id)
}
/// Saves (or clears, with `None`) the Agent's backup.
pub fn save(agent_id: &str, backup: Option<&str>) -> std::io::Result<()> {
    let path = store_path().ok_or_else(|| std::io::Error::other("backup store not set"))?;
    save_to(&path, agent_id, backup)
}
pub fn save_to(path: &Path, agent_id: &str, backup: Option<&str>) -> std::io::Result<()> {
    let mut store = read_store(path);
    match backup {
        Some(value) => store.insert(agent_id.to_owned(), value.to_owned()),
        None => store.remove(agent_id),
    };
    let bytes = serde_json::to_vec_pretty(&store).map_err(std::io::Error::other)?;
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes)?;
    std::fs::rename(temp, path)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OllamaModel {
    pub name: String,
    pub cloud: bool,
}
impl OllamaModel {
    /// `ollama:<name>|local` or `ollama:<name>|cloud`: the place Ollama
    /// reported is saved with the choice, never guessed from the name later.
    pub fn stored(&self) -> String {
        let place = if self.cloud { PLACE_CLOUD } else { PLACE_LOCAL };
        format!("{OLLAMA_PREFIX}{}{PLACE_SEPARATOR}{place}", self.name)
    }
}
/// The model name of a stored value (`ollama:` and the place removed).
pub fn model_name(stored: &str) -> &str {
    let name = stored.strip_prefix(OLLAMA_PREFIX).unwrap_or(stored);
    name.split_once(PLACE_SEPARATOR).map_or(name, |(name, _)| name)
}
/// Whether two stored values name the same model, whatever place they record.
pub fn same_model(a: &str, b: &str) -> bool {
    model_name(a) == model_name(b)
}
/// Text models in an `/api/tags` reply, cloud ones first.
pub fn parse_tags(tags: &Value) -> Vec<OllamaModel> {
    let mut models: Vec<OllamaModel> = tags["models"].as_array().into_iter().flatten()
        .filter(|m| m["capabilities"].as_array().is_none_or(|c| c.iter().any(|c| c == "completion")))
        .filter_map(|m| m["name"].as_str().map(|name| OllamaModel {
            name: name.to_owned(),
            cloud: m.get("remote_host").is_some() || name.ends_with("-cloud"),
        }))
        .collect();
    models.sort_by_key(|m| !m.cloud);
    models
}
/// The models this Mac's Ollama lists; empty when Ollama is not running.
pub async fn ollama_models() -> Vec<OllamaModel> {
    let client = reqwest::Client::new();
    let reply = client.get(format!("{OLLAMA_ORIGIN}/api/tags")).timeout(LIST_TIMEOUT).send().await;
    match reply {
        Ok(reply) => parse_tags(&reply.json().await.unwrap_or(Value::Null)),
        // Ollama not installed or not running: no Ollama backup to offer.
        Err(_) => Vec::new(),
    }
}
/// "Ollama: gpt-oss:120b-cloud (cloud)" for a stored value; one saved without
/// a known place reads "(cloud or remote)".
pub fn label(stored: &str) -> String {
    let recorded = stored.strip_prefix(OLLAMA_PREFIX).and_then(|v| v.split_once(PLACE_SEPARATOR)).map(|(_, place)| place);
    let place = match recorded {
        Some(PLACE_LOCAL) => PLACE_LOCAL,
        Some(PLACE_CLOUD) => PLACE_CLOUD,
        _ => "cloud or remote",
    };
    format!("Ollama: {} ({place})", model_name(stored))
}
/// The stored value with the place Ollama reports now, when it still lists
/// the model; otherwise unchanged (an unknown place stays treated as cloud).
pub fn refreshed(stored: String, listed: &[OllamaModel]) -> String {
    let name = model_name(&stored);
    listed.iter().find(|m| m.name == name).map_or(stored, OllamaModel::stored)
}
/// The line that opens a reply the backup gave.
pub fn switched_notice(reason: &str, stored: &str) -> String {
    SWITCHED_NOTICE.replace("{reason}", reason).replace("{label}", &label(stored))
}
/// One text-only answer from the stored Ollama backup. Nothing but the
/// request text is sent, and only to the local Ollama API.
pub async fn answer(stored: &str, input: &str) -> Result<String, &'static str> {
    if !stored.starts_with(OLLAMA_PREFIX) {
        return Err("unsupported backup");
    }
    let model = model_name(stored);
    let body = json!({"model": model, "stream": false, "messages": [{"role": "user", "content": input}]});
    let reply = reqwest::Client::new().post(format!("{OLLAMA_ORIGIN}/api/chat"))
        .timeout(ANSWER_TIMEOUT).json(&body).send().await.map_err(|_| "backup unreachable")?;
    if !reply.status().is_success() {
        return Err("backup refused");
    }
    let value: Value = reply.json().await.map_err(|_| "backup reply invalid")?;
    let text = value["message"]["content"].as_str().unwrap_or("").trim();
    if text.is_empty() {
        return Err("backup reply empty");
    }
    Ok(truncate(text, MAX_ANSWER_BYTES))
}
fn truncate(text: &str, limit: usize) -> String {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) { end -= 1; }
    text[..end].to_owned()
}

#[cfg(test)]
#[path = "backup_model_tests.rs"]
mod tests;

type TurnResult = hagency_agent_local::codex::Result<hagency_agent_local::codex::Completed>;

/// The main provider's turn result, or — once, and only for an
/// availability failure before any tool ran — the Agent's backup answer.
pub(crate) async fn after_main(agent_id: &str, input: &str, main: TurnResult) -> TurnResult {
    // Ask Ollama where the model runs now, so the notice never says "local"
    // for a model it serves remotely. Only after a failure: no cost otherwise.
    let stored = match (&main, load(agent_id)) {
        (Err(hagency_agent_local::codex::Error::Unavailable(_)), Some(stored)) => Some(refreshed(stored, &ollama_models().await)),
        (_, stored) => stored,
    };
    retry_once(stored, main, |stored| async move { answer(&stored, input).await }).await
}
async fn retry_once<F, Fut>(stored: Option<String>, main: TurnResult, backup: F) -> TurnResult
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, &'static str>>,
{
    use hagency_agent_local::codex::{Completed, Error, TurnStatus};
    // Only `Unavailable` may retry: the agent-local layer returns it solely for
    // a usage limit, overload or outage with no tool requested. Denials, stops
    // and failures after a tool ran arrive as other results and pass through.
    let Err(Error::Unavailable(cause)) = main else { return main };
    let Some(stored) = stored else { return Err(Error::Unavailable(cause)) };
    let began = std::time::Instant::now();
    match backup(stored.clone()).await {
        Ok(text) => {
            tracing::info!(code = "backup_model_answered", reason = cause.reason(), "Main model unavailable; the backup model answered");
            Ok(Completed {
                text: format!("{}\n\n{text}", switched_notice(cause.reason(), &stored)),
                usage: hagency_agent_local::unreported_usage(),
                status: TurnStatus::Completed,
                elapsed_seconds: began.elapsed().as_secs(),
                tool_requests: 0,
                tool_returns: 0,
                late_failure: None,
            })
        }
        Err(code) => {
            tracing::warn!(code, "Main model unavailable and the backup model did not answer");
            Err(Error::Failed)
        }
    }
}
