//! Rust-only SDK authorization. OAuth snapshots never enter local persistence.
use super::super::native::MatrixTokenSource;
use super::*;
use std::sync::Arc;
impl ServerLogin {
    pub(in crate::console) async fn authorize_matrix(
        &self,
        console: &super::super::Console,
        expected_origin: &str,
        expected_owner: &str,
        source: Arc<dyn MatrixTokenSource>,
    ) -> Result<String, &'static str> {
        let _transition = self.transition.lock().await;
        if self.stopped.load(Ordering::Acquire) {
            return Err("matrix_account_changed");
        }
        let started = Instant::now();
        tracing::debug!(
            stage = "owner_adoption_discovery",
            "Adopting SDK authorization"
        );
        crate::server_admission::discover(expected_origin)
            .await
            .map_err(|e| e.code())?;
        let snapshot = source
            .access_token()
            .await
            .map_err(super::super::native::sdk_source_failure)?;
        if snapshot.access_token.is_empty()
            || snapshot.access_token.len() > 4096
            || snapshot.client_id.is_empty()
            || snapshot.client_id.len() > 255
        {
            return Err("invalid_sdk_token");
        }
        let server = origin(expected_origin)?;
        let client = http()?;
        let (replaces, explicit) = match self
            .try_sdk_resume(
                console,
                expected_origin,
                expected_owner,
                source.clone(),
                &snapshot.client_id,
            )
            .await?
        {
            sdk_resume::Resume::Restored(cookie) => return Ok(cookie),
            sdk_resume::Resume::Register { replaces, explicit } => (replaces, explicit),
        };
        tracing::debug!(
            stage = "owner_adoption_grant",
            elapsed_ms = started.elapsed().as_millis() as u64,
            "Requesting owner session"
        );
        let grant = response(
            client
                .post(server.join("/hagency/api/v1/sessions/pasion").unwrap())
                .json(&json!({"accessToken":snapshot.access_token}))
                .send()
                .await
                .map_err(|error| {
                    tracing::warn!(
                        stage = "owner_adoption_grant",
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        timeout = error.is_timeout(),
                        connect = error.is_connect(),
                        body = error.is_body(),
                        "Owner session request transport failed"
                    );
                    "server_unavailable"
                })?,
        )
        .await?;
        let token = grant["token"]
            .as_str()
            .filter(|s| s.len() == 64)
            .ok_or("invalid_server_response")?
            .to_owned();
        let result = async {
            tracing::debug!(stage="owner_adoption_identity",elapsed_ms=started.elapsed().as_millis() as u64,"Checking owner identity");
            let identity = get(&client, server.join("/hagency/api/v1/identity").unwrap(), Some(&token)).await?;
            let subject = identity["subject"].as_str().filter(|s| !s.is_empty() && s.len() <= 255).ok_or("invalid_server_response")?;
            let user = identity["userId"].as_str().filter(|s| !s.is_empty() && s.len() <= 128).ok_or("invalid_server_response")?;
            if identity["mxid"] != expected_owner || identity["issuer"] != server.join("/auth/").unwrap().as_str() || identity["clientId"] != snapshot.client_id || grant["mxid"] != identity["mxid"] || grant["userId"] != identity["userId"] { return Err("matrix_account_mismatch"); }
            let previous = self.binding.lock().await.clone();
            if let Some(previous) = &previous && previous.origin == expected_origin && previous.owner.as_deref() == Some(expected_owner) && previous.subject.as_deref().is_some_and(|s| s != subject) { return Err("matrix_account_mismatch"); }
            let binding = Binding { origin: expected_origin.into(), client_id: snapshot.client_id.clone(), installation_id: previous.as_ref().filter(|p| p.origin == expected_origin && p.client_id == snapshot.client_id && p.owner.as_deref() == Some(expected_owner) && p.subject.as_deref() == Some(subject)).map(|p| p.installation_id.clone()).unwrap_or(random()?), name: "Hagency Desktop".into(), owner: Some(expected_owner.into()), subject: Some(subject.into()) };
            let until = deadline(&grant)?.min(deadline(&identity)?);
            tracing::debug!(stage="owner_adoption_device",elapsed_ms=started.elapsed().as_millis() as u64,"Registering execution device");
            if explicit && (!source.permits_explicit_reenrollment() || !source.consume_explicit_reenrollment()) { return Err("matrix_account_changed"); }
            self.sdk_resume.registering(&binding, user, &token, &self.stopped, replaces.as_deref())?;
            let (_, device) = register_device(&client, &server, &token, &binding, user).await
                .map_err(|_| "native_device_registration_unknown")?;
            self.sdk_resume.registered(&token, &device, &self.stopped)?;
            // A fresh owner needs no replacement permission, but any OAuth
            // provenance supplied for this successful registration is spent.
            // It cannot authorize a later replacement in the same Service.
            if !explicit { source.consume_explicit_reenrollment(); }
            let after = source.access_token().await.map_err(super::super::native::sdk_source_failure)?;
            if after.client_id != binding.client_id || self.stopped.load(Ordering::Acquire) { return Err("matrix_account_changed"); }
            self.save_matrix_binding(binding.clone(), &source).await?;
            let cookie = console.0.authority.server_session(Duration::from_secs(900)).map_err(|_| "local_state_unavailable")?;
            let mut sessions = self.sessions.lock().await;
            if self.stopped.load(Ordering::Acquire) { return Err("matrix_account_changed"); }
            for previous in sessions.values_mut() { self.queue_revocation(previous).await; }
            sessions.clear();
            sessions.insert(session_key(&cookie), RemoteSession { matrix_source: Some(source), token: token.clone(), oauth_token: String::new(), user_id: user.into(), device: Some(device), refresh_token: None, oauth_expires: until, authorized_until: until, invalidated: false, binding, checked: Instant::now(), expires: Instant::now() + Duration::from_secs(900) });
            *self.status.lock().await = json!({"state":"device_authorized","deviceAuthorized":true,"transportOnline":false});
            Self::start_worker(console);
            tracing::info!(stage="owner_adoption",elapsed_ms=started.elapsed().as_millis() as u64,"SDK owner authorization ready");
            Ok(cookie)
        }.await;
        if result
            .as_ref()
            .is_err_and(|code| !Self::preserves_sdk_resume(code) || !self.sdk_resume.owns(&token))
        {
            let _ = self.sdk_resume.revoke(Some(&token));
            // The SDK owns OAuth even when adoption fails. Only our issued grant
            // is revoked; neither the snapshot nor an SDK refresh token persists.
            let mut pending = self.revocations.lock().await;
            pending.push(Revocation {
                origin: expected_origin.into(),
                client_id: snapshot.client_id,
                session_token: token,
                oauth_token: String::new(),
                hint: "access_token".into(),
                session_done: false,
                oauth_done: true,
            });
            let _ = self.save_revocations(&pending);
            drop(pending);
            self.flush_revocations().await;
        }
        result
    }
    pub(in crate::console) async fn check_matrix_source(
        &self,
        cookie: &str,
    ) -> Result<(), &'static str> {
        if self.stopped.load(Ordering::Acquire) {
            return Err("matrix_account_changed");
        }
        let session = self
            .sessions
            .lock()
            .await
            .get(&session_key(cookie))
            .cloned()
            .filter(|s| !s.invalidated)
            .ok_or("matrix_account_changed")?;
        let source = session
            .matrix_source
            .as_ref()
            .ok_or("matrix_account_changed")?;
        let snapshot = source
            .access_token()
            .await
            .map_err(super::super::native::sdk_source_failure)?;
        let sessions = self.sessions.lock().await;
        if sessions
            .values()
            .find(|s| s.token == session.token)
            .is_none_or(|s| s.invalidated)
            || snapshot.client_id != session.binding.client_id
            || self.stopped.load(Ordering::Acquire)
        {
            return Err("matrix_account_changed");
        }
        Ok(())
    }
    pub(in crate::console) async fn renew_matrix(
        &self,
        console: &super::super::Console,
        cookie: &str,
    ) -> Result<Option<String>, Error> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(Error::Unauthorized);
        }
        let _renewal = self.renewal.lock().await;
        let key = session_key(cookie);
        self.renew_session_inner(&key).await?;
        let mut sessions = self.sessions.lock().await;
        let session = sessions.get_mut(&key).ok_or(Error::Unauthorized)?;
        if session.invalidated
            || session.matrix_source.is_none()
            || self.stopped.load(Ordering::Acquire)
        {
            return Err(Error::Unauthorized);
        }
        if session.expires <= Instant::now() + Duration::from_secs(60) {
            let next = console
                .0
                .authority
                .server_session(Duration::from_secs(900))?;
            let mut session = sessions.remove(&key).ok_or(Error::Unauthorized)?;
            session.expires = Instant::now() + Duration::from_secs(900);
            sessions.insert(session_key(&next), session);
            Ok(Some(next))
        } else {
            Ok(None)
        }
    }
    /// Test hook: the next renewal asks the server again instead of reusing a
    /// proof it checked within the last five seconds.
    #[cfg(test)]
    pub(in crate::console) async fn forget_recent_check(&self) {
        for session in self.sessions.lock().await.values_mut() {
            session.checked = Instant::now()
                .checked_sub(Duration::from_secs(6))
                .expect("monotonic clock older than six seconds");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::native::{Command, MatrixAccessToken, NativeError, NativeOwner};
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    include!("sdk_resume_tests.rs");
    struct Source {
        token: std::sync::Mutex<String>,
        current: Arc<AtomicBool>,
        error_code: &'static str,
        calls: std::sync::atomic::AtomicUsize,
        refreshes: std::sync::atomic::AtomicUsize,
    }
    impl MatrixTokenSource for Source {
        fn access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::Relaxed);
                if !self.current.load(Ordering::Acquire) {
                    return Err(NativeError {
                        status: 401,
                        code: self.error_code.into(),
                    });
                }
                Ok(MatrixAccessToken {
                    access_token: self.token.lock().unwrap().clone(),
                    client_id: "sdk-client".into(),
                })
            })
        }
        fn refresh_access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            self.refreshes.fetch_add(1, Ordering::Relaxed);
            {
                let mut token = self.token.lock().unwrap();
                if token.as_str() == "sdk-short" {
                    *token = "sdk-refreshed".into();
                }
            }
            self.access_token()
        }
    }
    async fn fixture(
        mxid: &str,
        late_epoch: Option<Arc<AtomicBool>>,
    ) -> (
        String,
        Arc<std::sync::Mutex<Vec<(String, Value)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        fixture_controlled(
            mxid,
            late_epoch,
            Arc::new(std::sync::atomic::AtomicU16::new(0)),
        )
        .await
    }
    #[derive(Default)]
    struct ProjectResponseGate {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    // 1 = slow response, 2 = renewal 503, 3 = renewal 401, 4 = lost central visibility.
    async fn fixture_controlled(
        mxid: &str,
        late_epoch: Option<Arc<AtomicBool>>,
        behavior: Arc<std::sync::atomic::AtomicU16>,
    ) -> (
        String,
        Arc<std::sync::Mutex<Vec<(String, Value)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        fixture_with_project_gate(mxid, late_epoch, behavior, None).await
    }

    async fn fixture_with_project_gate(
        mxid: &str,
        late_epoch: Option<Arc<AtomicBool>>,
        behavior: Arc<std::sync::atomic::AtomicU16>,
        project_gate: Option<Arc<ProjectResponseGate>>,
    ) -> (
        String,
        Arc<std::sync::Mutex<Vec<(String, Value)>>>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}/", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let records = requests.clone();
        let issuer = format!("{origin}auth/");
        let mxid = mxid.to_owned();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let records = records.clone();
                let issuer = issuer.clone();
                let mxid = mxid.clone();
                let late_epoch = late_epoch.clone();
                let behavior = behavior.clone();
                let project_gate = project_gate.clone();
                tokio::spawn(async move {
                    let mut raw = Vec::new();
                    let mut chunk = [0; 4096];
                    let split = loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&chunk[..n]);
                        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                    };
                    let headers = String::from_utf8_lossy(&raw[..split]).to_string();
                    let length = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    while raw.len() < split + length {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        raw.extend_from_slice(&chunk[..n]);
                    }
                    let path = headers
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .to_owned();
                    let mut body: Value =
                        serde_json::from_slice(&raw[split..split + length]).unwrap_or(Value::Null);
                    if path.starts_with("/_matrix/") {
                        let bearer = headers
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                                    .map(|(_, value)| value.trim().to_owned())
                            })
                            .unwrap_or_default();
                        body = json!({"testBearer":bearer});
                    }
                    if path.starts_with("/hagency/api/v1/management/") {
                        let device_grant = headers.lines().any(|line| {
                            line.split_once(':').is_some_and(|(key, value)| {
                                key.eq_ignore_ascii_case("authorization")
                                    && value.trim() == format!("Bearer {}", "b".repeat(64))
                            })
                        });
                        body["fixtureDeviceGrant"] = json!(device_grant);
                    }
                    let short_proof = body["accessToken"] == "sdk-short";
                    records.lock().unwrap().push((path.clone(), body));
                    if path == "/hagency/api/v1/devices" && behavior.load(Ordering::Acquire) == 8 {
                        return; // The device registration committed, but its reply was lost.
                    }
                    if path == "/hagency/api/v1/identity"
                        && let Some(flag) = &late_epoch
                    {
                        flag.store(false, Ordering::Release);
                    }
                    let until = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64
                        + 30_000;
                    let value = match path.as_str() {
                        "/hagency/api/v1/discovery" => {
                            json!({"product":"hagency-server","version":"0.1.0","protocolVersion":3,"capabilities":["pasion-oauth","owner-agent-appservice-v1","global-agent-identity-v2","execution-device-v1","owner-direct-v1"],"homeserver":issuer.strip_suffix("auth/").unwrap(),"issuer":issuer})
                        }
                        "/hagency/api/v1/sessions/pasion" => {
                            json!({"token":"a".repeat(64),"userId":"uid","mxid":mxid,"validUntilMs":until})
                        }
                        "/hagency/api/v1/identity" => {
                            json!({"userId":"uid","mxid":mxid,"issuer":issuer,"subject":"sub","clientId":"sdk-client","validUntilMs":until})
                        }
                        "/hagency/api/v1/devices" => {
                            json!({"deviceId":"device","token":"b".repeat(64),"generation":1,"validUntilMs":until})
                        }
                        "/hagency/api/v1/devices/current" => {
                            json!({"userId":"uid","mxid":mxid,"issuer":issuer,"subject":"sub","clientId":"sdk-client","deviceId":"device","generation":if behavior.load(Ordering::Acquire)==5 {2}else{1},"validUntilMs":if behavior.load(Ordering::Acquire)==12 {until+90_000}else{until}})
                        }
                        "/hagency/api/v1/sessions/current/renew" => {
                            json!({"validUntilMs":if short_proof{until-25_000}else{until}})
                        }
                        "/hagency/api/v1/sessions/current" => json!({}),
                        "/hagency/api/v1/management/session" => {
                            json!({"protocol":"hagency-management-v1","account":mxid,"device":"device","deviceGeneration":1,"canAdmitResources":false})
                        }
                        "/hagency/api/v1/management/inbox" => {
                            json!({"protocol":"hagency-management-v1","items":[],"nextAfter":null})
                        }
                        "/hagency/api/v1/management/requests/req_1/decide"
                        | "/hagency/api/v1/management/contributions/con_1" => {
                            json!({"admission":{"contribution":{"id":"con_1","state":"admitted"}}})
                        }
                        "/hagency/api/v1/management/projects"
                        | "/hagency/api/v1/management/project-requests/project_req_1/decide" => {
                            json!({"project":{"id":"project_1","state":"approved","allocationStatus":"awaiting_allocation"}})
                        }
                        "/hagency/api/v1/management/projects/project_1" => {
                            json!({"project":{"id":"project_1","state":"revoked","allocationStatus":"none"}})
                        }
                        "/hagency/api/v1/management/project-commands/project-once/recover" => {
                            json!({"recorded":true,"project":{"id":"project_1","state":"revoked","allocationStatus":"none"}})
                        }
                        "/hagency/api/v1/projects" => json!({"projects":[]}),
                        "/_matrix/client/v3/joined_rooms" => json!({"joined_rooms":[]}),
                        _ => json!({"code":"unexpected_request"}),
                    };
                    let mode = behavior.load(Ordering::Acquire);
                    if path.ends_with("/projects") && mode == 1
                        || path.ends_with("/devices/current") && mode == 10
                        || path.ends_with("/devices") && mode == 11
                    {
                        if let Some(gate) = project_gate {
                            gate.entered.notify_one();
                            gate.release.notified().await;
                        } else {
                            tokio::time::sleep(Duration::from_secs(1)).await;
                        }
                    }
                    let status = if path.ends_with("/devices/current") && mode == 6 {
                        503
                    } else if path.ends_with("/devices/current") && mode == 7 {
                        404
                    } else if path.ends_with("/devices/current") && mode == 9 {
                        401
                    } else if path.ends_with("/renew") && mode == 2 {
                        503
                    } else if path.ends_with("/renew") && mode == 3 {
                        401
                    } else if (path.ends_with("/contributions/con_1")
                        || path.ends_with("/projects/project_1")
                        || path.ends_with("/project-commands/project-once/recover"))
                        && mode == 4
                    {
                        403
                    } else {
                        200
                    };
                    let value = if status == 200 {
                        value
                    } else {
                        json!({"code":if status==401 {"sign_in_required"} else {"server_unavailable"}})
                    };
                    let body = value.to_string();
                    let response = format!(
                        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (origin, requests, task)
    }
    #[tokio::test]
    async fn shared_runtime_proof_is_bounded_and_expiry_always_forces_fresh_authorization() {
        let (origin, requests, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source.clone(),
        )
        .await
        .unwrap();
        let login = &native.test_console().0.server_login;
        source.calls.store(0, Ordering::Relaxed);
        for _ in 0..25 {
            login.authorized_device().await.unwrap();
        }
        assert_eq!(
            source.calls.load(Ordering::Relaxed),
            0,
            "finite fresh proof prevents poll amplification"
        );
        for session in login.sessions.lock().await.values_mut() {
            session.checked = Instant::now() - Duration::from_secs(6);
        }
        login.authorized_device().await.unwrap();
        assert_eq!(source.calls.load(Ordering::Relaxed), 2);
        for session in login.sessions.lock().await.values_mut() {
            session.checked = Instant::now();
            session.authorized_until = Instant::now() + Duration::from_secs(1);
            session.oauth_expires = session.authorized_until;
        }
        login.authorized_device().await.unwrap();
        assert_eq!(
            source.refreshes.load(Ordering::Relaxed),
            0,
            "fresh thirty-second server proof must not rotate long-lived OAuth grant"
        );
        assert_eq!(
            source.calls.load(Ordering::Relaxed),
            4,
            "near-expiry proof cannot use cache"
        );
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/renew"))
                .count(),
            2
        );
        source.current.store(false, Ordering::Release);
        for session in login.sessions.lock().await.values_mut() {
            session.checked = Instant::now() - Duration::from_secs(6);
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(1), login.authorized_device())
                .await
                .unwrap()
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_secs(1), login.authorized_device())
                .await
                .unwrap()
                .is_err(),
            "invalidated proof stays closed and lock drains"
        );
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn temporary_renewal_preserves_session_without_extending_expired_proof() {
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let (origin, requests, task) = fixture_controlled("@owner:test", None, mode.clone()).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let login = &native.test_console().0.server_login;
        let old = login.authorized_device().await.unwrap();
        mode.store(2, Ordering::Release);
        for s in login.sessions.lock().await.values_mut() {
            s.checked = Instant::now() - Duration::from_secs(6);
        }
        assert!(matches!(
            login.authorized_device().await,
            Err(Error::Unavailable)
        ));
        assert!(old.bearer().is_ok());
        assert!(login.revocations.lock().await.is_empty());
        for s in login.sessions.lock().await.values_mut() {
            s.authorized_until = Instant::now() - Duration::from_secs(1);
            s.oauth_expires = s.authorized_until;
        }
        assert!(matches!(
            login.authorized_device().await,
            Err(Error::Unavailable)
        ));
        assert!(
            login
                .sessions
                .lock()
                .await
                .values()
                .all(|s| !s.invalidated && s.authorized_until < Instant::now())
        );
        mode.store(0, Ordering::Release);
        assert!(login.authorized_device().await.unwrap().bearer().is_ok());
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/sessions/pasion"))
                .count(),
            1
        );
        mode.store(3, Ordering::Release);
        for s in login.sessions.lock().await.values_mut() {
            s.checked = Instant::now() - Duration::from_secs(6);
        }
        assert!(matches!(
            login.authorized_device().await,
            Err(Error::Unauthorized)
        ));
        assert!(old.bearer().is_err());
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn slow_management_does_not_block_renewal_and_late_logout_discards_reply() {
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(1));
        let gate = Arc::new(ProjectResponseGate::default());
        let (origin, _, task) =
            fixture_with_project_gate("@owner:test", None, mode, Some(gate.clone())).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let console = native.test_console().clone();
        let login = &console.0.server_login;
        let device = login.authorized_device().await.unwrap();
        let pin = super::super::super::device_execution::DeviceIdentity::from(&device);
        let query_console = console.clone();
        let read = tokio::spawn(async move {
            query_console
                .owner_api_pinned(&pin, OwnerOperation::Projects)
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
            .await
            .expect("project request reaches the held HTTP response");
        assert!(!read.is_finished());
        for s in login.sessions.lock().await.values_mut() {
            s.checked = Instant::now() - Duration::from_secs(6);
        }
        tokio::time::timeout(Duration::from_secs(5), login.authorized_device())
            .await
            .expect("a slow management request cannot hold the authorization lock")
            .unwrap();
        assert!(
            !read.is_finished(),
            "renewal completed before the project reply"
        );
        login.revoke_native_all().await;
        gate.release.notify_one();
        assert!(
            read.await.unwrap().is_err(),
            "late reply cannot revive logged-out authority"
        );
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn cookie_rotation_preserves_inflight_management_reply() {
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(1));
        let gate = Arc::new(ProjectResponseGate::default());
        let (origin, _, task) =
            fixture_with_project_gate("@owner:test", None, mode, Some(gate.clone())).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let console = native.test_console().clone();
        let login = &console.0.server_login;
        let device = login.authorized_device().await.unwrap();
        let pin = super::super::super::device_execution::DeviceIdentity::from(&device);
        let query_console = console.clone();
        let read = tokio::spawn(async move {
            query_console
                .owner_api_pinned(&pin, OwnerOperation::Projects)
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
            .await
            .expect("project request reaches the held HTTP response");
        assert!(!read.is_finished());
        for s in login.sessions.lock().await.values_mut() {
            s.checked = Instant::now() - Duration::from_secs(6);
        }
        tokio::time::timeout(Duration::from_secs(5), login.authorized_device())
            .await
            .expect("a slow management request cannot hold the authorization lock")
            .unwrap();
        assert!(
            !read.is_finished(),
            "renewal completed before the project reply"
        );
        let old_key = {
            let mut sessions = login.sessions.lock().await;
            let (key, session) = sessions.iter_mut().next().unwrap();
            session.expires = Instant::now() + Duration::from_secs(30);
            key.clone()
        };
        native.execute(Command::LoginStatus).await.unwrap();
        assert!(!login.sessions.lock().await.contains_key(&old_key));
        assert!(
            !read.is_finished(),
            "cookie rotation completed before the project reply"
        );
        gate.release.notify_one();
        read.await
            .unwrap()
            .expect("same grant survives cookie rotation while HTTP is in flight");
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn cookie_rotation_cannot_make_concurrent_runtime_authorization_stale() {
        let (origin, _, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let console = native.test_console().clone();
        let login = &console.0.server_login;
        let old_key = {
            let mut sessions = login.sessions.lock().await;
            let (key, s) = sessions.iter_mut().next().unwrap();
            s.expires = Instant::now() + Duration::from_secs(30);
            key.clone()
        };
        let gate = login.renewal.lock().await;
        let rotating = native.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let rotation = tokio::spawn(async move {
            started.send(()).unwrap();
            rotating.execute(Command::LoginStatus).await
        });
        ready.await.unwrap();
        let runtime_console = console.clone();
        let (started, ready) = tokio::sync::oneshot::channel();
        let runtime = tokio::spawn(async move {
            started.send(()).unwrap();
            runtime_console.authorized_device().await
        });
        ready.await.unwrap();
        drop(gate);
        rotation.await.unwrap().unwrap();
        assert!(runtime.await.unwrap().unwrap().bearer().is_ok());
        assert!(
            !login.sessions.lock().await.contains_key(&old_key),
            "test must exercise a real cookie rotation"
        );
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn actual_short_server_proof_refreshes_sdk_once_before_returning_a_live_device() {
        let (origin, requests, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source.clone(),
        )
        .await
        .unwrap();
        *source.token.lock().unwrap() = "sdk-short".into();
        for session in native
            .test_console()
            .0
            .server_login
            .sessions
            .lock()
            .await
            .values_mut()
        {
            session.checked = Instant::now() - Duration::from_secs(6);
        }
        let device = native.test_console().authorized_device().await.unwrap();
        assert!(device.bearer().is_ok());
        assert_eq!(source.refreshes.load(Ordering::Relaxed), 1);
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .any(|(p, b)| p.ends_with("/renew") && b["accessToken"] == "sdk-refreshed")
        );
        assert!(device.valid_until > Instant::now() + Duration::from_secs(20));
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn native_custody_replays_without_a_second_write_and_rechecks_central_visibility() {
        use crate::native_owner::{
            ManagementCommand, ManagementPackage, ManagementPermission as P,
        };
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let (origin, requests, task) = fixture_controlled("@owner:test", None, mode.clone()).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let package = |instance, permissions| ManagementPackage {
            digest: "a".repeat(64),
            instance,
            account_epoch: 1,
            permissions,
        };
        let grants = vec![
            P::ReadConnections,
            P::DecideInbox,
            P::ReadIntents,
            P::AcknowledgeIntents,
        ];
        let manager = native.management(package(1, grants.clone())).await.unwrap();
        let command = || ManagementCommand::Decide {
            command_id: "durable-decision".into(),
            request_id: "req_1".into(),
            expected_revision: 1,
            approve: true,
        };
        let first = manager.central(command()).await.unwrap();
        assert_eq!(manager.saved_commands().await.unwrap().len(), 1);
        assert_eq!(manager.central(command()).await.unwrap(), first);
        manager.retire();
        let manager = native.management(package(2, grants)).await.unwrap();
        assert_eq!(
            manager
                .retry_saved_command("durable-decision".into())
                .await
                .unwrap()["data"],
            first
        );
        let reader = native
            .management(package(3, vec![P::ReadIntents]))
            .await
            .unwrap();
        assert!(reader.saved_commands().await.unwrap().is_empty());
        assert!(
            reader
                .retry_saved_command("durable-decision".into())
                .await
                .is_err()
        );
        mode.store(4, Ordering::Release);
        assert!(
            manager
                .retry_saved_command("durable-decision".into())
                .await
                .is_err(),
            "cached administrator data still requires current central visibility"
        );
        assert_eq!(manager.saved_commands().await.unwrap().len(), 1);
        mode.store(0, Ordering::Release);
        manager
            .acknowledge_command("durable-decision".into())
            .await
            .unwrap();
        assert!(manager.saved_commands().await.unwrap().is_empty());
        assert!(
            manager.central(command()).await.is_err(),
            "acknowledged IDs cannot execute again"
        );
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/requests/req_1/decide"))
                .count(),
            1
        );
        native.shutdown().await;
        task.abort();
    }

    #[tokio::test]
    async fn project_saved_receipts_refresh_current_state_and_check_role_on_retry() {
        use crate::native_owner::{ManagementCommand, ManagementPackage, ManagementPermission};
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(0));
        let (origin, requests, task) = fixture_controlled("@owner:test", None, mode.clone()).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let manager = native
            .management(ManagementPackage {
                digest: "a".repeat(64),
                instance: 1,
                account_epoch: 1,
                permissions: vec![
                    ManagementPermission::DecideProjects,
                    ManagementPermission::ReadIntents,
                    ManagementPermission::AcknowledgeIntents,
                ],
            })
            .await
            .unwrap();
        let command = || ManagementCommand::ProjectDecide {
            command_id: "project-once".into(),
            request_id: "project_req_1".into(),
            expected_revision: 1,
            approve: true,
        };
        let first = manager.central(command()).await.unwrap();
        assert_eq!(first["project"]["state"], "approved");
        let latest = manager
            .retry_saved_command("project-once".into())
            .await
            .unwrap();
        assert_eq!(latest["operation"], "project_decide");
        assert_eq!(
            latest["data"]["project"]["state"], "revoked",
            "saved success must not resurrect an old approval"
        );
        assert_eq!(latest["data"]["project"]["allocationStatus"], "none");
        mode.store(4, Ordering::Release);
        assert!(
            manager
                .retry_saved_command("project-once".into())
                .await
                .is_err(),
            "cached project replies require current central role visibility"
        );
        {
            let log = requests.lock().unwrap();
            let mutations = log
                .iter()
                .filter(|(path, _)| path.ends_with("/project-requests/project_req_1/decide"))
                .collect::<Vec<_>>();
            assert_eq!(
                mutations.len(),
                1,
                "recovering an old result never decides twice"
            );
            assert!(
                log.iter()
                    .filter(|(path, _)| path.starts_with("/hagency/api/v1/management/"))
                    .all(|(_, body)| body["fixtureDeviceGrant"] == true)
            );
        }
        assert!(!native.has_callback_listener());
        native.shutdown().await;
        task.abort();
    }

    #[tokio::test]
    async fn central_management_uses_device_grant_exact_permissions_and_package_retirement() {
        use crate::native_owner::{ManagementCommand, ManagementPackage, ManagementPermission};
        let (origin, requests, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let manager = native
            .management(ManagementPackage {
                digest: "a".repeat(64),
                instance: 1,
                account_epoch: 1,
                permissions: vec![
                    ManagementPermission::ReadConnections,
                    ManagementPermission::ReadInbox,
                ],
            })
            .await
            .unwrap();
        assert_eq!(
            manager
                .central(ManagementCommand::Session {})
                .await
                .unwrap()["account"],
            "@owner:test"
        );
        assert_eq!(
            manager
                .central(ManagementCommand::Inbox {
                    after: None,
                    needs_my_action: true,
                    include_projects: false,
                    include_allocations: false,
                })
                .await
                .unwrap()["items"],
            json!([])
        );
        assert_eq!(
            manager
                .central(ManagementCommand::Decide {
                    command_id: "decide".into(),
                    request_id: "req_1".into(),
                    expected_revision: 1,
                    approve: true
                })
                .await
                .unwrap_err()
                .code,
            "management_permission_required"
        );
        assert_eq!(
            manager
                .central(ManagementCommand::Resume {
                    command_id: "reconnect".into(),
                    contribution_id: "con_1".into(),
                    expected_revision: 1,
                    resource_id: "resource_1".into(),
                    resource_revision: "a".repeat(64)
                })
                .await
                .unwrap_err()
                .code,
            "management_permission_required"
        );
        let count = {
            let requests = requests.lock().unwrap();
            let calls = requests
                .iter()
                .filter(|(path, _)| path.starts_with("/hagency/api/v1/management/"))
                .collect::<Vec<_>>();
            assert_eq!(calls.len(), 2);
            assert!(
                calls
                    .iter()
                    .all(|(_, input)| input["fixtureDeviceGrant"] == true)
            );
            calls.len()
        };
        manager.retire();
        assert_eq!(
            manager
                .central(ManagementCommand::Session {})
                .await
                .unwrap_err()
                .code,
            "management_access_required"
        );
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(path, _)| path.starts_with("/hagency/api/v1/management/"))
                .count(),
            count
        );
        assert!(!native.has_callback_listener());
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn management_account_enrollment_denies_read_only_expired_and_closed_prompts() {
        use crate::native_owner::{ManagementPackage, ManagementPermission, NamespaceEnrollment};
        let (origin, _, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .unwrap();
        let package = |instance, permissions| ManagementPackage {
            digest: "a".repeat(64),
            instance,
            account_epoch: 1,
            permissions,
        };
        let input = || serde_json::from_value::<NamespaceEnrollment>(serde_json::json!({
            "commandId":"current-enrollment", "account":{"accountId":"account_original", "expectedRevision":"a".repeat(64)},
            "model":"gpt-5.6-sol", "reasoning":"medium", "monthlyTokens":1000
        })).unwrap();
        let reader = native
            .management(package(1, vec![ManagementPermission::ReadAccounts]))
            .await
            .unwrap();
        assert_eq!(
            reader
                .enroll_account_resource(
                    input(),
                    Instant::now() + Duration::from_secs(60)
                )
                .await
                .err().unwrap()
                .code,
            "management_access_required"
        );
        let manager = native
            .management(package(2, vec![ManagementPermission::EnrollResources]))
            .await
            .unwrap();
        assert_eq!(
            manager
                .enroll_account_resource(input(), Instant::now())
                .await
                .err().unwrap()
                .code,
            "provider_confirmation_expired"
        );
        manager.retire();
        assert_eq!(
            manager
                .enroll_account_resource(
                    input(),
                    Instant::now() + Duration::from_secs(60)
                )
                .await
                .err().unwrap()
                .code,
            "management_access_required"
        );
        assert!(!native.has_callback_listener());
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn embedded_provider_calls_validate_scope_without_console_transport() {
        use crate::native_owner::ProviderAction;
        let (origin, requests, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source.clone(),
        )
        .await
        .unwrap();
        assert!(!native.has_callback_listener());
        // Disconnect needs no installed CLI and must never start a model. It
        // exercises the real typed service, ledger and SDK/device authorization.
        for runtime in ["claude", "octos"] {
            let result = native
                .execute(Command::RuntimeProvider {
                    runtime: runtime.into(),
                    action: ProviderAction::Disconnect,
                })
                .await
                .unwrap();
            assert_eq!(result["state"], "disconnected");
            assert_eq!(result["authenticated"], false);
            assert_eq!(result["credentialRef"], "");
            for action in [
                ProviderAction::Login,
                ProviderAction::Cancel,
                ProviderAction::Logout,
            ] {
                assert_eq!(
                    native
                        .execute(Command::RuntimeProvider {
                            runtime: runtime.into(),
                            action,
                        })
                        .await
                        .unwrap_err()
                        .code,
                    "local_provider_login_managed_locally"
                );
            }
        }
        assert_eq!(
            native
                .execute(Command::RuntimeProvider {
                    runtime: "../claude".into(),
                    action: ProviderAction::Disconnect,
                })
                .await
                .unwrap_err()
                .code,
            "invalid_arguments"
        );
        assert!(
            native
                .execute(Command::PrepareAgentWorkspace {
                    agent_id: "../foreign".into(),
                })
                .await
                .is_err()
        );
        assert_eq!(
            native
                .execute(Command::PrepareAgentWorkspace {
                    agent_id: "agt_foreign".into(),
                })
                .await
                .unwrap_err()
                .code,
            "owner_scope_required"
        );
        assert!(
            !requests
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _)| path.starts_with("/console/"))
        );
        source.current.store(false, Ordering::Release);
        assert!(
            native
                .execute(Command::RuntimeProvider {
                    runtime: "claude".into(),
                    action: ProviderAction::Disconnect,
                })
                .await
                .is_err()
        );
        native.shutdown().await;
        task.abort();
    }
    #[tokio::test]
    async fn management_retirement_observes_package_and_owner_irreversible_fences() {
        use crate::native_owner::{ManagementPackage, ManagementPermission};
        // Public setup selects process installation authority. Give this case
        // its own process and explicit temporary installation before any open.
        if std::env::var("HAGENCY_MATRIX_MANAGEMENT_RETIREMENT_FIXTURE").as_deref() != Ok("1") {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["console::server_login::shared_matrix::tests::management_retirement_observes_package_and_owner_irreversible_fences", "--exact", "--nocapture"])
                .env("HAGENCY_MATRIX_MANAGEMENT_RETIREMENT_FIXTURE", "1")
                .output().unwrap();
            assert!(result.status.success(), "{} {}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
            return;
        }
        let (origin, _, task) = fixture("@owner:test", None).await;
        let root = tempfile::tempdir().unwrap();
        // tempfile uses platform-default permissions. Let the fixture provision
        // its own owner-private installation directory, as current setup does.
        crate::native_owner::initialize_resource_activation_fixture(&root.path().join("installation"))
            .unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source.clone(),
        )
        .await
        .unwrap();
        let setup_package = || ManagementPackage {
            digest: "b".repeat(64), instance: 8000, account_epoch: 1,
            permissions: vec![ManagementPermission::ReadResources, ManagementPermission::PrepareSourceQuiescence],
        };
        let review = native.review_device_setup(setup_package(), None).await.unwrap();
        let selection = native.confirm_device_setup(review, setup_package(), || true).await.unwrap();
        native.suspend().await.unwrap();
        drop(native);
        let native = selection.open_with_matrix_guarded(
            &root.path().join("state"), &origin, "@owner:test", source, |_| Ok(()),
        ).await.unwrap();
        let package = |instance| ManagementPackage {
            digest: "a".repeat(64),
            instance,
            account_epoch: 1,
            permissions: vec![ManagementPermission::ReadResources],
        };
        let first = native.management(package(1)).await.unwrap();
        assert!(!first.is_retired());
        first.retire();
        assert!(first.is_retired());
        assert_eq!(
            first.resources(Default::default()).await.unwrap_err().code,
            "management_access_required"
        );

        // Closing a package does not log out the owner or revive the old package.
        let second = native.management(package(2)).await.unwrap();
        assert!(!second.is_retired());
        assert!(second.resources(Default::default()).await.is_ok());
        assert!(first.is_retired());
        native.retire();
        assert!(
            second.is_retired(),
            "owner retirement must invalidate every cached manager"
        );
        assert!(second.resources(Default::default()).await.is_err());
        assert!(native.management(package(3)).await.is_err());
        assert!(first.is_retired());
        native.shutdown().await;
        task.abort();
    }

    #[tokio::test]
    async fn sdk_authorization_uses_rotated_snapshot_and_never_refreshes_or_revokes_oauth() {
        let (origin, requests, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-first".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source.clone(),
        )
        .await
        .unwrap();
        assert!(!native.has_callback_listener());
        // The same SDK-authorized owner can open the direct resource service.
        // This does not mount console routes, start a provider or create a
        // second login. A closed package cannot renew its management authority.
        let management = native
            .management(crate::native_owner::ManagementPackage {
                digest: "a".repeat(64),
                instance: 1,
                account_epoch: 1,
                permissions: vec![hagency_store::management::Permission::ReadResources],
            })
            .await
            .unwrap();
        assert_eq!(
            management.resources(Default::default()).await.unwrap_err().code,
            "device_setup_required",
            "SDK login alone must not create a resource profile",
        );
        management.retire();
        assert_eq!(
            management
                .resources(Default::default())
                .await
                .unwrap_err()
                .code,
            "management_access_required"
        );
        assert_eq!(
            native.execute(Command::BeginLogin).await.unwrap_err().code,
            "sdk_owns_matrix_login"
        );
        *source.token.lock().unwrap() = "sdk-rotated".into();
        for session in native
            .test_console()
            .0
            .server_login
            .sessions
            .lock()
            .await
            .values_mut()
        {
            session.checked = Instant::now() - Duration::from_secs(6);
        }
        assert_eq!(native.identity().await.unwrap().subject, "sub");
        native.execute(Command::LoginStatus).await.unwrap();
        native
            .execute(Command::SpaceCandidates { cursor: None })
            .await
            .unwrap();
        {
            let mut sessions = native.test_console().0.server_login.sessions.lock().await;
            for session in sessions.values_mut() {
                session.expires = Instant::now() - Duration::from_secs(1);
                assert!(session.oauth_token.is_empty());
                assert!(session.refresh_token.is_none());
            }
        }
        native.execute(Command::LoginStatus).await.unwrap();
        native.execute(Command::Logout).await.unwrap();
        assert!(source.access_token().await.is_ok());
        let records = requests.lock().unwrap();
        assert!(
            records
                .iter()
                .any(|(path, body)| path == "/_matrix/client/v3/joined_rooms"
                    && body["testBearer"] == "Bearer sdk-rotated")
        );
        assert!(
            records.iter().any(
                |(path, body)| path.ends_with("/renew") && body["accessToken"] == "sdk-rotated"
            )
        );
        assert!(
            records
                .iter()
                .any(|(path, _)| path == "/hagency/api/v1/sessions/current")
        );
        assert!(!records.iter().any(|(path, _)| path.starts_with("/auth/")));
        let metadata =
            std::fs::read_to_string(root.path().join("state/server-login-revocations.json"))
                .unwrap_or_default();
        assert!(!metadata.contains("sdk-first") && !metadata.contains("sdk-rotated"));
        task.abort();
    }
    #[tokio::test]
    async fn wrong_sdk_account_is_rejected_and_only_our_grant_is_revoked() {
        let (origin, requests, task) = fixture("@other:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("shared-secret".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let error = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code, "matrix_account_mismatch");
        let records = requests.lock().unwrap();
        assert!(!records.iter().any(|(path, _)| path.starts_with("/auth/")));
        assert!(!records.iter().any(|(path, _)| path.ends_with("/devices")));
        assert!(
            records
                .iter()
                .any(|(path, _)| path == "/hagency/api/v1/sessions/current")
        );
        task.abort();
    }
    #[tokio::test]
    async fn late_sdk_epoch_change_cannot_install_owner_or_persist_shared_token() {
        let current = Arc::new(AtomicBool::new(true));
        let (origin, requests, task) = fixture("@owner:test", Some(current.clone())).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("shared-epoch-secret".into()),
            current,
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let error = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code, "matrix_account_changed");
        assert!(!root.path().join("state/server-login.json").exists());
        let records = requests.lock().unwrap();
        assert!(
            records
                .iter()
                .any(|(path, _)| path == "/hagency/api/v1/sessions/current")
        );
        assert!(!records.iter().any(|(path, _)| path.starts_with("/auth/")));
        let saved =
            std::fs::read_to_string(root.path().join("state/server-login-revocations.json"))
                .unwrap_or_default();
        assert!(!saved.contains("shared-epoch-secret"));
        task.abort();
    }
    #[tokio::test]
    async fn authorized_sdk_owner_can_retry_after_temporary_source_outage() {
        let (origin, requests, task) = fixture("@owner:test", None).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-temporary".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_authorization_unavailable",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let native = NativeOwner::open_with_matrix(
            &root.path().join("state"),
            &origin,
            "@owner:test",
            source.clone(),
        )
        .await
        .unwrap();
        let old = native
            .test_console()
            .0
            .server_login
            .authorized_device()
            .await
            .unwrap();
        source.current.store(false, Ordering::Release);
        let error = native.execute(Command::LoginStatus).await.unwrap_err();
        assert_eq!(error.code, "matrix_authorization_unavailable");
        assert_eq!(error.status, 503);
        assert!(
            old.bearer().is_ok(),
            "temporary failure cannot revoke an unexpired proof"
        );
        source.current.store(true, Ordering::Release);
        native.execute(Command::LoginStatus).await.unwrap();
        assert!(native.identity().await.is_ok());
        assert_eq!(
            requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(p, _)| p.ends_with("/sessions/pasion"))
                .count(),
            1,
            "temporary outage must recover the existing session"
        );
        native.shutdown().await;
        assert!(
            !requests
                .lock()
                .unwrap()
                .iter()
                .any(|(path, _)| path.starts_with("/auth/"))
        );
        task.abort();
    }
    #[tokio::test]
    async fn retire_while_waiting_for_profile_lock_does_not_publish_sdk_binding() {
        let mode = Arc::new(std::sync::atomic::AtomicU16::new(11));
        let gate = Arc::new(ProjectResponseGate::default());
        let (origin, _, task) =
            fixture_with_project_gate("@owner:test", None, mode, Some(gate.clone())).await;
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let native = NativeOwner::open(&root.path().join("state"), &origin, "@owner:test")
            .await
            .unwrap();
        let console = native.test_console().clone();
        let runner = console.clone();
        let origin2 = origin.clone();
        let source = Arc::new(Source {
            token: std::sync::Mutex::new("sdk-late".into()),
            current: Arc::new(AtomicBool::new(true)),
            error_code: "matrix_account_changed",
            calls: std::sync::atomic::AtomicUsize::new(0),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        let observed_source = source.clone();
        let authorize = tokio::spawn(async move {
            runner
                .0
                .server_login
                .authorize_matrix(&runner, &origin2, "@owner:test", source)
                .await
        });
        tokio::time::timeout(Duration::from_secs(10), gate.entered.notified())
            .await
            .unwrap();
        // The new missing-journal check also reads profiles before the POST.
        // Hold this lock only after that check, at the publication boundary.
        let locked = console.0.server_login.profiles.lock().await;
        gate.release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if observed_source.calls.load(Ordering::Acquire) >= 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        native.retire();
        drop(locked);
        assert_eq!(
            authorize.await.unwrap().unwrap_err(),
            "matrix_account_changed"
        );
        assert!(!root.path().join("state/server-login.json").exists());
        assert!(
            !root
                .path()
                .join("state/server-login-profiles.json")
                .exists()
        );
        native.shutdown().await;
        task.abort();
    }
    struct FailingSource(&'static str);
    impl MatrixTokenSource for FailingSource {
        fn access_token(
            &self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Result<MatrixAccessToken, NativeError>>
                    + Send
                    + '_,
            >,
        > {
            Box::pin(async move {
                Err(NativeError {
                    status: 503,
                    code: self.0.into(),
                })
            })
        }
    }
    #[tokio::test]
    async fn sdk_source_offline_and_unsupported_errors_preserve_retry_semantics_without_grants() {
        let (origin, requests, task) = fixture("@owner:test", None).await;
        for code in [
            "matrix_authorization_unavailable",
            "matrix_oauth_sign_in_required",
            "unsupported_hagency_issuer",
        ] {
            let root =
                hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
            let error = NativeOwner::open_with_matrix(
                &root.path().join("state"),
                &origin,
                "@owner:test",
                Arc::new(FailingSource(code)),
            )
            .await
            .err()
            .unwrap();
            assert_eq!(error.code, code);
            if code == "matrix_authorization_unavailable" {
                assert_eq!(error.status, 503);
            }
        }
        assert!(
            requests
                .lock()
                .unwrap()
                .iter()
                .all(|(path, _)| path == "/hagency/api/v1/discovery")
        );
        task.abort();
    }
    #[test]
    fn sdk_access_snapshot_debug_never_discloses_token() {
        let token = MatrixAccessToken {
            access_token: "private-token".into(),
            client_id: "sdk".into(),
        };
        assert!(!format!("{token:?}").contains("private-token"));
    }
}
