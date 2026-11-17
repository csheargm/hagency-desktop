//! Option D sign-in (evaluation/adr-replace-pasion.md): the desktop uses the
//! server's one native public client, so the password flow is a single
//! cookieless call to Pasion's native API and SSO returns once to the SDK's
//! loopback without a Pasion page. Servers without the native API keep the
//! older browser-emulating flow.

use anyhow::Result;
use matrix_sdk::{authentication::oauth::ClientId, Client};

use ::hagency::pasion::native::{self, NativeConfig};

/// Use the server's native client for this sign-in, if it has one.
/// `None` (no native API, or it could not be read) keeps the older flow, which
/// still signs the person in; the reason is logged.
pub(crate) async fn adopt_native_client(client: &Client, origin: &str) -> Option<NativeConfig> {
    match config_within(origin, CONFIG_DEADLINE).await {
        Ok(Some(config)) => {
            if client.oauth().client_id().is_none() {
                client.oauth().restore_registered_client(ClientId::new(config.client_id.clone()));
            }
            Some(config)
        }
        Ok(None) => None,
        Err(error) => {
            makepad_widgets::log!("Native sign-in config unavailable for {origin}: {error}");
            None
        }
    }
}

/// Longest config discovery may take before the older flow is used instead.
const CONFIG_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// `native::config` bounded by `deadline`: a server that accepts the
/// connection but never answers must not leave sign-in pending forever.
async fn config_within(origin: &str, deadline: std::time::Duration) -> Result<Option<NativeConfig>> {
    let answer = tokio::time::timeout(deadline, native::config(origin))
        .await
        .map_err(|_| anyhow::anyhow!("no answer within {deadline:?}"))?;
    answer.map_err(|error| anyhow::anyhow!("{error}"))
}

/// Whether the SDK will authorize as the server's native client.
#[cfg(test)] // only the adoption test reads it; prod checks via adopt_native_client
pub(crate) fn uses_native_client(client: &Client, config: &NativeConfig) -> bool {
    client.oauth().client_id().is_some_and(|id| id.as_str() == config.client_id)
}

/// Longest the detached native call may run (the SDK itself waits 300 s).
const NATIVE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);

/// Password sign-in or sign-up entirely in-app: the native API answers the
/// SDK's own PKCE request with its loopback callback, delivered here. A
/// native failure is returned with the server's reason, not as "cancelled".
pub(crate) async fn password_sign_in(client: &Client, origin: String, username: String, password: String, register: bool) -> Result<()> {
    let failure = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let task = std::sync::Arc::new(std::sync::Mutex::new(None::<tokio::task::JoinHandle<()>>));
    let (slot, handle) = (failure.clone(), task.clone());
    let result = super::homeserver::oauth_browser_login(client, false, |url| async move {
        // Detached: the SDK polls its loopback listener only after we return.
        let job = tokio::spawn(async move {
            let outcome = tokio::time::timeout(NATIVE_DEADLINE, deliver(origin, url.clone(), username, password, register)).await;
            let error = match outcome {
                Ok(Ok(())) => return,
                Ok(Err(error)) => error.to_string(),
                Err(_) => crate::i18n::tr("Your server did not answer in time. Try again.").to_owned(),
            };
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.clone());
            if let Some(callback) = error_callback(&url, &error) {
                // The SDK learns of the failure here; the reason comes from `slot`.
                let sent = match callback_client() {
                    Ok(http) => http.get(callback).send().await.map(drop).map_err(anyhow::Error::from),
                    Err(error) => Err(error),
                };
                if let Err(error) = sent {
                    makepad_widgets::log!("Native sign-in error callback failed: {error}");
                }
            }
        });
        *handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(job);
        Ok(())
    })
    .await;
    if let Some(job) = task.lock().unwrap_or_else(|e| e.into_inner()).take() {
        job.abort();
    }
    let reason = failure.lock().unwrap_or_else(|e| e.into_inner()).take();
    match (result, reason) {
        (Err(_), Some(reason)) => Err(anyhow::anyhow!(reason)),
        (result, _) => result,
    }
}

/// Loopback-only HTTP: never through a proxy, never following redirects.
fn callback_client() -> Result<matrix_sdk::reqwest::Client> {
    Ok(crate::http::client_builder().no_proxy().redirect(matrix_sdk::reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(8)).build()?)
}

/// Call the native API, wait for a new account to be provisioned, then hand
/// the code to the SDK's listener.
async fn deliver(origin: String, authorize_url: String, username: String, password: String, register: bool) -> Result<()> {
    let callback = native::authorize(&origin, &authorize_url, &username, &password, register).await?;
    if register {
        use super::provision_wait::{wait_for_new_account, PROVISION_POLL, PROVISION_WAIT};
        wait_for_new_account(&origin, &username, PROVISION_WAIT, PROVISION_POLL).await?;
    }
    callback_client()?.get(callback).send().await?;
    Ok(())
}

/// The SDK loopback URL reporting `detail` as an OAuth error for this request.
pub(crate) fn error_callback(authorize_url: &str, detail: &str) -> Option<String> {
    let request = url::Url::parse(authorize_url).ok()?;
    let find = |key: &str| request.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let mut callback = url::Url::parse(&find("redirect_uri")?).ok()?;
    {
        let mut query = callback.query_pairs_mut();
        if let Some(state) = find("state") {
            query.append_pair("state", &state);
        }
        query.append_pair("error", "access_denied").append_pair("error_description", detail);
    }
    Some(callback.to_string())
}

/// Shown once after any sign-in: who the server says this is.
pub(crate) fn signed_in_notice(user_id: &str) -> String {
    crate::i18n::format("Signed in as {user}. Not you? Sign out in Settings and sign in again.", &[("user", user_id.to_owned())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_go_to_the_sdk_loopback_with_its_state() {
        let url = "http://h/auth/authorize?client_id=c&redirect_uri=http%3A%2F%2F127.0.0.1%3A5%2F&state=s1";
        let callback = error_callback(url, "invalid_credentials").unwrap();
        let parsed = url::Url::parse(&callback).unwrap();
        assert_eq!((parsed.host_str(), parsed.port()), (Some("127.0.0.1"), Some(5)));
        let pairs: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
        assert!(pairs.contains(&("state".into(), "s1".into())));
        assert!(pairs.contains(&("error_description".into(), "invalid_credentials".into())));
        assert_eq!(error_callback("http://h/authorize?state=s", "x"), None);
    }

    #[tokio::test]
    async fn config_discovery_gives_up_on_a_stalled_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let _hold = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await;
        });
        let started = std::time::Instant::now();
        assert!(config_within(&origin, std::time::Duration::from_millis(200)).await.is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn notice_names_the_account() {
        assert!(signed_in_notice("@amy:alpha").contains("@amy:alpha"));
    }
}
