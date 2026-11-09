use super::*;
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Profiles {
    version: u32,
    accounts: Vec<Binding>,
    active_profile_id: Option<String>,
}
fn id(binding: &Binding) -> Option<String> {
    let owner = binding.owner.as_ref()?;
    let subject = binding.subject.as_ref()?;
    let raw = serde_json::to_vec(&(
        &binding.origin,
        crate::server_admission::profile_storage_namespace(&binding.origin),
        subject,
        owner,
    ))
    .ok()?;
    Some(format!("{:x}", Sha256::digest(raw)))
}
impl Profiles {
    pub(super) fn load(path: Option<&Path>, binding: Option<&Binding>) -> Result<Self, Error> {
        let mut profiles = match path {
            Some(path) if path.exists() => {
                serde_json::from_slice::<Self>(&read_private_json(path, 1024 * 1024)?)
                    .map_err(|_| Error::Unavailable)?
            }
            _ => Self {
                version: 1,
                ..Self::default()
            },
        };
        if profiles.version != 1
            || profiles.accounts.len() > 64
            || profiles.accounts.iter().any(|b| {
                origin(&b.origin).is_err()
                    || id(b).is_none()
                    || b.client_id.is_empty()
                    || b.client_id.len() > 128
                    || b.installation_id.len() < 32
                    || b.installation_id.len() > 128
                    || !b
                        .installation_id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                    || b.name.trim().is_empty()
                    || b.name.chars().count() > 128
                    || b.owner
                        .as_ref()
                        .is_none_or(|v| v.is_empty() || v.len() > 255)
                    || b.subject
                        .as_ref()
                        .is_none_or(|v| v.is_empty() || v.len() > 255)
            })
        {
            return Err(Error::Unavailable);
        }
        let mut seen = std::collections::HashSet::new();
        if profiles
            .accounts
            .iter()
            .any(|b| !seen.insert(id(b).unwrap()))
        {
            return Err(Error::Unavailable);
        }
        if let Some(binding) = binding {
            if let Some(key) = id(binding) {
                if !profiles
                    .accounts
                    .iter()
                    .any(|b| id(b).as_ref() == Some(&key))
                {
                    if profiles.accounts.len() >= 64 {
                        return Err(Error::Unavailable);
                    }
                    profiles.accounts.push(binding.clone());
                }
                profiles.active_profile_id = Some(key);
            } else {
                profiles.active_profile_id = None;
            }
        } else {
            profiles.active_profile_id = None;
        }
        Ok(profiles)
    }
    fn persist(&self, path: Option<&Path>) -> Result<(), &'static str> {
        let path = path.ok_or("local_state_unavailable")?;
        hagency_store::private::replace(
            path,
            &serde_json::to_vec(self).map_err(|_| "local_state_unavailable")?,
        )
        .map_err(|_| "local_state_unavailable")
    }
}
impl ServerLogin {
    pub(super) async fn known_matrix_owner(&self, origin: &str, owner: &str) -> bool {
        self.profiles
            .lock()
            .await
            .accounts
            .iter()
            .any(|b| b.origin == origin && b.owner.as_deref() == Some(owner))
    }
    pub(super) async fn known_server(&self, origin: &str) -> Option<Binding> {
        self.profiles
            .lock()
            .await
            .accounts
            .iter()
            .find(|b| b.origin == origin)
            .cloned()
    }
    pub(super) async fn known_identity(&self, origin: &str, subject: &str, mxid: &str) -> bool {
        self.profiles.lock().await.accounts.iter().any(|b| {
            b.origin == origin
                && b.subject.as_deref() == Some(subject)
                && b.owner.as_deref() == Some(mxid)
        })
    }
    pub(super) async fn profiles_status(&self) -> Value {
        let profiles = self.profiles.lock().await;
        json!({"activeProfileId": profiles.active_profile_id, "profiles":profiles.accounts.iter().map(|b|json!({"profileId":id(b),"server":b.origin,"issuer":format!("{}auth/",b.origin),"mxid":b.owner,"name":b.name})).collect::<Vec<_>>()})
    }
    pub(super) async fn select_profile(&self, key: Option<&str>) -> Result<Value, &'static str> {
        let mut profiles = self.profiles.lock().await;
        let selected = match key {
            Some(key) => Some(
                profiles
                    .accounts
                    .iter()
                    .find(|b| id(b).as_deref() == Some(key))
                    .cloned()
                    .ok_or("unknown_profile")?,
            ),
            None => None,
        };
        // Profile data is descriptive only. Selection never restores a token.
        let path = self.path.as_ref().ok_or("local_state_unavailable")?;
        match &selected {
            Some(binding) => hagency_store::private::replace(
                path,
                &serde_json::to_vec(binding).map_err(|_| "local_state_unavailable")?,
            )
            .map_err(|_| "local_state_unavailable")?,
            None => {
                if path.exists() {
                    std::fs::remove_file(path).map_err(|_| "local_state_unavailable")?;
                }
            }
        }
        profiles.active_profile_id = key.map(str::to_owned);
        profiles.persist(self.profiles_path.as_deref())?;
        *self.binding.lock().await = selected.clone();
        *self.pending.lock().await = None;
        *self.status.lock().await =
            json!({"state":"idle","deviceAuthorized":false,"transportOnline":false});
        Ok(json!({"activeProfileId":key,"needsLogin":true,"server":selected.map(|b|b.origin)}))
    }
    pub(super) async fn save_matrix_binding(
        &self,
        binding: Binding,
        source: &std::sync::Arc<dyn super::super::native::MatrixTokenSource>,
    ) -> Result<(), &'static str> {
        let key = id(&binding).ok_or("invalid_server_response")?;
        let mut profiles = self.profiles.lock().await;
        let mut current = self.binding.lock().await;
        let snapshot = source
            .access_token()
            .await
            .map_err(super::super::native::sdk_source_failure)?;
        if self.stopped.load(Ordering::Acquire) || snapshot.client_id != binding.client_id {
            return Err("matrix_account_changed");
        }
        let mut next = Profiles {
            version: profiles.version,
            accounts: profiles.accounts.clone(),
            active_profile_id: Some(key.clone()),
        };
        if let Some(previous) = next
            .accounts
            .iter_mut()
            .find(|b| id(b).as_ref() == Some(&key))
        {
            *previous = binding.clone();
        } else {
            if next.accounts.len() >= 64 {
                return Err("local_profile_limit");
            }
            next.accounts.push(binding.clone());
        }
        let path = self.path.as_ref().ok_or("local_state_unavailable")?;
        let raw = serde_json::to_vec(&binding).map_err(|_| "local_state_unavailable")?;
        // No awaits after the final epoch/liveness check and before publication.
        next.persist(self.profiles_path.as_deref())?;
        hagency_store::private::replace(path, &raw).map_err(|_| "local_state_unavailable")?;
        *profiles = next;
        *current = Some(binding);
        Ok(())
    }
    pub(super) async fn save_profile(&self, binding: &Binding) -> Result<(), &'static str> {
        let Some(key) = id(binding) else {
            return Ok(());
        };
        let mut profiles = self.profiles.lock().await;
        if let Some(previous) = profiles
            .accounts
            .iter_mut()
            .find(|b| id(b).as_ref() == Some(&key))
        {
            *previous = binding.clone();
        } else {
            if profiles.accounts.len() >= 64 {
                return Err("local_profile_limit");
            }
            profiles.accounts.push(binding.clone());
        }
        profiles.active_profile_id = Some(key);
        profiles.persist(self.profiles_path.as_deref())
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Switch {
    #[serde(deserialize_with = "required_profile")]
    profile_id: Option<String>,
}
fn required_profile<'de, D: serde::Deserializer<'de>>(
    value: D,
) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(value)
}
#[handler]
pub(super) async fn switch(req: &mut Request, depot: &Depot, res: &mut Response) {
    let result = async {
        if !same_origin(req, depot, true) || req.uri().query().is_some() {
            return Err("invalid_local_origin");
        }
        let console = console(depot).map_err(|_| "local_state_unavailable")?;
        let login = &console.0.server_login;
        let _transition = login.transition.lock().await;
        current(req, depot).map_err(|_| "local_access_required")?;
        if let Some(value) = cookie(req, COOKIE) {
            login
                .validate(&value)
                .await
                .map_err(|_| "local_access_required")?;
        }
        let raw = super::super::body(req, 8192)
            .await
            .map_err(|_| "invalid_arguments")?;
        let input: Switch = serde_json::from_slice(&raw).map_err(|_| "invalid_arguments")?;
        if let Some(key) = &input.profile_id
            && !login
                .profiles
                .lock()
                .await
                .accounts
                .iter()
                .any(|b| id(b).as_ref() == Some(key))
        {
            return Err("unknown_profile");
        }
        console
            .0
            .authority
            .revoke_all()
            .map_err(|_| "local_state_unavailable")?;
        *login.pending.lock().await = None;
        let mut sessions = login.sessions.lock().await;
        for session in sessions.values_mut() {
            login.queue_revocation(session).await;
        }
        drop(sessions);
        console.stop_owned_runtimes().await;
        console.stop_owner_provider().await;
        {
            let records = login.revocations.lock().await;
            login
                .save_revocations(&records)
                .map_err(|_| "local_state_unavailable")?;
        }
        login.flush_revocations().await;
        let value = login.select_profile(input.profile_id.as_deref()).await?;
        let ticket = console
            .0
            .authority
            .issue_owner_ticket()
            .map_err(|_| "local_state_unavailable")?;
        let bridge = console
            .0
            .authority
            .exchange(&ticket)
            .map_err(|_| "local_state_unavailable")?;
        Ok((value, bridge))
    }
    .await;
    match result {
        Ok((value, bridge)) => {
            res.add_header(
                "set-cookie",
                format!("{COOKIE}={bridge}; HttpOnly; SameSite=Strict; Path=/console"),
                true,
            )
            .unwrap();
            res.render(Json(value));
        }
        Err(code) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(json!({"code":code})));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revoked_device_snapshots_remain_unusable_after_new_profile_selection() {
        let (device, revoked) =
            fixture_device("https://example.test/", "alice-sub", "@alice:example.test");
        assert!(device.bearer().is_ok());
        revoked.store(true, Ordering::Release);
        assert!(device.bearer().is_err());
        assert_eq!(device.subject(), "alice-sub");
    }
    #[test]
    fn switch_requires_explicit_selection_and_rejects_authority_fields() {
        assert!(serde_json::from_value::<Switch>(json!({})).is_err());
        assert!(serde_json::from_value::<Switch>(json!({"profileId":null})).is_ok());
        assert!(
            serde_json::from_value::<Switch>(json!({"profileId":"known","owner":"@spoof:test"}))
                .is_err()
        );
    }
    fn binding(server: &str, owner: &str, subject: &str) -> Binding {
        Binding {
            origin: server.into(),
            client_id: "registered-client".into(),
            installation_id: "x".repeat(43),
            name: "Desktop".into(),
            owner: Some(owner.into()),
            subject: Some(subject.into()),
        }
    }
    #[tokio::test]
    async fn profile_switch_preserves_identity_metadata_without_restoring_credentials() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let login = ServerLogin::new(Some(root.path())).unwrap();
        let alice = binding("https://example.test/", "@alice:example.test", "alice-sub");
        let bob = binding("https://example.test/", "@bob:example.test", "bob-sub");
        let remote = binding("https://other.test/", "@alice:example.test", "alice-sub");
        assert_ne!(id(&alice), id(&bob));
        assert_ne!(id(&alice), id(&remote));
        login.save(alice.clone()).await.unwrap();
        login.save(bob.clone()).await.unwrap();
        login.save(remote.clone()).await.unwrap();
        let selected = login.select_profile(id(&alice).as_deref()).await.unwrap();
        assert_eq!(selected["server"], alice.origin);
        assert_eq!(
            login.binding.lock().await.as_ref().unwrap().subject,
            alice.subject
        );
        assert!(login.sessions.lock().await.is_empty());
        login.select_profile(None).await.unwrap();
        assert!(login.binding.lock().await.is_none());
        assert_eq!(
            login.profiles_status().await["profiles"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        let restarted = ServerLogin::new(Some(root.path())).unwrap();
        assert!(restarted.binding.lock().await.is_none());
        assert_eq!(
            restarted.profiles_status().await["profiles"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(restarted.sessions.lock().await.is_empty());
        assert!(
            restarted
                .select_profile(Some("not-a-profile"))
                .await
                .is_err()
        );
        let raw = std::fs::read_to_string(root.path().join("server-login-profiles.json")).unwrap();
        assert!(!raw.contains("token"));
    }
    #[tokio::test]
    async fn existing_single_owner_binding_is_retained_as_one_profile_without_authority() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let value = binding("https://example.test/", "@alice:example.test", "alice-sub");
        hagency_store::private::replace(
            &root.path().join("server-login.json"),
            &serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
        let login = ServerLogin::new(Some(root.path())).unwrap();
        let status = login.profiles_status().await;
        assert_eq!(status["activeProfileId"], id(&value).unwrap());
        assert_eq!(status["profiles"].as_array().unwrap().len(), 1);
        assert!(login.authorized_device().await.is_err());
    }
    #[tokio::test]
    async fn switching_revokes_every_browser_authority_and_only_mints_local_bridge() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let login = ServerLogin::new(Some(root.path())).unwrap();
        let authority = super::super::super::authority::Authority::new();
        let first = authority
            .exchange(&authority.issue_owner_ticket().unwrap())
            .unwrap();
        let second = authority.server_session(Duration::from_secs(900)).unwrap();
        authority.revoke_all().unwrap();
        assert!(authority.authenticate(&first).is_err());
        assert!(authority.authenticate(&second).is_err());
        let bridge = authority
            .exchange(&authority.issue_owner_ticket().unwrap())
            .unwrap();
        assert!(authority.authenticate(&bridge).is_ok());
        assert!(login.authorized_device().await.is_err());
        assert!(
            login
                .owner_api(&bridge, OwnerOperation::Agents)
                .await
                .is_err()
        );
    }
    #[test]
    fn old_profile_tombstone_cannot_stop_new_owner_or_gain_a_bridge() {
        let alice = binding("https://example.test/", "@alice:example.test", "alice-sub");
        let bob = binding("https://example.test/", "@bob:example.test", "bob-sub");
        let mut session = RemoteSession {
            matrix_source: None,
            token: String::new(),
            oauth_token: String::new(),
            user_id: "serveruser".into(),
            device: None,
            refresh_token: None,
            oauth_expires: Instant::now(),
            authorized_until: Instant::now(),
            invalidated: false,
            binding: alice.clone(),
            checked: Instant::now(),
            expires: Instant::now(),
        };
        assert!(may_stop_session(Some(&alice), &session)); // expiry permits stopping this profile only
        assert!(!may_stop_session(Some(&bob), &session));
        session.invalidated = true;
        assert!(!may_stop_session(Some(&alice), &session));
        assert!(!may_stop_session(Some(&bob), &session));
    }
    #[tokio::test]
    async fn profile_limit_refuses_new_binding_without_overwriting_active_owner() {
        let root = hagency_platform::legacy_writer::test_support::initialized_tempdir().unwrap();
        let login = ServerLogin::new(Some(root.path())).unwrap();
        let first = binding("https://example.test/", "@owner0:example.test", "sub0");
        login.save(first).await.unwrap();
        for n in 1..64 {
            login
                .save_profile(&binding(
                    "https://example.test/",
                    &format!("@owner{n}:example.test"),
                    &format!("sub{n}"),
                ))
                .await
                .unwrap();
        }
        let before = std::fs::read(root.path().join("server-login.json")).unwrap();
        assert_eq!(
            login
                .save(binding(
                    "https://example.test/",
                    "@overflow:example.test",
                    "overflow"
                ))
                .await
                .unwrap_err(),
            "local_profile_limit"
        );
        assert_eq!(
            std::fs::read(root.path().join("server-login.json")).unwrap(),
            before
        );
    }
}
