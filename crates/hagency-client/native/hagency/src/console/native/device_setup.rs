//! Native current-profile initialization. No source import or saved migration path.
use super::*;
use hagency_store::{
    management::Permission,
    owner_resources::{
        OwnerResourceRegistry, ProfileAuthority, adoption::ProfileInitializationAuthority,
    },
};
use std::time::{Duration, Instant};
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeviceSetupContext {
    pub server: String,
    pub account: String,
    pub initialization_id: Option<String>,
}
pub struct DeviceSetupReview {
    owner: NativeOwner,
    context: DeviceSetupContext,
    device: String,
    expires: Instant,
    generation: u64,
}
impl DeviceSetupReview {
    pub fn context(&self) -> &DeviceSetupContext {
        &self.context
    }
    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }
}
#[cfg(any(test, feature = "test-support"))]
static FIXTURE_INSTALLATION: StdMutex<Option<std::path::PathBuf>> = StdMutex::new(None);
/// Preserve explicit development startup selection when installing its service.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn resource_activation_fixture_root() -> Option<std::path::PathBuf> {
    FIXTURE_INSTALLATION
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}
/// Explicit test binary startup only. Never accepts environment configuration.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn initialize_resource_activation_fixture(root: &Path) -> Result<(), NativeError> {
    OwnerResourceRegistry::ensure_fixture_startup()
        .map_err(|_| error("fixture_authority_already_open"))?;
    let mut saved = FIXTURE_INSTALLATION
        .lock()
        .map_err(|_| error("fixture_root_unavailable"))?;
    hagency_store::private::directory(root).map_err(|_| error("fixture_root_unavailable"))?;
    let root = root
        .canonicalize()
        .map_err(|_| error("fixture_root_unavailable"))?;
    if saved.as_ref().is_some_and(|old| old != &root) {
        return Err(error("fixture_root_changed"));
    }
    let authority = root.join("authority");
    hagency_store::private::directory(&authority).map_err(|_| error("fixture_root_unavailable"))?;
    hagency_platform::legacy_writer::test_support::initialize(&authority)
        .map_err(|_| error("fixture_authority_already_open"))?;
    *saved = Some(root);
    Ok(())
}
pub(super) fn activation_registry(
    existing: bool,
) -> Result<OwnerResourceRegistry, hagency_store::Error> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = FIXTURE_INSTALLATION
        .lock()
        .map_err(|_| hagency_store::Error::Unavailable)?
        .clone()
    {
        let root = root.join("canonical");
        if existing {
            hagency_store::private::open(&root.join("domain.sqlite3"), false)?;
        }
        return OwnerResourceRegistry::isolated_for_tests(&root);
    }
    if existing {
        OwnerResourceRegistry::open_existing()
    } else {
        OwnerResourceRegistry::open()
    }
}
/// Deletes the canonical resource database an earlier build left, unless it
/// is open or a profile of this build uses it. A test build never reaches the
/// OS owner's own database: without a fixture root it refuses.
pub(super) fn discard_previous_registry() -> Result<bool, hagency_store::Error> {
    #[cfg(any(test, feature = "test-support"))]
    {
        let root = FIXTURE_INSTALLATION
            .lock()
            .map_err(|_| hagency_store::Error::Unavailable)?
            .clone()
            .ok_or(hagency_store::Error::Unavailable)?;
        OwnerResourceRegistry::discard_previous_build_for_tests(&root.join("canonical"))
    }
    #[cfg(not(any(test, feature = "test-support")))]
    OwnerResourceRegistry::discard_previous_build()
}
fn error(code: &str) -> NativeError {
    failure(409, code)
}
fn package_copy(p: &ManagementPackage) -> ManagementPackage {
    ManagementPackage {
        digest: p.digest.clone(),
        instance: p.instance,
        account_epoch: p.account_epoch,
        permissions: p.permissions.clone(),
    }
}
pub(super) async fn source(
    owner: &NativeOwner,
) -> Result<crate::console::AuthorizedDevice, NativeError> {
    // Every setup/open checks the current SDK account and execution device.
    owner.sync_matrix_source().await?;
    let cookie = owner.0.cookie.lock().await.clone();
    let raw = cookie
        .strip_prefix("hagency_console=")
        .ok_or_else(|| error("owner_authorization_required"))?;
    owner
        .0
        .host
        .console
        .0
        .server_login
        .check_matrix_source(raw)
        .await
        .map_err(|_| error("management_scope_changed"))?;
    let device = owner
        .0
        .host
        .console
        .authorized_device()
        .await
        .map_err(|_| error("owner_authorization_required"))?;
    if device.bearer().is_err()
        || device.origin() != owner.0.expected_origin
        || device.owner_mxid() != owner.0.expected_owner
        || owner
            .0
            .configuration_stopped
            .load(std::sync::atomic::Ordering::Acquire)
    {
        return Err(error("management_scope_changed"));
    }
    owner
        .0
        .host
        .check_state_lock()
        .map_err(|_| error("current_profile_root_changed"))?;
    Ok(device)
}

fn require_package(package: &ManagementPackage) -> Result<(), NativeError> {
    if !package.permissions.contains(&Permission::ReadResources)
        || !package
            .permissions
            .contains(&Permission::PrepareSourceQuiescence)
    {
        return Err(error("current_profile_permission_required"));
    }
    Ok(())
}
fn profile(device: &crate::console::AuthorizedDevice) -> Result<String, NativeError> {
    crate::console::owned_agents::profile_identity(
        device.origin(),
        device.issuer(),
        device.subject(),
        device.owner_mxid(),
    )
    .map_err(|_| error("management_scope_changed"))
}
pub(super) async fn same_device(
    owner: &NativeOwner,
    device: &crate::console::AuthorizedDevice,
) -> Result<(), NativeError> {
    let after = source(owner).await?;
    if crate::console::device_execution::DeviceIdentity::from(&after)
        != crate::console::device_execution::DeviceIdentity::from(device)
    {
        return Err(error("management_scope_changed"));
    }
    Ok(())
}
impl CurrentProfileSelection {
    pub async fn device_setup_phase(
        &self,
        owner: &NativeOwner,
    ) -> Result<&'static str, NativeError> {
        if !self.matches_host(
            owner.0.host.state_directory(),
            &owner.0.expected_origin,
            &owner.0.expected_owner,
        ) {
            return Err(error("current_profile_scope_changed"));
        }
        owner
            .validate_current_profile(&self.registry, &self.initialization_id)
            .await?;
        Ok("active")
    }
}
impl NativeOwner {
    pub async fn device_setup_context(
        &self,
        package: ManagementPackage,
        selection: Option<&CurrentProfileSelection>,
    ) -> Result<DeviceSetupContext, NativeError> {
        require_package(&package)?;
        let id = if let Some(selected) = selection {
            selected.device_setup_phase(self).await?;
            Some(selected.initialization_id.clone())
        } else {
            source(self).await?;
            let manager = self.management(package).await?;
            manager.current_setup_journal().await?;
            manager.retire();
            None
        };
        Ok(DeviceSetupContext {
            server: self.0.expected_origin.clone(),
            account: self.0.expected_owner.clone(),
            initialization_id: id,
        })
    }
    pub async fn review_device_setup(
        &self,
        package: ManagementPackage,
        selection: Option<CurrentProfileSelection>,
    ) -> Result<DeviceSetupReview, NativeError> {
        let context = self
            .device_setup_context(package_copy(&package), selection.as_ref())
            .await?;
        if selection.is_some() {
            return Err(error("current_profile_already_active"));
        }
        let device = source(self).await?;
        Ok(DeviceSetupReview {
            owner: self.clone(),
            context,
            device: device.device_id().into(),
            expires: Instant::now() + Duration::from_secs(120),
            generation: device.generation(),
        })
    }
    pub async fn confirm_device_setup(
        &self,
        review: DeviceSetupReview,
        package: ManagementPackage,
        native_gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<CurrentProfileSelection, NativeError> {
        require_package(&package)?;
        let gate = Arc::new(native_gate);
        if !Arc::ptr_eq(&self.0, &review.owner.0) || Instant::now() >= review.expires || !gate() {
            return Err(error("current_profile_review_expired"));
        }
        self.0
            .host
            .check_state_lock()
            .map_err(|_| error("current_profile_root_changed"))?;
        let device = source(self).await?;
        if device.device_id() != review.device || device.generation() != review.generation {
            return Err(error("management_scope_changed"));
        }
        let manager = self.management(package).await?;
        let journal = manager.current_setup_journal().await?;
        let registry = tokio::task::spawn_blocking(|| activation_registry(false))
            .await
            .map_err(|_| error("current_profile_registry_unavailable"))?
            .map_err(management::store_error)?;
        let profile = profile(&device)?;
        let digest=hagency_core::canonical::digest(&serde_json::json!({"kind":"current_profile","registry":registry.id(),"profile":profile}))
            .map_err(|_|error("current_profile_registry_unavailable"))?;
        let id = format!("current_{digest}");
        let weak = Arc::downgrade(&self.0);
        let until = review.expires.min(Instant::now() + Duration::from_secs(30));
        let live = gate.clone();
        let authority = ProfileInitializationAuthority::new(
            registry.clone(),
            ProfileAuthority::new(profile.clone(), device.owner_mxid().into(), until)
                .map_err(management::store_error)?,
            until,
            move || {
                live()
                    && weak.upgrade().is_some_and(|owner| {
                        !owner
                            .configuration_stopped
                            .load(std::sync::atomic::Ordering::Acquire)
                            && owner.host.check_state_lock().is_ok()
                    })
            },
        )
        .map_err(management::store_error)?;
        registry
            .initialize_current_profile(authority, id.clone(), journal)
            .await
            .map_err(management::store_error)?;
        same_device(self, &device).await?;
        if !gate() || Instant::now() >= review.expires {
            return Err(error("management_outcome_unknown"));
        }
        let selected = CurrentProfileSelection::completed(
            self,
            registry,
            id,
            profile,
            device.device_id().into(),
        );
        selected.persist(self)?;
        manager.retire();
        Ok(selected)
    }
}
#[cfg(any(test, feature = "test-support"))]
impl NativeOwner {
    /// Synthetic test fixture bootstrap only. The authenticated state must be
    /// inside the explicit installation fixture root. Never overwrites, opens,
    /// recovers or changes an existing personal database.
    #[doc(hidden)]
    pub async fn seed_empty_personal_activation_fixture(&self) -> Result<bool, NativeError> {
        let root = FIXTURE_INSTALLATION
            .lock()
            .map_err(|_| error("fixture_root_unavailable"))?
            .clone()
            .ok_or_else(|| error("fixture_root_unavailable"))?;
        let state = self
            .0
            .host
            .state_directory()
            .canonicalize()
            .map_err(|_| error("fixture_root_unavailable"))?;
        if !state.starts_with(&root) || self.0.current_profile.get().is_some() {
            return Err(error("fixture_profile_outside_root"));
        }
        let device = source(self).await?;
        let origin = device.origin().to_owned();
        let issuer = device.issuer().to_owned();
        let subject = device.subject().to_owned();
        let account = device.owner_mxid().to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let directory = crate::console::owned_agents::journal_profile_directory(
                &state, &origin, &issuer, &subject, &account,
            )
            .map_err(|_| error("fixture_profile_unavailable"))?;
            let path = directory.join("agent-local.sqlite");
            if std::fs::symlink_metadata(&path).is_ok() {
                return Ok(false);
            }
            let profile = crate::console::owned_agents::profile_identity(
                &origin, &issuer, &subject, &account,
            )
            .map_err(|_| error("fixture_profile_unavailable"))?;
            // Create-new is required: a concurrent legitimate initializer may
            // have published a database since the absence check above.
            hagency_store::private::write_new(&path, b"")
                .map_err(|_| error("fixture_personal_create_failed"))?;
            drop(
                hagency_agent_local::Ledger::open_scoped(&path, &account, &profile)
                    .map_err(|_| error("fixture_personal_create_failed"))?,
            );
            Ok(true)
        })
        .await
        .map_err(|_| error("fixture_personal_create_failed"))??;
        source(self).await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;
