use hagency_runtime::codex::{native_arguments, validate_native_configuration};
use serde_json::json;

#[test]
fn native_extensions_do_not_change_the_owner_account_binding() {
    let config = json!({
        "model_provider":"openai", "chatgpt_base_url":"https://chatgpt.com/backend-api",
        "web_search":"live", "features":{"hooks":true,"plugins":true,"multi_agent":true,"future_feature":{"enabled":true}},
        "skills":{"include_instructions":true,"bundled":{"enabled":true}},
        "mcp_servers":{"owner-tools":{"command":"owner-configured-command","enabled":true}},
        "instructions":"owner instructions", "developer_instructions":"project instructions",
        "project_doc_max_bytes":32768, "include_apps_instructions":true, "include_environment_context":true,
        "notify":["owner-configured-notification"]
    });
    validate_native_configuration(&config, true, Some(true)).unwrap();
    for (field, value) in [
        ("model_provider", json!("different-account")),
        ("chatgpt_base_url", json!("https://other-provider.invalid")),
        (
            "model_providers",
            json!({"openai":{"base_url":"https://other-provider.invalid"}}),
        ),
        ("web_search", json!("disabled")),
    ] {
        let mut changed = config.clone();
        changed[field] = value;
        assert!(
            validate_native_configuration(&changed, true, Some(true)).is_err(),
            "{field}"
        );
    }
}

#[test]
fn native_launch_preserves_extensions_and_only_sets_explicit_host_choices() {
    for shared in [false, true] {
        let args = native_arguments(shared, None);
        assert_eq!(&args[..3], ["app-server", "--listen", "stdio://"]);
        assert_eq!(args.len(), 5);
        assert!(!args.iter().any(|arg| arg == "--disable"
            || arg.starts_with("features.")
            || arg.starts_with("mcp_servers.")));
        for enabled in [false, true] {
            let args = native_arguments(shared, Some(enabled));
            assert_eq!(args.len(), 7);
            assert_eq!(
                args[6],
                format!(
                    "web_search=\"{}\"",
                    if enabled { "live" } else { "disabled" }
                )
            );
        }
    }
}
