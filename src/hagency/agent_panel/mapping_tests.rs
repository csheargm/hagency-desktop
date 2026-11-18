use super::*;

#[test]
fn who_can_ask_round_trips_through_the_request_policy() {
    assert_eq!(WhoCanAsk::from_requests("Deny"), Some(WhoCanAsk::OnlyMe));
    assert_eq!(WhoCanAsk::from_requests("Allow"), Some(WhoCanAsk::RoomMembers));
    assert_eq!(WhoCanAsk::from_requests("AskOwner"), None, "AskOwner is not one of the simple choices");
    for choice in [WhoCanAsk::OnlyMe, WhoCanAsk::RoomMembers] {
        assert_eq!(WhoCanAsk::from_requests(choice.requests().unwrap()), Some(choice));
    }
    assert_eq!(WhoCanAsk::Anyone.requests(), None, "desktop Agents never save Anyone");
    assert_eq!(WHO_CAN_ASK[1], WhoCanAsk::RoomMembers, "the default sits in the middle");
}

#[test]
fn policy_change_keeps_the_other_fields_and_the_revision() {
    let loaded = json!({"policy": {"revision": 7, "policy": {
        "budget": {"limit": {"Tokens": 5}, "period": "UtcDay"}, "requests": "Allow", "high_risk": "AskOwner"}}});
    let change = policy_change(&loaded, "requests", json!(REQUESTS_DENY)).unwrap();
    assert_eq!(change["expectedRevision"], 7);
    assert_eq!(change["policy"]["requests"], "Deny");
    assert_eq!(change["policy"]["high_risk"], "AskOwner", "high-risk tools stay with the owner");
    assert_eq!(change["policy"]["budget"]["limit"]["Tokens"], 5);
    assert!(policy_change(&json!({}), "requests", json!("Deny")).is_none(), "nothing saves before a load");
}

#[test]
fn credit_shows_tokens_left_for_the_period() {
    let policy = json!({"budget": {"limit": {"Tokens": 2_000_000}, "period": "UtcDay"}});
    let credit = credit(&policy, &json!({"spent": 700_000, "held": 100_000}));
    assert_eq!(credit.text, "1.2M of 2M tokens left today");
    assert!((credit.left.unwrap() - 0.6).abs() < 1e-6);
    let month = json!({"budget": {"limit": {"Tokens": 1000}, "period": "UtcMonth"}});
    assert_eq!(super::credit(&month, &json!({"spent": 1500, "held": 0})).text, "0 of 1K tokens left this month");
    assert_eq!(super::credit(&month, &json!({"spent": 1500, "held": 0})).left, Some(0.0), "overspend never goes negative");
    let zero = json!({"budget": {"limit": {"Tokens": 0}, "period": "Lifetime"}});
    assert_eq!(super::credit(&zero, &json!({})).left, Some(0.0));
    assert!(super::credit(&zero, &json!({})).text.ends_with("in total"));
}

#[test]
fn unlimited_credit_says_no_limit_and_still_reports_use() {
    let credit = credit(&json!({"budget": {"limit": "Unlimited", "period": "Lifetime"}}), &json!({"spent": 12_345, "held": 0}));
    assert_eq!(credit.text, "No limit");
    assert_eq!(credit.left, None);
    assert_eq!(credit.detail, "Used 12.3K · reserved 0");
}

#[test]
fn compact_token_counts() {
    assert_eq!(compact(999), "999");
    assert_eq!(compact(1_000), "1K");
    assert_eq!(compact(950_000), "950K");
    assert_eq!(compact(1_250_000), "1.2M");
    assert_eq!(compact(2_000_000), "2M");
}

#[test]
fn budget_values_validate_tokens() {
    assert_eq!(budget_value(false, "", 1).unwrap(), json!({"limit": "Unlimited", "period": "UtcDay"}));
    assert_eq!(budget_value(true, " 500 ", 2).unwrap(), json!({"limit": {"Tokens": 500}, "period": "UtcMonth"}));
    assert!(budget_value(true, "-1", 0).is_err());
    assert!(budget_value(true, "9007199254740992", 0).is_err());
    assert_eq!(period_index("UtcMonth"), 2);
    assert_eq!(period_index("bogus"), 0);
}

#[test]
fn model_profile_and_runtime_mapping() {
    assert_eq!(runtime_of("claude-managed:x"), "claude");
    assert_eq!(runtime_of("octos-managed:x"), "octos");
    assert_eq!(runtime_of("keychain:codex"), "codex");
    assert!(matches!(models_command("codex"), Command::Provider { action: ProviderAction::Models }));
    assert!(matches!(models_command("claude"), Command::RuntimeProvider { ref runtime, action: ProviderAction::Models } if runtime == "claude"));
    let choice = ModelChoice { model: "m2".into(), label: "M2".into(), efforts: vec![], default_effort: String::new() };
    let profile = json!({"model": "m1", "credential_ref": "c", "workspace_root": "/w", "reasoning_effort": "low", "web_search": false});
    let input = model_input(&profile, &choice, "c", "high").expect("a saved profile fences the save");
    assert_eq!(input["profile"], json!({"model": "m2", "credential_ref": "c", "workspace_root": "/w", "reasoning_effort": "high", "web_search": false}));
    assert!(input["expectedProfileDigest"].is_string(), "the save carries the saved profile's digest");
    assert!(model_input(&json!("not a profile"), &choice, "c", "high").is_none());
}

#[test]
fn approval_modes_and_rules_map_to_the_owner_call() {
    assert_eq!(approval_mode_labels()[approval_mode_index("outside")], "Ask only for outside changes");
    assert_eq!(approval_mode_index("bogus"), 1, "unknown shows the default, remember");
    assert_eq!(approval_mode_change(0), Some(json!({"mode": "ask"})));
    assert_eq!(approval_mode_change(3), None);
    let rule = json!({"tool": "claude.Bash", "directory": "/w", "request": "{\"command\":\"gh pr list\"}", "label": "gh pr list", "created": 5});
    assert_eq!(forget_change(&rule), json!({"forget": {"tool": "claude.Bash", "directory": "/w", "request": "{\"command\":\"gh pr list\"}"}}));
    assert_eq!(rule_label(&rule), "gh pr list · /w");
}

#[test]
fn runs_on_and_service_lines() {
    assert_eq!(runs_on(true, "Mac", Some("Claude")), "Runs on this Mac (Claude)");
    assert_eq!(runs_on(false, "Studio", None), "Runs on Studio");
    assert_eq!(runs_on(false, "", None), "Not assigned to a device yet");
    assert!(service_running("running") && service_running("waiting"));
    assert!(!service_running("stopped") && !service_running("elsewhere"));
    assert_eq!(service_line("stopped"), "Paused");
    assert_eq!(service_line("claude_sign_in"), "Claude sign-in needed — run `claude login`");
    assert_eq!(service_line("codex_sign_in"), "Codex sign-in needed — run `codex login`");
    assert!(service_running("claude_sign_in"), "a signed-out Agent is still started: Pause stays offered");
}

#[test]
fn watch_rooms_lists_active_project_rooms() {
    let status = json!({"rooms": [
        {"roomId": "!a:s", "name": "Project", "state": "active"},
        {"roomId": "!b:s", "name": "", "state": "active"},
        {"roomId": "!c:s", "name": "Old", "state": "revoked"},
        {"roomId": "", "name": "Broken", "state": "active"}]});
    assert_eq!(watch_rooms(&status), vec![("!a:s".to_owned(), "Project".to_owned()), ("!b:s".to_owned(), "!b:s".to_owned())]);
    assert!(watch_rooms(&json!({})).is_empty());
}

#[test]
fn saved_model_alias_and_display_name_show_the_same_label() {
    let choices = vec![ModelChoice { model: "sonnet".into(), label: "Claude Sonnet".into(), efforts: vec![], default_effort: String::new() }];
    for saved in ["sonnet", "Claude Sonnet", "claude sonnet"] {
        assert_eq!(model_position(&choices, saved), Some(0), "{saved} selects the provider's Sonnet");
        assert_eq!(model_label(&choices, saved), "Claude Sonnet");
        assert_eq!(model_label(&[], saved), "Claude Sonnet", "{saved} before the choices load");
    }
    assert_eq!(model_label(&[], "opus"), "Claude Opus");
    assert_eq!(model_label(&choices, "gpt-5"), "gpt-5", "an unknown model shows as saved");
    assert_eq!(model_position(&choices, "opus"), None);
}
