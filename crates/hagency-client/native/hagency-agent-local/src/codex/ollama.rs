//! "Ollama (local)": the same Codex app-server runtime, pointed at a model the
//! owner serves on this machine. Approvals, accounting, cancellation and the
//! ledger are the Codex runtime's own; only the provider binding differs.
//!
//! A desktop Agent accepts only a loopback host; a headless server accepts the
//! operator's own configured host. Ollama cloud models are always refused, so
//! room content never reaches ollama.com through this provider.
use super::{Error, Result};
use serde_json::Value;

/// Codex provider ID reserved for this binding (never a built-in ID).
pub const PROVIDER_ID: &str = "hagency-ollama";
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:11434";
const LOOPBACK_HOSTS: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];
const MAX_MODEL_BYTES: usize = 128;

/// An owner-chosen local model. Constructed only through [`LocalModel::new`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalModel {
    endpoint: String,
    origin: Origin,
}
/// Where the model runs: the user's machine, or the server operator's own
/// Ollama host (same VPC). Neither is an Ollama cloud model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    ThisMachine,
    ServerHost,
}
impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Self::ThisMachine => "Ollama (local)",
            Self::ServerHost => "Ollama (this server's own model)",
        }
    }
}
impl LocalModel {
    /// `endpoint` is `http://<loopback>:<port>` with no path.
    pub fn new(endpoint: &str) -> Result<Self> {
        let rest = endpoint
            .strip_prefix("http://")
            .ok_or(Error::Profile("Ollama endpoint must be http on loopback"))?;
        let (host, port) = rest
            .rsplit_once(':')
            .ok_or(Error::Profile("Ollama endpoint needs a port"))?;
        if !LOOPBACK_HOSTS.contains(&host) {
            return Err(Error::Profile("Ollama endpoint must be on this machine"));
        }
        if port.is_empty() || port.len() > 5 || port.parse::<u16>().map_or(true, |p| p == 0) {
            return Err(Error::Profile("Ollama endpoint port invalid"));
        }
        Ok(Self { endpoint: format!("http://{host}:{port}"), origin: Origin::ThisMachine })
    }
    /// A server operator's configured Ollama host (headless hosted Agents).
    /// Any host name is allowed, but only an explicit operator setting reaches
    /// here, and cloud models stay refused by [`validate_model`].
    pub fn server(endpoint: &str) -> Result<Self> {
        let (scheme, rest) = endpoint
            .split_once("://")
            .filter(|(s, _)| matches!(*s, "http" | "https"))
            .ok_or(Error::Profile("Ollama endpoint must be http or https"))?;
        let (host, port) = rest.rsplit_once(':').ok_or(Error::Profile("Ollama endpoint needs a port"))?;
        let host_ok = !host.is_empty()
            && host.len() <= 253
            && host.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'[' | b']' | b':'));
        if !host_ok || port.parse::<u16>().map_or(true, |p| p == 0) {
            return Err(Error::Profile("Ollama endpoint invalid"));
        }
        Ok(Self { endpoint: format!("{scheme}://{host}:{port}"), origin: Origin::ServerHost })
    }
    pub fn origin(&self) -> Origin {
        self.origin
    }
    /// `scheme://host:port` with no path.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    pub fn base_url(&self) -> String {
        format!("{}/v1", self.endpoint)
    }
    /// `-c` overrides selecting this provider. Web search stays off: it would
    /// run through a remote service, not the local model.
    pub fn overrides(&self) -> Vec<String> {
        vec![
            format!("model_provider=\"{PROVIDER_ID}\""),
            format!("model_providers.{PROVIDER_ID}.name=\"{}\"", self.origin.label()),
            format!("model_providers.{PROVIDER_ID}.base_url=\"{}\"", self.base_url()),
            format!("model_providers.{PROVIDER_ID}.wire_api=\"responses\""),
            format!("model_providers.{PROVIDER_ID}.requires_openai_auth=false"),
        ]
    }
    /// The effective configuration must still name exactly this provider.
    pub fn verify(&self, config: &Value) -> Result<()> {
        let provider = &config["model_providers"][PROVIDER_ID];
        if config["model_provider"] != PROVIDER_ID
            || provider["base_url"] != self.base_url().as_str()
            || provider["requires_openai_auth"] == true
            || config["web_search"] != "disabled"
        {
            return Err(Error::Profile("local model provider differs"));
        }
        Ok(())
    }
}
/// Refuses Ollama cloud models (`gpt-oss:120b-cloud`, `x:cloud`): Ollama
/// forwards them to ollama.com, so they are not local.
pub fn validate_model(model: &str) -> Result<()> {
    let tag = model.rsplit_once(':').map_or("", |(_, tag)| tag);
    if model.is_empty()
        || model.len() > MAX_MODEL_BYTES
        || tag == "cloud"
        || tag.ends_with("-cloud")
        || model.ends_with("-cloud")
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b':' | b'/'))
    {
        return Err(Error::Profile("model is not a local Ollama model"));
    }
    Ok(())
}
/// `account/read` for a local provider: no OpenAI login is required or used.
pub fn verify_account(response: &Value) -> Result<()> {
    if response["requiresOpenaiAuth"] == true {
        return Err(Error::Profile("local model provider requires an account"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_loopback_http_endpoints_are_accepted() {
        for ok in ["http://127.0.0.1:11434", "http://localhost:1", "http://[::1]:11434"] {
            assert!(LocalModel::new(ok).is_ok(), "{ok}");
        }
        for bad in [
            "https://127.0.0.1:11434", "http://10.0.0.5:11434", "http://127.0.0.1",
            "http://127.0.0.1:0", "http://127.0.0.1:11434/v1", "http://evil.test:11434",
            "http://127.0.0.1.evil.test:80", "http://user@127.0.0.1:80", "http://127.0.0.1:99999",
        ] {
            assert!(LocalModel::new(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn server_hosts_are_labelled_as_the_servers_model() {
        let s = LocalModel::server("http://ollama.internal:11434").unwrap();
        assert_eq!(s.origin(), Origin::ServerHost);
        assert!(s.overrides().iter().any(|v| v.contains("this server's own model")));
        assert_eq!(LocalModel::new(DEFAULT_ENDPOINT).unwrap().origin(), Origin::ThisMachine);
        for bad in ["ftp://h:1", "http://h", "http://h:0", "http://a b:1", "http://h:1/v1", "http://u@h:1"] {
            assert!(LocalModel::server(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn cloud_models_are_not_local() {
        for ok in ["qwen3:4b", "llama3.2:3b", "library/qwen3:8b", "qwen3"] {
            assert!(validate_model(ok).is_ok(), "{ok}");
        }
        for bad in ["gpt-oss:120b-cloud", "x:cloud", "deepseek-cloud", "", "a b", "q\"x"] {
            assert!(validate_model(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn effective_configuration_must_name_this_provider() {
        let local = LocalModel::new(DEFAULT_ENDPOINT).unwrap();
        let good = json!({"model_provider":PROVIDER_ID,"web_search":"disabled",
            "model_providers":{PROVIDER_ID:{"base_url":"http://127.0.0.1:11434/v1","requires_openai_auth":false}}});
        local.verify(&good).unwrap();
        let mut c = good.clone(); c["model_provider"] = json!("openai");
        assert!(local.verify(&c).is_err());
        let mut c = good.clone(); c["model_providers"][PROVIDER_ID]["base_url"] = json!("https://api.example.test/v1");
        assert!(local.verify(&c).is_err());
        let mut c = good.clone(); c["web_search"] = json!("live");
        assert!(local.verify(&c).is_err());
        let mut c = good; c["model_providers"][PROVIDER_ID]["requires_openai_auth"] = json!(true);
        assert!(local.verify(&c).is_err());
    }
    #[test]
    fn local_account_needs_no_openai_login() {
        verify_account(&json!({"requiresOpenaiAuth":false,"account":null})).unwrap();
        assert!(verify_account(&json!({"requiresOpenaiAuth":true})).is_err());
    }
    #[test]
    fn overrides_select_the_reserved_provider() {
        let o = LocalModel::new(DEFAULT_ENDPOINT).unwrap().overrides();
        assert!(o.contains(&format!("model_provider=\"{PROVIDER_ID}\"")));
        assert!(o.iter().any(|v| v.ends_with("base_url=\"http://127.0.0.1:11434/v1\"")));
    }
}
