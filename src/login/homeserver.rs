//! Resolve sign-in destinations before sending any credentials.
use anyhow::{bail, Result};
use std::future::IntoFuture;
use matrix_sdk::{
    config::RequestConfig,
    ruma::{
        api::client::uiaa::UserIdentifier,
        UserId,
    },
    Client,
};
use url::Url;

/// An explicit server wins; otherwise discover from a full Matrix ID. A local
/// username requires an explicit Hagency server. Email domains are never treated as homeservers.
pub fn login_server(user: &str, server: Option<&str>) -> Result<String> {
    let user = user.trim();
    let server = server.map(str::trim).filter(|s| !s.is_empty());
    if let Some(server) = server {
        if server.chars().any(char::is_whitespace) || server.contains('\\') {
            bail!("Enter a server name or an HTTP/HTTPS homeserver URL.");
        }
        if server.contains("://") {
            let url = Url::parse(server)?;
            if !matches!(url.scheme(), "https" | "http")
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
            {
                bail!("Use an HTTP/HTTPS homeserver URL without credentials, query, or fragment.");
            }
            return Ok(url.to_string());
        }
        let name = matrix_sdk::ruma::ServerName::parse(server)
            .map_err(|_| anyhow::anyhow!("Enter a valid Hagency server name."))?;
        return Ok(name.to_string());
    }
    if user.starts_with('@') && user.contains(':') {
        let id = UserId::parse(user)
            .map_err(|_| anyhow::anyhow!("Use a Matrix ID like @name:example.org."))?;
        return Ok(id.server_name().to_string());
    }
    if !user.starts_with('@') && user.contains('@') {
        bail!("Enter your homeserver when signing in with an email address.");
    }
    bail!("Enter your Hagency server address.")
}

pub fn password_identifier(user: &str) -> Result<UserIdentifier> {
    let user = user.trim();
    if user.is_empty() || user.chars().any(char::is_whitespace) {
        bail!("Enter your Matrix ID, username, or registered email address.");
    }
    if !user.starts_with('@') && user.contains('@') {
        return Ok(UserIdentifier::Email(
            matrix_sdk::ruma::api::client::uiaa::EmailUserIdentifier::new(user.to_owned()),
        ));
    }
    if user.contains(':') {
        UserId::parse(user)
            .map_err(|_| anyhow::anyhow!("Use a Matrix ID like @name:example.org."))?;
    }
    Ok(UserIdentifier::Matrix(
        matrix_sdk::ruma::api::client::uiaa::MatrixUserIdentifier::new(
            if user.contains(':') {
                user
            } else {
                user.trim_start_matches('@')
            }
            .to_owned(),
        ),
    ))
}

#[derive(Clone, Debug)]
pub struct LoginProvider {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct LoginMethods {
    pub homeserver: String,
    pub password: bool,
    pub sso: bool,
    pub oauth: bool,
    pub oauth_aware_preferred: bool,
    pub browser_registration: bool,
    pub providers: Vec<LoginProvider>,
}

pub(crate) async fn require_hagency(client: &Client) -> Result<()> {
    hagency::server_admission::discover(client.homeserver().as_str())
        .await.map_err(|e| anyhow::anyhow!("{}", crate::i18n::tr(e.message())))?;
    Ok(())
}

pub async fn login_methods(client: &Client) -> Result<LoginMethods> {
    require_hagency(client).await?;
    let metadata = tokio::time::timeout(
        std::time::Duration::from_secs(10), client.oauth().server_metadata(),
    ).await.map_err(|_| anyhow::anyhow!("Pasion sign-in is temporarily unavailable. Please retry."))??;
    validate_oauth_metadata(&metadata)?;
    if metadata.issuer != client.homeserver().join("auth/")? {
        bail!("This server advertised an unexpected Pasion issuer");
    }
    Ok(LoginMethods {
        homeserver: client.homeserver().to_string(), password: false, sso: false,
        oauth: true, oauth_aware_preferred: true,
        browser_registration: metadata.prompt_values_supported.contains(
            &matrix_sdk::ruma::api::client::discovery::get_authorization_server_metadata::v1::Prompt::Create,
        ),
        providers: Vec::new(),
    })
}

pub(crate) fn validate_oauth_origin(origin: &Url) -> Result<()> {
    let loopback = origin.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
            || host.eq_ignore_ascii_case("localhost")
    });
    if (origin.scheme() != "https" && !(origin.scheme() == "http" && loopback))
        || !origin.username().is_empty()
        || origin.password().is_some()
    {
        bail!("OAuth requires HTTPS, or a loopback development server");
    }
    Ok(())
}

fn validate_oauth_metadata(
    metadata: &matrix_sdk::ruma::api::client::discovery::get_authorization_server_metadata::v1::AuthorizationServerMetadata,
) -> Result<()> {
    validate_oauth_origin(&metadata.issuer)?;
    // SDK permits HTTP for developer discovery, but that must not permit an
    // unencrypted token/registration request to an unrelated remote host.
    let value = serde_json::to_value(metadata)?;
    for field in [
        "authorization_endpoint",
        "token_endpoint",
        "registration_endpoint",
        "revocation_endpoint",
    ] {
        if let Some(endpoint) = value[field].as_str() {
            validate_oauth_origin(&Url::parse(endpoint)?)?;
        }
    }
    Ok(())
}

/// Standard SDK OAuth: the SDK installs the Matrix device and alone refreshes it.
/// Instrument builds can hand a local test authorization URL to an isolated
/// browser. Login, PKCE, callback validation and SDK session persistence still
/// follow the production path. Packaged builds always use the system browser.
pub(crate) fn open_login_url(url: &str) -> Result<()> {
    #[cfg(all(feature = "palpo-instrument", target_os = "macos"))]
    if let Some(path) = std::env::var_os("HAGENCY_INSTRUMENT_LOGIN_URL") {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let parsed = Url::parse(url)?;
        anyhow::ensure!(
            parsed.scheme() == "http" && parsed.host_str() == Some("127.0.0.1")
                && parsed.path().starts_with("/auth/")
                && std::env::var("MAKEPAD_HIDE_WINDOWS").as_deref() == Ok("1"),
            "Instrument authorization requires a hidden local test instance"
        );
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true)
            .mode(0o600).open(path)?;
        file.write_all(url.as_bytes())?;
        file.sync_all()?;
        return Ok(());
    }
    robius_open::Uri::new(url).open().map_err(|_| anyhow::anyhow!("Unable to open your browser"))
}

pub(crate) async fn oauth_browser_login<F, Fut>(
    client: &Client,
    register: bool,
    open: F,
) -> Result<()>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    oauth_browser_login_expected(client, register, None, open).await
}

pub(crate) async fn oauth_browser_login_expected<F, Fut>(
    client: &Client,
    register: bool,
    expected: Option<(
        matrix_sdk::ruma::OwnedUserId,
        matrix_sdk::ruma::OwnedDeviceId,
    )>,
    open: F,
) -> Result<()>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    use matrix_sdk::{
        authentication::oauth::registration::{
            ApplicationType, ClientMetadata, Localized, OAuthGrantType,
        },
        ruma::{serde::Raw, api::client::discovery::get_authorization_server_metadata::v1::Prompt},
        utils::local_server::LocalServerBuilder,
    };
    require_hagency(client).await?;
    validate_oauth_origin(&client.homeserver())?;
    let oauth = client.oauth();
    let oauth_metadata = oauth.server_metadata().await?;
    validate_oauth_metadata(&oauth_metadata)?;
    if oauth_metadata.issuer != client.homeserver().join("auth/")? {
        bail!("This server advertised an unexpected Pasion issuer");
    }
    // Recheck at activation: registration may have been disabled since discovery.
    if register
        && !oauth_metadata
            .prompt_values_supported
            .contains(&Prompt::Create)
    {
        bail!(
            "This server does not advertise browser account creation. Try password registration if available."
        );
    }
    let (redirect, server) = LocalServerBuilder::new().spawn().await?;
    let metadata = ClientMetadata {
        client_name: Some(Localized::new("Hagency Desktop".to_owned(), [])),
        ..ClientMetadata::new(
            ApplicationType::Native,
            vec![OAuthGrantType::AuthorizationCode {
                redirect_uris: vec![Url::parse("http://127.0.0.1/")?],
            }],
            Localized::new(Url::parse("https://github.com/hagency-org/hagency-desktop")?, None),
        )
    };
    let data = oauth
        .login(
            redirect,
            expected
                .as_ref()
                .map(|(_, device)| device.clone())
                .or_else(|| client.device_id().map(ToOwned::to_owned)),
            Some(Raw::new(&metadata)?.into()),
            None,
        )
        .prompt(vec![if register {
            Prompt::Create
        } else {
            Prompt::from("login")
        }])
        .build()
        .await?;
    if let Err(e) = open(data.url.to_string()).await {
        oauth.abort_login(&data.state).await;
        return Err(e);
    }
    let query =
        match tokio::time::timeout(std::time::Duration::from_secs(300), server.into_future()).await
        {
            Ok(Some(q)) => q,
            _ => {
                oauth.abort_login(&data.state).await;
                bail!("Sign-in was cancelled or expired");
            }
        };
    let previous_token = client.access_token();
    let completed = async {
        oauth.finish_login(query.into()).await?;
        let identity =
            tokio::time::timeout(std::time::Duration::from_secs(8), client.whoami()).await??;
        if let Some((owner, device)) = expected.as_ref() {
            if &identity.user_id != owner
                || identity.device_id.as_deref() != Some(device.as_ref())
                || client.user_id() != Some(owner)
                || client.device_id() != Some(device.as_ref())
            {
                bail!("Pasion returned another Matrix account or device");
            }
        }
        if identity
            .device_id
            .as_deref()
            .is_some_and(|device| Some(device) != client.device_id())
        {
            bail!("The server returned a different Matrix device");
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    if let Err(error) = completed {
        // The SDK installs the exchanged token before whoami/SessionMismatch.
        // Revoke only that newly issued grant, never the previous stored one.
        if client.access_token().is_some() && client.access_token() != previous_token {
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(8), logout_sdk(client)).await;
        }
        return Err(error);
    }
    Ok(())
}

/// SDK OAuth revocation enforces HTTPS internally, including developer servers.
/// Keep its normal implementation and provide only a verified loopback fallback.
pub(crate) async fn logout_sdk(client: &Client) -> Result<()> {
    if matches!(
        client.auth_api(),
        Some(matrix_sdk::authentication::AuthApi::OAuth(_))
    ) && client.homeserver().scheme() == "http"
    {
        validate_oauth_origin(&client.homeserver())?;
        let metadata = client.oauth().server_metadata().await?;
        validate_oauth_metadata(&metadata)?;
        if metadata.revocation_endpoint.scheme() != "http" {
            client.logout().await?;
            return Ok(());
        }
        let refresh = client
            .oauth()
            .full_session()
            .and_then(|s| s.user.tokens.refresh_token);
        let (token, hint) = match refresh {
            Some(token) => (token, "refresh_token"),
            None => (
                client
                    .access_token()
                    .ok_or_else(|| anyhow::anyhow!("No OAuth session"))?,
                "access_token",
            ),
        };
        let oauth = client.oauth();
        let id = oauth
            .client_id()
            .ok_or_else(|| anyhow::anyhow!("No OAuth client identity"))?;
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("token", token.as_str()),
                ("client_id", id.as_str()),
                ("token_type_hint", hint),
            ])
            .finish();
        let response = crate::http::client_builder()
            .build()?
            .post(metadata.revocation_endpoint)
            .timeout(std::time::Duration::from_secs(8))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(encoded)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!("OAuth revocation was not accepted");
        }
        Ok(())
    } else {
        client.logout().await?;
        Ok(())
    }
}

/// Memory-only discovery: checking a server does not create a session/database.
pub async fn discover(user: &str, server: &str) -> Result<LoginMethods> {
    discover_with_timeout(user, server, std::time::Duration::from_secs(20)).await
}

async fn discover_with_timeout(
    user: &str,
    server: &str,
    timeout: std::time::Duration,
) -> Result<LoginMethods> {
    let destination = login_server(user, Some(server))?;
    // The SDK uses its own retry policy for well-known discovery and version
    // checks. Bound the entire operation, including DNS and those retries.
    tokio::time::timeout(timeout, async move {
        let builder = Client::builder()
            .server_name_or_homeserver_url(destination)
            .request_config(
                RequestConfig::new()
                    .timeout(std::time::Duration::from_secs(15))
                    .retry_limit(0),
            );
        let client = crate::sliding_sync::use_android_tls_roots(builder)
            .build()
            .await?;
        login_methods(&client).await
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!("The server check timed out. Check your connection and try again.")
    })?
}

/// Complete a legacy registration when the homeserver offers a simple UIAA
/// dummy or registration-token stage. Other stages need their own client UI.
#[cfg(test)]
async fn register_account(
    client: &Client,
    username: String,
    password: String,
    token: String,
) -> Result<()> {
    use matrix_sdk::ruma::api::client::{
        account::register::v3::Request,
        uiaa::{AuthData, AuthType, Dummy, RegistrationToken},
    };

    let mut request = Request::new();
    request.username = Some(username);
    request.password = Some(password);
    request.refresh_token = true;
    match client.matrix_auth().register(request.clone()).await {
        Ok(_) => {}
        Err(error) => {
            let Some(challenge) = error.as_uiaa_response() else {
                return Err(error.into());
            };
            let session_id = challenge.session.clone();
            let flow = challenge
                .flows
                .iter()
                .find(|flow| flow.stages.as_slice() == [AuthType::Dummy])
                .or_else(|| {
                    challenge
                        .flows
                        .iter()
                        .find(|flow| flow.stages.as_slice() == [AuthType::RegistrationToken])
                });
            match flow.map(|flow| flow.stages.first()) {
                Some(Some(AuthType::Dummy)) => {
                    let mut auth = Dummy::new();
                    auth.session = session_id;
                    request.auth = Some(AuthData::Dummy(auth));
                }
                Some(Some(AuthType::RegistrationToken)) => {
                    if token.is_empty() {
                        bail!("This server requires a registration token. Enter it and try again.");
                    }
                    let mut auth = RegistrationToken::new(token);
                    auth.session = session_id;
                    request.auth = Some(AuthData::RegistrationToken(auth));
                }
                _ => {
                    bail!("This server requires a registration step that Rinx cannot complete yet.")
                }
            }
            client.matrix_auth().register(request).await?;
        }
    }
    if !client.matrix_auth().logged_in() {
        bail!("Account created, but the server did not issue a login session. Please sign in.");
    }
    Ok(())
}

#[cfg(not(target_os = "ios"))]
pub async fn browser_login<F, Fut>(
    client: &Client,
    provider_id: Option<&str>,
    register: bool,
    open: F,
) -> matrix_sdk::Result<()>
where
    F: FnOnce(String) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = matrix_sdk::Result<()>> + Send + 'static,
{
    let mut login = client.matrix_auth().login_sso(move |sso_url| async move {
        let sso_url = if register {
            let mut url = url::Url::parse(&sso_url)
                .map_err(|error| matrix_sdk::Error::Io(std::io::Error::other(error)))?;
            url.query_pairs_mut().append_pair("action", "register");
            url.into()
        } else {
            sso_url
        };
        open(sso_url).await
    });
    if let Some(id) = provider_id {
        login = login.identity_provider_id(id);
    }
    login
        .initial_device_display_name("Rinx")
        .request_refresh_token()
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_id_discovers_its_server_and_explicit_url_wins() {
        assert_eq!(
            login_server(" @alex:example.org:8448 ", None).unwrap(),
            "example.org:8448"
        );
        assert_eq!(
            login_server("@alex:example.org", Some(" http://127.0.0.1:18120/ ")).unwrap(),
            "http://127.0.0.1:18120/"
        );
        assert!(login_server("alex", None).is_err());
        assert!(login_server("alex@example.org", None).is_err());
    }
    #[test]
    fn malformed_destinations_are_rejected_before_credentials() {
        for server in [
            "javascript://host",
            "https://user:secret@example.org",
            "https://example.org?token=secret",
            "https://example.org/#login",
            "foo bar",
            "https://example.org\\evil",
        ] {
            assert!(login_server("alex", Some(server)).is_err(), "{server}");
        }
        assert!(login_server("@alex:", None).is_err());
    }
    #[test]
    fn identifiers_use_matrix_protocol_types() {
        assert_eq!(
            serde_json::to_value(password_identifier("@alex").unwrap()).unwrap(),
            serde_json::json!({"type":"m.id.user","user":"alex"})
        );
        assert_eq!(
            serde_json::to_value(password_identifier(" alex@example.org ").unwrap()).unwrap(),
            serde_json::json!({"type":"m.id.thirdparty","medium":"email","address":"alex@example.org"})
        );
        assert!(password_identifier(" ").is_err());
        assert!(password_identifier("@alex:example.org").is_ok());
    }

    /// A local Matrix protocol fixture; no production credentials or browser.
    struct Server {
        url: String,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        requests: std::sync::Arc<std::sync::Mutex<Vec<(String, serde_json::Value)>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Server {
        fn new() -> Self {
            Self::with_registration(None)
        }
        fn with_registration(registration_token: Option<&'static str>) -> Self {
            use std::{
                io::{Read, Write},
                sync::{
                    Arc, Mutex,
                    atomic::{AtomicBool, Ordering},
                },
            };
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let requests = Arc::new(Mutex::new(Vec::new()));
            let received = requests.clone();
            let thread = std::thread::spawn(move || {
                while !stopped.load(Ordering::Relaxed) {
                    let Ok((mut socket, _)) = listener.accept() else {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    };
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                        .unwrap();
                    let mut bytes = Vec::new();
                    let mut buf = [0; 4096];
                    let (head, body) = loop {
                        let count = socket.read(&mut buf).unwrap();
                        if count == 0 {
                            panic!("Incomplete fixture request");
                        }
                        bytes.extend_from_slice(&buf[..count]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8(bytes[..end].to_vec()).unwrap();
                            let len = head
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|n| n.trim().parse::<usize>().unwrap())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + len {
                                break (head, bytes[end + 4..end + 4 + len].to_vec());
                            }
                        }
                    };
                    let route = head.lines().next().unwrap().to_owned();
                    let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
                    received.lock().unwrap().push((route.clone(), json.clone()));
                    let mut status = "200 OK";
                    let response = if route.contains("/createRoom ") {
                        serde_json::json!({"room_id":"!plain:localhost"})
                    } else if route.contains("/state/m.room.encryption/") {
                        status = "404 Not Found";
                        serde_json::json!({"errcode":"M_NOT_FOUND","error":"No encryption state"})
                    } else if route.contains("/versions ") {
                        serde_json::json!({"versions":["v1.11"],"unstable_features":{}})
                    } else if route.contains("/auth_metadata ") {
                        serde_json::json!({"prompt_values_supported":["login","create"]})
                    } else if route.starts_with("GET ") && route.contains("/login ") {
                        serde_json::json!({"flows":[{"type":"m.login.password"},{"type":"m.login.sso","oauth_aware_preferred":true,"identity_providers":[{"id":"company-custom-id","name":"Company SSO"}]},{"type":"m.login.token"}]})
                    } else if route.starts_with("POST ")
                        && route.contains("/register ")
                        && registration_token.is_some()
                    {
                        let stage = if registration_token == Some("") {
                            "m.login.dummy"
                        } else {
                            "m.login.registration_token"
                        };
                        let completed = json["auth"]["type"] == stage
                            && (stage == "m.login.dummy"
                                || json["auth"]["token"] == registration_token.unwrap());
                        if completed {
                            serde_json::json!({"user_id":"@fixture:localhost","device_id":"TESTDEVICE","access_token":"fixture-access","refresh_token":"fixture-refresh"})
                        } else {
                            status = "401 Unauthorized";
                            serde_json::json!({"flows":[{"stages":[stage]}],"session":"fixture-registration-session"})
                        }
                    } else if route.starts_with("POST ") && route.contains("/login ") {
                        serde_json::json!({"user_id":"@fixture:localhost","device_id":"TESTDEVICE","access_token":"fixture-access","refresh_token":"fixture-refresh"})
                    } else {
                        serde_json::json!({})
                    };
                    let body = response.to_string();
                    let _ = write!(
                        socket,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                }
            });
            Self {
                url,
                stop,
                requests,
                thread: Some(thread),
            }
        }
        async fn client(&self) -> Client {
            Client::builder()
                .homeserver_url(&self.url)
                .request_config(RequestConfig::new().retry_limit(0))
                .build()
                .await
                .unwrap()
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
            let result = self.thread.take().unwrap().join();
            if !std::thread::panicking() {
                result.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn plaintext_invitation_does_not_require_room_key_backup() {
        let server = Server::new();
        let client = Client::builder()
            .homeserver_url(&server.url)
            .with_enable_share_history_on_invite(true)
            .request_config(RequestConfig::new().retry_limit(0))
            .build().await.unwrap();
        client.matrix_auth().login_username("fixture", "fixture").send().await.unwrap();
        let room = client.create_room(
            matrix_sdk::ruma::api::client::room::create_room::v3::Request::new(),
        ).await.unwrap();
        crate::utils::invite_room_member(&room, matrix_sdk::ruma::user_id!("@guest:localhost"))
            .await.unwrap();
        let requests = server.requests.lock().unwrap();
        assert!(requests.iter().any(|(route, body)|
            route.starts_with("POST ") && route.contains("/invite ")
                && body["user_id"] == "@guest:localhost"));
        assert!(!requests.iter().any(|(route, _)| route.contains("/room_keys/")));
    }

    #[tokio::test]
    async fn ordinary_matrix_server_is_rejected_before_login_or_credentials() {
        let server = Server::new();
        let error = login_methods(&server.client().await).await.unwrap_err();
        assert!(error.to_string().contains("hagency") || error.to_string().contains("Hagency"));
        assert!(!server.requests.lock().unwrap().iter().any(|(route, _)| route.contains("/login ")));
    }

    #[tokio::test]
    async fn stalled_discovery_times_out_and_a_new_check_can_succeed() {
        use std::time::Duration;

        // Accept TCP but never send an HTTP response, as with a broken tunnel.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (accepted, connection) = tokio::sync::oneshot::channel();
        let stalled_server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let _ = accepted.send(socket);
        });
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            discover_with_timeout("", &url, Duration::from_millis(200)),
        )
        .await
        .expect("discovery must have a total deadline")
        .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
        let _socket = connection
            .await
            .expect("discovery contacted the stalled server");
        stalled_server.await.unwrap();

        let server = Server::new();
        assert!(discover("", &server.url).await.is_err());
        assert!(
            !server
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(route, _)| route.starts_with("POST "))
        );
    }

    #[tokio::test]
    async fn legacy_registration_completes_dummy_challenge() {
        let server = Server::with_registration(Some(""));
        let client = server.client().await;
        register_account(
            &client,
            "new_user".into(),
            "a-strong-password".into(),
            "".into(),
        )
        .await
        .unwrap();
        assert!(client.matrix_auth().logged_in());
        let requests = server.requests.lock().unwrap();
        let registrations: Vec<_> = requests
            .iter()
            .filter(|(route, _)| route.contains("/register "))
            .collect();
        assert_eq!(registrations.len(), 2);
        assert_eq!(registrations[1].1["auth"]["type"], "m.login.dummy");
        assert_eq!(
            registrations[1].1["auth"]["session"],
            "fixture-registration-session"
        );
    }

    #[tokio::test]
    async fn token_registration_requires_a_token_before_retrying() {
        let server = Server::with_registration(Some("invite"));
        let client = server.client().await;
        let error = register_account(
            &client,
            "new_user".into(),
            "a-strong-password".into(),
            "".into(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("registration token"));
        register_account(
            &client,
            "new_user".into(),
            "a-strong-password".into(),
            "invite".into(),
        )
        .await
        .unwrap();
        let requests = server.requests.lock().unwrap();
        let registrations: Vec<_> = requests
            .iter()
            .filter(|(route, _)| route.contains("/register "))
            .collect();
        assert_eq!(registrations.len(), 3);
        assert_eq!(registrations[2].1["auth"]["token"], "invite");
    }

    #[cfg(not(target_os = "ios"))]
    #[tokio::test]
    async fn selected_provider_uses_advertised_id_and_exchanges_token() {
        let server = Server::new();
        let client = server.client().await;
        browser_login(
            &client,
            Some("company-custom-id"),
            false,
            |link| async move {
                let url = Url::parse(&link).unwrap();
                assert!(
                    url.path()
                        .ends_with("/login/sso/redirect/company-custom-id")
                );
                let redirect = url
                    .query_pairs()
                    .find(|(k, _)| k == "redirectUrl")
                    .unwrap()
                    .1
                    .into_owned();
                let mut callback = Url::parse(&redirect).unwrap();
                assert!(matches!(
                    callback.host_str(),
                    Some("127.0.0.1" | "[::1]" | "localhost")
                ));
                callback
                    .query_pairs_mut()
                    .append_pair("loginToken", "fixture-login-token");
                matrix_sdk::reqwest::get(callback)
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(client.user_id().unwrap().as_str(), "@fixture:localhost");
        let requests = server.requests.lock().unwrap();
        let logins: Vec<_> = requests
            .iter()
            .filter(|(route, _)| route.starts_with("POST ") && route.contains("/login "))
            .collect();
        assert_eq!(logins.len(), 1);
        assert_eq!(logins[0].1["type"], "m.login.token");
        assert_eq!(logins[0].1["token"], "fixture-login-token");
        assert_eq!(logins[0].1["refresh_token"], true);
    }

    #[cfg(not(target_os = "ios"))]
    #[tokio::test]
    async fn browser_registration_sets_matrix_action_parameter() {
        let server = Server::new();
        let client = server.client().await;
        browser_login(&client, None, true, |link| async move {
            let url = Url::parse(&link).unwrap();
            assert!(
                url.query_pairs()
                    .any(|(key, value)| key == "action" && value == "register")
            );
            let redirect = url
                .query_pairs()
                .find(|(key, _)| key == "redirectUrl")
                .unwrap()
                .1
                .into_owned();
            let mut callback = Url::parse(&redirect).unwrap();
            callback
                .query_pairs_mut()
                .append_pair("loginToken", "fixture-login-token");
            matrix_sdk::reqwest::get(callback)
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            Ok(())
        })
        .await
        .unwrap();
        assert!(client.matrix_auth().logged_in());
    }

    #[cfg(not(target_os = "ios"))]
    #[tokio::test]
    async fn cancellation_closes_callback_and_does_not_exchange_token() {
        let server = Server::new();
        let client = server.client().await;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            browser_login(&client, None, false, |link| async move {
                let url = Url::parse(&link).unwrap();
                let redirect = url
                    .query_pairs()
                    .find(|(k, _)| k == "redirectUrl")
                    .unwrap()
                    .1
                    .into_owned();
                sender.send(redirect).unwrap();
                Ok(())
            })
            .await
        });
        let redirect = receiver.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matrix_sdk::reqwest::Client::builder()
            .no_proxy()
            .build().unwrap()
            .get(redirect).send().await.is_err());
        assert!(
            !server
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|(route, _)| route.starts_with("POST ") && route.contains("/login "))
        );
    }
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod oauth_tests;
