//! Revoke-only transport for an authenticated, grant-bound recovery record.
//! This never restores an SDK session, refreshes tokens, follows redirects, or
//! discovers a new destination while processing a persisted record.
use super::PersistedUserSession;
use anyhow::{Result, anyhow, ensure};
use matrix_sdk::{
    Client,
    reqwest::{StatusCode, redirect::Policy},
};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use url::Url;

const VERSION: u32 = 1;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);
const BATCH_TIMEOUT: Duration = Duration::from_secs(16);
const MAX_URL: usize = 8192;
const MAX_TOKEN: usize = 1024 * 1024;

/// Its serialized representation belongs inside the original grant's AEAD.
/// Shape validation alone never authenticates a public locator or grants access.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Target {
    version: u32,
    homeserver: Url,
    owner: matrix_sdk::ruma::OwnedUserId,
    device: matrix_sdk::ruma::OwnedDeviceId,
    protocol: Protocol,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "api", rename_all = "snake_case", deny_unknown_fields)]
enum Protocol {
    Matrix,
    // The pinned SDK's native OAuth client has a public client ID and no client
    // secret. Do not silently generalize this to confidential-client protocols.
    #[serde(rename = "oauth_public")]
    OAuthPublic {
        issuer: Url,
        endpoint: Url,
        client_id: String,
    },
}
fn validate_url(url: &Url, oauth: bool) -> Result<()> {
    ensure!(
        url.as_str().len() <= MAX_URL
            && url.host_str().is_some()
            && matches!(url.scheme(), "https" | "http")
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none(),
        "credential_revocation_target_invalid"
    );
    if oauth {
        crate::login::homeserver::validate_oauth_origin(url)
            .map_err(|_| anyhow!("credential_revocation_target_invalid"))?;
    }
    Ok(())
}
impl Target {
    pub(super) fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION,
            "credential_revocation_target_invalid"
        );
        validate_url(&self.homeserver, false)?;
        ensure!(
            self.homeserver.query().is_none(),
            "credential_revocation_target_invalid"
        );
        if let Protocol::OAuthPublic {
            issuer,
            endpoint,
            client_id,
        } = &self.protocol
        {
            validate_url(issuer, true)?;
            validate_url(endpoint, true)?;
            ensure!(
                !client_id.is_empty() && client_id.len() <= 255,
                "credential_revocation_target_invalid"
            );
        }
        Ok(())
    }
    pub(super) fn validate_session(&self, session: &PersistedUserSession) -> Result<()> {
        self.validate()?;
        ensure!(
            self.owner == session.meta().user_id && self.device == session.meta().device_id,
            "credential_revocation_identity_changed"
        );
        match (&self.protocol, session) {
            (Protocol::Matrix, PersistedUserSession::Matrix(_)) => (),
            (
                Protocol::OAuthPublic { client_id, .. },
                PersistedUserSession::OAuth {
                    oauth_client_id, ..
                },
            ) if client_id == oauth_client_id => (),
            _ => return Err(anyhow!("credential_revocation_identity_changed")),
        }
        let (access, refresh) = tokens(session);
        ensure!(
            !access.is_empty()
                && access.len() <= MAX_TOKEN
                && refresh.is_none_or(|token| !token.is_empty() && token.len() <= MAX_TOKEN),
            "credential_revocation_tokens_invalid"
        );
        Ok(())
    }
    pub(super) fn matches_homeserver(&self, homeserver: &str) -> bool {
        Url::parse(homeserver).is_ok_and(|url| url == self.homeserver)
    }
    fn matrix_logout_url(&self) -> Result<Url> {
        let mut url = self.homeserver.clone();
        url.path_segments_mut()
            .map_err(|_| anyhow!("credential_revocation_target_invalid"))?
            .pop_if_empty()
            .extend(["_matrix", "client", "v3", "logout"]);
        Ok(url)
    }
}

/// Capture while the original SDK is authenticated. Persist this in the grant's
/// authenticated envelope, so offline logout does not need fresh discovery.
pub(super) async fn capture_target(client: &Client) -> Result<Target> {
    let session = PersistedUserSession::from_sdk(
        client
            .session()
            .ok_or_else(|| anyhow!("credential_revocation_session_missing"))?,
    )?;
    let protocol = match &session {
        PersistedUserSession::Matrix(_) => Protocol::Matrix,
        PersistedUserSession::OAuth {
            oauth_client_id, ..
        } => {
            let metadata =
                tokio::time::timeout(REQUEST_TIMEOUT, client.oauth().cached_server_metadata())
                    .await
                    .map_err(|_| anyhow!("credential_revocation_metadata_unavailable"))?
                    .map_err(|_| anyhow!("credential_revocation_metadata_unavailable"))?;
            Protocol::OAuthPublic {
                issuer: metadata.issuer,
                endpoint: metadata.revocation_endpoint,
                client_id: oauth_client_id.clone(),
            }
        }
    };
    let target = Target {
        version: VERSION,
        homeserver: client.homeserver(),
        owner: session.meta().user_id.clone(),
        device: session.meta().device_id.clone(),
        protocol,
    };
    target.validate_session(&session)?;
    Ok(target)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum FailureCode {
    Transport,
    Timeout,
    HttpStatus,
    MatrixRefreshUnconfirmed,
}

/// Receipts are specific to the actual token bytes, not just account identity.
/// A new late refresh pair cannot inherit a previous pair's success flags.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Progress {
    pub access_confirmed: bool,
    pub refresh_confirmed: bool,
    pub complete: bool,
    pub failure_code: Option<FailureCode>,
    access_digest: Option<String>,
    refresh_digest: Option<String>,
}
fn digest(value: &str) -> String {
    blake3::hash(value.as_bytes()).to_hex().to_string()
}
fn tokens(session: &PersistedUserSession) -> (&str, Option<&str>) {
    let value = match session {
        PersistedUserSession::Matrix(value) => &value.tokens,
        PersistedUserSession::OAuth { user, .. } => &user.tokens,
    };
    (&value.access_token, value.refresh_token.as_deref())
}
impl Progress {
    fn for_tokens(mut self, session: &PersistedUserSession) -> Self {
        let (access, refresh) = tokens(session);
        let access_digest = Some(digest(access));
        let refresh_digest = refresh.map(digest);
        self.access_confirmed &= self.access_digest == access_digest;
        self.refresh_confirmed =
            refresh.is_none() || self.refresh_confirmed && self.refresh_digest == refresh_digest;
        self.access_digest = access_digest;
        self.refresh_digest = refresh_digest;
        self.complete = self.access_confirmed && self.refresh_confirmed;
        self.failure_code = None;
        self
    }
    /// Use this, and Target::validate_session, when reading a completed record.
    pub(super) fn confirmed_for(&self, session: &PersistedUserSession) -> bool {
        let (access, refresh) = tokens(session);
        self.complete
            && self.access_confirmed
            && self.refresh_confirmed
            && self.access_digest.as_deref() == Some(digest(access).as_str())
            && self.refresh_digest == refresh.map(digest)
    }
}

/// Validation errors occur before any request. Network failures return an Ok
/// partial receipt, which must be durably committed against the original file
/// identity/digest. Only complete+confirmed_for permits remote-success reporting.
/// A timeout never proves that the remote operation did not take effect.
pub(super) async fn revoke(
    target: &Target,
    session: &PersistedUserSession,
    prior: Progress,
) -> Result<Progress> {
    target.validate_session(session)?;
    let mut progress = prior.for_tokens(session);
    if progress.complete {
        return Ok(progress);
    }
    let client = crate::http::client_builder()
        .redirect(Policy::none())
        .connect_timeout(Duration::from_secs(4))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| anyhow!("credential_revocation_transport_unavailable"))?;
    let (access, refresh) = tokens(session);
    let result = tokio::time::timeout(BATCH_TIMEOUT, async {
        match &target.protocol {
            Protocol::OAuthPublic {
                endpoint,
                client_id,
                ..
            } => {
                for (token, hint, confirmed) in [
                    (Some(access), "access_token", &mut progress.access_confirmed),
                    (refresh, "refresh_token", &mut progress.refresh_confirmed),
                ] {
                    if *confirmed {
                        continue;
                    }
                    let Some(token) = token else {
                        *confirmed = true;
                        continue;
                    };
                    let encoded = url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs([
                            ("client_id", client_id.as_str()),
                            ("token", token),
                            ("token_type_hint", hint),
                        ])
                        .finish();
                    match client
                        .post(endpoint.clone())
                        .header("content-type", "application/x-www-form-urlencoded")
                        .body(encoded)
                        .send()
                        .await
                    {
                        Ok(response) if response.status() == StatusCode::OK => *confirmed = true,
                        Ok(_) => progress.failure_code = Some(FailureCode::HttpStatus),
                        Err(error) => {
                            progress.failure_code = Some(if error.is_timeout() {
                                FailureCode::Timeout
                            } else {
                                FailureCode::Transport
                            })
                        }
                    }
                }
            }
            Protocol::Matrix => {
                let response = client
                    .post(target.matrix_logout_url()?)
                    .bearer_auth(access)
                    .send()
                    .await;
                match response {
                    Ok(response) if response.status() == StatusCode::OK => {
                        progress.access_confirmed = true;
                        progress.refresh_confirmed = true;
                    }
                    Ok(response) if response.status() == StatusCode::UNAUTHORIZED => {
                        if unknown_matrix_token(response).await {
                            progress.access_confirmed = true;
                            // An expired access token can still have a usable
                            // refresh token. Never rotate it in a revoke-only job.
                            progress.refresh_confirmed = refresh.is_none();
                            if refresh.is_some() {
                                progress.failure_code = Some(FailureCode::MatrixRefreshUnconfirmed);
                            }
                        } else {
                            progress.failure_code = Some(FailureCode::HttpStatus);
                        }
                    }
                    Ok(_) => progress.failure_code = Some(FailureCode::HttpStatus),
                    Err(error) => {
                        progress.failure_code = Some(if error.is_timeout() {
                            FailureCode::Timeout
                        } else {
                            FailureCode::Transport
                        })
                    }
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    })
    .await;
    match result {
        Ok(result) => result?,
        Err(_) => progress.failure_code = Some(FailureCode::Timeout),
    }
    progress.complete = progress.access_confirmed && progress.refresh_confirmed;
    if progress.complete {
        progress.failure_code = None;
    }
    Ok(progress)
}

async fn unknown_matrix_token(mut response: matrix_sdk::reqwest::Response) -> bool {
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) if bytes.len().saturating_add(chunk.len()) <= 8192 => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(None) => break,
            _ => return false,
        }
    }
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .is_ok_and(|v| v["errcode"] == "M_UNKNOWN_TOKEN")
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::{
        SessionMeta, SessionTokens,
        authentication::{matrix::MatrixSession, oauth::UserSession},
    };
    use wiremock::{
        Mock, MockServer, ResponseTemplate,
        matchers::{method, path, body_string, header},
    };

    fn session(oauth: bool, access: &str, refresh: Option<&str>) -> PersistedUserSession {
        let meta = SessionMeta {
            user_id: "@revoke:example.test".try_into().unwrap(),
            device_id: "REVOKE".into(),
        };
        let tokens = SessionTokens {
            access_token: access.into(),
            refresh_token: refresh.map(Into::into),
        };
        if oauth {
            PersistedUserSession::OAuth {
                oauth_client_id: "public-client".into(),
                user: UserSession { meta, tokens },
            }
        } else {
            PersistedUserSession::Matrix(MatrixSession { meta, tokens })
        }
    }
    fn target(server: &MockServer, oauth: bool) -> Target {
        let origin = Url::parse(&server.uri()).unwrap();
        Target {
            version: VERSION,
            homeserver: origin.clone(),
            owner: "@revoke:example.test".try_into().unwrap(),
            device: "REVOKE".into(),
            protocol: if oauth {
                Protocol::OAuthPublic {
                    issuer: origin.clone(),
                    endpoint: origin.join("revoke").unwrap(),
                    client_id: "public-client".into(),
                }
            } else {
                Protocol::Matrix
            },
        }
    }
    fn form(token: &str, hint: &str) -> String {
        url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("client_id", "public-client"),
                ("token", token),
                ("token_type_hint", hint),
            ])
            .finish()
    }
    async fn oauth_reply(server: &MockServer, token: &str, hint: &str, status: u16, count: u64) {
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .and(body_string(form(token, hint)))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .respond_with(ResponseTemplate::new(status))
            .expect(count)
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn credential_migration_revoke_transport_confirms_each_oauth_token_and_binds_receipts() {
        let server = MockServer::start().await;
        let target = target(&server, true);
        let original = session(true, "access+&=", Some("refresh+&="));
        oauth_reply(&server, "access+&=", "access_token", 200, 1).await;
        oauth_reply(&server, "refresh+&=", "refresh_token", 200, 1).await;
        let done = revoke(&target, &original, Progress::default())
            .await
            .unwrap();
        assert!(done.complete && done.confirmed_for(&original));
        assert!(
            revoke(&target, &original, done.clone())
                .await
                .unwrap()
                .complete
        );
        let roundtrip: Progress =
            serde_json::from_slice(&serde_json::to_vec(&done).unwrap()).unwrap();
        assert!(roundtrip.confirmed_for(&original));
        let late = session(true, "late-access", Some("late-refresh"));
        assert!(!done.confirmed_for(&late));
        oauth_reply(&server, "late-access", "access_token", 200, 1).await;
        oauth_reply(&server, "late-refresh", "refresh_token", 200, 1).await;
        assert!(
            revoke(&target, &late, done)
                .await
                .unwrap()
                .confirmed_for(&late)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 4);
    }

    #[tokio::test]
    async fn credential_migration_revoke_transport_keeps_partial_receipt_and_retries_only_unconfirmed_token()
     {
        let server = MockServer::start().await;
        let target = target(&server, true);
        let original = session(true, "access", Some("refresh"));
        oauth_reply(&server, "access", "access_token", 200, 1).await;
        oauth_reply(&server, "refresh", "refresh_token", 503, 1).await;
        let partial = revoke(&target, &original, Progress::default())
            .await
            .unwrap();
        assert!(partial.access_confirmed && !partial.refresh_confirmed && !partial.complete);
        assert_eq!(partial.failure_code, Some(FailureCode::HttpStatus));
        server.verify().await;
        server.reset().await;
        oauth_reply(&server, "refresh", "refresh_token", 200, 1).await;
        assert!(
            revoke(&target, &original, partial)
                .await
                .unwrap()
                .confirmed_for(&original)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn credential_migration_revoke_transport_timeout_keeps_confirmed_access_and_unknown_refresh()
     {
        let server = MockServer::start().await;
        let target = target(&server, true);
        let original = session(true, "access", Some("refresh"));
        oauth_reply(&server, "access", "access_token", 200, 1).await;
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .and(body_string(form("refresh", "refresh_token")))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(12)))
            .expect(1)
            .mount(&server)
            .await;
        let pending = revoke(&target, &original, Progress::default())
            .await
            .unwrap();
        assert!(pending.access_confirmed && !pending.refresh_confirmed && !pending.complete);
        assert_eq!(pending.failure_code, Some(FailureCode::Timeout));
        // The request reached the actual server. Timeout is unknown effect,
        // never evidence that this token was not submitted or was revoked.
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn credential_migration_revoke_transport_never_redirects_credentials_or_accepts_identity_swaps()
     {
        let sink = MockServer::start().await;
        let server = MockServer::start().await;
        let target = target(&server, true);
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", sink.uri()))
            .expect(2)
            .mount(&server)
            .await;
        let original = session(true, "access", Some("refresh"));
        assert!(
            !revoke(&target, &original, Progress::default())
                .await
                .unwrap()
                .complete
        );
        assert!(sink.received_requests().await.unwrap().is_empty());
        let mut swapped = session(true, "access", Some("refresh"));
        if let PersistedUserSession::OAuth {
            oauth_client_id, ..
        } = &mut swapped
        {
            *oauth_client_id = "different-client".into();
        }
        assert!(
            revoke(&target, &swapped, Progress::default())
                .await
                .is_err()
        );
        let mut swapped = target.clone();
        swapped.device = "OTHER".into();
        assert!(
            revoke(&swapped, &original, Progress::default())
                .await
                .is_err()
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn credential_migration_revoke_transport_preserves_matrix_base_path_and_does_not_refresh_expired_tokens()
     {
        let server = MockServer::start().await;
        let mut target = target(&server, false);
        target.homeserver = Url::parse(&format!("{}/matrix/", server.uri())).unwrap();
        assert!(target.matches_homeserver(&format!("{}/matrix/", server.uri())));
        Mock::given(method("POST"))
            .and(path("/matrix/_matrix/client/v3/logout"))
            .and(header("authorization", "Bearer access"))
            .respond_with(
                ResponseTemplate::new(401).set_body_json(
                    serde_json::json!({"errcode":"M_UNKNOWN_TOKEN","soft_logout":true}),
                ),
            )
            .expect(1)
            .mount(&server)
            .await;
        let original = session(false, "access", Some("refresh"));
        let pending = revoke(&target, &original, Progress::default())
            .await
            .unwrap();
        assert!(pending.access_confirmed && !pending.refresh_confirmed && !pending.complete);
        assert_eq!(
            pending.failure_code,
            Some(FailureCode::MatrixRefreshUnconfirmed)
        );
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        server.verify().await;
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/matrix/_matrix/client/v3/logout"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .expect(1)
            .mount(&server)
            .await;
        assert!(
            revoke(&target, &original, pending)
                .await
                .unwrap()
                .confirmed_for(&original)
        );
    }

    #[tokio::test]
    async fn credential_migration_revoke_transport_rejects_oversize_unknown_response_and_captures_real_sdk_identity()
     {
        let server = MockServer::start().await;
        let target = target(&server, false);
        let original = session(false, "access", None);
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/logout"))
            .respond_with(ResponseTemplate::new(401).set_body_string(format!(
                "{{\"errcode\":\"M_UNKNOWN_TOKEN\"}}{}",
                " ".repeat(8192)
            )))
            .expect(1)
            .mount(&server)
            .await;
        let pending = revoke(&target, &original, Progress::default())
            .await
            .unwrap();
        assert!(!pending.access_confirmed && !pending.complete);
        let client = Client::builder()
            .homeserver_url(server.uri())
            .server_versions([matrix_sdk::ruma::api::MatrixVersion::V1_0])
            .build()
            .await
            .unwrap();
        client
            .restore_session(original.clone().into_sdk())
            .await
            .unwrap();
        let captured = capture_target(&client).await.unwrap();
        captured.validate_session(&original).unwrap();
        assert!(captured.matches_homeserver(&server.uri()));
        let restored: Target =
            serde_json::from_slice(&serde_json::to_vec(&captured).unwrap()).unwrap();
        restored.validate_session(&original).unwrap();
    }
}
