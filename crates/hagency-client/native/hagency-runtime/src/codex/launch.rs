//! Native configuration loading shared by provider inspection and Agent hosts.
//! Capability availability follows Codex configuration. Runtime authority still
//! comes from the host's scoped turn, sandbox and private approval responses.
use super::session::Error;
use serde_json::Value;

pub fn native_arguments(shared_auth: bool, web_search: Option<bool>) -> Vec<String> {
    let mut arguments = vec![
        "app-server".into(),
        "--listen".into(),
        "stdio://".into(),
        "-c".into(),
        format!(
            "cli_auth_credentials_store=\"{}\"",
            if shared_auth { "auto" } else { "keyring" }
        ),
    ];
    if let Some(enabled) = web_search {
        arguments.extend([
            "-c".into(),
            format!(
                "web_search=\"{}\"",
                if enabled { "live" } else { "disabled" }
            ),
        ]);
    }
    arguments
}

/// Verify the provider selected by the owner and an explicit web-search policy.
/// Extensions and instructions are native configuration, not evidence of an
/// account mismatch. Do not rewrite or expose their arguments or credentials.
pub fn validate_native_configuration(
    config: &Value,
    openai_account: bool,
    web_search: Option<bool>,
) -> Result<(), Error> {
    if !config.is_object() {
        return Err(Error::Policy);
    }
    if openai_account
        && (config["model_provider"] != "openai"
            || config["chatgpt_base_url"] != "https://chatgpt.com/backend-api"
            || config["model_providers"]
                .get("openai")
                .is_some_and(|value| !value.is_null()))
    {
        return Err(Error::Policy);
    }
    if let Some(enabled) = web_search
        && config["web_search"] != if enabled { "live" } else { "disabled" }
    {
        return Err(Error::Policy);
    }
    Ok(())
}
