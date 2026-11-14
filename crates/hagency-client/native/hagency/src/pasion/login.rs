//! Complete a Pasion OAuth authorization with a username and password over
//! plain HTTP, standing in for the browser the SDK would normally open.
//! Cookies are carried by hand because the shared reqwest build has no jar.
use reqwest::{StatusCode, header};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("pasion login failed at {step}: {detail}")]
    Step { step: &'static str, detail: String },
    #[error(transparent)]
    Http(#[from] reqwest::Error),
}
pub(super) fn step(step: &'static str, detail: impl Into<String>) -> LoginError {
    LoginError::Step { step, detail: detail.into() }
}

pub(super) fn client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
}

fn absolute(origin: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        location.to_owned()
    } else {
        format!("{}{}", origin.trim_end_matches('/'), location)
    }
}

/// Add `new` cookies, replacing any earlier cookie of the same name (a browser
/// jar does; Pasion re-issues `session` at login, and sending both made it
/// read the pre-login one).
fn merge_cookies(cookies: &mut Vec<String>, new: Vec<String>) {
    let name = |c: &String| c.split('=').next().unwrap_or_default().to_owned();
    let names: Vec<String> = new.iter().map(name).collect();
    cookies.retain(|c| !names.contains(&name(c)));
    cookies.extend(new);
}

/// The grant id in a `/login?kind=continue_authorization_grant&id=…` redirect.
fn continued_grant(location: &str) -> Option<String> {
    let url = reqwest::Url::parse("http://x").ok()?.join(location).ok()?;
    let pairs: Vec<(String, String)> = url.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let kind = pairs.iter().find(|(k, _)| k == "kind")?;
    (kind.1 == "continue_authorization_grant").then(|| pairs.iter().find(|(k, _)| k == "id").map(|(_, v)| v.clone())).flatten()
}

/// True when `url` is the SDK's loopback callback named in `authorize_url`.
fn is_client_callback(authorize_url: &str, url: &str) -> bool {
    let redirect = reqwest::Url::parse(authorize_url).ok().and_then(|u| u.query_pairs().find(|(k, _)| k == "redirect_uri").map(|(_, v)| v.into_owned()));
    let (Some(expected), Ok(got)) = (redirect.and_then(|r| reqwest::Url::parse(&r).ok()), reqwest::Url::parse(url)) else { return false };
    (expected.scheme(), expected.host_str(), expected.port_or_known_default()) == (got.scheme(), got.host_str(), got.port_or_known_default())
}

fn cookie_pairs(response: &reqwest::Response) -> Vec<String> {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|raw| raw.split(';').next())
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Drive `authorize_url` (the SDK's own request, including its loopback
/// redirect) to completion as `username`. Returns once the SDK's loopback
/// listener has received the authorization code.
pub async fn authorize(
    origin: &str,
    authorize_url: &str,
    username: &str,
    password: &str,
) -> Result<(), LoginError> {
    let http = client()?;
    let mut cookies: Vec<String> = Vec::new();
    let cookie_header = |cookies: &Vec<String>| cookies.join("; ");

    // 1. The authorize request records the grant and sends us to /login.
    let first = http.get(authorize_url).send().await?;
    merge_cookies(&mut cookies, cookie_pairs(&first));
    if !first.status().is_redirection() {
        return Err(step("authorize", format!("status {}", first.status())));
    }
    // The login page continues this grant (`…&id=<grant>`); after login the
    // front end goes straight to its consent screen, and so do we. Calling
    // authorize again starts a new grant and lands on login again (2026-11-13).
    let first_grant = first.headers().get(header::LOCATION).and_then(|v| v.to_str().ok()).and_then(continued_grant);

    // 2. Password login creates the browser session cookie.
    let login = http
        .post(format!("{}/auth/api/v1/auth/login", origin.trim_end_matches('/')))
        .header(header::COOKIE, cookie_header(&cookies))
        .json(&json!({"username":username,"password":password}))
        .send()
        .await?;
    merge_cookies(&mut cookies, cookie_pairs(&login));
    let body: serde_json::Value = login.json().await.unwrap_or_default();
    if body["status"] != "ok" && body["status"] != "success" && body.get("viewer").is_none() {
        return Err(step("login", body.to_string()));
    }

    // 3. The consent screen of the continued grant, or (older Pasion) what
    //    authorize redirects to once a session exists.
    let location = match first_grant {
        Some(grant) => format!("/consent/{grant}"),
        None => {
            let second = http.get(authorize_url).header(header::COOKIE, cookie_header(&cookies)).send().await?;
            merge_cookies(&mut cookies, cookie_pairs(&second));
            second
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
                .ok_or_else(|| step("consent-redirect", format!("status {}", second.status())))?
        }
    };
    let redirect_url = if let Some(grant) = location.rsplit("/consent/").next().filter(|_| location.contains("/consent/")) {
        let grant = grant.split(['?', '#']).next().unwrap_or(grant);
        // 4. Consent returns the client's callback URL.
        let consent = http
            .post(format!("{}/auth/api/v1/oauth2/consent/{grant}", origin.trim_end_matches('/')))
            .header(header::COOKIE, cookie_header(&cookies))
            .json(&json!({"action":"consent"}))
            .send()
            .await?;
        let body: serde_json::Value = consent.json().await?;
        body["redirect_url"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| step("consent", body.to_string()))?
    } else {
        // Auto-consent: the location is already the client callback.
        absolute(origin, &location)
    };

    // Anything but the client's own callback (a login page again, say) must
    // fail now with the real step, not "succeed" and leave the SDK waiting.
    if !is_client_callback(authorize_url, &redirect_url) {
        return Err(step("consent", format!("unexpected redirect {}", redirect_url.split('?').next().unwrap_or_default())));
    }
    // 5. Deliver the code to the SDK's loopback listener.
    let callback = http.get(&redirect_url).send().await?;
    match callback.status() {
        StatusCode::OK | StatusCode::NO_CONTENT | StatusCode::FOUND | StatusCode::SEE_OTHER => Ok(()),
        status => Err(step("callback", format!("status {status}"))),
    }
}

/// Run [`authorize`] on its own task so the caller can start polling the
/// SDK's loopback listener; the callback in step 5 would otherwise wait for a
/// listener that nobody drives. A failure is delivered to that same listener
/// as an OAuth `access_denied` error, so the caller's login fails at once
/// with the real reason instead of timing out.
pub fn authorize_detached(origin: String, authorize_url: String, username: String, password: String) {
    tokio::spawn(async move {
        if let Err(error) = authorize(&origin, &authorize_url, &username, &password).await {
            if let Some(callback) = error_callback(&authorize_url, &error.to_string()) {
                if let Ok(http) = client() {
                    let _ = http.get(callback).send().await;
                }
            }
        }
    });
}

/// The loopback callback URL that reports `detail` as an OAuth error for the
/// grant in `authorize_url`, or `None` when the URL carries no redirect_uri.
fn error_callback(authorize_url: &str, detail: &str) -> Option<reqwest::Url> {
    let request = reqwest::Url::parse(authorize_url).ok()?;
    let pairs: Vec<(String, String)> = request.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
    let redirect = pairs.iter().find(|(k, _)| k == "redirect_uri")?.1.clone();
    let mut callback = reqwest::Url::parse(&redirect).ok()?;
    {
        let mut query = callback.query_pairs_mut();
        if let Some((_, state)) = pairs.iter().find(|(k, _)| k == "state") {
            query.append_pair("state", state);
        }
        query.append_pair("error", "access_denied").append_pair("error_description", detail);
    }
    Some(callback)
}

/// True when the password already signs this user in (nothing to register).
pub async fn can_login(origin: &str, username: &str, password: &str) -> Result<bool, LoginError> {
    let http = client()?;
    let body: serde_json::Value = http
        .post(format!("{}/auth/api/v1/auth/login", origin.trim_end_matches('/')))
        .json(&json!({"username":username,"password":password}))
        .send()
        .await?
        .json()
        .await
        .unwrap_or_default();
    Ok(body["status"] == "success")
}

/// Self-service registration through Pasion; `AlreadyExists` is not an error
/// so reruns stay idempotent.
/// Register without an email.
pub async fn register(origin: &str, username: &str, password: &str) -> Result<bool, LoginError> {
    use super::account::{register, Registration};
    match register(origin, username, password, None).await? {
        Registration::Existing => Ok(false),
        Registration::Created => Ok(true),
        Registration::NeedsEmailCode { .. } => Err(step("register", format!("this server requires an email code; register in a browser at {origin}/auth/register"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absolute_keeps_full_urls_and_prefixes_paths() {
        assert_eq!(absolute("http://h:1/", "/auth/login?x=1"), "http://h:1/auth/login?x=1");
        assert_eq!(absolute("http://h:1", "http://127.0.0.1:5/?code=a"), "http://127.0.0.1:5/?code=a");
    }
    #[test]
    fn a_reissued_cookie_replaces_the_old_one() {
        let mut jar = vec!["session=before".to_owned(), "other=1".to_owned()];
        merge_cookies(&mut jar, vec!["session=after".to_owned()]);
        assert_eq!(jar, vec!["other=1".to_owned(), "session=after".to_owned()]);
    }
    #[test]
    fn the_login_redirect_names_the_grant_to_consent() {
        assert_eq!(continued_grant("/auth/login?kind=continue_authorization_grant&id=01ABC").as_deref(), Some("01ABC"));
        assert_eq!(continued_grant("/auth/login?kind=other&id=01ABC"), None);
        assert_eq!(continued_grant("/auth/consent/01ABC"), None);
    }
    #[test]
    fn only_the_sdk_loopback_counts_as_the_callback() {
        let url = "http://h:1/auth/authorize?client_id=c&redirect_uri=http%3A%2F%2F127.0.0.1%3A5%2F&state=s1";
        assert!(is_client_callback(url, "http://127.0.0.1:5/?state=s1&code=x"));
        assert!(!is_client_callback(url, "http://h:1/auth/login?kind=continue_authorization_grant&id=g"));
        assert!(!is_client_callback(url, "http://127.0.0.1:6/?code=x"));
        assert!(!is_client_callback(url, "http://127.0.0.1:55/?code=x"));
    }
    #[test]
    fn error_callback_targets_the_loopback_with_state() {
        let url = "http://h:1/auth/authorize?client_id=c&redirect_uri=http%3A%2F%2F127.0.0.1%3A5%2F&state=s1&scope=x";
        let callback = error_callback(url, "pasion login failed").unwrap();
        assert_eq!(callback.host_str(), Some("127.0.0.1"));
        assert_eq!(callback.port(), Some(5));
        let q: Vec<(String, String)> = callback.query_pairs().map(|(k, v)| (k.into_owned(), v.into_owned())).collect();
        assert!(q.contains(&("state".into(), "s1".into())));
        assert!(q.contains(&("error".into(), "access_denied".into())));
        assert!(error_callback("http://h:1/auth/authorize?state=s", "x").is_none());
    }
}
