//! Pasion login and user-owned device authorization. All OAuth credentials stay
//! in the Rust host. The browser receives navigation URLs and a local cookie.
use super::{COOKIE, Error, console, current, same_origin};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use reqwest::{Client, Url};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub(super) mod matrix_creations;
mod profiles;
mod sdk_resume;
mod shared_matrix;
mod room_binding;
pub(super) fn matrix_creation_router() -> Router {
    matrix_creations::router()
}

const CALLBACK: &str = "/console/server-login/callback";
const NONCE_COOKIE: &str = "hagency_server_login";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    origin: String,
    client_id: String,
    installation_id: String,
    name: String,
    owner: Option<String>,
    subject: Option<String>,
}
struct Pending {
    state: String,
    verifier: String,
    nonce: String,
    redirect: String,
    binding: Binding,
    known_identity_only: bool,
    expires: Instant,
}
#[derive(Clone)]
struct RemoteSession {
    matrix_source: Option<std::sync::Arc<dyn super::native::MatrixTokenSource>>,
    token: String,
    oauth_token: String,
    user_id: String,
    device: Option<AuthorizedDevice>,
    refresh_token: Option<String>,
    oauth_expires: Instant,
    authorized_until: Instant,
    invalidated: bool,
    binding: Binding,
    checked: Instant,
    expires: Instant,
}
/// A host-only credential; never serialized into a browser response.
#[derive(Clone)]
pub struct AuthorizedDevice {
    origin: String,
    device_id: String,
    generation: u64,
    token: String,
    owner_mxid: String,
    issuer: String,
    subject: String,
    user_id: String,
    valid_until: Instant,
    revoked: std::sync::Arc<AtomicBool>,
    stopped: std::sync::Arc<AtomicBool>,
}
#[cfg(test)]
pub(super) fn fixture_device(
    origin: &str,
    subject: &str,
    mxid: &str,
) -> (AuthorizedDevice, std::sync::Arc<AtomicBool>) {
    let revoked = std::sync::Arc::new(AtomicBool::new(false));
    (
        AuthorizedDevice {
            origin: origin.into(),
            issuer: format!("{}auth/", origin),
            subject: subject.into(),
            owner_mxid: mxid.into(),
            user_id: subject.into(),
            device_id: "fixture_device".into(),
            generation: 1,
            token: "e".repeat(64),
            valid_until: Instant::now() + Duration::from_secs(30),
            revoked: revoked.clone(),
            stopped: std::sync::Arc::new(AtomicBool::new(false)),
        },
        revoked,
    )
}
impl AuthorizedDevice {
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn issuer(&self) -> &str {
        &self.issuer
    }
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Check immediately before sending; callers must not persist snapshots.
    pub fn bearer(&self) -> Result<&str, Error> {
        if self.stopped.load(Ordering::Acquire)
            || self.revoked.load(Ordering::Acquire)
            || self.valid_until <= Instant::now()
        {
            Err(Error::Unauthorized)
        } else {
            Ok(&self.token)
        }
    }
    pub fn user_id(&self) -> &str {
        &self.user_id
    }
    pub fn owner_mxid(&self) -> &str {
        &self.owner_mxid
    }
    pub fn valid_until(&self) -> Instant {
        self.valid_until
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Revocation {
    origin: String,
    client_id: String,
    session_token: String,
    oauth_token: String,
    hint: String,
    session_done: bool,
    oauth_done: bool,
}
pub(super) struct ServerLogin {
    sdk_resume: sdk_resume::Journal,
    pub(super) native_expected: std::sync::Mutex<Option<(String, String)>>,
    path: Option<PathBuf>,
    profiles_path: Option<PathBuf>,
    profiles: Mutex<profiles::Profiles>,
    transition: Mutex<()>,
    renewal: Mutex<()>,
    worker_started: AtomicBool,
    stopped: std::sync::Arc<AtomicBool>,
    revocations_path: Option<PathBuf>,
    revocations: Mutex<Vec<Revocation>>,
    binding: Mutex<Option<Binding>>,
    pending: Mutex<Option<Pending>>,
    status: Mutex<Value>,
    sessions: Mutex<std::collections::HashMap<String, RemoteSession>>,
}
pub(in crate::console) fn read_private_json(path: &Path, limit: u64) -> Result<Vec<u8>, Error> {
    use std::io::Read;
    let file = hagency_store::private::open(path, false).map_err(|_| Error::Unavailable)?;
    if file.metadata().map_err(|_| Error::Unavailable)?.len() > limit {
        return Err(Error::Unavailable);
    }
    let mut raw = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut raw)
        .map_err(|_| Error::Unavailable)?;
    if raw.len() as u64 > limit {
        return Err(Error::Unavailable);
    }
    Ok(raw)
}
impl ServerLogin {
    pub(super) fn new(state: Option<&Path>) -> Result<Self, Error> {
        let path = state.map(|dir| dir.join("server-login.json"));
        let binding = match &path {
            Some(path) if path.exists() => {
                let raw = read_private_json(path, 16384)?;
                if raw.len() > 16384 {
                    return Err(Error::Unavailable);
                }
                let value: Binding =
                    serde_json::from_slice(&raw).map_err(|_| Error::Unavailable)?;
                origin(&value.origin).map_err(|_| Error::Unavailable)?;
                if value.client_id.is_empty()
                    || value.client_id.len() > 128
                    || value.installation_id.len() < 32
                    || value.installation_id.len() > 128
                    || !value
                        .installation_id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    || value.name.trim().is_empty()
                    || value.name.chars().count() > 128
                    || value.owner.is_some() != value.subject.is_some()
                    || value
                        .owner
                        .as_ref()
                        .is_some_and(|v| v.is_empty() || v.len() > 255)
                    || value
                        .subject
                        .as_ref()
                        .is_some_and(|v| v.is_empty() || v.len() > 255)
                {
                    return Err(Error::Unavailable);
                }

                Some(value)
            }
            _ => None,
        };
        let profiles_path = state.map(|dir| dir.join("server-login-profiles.json"));
        let profiles = profiles::Profiles::load(profiles_path.as_deref(), binding.as_ref())?;
        let revocations_path = state.map(|dir| dir.join("server-login-revocations.json"));
        let revocations: Vec<Revocation> = match &revocations_path {
            Some(path) if path.exists() => {
                let raw = read_private_json(path, 1024 * 1024)?;
                if raw.len() > 1024 * 1024 {
                    return Err(Error::Unavailable);
                }
                let values: Vec<Revocation> =
                    serde_json::from_slice(&raw).map_err(|_| Error::Unavailable)?;
                if values.len() > 64
                    || values.iter().any(|v| {
                        origin(&v.origin).is_err()
                            || v.client_id.is_empty()
                            || v.client_id.len() > 128
                            || (v.session_token.is_empty() && !v.session_done)
                            || (!v.session_token.is_empty() && v.session_token.len() != 64)
                            || (!v.oauth_done && v.oauth_token.is_empty())
                            || v.oauth_token.len() > 4096
                            || !["access_token", "refresh_token"].contains(&v.hint.as_str())
                    })
                {
                    return Err(Error::Unavailable);
                }
                values
            }
            _ => vec![],
        };
        let revocation_pending = !revocations.is_empty();
        Ok(Self {
            sdk_resume: sdk_resume::Journal::open(state)?,
            native_expected: std::sync::Mutex::new(None),
            path,
            profiles_path,
            profiles: Mutex::new(profiles),
            transition: Mutex::new(()),
            renewal: Mutex::new(()),
            revocations_path,
            revocations: Mutex::new(revocations),
            worker_started: AtomicBool::new(false),
            stopped: std::sync::Arc::new(AtomicBool::new(false)),
            binding: Mutex::new(binding),
            pending: Mutex::new(None),
            status: Mutex::new(if revocation_pending {
                json!({"state":"signed_out","deviceAuthorized":false,"transportOnline":false,"remoteRevocationPending":true})
            } else {
                json!({"state":"idle"})
            }),
            sessions: Mutex::new(std::collections::HashMap::new()),
        })
    }
    /// Start one bounded renewal task. Weak ownership ensures dropping the
    /// console ends the worker; UI activity is not required for authorization.
    pub(super) fn start_worker(console: &super::Console) {
        if console
            .0
            .server_login
            .worker_started
            .swap(true, Ordering::AcqRel)
        {
            return;
        }
        let weak = std::sync::Arc::downgrade(&console.0);
        tokio::spawn(async move {
            let mut last_revocations = Instant::now() - Duration::from_secs(10);
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                let Some(inner) = weak.upgrade() else { break };
                let login = &inner.server_login;
                if login.stopped.load(Ordering::Acquire) {
                    break;
                }
                let keys: Vec<_> = login
                    .sessions
                    .lock()
                    .await
                    .iter()
                    .filter(|(_, s)| {
                        !s.invalidated
                            && (s.checked.elapsed() >= Duration::from_secs(10)
                                || s.authorized_until <= Instant::now())
                    })
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in keys {
                    let _ = login.renew_session(&key).await;
                }
                if last_revocations.elapsed() >= Duration::from_secs(10) {
                    login.flush_revocations().await;
                    last_revocations = Instant::now();
                }
            }
        });
    }
    /// A remote cookie cannot turn into a local recovery login when revoked.
    pub(super) async fn validate(&self, cookie: &str) -> Result<(), Error> {
        let key = session_key(cookie);
        let session = self.sessions.lock().await.get(&key).cloned();
        let Some(session) = session else {
            return Ok(());
        };
        if session.invalidated || session.expires <= Instant::now() {
            return Err(Error::Unauthorized);
        }
        self.renew_session(&key).await
    }
    /// Stop snapshots immediately without waiting for network or session locks.
    pub(super) fn retire(&self) {
        self.stopped.store(true, Ordering::Release);
    }
    /// Refresh a snapshot outside the session map lock. Renewal is single-flight,
    /// while management requests and sign-out can proceed independently.
    async fn renew_session(&self, key: &str) -> Result<(), Error> {
        let _renewal = self.renewal.lock().await;
        self.renew_session_inner(key).await
    }
    // Caller holds the renewal guard through cookie-generation selection.
    async fn renew_session_inner(&self, key: &str) -> Result<(), Error> {
        let mut snapshot = self
            .sessions
            .lock()
            .await
            .get(key)
            .cloned()
            .filter(|s| !s.invalidated)
            .ok_or(Error::Unauthorized)?;
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(8), renew(&mut snapshot))
            .await
            .unwrap_or(Err(Error::Unavailable));
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(key)
            .filter(|s| !s.invalidated && s.token == snapshot.token)
            .ok_or(Error::Unauthorized)?;
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Unauthorized);
        }
        match result {
            Ok(()) => {
                *session = snapshot;
                let mut status = self.status.lock().await;
                if status["code"] == "matrix_authorization_unavailable" {
                    *status = json!({"state":"device_authorized","deviceAuthorized":true,"transportOnline":false});
                    tracing::info!(stage = "owner_renewal", "Owner authorization recovered");
                }
                drop(status);
                tracing::debug!(
                    stage = "owner_renewal",
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    remaining_ms = session
                        .authorized_until
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64,
                    "Owner authorization checked"
                );
                Ok(())
            }
            Err(Error::Unavailable) => {
                // Preserve rotated OAuth credentials, but never extend a proof
                // after a failed request. A later SDK check can recover it.
                *session = snapshot;
                tracing::warn!(
                    stage = "owner_renewal",
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    remaining_ms = session
                        .authorized_until
                        .saturating_duration_since(Instant::now())
                        .as_millis() as u64,
                    code = "matrix_authorization_unavailable",
                    "Owner renewal will retry"
                );
                *self.status.lock().await = json!({"state":"failed","code":"matrix_authorization_unavailable","deviceAuthorized":false,"transportOnline":false});
                Err(Error::Unavailable)
            }
            Err(error) => {
                if session.matrix_source.is_some() {
                    // An SDK account switch can race local retirement. Renewal
                    // refusal does not prove that the saved original account
                    // was revoked: close this process's capability only. A
                    // fresh same-owner resume independently verifies the
                    // original device; explicit logout persists its tombstone.
                    invalidate(session);
                } else {
                    self.queue_revocation(session).await;
                }
                *self.status.lock().await = json!({"state":"failed","code":"sign_in_required","deviceAuthorized":false,"transportOnline":false});
                Err(error)
            }
        }
    }
    /// Only the trusted Rust runtime can obtain a current device credential.
    pub(super) async fn authorized_device(&self) -> Result<AuthorizedDevice, Error> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Unauthorized);
        }
        let _renewal = self.renewal.lock().await;
        let key = self
            .sessions
            .lock()
            .await
            .iter()
            // An expired finite proof can still be refreshed. Selecting the
            // grant never authorizes execution; renewal must succeed below.
            .find(|(_, s)| !s.invalidated)
            .map(|(key, _)| key.clone())
            .ok_or(Error::Unauthorized)?;
        self.renew_session_inner(&key).await?;
        let sessions = self.sessions.lock().await;
        let session = sessions
            .get(&key)
            .filter(|s| !s.invalidated)
            .ok_or(Error::Unauthorized)?;
        if self.stopped.load(Ordering::Acquire) || session.authorized_until <= Instant::now() {
            return Err(Error::Unauthorized);
        }
        let mut device = session.device.clone().ok_or(Error::Unauthorized)?;
        device.valid_until = session.authorized_until;
        device.stopped = self.stopped.clone();
        Ok(device)
    }
    async fn queue_revocation(&self, session: &mut RemoteSession) {
        if session.invalidated {
            return;
        }
        let shared = session.matrix_source.is_some();
        if shared {
            // A later resume must not race an explicit or terminal revocation.
            if self.sdk_resume.revoke(Some(&session.token)).is_err() {
                tracing::error!(
                    code = "native_logout_persistence_failed",
                    "Native session revocation could not be persisted"
                );
            }
        }
        let (oauth_token, hint) = if shared {
            session.oauth_token.clear();
            session.refresh_token = None;
            (String::new(), "access_token")
        } else {
            match session.refresh_token.take() {
                Some(token) => (token, "refresh_token"),
                None => (std::mem::take(&mut session.oauth_token), "access_token"),
            }
        };
        let pending = Revocation {
            origin: session.binding.origin.clone(),
            client_id: session.binding.client_id.clone(),
            session_token: std::mem::take(&mut session.token),
            oauth_token,
            hint: hint.to_owned(),
            session_done: false,
            oauth_done: shared,
        };
        invalidate(session);
        let mut revocations = self.revocations.lock().await;
        // The saved SDK record may already have queued this exact native
        // credential before the in-memory session is retired. Preserve any
        // existing OAuth cleanup, and retry the original remote revoke once.
        if !shared
            || !revocations.iter().any(|r| {
                r.origin == pending.origin
                    && r.client_id == pending.client_id
                    && r.session_token == pending.session_token
            })
        {
            revocations.push(pending);
        }
        let _ = self.save_revocations(&revocations);
    }
    pub(super) async fn revoke_native_all(&self) {
        self.queue_saved_sdk_revocation().await;
        for session in self.sessions.lock().await.values_mut() {
            self.queue_revocation(session).await;
        }
        self.pending.lock().await.take();
    }
    pub(super) async fn flush_native_revocations(&self) {
        self.flush_revocations().await;
    }
    pub(super) async fn sign_out(&self, cookie: &str) {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(&session_key(cookie)) {
            // Persist only a pending revoke record, never a usable runtime
            // credential, before network operations. Stop snapshots immediately.
            self.queue_revocation(session).await;
            *self.status.lock().await = json!({"state":"signed_out","deviceAuthorized":false,"transportOnline":false,"remoteRevocationPending":true});
        }
        drop(sessions);
        self.flush_revocations().await;
    }
    fn save_revocations(&self, values: &[Revocation]) -> Result<(), Error> {
        let path = self.revocations_path.as_ref().ok_or(Error::Unavailable)?;
        let raw = serde_json::to_vec(values).map_err(|_| Error::Unavailable)?;
        hagency_store::private::replace(path, &raw).map_err(|_| Error::Unavailable)
    }
    async fn revoke_failed_login(
        &self,
        binding: &Binding,
        session_token: &str,
        token: &str,
        refresh_token: Option<&str>,
    ) -> Result<(), &'static str> {
        let mut revocations = self.revocations.lock().await;
        revocations.push(Revocation {
            origin: binding.origin.clone(),
            client_id: binding.client_id.clone(),
            session_token: session_token.into(),
            oauth_token: refresh_token.unwrap_or(token).into(),
            hint: if refresh_token.is_some() {
                "refresh_token"
            } else {
                "access_token"
            }
            .into(),
            session_done: session_token.is_empty(),
            oauth_done: false,
        });
        self.save_revocations(&revocations)
            .map_err(|_| "local_state_unavailable")?;
        drop(revocations);
        self.flush_revocations().await;
        Ok(())
    }
    async fn flush_revocations(&self) {
        let mut pending = self.revocations.lock().await;
        if pending.is_empty() {
            return;
        }
        let Ok(client) = http() else { return };
        for record in pending.iter_mut() {
            let Ok(server) = origin(&record.origin) else {
                continue;
            };
            if !record.session_done
                && let Ok(reply) = client
                    .delete(server.join("/hagency/api/v1/sessions/current").unwrap())
                    .bearer_auth(&record.session_token)
                    .send()
                    .await
            {
                // Expired/previously revoked sessions are already inactive.
                record.session_done = reply.status().is_success()
                    || reply.status() == reqwest::StatusCode::UNAUTHORIZED;
            }
            if !record.oauth_done
                && let Ok(reply) = client
                    .post(server.join("/auth/oauth2/revoke").unwrap())
                    .form(&[
                        ("client_id", record.client_id.as_str()),
                        ("token", record.oauth_token.as_str()),
                        ("token_type_hint", record.hint.as_str()),
                    ])
                    .send()
                    .await
            {
                record.oauth_done = reply.status().is_success();
            }
        }
        pending.retain(|p| !(p.session_done && p.oauth_done));
        let _ = self.save_revocations(&pending);
        if pending.is_empty() {
            let mut status = self.status.lock().await;
            if status["state"] == "signed_out" {
                status["remoteRevocationPending"] = json!(false);
            }
        }
    }
    async fn save(&self, binding: Binding) -> Result<(), &'static str> {
        let path = self.path.as_ref().ok_or("local_state_unavailable")?;
        let raw = serde_json::to_vec(&binding).map_err(|_| "local_state_unavailable")?;
        self.save_profile(&binding).await?;
        hagency_store::private::replace(path, &raw).map_err(|_| "local_state_unavailable")?;
        *self.binding.lock().await = Some(binding);
        Ok(())
    }
}
fn session_key(cookie: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(cookie.as_bytes()))
}
fn invalidate(session: &mut RemoteSession) {
    session.invalidated = true;
    session.authorized_until = Instant::now();
    if let Some(device) = session.device.take() {
        device.revoked.store(true, Ordering::Release);
    }
    session.oauth_token.clear();
    session.refresh_token = None;
    session.token.clear();
}
fn deadline(value: &Value) -> Result<Instant, &'static str> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "invalid_local_clock")?
        .as_millis() as u64;
    let until = value["validUntilMs"]
        .as_u64()
        .ok_or("invalid_server_response")?;
    let remaining = until
        .checked_sub(now)
        .filter(|ms| *ms > 0 && *ms <= 31_000)
        .ok_or("sign_in_required")?;
    Ok(Instant::now() + Duration::from_millis(remaining))
}
fn oauth_credentials(tokens: &Value) -> Result<(String, Option<String>, Duration), &'static str> {
    if tokens["token_type"]
        .as_str()
        .is_some_and(|t| !t.eq_ignore_ascii_case("bearer"))
    {
        return Err("invalid_server_response");
    }
    let token = tokens["access_token"]
        .as_str()
        .filter(|t| !t.is_empty() && t.len() <= 4096)
        .ok_or("invalid_server_response")?
        .to_owned();
    let refresh = match tokens.get("refresh_token") {
        Some(value) => Some(
            value
                .as_str()
                .filter(|t| !t.is_empty() && t.len() <= 4096)
                .ok_or("invalid_server_response")?
                .to_owned(),
        ),
        None => None,
    };
    let lifetime = tokens["expires_in"]
        .as_u64()
        .filter(|v| *v > 0 && *v <= 31_536_000)
        .ok_or("invalid_server_response")?;
    Ok((token, refresh, Duration::from_secs(lifetime)))
}
fn may_stop_session(active: Option<&Binding>, session: &RemoteSession) -> bool {
    !session.invalidated && active.is_some_and(|binding| same_profile(binding, &session.binding))
}
fn same_profile(left: &Binding, right: &Binding) -> bool {
    left.origin == right.origin
        && left.owner.is_some()
        && left.owner == right.owner
        && left.subject.is_some()
        && left.subject == right.subject
}
fn identity_matches(identity: &Value, binding: &Binding) -> bool {
    identity["mxid"] == binding.owner.as_deref().unwrap_or("")
        && identity["subject"] == binding.subject.as_deref().unwrap_or("")
        && identity["clientId"] == binding.client_id
        && identity["issuer"] == format!("{}auth/", binding.origin)
}
fn renewal_error(code: &str) -> Error {
    if matches!(
        code,
        "sign_in_required" | "authorization_expired" | "authentication_required"
    ) {
        Error::Unauthorized
    } else {
        Error::Unavailable
    }
}
async fn renew(session: &mut RemoteSession) -> Result<(), Error> {
    if session.invalidated {
        return Err(Error::Unauthorized);
    }
    // Every execution RPC still reaches the server's current lease/membership
    // gates. A finite SDK proof need not be re-requested before AND after each
    // poll: refresh it at most five seconds apart, or before its deadline.
    if session.matrix_source.is_some()
        && session.checked.elapsed() < Duration::from_secs(5)
        && session.authorized_until > Instant::now() + Duration::from_secs(10)
        && session.oauth_expires > Instant::now() + Duration::from_secs(10)
    {
        return Ok(());
    }
    if session.matrix_source.is_none()
        && session.checked.elapsed() < Duration::from_secs(20)
        && session.authorized_until > Instant::now()
        && session.oauth_expires > Instant::now() + Duration::from_secs(10)
    {
        return Ok(());
    }
    let server = origin(&session.binding.origin).map_err(|_| Error::Unauthorized)?;
    let client = http().map_err(|_| Error::Unavailable)?;
    let shared_token = if let Some(source) = &session.matrix_source {
        let snapshot = source.access_token().await.map_err(|error| {
            let code = super::native::sdk_source_failure(error);
            eprintln!("hagency owner renewal failure: stage=sdk_source code={code}");
            if code == "matrix_authorization_unavailable" {
                Error::Unavailable
            } else {
                Error::Unauthorized
            }
        })?;
        if snapshot.client_id != session.binding.client_id || snapshot.access_token.is_empty() {
            return Err(Error::Unauthorized);
        }
        Some(snapshot.access_token)
    } else {
        None
    };
    if shared_token.is_none() && session.oauth_expires <= Instant::now() + Duration::from_secs(10) {
        if let Some(refresh) = &session.refresh_token {
            let tokens = oauth_refresh_response(
                client
                    .post(server.join("/auth/oauth2/token").unwrap())
                    .form(&[
                        ("grant_type", "refresh_token"),
                        ("client_id", session.binding.client_id.as_str()),
                        ("refresh_token", refresh),
                    ])
                    .send()
                    .await
                    .map_err(|_| Error::Unavailable)?,
            )
            .await?;
            let (token, next_refresh, lifetime) =
                oauth_credentials(&tokens).map_err(|_| Error::Unavailable)?;
            session.oauth_token = token;
            if next_refresh.is_some() {
                session.refresh_token = next_refresh;
            }
            session.oauth_expires = Instant::now() + lifetime;
        } else if session.oauth_expires <= Instant::now() {
            return Err(Error::Unauthorized);
        }
    }
    let renewal_response = client
        .post(
            server
                .join("/hagency/api/v1/sessions/current/renew")
                .unwrap(),
        )
        .bearer_auth(&session.token)
        .json(&json!({"accessToken":shared_token.as_ref().unwrap_or(&session.oauth_token)}))
        .send()
        .await
        .map_err(|_| {
            eprintln!("hagency owner renewal failure: stage=server_session_transport");
            Error::Unavailable
        })?;
    if !renewal_response.status().is_success() {
        eprintln!(
            "hagency owner renewal failure: stage=server_session status={}",
            renewal_response.status().as_u16()
        );
    }
    let renewed = response(renewal_response).await.map_err(renewal_error)?;
    let mut until = deadline(&renewed).map_err(|_| Error::Unavailable)?;
    if until <= Instant::now() + Duration::from_secs(20)
        && let Some(source) = &session.matrix_source
    {
        // A short returned proof signals actual token expiry, rather than the
        // normal thirty-second server freshness window. Refresh only once.
        let refreshed = source.refresh_access_token().await.map_err(|error| {
            let code = super::native::sdk_source_failure(error);
            eprintln!("hagency owner renewal failure: stage=sdk_refresh code={code}");
            if code == "matrix_authorization_unavailable" {
                Error::Unavailable
            } else {
                Error::Unauthorized
            }
        })?;
        if refreshed.client_id != session.binding.client_id || refreshed.access_token.is_empty() {
            return Err(Error::Unauthorized);
        }
        let response = client
            .post(
                server
                    .join("/hagency/api/v1/sessions/current/renew")
                    .unwrap(),
            )
            .bearer_auth(&session.token)
            .json(&json!({"accessToken":refreshed.access_token}))
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            eprintln!(
                "hagency owner renewal failure: stage=server_after_refresh status={}",
                response.status().as_u16()
            );
        }
        let refreshed = self::response(response).await.map_err(renewal_error)?;
        until = deadline(&refreshed).map_err(|_| Error::Unavailable)?;
    }
    let identity = get(
        &client,
        server.join("/hagency/api/v1/identity").unwrap(),
        Some(&session.token),
    )
    .await
    .map_err(|code| {
        tracing::warn!(stage = "server_identity", code, "Owner renewal failed");
        renewal_error(code)
    })?;
    if !identity_matches(&identity, &session.binding) || identity["userId"] != session.user_id {
        return Err(Error::Unauthorized);
    }
    let identity_until = deadline(&identity).map_err(|_| Error::Unavailable)?;
    if let Some(source) = &session.matrix_source {
        let after = source.access_token().await.map_err(|error| {
            let code = super::native::sdk_source_failure(error);
            eprintln!("hagency owner renewal failure: stage=sdk_source code={code}");
            if code == "matrix_authorization_unavailable" {
                Error::Unavailable
            } else {
                Error::Unauthorized
            }
        })?;
        if after.client_id != session.binding.client_id {
            return Err(Error::Unauthorized);
        }
        session.oauth_expires = until.min(identity_until);
    }
    session.authorized_until = until.min(identity_until).min(session.oauth_expires);
    session.checked = Instant::now();
    Ok(())
}
fn random() -> Result<String, &'static str> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| "random_unavailable")?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
pub(super) fn origin(value: &str) -> Result<Url, &'static str> {
    crate::server_admission::normalize_origin(value).map_err(|e| e.code())
}
pub(super) fn http() -> Result<Client, &'static str> {
    static CLIENT: std::sync::LazyLock<Result<Client, &'static str>> =
        std::sync::LazyLock::new(|| {
            Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .timeout(Duration::from_secs(15))
                .build()
                .map_err(|_| "server_unavailable")
        });
    CLIENT.clone()
}
async fn read_response(mut response: reqwest::Response) -> Result<Value, &'static str> {
    let mut raw = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "server_unavailable")? {
        if raw.len() + chunk.len() > 128 * 1024 {
            return Err("invalid_server_response");
        }
        raw.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&raw).map_err(|_| "invalid_server_response")
}
async fn oauth_refresh_response(response: reqwest::Response) -> Result<Value, Error> {
    let status = response.status();
    let value = read_response(response)
        .await
        .map_err(|_| Error::Unavailable)?;
    if !status.is_success() {
        return Err(
            if matches!(status.as_u16(), 400 | 401 | 403)
                && matches!(
                    value["error"].as_str(),
                    Some("invalid_grant" | "invalid_client" | "unauthorized_client")
                )
            {
                Error::Unauthorized
            } else {
                Error::Unavailable
            },
        );
    }
    Ok(value)
}
async fn response(response: reqwest::Response) -> Result<Value, &'static str> {
    let status = response.status();
    let value = read_response(response).await?;
    if !status.is_success() {
        if matches!(status.as_u16(), 401 | 403) {
            return Err("sign_in_required");
        }
        return Err(match value["code"].as_str() {
            Some("sign_in_required") => "sign_in_required",
            _ => "server_request_failed",
        });
    }
    Ok(value)
}
async fn get(client: &Client, url: Url, token: Option<&str>) -> Result<Value, &'static str> {
    let mut request = client.get(url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    response(request.send().await.map_err(|_| "server_unavailable")?).await
}
fn cookie(req: &Request, name: &str) -> Option<String> {
    req.headers()
        .get("cookie")?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|v| {
            v.trim()
                .strip_prefix(&format!("{name}="))
                .map(str::to_owned)
        })
}
fn local_redirect(req: &Request) -> Result<String, &'static str> {
    let host = req
        .headers()
        .get("host")
        .and_then(|h| h.to_str().ok())
        .ok_or("invalid_local_origin")?;
    let url =
        Url::parse(&format!("http://{host}{CALLBACK}")).map_err(|_| "invalid_local_origin")?;
    if !url.host_str().is_some_and(|h| {
        h.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    }) {
        return Err("invalid_local_origin");
    }
    Ok(url.to_string())
}
pub(super) fn router() -> Router {
    Router::with_path("server-login")
        .get(login_status)
        .push(Router::with_path("switch").post(profiles::switch))
        .push(Router::with_path("start").post(start))
        .push(Router::with_path("callback").get(callback))
        .push(Router::with_path("sign-out").post(sign_out))
}
#[handler]
async fn login_status(req: &mut Request, depot: &Depot, res: &mut Response) {
    if !same_origin(req, depot, false) {
        super::failed(res, Error::Invalid);
        return;
    }
    let Ok(console) = console(depot) else {
        super::failed(res, Error::Unavailable);
        return;
    };
    ServerLogin::start_worker(console);
    let local_access_ready = current(req, depot).is_ok()
        && match cookie(req, COOKIE) {
            Some(value) => console.0.server_login.validate(&value).await.is_ok(),
            None => false,
        };
    let profiles = console.0.server_login.profiles_status().await;
    let binding = console.0.server_login.binding.lock().await;
    res.render(Json(json!({"localAccessReady":local_access_ready,"profiles":profiles["profiles"],"activeProfileId":profiles["activeProfileId"],"configured":binding.as_ref().is_some_and(|b|b.owner.is_some()),"server":binding.as_ref().map(|b|&b.origin),"name":binding.as_ref().map(|b|&b.name),"status":*console.0.server_login.status.lock().await})));
}
#[handler]
async fn sign_out(req: &mut Request, depot: &Depot, res: &mut Response) {
    if !same_origin(req, depot, true)
        || req.uri().query().is_some()
        || !super::body(req, 1).await.is_ok_and(|b| b.is_empty())
    {
        super::failed(res, Error::Unauthorized);
        return;
    }
    let Ok(console) = console(depot) else {
        super::failed(res, Error::Unavailable);
        return;
    };
    let _transition = console.0.server_login.transition.lock().await;
    let Some(value) = cookie(req, COOKIE) else {
        super::failed(res, Error::Unauthorized);
        return;
    };
    let active = console.0.server_login.binding.lock().await.clone();
    let permitted = console
        .0
        .server_login
        .sessions
        .lock()
        .await
        .get(&session_key(&value))
        .is_some_and(|session| may_stop_session(active.as_ref(), session));
    if !permitted {
        super::failed(res, Error::Unauthorized);
        return;
    }
    let bridge_allowed = console.0.authority.authenticate(&value).is_ok()
        && console.0.server_login.validate(&value).await.is_ok();
    if let Err(error) = console.0.authority.revoke_all() {
        super::failed(res, error);
        return;
    }
    *console.0.server_login.pending.lock().await = None;
    {
        let mut sessions = console.0.server_login.sessions.lock().await;
        for session in sessions.values_mut() {
            console.0.server_login.queue_revocation(session).await;
        }
    }
    *console.0.server_login.status.lock().await = json!({"state":"signed_out","deviceAuthorized":false,"transportOnline":false,"remoteRevocationPending":true});
    console.stop_owned_runtimes().await;
    console.stop_owner_provider().await;
    {
        let records = console.0.server_login.revocations.lock().await;
        if let Err(error) = console.0.server_login.save_revocations(&records) {
            super::failed(res, error);
            return;
        }
    }
    console.0.server_login.flush_revocations().await;
    let new_cookie = if bridge_allowed {
        console
            .0
            .authority
            .issue_owner_ticket()
            .and_then(|ticket| console.0.authority.exchange(&ticket))
    } else {
        Ok(String::new())
    };
    let Ok(new_cookie) = new_cookie else {
        super::failed(res, Error::Unavailable);
        return;
    };
    res.add_header(
        "set-cookie",
        if new_cookie.is_empty() {
            format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/console; Max-Age=0")
        } else {
            format!("{COOKIE}={new_cookie}; HttpOnly; SameSite=Strict; Path=/console")
        },
        true,
    )
    .unwrap();
    res.render(Json(json!({"ok":true})));
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    server: String,
    name: String,
}
#[handler]
async fn start(req: &mut Request, depot: &Depot, res: &mut Response) {
    let result = begin(req, depot).await;
    match result {
        Ok((url, nonce)) => {
            res.add_header("set-cookie",format!("{NONCE_COOKIE}={nonce}; HttpOnly; SameSite=Lax; Path=/console/server-login; Max-Age=300"),true).unwrap();
            res.render(Json(json!({"url":url})));
        }
        Err(code) => {
            res.status_code(if code.ends_with("unavailable") {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_REQUEST
            });
            res.render(Json(json!({"code":code})));
        }
    }
}
async fn begin(req: &mut Request, depot: &Depot) -> Result<(String, String), &'static str> {
    if !same_origin(req, depot, true) {
        return Err("invalid_local_origin");
    }
    let console = console(depot).map_err(|_| "local_state_unavailable")?;
    let login = &console.0.server_login;
    let _transition = login.transition.lock().await;
    let binding = login.binding.lock().await.clone();
    let local_access = current(req, depot).is_ok()
        && match cookie(req, COOKIE) {
            Some(value) => login.validate(&value).await.is_ok(),
            None => false,
        };
    let raw = super::body(req, 8192).await.map_err(|_| "invalid_login")?;
    let input: Start = serde_json::from_slice(&raw).map_err(|_| "invalid_login")?;
    let server = origin(&input.server)?;
    let binding = if local_access {
        binding
    } else {
        // Expired browser authority may authenticate an already admitted identity,
        // but may neither select a new owner nor modify the active binding yet.
        let mut known = login
            .known_server(server.as_str())
            .await
            .ok_or("local_access_required")?;
        known.owner = None;
        known.subject = None;
        Some(known)
    };

    if input.name.trim().is_empty() || input.name.chars().count() > 128 {
        return Err("invalid_name");
    }
    if binding
        .as_ref()
        .is_some_and(|b| b.owner.is_some() && b.origin != server.as_str())
    {
        return Err("owner_mismatch");
    }
    if login.revocations.lock().await.len() >= 63 {
        return Err("local_session_limit");
    }
    let client = http()?;
    crate::server_admission::discover(server.as_str())
        .await
        .map_err(|e| e.code())?;
    let mut binding = match binding.filter(|b| b.origin == server.as_str()) {
        Some(binding) => binding,
        None => {
            let redirect = local_redirect(req)?;
            let mut registered = Url::parse(&redirect).unwrap();
            registered
                .set_port(None)
                .map_err(|_| "invalid_local_origin")?;
            let registration=response(client.post(server.join("/auth/oauth2/registration").unwrap()).json(&json!({"client_name":"Hagency Client","client_uri":"https://github.com/chrislearn/hagency-client","application_type":"native","token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"],"redirect_uris":[registered]})).send().await.map_err(|_|"server_unavailable")?).await?;
            let client_id = registration["client_id"]
                .as_str()
                .filter(|v| v.len() <= 128)
                .ok_or("invalid_server_response")?
                .to_owned();
            Binding {
                origin: server.to_string(),
                client_id,
                installation_id: random()?,
                name: input.name.trim().to_owned(),
                owner: None,
                subject: None,
            }
        }
    };
    if binding.owner.is_none() {
        binding.name = input.name.trim().to_owned();
    }
    if local_access {
        login.save(binding.clone()).await?;
    }
    let state = random()?;
    let verifier = random()?;
    let nonce = random()?;
    let redirect = local_redirect(req)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut authorize = server.join("/auth/authorize").unwrap();
    let device = format!("Hagency{}", &binding.installation_id[..16]);
    authorize.query_pairs_mut().extend_pairs([
        ("prompt", "login"),
        ("response_type", "code"),
        ("client_id", binding.client_id.as_str()),
        ("redirect_uri", redirect.as_str()),
        ("state", state.as_str()),
        ("code_challenge", challenge.as_str()),
        ("code_challenge_method", "S256"),
        (
            "scope",
            format!("urn:matrix:client:api:* urn:matrix:client:device:{device}").as_str(),
        ),
    ]);
    *login.pending.lock().await = Some(Pending {
        state,
        verifier,
        nonce: nonce.clone(),
        redirect,
        binding,
        known_identity_only: !local_access,
        expires: Instant::now() + Duration::from_secs(300),
    });
    *login.status.lock().await = json!({"state":"signing_in"});
    Ok((authorize.to_string(), nonce))
}
#[handler]
async fn callback(req: &mut Request, depot: &Depot, res: &mut Response) {
    let result = finish(req, depot).await;
    let Ok(console) = console(depot) else {
        super::failed(res, Error::Unavailable);
        return;
    };
    if result
        .as_ref()
        .is_err_and(|code| *code == "invalid_oauth_state")
    {
        res.status_code(StatusCode::BAD_REQUEST);
        res.render(Json(json!({"code":"invalid_oauth_state"})));
        return;
    }
    match result {
        Ok((session, status)) => {
            *console.0.server_login.status.lock().await = status;
            res.add_header(
                "set-cookie",
                format!(
                    "{COOKIE}={session}; HttpOnly; SameSite=Strict; Path=/console; Max-Age=900"
                ),
                true,
            )
            .unwrap();
        }
        Err(code) => {
            *console.0.server_login.status.lock().await = json!({"state":"failed","code":code});
        }
    }
    res.add_header(
        "set-cookie",
        format!("{NONCE_COOKIE}=; HttpOnly; SameSite=Lax; Path=/console/server-login; Max-Age=0"),
        false,
    )
    .unwrap();
    res.status_code(StatusCode::SEE_OTHER);
    res.add_header("location", "/console/", true).unwrap();
}
async fn finish(req: &mut Request, depot: &Depot) -> Result<(String, Value), &'static str> {
    let console = console(depot).map_err(|_| "local_state_unavailable")?;
    let login = &console.0.server_login;
    let _transition = login.transition.lock().await;
    let state = req.query::<String>("state").ok_or("invalid_oauth_state")?;
    let mut lock = login.pending.lock().await;
    let pending = lock.as_ref().ok_or("invalid_oauth_state")?;
    if pending.expires <= Instant::now()
        || pending.state != state
        || cookie(req, NONCE_COOKIE).as_deref() != Some(pending.nonce.as_str())
    {
        return Err("invalid_oauth_state");
    }
    let mut pending = lock.take().unwrap();
    drop(lock);
    if req
        .query::<String>("iss")
        .is_some_and(|issuer| issuer != format!("{}auth/", pending.binding.origin))
    {
        return Err("owner_mismatch");
    }
    let code = req
        .query::<String>("code")
        .filter(|c| c.len() <= 4096)
        .ok_or("oauth_login_cancelled")?;
    let client = http()?;
    let server = origin(&pending.binding.origin)?;
    let tokens = response(
        client
            .post(server.join("/auth/oauth2/token").unwrap())
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", pending.binding.client_id.as_str()),
                ("redirect_uri", pending.redirect.as_str()),
                ("code", code.as_str()),
                ("code_verifier", pending.verifier.as_str()),
            ])
            .send()
            .await
            .map_err(|_| "server_unavailable")?,
    )
    .await?;
    let (token, refresh_token, oauth_lifetime) = oauth_credentials(&tokens)?;
    let mut issued_session = String::new();
    let completed = async {
        let grant = response(
            client
                .post(server.join("/hagency/api/v1/sessions/pasion").unwrap())
                .json(&json!({"accessToken":token}))
                .send()
                .await
                .map_err(|_| "server_unavailable")?,
        )
        .await?;
        let session_token = grant["token"]
            .as_str()
            .filter(|t| t.len() == 64)
            .ok_or("invalid_server_response")?;
        issued_session = session_token.to_owned();
        let verified = async {
            let identity = get(
                &client,
                server.join("/hagency/api/v1/identity").unwrap(),
                Some(session_token),
            )
            .await?;
            let authorized_until = deadline(&grant)?.min(deadline(&identity)?);
            identity["userId"]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 128)
                .ok_or("invalid_server_response")?;
            let user = identity["mxid"]
                .as_str()
                .filter(|v| {
                    v.len() <= 255
                        && v.starts_with('@')
                        && v[1..]
                            .split_once(':')
                            .is_some_and(|(local, host)| !local.is_empty() && !host.is_empty())
                        && !v.chars().any(|c| c.is_whitespace() || c.is_control())
                })
                .ok_or("invalid_server_response")?;
            let subject = identity["subject"]
                .as_str()
                .filter(|v| !v.is_empty() && v.len() <= 255)
                .ok_or("invalid_server_response")?;
            if identity["issuer"] != server.join("/auth/").unwrap().as_str()
                || grant["mxid"] != identity["mxid"]
                || grant["userId"] != identity["userId"]
                || identity["clientId"] != pending.binding.client_id
                || pending
                    .binding
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner != user)
                || pending
                    .binding
                    .subject
                    .as_ref()
                    .is_some_and(|owner| owner != subject)
            {
                return Err("owner_mismatch");
            }
            if pending.known_identity_only
                && !login.known_identity(server.as_str(), subject, user).await
            {
                return Err("owner_mismatch");
            }
            if let Some((expected_origin, expected_owner)) = login
                .native_expected
                .lock()
                .map_err(|_| "local_state_unavailable")?
                .as_ref()
                && (pending.binding.origin != *expected_origin || user != expected_owner.as_str())
            {
                return Err("matrix_account_mismatch");
            }
            Ok((
                identity.clone(),
                authorized_until,
                user.to_owned(),
                subject.to_owned(),
            ))
        }
        .await;
        let (identity, authorized_until, user, subject) = verified?;
        if pending.known_identity_only {
            console
                .0
                .authority
                .revoke_all()
                .map_err(|_| "local_state_unavailable")?;
            console.stop_owned_runtimes().await;
            console.stop_owner_provider().await;
        }
        pending.binding.owner = Some(user.to_owned());
        pending.binding.subject = Some(subject.to_owned());
        login.save(pending.binding.clone()).await?;
        let expiry = if refresh_token.is_some() {
            900
        } else {
            oauth_lifetime.as_secs().min(900)
        };
        if expiry == 0 {
            return Err("sign_in_required");
        }
        let session = console
            .0
            .authority
            .server_session(Duration::from_secs(expiry))
            .map_err(|_| "local_state_unavailable")?;
        let mut sessions = login.sessions.lock().await;
        // Replacing this installation's login stops prior capabilities even when
        // their UI cookie has already expired and can be removed from memory.
        for previous in sessions.values_mut() {
            login.queue_revocation(previous).await;
        }
        sessions.retain(|_, s| s.expires > Instant::now());
        if sessions.len() >= 64 {
            return Err("local_session_limit");
        }
        sessions.insert(
            URL_SAFE_NO_PAD.encode(Sha256::digest(session.as_bytes())),
            RemoteSession {
                matrix_source: None,
                token: session_token.to_owned(),
                oauth_token: token.clone(),
                user_id: identity["userId"].as_str().unwrap().into(),
                device: None,
                refresh_token: refresh_token.clone(),
                oauth_expires: Instant::now() + oauth_lifetime,
                authorized_until: authorized_until.min(Instant::now() + oauth_lifetime),
                invalidated: false,
                binding: pending.binding.clone(),
                checked: Instant::now(),
                expires: Instant::now() + Duration::from_secs(expiry),
            },
        );
        drop(sessions);
        let result = register_device(
            &client,
            &server,
            session_token,
            &pending.binding,
            identity["userId"].as_str().unwrap(),
        )
        .await;
        let (status, device_token) = match result {
            Ok((status, token)) => (status, Some(token)),
            Err(code) => (
                json!({"state":"signed_in","deviceAuthorized":false,"code":code}),
                None,
            ),
        };
        if let Some(remote) = login
            .sessions
            .lock()
            .await
            .get_mut(&URL_SAFE_NO_PAD.encode(Sha256::digest(session.as_bytes())))
        {
            remote.device = device_token;
        }
        ServerLogin::start_worker(console);
        Ok((session, status))
    }
    .await;
    if completed.is_err() {
        login
            .revoke_failed_login(
                &pending.binding,
                &issued_session,
                &token,
                refresh_token.as_deref(),
            )
            .await?;
    }
    completed
}
async fn register_device(
    client: &Client,
    server: &Url,
    token: &str,
    binding: &Binding,
    user_id: &str,
) -> Result<(Value, AuthorizedDevice), &'static str> {
    let value = response(
        client
            .post(server.join("/hagency/api/v1/devices").unwrap())
            .bearer_auth(token)
            .json(&json!({"installationId":binding.installation_id,"name":binding.name}))
            .send()
            .await
            .map_err(|_| "server_unavailable")?,
    )
    .await?;
    let device = value["deviceId"]
        .as_str()
        .ok_or("invalid_server_response")?;
    let token = value["token"]
        .as_str()
        .filter(|t| t.len() == 64)
        .ok_or("invalid_server_response")?
        .to_owned();
    Ok((
        json!({"state":"device_authorized","deviceId":device,"generation":value["generation"],"deviceAuthorized":true,"transportOnline":false}),
        AuthorizedDevice {
            origin: binding.origin.clone(),
            device_id: device.to_owned(),
            generation: value["generation"]
                .as_u64()
                .filter(|v| *v > 0)
                .ok_or("invalid_server_response")?,
            token,
            owner_mxid: binding.owner.clone().ok_or("invalid_server_response")?,
            issuer: format!("{}auth/", binding.origin),
            subject: binding.subject.clone().ok_or("invalid_server_response")?,
            user_id: user_id.into(),
            valid_until: deadline(&value)?,
            revoked: std::sync::Arc::new(AtomicBool::new(false)),
            stopped: std::sync::Arc::new(AtomicBool::new(false)),
        },
    ))
}

/// A closed set of owner operations. No caller supplies a URL or bearer token.
pub(super) enum OwnerOperation {
    Management(super::native::management_server::ServerOperation),
    Allocation(super::native::allocation_transport::ServerOperation),
    ProjectSetup(super::native::project_setup::ServerOperation),
    AgentCommandStatus {
        operation: String,
        command: String,
    },
    Agents,
    RoomAgents,
    Devices,
    Agent {
        agent: String,
    },
    AssignExecutionDevice {
        agent: String,
        expected: i64,
    },
    AgentOwnerDirect {
        agent: String,
    },
    OwnerDirect {
        agent: String,
        room: String,
    },
    Projects,
    ScopeState {
        project: String,
        room: Option<String>,
    },
    ScopePause {
        project: String,
        room: Option<String>,
        paused: bool,
    },
    ProjectPolicy {
        project: String,
        expected: i64,
        policy: super::native::ProjectCreationPolicy,
    },
    RoomPolicy {
        project: String,
        room: String,
        expected: i64,
        policy: super::native::RoomCreationPolicy,
    },
    AdoptProject {
        space: String,
    },
    ProjectRooms {
        project: String,
    },
    AdoptRoom {
        project: String,
        room: String,
    },
    RoomRoster {
        project: String,
        room: String,
    },
    Bindings {
        agent: String,
    },
    Binding {
        binding: String,
    },
    SetThreadAutoReply {
        binding: String,
        enabled: bool,
    },
    PauseBinding {
        binding: String,
    },
    ResumeBinding {
        binding: String,
    },
    LeaveBinding {
        binding: String,
    },
    Create {
        name: String,
        command: String,
    },
    Bind {
        agent: String,
        project: String,
        room: String,
        command: String,
    },
    Pause {
        agent: String,
    },
    Resume {
        agent: String,
    },
    Retire {
        agent: String,
    },
}
pub(super) struct OwnerReply {
    pub owner: String,
    pub origin: String,
    pub issuer: String,
    pub subject: String,
    pub value: Value,
}
#[derive(Debug)]
pub(super) struct OwnerError {
    pub status: u16,
    pub code: String,
}
impl OwnerError {
    fn new(status: u16, code: &str) -> Self {
        Self {
            status,
            code: code.into(),
        }
    }
}
fn operation_id(value: &str) -> Result<(), OwnerError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        Err(OwnerError::new(400, "invalid_arguments"))
    } else {
        Ok(())
    }
}
fn policy_scope(project: &str, room: Option<&str>) -> Result<String, OwnerError> {
    operation_id(project)?;
    match room {
        Some(room) => {
            matrix_creations::room_id(room)?;
            if room.contains('%') || !room.contains(':') {
                return Err(OwnerError::new(400, "invalid_room_id"));
            }
            Ok(format!(
                "projects/{project}/rooms/{}",
                percent_encoding::utf8_percent_encode(room, percent_encoding::NON_ALPHANUMERIC)
            ))
        }
        None => Ok(format!("projects/{project}")),
    }
}
impl OwnerOperation {
    fn request(self) -> Result<(reqwest::Method, String, Option<Value>), OwnerError> {
        use reqwest::Method;
        if let Self::Management(operation) = self {
            return operation.request();
        }
        if let Self::Allocation(operation) = self {
            return operation.request();
        }
        if let Self::ProjectSetup(operation) = self {
            return operation.request();
        }
        let binding_request = match &self {
            Self::Binding { binding } => Some((Method::GET, binding, "")),
            Self::PauseBinding { binding } => Some((Method::POST, binding, "/pause")),
            Self::ResumeBinding { binding } => Some((Method::POST, binding, "/resume")),
            Self::LeaveBinding { binding } => Some((Method::DELETE, binding, "")),
            _ => None,
        };
        if let Some((method, binding, suffix)) = binding_request {
            operation_id(binding)?;
            return Ok((
                method,
                format!("/hagency/api/v1/bindings/{binding}{suffix}"),
                None,
            ));
        }
        let (method, suffix, body) = match self {
            Self::Management(_) => unreachable!("management handled above"),
            Self::Allocation(_) => unreachable!("allocation handled above"),
            Self::ProjectSetup(_) => unreachable!("project setup handled above"),
            Self::SetThreadAutoReply { binding, enabled } => {
                operation_id(&binding)?;
                (
                    Method::PUT,
                    format!("bindings/{binding}/reply-policy"),
                    Some(json!({"threadAutoReply":enabled})),
                )
            }
            Self::AgentCommandStatus { operation, command } => {
                if !matches!(operation.as_str(), "agent.create" | "agent.bind") {
                    return Err(OwnerError::new(400, "invalid_arguments"));
                }
                operation_id(&command)?;
                (Method::GET, format!("commands/{operation}/{command}"), None)
            }
            Self::Agents => (Method::GET, "agents".into(), None),
            Self::RoomAgents => (Method::GET, "room-agents".into(), None),
            Self::Projects => (Method::GET, "projects".into(), None),
            Self::ScopeState { project, room } => (
                Method::GET,
                format!("{}/service-state", policy_scope(&project, room.as_deref())?),
                None,
            ),
            Self::ScopePause {
                project,
                room,
                paused,
            } => (
                Method::POST,
                format!(
                    "{}/{}",
                    policy_scope(&project, room.as_deref())?,
                    if paused {
                        "pause-service"
                    } else {
                        "clear-service-pause"
                    }
                ),
                None,
            ),
            Self::ProjectPolicy {
                project,
                expected,
                policy,
            } => (
                Method::PUT,
                format!("{}/creation-policy", policy_scope(&project, None)?),
                Some(json!({"expectedRevision":expected,"policy":policy})),
            ),
            Self::RoomPolicy {
                project,
                room,
                expected,
                policy,
            } => (
                Method::PUT,
                format!("{}/creation-policy", policy_scope(&project, Some(&room))?),
                Some(json!({"expectedRevision":expected,"policy":policy})),
            ),
            Self::AdoptProject { space } => {
                matrix_creations::room_id(&space)?;
                (
                    Method::POST,
                    "projects/adopt".into(),
                    Some(json!({"spaceId":space})),
                )
            }
            Self::ProjectRooms { project } => {
                operation_id(&project)?;
                (Method::GET, format!("projects/{project}/rooms"), None)
            }
            Self::AdoptRoom { project, room } => {
                operation_id(&project)?;
                matrix_creations::room_id(&room)?;
                (
                    Method::POST,
                    format!("projects/{project}/rooms/adopt"),
                    Some(json!({"roomId":room})),
                )
            }
            Self::RoomRoster { project, room } => {
                operation_id(&project)?;
                let room = matrix_creations::segment(&room)?;
                (
                    Method::GET,
                    format!("projects/{project}/rooms/{room}/agents"),
                    None,
                )
            }
            Self::Devices => (Method::GET, "devices".into(), None),
            Self::Agent { agent } => {
                operation_id(&agent)?;
                (Method::GET, format!("agents/{agent}"), None)
            }
            Self::AssignExecutionDevice { agent, expected } => {
                operation_id(&agent)?;
                if expected < 0 {
                    return Err(OwnerError::new(400, "invalid_arguments"));
                }
                (
                    Method::PUT,
                    format!("agents/{agent}/execution-device"),
                    Some(json!({"expectedGeneration":expected})),
                )
            }
            Self::AgentOwnerDirect { agent } => {
                operation_id(&agent)?;
                (Method::GET, format!("agents/{agent}/owner-direct"), None)
            }
            Self::OwnerDirect { agent, room } => {
                operation_id(&agent)?;
                matrix_creations::room_id(&room)?;
                (
                    Method::POST,
                    format!("agents/{agent}/owner-direct"),
                    Some(json!({"roomId":room})),
                )
            }
            Self::Bindings { agent } => {
                operation_id(&agent)?;
                (Method::GET, format!("agents/{agent}/bindings"), None)
            }
            Self::Binding { .. }
            | Self::PauseBinding { .. }
            | Self::ResumeBinding { .. }
            | Self::LeaveBinding { .. } => unreachable!("binding operation handled before match"),
            Self::Create { name, command } => {
                operation_id(&command)?;
                (
                    Method::POST,
                    "agents".into(),
                    Some(json!({"displayName":name,"idempotencyKey":command})),
                )
            }
            Self::Bind {
                agent,
                project,
                room,
                command,
            } => {
                operation_id(&agent)?;
                operation_id(&project)?;
                operation_id(&command)?;
                (
                    Method::POST,
                    format!("agents/{agent}/bindings"),
                    Some(json!({"projectId":project,"roomId":room,"idempotencyKey":command})),
                )
            }
            Self::Pause { agent } => {
                operation_id(&agent)?;
                (Method::POST, format!("agents/{agent}/pause"), None)
            }
            Self::Resume { agent } => {
                operation_id(&agent)?;
                (Method::POST, format!("agents/{agent}/resume"), None)
            }
            Self::Retire { agent } => {
                operation_id(&agent)?;
                (Method::DELETE, format!("agents/{agent}"), None)
            }
        };
        Ok((method, format!("/hagency/api/v1/{suffix}"), body))
    }
}
impl ServerLogin {
    pub(super) fn state_directory(&self) -> Option<PathBuf> {
        self.path.as_ref()?.parent().map(Path::to_path_buf)
    }
    pub(super) async fn owner_api(
        &self,
        cookie: &str,
        operation: OwnerOperation,
    ) -> Result<OwnerReply, OwnerError> {
        self.owner_api_inner(Some(cookie), None, operation, None)
            .await
    }
    async fn owner_api_inner(
        &self,
        cookie: Option<&str>,
        pin: Option<&super::device_execution::DeviceIdentity>,
        operation: OwnerOperation,
        creation_scope: Option<&matrix_creations::Scope<'_>>,
    ) -> Result<OwnerReply, OwnerError> {
        let needs_device = matches!(
            &operation,
            OwnerOperation::Create { .. }
                | OwnerOperation::AssignExecutionDevice { .. }
                | OwnerOperation::Management(..)
                | OwnerOperation::Allocation(..)
                | OwnerOperation::ProjectSetup(..)
        );
        let native_allocation = matches!(&operation, OwnerOperation::Allocation(_));
        let (method, path, body) = operation.request()?;
        let shared_protocol = native_allocation
            && path.starts_with("/hagency/api/v1/management/shared-execution/");
        if self.stopped.load(Ordering::Acquire) {
            return Err(OwnerError::new(401, "sign_in_required"));
        }
        let renewal = self.renewal.lock().await;
        let key = {
            let sessions = self.sessions.lock().await;
            if let Some(cookie) = cookie {
                session_key(cookie)
            } else {
                sessions
                    .iter()
                    .find(|(_, s)| {
                        !s.invalidated
                            && s.device.as_ref().is_some_and(|d| {
                                pin == Some(&super::device_execution::DeviceIdentity::from(d))
                            })
                    })
                    .map(|(key, _)| key.clone())
                    .ok_or_else(|| OwnerError::new(401, "sign_in_required"))?
            }
        };
        self.renew_session_inner(&key).await.map_err(|e| {
            OwnerError::new(
                if matches!(e, Error::Unavailable) {
                    503
                } else {
                    401
                },
                if matches!(e, Error::Unavailable) {
                    "server_unavailable"
                } else {
                    "sign_in_required"
                },
            )
        })?;
        let session = self
            .sessions
            .lock()
            .await
            .get(&key)
            .cloned()
            .filter(|s| !s.invalidated && s.expires > Instant::now())
            .ok_or_else(|| OwnerError::new(401, "sign_in_required"))?;
        drop(renewal);
        if let Some(scope) = creation_scope {
            scope.matches(&session)?;
            scope.check().await?;
        }
        if pin.is_some_and(|pin| {
            session
                .device
                .as_ref()
                .is_none_or(|device| pin != &super::device_execution::DeviceIdentity::from(device))
        }) {
            return Err(OwnerError::new(401, "device_authorization_required"));
        }
        let server = origin(&session.binding.origin)
            .map_err(|_| OwnerError::new(401, "sign_in_required"))?;
        let client = http().map_err(|_| OwnerError::new(503, "server_unavailable"))?;
        let mut device = session.device.clone();
        let bearer = if needs_device {
            let device = device
                .as_mut()
                .ok_or_else(|| OwnerError::new(401, "device_authorization_required"))?;
            device.valid_until = session.authorized_until;
            device
                .bearer()
                .map_err(|_| OwnerError::new(401, "device_authorization_required"))?
        } else {
            &session.token
        };
        let mut request = client
            .request(method, server.join(&path).unwrap())
            .bearer_auth(bearer);
        if shared_protocol {
            request = request.header("x-hagency-shared-protocol", "native-context-v3;claude-v4");
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        let mut reply = request
            .send()
            .await
            .map_err(|_| OwnerError::new(503, "server_unavailable"))?;
        let status = reply.status().as_u16();
        let mut raw = Vec::new();
        while let Some(chunk) = reply
            .chunk()
            .await
            .map_err(|_| OwnerError::new(503, "server_unavailable"))?
        {
            if raw.len() + chunk.len() > 128 * 1024 {
                return Err(OwnerError::new(502, "invalid_server_response"));
            }
            raw.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&raw)
            .map_err(|_| OwnerError::new(502, "invalid_server_response"))?;
        if !(200..300).contains(&status) {
            let code = value["code"]
                .as_str()
                .filter(|c| {
                    !c.is_empty()
                        && c.len() <= 80
                        && c.bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                })
                .unwrap_or("server_request_failed");
            return Err(OwnerError::new(status, code));
        }
        if let Some(scope) = creation_scope {
            scope.check().await?;
        }
        let sessions = self.sessions.lock().await;
        // Cookie renewal can move this same grant while management HTTP is in
        // flight. Validate the grant itself; logout still invalidates/removes it.
        let current = sessions.values().find(|s| s.token == session.token);
        if self.stopped.load(Ordering::Acquire)
            || current.is_none_or(|s| s.invalidated || s.expires <= Instant::now())
        {
            return Err(OwnerError::new(401, "sign_in_required"));
        }
        if current.is_none_or(|s| s.authorized_until <= Instant::now()) {
            return Err(OwnerError::new(503, "server_unavailable"));
        }
        Ok(OwnerReply {
            owner: session
                .binding
                .owner
                .clone()
                .ok_or_else(|| OwnerError::new(401, "sign_in_required"))?,
            origin: session.binding.origin.clone(),
            issuer: format!("{}auth/", session.binding.origin),
            subject: session
                .binding
                .subject
                .clone()
                .ok_or_else(|| OwnerError::new(401, "sign_in_required"))?,
            value,
        })
    }
}

impl super::Console {
    /// Background service discovery uses the current authenticated session pinned
    /// to the exact owner/device generation; no browser cookie is retained.
    pub(super) async fn owner_api_pinned(
        &self,
        pin: &super::device_execution::DeviceIdentity,
        operation: OwnerOperation,
    ) -> Result<OwnerReply, OwnerError> {
        let current = self.authorized_device().await.map_err(|e| {
            OwnerError::new(
                if matches!(e, Error::Unavailable) {
                    503
                } else {
                    401
                },
                if matches!(e, Error::Unavailable) {
                    "server_unavailable"
                } else {
                    "device_authorization_required"
                },
            )
        })?;
        current
            .bearer()
            .map_err(|_| OwnerError::new(401, "device_authorization_required"))?;
        if pin != &super::device_execution::DeviceIdentity::from(&current) {
            return Err(OwnerError::new(401, "device_authorization_required"));
        }
        self.0
            .server_login
            .owner_api_inner(None, Some(pin), operation, None)
            .await
    }
    pub(super) async fn owner_api(
        &self,
        cookie: &str,
        operation: OwnerOperation,
    ) -> Result<OwnerReply, OwnerError> {
        self.0.server_login.owner_api(cookie, operation).await
    }
}

#[cfg(test)]
mod private_json_tests {
    use super::*;
    #[test]
    fn restart_reads_bounded_json_larger_than_a_single_token_without_restoring_authority() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let binding = Binding {
            origin: "http://127.0.0.1:13377/".into(),
            client_id: "c".repeat(128),
            installation_id: "i".repeat(64),
            name: "n".repeat(128),
            owner: Some(format!("@{}:example.test", "o".repeat(180))),
            subject: Some("s".repeat(200)),
        };
        let body = serde_json::to_vec(&binding).unwrap();
        assert!(body.len() > 512);
        hagency_store::private::replace(&root.path().join("server-login.json"), &body).unwrap();
        let mut records = vec![Revocation {
            origin: binding.origin.clone(),
            client_id: binding.client_id.clone(),
            session_token: "d".repeat(64),
            oauth_token: "t".repeat(1000),
            hint: "refresh_token".into(),
            session_done: false,
            oauth_done: false,
        }];
        records.push(Revocation {
            origin: binding.origin.clone(),
            client_id: binding.client_id.clone(),
            session_token: String::new(),
            oauth_token: "minted-before-grant".into(),
            hint: "access_token".into(),
            session_done: true,
            oauth_done: false,
        });
        hagency_store::private::replace(
            &root.path().join("server-login-revocations.json"),
            &serde_json::to_vec(&records).unwrap(),
        )
        .unwrap();
        let login = ServerLogin::new(Some(root.path())).unwrap();
        assert!(login.sessions.try_lock().unwrap().is_empty());
        assert_eq!(login.revocations.try_lock().unwrap().len(), 2);
        assert!(read_private_json(&root.path().join("server-login.json"), 512).is_err());
    }
}

#[cfg(test)]
mod scope_policy_request_tests {
    use super::*;
    #[test]
    fn reply_policy_uses_owner_server_binding_path() {
        for enabled in [false, true] {
            let (method, path, body) = OwnerOperation::SetThreadAutoReply {
                binding: "bnd_1".into(),
                enabled,
            }
            .request()
            .unwrap();
            assert_eq!(method, reqwest::Method::PUT);
            assert_eq!(path, "/hagency/api/v1/bindings/bnd_1/reply-policy");
            assert_eq!(body, Some(json!({"threadAutoReply":enabled})));
        }
    }
    #[test]
    fn scope_requests_are_fixed_origin_paths_without_claimed_administrator() {
        let (method, path, body) = OwnerOperation::ScopePause {
            project: "prj_1".into(),
            room: Some("!r:test".into()),
            paused: true,
        }
        .request()
        .unwrap();
        assert_eq!(method, reqwest::Method::POST);
        assert_eq!(
            path,
            "/hagency/api/v1/projects/prj_1/rooms/%21r%3Atest/pause-service"
        );
        assert!(body.is_none());
        assert!(
            OwnerOperation::ScopeState {
                project: "../other".into(),
                room: None
            }
            .request()
            .is_err()
        );
        assert!(
            OwnerOperation::ScopeState {
                project: "prj_1".into(),
                room: Some("!r:test/admin".into())
            }
            .request()
            .is_err()
        );
        let (_, _, body) = OwnerOperation::RoomPolicy {
            project: "prj_1".into(),
            room: "!r:test".into(),
            expected: 9,
            policy: super::super::native::RoomCreationPolicy::Disabled,
        }
        .request()
        .unwrap();
        assert_eq!(
            body.unwrap(),
            json!({"expectedRevision":9,"policy":{"mode":"disabled"}})
        );
    }
}

#[cfg(test)]
mod shared_protocol_tests;

#[cfg(test)]
mod oauth_refresh_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn oauth_http_refresh_failures_preserve_credentials_and_allow_a_valid_retry() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let now = Instant::now();
        let mut session = RemoteSession {
            matrix_source: None,
            token: "owner-session".into(),
            oauth_token: "old-access".into(),
            user_id: "owner".into(),
            device: None,
            refresh_token: Some("retained-refresh".into()),
            oauth_expires: now,
            authorized_until: now,
            invalidated: false,
            binding: Binding {
                origin: origin.clone(),
                client_id: "client".into(),
                installation_id: "installation".into(),
                name: "fixture".into(),
                owner: Some("@owner:test".into()),
                subject: Some("subject".into()),
            },
            checked: now,
            expires: now + Duration::from_secs(900),
        };
        let cases = [
            (503, json!({"error":"server_error"}), false),
            (429, json!({"error":"slow_down"}), false),
            (400, json!({"error":"invalid_grant"}), true),
            (401, json!({"error":"invalid_client"}), true),
            (200, json!({}), false),
        ];
        let mut replies: Vec<_> = cases
            .iter()
            .map(|(status, body, _)| (*status, body.clone()))
            .collect();
        let until = (SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            + 30_000) as u64;
        replies.extend([(200, json!({"access_token":"new-access","refresh_token":"new-refresh","expires_in":900})),
            (200, json!({"validUntilMs":until})),
            (200, json!({"mxid":"@owner:test","subject":"subject","clientId":"client","issuer":format!("{origin}auth/"),"userId":"owner","validUntilMs":until}))]);
        let server = tokio::spawn(async move {
            for (index, (status, body)) in replies.into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut raw = Vec::new();
                loop {
                    let mut chunk = [0u8; 4096];
                    let size = socket.read(&mut chunk).await.unwrap();
                    assert!(size > 0);
                    raw.extend_from_slice(&chunk[..size]);
                    let request = String::from_utf8_lossy(&raw);
                    if let Some((headers, body)) = request.split_once("\r\n\r\n") {
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if body.len() >= length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(raw).unwrap();
                if index <= 5 {
                    assert!(request.starts_with("POST /auth/oauth2/token "));
                    assert!(request.contains("refresh_token=retained-refresh"));
                }
                let body = body.to_string();
                socket.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        for (index, (_, _, terminal)) in cases.into_iter().enumerate() {
            let result = if index == 0 {
                // Standalone OAuth must reach renewal after the old proof
                // expires, rather than turning a temporary outage into logout.
                let root =
                    hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
                let login = ServerLogin::new(Some(root.path())).unwrap();
                login
                    .sessions
                    .lock()
                    .await
                    .insert("expired-proof".into(), session.clone());
                let result = login.authorized_device().await.map(|_| ());
                session = login
                    .sessions
                    .lock()
                    .await
                    .get("expired-proof")
                    .unwrap()
                    .clone();
                result
            } else {
                renew(&mut session).await
            };
            assert!(if terminal {
                matches!(result, Err(Error::Unauthorized))
            } else {
                matches!(result, Err(Error::Unavailable))
            });
            assert_eq!(session.oauth_token, "old-access");
            assert_eq!(session.refresh_token.as_deref(), Some("retained-refresh"));
            assert_eq!(session.authorized_until, now);
            assert!(!session.invalidated);
        }
        renew(&mut session).await.unwrap();
        assert_eq!(session.oauth_token, "new-access");
        assert_eq!(session.refresh_token.as_deref(), Some("new-refresh"));
        assert!(session.authorized_until > now);
        server.await.unwrap();
    }
}
