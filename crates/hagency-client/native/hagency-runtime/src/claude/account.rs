//! Host-verified provider identity, followed by a receipt from the original
//! native process. Neither public CLI metadata nor a credential path supplies
//! an account UUID. The trusted provider adapter verifies its authenticated
//! profile before constructing AccountBinding; no wire route deserializes it.
use super::session::Error;
use hagency_core::canonical;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub const PROVIDER: &str = "anthropic.claude_ai";
/// An API key or a bearer token from the owner's own Claude Code settings, for
/// Anthropic's API or another endpoint that speaks its protocol.
pub const API_PROVIDER: &str = "claude_code.api_credential";
/// Used when the owner's settings name no endpoint.
pub const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";

/// Which credential the host observed, and so which one it injects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    ClaudeAi,
    AuthToken,
    ApiKey,
}

/// A claude.ai sign-in token. It is a subscription credential, never an API
/// credential, whichever variable carries it.
fn subscription_token(credential: &str) -> bool {
    credential.starts_with("sk-ant-oat")
}

/// An http(s) address without credentials, query or fragment; its host in
/// lower case, without the trailing slashes.
fn endpoint(value: &str) -> Option<(String, String)> {
    let value = value.trim_end_matches('/');
    let rest = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))?;
    let authority = rest.split('/').next()?;
    let host = authority
        .rsplit_once(':')
        .filter(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(authority, |(host, _)| host);
    if value.len() > 256
        || host.is_empty()
        || !value.bytes().all(|b| b.is_ascii_graphic())
        || value.contains(['@', '?', '#', '"', '\\'])
    {
        return None;
    }
    Some((value.into(), host.to_ascii_lowercase()))
}

#[derive(Clone)]
pub struct AccountBinding {
    provider: &'static str,
    source: Source,
    subject: String,
    email: String,
    digest: String,
    expires: Instant,
}
impl AccountBinding {
    /// Trusted native provider adapter only, from the credential the CLI's
    /// own settings select. The subject is the endpoint and a digest of the
    /// credential: the same key at the same endpoint is the same account, and
    /// neither value is accepted from Matrix, a webview or model output.
    pub fn from_api_credential(
        base_url: Option<&str>,
        credential: &str,
        key: bool,
        expires: Instant,
    ) -> Result<Self, Error> {
        let (address, host) = endpoint(base_url.unwrap_or(DEFAULT_ENDPOINT)).ok_or(Error::Identity)?;
        let anthropic = host == "anthropic.com" || host.ends_with(".anthropic.com");
        if credential.is_empty()
            || credential.len() > 8192
            || !credential.bytes().all(|b| b.is_ascii_graphic())
            || subscription_token(credential)
            // A bearer token for Anthropic's own API is a claude.ai sign-in.
            || (!key && anthropic)
            || expires <= Instant::now()
            || expires > Instant::now() + Duration::from_secs(60)
        {
            return Err(Error::Identity);
        }
        let fingerprint = canonical::digest(&json!({
            "purpose":"hagency.claude.api-credential.v1","endpoint":address,"credential":credential
        }))
        .map_err(|_| Error::Identity)?;
        let subject = canonical::encode(&json!({"endpoint":address,"credential":fingerprint}))
            .map_err(|_| Error::Identity)?;
        let digest = canonical::digest(&json!({"provider":API_PROVIDER,"subject":subject}))
            .map_err(|_| Error::Identity)?;
        Ok(Self {
            provider: API_PROVIDER,
            source: if key { Source::ApiKey } else { Source::AuthToken },
            subject,
            email: String::new(),
            digest,
            expires,
        })
    }
    /// False for a claude.ai sign-in.
    pub fn is_api_credential(&self) -> bool {
        self.source != Source::ClaudeAi
    }
    pub fn provider(&self) -> &'static str {
        self.provider
    }
    /// Trusted native provider adapter only, after an authenticated profile
    /// request and unchanged selected-login check. Never accept these values
    /// from Matrix, configuration, a webview or model output.
    pub fn from_authenticated_profile(
        account: &str,
        organization: &str,
        email: &str,
        expires: Instant,
    ) -> Result<Self, Error> {
        fn uuid(s: &str) -> Option<String> {
            (s.len() == 36
                && s.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                }))
            .then(|| s.to_ascii_lowercase())
        }
        if email.is_empty()
            || email.len() > 512
            || email.chars().any(char::is_control)
            || expires <= Instant::now()
            || expires > Instant::now() + Duration::from_secs(60)
        {
            return Err(Error::Identity);
        }
        let account = uuid(account).ok_or(Error::Identity)?;
        let organization = uuid(organization).ok_or(Error::Identity)?;
        let subject = canonical::encode(&json!({"account":account,"organization":organization}))
            .map_err(|_| Error::Identity)?;
        let digest = canonical::digest(&json!({"provider":PROVIDER,"subject":subject}))
            .map_err(|_| Error::Identity)?;
        Ok(Self {
            provider: PROVIDER,
            source: Source::ClaudeAi,
            subject,
            email: email.into(),
            digest,
            expires,
        })
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn provider_digest(&self) -> &str {
        &self.digest
    }
    pub fn expires(&self) -> Instant {
        self.expires
    }
    pub fn check(&self) -> Result<(), Error> {
        if Instant::now() < self.expires {
            Ok(())
        } else {
            Err(Error::Identity)
        }
    }
}

/// A transient access token observed by the trusted host at the same time as
/// its authenticated provider identity. Never serialize, log or persist this
/// value, and never put a refresh token in a shared native process.
pub struct SharedCredential {
    binding: AccountBinding,
    token: String,
    endpoint: Option<String>,
    models: Vec<(&'static str, String)>,
}
/// The owner's model mapping that a shared process keeps: what the aliases and
/// the CLI's own background and sub-agent calls resolve to at the endpoint.
pub const MODEL_VARIABLES: [&str; 5] = [
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "CLAUDE_CODE_SUBAGENT_MODEL",
];
impl SharedCredential {
    pub fn from_authenticated_login(binding: AccountBinding, token: String) -> Result<Self, Error> {
        binding.check()?;
        if binding.source != Source::ClaudeAi
            || token.is_empty()
            || token.len() > 8192
            || !token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Identity);
        }
        Ok(Self {
            binding,
            token,
            endpoint: None,
            models: Vec::new(),
        })
    }
    /// The API credential the binding was made from, with the same endpoint.
    /// `models` holds values of MODEL_VARIABLES from the same settings.
    pub fn from_api_settings(
        base_url: Option<&str>,
        credential: String,
        key: bool,
        models: &[(String, String)],
        expires: Instant,
    ) -> Result<Self, Error> {
        let binding = AccountBinding::from_api_credential(base_url, &credential, key, expires)?;
        let address = match base_url {
            Some(value) => Some(endpoint(value).ok_or(Error::Identity)?.0),
            None => None,
        };
        let mut kept = Vec::new();
        for (name, value) in models {
            let name = MODEL_VARIABLES
                .iter()
                .find(|known| *known == name)
                .ok_or(Error::Identity)?;
            if !super::model_name(value) {
                return Err(Error::Identity);
            }
            kept.push((*name, value.clone()));
        }
        Ok(Self {
            binding,
            token: credential,
            endpoint: address,
            models: kept,
        })
    }
    pub fn binding(&self) -> &AccountBinding {
        &self.binding
    }
    pub(crate) fn inject(&self, launch: &mut hagency_platform::Launch) -> Result<(), Error> {
        self.binding.check()?;
        let mut set = |name: &str, value: &str| {
            launch.environment.insert(name.into(), value.into());
        };
        match self.binding.source {
            Source::ClaudeAi => set("CLAUDE_CODE_OAUTH_TOKEN", &self.token),
            Source::AuthToken | Source::ApiKey => {
                set(
                    if self.binding.source == Source::ApiKey {
                        "ANTHROPIC_API_KEY"
                    } else {
                        "ANTHROPIC_AUTH_TOKEN"
                    },
                    &self.token,
                );
                if let Some(endpoint) = &self.endpoint {
                    set("ANTHROPIC_BASE_URL", endpoint);
                }
                for (name, value) in &self.models {
                    set(name, value);
                }
                // Without this the CLI hands the credential to every command
                // a tool runs, and a room member could have it printed.
                set("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1");
            }
        }
        Ok(())
    }
}

/// An immutable native initialization observation, never a capacity proof.
pub(crate) struct Metadata {
    email: Option<String>,
    provider: String,
    api_key_source: Option<String>,
    token_source: Option<String>,
}
impl Metadata {
    pub(crate) fn read(value: &Value) -> Option<Self> {
        fn field(value: &Value) -> Option<String> {
            value
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 512 && !s.chars().any(char::is_control))
                .map(str::to_owned)
        }
        Some(Self {
            email: match value.get("email") {
                None | Some(Value::Null) => None,
                Some(value) => Some(field(value)?),
            },
            provider: field(&value["apiProvider"])?,
            api_key_source: match value.get("apiKeySource") {
                None | Some(Value::Null) => None,
                Some(value) => Some(field(value)?),
            },
            token_source: match value.get("tokenSource") {
                None | Some(Value::Null) => None,
                Some(value) => Some(field(value)?),
            },
        })
    }
    pub(crate) fn matches(&self, binding: &AccountBinding) -> bool {
        self.email.as_deref() == Some(binding.email.as_str())
            && self.provider == "firstParty"
            && self
                .api_key_source
                .as_deref()
                .is_none_or(|source| source == "none")
    }
    pub(crate) fn matches_injected(&self, binding: &AccountBinding) -> bool {
        let none = |source: &Option<String>| source.as_deref().is_none_or(|s| s == "none");
        self.provider == "firstParty"
            && match binding.source {
                Source::ClaudeAi => {
                    self.token_source.as_deref() == Some("CLAUDE_CODE_OAUTH_TOKEN")
                        && none(&self.api_key_source)
                        && self
                            .email
                            .as_deref()
                            .is_none_or(|email| email == binding.email)
                }
                // Claude Code 2.1.322 reports these for the two variables.
                Source::AuthToken => {
                    self.token_source.as_deref() == Some("ANTHROPIC_AUTH_TOKEN")
                        && none(&self.api_key_source)
                        && self.email.is_none()
                }
                Source::ApiKey => {
                    self.api_key_source.as_deref() == Some("ANTHROPIC_API_KEY")
                        && none(&self.token_source)
                        && self.email.is_none()
                }
            }
    }
}

/// Only OwnedClaudeSession can produce the public process receipt. It is
/// bound to the original native connection/turn generation, not a PID or JSON.
pub struct AccountObservation {
    pub(crate) source: Arc<()>,
    pub(crate) binding: AccountBinding,
}
impl AccountObservation {
    pub fn provider_digest(&self) -> &str {
        self.binding.provider_digest()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injected_auth_requires_the_native_token_source_and_host_binding() {
        let binding = AccountBinding::from_authenticated_profile(
            "01234567-89ab-cdef-0123-456789abcdef", "89abcdef-0123-4567-89ab-cdef01234567",
            "owner@example.test", Instant::now() + Duration::from_secs(30)).unwrap();
        let metadata = Metadata::read(&json!({"tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","apiProvider":"firstParty"})).unwrap();
        assert!(!metadata.matches(&binding), "ordinary native login still needs its email observation");
        assert!(metadata.matches_injected(&binding));
        for value in [
            json!({"tokenSource":"apiKey","apiProvider":"firstParty"}),
            json!({"tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","apiProvider":"bedrock"}),
            json!({"tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","apiProvider":"firstParty","apiKeySource":"ANTHROPIC_API_KEY"}),
            json!({"tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","apiProvider":"firstParty","email":"different@example.test"}),
        ] { assert!(!Metadata::read(&value).unwrap().matches_injected(&binding)); }
    }
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }
    #[test]
    fn api_credential_identity_is_the_endpoint_and_the_credential() {
        let gateway = Some("https://Gateway.example:8443/v1/");
        let token = AccountBinding::from_api_credential(gateway, "token-one", false, soon()).unwrap();
        assert!(token.is_api_credential());
        assert_eq!(token.provider(), API_PROVIDER);
        assert!(!token.subject().contains("token-one"), "the credential itself is never the subject");
        let same = AccountBinding::from_api_credential(Some("https://Gateway.example:8443/v1"), "token-one", false, soon()).unwrap();
        assert_eq!(token.provider_digest(), same.provider_digest(), "a trailing slash is not another account");
        for (base, credential, key) in [
            (gateway, "token-two", false),
            (Some("https://other.example"), "token-one", false),
            (None, "token-one", true),
        ] {
            let other = AccountBinding::from_api_credential(base, credential, key, soon()).unwrap();
            assert_ne!(token.provider_digest(), other.provider_digest());
        }
        // Anthropic's own API takes a Console key, never a bearer token, and
        // a claude.ai sign-in token is refused under either variable.
        assert!(AccountBinding::from_api_credential(None, "sk-ant-api03-example", true, soon()).is_ok());
        for (base, credential, key) in [
            (None, "bearer-for-anthropic", false),
            (Some("https://api.anthropic.com"), "bearer-for-anthropic", false),
            (gateway, "sk-ant-oat01-example", false),
            (None, "sk-ant-oat01-example", true),
            (Some("ftp://gateway.example"), "token-one", false),
            (Some("https://user@gateway.example"), "token-one", false),
            (Some("https://gateway.example/?a=b"), "token-one", false),
            (gateway, "", false),
            (gateway, "has space", false),
        ] {
            assert!(AccountBinding::from_api_credential(base, credential, key, soon()).is_err(), "{base:?} {key}");
        }
        assert!(AccountBinding::from_api_credential(gateway, "token-one", false, Instant::now() + Duration::from_secs(61)).is_err());
    }
    #[test]
    fn api_credential_is_injected_with_its_endpoint_mapping_and_the_subprocess_scrub() {
        let models = [("ANTHROPIC_DEFAULT_HAIKU_MODEL".to_string(), "small-model".to_string())];
        let launch = |credential: &SharedCredential| {
            let mut launch = hagency_platform::Launch {
                executable: "/bin/true".into(),
                directory: "/".into(),
                arguments: Vec::new(),
                environment: Default::default(),
                require_crash_containment: false,
            };
            credential.inject(&mut launch).unwrap();
            launch
                .environment
                .into_iter()
                .map(|(k, v)| (k.into_string().unwrap(), v.into_string().unwrap()))
                .collect::<std::collections::BTreeMap<_, _>>()
        };
        let token = SharedCredential::from_api_settings(Some("http://127.0.0.1:4000/"), "token-one".into(), false, &models, soon()).unwrap();
        let environment = launch(&token);
        assert_eq!(environment["ANTHROPIC_AUTH_TOKEN"], "token-one");
        assert_eq!(environment["ANTHROPIC_BASE_URL"], "http://127.0.0.1:4000");
        assert_eq!(environment["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "small-model");
        assert_eq!(environment["CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"], "1");
        assert!(!environment.contains_key("ANTHROPIC_API_KEY") && !environment.contains_key("CLAUDE_CODE_OAUTH_TOKEN"));
        let key = SharedCredential::from_api_settings(None, "sk-ant-api03-example".into(), true, &[], soon()).unwrap();
        let environment = launch(&key);
        assert_eq!(environment["ANTHROPIC_API_KEY"], "sk-ant-api03-example");
        assert_eq!(environment["CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"], "1");
        assert!(!environment.contains_key("ANTHROPIC_BASE_URL") && !environment.contains_key("ANTHROPIC_AUTH_TOKEN"));
        // Only the model mapping travels; any other variable is refused.
        for bad in [("ANTHROPIC_CUSTOM_HEADERS", "x: y"), ("ANTHROPIC_DEFAULT_HAIKU_MODEL", "two words")] {
            let models = [(bad.0.to_string(), bad.1.to_string())];
            assert!(SharedCredential::from_api_settings(None, "sk-ant-api03-example".into(), true, &models, soon()).is_err());
        }
        // A claude.ai binding cannot carry an API credential's fields, and the reverse.
        assert!(SharedCredential::from_authenticated_login(key.binding().clone(), "x".into()).is_err());
    }
    #[test]
    fn injected_api_credential_must_be_the_source_the_process_reports() {
        let token = AccountBinding::from_api_credential(Some("https://gateway.example"), "token-one", false, soon()).unwrap();
        let key = AccountBinding::from_api_credential(None, "sk-ant-api03-example", true, soon()).unwrap();
        let by_token = json!({"tokenSource":"ANTHROPIC_AUTH_TOKEN","apiProvider":"firstParty"});
        let by_key = json!({"tokenSource":"none","apiKeySource":"ANTHROPIC_API_KEY","apiProvider":"firstParty"});
        assert!(Metadata::read(&by_token).unwrap().matches_injected(&token));
        assert!(Metadata::read(&by_key).unwrap().matches_injected(&key));
        assert!(!Metadata::read(&by_token).unwrap().matches_injected(&key));
        assert!(!Metadata::read(&by_key).unwrap().matches_injected(&token));
        for value in [
            json!({"tokenSource":"CLAUDE_CODE_OAUTH_TOKEN","apiProvider":"firstParty"}),
            json!({"tokenSource":"ANTHROPIC_AUTH_TOKEN","apiProvider":"bedrock"}),
            json!({"tokenSource":"ANTHROPIC_AUTH_TOKEN","apiProvider":"firstParty","apiKeySource":"ANTHROPIC_API_KEY"}),
            json!({"tokenSource":"ANTHROPIC_AUTH_TOKEN","apiProvider":"firstParty","email":"owner@example.test"}),
        ] {
            let metadata = Metadata::read(&value).unwrap();
            assert!(!metadata.matches_injected(&token) && !metadata.matches_injected(&key));
            assert!(!metadata.matches(&token), "an API credential never passes as an ordinary sign-in");
        }
    }
}
