//! Pasion's cookieless native sign-in API (`/auth/api/v1/native/*`): the
//! desktop signs in or creates an account with a username and password and
//! receives the authorization code for the SDK's own PKCE request — no
//! browser, no Pasion page, no cookie. Servers without the API answer 404 on
//! `config`, and the caller keeps its older flow.
use serde::Deserialize;
use serde_json::{Value, json};

use super::login::{LoginError, client as http, step};

/// The server's native sign-in settings.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct NativeConfig {
    /// The one public client the API serves; the SDK must use it.
    pub client_id: String,
    pub password_registration_enabled: bool,
    #[serde(default)]
    pub providers: Vec<super::account::UpstreamProvider>,
}

impl NativeConfig {
    /// The provider id for `brand` (e.g. `"google"`), if the server offers it.
    #[must_use]
    pub fn provider(&self, brand: &str) -> Option<&super::account::UpstreamProvider> {
        self.providers.iter().find(|p| p.brand_name.as_deref().is_some_and(|b| b.eq_ignore_ascii_case(brand)))
    }
}

/// Live availability of a sign-up username.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Availability {
    Available(String),
    Taken(String),
    Invalid,
}

/// Query parameters of the SDK authorize URL the API needs.
const AUTHORIZE_FIELDS: [&str; 6] = ["client_id", "redirect_uri", "scope", "state", "code_challenge", "code_challenge_method"];

fn api(origin: &str, path: &str) -> String {
    format!("{}/auth/api/v1/native/{path}", origin.trim_end_matches('/'))
}

/// Native settings, or `None` when this server has no native API.
pub async fn config(origin: &str) -> Result<Option<NativeConfig>, LoginError> {
    let response = http()?.get(api(origin, "config")).send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body: Value = response.error_for_status()?.json().await?;
    serde_json::from_value(body).map(Some).map_err(|e| step("native_config", e.to_string()))
}

/// The request body: the SDK's own authorize parameters plus credentials.
pub fn request_body(authorize_url: &str, username: &str, password: &str) -> Result<Value, LoginError> {
    let url = reqwest::Url::parse(authorize_url).map_err(|e| step("authorize_url", e.to_string()))?;
    let mut body = json!({"username": username, "password": password});
    for field in AUTHORIZE_FIELDS {
        let value = url.query_pairs().find(|(k, _)| k == field).map(|(_, v)| v.into_owned())
            .ok_or_else(|| step("authorize_url", format!("missing {field}")))?;
        body[field] = json!(value);
    }
    Ok(body)
}

/// Sign in (or, with `register`, create the account) and return the
/// loopback callback URL carrying the code. Errors carry the server's code,
/// e.g. `invalid_credentials`, `username_exists`, `rate_limited`.
pub async fn authorize(origin: &str, authorize_url: &str, username: &str, password: &str, register: bool) -> Result<String, LoginError> {
    let body = request_body(authorize_url, username, password)?;
    let path = if register { "register" } else { "login" };
    let response = http()?.post(api(origin, path)).json(&body).send().await?;
    let status = response.status();
    let text = response.text().await?;
    // A gateway page instead of the API's JSON keeps its HTTP status.
    let reply: Value = serde_json::from_str(&text).map_err(|_| step(path, format!("status {status}")))?;
    match (reply["status"].as_str(), reply["redirect_url"].as_str()) {
        (Some("success"), Some(url)) if status.is_success() && same_loopback(authorize_url, url) => Ok(url.to_owned()),
        (Some("success"), _) => Err(step(path, "unexpected redirect")),
        _ => Err(step(path, reply["error"].as_str().unwrap_or("unknown_error").to_owned())),
    }
}

/// The code may only go to the SDK's own loopback listener.
fn same_loopback(authorize_url: &str, callback: &str) -> bool {
    let expected = reqwest::Url::parse(authorize_url).ok()
        .and_then(|u| u.query_pairs().find(|(k, _)| k == "redirect_uri").map(|(_, v)| v.into_owned()))
        .and_then(|r| reqwest::Url::parse(&r).ok());
    let got = reqwest::Url::parse(callback).ok();
    match (expected, got) {
        (Some(e), Some(g)) => g.host_str() == Some("127.0.0.1") && (e.scheme(), e.host_str(), e.port()) == (g.scheme(), g.host_str(), g.port()),
        _ => false,
    }
}

/// `GET username-available`: the lowercased name and whether it is free.
pub async fn username_available(origin: &str, username: &str) -> Result<Availability, LoginError> {
    let mut url = reqwest::Url::parse(&api(origin, "username-available")).map_err(|e| step("username", e.to_string()))?;
    url.query_pairs_mut().append_pair("username", username);
    let reply: Value = http()?.get(url).send().await?.error_for_status()?.json().await?;
    let name = reply["username"].as_str().unwrap_or_default().to_owned();
    Ok(match reply["status"].as_str() {
        Some("available") => Availability::Available(name),
        Some("taken") => Availability::Taken(name),
        _ => Availability::Invalid,
    })
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
