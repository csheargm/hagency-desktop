//! Private SDK-backed native credentials. Loading this journal grants no authority.
//! Fresh upstream renewal and an exact current-device proof precede every restore.
use super::*;

const FILE: &str = "server-login-sdk-resume.json";
const LIMIT: u64 = 16 * 1024;

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum State {
    Registering,
    Active,
    Revoked,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Device {
    id: String,
    generation: u64,
    token: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Record {
    version: u8,
    scope: String,
    state: State,
    binding: Binding,
    user_id: String,
    session_token: String,
    device: Option<Device>,
}
pub(super) struct Journal {
    path: Option<PathBuf>,
    scope: String,
    record: std::sync::Mutex<Option<Record>>,
}
pub(super) enum Resume {
    Restored(String),
    Register {
        replaces: Option<String>,
        explicit: bool,
    },
}
fn token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn field(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
impl Journal {
    pub(super) fn open(state: Option<&Path>) -> Result<Self, Error> {
        let path = state.map(|p| p.join(FILE));
        let scope = match state {
            Some(p) => {
                let p = p.canonicalize().map_err(|_| Error::Unavailable)?;
                let p = p.to_str().ok_or(Error::Unavailable)?;
                URL_SAFE_NO_PAD.encode(Sha256::digest(p.as_bytes()))
            }
            None => String::new(),
        };
        let record: Option<Record> = match &path {
            Some(p) if p.exists() => Some(
                serde_json::from_slice(&read_private_json(p, LIMIT)?)
                    .map_err(|_| Error::Unavailable)?,
            ),
            _ => None,
        };
        if let Some(r) = &record
            && (r.version != 1
                || r.scope != scope
                || origin(&r.binding.origin).is_err()
                || !field(&r.binding.client_id, 255)
                || !field(&r.binding.installation_id, 128)
                || !field(&r.binding.name, 128)
                || r.binding.owner.as_deref().is_none_or(|s| !field(s, 255))
                || r.binding.subject.as_deref().is_none_or(|s| !field(s, 255))
                || !field(&r.user_id, 128)
                || !token(&r.session_token)
                || r.state == State::Active && r.device.is_none()
                || r.device.as_ref().is_some_and(|d| {
                    !field(&d.id, 128)
                        || d.generation == 0
                        || d.generation > 9_007_199_254_740_991
                        || !token(&d.token)
                }))
        {
            return Err(Error::Unavailable);
        }
        Ok(Self {
            path,
            scope,
            record: std::sync::Mutex::new(record),
        })
    }
    fn persist(&self, r: &Record) -> Result<(), &'static str> {
        let path = self.path.as_ref().ok_or("local_state_unavailable")?;
        let raw = serde_json::to_vec(r).map_err(|_| "local_state_unavailable")?;
        if raw.len() as u64 > LIMIT {
            return Err("local_state_unavailable");
        }
        hagency_store::private::replace(path, &raw).map_err(|_| "local_state_unavailable")
    }
    fn snapshot(&self) -> Result<Option<Record>, &'static str> {
        self.record
            .lock()
            .map(|r| r.clone())
            .map_err(|_| "local_state_unavailable")
    }
    fn is_active(&self, expected: &Record) -> bool {
        self.record.lock().is_ok_and(|r| {
            r.as_ref().is_some_and(|r| {
                r.state == State::Active
                    && r.session_token == expected.session_token
                    && r.device
                        .as_ref()
                        .zip(expected.device.as_ref())
                        .is_some_and(|(a, b)| {
                            a.id == b.id && a.generation == b.generation && a.token == b.token
                        })
            })
        })
    }
    pub(super) fn owns(&self, token: &str) -> bool {
        self.record
            .lock()
            .is_ok_and(|r| r.as_ref().is_some_and(|r| r.session_token == token))
    }
    /// This write occurs before the first device-registration POST. An uncertain
    /// response can never silently repeat that generation-changing operation.
    pub(super) fn registering(
        &self,
        binding: &Binding,
        user: &str,
        token: &str,
        stopped: &AtomicBool,
        replaces: Option<&str>,
    ) -> Result<(), &'static str> {
        let mut current = self.record.lock().map_err(|_| "local_state_unavailable")?;
        if stopped.load(Ordering::Acquire) {
            return Err("matrix_account_changed");
        }
        if current.as_ref().is_some_and(|r| {
            r.state != State::Revoked || replaces != Some(r.session_token.as_str())
        }) || current.is_none() && replaces.is_some()
        {
            return Err("native_resume_required");
        }
        let record = Record {
            version: 1,
            scope: self.scope.clone(),
            state: State::Registering,
            binding: binding.clone(),
            user_id: user.into(),
            session_token: token.into(),
            device: None,
        };
        self.persist(&record)?;
        *current = Some(record);
        Ok(())
    }
    pub(super) fn registered(
        &self,
        token: &str,
        device: &AuthorizedDevice,
        stopped: &AtomicBool,
    ) -> Result<(), &'static str> {
        let mut current = self.record.lock().map_err(|_| "local_state_unavailable")?;
        let r = current
            .as_ref()
            .filter(|r| r.state == State::Registering && r.session_token == token)
            .ok_or("native_resume_required")?;
        if stopped.load(Ordering::Acquire) {
            return Err("matrix_account_changed");
        }
        let mut next = r.clone();
        next.state = State::Active;
        next.device = Some(Device {
            id: device.device_id.clone(),
            generation: device.generation,
            token: device.token.clone(),
        });
        self.persist(&next)?;
        *current = Some(next);
        Ok(())
    }
    pub(super) fn revoke(&self, expected: Option<&str>) -> Result<Option<Record>, Error> {
        let mut current = self.record.lock().map_err(|_| Error::Unavailable)?;
        let Some(r) = current
            .as_mut()
            .filter(|r| expected.is_none_or(|v| r.session_token == v))
        else {
            return Ok(None);
        };
        r.state = State::Revoked;
        // Keep memory retired even if storage failed; report the durable failure.
        self.persist(r).map_err(|_| Error::Unavailable)?;
        Ok(Some(r.clone()))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CurrentDevice {
    user_id: String,
    mxid: String,
    subject: String,
    client_id: String,
    issuer: String,
    device_id: String,
    generation: u64,
    valid_until_ms: u64,
}
impl ServerLogin {
    /// Public OAuth registration only. This is a migration hint, never a
    /// credential or proof of authority. Normal renewal/current-device proof
    /// must still succeed before the existing native root can run.
    pub(in crate::console) fn resume_client_id(
        state: &Path, expected_origin: &str, expected_owner: &str,
    ) -> Result<String, &'static str> {
        let journal = Journal::open(Some(state)).map_err(|_| "native_resume_unavailable")?;
        let record = journal.snapshot()?.ok_or("native_resume_missing")?;
        if record.binding.origin != expected_origin || record.binding.owner.as_deref() != Some(expected_owner) {
            return Err("matrix_account_mismatch");
        }
        if record.state != State::Active { return Err("native_resume_revoked"); }
        Ok(record.binding.client_id)
    }
    pub(in crate::console) fn preserves_sdk_resume(code: &str) -> bool {
        matches!(
            code,
            "server_unavailable"
                | "matrix_authorization_unavailable"
                | "native_resume_unavailable"
                | "native_resume_protocol_unavailable"
                | "native_device_registration_unknown"
                | "local_state_unavailable"
        )
    }
    pub(in crate::console) fn revoke_sdk_resume(&self) -> Result<(), Error> {
        self.sdk_resume.revoke(None).map(|_| ())
    }
    pub(super) async fn queue_saved_sdk_revocation(&self) {
        // Even a failed local tombstone must still try revoking the exact
        // original remote credential. The checked shutdown reports that failure.
        let r = match self.sdk_resume.revoke(None) {
            Ok(r) => r,
            Err(_) => self.sdk_resume.snapshot().ok().flatten(),
        };
        let Some(r) = r else { return };
        let mut pending = self.revocations.lock().await;
        if !pending.iter().any(|p| {
            p.origin == r.binding.origin
                && p.client_id == r.binding.client_id
                && p.session_token == r.session_token
        }) {
            pending.push(Revocation {
                origin: r.binding.origin,
                client_id: r.binding.client_id,
                session_token: r.session_token,
                oauth_token: String::new(),
                hint: "access_token".into(),
                session_done: false,
                oauth_done: true,
            });
            let _ = self.save_revocations(&pending);
        }
    }
    pub(super) async fn try_sdk_resume(
        &self,
        console: &super::super::Console,
        expected_origin: &str,
        expected_owner: &str,
        source: std::sync::Arc<dyn super::super::native::MatrixTokenSource>,
        client_id: &str,
    ) -> Result<Resume, &'static str> {
        let Some(record) = self.sdk_resume.snapshot()? else {
            let known = self
                .known_matrix_owner(expected_origin, expected_owner)
                .await;
            if known && !source.permits_explicit_reenrollment() {
                // Lost credentials and an upgrade from the former memory-only
                // client are not proof that another registration is harmless.
                return Err("native_resume_missing");
            }
            return Ok(Resume::Register {
                replaces: None,
                explicit: known,
            });
        };
        if record.binding.origin != expected_origin
            || record.binding.owner.as_deref() != Some(expected_owner)
        {
            return Err("matrix_account_mismatch");
        }
        match record.state {
            State::Registering => return Err("native_device_registration_unknown"),
            State::Revoked => {
                if !source.permits_explicit_reenrollment() {
                    return Err("native_resume_revoked");
                }
                self.revoke_sdk_resume()
                    .map_err(|_| "local_state_unavailable")?;
                self.queue_saved_sdk_revocation().await;
                self.flush_revocations().await;
                if self
                    .revocations
                    .lock()
                    .await
                    .iter()
                    .any(|p| p.session_token == record.session_token)
                {
                    return Err("native_resume_unavailable");
                }
                let current = source
                    .access_token()
                    .await
                    .map_err(super::super::native::sdk_source_failure)?;
                if current.client_id != client_id
                    || !source.permits_explicit_reenrollment()
                    || self.stopped.load(Ordering::Acquire)
                {
                    return Err("matrix_account_changed");
                }
                return Ok(Resume::Register {
                    replaces: Some(record.session_token),
                    explicit: true,
                });
            }
            State::Active => (),
        }
        if record.binding.client_id != client_id {
            return Err("matrix_account_mismatch");
        }
        if self
            .revocations
            .lock()
            .await
            .iter()
            .any(|r| r.session_token == record.session_token)
        {
            return Err("native_resume_revoked");
        }
        let d = record.device.as_ref().ok_or("native_resume_unavailable")?;
        let now = Instant::now();
        let device = AuthorizedDevice {
            origin: record.binding.origin.clone(),
            device_id: d.id.clone(),
            generation: d.generation,
            token: d.token.clone(),
            owner_mxid: expected_owner.into(),
            issuer: format!("{}auth/", expected_origin),
            subject: record
                .binding
                .subject
                .clone()
                .ok_or("native_resume_unavailable")?,
            user_id: record.user_id.clone(),
            valid_until: now,
            revoked: std::sync::Arc::new(AtomicBool::new(false)),
            stopped: self.stopped.clone(),
        };
        let mut session = RemoteSession {
            matrix_source: Some(source.clone()),
            token: record.session_token.clone(),
            oauth_token: String::new(),
            user_id: record.user_id.clone(),
            device: Some(device),
            refresh_token: None,
            oauth_expires: now,
            authorized_until: now,
            invalidated: false,
            binding: record.binding.clone(),
            checked: now,
            expires: now + Duration::from_secs(900),
        };
        renew(&mut session).await.map_err(|e| match e {
            Error::Unauthorized => "sign_in_required",
            _ => "native_resume_unavailable",
        })?;
        let client = http()?;
        let server = origin(expected_origin)?;
        let response = client
            .get(server.join("/hagency/api/v1/devices/current").unwrap())
            .bearer_auth(&d.token)
            .send()
            .await
            .map_err(|_| "native_resume_unavailable")?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err("native_resume_protocol_unavailable");
        }
        if matches!(response.status().as_u16(), 401 | 403) {
            // Fresh renewal already proved this exact original account. The
            // rejected device credential can never become current again.
            self.revoke_sdk_resume()
                .map_err(|_| "local_state_unavailable")?;
            return Err("sign_in_required");
        }
        let value = super::response(response).await.map_err(|code| match code {
            "sign_in_required" | "authorization_expired" | "authentication_required" => {
                "sign_in_required"
            }
            _ => "native_resume_unavailable",
        })?;
        let proof: CurrentDevice =
            serde_json::from_value(value).map_err(|_| "native_resume_protocol_unavailable")?;
        if proof.user_id != record.user_id
            || proof.mxid != expected_owner
            || proof.subject != record.binding.subject.as_deref().unwrap_or("")
            || proof.client_id != client_id
            || proof.issuer != format!("{expected_origin}auth/")
            || proof.device_id != d.id
            || proof.generation != d.generation
        {
            return Err("matrix_account_mismatch");
        }
        // A proof outside the local clock window says the clocks disagree or
        // the answer was slow, not that the owner signed out. Renewal treats
        // the same miss as unavailable; so does this restore.
        session.authorized_until = session.authorized_until.min(
            deadline(&json!({"validUntilMs":proof.valid_until_ms}))
                .map_err(|_| "native_resume_unavailable")?,
        );
        let after = source
            .access_token()
            .await
            .map_err(super::super::native::sdk_source_failure)?;
        if after.client_id != client_id
            || self.stopped.load(Ordering::Acquire)
            || !self.sdk_resume.is_active(&record)
        {
            return Err("matrix_account_changed");
        }
        self.save_matrix_binding(record.binding.clone(), &source)
            .await?;
        let cookie = console
            .0
            .authority
            .server_session(Duration::from_secs(900))
            .map_err(|_| "local_state_unavailable")?;
        let mut sessions = self.sessions.lock().await;
        if self.stopped.load(Ordering::Acquire) || !self.sdk_resume.is_active(&record) {
            return Err("matrix_account_changed");
        }
        if session.authorized_until <= Instant::now() {
            return Err("native_resume_unavailable");
        }
        if sessions
            .values()
            .any(|s| !s.invalidated && s.token != record.session_token)
        {
            return Err("matrix_account_changed");
        }
        // Restoring the original Active device satisfies this sign-in. Retaining
        // an unused permission here would allow a later remote revocation to
        // trigger registration again without another actual OAuth sign-in.
        source.consume_explicit_reenrollment();
        sessions.clear();
        sessions.insert(session_key(&cookie), session);
        drop(sessions);
        *self.status.lock().await =
            json!({"state":"device_authorized","deviceAuthorized":true,"transportOnline":false});
        Self::start_worker(console);
        Ok(Resume::Restored(cookie))
    }
}

impl super::super::native::NativeOwner {
    /// Test fixture: an executor resume record of `state`, written through the
    /// real journal (scope bound to the root's path). Grants nothing. Hosts
    /// with `test-support` use it to test their owner-root selection.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn write_test_resume_record(
        state: &Path,
        origin: &str,
        owner: &str,
        client_id: &str,
        active: bool,
    ) {
        let journal = Journal::open(Some(state)).unwrap();
        let record = Record {
            version: 1,
            scope: journal.scope.clone(),
            state: if active {
                State::Active
            } else {
                State::Revoked
            },
            binding: Binding {
                origin: origin.into(),
                client_id: client_id.into(),
                installation_id: "fixture-installation".into(),
                name: "Hagency Desktop".into(),
                owner: Some(owner.into()),
                subject: Some(owner.into()),
            },
            user_id: "fixture-user".into(),
            session_token: "a".repeat(64),
            device: Some(Device {
                id: "FIXTURE_DEVICE".into(),
                generation: 1,
                token: "b".repeat(64),
            }),
        };
        journal.persist(&record).unwrap();
    }
    /// Reduction-only explicit logout/removal when no NativeOwner was opened.
    /// The trusted host supplies its original state root and account binding.
    /// No SDK token, session, runtime or registration is created by this call.
    pub fn revoke_saved_matrix_resumption(
        state: &Path,
        expected_origin: &str,
        expected_owner: &str,
    ) -> Result<bool, super::super::native::NativeError> {
        let fail = |code: &str| super::super::native::NativeError {
            status: 409,
            code: code.into(),
        };
        let expected_origin = origin(expected_origin)
            .map_err(|_| fail("invalid_server_origin"))?
            .to_string();
        if !expected_owner.starts_with('@')
            || !expected_owner.contains(':')
            || !field(expected_owner, 255)
        {
            return Err(fail("invalid_owner_identity"));
        }
        crate::owner_host::with_existing_state(state, |path| {
            let journal =
                Journal::open(Some(path)).map_err(|_| fail("native_logout_persistence_failed"))?;
            let Some(record) = journal
                .snapshot()
                .map_err(|_| fail("native_logout_persistence_failed"))?
            else {
                return Ok(false);
            };
            if record.binding.origin != expected_origin
                || record.binding.owner.as_deref() != Some(expected_owner)
            {
                return Err(fail("matrix_account_mismatch"));
            }
            journal
                .revoke(None)
                .map_err(|_| fail("native_logout_persistence_failed"))?;
            let revocations_path = path.join("server-login-revocations.json");
            let mut pending: Vec<Revocation> = if revocations_path.exists() {
                serde_json::from_slice(
                    &read_private_json(&revocations_path, 1024 * 1024)
                        .map_err(|_| fail("native_logout_persistence_failed"))?,
                )
                .map_err(|_| fail("native_logout_persistence_failed"))?
            } else {
                Vec::new()
            };
            if pending.len() > 64 {
                return Err(fail("native_logout_persistence_failed"));
            }
            if !pending.iter().any(|r| {
                r.origin == record.binding.origin
                    && r.client_id == record.binding.client_id
                    && r.session_token == record.session_token
            }) {
                if pending.len() == 64 {
                    return Err(fail("native_logout_persistence_failed"));
                }
                pending.push(Revocation {
                    origin: record.binding.origin,
                    client_id: record.binding.client_id,
                    session_token: record.session_token,
                    oauth_token: String::new(),
                    hint: "access_token".into(),
                    session_done: false,
                    oauth_done: true,
                });
            }
            let raw = serde_json::to_vec(&pending)
                .map_err(|_| fail("native_logout_persistence_failed"))?;
            hagency_store::private::replace(&revocations_path, &raw)
                .map_err(|_| fail("native_logout_persistence_failed"))?;
            Ok(true)
        })
        .map_err(|e| {
            fail(match e {
                crate::owner_host::Error::AlreadyRunning => "native_owner_state_busy",
                _ => "native_owner_state_unavailable",
            })
        })?
        .unwrap_or(Ok(false))
    }
}
