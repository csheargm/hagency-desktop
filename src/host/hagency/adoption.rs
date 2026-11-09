//! Native coordinator handoff into the existing Resources and grant UI.
use super::*;
use ::hagency::daemon::bridge::CurrentSelection as CurrentProfileSelection;

impl Host {
    /// Native-only: a mini-app cannot manufacture this sealed profile selection.
    pub(crate) async fn accept_current_profile(
        &self,
        lease: &Lease,
        selection: CurrentProfileSelection,
    ) -> Result<(), String> {
        let account = self.check(lease)?;
        lease.authorize(&account, REVIEW, None)?;
        let _lane = self.0.initialising.lock().await;
        self.check(lease)?;
        let service = crate::hagency::backend::Service::for_current()?;
        service.select_current_profile(selection).await?;
        self.clear_setup_caches()?;
        if lease
            .authorize(&account, grants::REVIEW_GRANT, None)
            .is_ok()
        {
            self.attach_current_grant_access(lease, &service).await
        } else {
            let manager = service
                .management_until(ManagementPackage {
                    digest: self.0.digest.clone(),
                    instance: self.0.identity.generation,
                    account_epoch: self.0.epoch + 1,
                    permissions: vec![ManagementPermission::ReadResources],
                }, lease.expires())
                .await?;
            let result = manager
                .resources(Default::default())
                .await
                .map_err(|e| e.code);
            manager.retire();
            self.check(lease)?;
            result.map(|_| ())
        }
    }
    /// A failed native confirmation retires its source owner. Recovery must
    /// release the old package's Arc before Service reacquires that OS lock.
    pub(super) fn evict_retired_setup_caches(&self) -> Result<(), String> {
        let mut manager = self
            .0
            .manager
            .lock()
            .map_err(|_| "management_unavailable")?;
        let mut access = self
            .0
            .grant_access
            .lock()
            .map_err(|_| "management_unavailable")?;
        if manager.as_ref().is_some_and(|v| v.is_retired()) {
            manager.take();
        }
        if access.as_ref().is_some_and(|v| v.retired()) {
            access.take();
        }
        Ok(())
    }
    fn clear_setup_caches(&self) -> Result<(), String> {
        if let Some(access) = self
            .0
            .grant_access
            .lock()
            .map_err(|_| "management_unavailable")?
            .take()
        {
            access.retire();
        }
        if let Some(manager) = self
            .0
            .manager
            .lock()
            .map_err(|_| "management_unavailable")?
            .take()
        {
            manager.retire();
        }
        Ok(())
    }
    async fn attach_current_grant_access(
        &self,
        lease: &Lease,
        service: &crate::hagency::backend::Service,
    ) -> Result<(), String> {
        let package = || ManagementPackage {
            digest: self.0.digest.clone(),
            instance: self.0.identity.generation,
            account_epoch: self.0.epoch + 1,
            permissions: permissions(lease),
        };
        let manager = service.management_until(package(), lease.expires()).await?;
        // Canonical inventory must actually be readable before enabling consent.
        if let Err(error) = manager.resources(Default::default()).await {
            manager.retire();
            return Err(error.code);
        }
        let access = match service.current_grant_access_until(package(), lease.expires()).await {
            Ok(access) => access,
            Err(error) => {
                manager.retire();
                return Err(error);
            }
        };
        if let Err(error) = self.check(lease) {
            access.retire();
            manager.retire();
            return Err(error);
        }
        let mut cached = self
            .0
            .manager
            .lock()
            .map_err(|_| "management_unavailable")?;
        let mut grants = self
            .0
            .grant_access
            .lock()
            .map_err(|_| "management_unavailable")?;
        if let Err(error) = self.check(lease) {
            access.retire();
            manager.retire();
            return Err(error);
        }
        if manager.is_retired() || access.retired() {
            access.retire();
            manager.retire();
            return Err("management_scope_changed".into());
        }
        if let Some(old) = cached.replace(manager) {
            old.retire();
        }
        if let Some(old) = grants.replace(access) {
            old.retire();
        }
        Ok(())
    }
}

pub(crate) use ::hagency::daemon::bridge::DeviceSetupReview;

pub(crate) const STATUS: &str = "hagency.onboarding.status";
pub(crate) const REVIEW: &str = "hagency.onboarding.review";
pub(crate) use ::hagency::native_owner::DeviceSetupContext;

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceSetupProgress {
    pub state: String,
    pub initialization_id: Option<String>,
    pub reason: Option<String>,
}
pub(crate) enum DeviceSetupResult {
    Review(Box<DeviceSetupReview>),
    Progress(DeviceSetupProgress),
}
#[derive(Clone)]
pub(crate) struct DeviceSetupFlow {
    host: Host,
    lease: Lease,
}
impl DeviceSetupFlow {
    fn check(&self) -> Result<(), String> {
        let account = self.host.check(&self.lease)?;
        self.lease.authorize(&account, REVIEW, None)
    }
    pub fn account(&self) -> &str {
        &self.lease.identity().account
    }
    pub fn active(&self) -> bool {
        self.check().is_ok()
    }
    fn package(&self) -> ManagementPackage {
        ManagementPackage {
            digest: self.host.0.digest.clone(),
            instance: self.host.0.identity.generation,
            account_epoch: self.host.0.epoch + 1,
            permissions: vec![
                ManagementPermission::ReadResources,
                ManagementPermission::ReadProviders,
                ManagementPermission::ConnectProviders,
                ManagementPermission::PrepareSourceQuiescence,
            ],
        }
    }
    pub async fn open(&self) -> Result<DeviceSetupResult, String> {
        self.check()?;
        self.host.evict_retired_setup_caches()?;
        let service = crate::hagency::backend::Service::for_current()?;
        if let Some(selection) = service.recover_activation_task().await? {
            self.host
                .accept_current_profile(&self.lease, selection)
                .await?;
            service.activation_handoff_complete().await;
        }
        self.check()?;
        if service.activation_state().await? == "active" {
            let manager = self.host.manager(&self.lease).await?;
            manager
                .resources(Default::default())
                .await
                .map_err(|e| e.code)?;
            self.check()?;
            return Ok(DeviceSetupResult::Progress(DeviceSetupProgress {
                state: "active".into(),
                initialization_id: None,
                reason: None,
            }));
        }
        let review = service.activation_review(self.package(), self.lease.expires()).await?;
        self.check()?;
        Ok(DeviceSetupResult::Review(Box::new(review)))
    }
    pub async fn confirm(&self, review: DeviceSetupReview) -> Result<DeviceSetupResult, String> {
        self.check()?;
        if review.expired() {
            return Err("current_profile_review_expired".into());
        }
        let service = crate::hagency::backend::Service::for_current()?;
        let weak = Arc::downgrade(&self.host.0);
        let lease = self.lease.clone();
        let result = service
            .confirm_activation(
                self.package(),
                review,
                Arc::new(move || {
                    weak.upgrade()
                        .is_some_and(|h| Host(h).check(&lease).is_ok())
                }),
            )
            .await;
        self.host.evict_retired_setup_caches()?;
        let selection = result?;
        self.check()?;
        self.host
            .accept_current_profile(&self.lease, selection)
            .await?;
        service.activation_handoff_complete().await;
        self.check()?;
        Ok(DeviceSetupResult::Progress(DeviceSetupProgress {
            state: "active".into(),
            initialization_id: None,
            reason: None,
        }))
    }
}
impl Host {
    pub(crate) fn onboarding_handoff(
        &self,
        lease: Lease,
        args: Value,
    ) -> Result<DeviceSetupFlow, String> {
        let _: Empty = parse(args)?;
        let flow = DeviceSetupFlow {
            host: self.clone(),
            lease,
        };
        flow.check()?;
        Ok(flow)
    }
    pub(crate) async fn onboarding_status(&self, lease: &Lease) -> Result<Value, String> {
        let account = self.check(lease)?;
        lease.authorize(&account, STATUS, None)?;
        self.evict_retired_setup_caches()?;
        let service = crate::hagency::backend::Service::for_current()?;
        let state = service.activation_state().await?;
        self.check(lease)?;
        let can_review = state != "active";
        let mut available = false;
        let mut intents = Vec::new();
        if state == "active" {
            self.manager(lease).await?;
            let access = self
                .0
                .grant_access
                .lock()
                .map_err(|_| "management_unavailable")?
                .clone();
            if let Some(access) = access {
                for item in access.list_existing().await.map_err(|e| e.code)? {
                    intents.push(json!({"intentId":item.intent_id,"contributionId":item.contribution_id,"resourceId":item.resource_id,"state":"saved"}));
                }
                available = !access.retired();
            }
        }
        self.check(lease)?;
        rinx_miniapp_core::bounded_reply(
            json!({"state":state,"canReview":can_review,"grantReviewAvailable":available,"grantIntents":intents}),
        )
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn onboarding_contract_has_no_script_consent_or_identity_fields() {
        for value in [
            json!({"tokens":700}),
            json!({"confirm":true}),
            json!({"migrationId":"other"}),
            json!({"path":"/tmp"}),
            json!({"provider":"codex"}),
        ] {
            assert!(parse::<Empty>(value).is_err());
        }
        assert!(parse::<Empty>(json!({})).is_ok());
    }
}
