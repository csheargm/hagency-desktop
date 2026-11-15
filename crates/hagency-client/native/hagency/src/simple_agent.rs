//! One-action Agent setup for Hagency Desktop. Both drive the same owner `Command`s the Agents
//! page issues by hand today: private Space, Agent, local provider login,
//! model profile, permissive request policy, service start and an owner DM.
//!
//! The executor is abstract so the in-process `NativeOwner` (Desktop) and tests share one code path.
use crate::native_owner::{Command, LocalAction, NativeError, NativeOwner, ProviderAction};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, pin::Pin, time::Duration};

mod agents; mod start; mod profile; mod progress;
pub use agents::{AgentIdentity, create_agent, find_agent};
pub use progress::{STEPS, current_step, last_failed_step, progress_text};
use progress::{CURRENT_STEP, LAST_FAILED_STEP, step_index};
#[cfg(test)] mod policy_tests;

pub type CommandFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, NativeError>> + Send + 'a>>;

pub trait CommandExecutor: Send + Sync {
    fn execute(&self, command: Command) -> CommandFuture<'_>;
}
impl CommandExecutor for NativeOwner {
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        Box::pin(NativeOwner::execute(self, command))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runtime {
    Codex,
    Claude,
    /// Any OpenAI-compatible chat endpoint (vLLM, Ollama, a hosted key).
    /// Accepted by the config surface now; the runtime is not implemented yet.
    OpenAiCompatible { endpoint: String },
}
impl std::str::FromStr for Runtime {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, String> {
        match value.to_ascii_lowercase().as_str() {
            "codex" => Ok(Self::Codex),
            "claude" | "claude-code" | "claude_code" => Ok(Self::Claude),
            other => Err(format!("unknown runtime {other:?}; use codex or claude")),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SimpleAgentSpec {
    pub name: String,
    pub runtime: Runtime,
    /// Provider model id; the first advertised model when absent.
    pub model: Option<String>,
    pub workspace_root: Option<String>,
    /// Token reservation for "Start responding"; 0 lets the runtime choose.
    pub reservation: u64,
    /// Tokens the Agent may spend per UTC day (the Policy budget; the "credit").
    pub daily_budget: u64,
    /// High-risk tool policy on the Agent card: "AskOwner" when an owner is at
    /// the keyboard (desktop, `agent up`), "Deny" for hosted Agents nobody
    /// watches (the concierge) until approvals reach the owner by DM.
    pub high_risk: &'static str,
    /// Model profile reasoning effort; `DEFAULT_REASONING_EFFORT` when absent.
    pub reasoning_effort: Option<String>,
}
/// Default daily allowance for humans in bound rooms (tokens per UTC day).
pub const DAILY_TOKEN_BUDGET: u64 = 500_000;
/// Default for an Agent on its owner's own subscription: one coding run that
/// opens a PR takes ~500K tokens, which used up the smaller default at once.
pub const PERSONAL_DAILY_TOKEN_BUDGET: u64 = 10_000_000;
/// The Thinking level an Agent gets unless its owner picks another.
pub const DEFAULT_REASONING_EFFORT: &str = "medium";
/// Ask the owner per tool.
pub const HIGH_RISK_ASK_OWNER: &str = "AskOwner";
/// Decline every high-risk tool.
pub const HIGH_RISK_DENY: &str = "Deny";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SimpleAgentReport {
    pub agent_id: String,
    pub puppet_mxid: String,
    pub project_id: String,
    pub space_id: String,
    pub model: String,
    pub started: bool,
    pub owner_direct_room_id: Option<String>,
}

/// Run one step and name it when it fails, so a headless log says where.
pub(super) async fn step(exec: &dyn CommandExecutor, label: &str, command: Command) -> Result<Value, NativeError> {
    CURRENT_STEP.store(step_index(label), std::sync::atomic::Ordering::Relaxed);
    match exec.execute(command).await {
        Ok(value) => Ok(value),
        Err(error) => {
            tracing::warn!(step = label, code = %error.code, status = error.status, "simple agent step failed");
            LAST_FAILED_STEP.store(step_index(label), std::sync::atomic::Ordering::Relaxed);
            Err(error)
        }
    }
}

pub(super) fn failure(code: &str) -> NativeError {
    NativeError { status: 502, code: code.to_owned() }
}
pub(super) fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_owned()
}

/// The first active project, or a fresh private Space adopted as one.
pub async fn ensure_project(exec: &dyn CommandExecutor, name: &str) -> Result<(String, String), NativeError> {
    let listed = step(exec, "projects", Command::Projects).await?;
    if let Some(project) = listed["projects"]
        .as_array()
        .and_then(|projects| projects.iter().find(|p| p["active"] != false))
    {
        return Ok((text(project, "id"), text(project, "spaceId")));
    }
    let command_id = crate::daemon::new_request_id();
    let input = json!({"commandId":command_id,"kind":"space","name":format!("{name} space"),"projectId":null,"spaceId":null});
    let mut reply = step(exec, "create-space", Command::CreateMatrix { input }).await?;
    for _ in 0..30 {
        let creation = &reply["creation"];
        match creation["phase"].as_str() {
            Some("complete") => {
                // The journal nests the adopted project; fall back to it.
                let project_id = [text(creation, "projectId"), text(&creation["project"], "id")].into_iter().find(|v| !v.is_empty()).unwrap_or_default();
                let space_id = [text(creation, "spaceId"), text(&creation["project"], "spaceId"), text(creation, "roomId")].into_iter().find(|v| !v.is_empty()).unwrap_or_default();
                return Ok((project_id, space_id));
            }
            Some("partial") | Some("unknown") => {
                return Err(failure(creation["lastError"].as_str().unwrap_or("space_creation_failed")));
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
        reply = exec.execute(Command::Creation { id: command_id.clone() }).await?;
    }
    Err(failure("space_creation_timeout"))
}

/// Connect the Mac's existing provider login and pick a model.
async fn connect_provider(
    exec: &dyn CommandExecutor,
    runtime: &Runtime,
    wanted: Option<&str>,
) -> Result<(String, String), NativeError> {
    let (Some(use_local), Some(models)) = (provider_command(runtime, ProviderAction::UseLocal), provider_command(runtime, ProviderAction::Models)) else {
        // TODO: an OpenAI-compatible runtime (local models, API keys); the config surface exists already.
        return Err(failure("openai_compatible_runtime_not_implemented"));
    };
    let connected = step(exec, "use-local-login", use_local).await?;
    let mut credential = text(&connected, "credentialRef");
    let listed = step(exec, "models", models).await?;
    let models = listed["models"].as_array().cloned().unwrap_or_default();
    let chosen = match wanted {
        Some(wanted) => models.iter().find(|m| m["model"] == wanted).cloned().unwrap_or(json!({"model":wanted})),
        None => models.first().cloned().ok_or_else(|| failure("provider_models_unavailable"))?,
    };
    if credential.is_empty() {
        credential = text(&chosen, "credentialRef");
    }
    if credential.is_empty() {
        return Err(failure("provider_credential_unavailable"));
    }
    Ok((text(&chosen, "model"), credential))
}

/// The owner command for a local runtime's provider; Codex is the default one.
pub fn provider_command(runtime: &Runtime, action: ProviderAction) -> Option<Command> {
    match runtime {
        Runtime::Codex => Some(Command::Provider { action }),
        Runtime::Claude => Some(Command::RuntimeProvider { runtime: "claude".into(), action }),
        Runtime::OpenAiCompatible { .. } => None,
    }
}
pub(super) fn local(agent_id: &str, action: LocalAction, input: Option<Value>) -> Command {
    Command::Local { agent_id: agent_id.to_owned(), action, input, binding_id: None, requester: None }
}

async fn configure(exec: &dyn CommandExecutor, agent_id: &str, spec: &SimpleAgentSpec, model: &str, credential: &str) -> Result<(), NativeError> {
    // The save names the profile it replaces (compare-and-swap): read it first.
    let current = step(exec, "read-model", local(agent_id, LocalAction::AgentModel, None)).await?;
    let profile = profile::model_input(spec, model, credential);
    let input = crate::native_owner::model_save_request(&current["modelProfile"], profile).ok_or_else(|| failure("invalid_model_profile"))?;
    step(exec, "save-model", local(agent_id, LocalAction::SaveAgentModel, Some(input))).await?;
    // Default "who can ask": humans in rooms the owner bound the Agent to are
    // allowed under a daily token budget. The server already drops messages
    // from other Agents, and high-risk tools stay under the per-room
    // AllowWithRules written at bind time (trusted-tools mode only).
    // TODO: AskOwner-once for humans outside bound rooms and for other Agents
    // (depth one) need requester-layer rows the policy model cannot express yet.
    let current = step(exec, "agent-policy", local(agent_id, LocalAction::AgentPolicy, None)).await?;
    // Fail closed: an unexpected reply must not start an Agent that still has
    // its previous (possibly unlimited) budget and high-risk settings.
    let mut policy = current["policy"]["policy"].clone();
    let revision = current["policy"]["revision"].clone();
    if !policy.is_object() || revision.is_null() {
        return Err(failure("invalid_server_response"));
    }
    policy["requests"] = json!("Allow");
    policy["budget"] = json!({"limit":{"Tokens":spec.daily_budget},"period":"UtcDay"});
    policy["high_risk"] = json!(spec.high_risk);
    let input = json!({"expectedRevision":revision,"policy":policy});
    step(exec, "save-policy", local(agent_id, LocalAction::SaveAgentPolicy, Some(input))).await?;
    Ok(())
}

/// "Start responding" (idempotent: an already active runtime counts as started).
pub async fn start_service(exec: &dyn CommandExecutor, agent_id: &str, reservation: u64) -> Result<bool, NativeError> {
    let input = start::input(reservation);
    match step(exec, "start-service", local(agent_id, LocalAction::StartAgentService, Some(input))).await {
        Ok(_) => Ok(true),
        Err(error) if error.code == "agent_runtime_already_active" => Ok(true),
        Err(error) => Err(error),
    }
}

/// Create (or reuse, by display name) a working Agent in one call.
pub async fn create_simple_agent(exec: &dyn CommandExecutor, spec: &SimpleAgentSpec) -> Result<SimpleAgentReport, NativeError> {
    create_simple_agent_as(exec, spec, AgentIdentity::ByName).await
}

/// Like `create_simple_agent`, with the caller choosing which Agent to reuse.
pub async fn create_simple_agent_as(exec: &dyn CommandExecutor, spec: &SimpleAgentSpec, identity: AgentIdentity<'_>) -> Result<SimpleAgentReport, NativeError> {
    // The step record is per setup: forget the previous run's, and blame the
    // running step for failures raised between commands (e.g. a bad reply).
    // TODO: carry the step in the returned error; the atomics are one per process.
    progress::begin();
    let result = run_setup(exec, spec, identity).await;
    if result.is_err() {
        progress::fail_current();
    }
    result
}

async fn run_setup(exec: &dyn CommandExecutor, spec: &SimpleAgentSpec, identity: AgentIdentity<'_>) -> Result<SimpleAgentReport, NativeError> {
    // The local login is checked first: a Mac that is not signed in must not
    // end up with an Agent that has no runtime (seen as a dead "Stopped" Agent).
    let (model, credential) = connect_provider(exec, &spec.runtime, spec.model.as_deref()).await?;
    let (project_id, space_id) = ensure_project(exec, &spec.name).await?;
    let (agent_id, puppet_mxid, _reused) = agents::resolve_agent(exec, &spec.name, identity).await?;
    if let Err(error) = configure(exec, &agent_id, spec, &model, &credential).await {
        if error.code != "agent_execution_device_required" {
            return Err(error);
        }
        // A reused Agent may still be assigned to an earlier device of this owner.
        let details = step(exec, "agent-details", Command::AgentDetails { agent_id: agent_id.clone() }).await?;
        let generation = details["agent"]["generation"].as_i64().unwrap_or(0);
        step(exec, "assign-device", Command::AssignAgentToCurrentDevice { agent_id: agent_id.clone(), expected_generation: generation }).await?;
        configure(exec, &agent_id, spec, &model, &credential).await?;
    }
    let started = start_service(exec, &agent_id, spec.reservation).await?;
    let direct = owner_direct(exec, &agent_id).await?;
    Ok(SimpleAgentReport { agent_id, puppet_mxid, project_id, space_id, model, started, owner_direct_room_id: Some(direct) })
}

/// The private owner chat gives the Agent-wide service its first room and
/// carries owner approvals; setup is not done without it.
pub async fn owner_direct(exec: &dyn CommandExecutor, agent_id: &str) -> Result<String, NativeError> {
    let reply = step(exec, "owner-direct", Command::EnsureAgentOwnerDirect { agent_id: agent_id.to_owned() }).await?;
    reply["ownerDirect"]["roomId"].as_str().or(reply["roomId"].as_str()).map(str::to_owned).ok_or_else(|| failure("owner_direct_room_missing"))
}

#[cfg(test)] mod spec_tests;
#[cfg(test)] mod identity_tests;
#[cfg(test)] mod tests;
