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

/// An immutable native initialization observation, never a capacity proof.
pub(crate) struct Metadata {
    email: String,
    provider: String,
    api_key_source: Option<String>,
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
            email: field(&value["email"])?,
            provider: field(&value["apiProvider"])?,
            api_key_source: match value.get("apiKeySource") {
                None | Some(Value::Null) => None,
                Some(value) => Some(field(value)?),
            },
        })
    }
    pub(crate) fn matches(&self, binding: &AccountBinding) -> bool {
        self.email == binding.email
            && self.provider == "firstParty"
            && self
                .api_key_source
                .as_deref()
                .is_none_or(|source| source == "none")
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
