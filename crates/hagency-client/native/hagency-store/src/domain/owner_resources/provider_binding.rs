//! Read-only binding from a fresh native provider or local-profile observation
//! to canonical owner state. Octos profiles are not provider-account proofs.
use super::*;

/// Native-only, non-deserializable binding. It cannot enroll or allocate a
/// resource and carries no raw provider subject or credential. `runtime()`
/// distinguishes verified provider accounts from owner-selected Octos profiles.
pub struct VerifiedOwnerResourceBinding {
    pub(super) registry_id: String,
    pub(super) profile: ProfileAuthority,
    pub(super) resource_id: String,
    pub(super) resource_revision: String,
    runtime: String,
    seat_id: String,
    provider_digest: String,
    access_revision: u64,
    octos: Option<OwnerOctosProfile>,
    pub(super) expires: Instant,
}
impl VerifiedOwnerResourceBinding {
    pub fn resource_id(&self) -> &str {
        &self.resource_id
    }
    pub fn resource_revision(&self) -> &str {
        &self.resource_revision
    }
    pub fn runtime(&self) -> &str {
        &self.runtime
    }
    /// Derive only a revocation check for an existing process. New work still
    /// requires fresh owner/resource authority and an active execution lease.
    pub fn octos_running_guard(&self) -> Result<OctosRunningGuard, Error> {
        self.check()?;
        if self.runtime != "octos" {
            return Err(Error::LocalAuthority);
        }
        self.octos.as_ref().ok_or(Error::LocalAuthority)?.running_guard()
    }
    /// Qualify only the already-retained isolated process for this verified
    /// owner-selected resource. The installation key never leaves the store.
    /// This grants no activity lease or provider-account attestation.
    pub fn qualify_octos_process(
        &self,
        process: &mut hagency_runtime::owned::OwnedOctosSession,
    ) -> Result<hagency_runtime::owned::SharedTextProcessObservation, Error> {
        self.check()?;
        if self.runtime != "octos" { return Err(Error::LocalAuthority); }
        let observation = self.octos.as_ref().ok_or(Error::LocalAuthority)?.qualify_process(process)?;
        if self.check().is_err() {
            process.stop();
            return Err(Error::LocalAuthority);
        }
        Ok(observation)
    }
    /// A private journal commitment, never a serialized authority proof.
    pub fn binding_digest(&self) -> Result<String, Error> {
        self.check()?;
        canonical::digest(&json!({
            "registry": self.registry_id, "profile": self.profile.digest,
            "owner": self.profile.account, "resource": self.resource_id,
            "revision": self.resource_revision, "runtime": self.runtime,
            "seat": self.seat_id, "provider": self.provider_digest,
            "accessRevision": self.access_revision,
        }))
        .map_err(Into::into)
    }
    pub(super) fn check(&self) -> Result<(), Error> {
        self.profile.check()?;
        if Instant::now() >= self.expires {
            return Err(Error::LocalAuthority);
        }
        if let Some(octos) = &self.octos {
            octos.check(&self.registry_id)?;
        }
        Ok(())
    }
    pub(super) fn check_canonical(&self, db: &Connection, registry: &str) -> Result<(), Error> {
        self.check()?;
        if registry != self.registry_id {
            return Err(Error::LocalAuthority);
        }
        authorize_resource(db, &self.profile, &self.resource_id)?;
        let resource = read_resource(db, &self.resource_id)?;
        let access: u64 = db.query_row(
            "SELECT revision FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2 AND active=1",
            params![self.profile.digest,self.resource_id], |r| r.get(0),
        )?;
        let account: Option<(String,String)> = db.query_row(
            "SELECT provider_digest,identity_kind FROM owner_resource_accounts WHERE seat_id=?1",
            [&resource.seat_id], |r| Ok((r.get(0)?,r.get(1)?)),
        ).optional()?;
        if access != self.access_revision
            || resource.seat_id != self.seat_id
            || resource.framework != self.runtime
            || crate::resource_publication_revision(&resource)? != self.resource_revision
            || account != Some((self.provider_digest.clone(),
                if self.octos.is_some() { "explicit_owner_mapping" } else { "verified_provider" }.into()))
        {
            return Err(Error::LocalAuthority);
        }
        if let Some(octos) = &self.octos {
            let stored: Option<(String,String,String)> = db.query_row(
                "SELECT o.profile_id,m.model,m.configuration_digest FROM owner_octos_model_resources m JOIN owner_octos_profiles o ON o.profile_digest=m.profile_digest WHERE m.profile_digest=?1 AND m.resource_id=?2",
                params![self.provider_digest,self.resource_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
            ).optional()?;
            if self.runtime != "octos" || octos.digest() != self.provider_digest
                || resource.octos_profile.as_deref() != Some(octos.profile_id())
                || resource.model != octos.model()
                || !stored.is_some_and(|(profile, model, version)| profile == octos.profile_id()
                    && model == octos.model() && octos.matches_version(&version))
            {
                return Err(Error::LocalAuthority);
            }
        } else if self.runtime == "octos" {
            return Err(Error::LocalAuthority);
        }
        self.check()
    }
}
impl OwnerResourceRegistry {
    pub async fn verify_provider_resource(
        &self,
        profile: ProfileAuthority,
        resource_id: String,
        provider: VerifiedProviderAccount,
    ) -> Result<VerifiedOwnerResourceBinding, Error> {
        self.verify_resource_binding(profile, resource_id, provider, false)
            .await
    }
    /// A pooled Claude launch runs on whichever Claude account the owner's
    /// Claude Code is signed in to now: owners switch between several of
    /// their accounts. The observation must still be a verified Claude sign-in
    /// (provider profile and CLI agree), but it need not be the account the
    /// resource was created with. The binding keeps that recorded identity, so
    /// the resource's records, assignment pins and receipts are unchanged.
    /// Codex resources and every other caller keep the exact account check.
    pub async fn verify_claude_launch_resource(
        &self,
        profile: ProfileAuthority,
        resource_id: String,
        provider: VerifiedProviderAccount,
    ) -> Result<VerifiedOwnerResourceBinding, Error> {
        if provider.runtime != "claude" || provider.claude.is_none() {
            return Err(Error::LocalAuthority);
        }
        self.verify_resource_binding(profile, resource_id, provider, true)
            .await
    }
    async fn verify_resource_binding(
        &self,
        profile: ProfileAuthority,
        resource_id: String,
        provider: VerifiedProviderAccount,
        any_claude_account: bool,
    ) -> Result<VerifiedOwnerResourceBinding, Error> {
        profile.check()?;
        hagency_core::project::identifier(&resource_id, 128)?;
        if Instant::now() >= provider.expires || self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let fence = self.0.clone();
        let caller = profile.clone();
        let result = self.0.store.call(4096, move |db| {
            profile.check()?;
            if fence.closed.load(Ordering::Acquire) || Instant::now() >= provider.expires {
                return Err(Error::LocalAuthority);
            }
            authorize_resource(&db.db,&profile,&resource_id)?;
            let resource = read_resource(&db.db,&resource_id)?;
            let access_revision = db.db.query_row(
                "SELECT revision FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2 AND active=1",
                params![profile.digest,resource_id], |r| r.get(0),
            )?;
            let provider_digest = if any_claude_account {
                if resource.framework != "claude" {
                    return Err(Error::LocalAuthority);
                }
                db.db.query_row(
                    "SELECT provider_digest FROM owner_resource_accounts WHERE seat_id=?1 AND identity_kind='verified_provider'",
                    [&resource.seat_id], |r| r.get::<_, String>(0),
                ).optional()?.ok_or(Error::LocalAuthority)?
            } else {
                provider.digest
            };
            let result = VerifiedOwnerResourceBinding {
                registry_id: fence.id.clone(), profile, resource_id,
                resource_revision: crate::resource_publication_revision(&resource)?,
                runtime: provider.runtime, seat_id: resource.seat_id,
                provider_digest, access_revision, octos: None, expires: provider.expires,
            };
            result.check_canonical(&db.db,&fence.id)?;
            if fence.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
            Ok(result)
        }).await?;
        caller.check()?;
        result.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }

    /// Bind a fresh, owner-selected Octos profile to an existing resource.
    /// This attests only the local configuration and its recorded budget;
    /// it does not attest a provider identity or launch a process.
    pub async fn verify_octos_resource(
        &self,
        profile: ProfileAuthority,
        resource_id: String,
        octos: OwnerOctosProfile,
    ) -> Result<VerifiedOwnerResourceBinding, Error> {
        profile.check()?;
        octos.check(self.id())?;
        hagency_core::project::identifier(&resource_id, 128)?;
        if self.0.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
        let fence = self.0.clone();
        let caller = profile.clone();
        let result = self.0.store.call(4096, move |db| {
            profile.check()?;
            octos.check(&fence.id)?;
            if fence.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
            authorize_resource(&db.db,&profile,&resource_id)?;
            let resource = read_resource(&db.db,&resource_id)?;
            let access_revision = db.db.query_row(
                "SELECT revision FROM owner_resource_grants WHERE profile=?1 AND resource_id=?2 AND active=1",
                params![profile.digest,resource_id], |r| r.get(0),
            )?;
            let result = VerifiedOwnerResourceBinding {
                registry_id: fence.id.clone(), profile, resource_id,
                resource_revision: crate::resource_publication_revision(&resource)?,
                runtime: "octos".into(), seat_id: resource.seat_id,
                provider_digest: octos.digest().into(), access_revision,
                expires: octos.expires(), octos: Some(octos),
            };
            result.check_canonical(&db.db,&fence.id)?;
            if fence.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
            Ok(result)
        }).await?;
        caller.check()?;
        result.check()?;
        if self.0.closed.load(Ordering::Acquire) { return Err(Error::LocalAuthority); }
        Ok(result)
    }
}
