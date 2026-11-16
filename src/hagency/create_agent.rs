//! The one-screen Create agent dialog (Buzz style): Name, Harness, Model and
//! Thinking (and an optional Backup model), then the same setup as one-click creation
//! (`quick_setup`). This file is the dialog's state and its mapping to a
//! `SimpleAgentSpec`; the widget lives in `create_agent/dialog.rs`.
mod dialog;
pub use dialog::{CreateAgentAction, CreateAgentDialog, CreateAgentDialogWidgetRefExt, script_mod};

use ::hagency::simple_agent::{DEFAULT_REASONING_EFFORT, Runtime, SimpleAgentSpec};
use serde_json::Value;

/// Local harnesses in dialog order.
const HARNESSES: [Runtime; 2] = [Runtime::Claude, Runtime::Codex];
/// Thinking levels offered; each is the model profile's `reasoning_effort`.
pub(crate) const THINKING: [&str; 3] = ["low", "medium", "high"];
/// The server's Agent display-name limit (owned_agents `Create`).
const NAME_MAX_CHARS: usize = 64;

/// One model the harness's provider lists, as the dialog shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelChoice {
    pub model: String,
    pub label: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
}

/// The provider's model list (`Provider`/`RuntimeProvider` `Models` reply),
/// the provider's default model first.
pub(crate) fn parse_models(reply: &Value) -> Vec<ModelChoice> {
    let rows = reply["models"].as_array().cloned().unwrap_or_default();
    let mut models: Vec<(bool, ModelChoice)> = rows.iter().filter_map(|row| {
        let model = row["model"].as_str().filter(|m| !m.is_empty())?.to_owned();
        let efforts = row["supportedReasoningEfforts"].as_array().map(|list| {
            list.iter().filter_map(|e| e["reasoningEffort"].as_str().map(str::to_owned)).collect()
        }).unwrap_or_default();
        Some((row["isDefault"] == true, ModelChoice {
            label: row["displayName"].as_str().filter(|l| !l.is_empty()).unwrap_or(&model).to_owned(),
            default_effort: row["defaultReasoningEffort"].as_str().unwrap_or_default().to_owned(),
            model,
            efforts,
        }))
    }).collect();
    models.sort_by_key(|(default, _)| !*default); // stable: provider order otherwise
    models.into_iter().map(|(_, choice)| choice).collect()
}

/// The Thinking level to save: the chosen one when the model offers it,
/// otherwise the model's own default (a model that lists nothing takes it as is).
pub(crate) fn effort_for(choice: &ModelChoice, wanted: &str) -> String {
    if choice.efforts.is_empty() || choice.efforts.iter().any(|e| e == wanted) {
        return wanted.to_owned();
    }
    [&choice.default_effort, &choice.efforts[0]].into_iter().find(|e| !e.is_empty()).cloned().unwrap_or_default()
}

/// Same rule as the server: non-blank, at most 64 characters, no control characters.
pub(crate) fn validate_name(name: &str) -> Result<String, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a name for the agent.");
    }
    if name.chars().count() > NAME_MAX_CHARS || name.chars().any(char::is_control) {
        return Err("Use at most 64 characters, without line breaks.");
    }
    Ok(name.to_owned())
}

/// What the dialog shows when setup stops: the step, the code and, for the
/// codes an owner can act on, what to do. Never a bare code.
pub(crate) fn failure_message(step: &str, code: &str) -> String {
    let hint = match code {
        "invalid_arguments" => "The request was refused as malformed; update Hagency or report this.",
        "device_setup_required" => "This Mac is not set up to run Agents yet; open the Agent's Hosting tab once.",
        "provider_models_unavailable" | "provider_credential_unavailable" => "Sign in to Claude Code or Codex on this Mac, then try again.",
        "codex_version_unsupported" => "Update the Codex CLI to 0.167.0 or newer, or choose Claude.",
        "sign_in_required" | "matrix_oauth_sign_in_required" => "Sign in again, then retry.",
        _ => "Try again; the Agent may already exist in your list.",
    };
    format!("{} \"{step}\": {code}. {}", crate::i18n::tr("Create agent stopped at step"), crate::i18n::tr(hint))
}

pub(crate) fn harness_label(runtime: &Runtime) -> &'static str {
    if *runtime == Runtime::Claude { "Claude" } else { "Codex" }
}
/// "ann-claude": the owner's name and the harness, like the demo Agents.
pub(crate) fn default_name(localpart: &str, runtime: &Runtime) -> String {
    format!("{localpart}-{}", harness_label(runtime).to_ascii_lowercase())
}

/// What the dialog holds between draws. Harness probes arrive one by one.
#[derive(Default)]
pub(crate) struct DialogState {
    /// Per `HARNESSES` entry: `None` while checking, then the models or the error code.
    pub probes: [Option<Result<Vec<ModelChoice>, String>>; 2],
    pub harness: usize,
    pub model: usize,
    pub thinking: usize,
    pub name: String,
    /// "Add to rooms": the checklist rows and the room the app showed on opening.
    pub rooms: Vec<crate::hagency::bring_agent::RoomChoice>,
    pub current_room: Option<String>,
    /// "Backup model": the Ollama models listed (stored form) and the chosen row, 0 = None.
    pub backups: Vec<String>,
    pub backup: usize,
}
impl DialogState {
    pub fn new(name_for: impl Fn(&Runtime) -> String) -> Self {
        let mut state = Self { thinking: THINKING.iter().position(|t| *t == DEFAULT_REASONING_EFFORT).unwrap_or(1), ..Self::default() };
        state.name = name_for(&HARNESSES[0]);
        state
    }
    pub fn probe_commands() -> impl Iterator<Item = (usize, Runtime)> {
        HARNESSES.into_iter().enumerate()
    }
    /// Harnesses whose provider answered with at least one model, as indexes into `HARNESSES`.
    pub fn available(&self) -> Vec<usize> {
        (0..HARNESSES.len()).filter(|i| matches!(&self.probes[*i], Some(Ok(models)) if !models.is_empty())).collect()
    }
    /// A probe failed because this window's Agent sign-in expired, not because
    /// the Mac lacks a Claude Code or Codex login: only signing in again helps.
    pub fn needs_sign_in(&self) -> bool {
        self.probes.iter().flatten().any(|probe| matches!(probe, Err(code) if matches!(code.as_str(),
            "native_resume_missing" | "native_resume_revoked" | "sign_in_required" | "matrix_sign_in_required"
            | "matrix_oauth_sign_in_required" | "matrix_authorization_required" | "matrix_account_mismatch")))
    }
    /// The probe's owner grant names another account than this window's.
    pub fn account_mismatch(&self) -> bool {
        self.probes.iter().flatten().any(|probe| matches!(probe, Err(code) if code == "matrix_account_mismatch"))
    }
    pub fn checking(&self) -> bool {
        self.probes.iter().any(Option::is_none)
    }
    pub fn runtime(&self) -> Option<Runtime> {
        self.available().get(self.harness).map(|i| HARNESSES[*i].clone())
    }
    pub fn models(&self) -> &[ModelChoice] {
        match self.available().get(self.harness).and_then(|i| self.probes[*i].as_ref()) {
            Some(Ok(models)) => models,
            _ => &[],
        }
    }
    /// The backup model to save for the new Agent, None unless chosen.
    pub fn backup_choice(&self) -> Option<String> {
        self.backup.checked_sub(1).and_then(|i| self.backups.get(i)).cloned()
    }
    /// The spec to create: one-click defaults plus the chosen model and Thinking.
    pub fn spec(&self, defaults: impl Fn(String, Runtime) -> SimpleAgentSpec) -> Result<SimpleAgentSpec, &'static str> {
        let name = validate_name(&self.name)?;
        let runtime = self.runtime().ok_or("No Claude Code or Codex login was found on this Mac.")?;
        let choice = self.models().get(self.model).ok_or("Choose a model.")?;
        let mut spec = defaults(name, runtime);
        spec.model = Some(choice.model.clone());
        spec.reasoning_effort = Some(effort_for(choice, THINKING[self.thinking.min(THINKING.len() - 1)]));
        Ok(spec)
    }
}

#[cfg(test)]
mod tests;
