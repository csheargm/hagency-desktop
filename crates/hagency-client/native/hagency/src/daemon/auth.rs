//! Daemon-owned Matrix SDK OAuth custody.
//!
//! The daemon has its own SDK device and OAuth client registration, separate
//! from Hagency Desktop's. Sharing Desktop's refresh token would make two
//! processes race over refresh-token rotation, so enrollment always performs
//! a fresh browser authorization. Afterwards the SDK alone refreshes and
//! persists the session, so the daemon starts and keeps working while every
//! UI is closed. No token, refresh credential or provider text is logged,
//! returned over IPC or written anywhere except the private session file.
//!
//! This mirrors Desktop's proven `SdkTokens` checks: issuer pinning, a fresh
//! `whoami` for exact owner and device, and a one-time explicit-enrollment
//! permission consumed by the native device registration.
use crate::native_owner::{MatrixAccessToken, MatrixTokenSource, NativeError};
use matrix_sdk::{
    Client, HttpError, RefreshTokenError, SessionChange,
    authentication::{
        AuthSession,
        oauth::{
            ClientId, OAuthSession, UserSession,
            error::{BasicErrorResponseType, OAuthError, RequestTokenError},
        },
    },
};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};

const SESSION: &str = "session.json";
const STORE: &str = "sdk-store";
const STORE_KEY: &str = "store-key";
const LIMIT: usize = 64 * 1024;
const TIMEOUT: Duration = Duration::from_secs(8);
static SESSION_IO: Mutex<()> = Mutex::new(());

/// Observed authorization state. Only `Authorized` permits hosting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthState {
    Authorized,
    /// Temporary: network or server unavailable. Hosting retries.
    Unavailable,
    /// Terminal for this session: the owner must enroll again.
    SignInRequired,
    Retired,
}

impl AuthState {
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Authorized,
            1 => Self::Unavailable,
            2 => Self::SignInRequired,
            _ => Self::Retired,
        }
    }
    fn as_u8(self) -> u8 {
        match self {
            Self::Authorized => 0,
            Self::Unavailable => 1,
            Self::SignInRequired => 2,
            Self::Retired => 3,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Saved {
    version: u8,
    homeserver: String,
    owner: String,
    oauth_client_id: String,
    user: UserSession,
}

#[derive(Debug, thiserror::Error, Clone, Copy, PartialEq, Eq)]
pub enum AuthError {
    #[error("no daemon enrollment exists")]
    NotEnrolled,
    #[error("the daemon session belongs to another account")]
    AccountMismatch,
    #[error("the daemon must be enrolled again")]
    SignInRequired,
    #[error("authorization is temporarily unavailable")]
    Unavailable,
    #[error("the daemon session store is unavailable")]
    Storage,
    #[error("the Hagency server is unsupported")]
    UnsupportedServer,
    #[error("sign-in was cancelled or expired")]
    Cancelled,
}

impl AuthError {
    pub fn code(self) -> &'static str {
        match self {
            Self::NotEnrolled => "daemon_enrollment_required",
            Self::AccountMismatch => "matrix_account_mismatch",
            Self::SignInRequired => "matrix_authorization_required",
            Self::Unavailable => "matrix_authorization_unavailable",
            Self::Storage => "daemon_session_storage_unavailable",
            Self::UnsupportedServer => "unsupported_hagency_server",
            Self::Cancelled => "daemon_sign_in_cancelled",
        }
    }
}

/// One SDK session. Callers hold it in an `Arc`; the native engine receives
/// only a weak [`DaemonTokens`] source.
pub struct SdkCustody {
    client: Client,
    origin: String,
    owner: String,
    state: AtomicU8,
    logout_home: Mutex<Option<PathBuf>>,
    /// Set only by a fresh browser enrollment in this process, consumed once.
    explicit: AtomicBool,
    monitor: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for SdkCustody {
    fn drop(&mut self) {
        if let Ok(mut task) = self.monitor.lock()
            && let Some(task) = task.take()
        {
            task.abort();
        }
    }
}

fn store_key(dir: &Path) -> Result<String, AuthError> {
    let path = dir.join(STORE_KEY);
    match hagency_store::private::read_secret(&path) {
        Ok(bytes) => String::from_utf8(bytes)
            .ok()
            .filter(|k| k.len() == 64)
            .ok_or(AuthError::Storage),
        Err(hagency_store::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut key = [0u8; 32];
            getrandom::fill(&mut key).map_err(|_| AuthError::Storage)?;
            let key: String = key.iter().map(|b| format!("{b:02x}")).collect();
            hagency_store::private::write_new(&path, key.as_bytes())
                .map_err(|_| AuthError::Storage)?;
            Ok(key)
        }
        Err(_) => Err(AuthError::Storage),
    }
}

async fn build_client(dir: &Path, origin: &str) -> Result<Client, AuthError> {
    hagency_store::private::directory(dir).map_err(|_| AuthError::Storage)?;
    let store = dir.join(STORE);
    hagency_store::private::directory(&store).map_err(|_| AuthError::Storage)?;
    let key = store_key(dir)?;
    Client::builder()
        .homeserver_url(origin)
        .sqlite_store(&store, Some(&key))
        .handle_refresh_tokens()
        .request_config(matrix_sdk::config::RequestConfig::new().timeout(Duration::from_secs(60)))
        .build()
        .await
        .map_err(|_| AuthError::Storage)
}

/// OAuth staging has no persistent crypto store and cannot refresh a token.
async fn provisioning_client(origin: &str) -> Result<Client, AuthError> {
    Client::builder()
        .homeserver_url(origin)
        .with_encryption_settings(matrix_sdk::encryption::EncryptionSettings {
            auto_enable_cross_signing: false,
            auto_enable_backups: false,
            ..Default::default()
        })
        .request_config(matrix_sdk::config::RequestConfig::new().timeout(TIMEOUT))
        .build()
        .await
        .map_err(|_| AuthError::Unavailable)
}

fn requested_device(url: &reqwest::Url) -> Result<String, AuthError> {
    let devices: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key == "scope")
        .flat_map(|(_, value)| {
            value
                .split_whitespace()
                .filter_map(|scope| {
                    scope
                        .strip_prefix("urn:matrix:client:device:")
                        .or_else(|| scope.strip_prefix("urn:matrix:org.matrix.msc2967.client:device:"))
                        .map(str::to_owned)
                })
                .collect::<Vec<_>>()
        })
        .collect();
    if devices.len() != 1 || devices[0].is_empty() {
        return Err(AuthError::AccountMismatch);
    }
    Ok(devices[0].clone())
}

#[derive(Deserialize)]
struct FreshIdentity {
    user_id: matrix_sdk::ruma::OwnedUserId,
    device_id: matrix_sdk::ruma::OwnedDeviceId,
}

async fn complete_new_grant(
    client: &Client,
    query: matrix_sdk::utils::UrlOrQuery,
    origin: &str,
    expected_owner: Option<&str>,
    expected_device: &str,
    budget: Duration,
    first_delay: Duration,
) -> Result<Saved, AuthError> {
    // Never call this method again for the same code. A failed whoami can be
    // resumed using the SDK's already-issued tokens, not another exchange.
    let deadline = tokio::time::Instant::now() + budget;
    if let Err(error) = tokio::time::timeout_at(deadline, client.oauth().finish_login(query))
        .await
        .map_err(|_| AuthError::Unavailable)?
    {
        if matches!(&error,matrix_sdk::Error::OAuth(error) if matches!(error.as_ref(),OAuthError::SessionMismatch))
        {
            return Err(AuthError::AccountMismatch);
        }
        if !error
            .as_client_api_error()
            .is_some_and(|error| error.status_code.as_u16() == 401)
        {
            return Err(AuthError::SignInRequired);
        }
    }
    let tokens = client.session_tokens().ok_or(AuthError::SignInRequired)?;
    let client_id = client
        .oauth()
        .client_id()
        .ok_or(AuthError::SignInRequired)?
        .as_str()
        .to_owned();
    let identity = fresh_identity(
        origin,
        &tokens.access_token,
        deadline.saturating_duration_since(tokio::time::Instant::now()),
        first_delay,
    )
    .await?;
    if identity.device_id.as_str() != expected_device
        || expected_owner.is_some_and(|owner| owner != identity.user_id.as_str())
    {
        return Err(AuthError::AccountMismatch);
    }
    Ok(Saved {
        version: 1,
        homeserver: origin.into(),
        owner: identity.user_id.to_string(),
        oauth_client_id: client_id,
        user: UserSession {
            meta: matrix_sdk::SessionMeta {
                user_id: identity.user_id,
                device_id: identity.device_id,
            },
            tokens,
        },
    })
}

async fn fresh_identity(
    origin: &str,
    token: &str,
    budget: Duration,
    first_delay: Duration,
) -> Result<FreshIdentity, AuthError> {
    let url = reqwest::Url::parse(origin)
        .and_then(|url| url.join("/_matrix/client/v3/account/whoami"))
        .map_err(|_| AuthError::UnsupportedServer)?;
    // No redirects, retries, token refresh or credential replacement in this
    // transport. Only this read-only identity probe can repeat a fresh grant.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| AuthError::Unavailable)?;
    tokio::time::timeout(budget, async {
        let mut delay = first_delay;
        for attempt in 0..9 {
            let response = http
                .get(url.clone())
                .bearer_auth(token)
                .send()
                .await
                .map_err(|_| AuthError::Unavailable)?;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED {
                if attempt == 8 {
                    return Err(AuthError::SignInRequired);
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(1));
                continue;
            }
            if !response.status().is_success() {
                return Err(AuthError::Unavailable);
            }
            return response
                .json()
                .await
                .map_err(|_| AuthError::AccountMismatch);
        }
        Err(AuthError::SignInRequired)
    })
    .await
    .map_err(|_| AuthError::Unavailable)?
}

fn read_saved(dir: &Path) -> Result<Option<Saved>, AuthError> {
    let Some(bytes) =
        super::read_private(&dir.join(SESSION), LIMIT).map_err(|_| AuthError::Storage)?
    else {
        return Ok(None);
    };
    let saved: Saved = serde_json::from_slice(&bytes).map_err(|_| AuthError::Storage)?;
    if saved.version != 1 || saved.oauth_client_id.is_empty() {
        return Err(AuthError::Storage);
    }
    Ok(Some(saved))
}

fn write_saved(dir: &Path, saved: &Saved) -> Result<(), AuthError> {
    let raw = serde_json::to_vec(saved).map_err(|_| AuthError::Storage)?;
    hagency_store::private::replace(&dir.join(SESSION), &raw).map_err(|_| AuthError::Storage)
}

fn saved_from_client(client: &Client, origin: &str, owner: &str) -> Result<Saved, AuthError> {
    match client.session() {
        Some(AuthSession::OAuth(session)) => Ok(Saved {
            version: 1,
            homeserver: origin.into(),
            owner: owner.into(),
            oauth_client_id: session.client_id.as_str().into(),
            user: session.user,
        }),
        _ => Err(AuthError::SignInRequired),
    }
}

fn verify_same_session(previous: &Saved, next: &Saved) -> Result<(), AuthError> {
    if !super::same_origin(&previous.homeserver, &next.homeserver)
        || previous.owner != next.owner
        || previous.oauth_client_id != next.oauth_client_id
        || previous.user.meta != next.user.meta
    {
        Err(AuthError::AccountMismatch)
    } else {
        Ok(())
    }
}

/// The SDK persists rotated tokens through these callbacks before it
/// broadcasts `TokensRefreshed`. Identity is rechecked on every write.
fn install_callbacks(
    client: &Client,
    dir: PathBuf,
    origin: String,
    owner: String,
) -> Result<(), AuthError> {
    let read_dir = dir.clone();
    let read_owner = owner.clone();
    client
        .set_session_callbacks(
            Box::new(move |client| {
                let _io = SESSION_IO
                    .lock()
                    .map_err(|_| std::io::Error::other("session io"))?;
                let saved = read_saved(&read_dir)
                    .ok()
                    .flatten()
                    .ok_or_else(|| std::io::Error::other("session missing"))?;
                if saved.owner != read_owner
                    || client.session_meta().is_none_or(|m| {
                        m.user_id != saved.user.meta.user_id
                            || m.device_id != saved.user.meta.device_id
                    })
                {
                    return Err(std::io::Error::other("session identity changed").into());
                }
                Ok(saved.user.tokens)
            }),
            Box::new(move |client| {
                let _io = SESSION_IO
                    .lock()
                    .map_err(|_| std::io::Error::other("session io"))?;
                let previous = read_saved(&dir)
                    .ok()
                    .flatten()
                    .ok_or_else(|| std::io::Error::other("session missing"))?;
                let next = saved_from_client(&client, &origin, &owner)
                    .map_err(|_| std::io::Error::other("no oauth session"))?;
                if previous.user.meta != next.user.meta
                    || previous.oauth_client_id != next.oauth_client_id
                {
                    return Err(std::io::Error::other("session identity changed").into());
                }
                write_saved(&dir, &next).map_err(|_| std::io::Error::other("session write"))?;
                Ok(())
            }),
        )
        .map_err(|_| AuthError::Storage)
}

pub(crate) fn refresh_requires_login(error: &RefreshTokenError) -> bool {
    match error {
        RefreshTokenError::RefreshTokenRequired => true,
        RefreshTokenError::OAuth(error) => match error.as_ref() {
            OAuthError::NotRegistered | OAuthError::NotAuthenticated => true,
            OAuthError::RefreshToken(RequestTokenError::ServerResponse(response)) => matches!(
                response.error(),
                BasicErrorResponseType::InvalidGrant
                    | BasicErrorResponseType::InvalidClient
                    | BasicErrorResponseType::UnauthorizedClient
            ),
            _ => false,
        },
        RefreshTokenError::MatrixAuth(error) => http_requires_login(error),
    }
}

pub(crate) fn http_requires_login(error: &HttpError) -> bool {
    use matrix_sdk::ruma::api::error::ErrorKind;
    matches!(
        error.client_api_error_kind(),
        Some(ErrorKind::UnknownToken(..) | ErrorKind::MissingToken)
    ) || matches!(error, HttpError::RefreshToken(error) if refresh_requires_login(error))
}

fn source_error(code: &str) -> NativeError {
    NativeError {
        status: if code == "matrix_authorization_unavailable" {
            503
        } else {
            401
        },
        code: code.into(),
    }
}

/// HTTPS, or a loopback development server. Same rule as Desktop's login.
pub(crate) fn valid_origin(origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
            || host.eq_ignore_ascii_case("localhost")
    });
    (url.scheme() == "https" || (url.scheme() == "http" && loopback))
        && url.username().is_empty()
        && url.password().is_none()
}

impl SdkCustody {
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn owner(&self) -> &str {
        &self.owner
    }
    pub fn state(&self) -> AuthState {
        AuthState::from_u8(self.state.load(Ordering::Acquire))
    }
    fn set_state(&self, state: AuthState) {
        // Retired and sign-in-required are terminal for this custody.
        let _ = self
            .state
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                matches!(
                    AuthState::from_u8(current),
                    AuthState::Authorized | AuthState::Unavailable
                )
                .then_some(state.as_u8())
            });
    }
    /// Gate used by every native worker. Temporary unavailability keeps
    /// custody; the native transports themselves fail closed per request.
    pub fn live(&self) -> bool {
        matches!(self.state(), AuthState::Authorized | AuthState::Unavailable)
            && self.logout_home.lock().is_ok_and(|home| home.as_ref().is_none_or(|home| super::revocation::load(home).is_ok_and(|r| r.is_none())))
    }
    pub(super) fn fence_on_logout(&self, home: &Path) {
        *self.logout_home.lock().unwrap_or_else(|p| p.into_inner()) = Some(home.into());
    }
    pub fn retire(&self) {
        self.set_state(AuthState::Retired);
    }
    pub fn tokens(self: &Arc<Self>) -> Arc<dyn MatrixTokenSource> {
        Arc::new(DaemonTokens(Arc::downgrade(self)))
    }
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// Restore the enrolled session without any UI. Fails closed when the
    /// saved identity differs from the expected owner.
    pub async fn restore(dir: &Path, origin: &str, owner: &str) -> Result<Arc<Self>, AuthError> {
        let saved = read_saved(dir)?.ok_or(AuthError::NotEnrolled)?;
        if !super::same_origin(&saved.homeserver, origin)
            || saved.owner != owner
            || saved.user.meta.user_id.as_str() != owner
        {
            return Err(AuthError::AccountMismatch);
        }
        let client = build_client(dir, origin).await?;
        install_callbacks(&client, dir.into(), origin.into(), owner.into())?;
        client
            .restore_session(AuthSession::OAuth(Box::new(OAuthSession {
                client_id: ClientId::new(saved.oauth_client_id.clone()),
                user: saved.user,
            })))
            .await
            .map_err(|_| AuthError::Storage)?;
        Ok(Self::attach(client, origin, owner, false))
    }

    fn attach(client: Client, origin: &str, owner: &str, explicit: bool) -> Arc<Self> {
        let custody = Arc::new(Self {
            client: client.clone(),
            origin: origin.into(),
            owner: owner.into(),
            state: AtomicU8::new(AuthState::Authorized.as_u8()),
            logout_home: Mutex::new(None),
            explicit: AtomicBool::new(explicit),
            monitor: Mutex::new(None),
        });
        let weak = Arc::downgrade(&custody);
        let mut changes = client.subscribe_to_session_changes();
        let task = tokio::spawn(async move {
            while let Ok(change) = changes.recv().await {
                let Some(custody) = weak.upgrade() else { break };
                match change {
                    SessionChange::UnknownToken(_) => {
                        tracing::warn!(
                            code = "matrix_authorization_required",
                            "Daemon SDK session requires enrollment"
                        );
                        custody.set_state(AuthState::SignInRequired);
                        break;
                    }
                    SessionChange::TokensRefreshed => {
                        tracing::info!("Daemon SDK credentials refreshed and persisted");
                    }
                }
            }
        });
        *custody.monitor.lock().unwrap_or_else(|p| p.into_inner()) = Some(task);
        custody
    }

    /// Explicit enrollment through the system browser. The daemon gets its
    /// own SDK device and OAuth client; Desktop's credentials are never read.
    /// `open` receives the authorization URL to present to the owner.
    pub async fn enroll<F, Fut>(
        dir: &Path,
        origin: &str,
        expected_owner: Option<&str>,
        existing_public_client: Option<&str>,
        open: F,
    ) -> Result<Arc<Self>, AuthError>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), ()>>,
    {
        if !valid_origin(origin) {
            return Err(AuthError::UnsupportedServer);
        }
        if let Some(saved) = read_saved(dir)? {
            // A cancelled controller can leave a fully persisted SDK session
            // before native registration completes. Retry the same device;
            // never replace its grant or erase its crypto store.
            if !super::same_origin(&saved.homeserver, origin)
                || expected_owner.is_some_and(|owner| owner != saved.owner)
                || saved.user.meta.user_id.as_str() != saved.owner
                || existing_public_client.is_some_and(|id| id != saved.oauth_client_id)
            {
                return Err(AuthError::AccountMismatch);
            }
            let custody = Self::restore(dir, origin, &saved.owner).await?;
            // This method is reached only through explicit setup. Background
            // restore continues to have no one-time registration permission.
            custody.explicit.store(true, Ordering::Release);
            return Ok(custody);
        }
        Self::browser_authorize(
            dir,
            origin,
            expected_owner,
            existing_public_client,
            None,
            open,
        )
        .await
    }

    /// An explicit retry first verifies the saved device. Only a terminal token
    /// failure opens OAuth again, using the same public client and crypto store.
    /// The caller must have drained all previous SDK/native custody first.
    pub async fn reauthenticate<F, Fut>(
        dir: &Path,
        origin: &str,
        expected_owner: Option<&str>,
        existing_public_client: Option<&str>,
        open: F,
    ) -> Result<Arc<Self>, AuthError>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), ()>>,
    {
        if !valid_origin(origin) {
            return Err(AuthError::UnsupportedServer);
        }
        let Some(saved) = read_saved(dir)? else {
            return Self::enroll(dir, origin, expected_owner, existing_public_client, open).await;
        };
        if !super::same_origin(&saved.homeserver, origin)
            || expected_owner.is_some_and(|owner| owner != saved.owner)
            || saved.user.meta.user_id.as_str() != saved.owner
            || existing_public_client.is_some_and(|id| id != saved.oauth_client_id)
        {
            return Err(AuthError::AccountMismatch);
        }
        // Defer callbacks until the exact returned identity is verified. SDK
        // finish_login rejects another SessionMeta before touching crypto, but
        // a refresh callback must not persist its provisional tokens either.
        let client = build_client(dir, origin).await?;
        client
            .restore_session(AuthSession::OAuth(Box::new(OAuthSession {
                client_id: ClientId::new(saved.oauth_client_id.clone()),
                user: saved.user.clone(),
            })))
            .await
            .map_err(|_| AuthError::Storage)?;
        let custody = Self::attach(client.clone(), origin, &saved.owner, false);
        match custody.token().await {
            Ok(_) => {
                let current = saved_from_client(&client, origin, &saved.owner)?;
                verify_same_session(&saved, &current)?;
                {
                    let _io = SESSION_IO.lock().map_err(|_| AuthError::Storage)?;
                    write_saved(dir, &current)?;
                }
                install_callbacks(&client, dir.into(), origin.into(), saved.owner.clone())?;
                custody.explicit.store(true, Ordering::Release);
                Ok(custody)
            }
            Err(error) if error.code == "matrix_authorization_required" => {
                drop(custody);
                Self::browser_authorize(
                    dir,
                    origin,
                    Some(&saved.owner.clone()),
                    existing_public_client,
                    Some((client, saved)),
                    open,
                )
                .await
            }
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "matrix_account_mismatch" | "unsupported_hagency_issuer"
                ) =>
            {
                Err(AuthError::AccountMismatch)
            }
            Err(_) => Err(AuthError::Unavailable),
        }
    }

    async fn browser_authorize<F, Fut>(
        dir: &Path,
        origin: &str,
        expected_owner: Option<&str>,
        existing_public_client: Option<&str>,
        previous: Option<(Client, Saved)>,
        open: F,
    ) -> Result<Arc<Self>, AuthError>
    where
        F: FnOnce(String) -> Fut,
        Fut: std::future::Future<Output = Result<(), ()>>,
    {
        use matrix_sdk::{
            authentication::oauth::registration::{
                ApplicationType, ClientMetadata, Localized, OAuthGrantType,
            },
            ruma::serde::Raw,
            utils::local_server::LocalServerBuilder,
        };
        hagency_store::private::directory(dir).map_err(|_| AuthError::Storage)?;
        crate::server_admission::discover(origin)
            .await
            .map_err(|_| AuthError::UnsupportedServer)?;
        // The SDK exchanges the one-use code before its first whoami. Keep
        // provisional credentials out of the durable crypto store, and disable
        // automatic refresh so a cold identity never rotates the issued token.
        let previous = previous.map(|(client, saved)| {
            drop(client);
            saved
        });
        let client = provisioning_client(origin).await?;
        let oauth = client.oauth();
        if let Some(client_id) = previous
            .as_ref()
            .map(|saved| saved.oauth_client_id.as_str())
            .or(existing_public_client)
        {
            if client_id.is_empty()
                || client_id.len() > 255
                || client_id.chars().any(char::is_control)
            {
                return Err(AuthError::AccountMismatch);
            }
            // Reuse only the public registration here. The login below also
            // pins the saved device when this is explicit reauthorization.
            oauth.restore_registered_client(ClientId::new(client_id.to_owned()));
        }
        let metadata = tokio::time::timeout(TIMEOUT, oauth.server_metadata())
            .await
            .map_err(|_| AuthError::Unavailable)?
            .map_err(|_| AuthError::Unavailable)?;
        let issuer = reqwest::Url::parse(origin)
            .and_then(|u| u.join("auth/"))
            .map_err(|_| AuthError::UnsupportedServer)?;
        if metadata.issuer != issuer {
            return Err(AuthError::UnsupportedServer);
        }
        let (redirect, server) = LocalServerBuilder::new()
            .spawn()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let registration = ClientMetadata {
            client_name: Some(Localized::new("Hagency Owner Daemon".to_owned(), [])),
            ..ClientMetadata::new(
                ApplicationType::Native,
                vec![OAuthGrantType::AuthorizationCode {
                    redirect_uris: vec![
                        reqwest::Url::parse("http://127.0.0.1/").map_err(|_| AuthError::Storage)?,
                    ],
                }],
                Localized::new(
                    reqwest::Url::parse("https://github.com/chrislearn/hagency-client")
                        .map_err(|_| AuthError::Storage)?,
                    None,
                ),
            )
        };
        let data = oauth
            .login(
                redirect,
                previous
                    .as_ref()
                    .map(|saved| saved.user.meta.device_id.clone()),
                Some(
                    Raw::new(&registration)
                        .map_err(|_| AuthError::Storage)?
                        .into(),
                ),
                None,
            )
            .build()
            .await
            .map_err(|_| AuthError::Unavailable)?;
        let requested_device = requested_device(&data.url)?;
        if open(data.url.to_string()).await.is_err() {
            oauth.abort_login(&data.state).await;
            return Err(AuthError::Cancelled);
        }
        let query = match tokio::time::timeout(Duration::from_secs(300), server.into_future()).await
        {
            Ok(Some(query)) => query,
            _ => {
                oauth.abort_login(&data.state).await;
                return Err(AuthError::Cancelled);
            }
        };
        let completed = async {
            let saved = complete_new_grant(
                &client,
                query.into(),
                origin,
                expected_owner,
                &requested_device,
                TIMEOUT,
                Duration::from_millis(100),
            )
            .await?;
            let owner = saved.owner.clone();
            if let Some(previous) = &previous {
                verify_same_session(previous, &saved)?;
            }
            // Reset only an uncommitted first-enrollment store, after identity
            // is verified. Existing enrollment keys and history stay intact.
            if previous.is_none() && dir.join(STORE).exists() {
                std::fs::remove_dir_all(dir.join(STORE)).map_err(|_| AuthError::Storage)?;
            }
            {
                let _io = SESSION_IO.lock().map_err(|_| AuthError::Storage)?;
                write_saved(dir, &saved)?;
            }
            let custody = Self::restore(dir, origin, &owner).await?;
            custody.explicit.store(true, Ordering::Release);
            Ok(custody)
        }
        .await;
        match completed {
            Ok(custody) => Ok(custody),
            Err(error) => {
                // Revoke only the grant this attempt created.
                if client.access_token().is_some_and(|token| {
                    previous
                        .as_ref()
                        .is_none_or(|saved| token != saved.user.tokens.access_token)
                }) {
                    let _ = tokio::time::timeout(TIMEOUT, client.oauth().logout()).await;
                }
                let _io = SESSION_IO.lock().map_err(|_| AuthError::Storage)?;
                if let Some(previous) = &previous {
                    write_saved(dir, previous)?;
                } else {
                    let _ = std::fs::remove_file(dir.join(SESSION));
                }
                Err(error)
            }
        }
    }

    /// Explicit owner sign-out of the daemon device: revokes the SDK grant
    /// and deletes the saved session. Native enrollment revocation is done
    /// separately by the runtime before this is called.
    pub async fn sign_out(&self, dir: &Path) -> Result<(), AuthError> {
        self.set_state(AuthState::Retired);
        let revoked = tokio::time::timeout(TIMEOUT, self.client.oauth().logout())
            .await
            .map_err(|_| AuthError::Unavailable)
            .and_then(|r| r.map_err(|_| AuthError::Unavailable));
        // Preserve private retry material if the remote outcome is unknown.
        // Native execution has already been durably revoked by the caller.
        revoked?;
        let _io = SESSION_IO.lock().map_err(|_| AuthError::Storage)?;
        match std::fs::remove_file(dir.join(SESSION)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(AuthError::Storage),
        }
        Ok(())
    }

    /// The enrolled identity, if any, without opening the SDK store.
    pub fn enrolled(dir: &Path) -> Result<Option<(String, String)>, AuthError> {
        Ok(read_saved(dir)?.map(|s| (s.homeserver, s.owner)))
    }

    async fn token(&self) -> Result<MatrixAccessToken, NativeError> {
        if !self.live() {
            return Err(source_error("matrix_account_changed"));
        }
        let oauth = self.client.oauth();
        let client_id = oauth
            .client_id()
            .ok_or_else(|| source_error("matrix_oauth_sign_in_required"))?
            .as_str()
            .to_owned();
        let metadata = tokio::time::timeout(TIMEOUT, oauth.cached_server_metadata())
            .await
            .map_err(|_| source_error("matrix_authorization_unavailable"))?
            .map_err(|_| source_error("matrix_authorization_unavailable"))?;
        let issuer = reqwest::Url::parse(&self.origin)
            .and_then(|url| url.join("auth/"))
            .map_err(|_| source_error("unsupported_hagency_issuer"))?;
        if metadata.issuer != issuer {
            return Err(source_error("unsupported_hagency_issuer"));
        }
        // This real SDK request refreshes an expired access token.
        let identity = match tokio::time::timeout(TIMEOUT, self.client.whoami()).await {
            Err(_) => {
                self.set_state(AuthState::Unavailable);
                return Err(source_error("matrix_authorization_unavailable"));
            }
            Ok(Err(error)) if http_requires_login(&error) => {
                self.set_state(AuthState::SignInRequired);
                return Err(source_error("matrix_authorization_required"));
            }
            Ok(Err(_)) => {
                self.set_state(AuthState::Unavailable);
                return Err(source_error("matrix_authorization_unavailable"));
            }
            Ok(Ok(identity)) => identity,
        };
        if !self.live() {
            return Err(source_error("matrix_account_changed"));
        }
        if identity.user_id.as_str() != self.owner
            || identity.device_id.as_deref() != self.client.device_id()
        {
            self.set_state(AuthState::SignInRequired);
            return Err(source_error("matrix_account_mismatch"));
        }
        self.set_state(AuthState::Authorized);
        Ok(MatrixAccessToken {
            access_token: self
                .client
                .access_token()
                .ok_or_else(|| source_error("matrix_authorization_required"))?,
            client_id,
        })
    }

    async fn refresh(&self) -> Result<MatrixAccessToken, NativeError> {
        if !self.live() {
            return Err(source_error("matrix_account_changed"));
        }
        match tokio::time::timeout(TIMEOUT, self.client.oauth().refresh_access_token()).await {
            Err(_) => {
                self.set_state(AuthState::Unavailable);
                return Err(source_error("matrix_authorization_unavailable"));
            }
            Ok(Err(error)) if refresh_requires_login(&error) => {
                self.set_state(AuthState::SignInRequired);
                return Err(source_error("matrix_authorization_required"));
            }
            Ok(Err(_)) => {
                self.set_state(AuthState::Unavailable);
                return Err(source_error("matrix_authorization_unavailable"));
            }
            Ok(Ok(())) => {}
        }
        self.token().await
    }
}

/// Weak token source handed to the native engine. A retired custody stops
/// authorizing immediately, even while the engine still holds this handle.
pub struct DaemonTokens(Weak<SdkCustody>);

impl MatrixTokenSource for DaemonTokens {
    fn permits_explicit_reenrollment(&self) -> bool {
        self.0
            .upgrade()
            .is_some_and(|c| c.live() && c.explicit.load(Ordering::Acquire))
    }
    fn consume_explicit_reenrollment(&self) -> bool {
        self.0
            .upgrade()
            .is_some_and(|c| c.live() && c.explicit.swap(false, Ordering::AcqRel) && c.live())
    }
    fn access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async move {
            let custody = self
                .0
                .upgrade()
                .ok_or_else(|| source_error("matrix_account_changed"))?;
            custody.token().await
        })
    }
    fn refresh_access_token(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>> + Send + '_>,
    > {
        Box::pin(async move {
            let custody = self
                .0
                .upgrade()
                .ok_or_else(|| source_error("matrix_account_changed"))?;
            custody.refresh().await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private_dir() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        temp
    }

    #[test]
    fn only_https_or_loopback_servers_are_enrollable() {
        assert!(valid_origin("https://hagency.example/"));
        assert!(valid_origin("http://127.0.0.1:8008/"));
        assert!(valid_origin("http://localhost:8008/"));
        assert!(!valid_origin("http://hagency.example/"));
        assert!(!valid_origin("https://user:pw@hagency.example/"));
        assert!(!valid_origin("not a url"));
    }

    #[tokio::test]
    async fn restore_requires_enrollment_and_the_exact_owner() {
        let temp = private_dir();
        assert_eq!(
            SdkCustody::restore(temp.path(), "https://s.example/", "@o:s.example")
                .await
                .err(),
            Some(AuthError::NotEnrolled)
        );
        let saved = Saved {
            version: 1,
            homeserver: "https://s.example/".into(),
            owner: "@o:s.example".into(),
            oauth_client_id: "client".into(),
            user: serde_json::from_value(serde_json::json!({
                "user_id":"@o:s.example","device_id":"DAEMONDEV","access_token":"redacted"
            }))
            .unwrap(),
        };
        write_saved(temp.path(), &saved).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                std::fs::metadata(temp.path().join(SESSION)).unwrap().mode() & 0o077,
                0
            );
        }
        assert_eq!(
            SdkCustody::enrolled(temp.path()).unwrap(),
            Some(("https://s.example/".into(), "@o:s.example".into()))
        );
        for (origin, owner) in [
            ("https://other.example/", "@o:s.example"),
            ("https://s.example/", "@p:s.example"),
        ] {
            assert_eq!(
                SdkCustody::restore(temp.path(), origin, owner).await.err(),
                Some(AuthError::AccountMismatch)
            );
        }
        // Enrollment never overwrites an existing device silently.
        let refused =
            SdkCustody::enroll(temp.path(), "http://127.0.0.1:9/", None, None, |_| async {
                Ok(())
            })
            .await;
        assert!(refused.is_err());
        assert!(temp.path().join(SESSION).exists());
    }

    #[tokio::test]
    async fn explicit_setup_resumes_saved_session_after_interrupted_registration() {
        let temp = private_dir();
        let saved = Saved {
            version: 1,
            homeserver: "http://127.0.0.1:9/".into(),
            owner: "@o:127.0.0.1".into(),
            oauth_client_id: "client".into(),
            user: serde_json::from_value(serde_json::json!({
                "user_id":"@o:127.0.0.1","device_id":"DAEMONDEV","access_token":"test-only"
            }))
            .unwrap(),
        };
        // This is the durable boundary reached just before native enrollment.
        write_saved(temp.path(), &saved).unwrap();
        let original = std::fs::read(temp.path().join(SESSION)).unwrap();
        assert!(
            super::super::enrollment::Enrollment::load(temp.path())
                .unwrap()
                .is_none()
        );
        let custody = SdkCustody::enroll(
            temp.path(),
            &saved.homeserver,
            Some(&saved.owner),
            Some("client"),
            |_| async { panic!("retry must not create another browser grant") },
        )
        .await
        .unwrap();
        assert_eq!(custody.client().device_id().unwrap().as_str(), "DAEMONDEV");
        assert_eq!(std::fs::read(temp.path().join(SESSION)).unwrap(), original);
        assert!(custody.tokens().consume_explicit_reenrollment());
        assert!(!custody.tokens().consume_explicit_reenrollment());
        custody.retire();
        drop(custody);
        for (owner, client) in [
            ("@other:127.0.0.1", "client"),
            ("@o:127.0.0.1", "other-client"),
        ] {
            assert!(matches!(
                SdkCustody::enroll(
                    temp.path(),
                    &saved.homeserver,
                    Some(owner),
                    Some(client),
                    |_| async { panic!("mismatched saved scope must not launch login") }
                )
                .await,
                Err(AuthError::AccountMismatch)
            ));
        }
        assert_eq!(std::fs::read(temp.path().join(SESSION)).unwrap(), original);
    }

    #[tokio::test]
    async fn restored_custody_gates_tokens_and_explicit_permission() {
        let temp = private_dir();
        let saved = Saved {
            version: 1,
            homeserver: "http://127.0.0.1:9/".into(),
            owner: "@o:127.0.0.1".into(),
            oauth_client_id: "client".into(),
            user: serde_json::from_value(serde_json::json!({
                "user_id":"@o:127.0.0.1","device_id":"DAEMONDEV","access_token":"redacted"
            }))
            .unwrap(),
        };
        write_saved(temp.path(), &saved).unwrap();
        let custody = SdkCustody::restore(temp.path(), "http://127.0.0.1:9/", "@o:127.0.0.1")
            .await
            .unwrap();
        let tokens = custody.tokens();
        // A restored session is not a fresh sign-in.
        assert!(!tokens.permits_explicit_reenrollment());
        assert!(!tokens.consume_explicit_reenrollment());
        // Nothing listens on port 9: unavailable, never authorized.
        let error = tokens.access_token().await.unwrap_err();
        assert_eq!(error.code, "matrix_authorization_unavailable");
        assert!(custody.live());
        custody.retire();
        assert!(!custody.live());
        assert_eq!(
            tokens.access_token().await.unwrap_err().code,
            "matrix_account_changed"
        );
        // Retirement is terminal.
        custody.set_state(AuthState::Authorized);
        assert_eq!(custody.state(), AuthState::Retired);
        drop(custody);
        assert_eq!(
            tokens.access_token().await.unwrap_err().code,
            "matrix_account_changed"
        );
    }
}

#[cfg(test)]
mod reauthentication_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn server(authorized: bool) -> (String, tokio::task::JoinHandle<()>) {
        server_identity(authorized,"@owner:test","ORIGINAL_DEVICE").await
    }
    async fn server_identity(authorized:bool,owner:&'static str,device:&'static str)->(String,tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let base = origin.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let base = base.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    let mut buf = [0; 2048];
                    while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                        let Ok(n) = stream.read(&mut buf).await else {
                            return;
                        };
                        if n == 0 || bytes.len() > 16384 {
                            return;
                        }
                        bytes.extend_from_slice(&buf[..n]);
                    }
                    let request = String::from_utf8_lossy(&bytes);
                    let path = request.split_whitespace().nth(1).unwrap_or("");
                    let (status, value) = if path.contains("auth_metadata") {
                        (
                            200,
                            serde_json::json!({"issuer":format!("{base}auth/"),"authorization_endpoint":format!("{base}auth/authorize"),"token_endpoint":format!("{base}auth/token"),"revocation_endpoint":format!("{base}auth/revoke"),"response_types_supported":["code"],"response_modes_supported":["query","fragment"],"grant_types_supported":["authorization_code","refresh_token"],"code_challenge_methods_supported":["S256"]}),
                        )
                    } else if path.ends_with("/account/whoami") {
                        if authorized {
                            (
                                200,
                                serde_json::json!({"user_id":owner,"device_id":device}),
                            )
                        } else {
                            (
                                401,
                                serde_json::json!({"errcode":"M_UNKNOWN_TOKEN","error":"expired fixture"}),
                            )
                        }
                    } else if path == "/hagency/api/v1/discovery" {
                        (
                            200,
                            serde_json::json!({"product":"hagency-server","version":"1.0.0","protocolVersion":3,"homeserver":base,"issuer":format!("{base}auth/"),"capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"]}),
                        )
                    } else if path.ends_with("/versions") {
                        (200, serde_json::json!({"versions":["v1.15"]}))
                    } else {
                        (
                            404,
                            serde_json::json!({"errcode":"M_UNRECOGNIZED","error":"fixture route"}),
                        )
                    };
                    let body = serde_json::to_vec(&value).unwrap();
                    let response = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                    let _ = stream.write_all(&body).await;
                });
            }
        });
        (origin, task)
    }
    fn fixture(origin: &str) -> (tempfile::TempDir, Saved, Vec<u8>) {
        let temp = tempfile::tempdir().unwrap();
        #[cfg(unix)] {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        hagency_store::private::directory(temp.path()).unwrap();
        let saved=Saved{version:1,homeserver:origin.into(),owner:"@owner:test".into(),oauth_client_id:"original_public_client".into(),user:serde_json::from_value(serde_json::json!({"user_id":"@owner:test","device_id":"ORIGINAL_DEVICE","access_token":"test-only-old-token"})).unwrap()};
        write_saved(temp.path(), &saved).unwrap();
        hagency_store::private::directory(&temp.path().join(STORE)).unwrap();
        std::fs::write(
            temp.path().join(STORE).join("original-crypto-sentinel"),
            b"must survive",
        )
        .unwrap();
        let original = std::fs::read(temp.path().join(SESSION)).unwrap();
        (temp, saved, original)
    }
    #[tokio::test]
    async fn bare_origin_restores_and_verifies_the_pinned_issuer() {
        let (origin, server) = server(true).await;
        let bare = origin.trim_end_matches('/');
        let (temp, saved, _) = fixture(bare);
        let custody = SdkCustody::restore(temp.path(), bare, &saved.owner).await.unwrap();
        let token = custody.token().await.expect("bare origin must resolve /auth/ like browser enrollment");
        assert_eq!(token.client_id, saved.oauth_client_id);
        assert_eq!(custody.client.device_id().unwrap().as_str(), "ORIGINAL_DEVICE");
        custody.retire();
        server.abort();
    }

    #[tokio::test]
    async fn valid_saved_device_is_freshly_verified_without_another_browser_grant() {
        let (origin, server) = server(true).await;
        let (temp, saved, _) = fixture(&origin);
        let custody = SdkCustody::reauthenticate(
            temp.path(),
            &origin,
            Some(&saved.owner),
            Some(&saved.oauth_client_id),
            |_| async { panic!("valid device must be reused") },
        )
        .await
        .unwrap();
        assert_eq!(
            custody.client.device_id().unwrap().as_str(),
            "ORIGINAL_DEVICE"
        );
        assert!(custody.tokens().consume_explicit_reenrollment());
        assert!(!custody.tokens().consume_explicit_reenrollment());
        assert_eq!(
            read_saved(temp.path()).unwrap().unwrap().user.meta,
            saved.user.meta
        );
        assert_eq!(
            std::fs::read(temp.path().join(STORE).join("original-crypto-sentinel")).unwrap(),
            b"must survive"
        );
        server.abort();
    }
    #[tokio::test]
    async fn revoked_saved_device_reauthorizes_same_device_and_cancel_preserves_custody() {
        let (origin, server) = server(false).await;
        let (temp, saved, original) = fixture(&origin);
        let opened = Arc::new(AtomicBool::new(false));
        let seen = opened.clone();
        let result =
            SdkCustody::reauthenticate(
                temp.path(),
                &origin,
                Some(&saved.owner),
                Some(&saved.oauth_client_id),
                move |url| async move {
                    let url = reqwest::Url::parse(&url).unwrap();
                    let pairs: std::collections::HashMap<_, _> =
                        url.query_pairs().into_owned().collect();
                    assert_eq!(
                        pairs.get("client_id").map(String::as_str),
                        Some("original_public_client")
                    );
                    assert!(pairs["scope"].split(' ').any(
                        |s| s == "urn:matrix:client:device:ORIGINAL_DEVICE"
                    ));
                    assert_eq!(requested_device(&url).unwrap(), "ORIGINAL_DEVICE");
                    seen.store(true, Ordering::Release);
                    Err(())
                },
            )
            .await;
        assert!(matches!(result, Err(AuthError::Cancelled)));
        assert!(opened.load(Ordering::Acquire));
        assert_eq!(std::fs::read(temp.path().join(SESSION)).unwrap(), original);
        assert_eq!(
            std::fs::read(temp.path().join(STORE).join("original-crypto-sentinel")).unwrap(),
            b"must survive"
        );
        server.abort();
    }
    #[tokio::test]
    async fn temporary_failure_never_opens_another_login_or_removes_original_session() {
        let (temp,saved,original)=fixture("http://127.0.0.1:9/");
        let result=SdkCustody::reauthenticate(temp.path(),&saved.homeserver,Some(&saved.owner),Some(&saved.oauth_client_id),|_|async {panic!("temporary failure cannot replace an enrolled session")}).await;
        assert!(matches!(result,Err(AuthError::Unavailable)));
        assert_eq!(std::fs::read(temp.path().join(SESSION)).unwrap(),original);
        assert!(temp.path().join(STORE).join("original-crypto-sentinel").exists());
    }
    #[tokio::test]
    async fn fresh_wrong_owner_or_device_is_refused_without_new_browser_or_token_write() {
        for (owner,device) in [("@other:test","ORIGINAL_DEVICE"),("@owner:test","OTHER_DEVICE")] {
            let (origin,server)=server_identity(true,owner,device).await;
            let (temp,saved,original)=fixture(&origin);
            let result=SdkCustody::reauthenticate(temp.path(),&origin,Some(&saved.owner),Some(&saved.oauth_client_id),|_|async {panic!("wrong whoami must not create another login")}).await;
            assert!(matches!(result,Err(AuthError::AccountMismatch)));
            assert_eq!(std::fs::read(temp.path().join(SESSION)).unwrap(),original);
            assert!(temp.path().join(STORE).join("original-crypto-sentinel").exists());
            server.abort();
        }
    }

    #[tokio::test]
    async fn reauthentication_refuses_wrong_scope_before_touching_original_session() {
        let (temp, saved, original) = fixture("http://127.0.0.1:9/");
        for (owner, client) in [
            ("@other:test", "original_public_client"),
            ("@owner:test", "other_client"),
        ] {
            let result = SdkCustody::reauthenticate(
                temp.path(),
                &saved.homeserver,
                Some(owner),
                Some(client),
                |_| async { panic!("scope mismatch must not start browser") },
            )
            .await;
            assert!(matches!(result, Err(AuthError::AccountMismatch)));
        }
        assert_eq!(std::fs::read(temp.path().join(SESSION)).unwrap(), original);
    }
}

#[cfg(test)]
#[path = "auth_readiness_tests.rs"]
pub(crate) mod readiness_tests;
