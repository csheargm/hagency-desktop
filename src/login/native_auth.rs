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
    match native::config(origin).await {
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

/// Whether the SDK will authorize as the server's native client.
pub(crate) fn uses_native_client(client: &Client, config: &NativeConfig) -> bool {
    client.oauth().client_id().is_some_and(|id| id.as_str() == config.client_id)
}

/// Password sign-in or sign-up entirely in-app: the native API answers the
/// SDK's own PKCE request with its loopback callback, delivered here.
pub(crate) async fn password_sign_in(client: &Client, origin: String, username: String, password: String, register: bool) -> Result<()> {
    super::homeserver::oauth_browser_login(client, false, |url| async move {
        // Detached: the SDK polls its loopback listener only after we return.
        tokio::spawn(deliver(origin, url, username, password, register));
        Ok(())
    })
    .await
}

/// Call the native API, wait for a new account to be provisioned, then hand
/// the code (or the error, as an OAuth error) to the SDK's listener.
async fn deliver(origin: String, authorize_url: String, username: String, password: String, register: bool) {
    let result = async {
        let callback = native::authorize(&origin, &authorize_url, &username, &password, register).await?;
        if register {
            use super::provision_wait::{wait_for_new_account, PROVISION_POLL, PROVISION_WAIT};
            wait_for_new_account(&origin, &username, PROVISION_WAIT, PROVISION_POLL).await?;
        }
        anyhow::Ok(callback)
    }
    .await;
    let target = match result {
        Ok(callback) => Some(callback),
        Err(error) => error_callback(&authorize_url, &error.to_string()),
    };
    let Some(target) = target else { return };
    // A failed delivery leaves the SDK waiting; its own timeout then reports
    // "Sign-in was cancelled or expired", so the person is never stuck.
    match crate::http::client_builder().build() {
        Ok(http) => {
            if let Err(error) = http.get(target).send().await {
                makepad_widgets::log!("Native sign-in callback failed: {error}");
            }
        }
        Err(error) => makepad_widgets::log!("Native sign-in callback client failed: {error}"),
    }
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

    #[test]
    fn notice_names_the_account() {
        assert!(signed_in_notice("@amy:alpha").contains("@amy:alpha"));
    }
}
