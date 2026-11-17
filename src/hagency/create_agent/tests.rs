use super::*;
use crate::hagency::quick_setup::default_spec;
use serde_json::json;

fn claude_models() -> Value {
    json!({"models":[
        {"model":"sonnet","displayName":"Claude Sonnet","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"low"},{"reasoningEffort":"high"}]},
        {"model":"opus","displayName":"Claude Opus","defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"},{"reasoningEffort":"max"}]}]})
}
fn codex_models() -> Value {
    json!({"models":[
        {"model":"gpt-mini","displayName":"","supportedReasoningEfforts":[{"reasoningEffort":"low"}]},
        {"model":"gpt-6.1","displayName":"GPT-6.1","isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}]}]})
}
fn ready(claude: Option<Value>, codex: Option<Value>) -> DialogState {
    let mut state = DialogState::new(|runtime| default_name("ann", runtime));
    let probe = |reply: Option<Value>| Some(reply.map(|r| parse_models(&r)).ok_or_else(|| "local_provider_not_signed_in".to_owned()));
    state.probes = [probe(claude), probe(codex), probe(None)];
    state
}

#[test]
fn provider_default_model_comes_first_and_labels_fall_back_to_the_id() {
    let models = parse_models(&codex_models());
    assert_eq!(models.iter().map(|m| m.model.as_str()).collect::<Vec<_>>(), ["gpt-6.1", "gpt-mini"]);
    assert_eq!(models[1].label, "gpt-mini");
    assert!(parse_models(&json!({})).is_empty());
}
#[test]
fn defaults_are_claude_first_model_and_medium() {
    let spec = ready(Some(claude_models()), Some(codex_models())).spec(default_spec).unwrap();
    assert_eq!(spec.name, "ann-claude");
    assert_eq!(spec.runtime, Runtime::Claude);
    assert_eq!(spec.model.as_deref(), Some("sonnet"));
    assert_eq!(spec.reasoning_effort.as_deref(), Some("medium"));
    // One-click policy defaults are kept.
    assert_eq!(spec.high_risk, ::hagency::simple_agent::HIGH_RISK_ASK_OWNER);
}
#[test]
fn only_harnesses_with_models_are_offered() {
    let state = ready(None, Some(codex_models()));
    assert_eq!(state.available(), vec![1]);
    let spec = state.spec(default_spec).unwrap();
    assert_eq!(spec.runtime, Runtime::Codex);
    assert_eq!(spec.model.as_deref(), Some("gpt-6.1"));
    let none = ready(None, None);
    assert!(none.available().is_empty() && !none.checking());
    assert!(none.spec(default_spec).is_err());
}
#[test]
fn chosen_model_and_thinking_map_to_the_profile() {
    let mut state = ready(Some(claude_models()), Some(codex_models()));
    state.model = 1;
    state.thinking = 2;
    let spec = state.spec(default_spec).unwrap();
    assert_eq!(spec.model.as_deref(), Some("opus"));
    assert_eq!(spec.reasoning_effort.as_deref(), Some("high"));
}
#[test]
fn unsupported_thinking_falls_back_to_the_model_default() {
    let models = parse_models(&claude_models());
    assert_eq!(effort_for(&models[1], "low"), "medium");
    let codex = parse_models(&codex_models());
    assert_eq!(effort_for(&codex[0], "low"), "medium");
    assert_eq!(effort_for(&codex[1], "high"), "low"); // no default listed: first offered
    let open = ModelChoice { model: "m".into(), label: "m".into(), efforts: vec![], default_effort: String::new() };
    assert_eq!(effort_for(&open, "high"), "high");
}
#[test]
fn names_follow_the_server_rule() {
    assert_eq!(validate_name("  ann-claude ").unwrap(), "ann-claude");
    assert!(validate_name("   ").is_err());
    assert!(validate_name(&"a".repeat(65)).is_err());
    assert!(validate_name(&"界".repeat(64)).is_ok());
    assert!(validate_name("two\nlines").is_err());
    let mut state = ready(Some(claude_models()), None);
    state.name = String::new();
    assert!(state.spec(default_spec).is_err());
}
#[test]
fn checking_until_every_probe_answers() {
    let mut state = DialogState::new(|runtime| default_name("ann", runtime));
    assert!(state.checking());
    state.probes[0] = Some(Ok(parse_models(&claude_models())));
    assert!(state.checking());
    assert_eq!(state.available(), vec![0]);
}
#[test]
fn default_names_follow_the_harness() {
    assert_eq!(default_name("ben", &Runtime::Codex), "ben-codex");
    assert_eq!(THINKING[DialogState::new(|_| String::new()).thinking], ::hagency::simple_agent::DEFAULT_REASONING_EFFORT);
}

#[test]
fn failures_name_the_step_and_say_what_to_do() {
    let message = failure_message("start-service", "invalid_arguments");
    assert!(message.contains("\"start-service\"") && message.contains("invalid_arguments") && message.contains("malformed"), "{message}");
    assert!(failure_message("models", "zzz").contains("Try again"));
}
#[test]
fn backup_is_none_unless_a_listed_model_is_chosen() {
    let mut state = ready(Some(claude_models()), None);
    assert_eq!(state.backup_choice(), None);
    state.backups = vec!["ollama:gpt-oss:120b-cloud|cloud".into(), "ollama:qwen3:8b|local".into()];
    assert_eq!(state.backup_choice(), None, "row 0 is None");
    state.backup = 2;
    assert_eq!(state.backup_choice().as_deref(), Some("ollama:qwen3:8b|local"));
}
#[test]
fn revoked_agent_sign_in_asks_to_sign_in_again_not_for_a_provider_login() {
    // Regression: a revoked owner sign-in showed "No Claude Code or Codex login found".
    let mut state = ready(None, None);
    assert!(!state.needs_sign_in());
    state.probes = std::array::from_fn(|_| Some(Err("native_resume_revoked".into())));
    assert!(state.needs_sign_in());
    assert!(state.available().is_empty());
    state.probes[1] = None;
    assert!(state.needs_sign_in() && state.checking());
}
#[test]
fn account_mismatch_offers_sign_in_again_not_a_generic_error() {
    // Regression: @Anna vs @anna showed "operation could not finish" with no way out.
    let mut state = ready(None, None);
    state.probes = std::array::from_fn(|_| Some(Err("matrix_account_mismatch".into())));
    assert!(state.needs_sign_in() && state.account_mismatch());
}

#[test]
fn ollama_local_is_offered_with_its_local_models() {
    let mut state = ready(None, None);
    state.probes[2] = Some(Ok(parse_models(&json!({"models":[{"model":"qwen3:4b","displayName":"qwen3:4b"}]}))));
    assert_eq!(state.runtime(), Some(Runtime::Ollama));
    assert_eq!(harness_label(&Runtime::Ollama), "Ollama (local)");
    state.name = default_name("ann", &Runtime::Ollama);
    let spec = state.spec(default_spec).unwrap();
    assert_eq!((spec.name.as_str(), spec.runtime, spec.model.as_deref()), ("ann-ollama", Runtime::Ollama, Some("qwen3:4b")));
}
