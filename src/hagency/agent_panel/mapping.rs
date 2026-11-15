//! Pure mappings between the agent panel's controls and the existing Agent
//! layer: request policy (who can ask), budget and usage (credit card),
//! issue-watch rooms and the model profile. No widget state lives here.
use crate::hagency::create_agent::ModelChoice;
use crate::hagency::model::{Command, ProviderAction};
use serde_json::{Value, json};

/// `RequestPolicy` wire names (hagency-agent-local `RequestPolicy`).
pub(crate) const REQUESTS_ALLOW: &str = "Allow";
pub(crate) const REQUESTS_DENY: &str = "Deny";
/// `Period` wire names in dropdown order.
pub(crate) const PERIODS: [&str; 3] = ["Lifetime", "UtcDay", "UtcMonth"];
/// Largest token count JSON numbers carry exactly (same bound as the Limits page).
const MAX_EXACT_TOKENS: u64 = 9_007_199_254_740_991;

/// "Who can ask this agent", mapped onto the Agent-layer `requests` policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WhoCanAsk {
    OnlyMe,
    RoomMembers,
    /// Hosted (concierge-style) Agents only; never offered for desktop Agents.
    Anyone,
}
pub(crate) const WHO_CAN_ASK: [WhoCanAsk; 3] = [WhoCanAsk::OnlyMe, WhoCanAsk::RoomMembers, WhoCanAsk::Anyone];
impl WhoCanAsk {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::OnlyMe => "Only me",
            Self::RoomMembers => "People in rooms I add it to",
            Self::Anyone => "Anyone who can reach it (hosted Agents)",
        }
    }
    /// The choice a saved `requests` value shows; `AskOwner` has no simple choice.
    pub(crate) fn from_requests(requests: &str) -> Option<Self> {
        match requests {
            REQUESTS_DENY => Some(Self::OnlyMe),
            REQUESTS_ALLOW => Some(Self::RoomMembers),
            _ => None,
        }
    }
    /// The `requests` value to save; `None` when a desktop Agent cannot have it.
    pub(crate) fn requests(self) -> Option<&'static str> {
        match self {
            Self::OnlyMe => Some(REQUESTS_DENY),
            Self::RoomMembers => Some(REQUESTS_ALLOW),
            Self::Anyone => None,
        }
    }
}

/// The `SaveAgentPolicy` input: the loaded Agent policy with one field
/// replaced, fenced by its revision. `None` until a policy is loaded.
pub(crate) fn policy_change(loaded: &Value, field: &str, value: Value) -> Option<Value> {
    let revision = loaded["policy"]["revision"].as_i64()?;
    let mut policy = loaded["policy"]["policy"].clone();
    if !policy.is_object() {
        return None;
    }
    policy[field] = value;
    Some(json!({"expectedRevision": revision, "policy": policy}))
}

/// The budget object to save, from the Settings controls.
pub(crate) fn budget_value(limited: bool, tokens: &str, period_index: usize) -> Result<Value, &'static str> {
    let period = PERIODS[period_index.min(PERIODS.len() - 1)];
    if !limited {
        return Ok(json!({"limit": "Unlimited", "period": period}));
    }
    let tokens: u64 = tokens.trim().parse().map_err(|_| "Enter a whole number of tokens.")?;
    if tokens > MAX_EXACT_TOKENS {
        return Err("Token count exceeds the supported exact integer range.");
    }
    Ok(json!({"limit": {"Tokens": tokens}, "period": period}))
}
pub(crate) fn period_index(period: &str) -> usize {
    PERIODS.iter().position(|p| *p == period).unwrap_or(0)
}

/// What the credit card shows: one line and the share of credit left.
#[derive(Debug, PartialEq)]
pub(crate) struct Credit {
    pub text: String,
    pub detail: String,
    /// Share of the limit left (0..=1); `None` for no limit.
    pub left: Option<f32>,
}
/// Credit from the Agent policy's budget and its usage (`{spent, held}`).
pub(crate) fn credit(policy: &Value, usage: &Value) -> Credit {
    let spent = usage["spent"].as_u64().unwrap_or(0);
    let held = usage["held"].as_u64().unwrap_or(0);
    let used = spent.saturating_add(held);
    let used_text = crate::i18n::format("Used {spent} · reserved {held}", &[("spent", compact(spent)), ("held", compact(held))]);
    let Some(limit) = policy["budget"]["limit"]["Tokens"].as_u64() else {
        return Credit { text: crate::i18n::tr("No limit").into(), detail: used_text, left: None };
    };
    let left = limit.saturating_sub(used);
    let template = match policy["budget"]["period"].as_str().unwrap_or("") {
        "UtcDay" => "{left} of {limit} tokens left today",
        "UtcMonth" => "{left} of {limit} tokens left this month",
        _ => "{left} of {limit} tokens left in total",
    };
    let text = crate::i18n::format(template, &[("left", compact(left)), ("limit", compact(limit))]);
    let ratio = if limit == 0 { 0.0 } else { (left as f64 / limit as f64) as f32 };
    Credit { text, detail: used_text, left: Some(ratio) }
}
/// 1.2M, 950K, 999: one decimal at most, trailing ".0" dropped.
pub(crate) fn compact(tokens: u64) -> String {
    let short = |value: f64, unit: &str| {
        let text = format!("{value:.1}");
        format!("{}{unit}", text.strip_suffix(".0").unwrap_or(&text))
    };
    match tokens {
        n if n >= 1_000_000 => short(n as f64 / 1_000_000.0, "M"),
        n if n >= 1_000 => short(n as f64 / 1_000.0, "K"),
        n => n.to_string(),
    }
}

/// Approval modes (hagency-agent-local `remembered` wire names) in dropdown order.
pub(crate) const APPROVAL_MODES: [&str; 3] = ["ask", "remember", "outside"];
pub(crate) fn approval_mode_labels() -> [&'static str; 3] {
    ["Ask every time", "Ask, remember approvals", "Ask only for outside changes"]
}
/// The one-line explanation under the Approvals setting for a saved mode.
pub(crate) fn approval_mode_hint(mode: &str) -> &'static str {
    match mode {
        "outside" => "Ask only for outside changes runs reads and edits inside the workspace without asking; GitHub writes, pushes, network tools and other folders still ask.",
        "ask" => "Every tool request asks you first.",
        _ => "Ask, remember approvals reuses what you chose to always allow.",
    }
}
/// Dropdown index for the saved mode (unknown shows "remember", the default).
pub(crate) fn approval_mode_index(mode: &str) -> usize {
    APPROVAL_MODES.iter().position(|m| *m == mode).unwrap_or(1)
}
/// The `{mode}` change to save for a dropdown index.
pub(crate) fn approval_mode_change(index: usize) -> Option<Value> {
    APPROVAL_MODES.get(index).map(|mode| json!({"mode": mode}))
}
/// The `{forget}` change that revokes one remembered rule: exactly its key.
pub(crate) fn forget_change(rule: &Value) -> Value {
    json!({"forget": {"tool": rule["tool"], "directory": rule["directory"], "request": rule["request"]}})
}
/// "Always allow <label> · <directory>".
pub(crate) fn rule_label(rule: &Value) -> String {
    let text = |key: &str| rule[key].as_str().unwrap_or("").to_owned();
    let label = Some(text("label")).filter(|l| !l.is_empty()).unwrap_or_else(|| text("tool"));
    format!("{label} · {}", text("directory"))
}

/// The Agent's project rooms (`{roomId, name}` of active, non-private bindings).
pub(crate) fn watch_rooms(status: &Value) -> Vec<(String, String)> {
    status["rooms"].as_array().into_iter().flatten()
        .filter(|room| room["state"] == "active")
        .filter_map(|room| {
            let id = room["roomId"].as_str().filter(|id| id.starts_with('!'))?;
            let name = room["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(id);
            Some((id.to_owned(), name.to_owned()))
        })
        .collect()
}

/// The local runtime behind a model profile's credential.
pub(crate) fn runtime_of(credential_ref: &str) -> &'static str {
    if credential_ref.starts_with("claude-managed:") {
        "claude"
    } else if credential_ref.starts_with("octos-managed:") {
        "octos"
    } else {
        "codex"
    }
}
pub(crate) fn harness_label(runtime: &str) -> &'static str {
    match runtime {
        "claude" => "Claude",
        "octos" => "Octos",
        _ => "Codex",
    }
}
/// The read-only model list for one runtime, routed like the Agents panel does.
pub(crate) fn models_command(runtime: &str) -> Command {
    match runtime {
        "codex" => Command::Provider { action: ProviderAction::Models },
        other => Command::RuntimeProvider { runtime: other.into(), action: ProviderAction::Models },
    }
}
/// The `SaveAgentModel` input: the saved profile with model and Thinking
/// changed, fenced by the saved profile's digest. `None` if none is loaded.
pub(crate) fn model_input(profile: &Value, choice: &ModelChoice, credential: &str, effort: &str) -> Option<Value> {
    hagency::native_owner::model_save_request(profile, json!({
        "model": choice.model,
        "credential_ref": credential,
        "workspace_root": profile["workspace_root"].as_str().unwrap_or(""),
        "reasoning_effort": effort,
        "web_search": profile["web_search"].as_bool().unwrap_or(true),
    }))
}

/// "Runs on this Mac (Claude)", "Runs on <device>", or not assigned.
pub(crate) fn runs_on(here: bool, device_name: &str, harness: Option<&str>) -> String {
    match (here, harness) {
        (true, Some(harness)) => crate::i18n::format("Runs on this Mac ({harness})", &[("harness", harness.into())]),
        (true, None) => crate::i18n::tr("Runs on this Mac").into(),
        (false, _) if !device_name.is_empty() => crate::i18n::format("Runs on {device}", &[("device", device_name.into())]),
        _ => crate::i18n::tr("Not assigned to a device yet").into(),
    }
}
/// The service line for the mini app's one-word state (`local_state::service_state`).
pub(crate) fn service_line(state: &str) -> &'static str {
    match state {
        "processing" => "Working on a request",
        "running" => "Running",
        "starting" => "Starting…",
        "waiting" => "Waiting for your approval",
        "attention" => "Needs attention",
        "stopped" => "Paused",
        "elsewhere" => "Runs on another device",
        _ => "Status unavailable",
    }
}
/// Whether the service toggle offers Pause (true) or Resume (false).
pub(crate) fn service_running(state: &str) -> bool {
    matches!(state, "processing" | "running" | "starting" | "waiting" | "attention")
}

#[cfg(test)]
#[path = "mapping_tests.rs"]
mod tests;
