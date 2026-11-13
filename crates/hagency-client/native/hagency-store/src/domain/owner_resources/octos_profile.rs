//! Owner-selected Octos configuration identities. A profile is a budget
//! boundary, not evidence of a third-party provider's account or quota.
use super::*;
use hagency_runtime::octos::profile_binding::ProfileBinding;
use sha2::{Digest, Sha256};

const KEY_FILE: &str = "octos-profile-key-v1";

/// Only checks that a running process still uses the owner's original
/// configuration and installation key. It grants no work or activity lease.
pub struct OctosRunningGuard {
    profile: hagency_runtime::octos::profile_binding::RunningProfileGuard,
    root: PathBuf,
    key_commitment: String,
}
impl OctosRunningGuard {
    pub fn check(&self) -> Result<(), Error> {
        self.profile.check().map_err(|_| Error::LocalAuthority)?;
        let bytes = crate::private::read_secret(&self.root.join(KEY_FILE))?;
        let key: [u8; 32] = bytes.try_into().map_err(|_| Error::Private)?;
        if commitment(&key) != self.key_commitment {
            return Err(Error::LocalAuthority);
        }
        self.profile.check().map_err(|_| Error::LocalAuthority)
    }
}

/// An original native observation; never deserialized or accepted from the
/// server. Paths, configuration bytes, and credentials stay local.
pub struct OwnerOctosProfile {
    binding: Arc<ProfileBinding>,
    registry_id: String,
    root: PathBuf,
    key_commitment: String,
    digest: String,
    version: String,
    legacy_version: String,
}
impl OwnerOctosProfile {
    pub(super) fn running_guard(&self) -> Result<OctosRunningGuard, Error> {
        self.check(&self.registry_id)?;
        let guard = OctosRunningGuard {
            profile: self.binding.running_guard().map_err(|_| Error::LocalAuthority)?,
            root: self.root.clone(),
            key_commitment: self.key_commitment.clone(),
        };
        guard.check()?;
        Ok(guard)
    }
    pub(super) fn check(&self, registry: &str) -> Result<(), Error> {
        if self.registry_id != registry {
            return Err(Error::LocalAuthority);
        }
        self.binding.check().map_err(|_| Error::LocalAuthority)?;
        let bytes = crate::private::read_secret(&self.root.join(KEY_FILE))?;
        let key: [u8; 32] = bytes.try_into().map_err(|_| Error::Private)?;
        if commitment(&key) != self.key_commitment {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    pub(super) fn expires(&self) -> Instant {
        self.binding.expires()
    }
    pub(super) fn digest(&self) -> &str {
        &self.digest
    }
    pub(super) fn version(&self) -> &str {
        &self.version
    }
    pub(super) fn matches_version(&self, stored: &str) -> bool {
        stored == self.version || stored == self.legacy_version && self.binding.is_primary()
    }
    pub fn profile_id(&self) -> &str {
        self.binding.profile_id()
    }
    pub fn model(&self) -> &str {
        &self.binding.model().model
    }
    pub(super) fn qualify_process(
        &self,
        process: &mut hagency_runtime::owned::OwnedOctosSession,
    ) -> Result<hagency_runtime::owned::SharedTextProcessObservation, Error> {
        self.check(&self.registry_id)?;
        let bytes = crate::private::read_secret(&self.root.join(KEY_FILE))?;
        let key: [u8; 32] = bytes.try_into().map_err(|_| Error::Private)?;
        if process.shared_profile_selection_key(&key).ok().as_deref() != Some(self.version.as_str()) {
            process.stop();
            return Err(Error::LocalAuthority);
        }
        let observation = process
            .qualify_shared_profile(&key)
            .map_err(|_| Error::LocalAuthority)?;
        if observation.provider_digest() != Some(self.digest.as_str())
            || self.check(&self.registry_id).is_err()
        {
            process.stop();
            return Err(Error::LocalAuthority);
        }
        Ok(observation)
    }
}

fn commitment(key: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"hagency-octos-ledger-key-v1\0");
    hash.update(key);
    format!("{:x}", hash.finalize())
}

/// SQLite serializes initialization. An orphaned file may be reused after a
/// crash before the first commit; after a commitment exists, missing or
/// changed key material is a hard failure, never a fresh accounting identity.
fn key(db: &mut Connection, root: &Path) -> Result<[u8; 32], Error> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let recorded: Option<String> = tx
        .query_row(
            "SELECT commitment FROM owner_octos_key WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if recorded.is_none()
        && tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM owner_octos_profiles)",
            [],
            |r| r.get::<_, bool>(0),
        )?
    {
        return Err(Error::LocalAuthority);
    }
    let path = root.join(KEY_FILE);
    let bytes = match crate::private::read_secret(&path) {
        Ok(bytes) => bytes,
        Err(Error::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound && recorded.is_none() =>
        {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|_| Error::Unavailable)?;
            if bytes.iter().all(|byte| *byte == 0) {
                return Err(Error::Unavailable);
            }
            crate::private::write_new(&path, &bytes)?;
            bytes.to_vec()
        }
        Err(error) => return Err(error),
    };
    let key: [u8; 32] = bytes.try_into().map_err(|_| Error::Private)?;
    if key.iter().all(|byte| *byte == 0) {
        return Err(Error::Private);
    }
    let digest = commitment(&key);
    match recorded {
        Some(expected) if expected != digest => return Err(Error::LocalAuthority),
        Some(_) => {}
        None => {
            tx.execute("INSERT INTO owner_octos_key VALUES(1,?1)", [&digest])?;
        }
    }
    tx.commit()?;
    Ok(key)
}

impl OwnerResourceRegistry {
    /// Recover the local profile name for an owner-authorized resource. This
    /// metadata alone grants no execution; the caller must observe and verify
    /// the actual profile before starting an isolated process.
    pub async fn octos_profile_for_resource(
        &self,
        profile: ProfileAuthority,
        resource_id: String,
        revision: String,
    ) -> Result<String, Error> {
        profile.check()?;
        hash(&revision)?;
        hagency_core::project::identifier(&resource_id, 128)?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let fence = self.0.clone();
        let caller = profile.clone();
        let id = self.0.store.call(4096, move |db| {
            profile.check()?;
            if fence.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
            authorize_resource(&db.db, &profile, &resource_id)?;
            let resource = read_resource(&db.db, &resource_id)?;
            if resource.framework != "octos"
                || resource.provider.as_deref() != Some(hagency_core::qualification::OWNER_ENDPOINT)
                || crate::resource_publication_revision(&resource)? != revision
            { return Err(Error::LocalAuthority); }
            let recorded: Option<(String,String)> = db.db.query_row(
                "SELECT o.profile_id,m.model FROM owner_octos_model_resources m JOIN owner_octos_profiles o ON o.profile_digest=m.profile_digest JOIN owner_resource_accounts a ON a.provider_digest=o.profile_digest WHERE a.seat_id=?1 AND a.identity_kind='explicit_owner_mapping' AND m.resource_id=?2",
                params![resource.seat_id,resource_id], |r| Ok((r.get(0)?,r.get(1)?)),
            ).optional()?;
            let (id, model) = recorded.ok_or(Error::LocalAuthority)?;
            if !hagency_runtime::octos::profile_id(&id)
                || resource.octos_profile.as_deref() != Some(id.as_str()) || resource.model != model
            { return Err(Error::LocalAuthority); }
            profile.check()?;
            if fence.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
            Ok(id)
        }).await?;
        caller.check()?;
        if self.0.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
        Ok(id)
    }

    /// Read all owner-visible model resources for this local profile without
    /// creating a ledger key, a binding, or a grant.
    pub async fn octos_account_overview(
        &self,
        profile: ProfileAuthority,
        binding: ProfileBinding,
    ) -> Result<(Vec<(String,String)>, Option<u64>), Error> {
        profile.check()?;
        binding.check().map_err(|_| Error::LocalAuthority)?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let fence = self.0.clone();
        let caller = profile.clone();
        let result=self.0.store.call(4096,move |db| {
            profile.check()?;
            binding.check().map_err(|_| Error::LocalAuthority)?;
            if fence.closed.load(Ordering::Acquire) {return Err(Error::LocalAuthority);}
            let recorded:Option<String>=db.db.query_row("SELECT commitment FROM owner_octos_key WHERE singleton=1",[],|r|r.get(0)).optional()?;
            let Some(expected)=recorded else {
                if db.db.query_row("SELECT EXISTS(SELECT 1 FROM owner_octos_profiles)",[],|r|r.get::<_,bool>(0))? {return Err(Error::LocalAuthority);}
                binding.check().map_err(|_| Error::LocalAuthority)?;
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {return Err(Error::LocalAuthority);}
                return Ok((Vec::new(), None));
            };
            let bytes=crate::private::read_secret(&fence.root.join(KEY_FILE))?;
            let key:[u8;32]=bytes.try_into().map_err(|_| Error::Private)?;
            if key.iter().all(|b|*b==0) || commitment(&key)!=expected {return Err(Error::LocalAuthority);}
            let digest=binding.accounting_key(&key).map_err(|_| Error::LocalAuthority)?;
            let mut statement=db.db.prepare("SELECT m.model,r.id FROM owner_octos_profiles o JOIN owner_resource_accounts a ON a.provider_digest=o.profile_digest AND a.identity_kind='explicit_owner_mapping' JOIN owner_octos_model_resources m ON m.profile_digest=o.profile_digest JOIN resources r ON r.id=m.resource_id AND json_extract(r.config,'$.seatId')=a.seat_id JOIN owner_resource_grants g ON g.resource_id=r.id AND g.profile=?2 AND g.active=1 WHERE o.profile_digest=?1 AND json_extract(r.config,'$.framework')='octos' AND json_extract(r.config,'$.octosProfile')=o.profile_id AND json_extract(r.config,'$.model')=m.model ORDER BY r.id LIMIT 65")?;
            let ids=statement.query_map(params![digest,profile.digest],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
            if ids.len()>64 {return Err(Error::Capacity);}
            let quota = if ids.is_empty() { None } else {
                let seat: String = db.db.query_row("SELECT seat_id FROM owner_resource_accounts WHERE provider_digest=?1 AND identity_kind='explicit_owner_mapping'", [&digest], |r| r.get(0))?;
                crate::domain::account_ceiling::declaration(&db.db, &seat)?.and_then(|d| d.quota_tokens.map(u64::from))
            };
            binding.check().map_err(|_| Error::LocalAuthority)?;
            profile.check()?;
            if fence.closed.load(Ordering::Acquire) {return Err(Error::LocalAuthority);}
            Ok((ids,quota))
        }).await?;
        caller.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }
    pub async fn find_octos_account_resources(
        &self, profile: ProfileAuthority, binding: ProfileBinding,
    ) -> Result<Vec<(String,String)>, Error> {
        Ok(self.octos_account_overview(profile, binding).await?.0)
    }
    pub async fn find_octos_account_resource(
        &self, profile: ProfileAuthority, binding: ProfileBinding,
    ) -> Result<Option<String>, Error> {
        let model = binding.model().model.clone();
        Ok(self.find_octos_account_resources(profile, binding).await?.into_iter()
            .find_map(|(name, resource)| (name == model).then_some(resource)))
    }
    /// Derive the stable local budget slot and a short-lived version witness.
    /// This does not enroll, publish, allocate, or launch a shared Agent.
    pub async fn observe_octos_profile(
        &self,
        binding: ProfileBinding,
    ) -> Result<OwnerOctosProfile, Error> {
        binding.check().map_err(|_| Error::LocalAuthority)?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let binding = Arc::new(binding);
        let observed = binding.clone();
        let fence = self.0.clone();
        let (digest, version, legacy_version, key_commitment) = self
            .0
            .store
            .call(4096, move |db| {
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                observed.check().map_err(|_| Error::LocalAuthority)?;
                let key = key(&mut db.db, &fence.root)?;
                let digest = observed
                    .accounting_key(&key)
                    .map_err(|_| Error::LocalAuthority)?;
                let legacy_version = observed
                    .configuration_key(&key)
                    .map_err(|_| Error::LocalAuthority)?;
                let version = observed.selected_model_key(&key).map_err(|_| Error::LocalAuthority)?;
                observed.check().map_err(|_| Error::LocalAuthority)?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                Ok((digest, version, legacy_version, commitment(&key)))
            })
            .await?;
        let result = OwnerOctosProfile {
            binding,
            registry_id: self.id().into(),
            root: self.0.root.clone(),
            key_commitment,
            digest,
            version,
            legacy_version,
        };
        result.check(self.id())?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }

    /// Recover only the original account-form enrollment, scoped to its owner.
    /// No current configuration is needed to return an already committed result.
    pub async fn recover_octos_account_enrollment(
        &self,
        profile: ProfileAuthority,
        input: NamespaceEnrollment,
    ) -> Result<Option<OwnerResourceReceipt>, Error> {
        profile.check()?;
        input.validate()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let digest = canonical::digest(&serde_json::to_value(&input)?)?;
        let fence = self.0.clone();
        let caller = profile.clone();
        let result=self.0.store.call(4096,move |db| {
            profile.check()?;
            let old:Option<(String,String,String)>=db.db.query_row(
                "SELECT n.profile,n.input_digest,r.result FROM owner_octos_enrollments n JOIN owner_resource_receipts r ON r.command_id=n.command_id WHERE n.command_id=?1",
                [&input.command_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let result=match old {
                Some((owner,original,value))=>{
                    if owner!=profile.digest {return Err(Error::LocalAuthority);}
                    if original!=digest {return Err(Error::Conflict);}
                    Some(serde_json::from_str(&value)?)
                }
                None=>{
                    if db.db.query_row("SELECT EXISTS(SELECT 1 FROM owner_resource_receipts WHERE command_id=?1)",[&input.command_id],|r|r.get::<_,bool>(0))? {return Err(Error::Conflict);}
                    None
                }
            };
            profile.check()?;
            if fence.closed.load(Ordering::Acquire) {return Err(Error::LocalAuthority);}
            Ok(result)
        }).await?;
        caller.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }
}

/// The caller holds the original consent and checks it again before commit.
/// A profile has one stable seat. Each owner-approved configured model gets
/// its own resource on that seat, so all share the same declared total.
pub(super) fn enroll(
    tx: &Transaction<'_>,
    proof: &OwnerOctosProfile,
    monthly_tokens: u64,
) -> Result<(String, bool), Error> {
    proof.check(&proof.registry_id)?;
    let stored_key: Option<String> = tx
        .query_row(
            "SELECT commitment FROM owner_octos_key WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if stored_key.as_deref() != Some(&proof.key_commitment) {
        return Err(Error::LocalAuthority);
    }
    let seat_id = format!("seat_owner_{}", proof.digest);
    let account: Option<(String, String)> = tx
        .query_row(
            "SELECT seat_id,identity_kind FROM owner_resource_accounts WHERE provider_digest=?1",
            [&proof.digest],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let fresh = account.is_none();
    if let Some((stored_seat, kind)) = &account {
        if stored_seat != &seat_id || kind != "explicit_owner_mapping" { return Err(Error::LocalAuthority); }
        let profile: String = tx.query_row("SELECT profile_id FROM owner_octos_profiles WHERE profile_digest=?1",
            [&proof.digest], |r| r.get(0)).map_err(|_| Error::Quarantined)?;
        if profile != proof.profile_id() { return Err(Error::LocalAuthority); }
    } else {
        let occupied: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM seats WHERE id=?1) OR EXISTS(SELECT 1 FROM owner_octos_profiles WHERE profile_digest=?2)",
            params![seat_id,proof.digest], |r| r.get(0))?;
        if occupied { return Err(Error::Quarantined); }
        let seat: Seat = serde_json::from_value(json!({"id":seat_id,
            "declaration":{"quotaTokens":monthly_tokens,"period":"monthly"}}))?;
        seat.validate()?;
        bounded_row(tx, "seats", "id", &seat_id, 2048)?;
        tx.execute("INSERT INTO seats(id,config) VALUES(?1,?2)", params![seat_id,serde_json::to_string(&seat)?])?;
        tx.execute("INSERT INTO owner_resource_accounts VALUES(?1,?2,'explicit_owner_mapping')", params![proof.digest,seat_id])?;
        tx.execute("INSERT INTO owner_octos_profiles VALUES(?1,?2,?3,?4)",
            params![proof.digest,proof.profile_id(),proof.model(),proof.version])?;
    }
    let existing: Option<String> = tx.query_row(
        "SELECT resource_id FROM owner_octos_model_resources WHERE profile_digest=?1 AND model=?2",
        params![proof.digest,proof.model()], |r| r.get(0)).optional()?;
    if let Some(resource_id) = existing {
        let old = read_resource(tx, &resource_id).map_err(|_| Error::Quarantined)?;
        let requested = serde_json::json!({"tokens":monthly_tokens,"period":"monthly"});
        if old.seat_id != seat_id || old.framework != "octos" || old.model != proof.model()
            || old.provider.as_deref() != Some(hagency_core::qualification::OWNER_ENDPOINT)
            || old.octos_profile.as_deref() != Some(proof.profile_id())
            || serde_json::to_value(crate::domain::resource_ceiling::effective_ceiling(tx, &old)?)? != requested
        { return Err(Error::Conflict); }
        tx.execute("UPDATE owner_octos_model_resources SET configuration_digest=?1 WHERE profile_digest=?2 AND model=?3",
            params![proof.version,proof.digest,proof.model()])?;
        return Ok((old.id(), false));
    }
    let declaration = crate::domain::account_ceiling::declaration(tx, &seat_id)?.ok_or(Error::LocalAuthority)?;
    let total = declaration.quota_tokens.map(u64::from).ok_or(Error::LocalAuthority)?;
    if monthly_tokens > total { return Err(Error::NoCeiling); }
    let model_count: u64 = tx.query_row("SELECT COUNT(*) FROM owner_octos_model_resources WHERE profile_digest=?1", [&proof.digest], |r| r.get(0))?;
    if model_count >= 64 { return Err(Error::Capacity); }
    let preset_id = if fresh { format!("owner_octos_{}", proof.digest) } else {
        let id = canonical::digest(&json!({"profile":proof.digest,"model":proof.model()}))?;
        format!("owner_octos_{id}")
    };
    let candidate: Resource = serde_json::from_value(json!({
        "presetId":preset_id,"seatId":seat_id,"framework":"octos", "model":proof.model(),
        "provider":hagency_core::qualification::OWNER_ENDPOINT,"octosProfile":proof.profile_id(),
        "published":false,"ceiling":{"tokens":monthly_tokens,"period":"monthly"}
    }))?;
    let resource = prepare_resource_write(tx, &candidate, Some(false), true)?;
    write_resource_configuration(tx, &resource, true)?;
    bounded_row(tx, "owner_octos_model_resources", "resource_id", &resource.id(), 2048)?;
    tx.execute("INSERT INTO owner_octos_model_resources(profile_digest,model,resource_id,configuration_digest) VALUES(?1,?2,?3,?4)",
        params![proof.digest,proof.model(),resource.id(),proof.version])?;
    Ok((resource.id(), true))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;

    fn expires() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }
    fn authority() -> ProfileAuthority {
        ProfileAuthority::new("a".repeat(64), "@owner:example.test".into(), expires()).unwrap()
    }
    fn write_profile(home: &Path, id: &str, model: &str, secret: &str) {
        fs::write(
            home.join("profiles").join(format!("{id}.json")),
            json!({
                "id":id,"config":{"llm":{"primary":{"family_id":"local","model_id":model}},
                "env_vars":{"TOKEN":secret}}
            })
            .to_string(),
        )
        .unwrap();
    }
    async fn observe(registry: &OwnerResourceRegistry, home: &Path, id: &str) -> OwnerOctosProfile {
        registry
            .observe_octos_profile(ProfileBinding::observe(home, id, expires()).unwrap())
            .await
            .unwrap()
    }
    async fn approve(
        registry: &OwnerResourceRegistry,
        proof: OwnerOctosProfile,
        command: &str,
        budget: u64,
    ) -> OwnerResourceReceipt {
        registry
            .prepare_octos_enrollment(authority(), command.into(), proof, budget, expires())
            .unwrap()
            .approve()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn octos_account_enrollment_recovers_only_the_original_owner_request() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        fs::create_dir(home.join("profiles")).unwrap();
        write_profile(&home, "coding", "local-model", "fixture-secret");
        let registry = OwnerResourceRegistry::open_root(&home.join("ledger")).unwrap();
        let input = NamespaceEnrollment {
            command_id: "original".into(),
            account: crate::management::AccountModelQuery {
                account_id: "account_octos_coding".into(),
                expected_revision: "a".repeat(64),
            },
            model: "local-model".into(),
            reasoning: None,
            monthly_tokens: 1000,
        };
        let proof = observe(&registry, &home, "coding").await;
        let mut wrong = input.clone();
        wrong.account.account_id = "account_octos_other".into();
        assert!(
            registry
                .prepare_octos_account_enrollment(authority(), wrong, proof, expires())
                .is_err()
        );
        let original = registry
            .prepare_octos_account_enrollment(
                authority(),
                input.clone(),
                observe(&registry, &home, "coding").await,
                expires(),
            )
            .unwrap()
            .approve()
            .await
            .unwrap();
        // Recovery is receipt-only even if the local configuration has since changed.
        write_profile(&home, "coding", "other-model", "rotated-fixture");
        let recovered = registry
            .recover_octos_account_enrollment(authority(), input.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            serde_json::to_value(recovered).unwrap(),
            serde_json::to_value(original).unwrap()
        );
        let mut changed = input.clone();
        changed.monthly_tokens = 2000;
        assert!(matches!(
            registry
                .recover_octos_account_enrollment(authority(), changed)
                .await,
            Err(Error::Conflict)
        ));
        let other =
            ProfileAuthority::new("b".repeat(64), "@other:example.test".into(), expires()).unwrap();
        assert!(matches!(
            registry
                .recover_octos_account_enrollment(other, input)
                .await,
            Err(Error::LocalAuthority)
        ));
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn octos_account_lookup_is_read_only_and_scoped_to_the_owner_and_profile() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        fs::create_dir(home.join("profiles")).unwrap();
        write_profile(&home, "first", "local-model", "one");
        write_profile(&home, "second", "local-model", "two");
        let ledger = home.join("ledger");
        let registry = OwnerResourceRegistry::open_root(&ledger).unwrap();
        let binding = || ProfileBinding::observe(&home, "first", expires()).unwrap();
        assert!(
            registry
                .find_octos_account_resource(authority(), binding())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !ledger.join(KEY_FILE).exists(),
            "account discovery must not create a billing key"
        );
        let first = approve(
            &registry,
            observe(&registry, &home, "first").await,
            "one",
            1000,
        )
        .await;
        assert_eq!(
            registry
                .find_octos_account_resource(authority(), binding())
                .await
                .unwrap(),
            Some(first.resource_id)
        );
        let other =
            ProfileAuthority::new("b".repeat(64), "@other:example.test".into(), expires()).unwrap();
        assert!(
            registry
                .find_octos_account_resource(other, binding())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            registry
                .find_octos_account_resource(
                    authority(),
                    ProfileBinding::observe(&home, "second", expires()).unwrap()
                )
                .await
                .unwrap()
                .is_none()
        );
        fs::remove_file(ledger.join(KEY_FILE)).unwrap();
        assert!(
            registry
                .find_octos_account_resource(authority(), binding())
                .await
                .is_err(),
            "missing key is not an empty new budget"
        );
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn owner_octos_profiles_keep_separate_stable_budgets_and_recheck_edits() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        fs::create_dir(home.join("profiles")).unwrap();
        write_profile(&home, "first", "local-model", "fixture-secret-one");
        write_profile(&home, "second", "local-model", "fixture-secret-two");
        let registry = OwnerResourceRegistry::open_root(&home.join("ledger")).unwrap();
        let first = observe(&registry, &home, "first").await;
        let first_key = first.digest.clone();
        let first_version = first.version.clone();
        let pending = registry
            .prepare_octos_enrollment(authority(), "stale".into(), first, 1000, expires())
            .unwrap();
        write_profile(&home, "first", "local-model", "fixture-secret-rotated");
        assert!(matches!(
            pending.approve().await,
            Err(Error::LocalAuthority)
        ));
        let updated = observe(&registry, &home, "first").await;
        assert_eq!(updated.digest, first_key);
        assert_ne!(updated.version, first_version);
        let enrolled = approve(&registry, updated, "first", 1000).await;
        let second = observe(&registry, &home, "second").await;
        assert_ne!(first_key, second.digest);
        let other = approve(&registry, second, "second", 2000).await;
        assert_ne!(enrolled.resource_id, other.resource_id);
        let resource_id = enrolled.resource_id.clone();
        let revision = registry.0.store.call(4096, move |db| {
            crate::resource_publication_revision(&read_resource(&db.db, &resource_id)?)
        }).await.unwrap();
        assert_eq!(registry.octos_profile_for_resource(authority(), enrolled.resource_id.clone(), revision.clone()).await.unwrap(), "first");
        assert!(registry.octos_profile_for_resource(authority(), enrolled.resource_id.clone(), "b".repeat(64)).await.is_err());
        let other_owner = ProfileAuthority::new("b".repeat(64), "@other:example.test".into(), expires()).unwrap();
        assert!(registry.octos_profile_for_resource(other_owner, enrolled.resource_id.clone(), revision).await.is_err());
        let verified = registry
            .verify_octos_resource(
                authority(),
                enrolled.resource_id.clone(),
                observe(&registry, &home, "first").await,
            )
            .await
            .unwrap();
        let running = verified.octos_running_guard().unwrap();
        assert!(running.check().is_ok());
        let key_path = home.join("ledger").join(KEY_FILE);
        let key = crate::private::read_secret(&key_path).unwrap();
        crate::private::replace(&key_path, &[9; 32]).unwrap();
        assert!(running.check().is_err());
        crate::private::replace(&key_path, &key).unwrap();
        assert!(running.check().is_ok());
        assert!(verified.binding_digest().is_ok());
        assert!(
            registry
                .verify_octos_resource(
                    authority(),
                    enrolled.resource_id.clone(),
                    observe(&registry, &home, "second").await,
                )
                .await
                .is_err()
        );
        write_profile(&home, "first", "local-model", "another-credential");
        assert!(running.check().is_err());
        assert!(verified.binding_digest().is_err());
        assert!(
            registry
                .verify_octos_resource(
                    authority(),
                    enrolled.resource_id.clone(),
                    observe(&registry, &home, "first").await,
                )
                .await
                .is_err(),
            "a changed configuration needs fresh owner consent"
        );
        let fresh = observe(&registry, &home, "first").await;
        let again = approve(&registry, fresh, "reconfirm", 1000).await;
        assert_eq!(again.resource_id, enrolled.resource_id);
        let verified_after = registry
            .verify_octos_resource(
                authority(),
                enrolled.resource_id.clone(),
                observe(&registry, &home, "first").await,
            )
            .await
            .unwrap();
        fs::write(
            home.join("profile-defaults.json"),
            b"{\"env_vars\":{\"TOKEN\":\"fixture\"}}",
        )
        .unwrap();
        assert!(verified_after.binding_digest().is_err());
        let defaults_changed = observe(&registry, &home, "first").await;
        assert_eq!(defaults_changed.digest, first_key);
        assert!(
            registry
                .verify_octos_resource(authority(), enrolled.resource_id.clone(), defaults_changed,)
                .await
                .is_err()
        );
        let reconfirmed = approve(
            &registry,
            observe(&registry, &home, "first").await,
            "defaults_reconfirm",
            1000,
        )
        .await;
        assert_eq!(reconfirmed.resource_id, enrolled.resource_id);
        let mismatched_budget = observe(&registry, &home, "first").await;
        let pending = registry
            .prepare_octos_enrollment(
                authority(),
                "different_budget".into(),
                mismatched_budget,
                5000,
                expires(),
            )
            .unwrap();
        assert!(matches!(pending.approve().await, Err(Error::Conflict)));
        write_profile(&home, "first", "other-model", "another-credential");
        let changed_model = observe(&registry, &home, "first").await;
        assert_eq!(changed_model.digest, first_key);
        let pending = registry
            .prepare_octos_enrollment(
                authority(),
                "different_model".into(),
                changed_model,
                1000,
                expires(),
            )
            .unwrap();
        let changed = pending.approve().await.unwrap();
        assert_ne!(changed.resource_id, enrolled.resource_id);
        let (accounts, kind, seats, snapshot) = registry
            .0
            .store
            .call(4096, move |db| {
                let accounts: u64 =
                    db.db
                        .query_row("SELECT COUNT(*) FROM owner_octos_profiles", [], |r| {
                            r.get(0)
                        })?;
                let kind: String = db.db.query_row(
                    "SELECT identity_kind FROM owner_resource_accounts WHERE provider_digest=?1",
                    [first_key],
                    |r| r.get(0),
                )?;
                let seats: u64 = db
                    .db
                    .query_row("SELECT COUNT(*) FROM seats", [], |r| r.get(0))?;
                let snapshot: String =
                    db.db
                        .query_row("SELECT group_concat(config) FROM resources", [], |r| {
                            r.get(0)
                        })?;
                Ok((accounts, kind, seats, snapshot))
            })
            .await
            .unwrap();
        assert_eq!(
            (accounts, kind.as_str(), seats),
            (2, "explicit_owner_mapping", 2)
        );
        assert!(!snapshot.contains("credential") && !snapshot.contains("fixture-secret"));
        assert!(!snapshot.contains(home.to_str().unwrap()));
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_lost_or_replaced_key_never_reinitializes_a_profile_budget() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        fs::create_dir(home.join("profiles")).unwrap();
        write_profile(&home, "first", "local-model", "fixture-secret");
        let root = home.join("ledger");
        let registry = OwnerResourceRegistry::open_root(&root).unwrap();
        let first = observe(&registry, &home, "first").await;
        let key = first.digest.clone();
        let pending = registry
            .prepare_octos_enrollment(authority(), "original".into(), first, 1000, expires())
            .unwrap();
        let saved = crate::private::read_secret(&root.join(KEY_FILE)).unwrap();
        crate::private::replace(&root.join(KEY_FILE), &[9; 32]).unwrap();
        assert!(pending.approve().await.is_err());
        assert!(
            registry
                .observe_octos_profile(ProfileBinding::observe(&home, "first", expires()).unwrap())
                .await
                .is_err()
        );
        crate::private::replace(&root.join(KEY_FILE), &saved).unwrap();
        let restored = observe(&registry, &home, "first").await;
        assert_eq!(restored.digest, key);
        approve(&registry, restored, "enroll", 1000).await;
        registry.shutdown().await.unwrap();
        drop(registry);
        let registry = OwnerResourceRegistry::open_root(&root).unwrap();
        assert_eq!(observe(&registry, &home, "first").await.digest, key);
        fs::remove_file(root.join(KEY_FILE)).unwrap();
        assert!(
            registry
                .observe_octos_profile(ProfileBinding::observe(&home, "first", expires()).unwrap())
                .await
                .is_err()
        );
        assert!(!root.join(KEY_FILE).exists());
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn configured_models_have_distinct_resources_but_one_shared_profile_budget() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        fs::create_dir(home.join("profiles")).unwrap();
        let file = home.join("profiles/coding.json");
        fs::write(&file, json!({"id":"coding", "config":{"llm":{
            "primary":{"family_id":"local","model_id":"qwen"},
            "fallbacks":[{"family_id":"local","model_id":"deepseek"}]},
            "env_vars":{"TOKEN":"synthetic"}}}).to_string()).unwrap();
        let registry = OwnerResourceRegistry::open_root(&home.join("ledger")).unwrap();
        let first = approve(&registry, observe(&registry, &home, "coding").await, "first_model", 1000).await;
        let select = || ProfileBinding::observe_model(&home, "coding", "deepseek", expires()).unwrap();
        let invalid = registry.observe_octos_profile(select()).await.unwrap();
        let pending = registry.prepare_octos_enrollment(authority(), "too_much".into(), invalid, 1001, expires()).unwrap();
        assert!(matches!(pending.approve().await, Err(Error::NoCeiling)));
        let second = approve(&registry, registry.observe_octos_profile(select()).await.unwrap(), "second_model", 1000).await;
        assert_ne!(first.resource_id, second.resource_id);
        let (models, quota) = registry.octos_account_overview(authority(), select()).await.unwrap();
        assert_eq!(quota, Some(1000));
        assert_eq!(models.len(), 2);
        assert!(models.contains(&(String::from("qwen"), first.resource_id.clone())));
        assert!(models.contains(&(String::from("deepseek"), second.resource_id.clone())));
        let reconfirm = approve(&registry, registry.observe_octos_profile(select()).await.unwrap(), "same_model", 1000).await;
        assert_eq!(reconfirm.resource_id, second.resource_id);
        assert!(registry.verify_octos_resource(authority(), first.resource_id.clone(), registry.observe_octos_profile(select()).await.unwrap()).await.is_err());
        assert!(registry.verify_octos_resource(authority(), second.resource_id.clone(), registry.observe_octos_profile(select()).await.unwrap()).await.is_ok());

        // Exercise the real account-capacity calculation: a grant against
        // one pool reduces the other model's available shared seat capacity.
        let first_id = first.resource_id.clone();
        let second_id = second.resource_id.clone();
        registry.0.store.call(4096, move |db| {
            let tx = db.db.transaction()?;
            let first = read_resource(&tx, &first_id)?;
            let second = read_resource(&tx, &second_id)?;
            assert_eq!(first.seat_id, second.seat_id);
            let at = 1_793_493_000_000;
            let calendar = hagency_core::recurring_quota::Calendar {
                cadence: hagency_core::recurring_quota::Cadence::Monthly, time_zone: "UTC".into(),
            };
            crate::domain::resource_ceiling::bind_calendar(&tx, &first.id(), &"a".repeat(64), "shared_first", Some(&calendar), at)?;
            crate::domain::check_grant(&tx, &first, "shared_first", 600, at)?;
            let vector: serde_json::Value = serde_json::from_str(include_str!("capacity/managed_agents/pool-agent-v1.json"))?;
            let mut receipt: NativeGrantReceipt = serde_json::from_value(vector["command"]["poolGrant"].clone())?;
            let period = calendar.period_at(at).unwrap();
            receipt.version = 4; receipt.grant_id = "shared_first".into();
            receipt.resource_id = first.id(); receipt.resource_revision = crate::resource_publication_revision(&first)?;
            receipt.allocated_tokens = 600; receipt.admission_request_id = Some("admission_shared_first".into());
            receipt.calendar = Some(calendar); receipt.period_key = period.key;
            receipt.period_start_ms = period.start_ms; receipt.period_end_ms = period.end_ms;
            assert!(receipt.supported_shape());
            tx.execute("INSERT INTO owner_capacity_grants(id,profile,resource_id,preset_id,seat_id,allocated_tokens,receipt,active,accounting_mode) VALUES(?1,?2,?3,?4,?5,?6,?7,1,'reservation_only_v1')",
                params!["shared_first", "a".repeat(64), first.id(), first.preset_id, first.seat_id, 600, serde_json::to_string(&receipt)?])?;
            assert!(crate::domain::check_grant(&tx, &second, "shared_second", 400, at).is_ok());
            assert!(matches!(crate::domain::check_grant(&tx, &second, "shared_second", 401, at), Err(Error::InsufficientCapacity)));
            Ok(())
        }).await.unwrap();
        let id = second.resource_id.clone();
        registry.0.store.call(4096, move |db| {
            let resource = read_resource(&db.db, &id)?;
            let revision = crate::resource_publication_revision(&resource)?;
            db.db.execute("INSERT INTO owner_resource_ceiling_amendments(resource_id,sequence,profile,command_id,base_revision,previous_tokens,tokens,period,created_at_ms) VALUES(?1,1,?2,'model_cap_amendment',?3,1000,900,'monthly',1)", params![id, "a".repeat(64), revision])?;
            Ok(())
        }).await.unwrap();
        let reconfirmed = approve(&registry, registry.observe_octos_profile(select()).await.unwrap(), "reconfirm_amended", 900).await;
        assert_eq!(reconfirmed.resource_id, second.resource_id);
        registry.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn single_model_schema_upgrade_retains_existing_resource_and_legacy_binding() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        fs::create_dir(home.join("profiles")).unwrap();
        write_profile(&home, "coding", "qwen", "synthetic");
        let root = home.join("ledger");
        let registry = OwnerResourceRegistry::open_root(&root).unwrap();
        let proof = observe(&registry, &home, "coding").await;
        let legacy = proof.legacy_version.clone();
        let digest = proof.digest.clone();
        let enrolled = approve(&registry, proof, "legacy_model", 1000).await;
        registry.shutdown().await.unwrap();
        drop(registry);
        {
            let db = Connection::open(root.join("domain.sqlite3")).unwrap();
            db.execute("UPDATE owner_octos_profiles SET configuration_digest=?1 WHERE profile_digest=?2", params![legacy, digest]).unwrap();
            db.execute_batch("DROP TABLE owner_octos_model_resources; PRAGMA user_version=112;").unwrap();
        }
        let registry = OwnerResourceRegistry::open_root(&root).unwrap();
        assert!(registry.verify_octos_resource(authority(), enrolled.resource_id.clone(), observe(&registry, &home, "coding").await).await.is_ok());
        let (models, quota) = registry.octos_account_overview(authority(), ProfileBinding::observe(&home, "coding", expires()).unwrap()).await.unwrap();
        assert_eq!(models, vec![("qwen".into(), enrolled.resource_id)]);
        assert_eq!(quota, Some(1000));
        registry.shutdown().await.unwrap();
    }
}
