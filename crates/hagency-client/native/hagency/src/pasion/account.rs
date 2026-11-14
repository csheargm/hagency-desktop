//! Pasion account calls the desktop makes before anyone is signed in:
//! which upstream providers the server offers (for "Continue with GitHub"),
//! password registration with an optional email, and starting password
//! recovery ("Forgot password?"). All over Pasion's `/auth/api/v1/auth` API.
use serde::Deserialize;
use serde_json::{Value, json};

use super::login::{LoginError, can_login, client as http, step};

/// `brand_name` Pasion uses for its GitHub upstream provider.
pub const GITHUB_BRAND: &str = "github";
/// Max registration steps followed before giving up.
const MAX_REGISTRATION_STEPS: usize = 4;

/// One upstream provider from `GET /api/v1/auth/providers`.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct UpstreamProvider {
    pub id: String,
    #[serde(default)]
    pub brand_name: Option<String>,
    #[serde(default)]
    pub human_name: Option<String>,
}

/// Result of starting a registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Registration {
    /// The username/password already signs in; nothing was created.
    Existing,
    /// The account was created.
    Created,
    /// Pasion emailed a code; finish with [`verify_registration_email`].
    NeedsEmailCode { registration_id: String },
}

fn api(origin: &str, path: &str) -> String {
    format!("{}/auth/api/v1/auth/{path}", origin.trim_end_matches('/'))
}

/// Whether any listed provider is GitHub.
#[must_use]
pub fn offers_github(providers: &[UpstreamProvider]) -> bool {
    providers.iter().any(|p| p.brand_name.as_deref().is_some_and(|b| b.eq_ignore_ascii_case(GITHUB_BRAND)))
}

/// The upstream identity providers Pasion advertises.
pub async fn upstream_providers(origin: &str) -> Result<Vec<UpstreamProvider>, LoginError> {
    let body: Value = http()?.get(api(origin, "providers")).send().await?.error_for_status()?.json().await?;
    serde_json::from_value(body["providers"].clone()).map_err(|e| step("providers", e.to_string()))
}

/// A light syntax check run before the server's own validation.
#[must_use]
pub fn plausible_email(email: &str) -> bool {
    let email = email.trim();
    let Some((local, domain)) = email.split_once('@') else { return false };
    !local.is_empty() && !email.contains(char::is_whitespace) && !domain.contains('@')
        && domain.split('.').count() >= 2 && domain.split('.').all(|part| !part.is_empty())
}

/// `POST /recovery/start`. Pasion answers the same way whether or not the
/// address belongs to an account, so success means only "request accepted".
pub async fn start_recovery(origin: &str, email: &str) -> Result<(), LoginError> {
    let body: Value = http()?.post(api(origin, "recovery/start")).json(&json!({"email": email.trim()})).send().await?.json().await?;
    match body["status"].as_str() {
        Some("success") => Ok(()),
        _ => Err(step("recovery", body["error"].as_str().unwrap_or("unknown_error").to_owned())),
    }
}

/// Register, sending `email` when given. With an email Pasion may stop at
/// `verify_email`; the caller then asks the user for the emailed code.
pub async fn register(origin: &str, username: &str, password: &str, email: Option<&str>) -> Result<Registration, LoginError> {
    if can_login(origin, username, password).await? {
        return Ok(Registration::Existing);
    }
    let mut payload = json!({"username": username, "password": password, "password_confirm": password});
    if let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) {
        payload["email"] = json!(email);
    }
    let response = http()?.post(api(origin, "register")).json(&payload).send().await?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or_default();
    let error = body["error"].as_str().unwrap_or_default().to_ascii_lowercase();
    if error.contains("exist") || error.contains("taken") || status == reqwest::StatusCode::CONFLICT {
        return Ok(Registration::Existing);
    }
    if error == "rate_limited" {
        return Err(step("register", "Pasion rate-limited registration from this address; wait or raise [rate_limiting] registration in pasion.toml"));
    }
    let id = body["id"].as_str().filter(|_| body["status"] == "success").ok_or_else(|| step("register", format!("status {status}: {body}")))?.to_owned();
    follow_steps(origin, &id, username, body["next_step"].as_str().map(str::to_owned)).await
}

/// Submit the emailed code, then finish the registration.
pub async fn verify_registration_email(origin: &str, registration_id: &str, username: &str, code: &str) -> Result<Registration, LoginError> {
    let reply: Value = http()?.post(format!("{}/verify-email", api(origin, &format!("register/{registration_id}"))))
        .json(&json!({"code": code.trim()})).send().await?.json().await.unwrap_or_default();
    if reply["status"] != "success" {
        return Err(step("verify_email", reply["error"].as_str().unwrap_or("invalid_code").to_owned()));
    }
    follow_steps(origin, registration_id, username, reply["next_step"].as_str().map(str::to_owned)).await
}

/// Registration is a short flow: optional email code, display name, finish.
async fn follow_steps(origin: &str, id: &str, username: &str, mut next: Option<String>) -> Result<Registration, LoginError> {
    let base = api(origin, &format!("register/{id}"));
    let http = http()?;
    for _ in 0..MAX_REGISTRATION_STEPS {
        let (path, payload) = match next.as_deref() {
            Some("verify_email") => return Ok(Registration::NeedsEmailCode { registration_id: id.to_owned() }),
            Some("display_name") => ("display-name", json!({"display_name": username, "skip": false})),
            None | Some("finish") | Some("complete") => ("finish", json!({})),
            Some(other) => return Err(step("register", format!("unsupported registration step {other}; register in a browser at {origin}/auth/register"))),
        };
        let reply: Value = http.post(format!("{base}/{path}")).json(&payload).send().await?.json().await.unwrap_or_default();
        if reply["status"] != "success" {
            return Err(step("register", format!("{path}: {reply}")));
        }
        if path == "finish" {
            return Ok(Registration::Created);
        }
        next = reply["next_step"].as_str().map(str::to_owned);
    }
    Err(step("register", "registration did not finish"))
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
