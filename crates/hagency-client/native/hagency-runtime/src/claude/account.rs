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

#[derive(Clone)]
pub struct AccountBinding {
    subject: String,
    email: String,
    digest: String,
    expires: Instant,
}
impl AccountBinding {
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
}
impl SharedCredential {
    pub fn from_authenticated_login(binding: AccountBinding, token: String) -> Result<Self, Error> {
        binding.check()?;
        if token.is_empty() || token.len() > 8192 || !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Identity);
        }
        Ok(Self { binding, token })
    }
    pub fn binding(&self) -> &AccountBinding {
        &self.binding
    }
    pub(crate) fn inject(&self, launch: &mut hagency_platform::Launch) -> Result<(), Error> {
        self.binding.check()?;
        launch
            .environment
            .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), self.token.clone().into());
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
        self.provider == "firstParty"
            && self.token_source.as_deref() == Some("CLAUDE_CODE_OAUTH_TOKEN")
            && self.api_key_source.as_deref().is_none_or(|s| s == "none")
            && self
                .email
                .as_deref()
                .is_none_or(|email| email == binding.email)
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
}
