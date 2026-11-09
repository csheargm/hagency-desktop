//! Current profile selection. Disk records describe identity; fresh SDK and
//! owner-root authority are required on every open.
use super::*;
use hagency_store::owner_resources::{
    OwnerResourceRegistry, ProfileAuthority, adoption::ProfileInitializationAuthority,
};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    time::{Duration, Instant},
};
mod persistence;
mod allocation_executor;
pub use allocation_executor::CurrentAllocationExecutor;
mod shared_executor;
pub use shared_executor::CurrentSharedExecutor;

const INITIALIZER_FILE: &str = "adopted-management-initializer-v1.json";
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Descriptor {
    version: u8,
    registry_id: String,
    #[serde(rename = "migrationId")]
    initialization_id: String,
    origin: String,
    owner: String,
    profile: String,
    original_device: String,
    current_profile: bool,
}
fn descriptor_error() -> NativeError {
    failure(409, "current_profile_initializer_unavailable")
}
fn read_descriptor(state: &Path) -> Result<Option<Descriptor>, NativeError> {
    let path = state.join(INITIALIZER_FILE);
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(descriptor_error()),
        Ok(_) => {}
    }
    let file = hagency_store::private::open(&path, false).map_err(|_| descriptor_error())?;
    let mut bytes = Vec::new();
    file.take(4097)
        .read_to_end(&mut bytes)
        .map_err(|_| descriptor_error())?;
    if bytes.len() > 4096 {
        return Err(descriptor_error());
    }
    let value: Descriptor = serde_json::from_slice(&bytes).map_err(|_| descriptor_error())?;
    if value.version != 4
        || !value.current_profile
        || value.profile.len() != 64
        || !value.profile.bytes().all(|b| b.is_ascii_hexdigit())
        || value.registry_id.is_empty()
        || value.registry_id.len() > 128
        || value.original_device.is_empty()
        || value.original_device.len() > 128
        || key(&value.initialization_id).is_err()
    {
        return Err(descriptor_error());
    }
    Ok(Some(value))
}
#[derive(Clone)]
pub struct CurrentProfileSelection {
    pub(super) registry: OwnerResourceRegistry,
    pub(super) initialization_id: String,
    state: std::path::PathBuf,
    origin: String,
    owner: String,
    pub(super) profile: String,
    pub(super) device: String,
}
impl CurrentProfileSelection {
    fn descriptor(&self) -> Descriptor {
        Descriptor {
            version: 4,
            registry_id: self.registry.id().into(),
            initialization_id: self.initialization_id.clone(),
            origin: self.origin.clone(),
            owner: self.owner.clone(),
            profile: self.profile.clone(),
            original_device: self.device.clone(),
            current_profile: true,
        }
    }
    pub(super) fn persist(&self, owner: &NativeOwner) -> Result<(), NativeError> {
        owner
            .0
            .host
            .check_state_lock()
            .map_err(|_| descriptor_error())?;
        let descriptor = self.descriptor();
        if let Some(old) = read_descriptor(&self.state)? {
            return if old == descriptor {
                Ok(())
            } else {
                Err(descriptor_error())
            };
        }
        let bytes = serde_json::to_vec(&descriptor).map_err(|_| descriptor_error())?;
        if bytes.len() > 4096 {
            return Err(descriptor_error());
        }
        persistence::persist_new(&self.state, &bytes)
    }
    fn loaded(
        state: &Path,
        origin: &str,
        owner: &str,
        registry: OwnerResourceRegistry,
        descriptor: Descriptor,
    ) -> Result<Self, NativeError> {
        if descriptor.origin != origin
            || descriptor.owner != owner
            || descriptor.registry_id != registry.id()
        {
            return Err(descriptor_error());
        }
        Ok(Self {
            registry,
            initialization_id: descriptor.initialization_id,
            state: state.canonicalize().map_err(|_| descriptor_error())?,
            origin: origin.into(),
            owner: owner.into(),
            profile: descriptor.profile,
            device: descriptor.original_device,
        })
    }
    /// Trusted Desktop restart path. Only the OS-owner canonical registry can be
    /// selected; no descriptor field can choose a database path. The returned
    /// choice still requires fresh SDK authorization and exact active-plan checks.
    pub async fn load_for_host(
        state: &Path,
        origin: &str,
        owner: &str,
    ) -> Result<Option<Self>, NativeError> {
        let state = state.to_owned();
        let origin = origin.to_owned();
        let owner = owner.to_owned();
        tokio::task::spawn_blocking(move || {
            let Some(descriptor) = read_descriptor(&state)? else {
                return Ok(None);
            };
            let registry =
                super::device_setup::activation_registry(true).map_err(|_| descriptor_error())?;
            Self::loaded(&state, &origin, &owner, registry, descriptor).map(Some)
        })
        .await
        .map_err(|_| descriptor_error())?
    }
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn load_for_isolated_tests(
        state: &Path,
        origin: &str,
        owner: &str,
        registry: OwnerResourceRegistry,
    ) -> Result<Option<Self>, NativeError> {
        read_descriptor(state)?
            .map(|d| Self::loaded(state, origin, owner, registry, d))
            .transpose()
    }
    pub(super) fn completed(
        owner: &NativeOwner,
        registry: OwnerResourceRegistry,
        id: String,
        profile: String,
        device: String,
    ) -> Self {
        Self {
            registry,
            initialization_id: id,
            state: owner.0.host.state_directory().into(),
            origin: owner.0.expected_origin.clone(),
            owner: owner.0.expected_owner.clone(),
            profile,
            device,
        }
    }
    /// Identity comparison for a trusted host's existing account slot. No path
    /// or private identity is returned to a script.
    pub fn matches_host(&self, state: &Path, origin: &str, owner: &str) -> bool {
        state.canonicalize().is_ok_and(|path| self.state == path)
            && self.origin == origin
            && self.owner == owner
    }
    async fn check_owner(&self, owner: &NativeOwner) -> Result<(), NativeError> {
        if !self.matches_host(
            owner.0.host.state_directory(),
            &owner.0.expected_origin,
            &owner.0.expected_owner,
        ) || owner.0.current_profile.get() != Some(&self.initialization_id)
        {
            return Err(failure(401, "current_profile_scope_changed"));
        }
        owner
            .0
            .host
            .check_state_lock()
            .map_err(|_| failure(409, "source_root_changed"))?;
        owner.sync_management_matrix().await?;
        let cookie = owner.0.cookie.lock().await.clone();
        let raw = cookie
            .strip_prefix("hagency_console=")
            .ok_or_else(|| failure(401, "owner_authorization_required"))?;
        owner
            .0
            .host
            .console
            .0
            .server_login
            .check_matrix_source(raw)
            .await
            .map_err(|_| failure(401, "current_profile_scope_changed"))?;
        let device = owner
            .0
            .host
            .console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        if device.bearer().is_err()
            || crate::console::owned_agents::profile_identity(
                device.origin(),
                device.issuer(),
                device.subject(),
                device.owner_mxid(),
            )
            .map_err(|_| failure(401, "management_profile_mismatch"))?
                != self.profile
            || !owner
                .0
                .management
                .matches_current(&self.registry, &self.profile, &self.owner)
                .await
        {
            return Err(failure(401, "current_profile_scope_changed"));
        }
        Ok(())
    }
    pub async fn open_with_matrix_guarded(
        &self,
        state: &Path,
        origin: &str,
        owner: &str,
        source: Arc<dyn MatrixTokenSource>,
        guard: impl FnOnce(&NativeOwner) -> Result<(), NativeError> + Send,
    ) -> Result<NativeOwner, NativeError> {
        if !self.matches_host(state, origin, owner) {
            return Err(failure(401, "current_profile_scope_changed"));
        }
        let native = NativeOwner::open_current_profile_with_matrix_guarded(
            state,
            origin,
            owner,
            source,
            self.registry.clone(),
            self.initialization_id.clone(),
            guard,
        )
        .await?;
        if let Err(error) = self.check_owner(&native).await {
            let _ = native.suspend().await;
            return Err(error);
        }
        Ok(native)
    }
    /// Native review gets its own manager. Script service permissions never
    /// acquire ConsentGrants by sharing the ordinary Resources manager.
    pub async fn grant_access(
        &self,
        owner: &NativeOwner,
        mut package: ManagementPackage,
    ) -> Result<OwnerGrantAccess, NativeError> {
        self.check_owner(owner).await?;
        let device = owner
            .0
            .host
            .console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        if device.device_id() != self.device {
            return Err(failure(
                409,
                "current_grant_device_reauthorization_required",
            ));
        }
        package.permissions = vec![
            ManagementPermission::ReadResources,
            ManagementPermission::ReadProviders,
            ManagementPermission::ConnectProviders,
            ManagementPermission::ConsentGrants,
            ManagementPermission::ReadIntents,
            ManagementPermission::PublishGrants,
        ];
        let manager = owner.management(package).await?;
        let result = OwnerGrantIntents::new(manager.clone())
            .bind_registry(self.registry.clone())
            .await;
        if let Err(error) = self.check_owner(owner).await {
            manager.retire();
            return Err(error);
        }
        if result.is_err() {
            manager.retire();
        }
        result
    }
}
impl NativeOwner {
    pub async fn current_profile_selection(
        &self,
        registry: OwnerResourceRegistry,
    ) -> Result<CurrentProfileSelection, NativeError> {
        let id = self
            .0
            .current_profile
            .get()
            .ok_or_else(|| failure(409, "current_profile_required"))?;
        self.validate_current_profile(&registry, id).await?;
        let descriptor =
            read_descriptor(self.0.host.state_directory())?.ok_or_else(descriptor_error)?;
        let selected = CurrentProfileSelection::loaded(
            self.0.host.state_directory(),
            &self.0.expected_origin,
            &self.0.expected_owner,
            registry,
            descriptor,
        )?;
        selected.check_owner(self).await?;
        Ok(selected)
    }
    async fn profile_initialization_authority(
        &self,
        registry: &OwnerResourceRegistry,
        id: &str,
    ) -> Result<
        (
            ProfileInitializationAuthority,
            String,
            crate::console::AuthorizedDevice,
        ),
        NativeError,
    > {
        let device = super::device_setup::source(self).await?;
        let profile = crate::console::owned_agents::profile_identity(
            device.origin(),
            device.issuer(),
            device.subject(),
            device.owner_mxid(),
        )
        .map_err(|_| failure(401, "management_profile_mismatch"))?;
        let descriptor =
            read_descriptor(self.0.host.state_directory())?.ok_or_else(descriptor_error)?;
        if descriptor.registry_id != registry.id()
            || descriptor.initialization_id != id
            || descriptor.profile != profile
            || descriptor.origin != self.0.expected_origin
            || descriptor.owner != self.0.expected_owner
        {
            return Err(descriptor_error());
        }
        let expires = Instant::now() + Duration::from_secs(30);
        let weak = Arc::downgrade(&self.0);
        let authority = ProfileInitializationAuthority::new(
            registry.clone(),
            ProfileAuthority::new(profile.clone(), device.owner_mxid().into(), expires)
                .map_err(management::store_error)?,
            expires,
            move || {
                weak.upgrade().is_some_and(|owner| {
                    !owner
                        .configuration_stopped
                        .load(std::sync::atomic::Ordering::Acquire)
                        && owner.host.check_state_lock().is_ok()
                })
            },
        )
        .map_err(management::store_error)?;
        Ok((authority, profile, device))
    }
    pub(super) async fn validate_current_profile(
        &self,
        registry: &OwnerResourceRegistry,
        id: &str,
    ) -> Result<(), NativeError> {
        let (authority, _, device) = self.profile_initialization_authority(registry, id).await?;
        registry
            .current_profile_active(authority, id.into())
            .await
            .map_err(management::store_error)?;
        super::device_setup::same_device(self, &device).await?;
        Ok(())
    }
    pub(super) async fn initialize_current_management(
        &self,
        registry: &OwnerResourceRegistry,
        id: &str,
    ) -> Result<(), NativeError> {
        let (authority, profile, device) =
            self.profile_initialization_authority(registry, id).await?;
        let account = device.owner_mxid().to_owned();
        let directory = self
            .0
            .host
            .state_directory()
            .join("owned-agent-owners")
            .join(format!("management_{profile}"));
        let journal = tokio::task::spawn_blocking(move || {
            hagency_store::management::ServerManagementJournal::open(&directory, &profile, &account)
        })
        .await
        .map_err(|_| failure(503, "management_state_unavailable"))?
        .map_err(management::store_error)?;
        let backing = registry
            .current_management(authority, id.into(), journal)
            .await
            .map_err(management::store_error)?;
        self.0.management.install_current(backing).await?;
        super::device_setup::same_device(self, &device).await?;
        Ok(())
    }
}
#[cfg(feature = "shared-intake")]
impl CurrentProfileSelection {
    pub(super) async fn check_owner_stop(&self, owner: &NativeOwner) -> Result<(), NativeError> {
        self.check_executor_owner(owner).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_profile_descriptor_accepts_only_the_current_protocol() {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("state");
        hagency_store::private::directory(&state).unwrap();
        let descriptor = Descriptor {
            version: 4,
            registry_id: "r".repeat(64),
            initialization_id: "current_test".into(),
            origin: "https://server.test/".into(),
            owner: "@owner:test".into(),
            profile: "a".repeat(64),
            original_device: "device".into(),
            current_profile: true,
        };
        let path = state.join(INITIALIZER_FILE);
        let mut value = serde_json::to_value(&descriptor).unwrap();
        for version in [1, 2, 3] {
            value["version"] = version.into();
            hagency_store::private::replace(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(read_descriptor(&state).is_err());
        }
        value["version"] = 4.into();
        value["taskPrepare"] = serde_json::json!({});
        hagency_store::private::replace(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read_descriptor(&state).is_err());
        value.as_object_mut().unwrap().remove("taskPrepare");
        hagency_store::private::replace(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read_descriptor(&state).unwrap().is_some());
        value["currentProfile"] = false.into();
        hagency_store::private::replace(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(read_descriptor(&state).is_err());
    }
    #[tokio::test]
    async fn current_profile_reopen_retains_original_executor_device_and_owner_binding(){
        use crate::console::native::reconciliation_sources::tests::Fixture;
        let f=Fixture::new().await;
        let registry=OwnerResourceRegistry::isolated_for_tests(&f.temp.path().join("canonical")).unwrap();
        let f=f.initialize_current(registry.clone()).await;
        let selected=f.owner.current_profile_selection(registry.clone()).await.unwrap();
        selected.check_executor_owner(&f.owner).await.unwrap();
        let mut changed=selected.clone();changed.device="another_device".into();
        assert_eq!(changed.check_executor_owner(&f.owner).await.unwrap_err().code,"current_executor_device_reauthorization_required");
        let mut changed_owner=selected.clone();changed_owner.owner="@another:test".into();
        assert_eq!(changed_owner.check_owner(&f.owner).await.unwrap_err().code,"current_profile_scope_changed");
        f.live.store(false,std::sync::atomic::Ordering::Release);
        assert!(selected.check_owner(&f.owner).await.is_err());
        drop(selected);drop(changed);drop(changed_owner);f.stop().await;registry.shutdown().await.unwrap();
    }

}
