//! Read-only Octos profile inventory and owner-confirmed resource enrollment.
//! A profile is a local budget boundary, never proof of a provider account.
use super::{
    AccountInventory, AccountModelQuery, AccountSummary, NamespaceEnrollment, NativeError,
    NativeManagement, ResourceDetailQuery, failure, management::store_error,
};
use crate::console::owner_provider::{self, ProviderPaths};
use hagency_runtime::octos::profile_binding::{ProfileBinding, profile_ids};
use hagency_store::{
    management::{HostContext, Permission, Session},
    owner_resources::OwnerResourceReceipt,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::OnceLock,
    time::{Duration, Instant},
};

const PREFIX: &str = "account_octos_";
pub(super) fn is_octos_account(id: &str) -> bool {
    id.starts_with(PREFIX)
}
fn profile_id(id: &str) -> Option<&str> {
    id.strip_prefix(PREFIX)
        .filter(|id| hagency_runtime::octos::profile_id(id))
}
fn ui_key() -> Result<&'static [u8; 32], NativeError> {
    static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
    KEY.get_or_init(|| {
        let mut key = [0; 32];
        getrandom::fill(&mut key).ok()?;
        (!key.iter().all(|b| *b == 0)).then_some(key)
    })
    .as_ref()
    .ok_or_else(|| failure(503, "management_unavailable"))
}
fn revision(binding: &ProfileBinding, key: &[u8; 32]) -> Result<String, NativeError> {
    let account = binding
        .accounting_key(key)
        .map_err(|_| failure(409, "provider_account_changed"))?;
    let version = binding
        .configuration_key(key)
        .map_err(|_| failure(409, "provider_account_changed"))?;
    hagency_core::canonical::digest(&json!({"version":1,"account":account,"configuration":version}))
        .map_err(|_| failure(503, "management_unavailable"))
}
fn observe(home: &Path, id: &str, deadline: Instant) -> Result<ProfileBinding, NativeError> {
    ProfileBinding::observe(home, id, deadline)
        .map_err(|_| failure(409, "provider_account_changed"))
}
fn summary(binding: &ProfileBinding, revision: String, resource_count: usize) -> AccountSummary {
    AccountSummary {
        id: format!("{PREFIX}{}", binding.profile_id()),
        kind: "local_configuration",
        runtime: "octos".into(),
        state: "active".into(),
        ordinal: None,
        revision: Some(revision),
        profile: Some(binding.profile_id().into()),
        readiness: "unknown",
        identity_key: None,
        resource_count: resource_count as u64,
    }
}
fn same_paths(a: &ProviderPaths, b: &ProviderPaths) -> bool {
    a.home == b.home
        && a.codex_home == b.codex_home
        && a.credential_ref == b.credential_ref
        && a.shared == b.shared
}

impl NativeManagement {
    async fn octos_paths(&self, context: &HostContext) -> Result<ProviderPaths, NativeError> {
        #[cfg(not(feature = "resource-adoption"))]
        {
            let _ = context;
            Err(failure(409, "device_setup_required"))
        }
        #[cfg(feature = "resource-adoption")]
        {
            let console = &self.owner.0.host.console;
            let device = console
                .authorized_device()
                .await
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            device
                .bearer()
                .map_err(|_| failure(401, "owner_authorization_required"))?;
            self.check_observation_device(context, &device)?;
            let root = console
                .0
                .server_login
                .state_directory()
                .ok_or_else(|| failure(503, "provider_directory_unavailable"))?;
            owner_provider::external::connected_paths(
                &root,
                device.origin(),
                device.issuer(),
                device.subject(),
                device.owner_mxid(),
                hagency_agent_local::runtime::Kind::Octos,
            )
            .map_err(|e| failure(e.status, e.code))
        }
    }
    async fn octos_witness(
        &self,
        context: &HostContext,
        session: &Session,
        id: &str,
        expected: Option<&str>,
    ) -> Result<(ProviderPaths, ProfileBinding, String), NativeError> {
        if !hagency_runtime::octos::profile_id(id) {
            return Err(failure(400, "invalid_management_arguments"));
        }
        let paths = self.octos_paths(context).await?;
        let deadline = session
            .valid_until()
            .min(Instant::now() + Duration::from_secs(30));
        let binding = observe(&paths.codex_home, id, deadline)?;
        let version = revision(&binding, ui_key()?)?;
        if expected.is_some_and(|value| value != version) {
            return Err(failure(409, "provider_account_changed"));
        }
        let current = self.octos_paths(context).await?;
        if !same_paths(&paths, &current) {
            return Err(failure(409, "provider_account_changed"));
        }
        binding
            .check()
            .map_err(|_| failure(409, "provider_account_changed"))?;
        Ok((paths, binding, version))
    }
    async fn octos_profile(
        &self,
        context: &HostContext,
        session: &Session,
        id: &str,
        expected: Option<&str>,
    ) -> Result<(AccountSummary, ProfileBinding, Vec<(String,String)>), NativeError> {
        let (paths, binding, version) = self.octos_witness(context, session, id, expected).await?;
        let deadline = session
            .valid_until()
            .min(Instant::now() + Duration::from_secs(30));
        // A second witness feeds the read-only lookup; it must describe the
        // same original bytes as the one that will be returned for review.
        let lookup = observe(&paths.codex_home, id, deadline)?;
        if revision(&lookup, ui_key()?)? != version {
            return Err(failure(409, "provider_account_changed"));
        }
        let resources = session
            .octos_account_resources(context, lookup)
            .await
            .map_err(store_error)?;
        let current = self.octos_paths(context).await?;
        if !same_paths(&paths, &current) {
            return Err(failure(409, "provider_account_changed"));
        }
        binding
            .check()
            .map_err(|_| failure(409, "provider_account_changed"))?;
        let row = summary(&binding, version, resources.len());
        Ok((row, binding, resources))
    }
    pub(super) async fn octos_accounts(
        &self,
        context: &HostContext,
        session: &Session,
    ) -> Result<Vec<AccountSummary>, NativeError> {
        if !self.account_enrollment_ready() {
            return Ok(Vec::new());
        }
        let paths = match self.octos_paths(context).await {
            Ok(paths) => paths,
            Err(e)
                if matches!(
                    e.code.as_str(),
                    "provider_profile_mismatch" | "local_provider_home_unavailable"
                ) =>
            {
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let ids = match profile_ids(&paths.codex_home) {
            Ok(ids) => ids,
            Err(_) if !paths.codex_home.join("profiles").exists() => return Ok(Vec::new()),
            Err(_) => return Err(failure(409, "octos_profiles_unavailable")),
        };
        let mut rows = Vec::new();
        for id in ids {
            // A malformed or symlinked file is not a valid profile. A valid
            // profile changing during observation fails the entire listing.
            let deadline = session
                .valid_until()
                .min(Instant::now() + Duration::from_secs(30));
            if observe(&paths.codex_home, &id, deadline).is_err() {
                continue;
            }
            let (row, _, _) = self.octos_profile(context, session, &id, None).await?;
            rows.push(row);
        }
        if !same_paths(&paths, &self.octos_paths(context).await?) {
            return Err(failure(409, "provider_account_changed"));
        }
        Ok(rows)
    }
    pub(super) async fn octos_account_detail(
        &self,
        context: &HostContext,
        session: &Session,
        id: &str,
        after: Option<String>,
    ) -> Result<AccountInventory, NativeError> {
        session
            .authorize(context, Permission::ReadAccounts)
            .map_err(store_error)?;
        let id = profile_id(id).ok_or_else(|| failure(400, "invalid_management_arguments"))?;
        let (account, _, resources) = self.octos_profile(context, session, id, None).await?;
        let page = resources.into_iter().map(|(_, resource)| resource)
            .filter(|resource| after.as_ref().is_none_or(|a| resource > a)).take(17).collect::<Vec<_>>();
        let next_after = (page.len() > 16).then(|| page[15].clone());
        let mut resources = Vec::new();
        for resource in page.into_iter().take(16) {
            if let Ok(detail) = session
                .resource_detail(context, ResourceDetailQuery { id: resource })
                .await
            {
                resources.push(detail.resource);
            }
        }
        let observed_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|v| u64::try_from(v.as_millis()).ok())
            .ok_or_else(|| failure(503, "management_unavailable"))?;
        self.finish(
            context,
            Ok(AccountInventory {
                protocol: hagency_store::management::PROTOCOL,
                observed_at_ms,
                accounts: vec![],
                account: Some(account),
                resources,
                next_after,
            }),
            false,
        )
        .await
    }
    pub(super) async fn octos_account_models(
        &self,
        input: AccountModelQuery,
    ) -> Result<Value, NativeError> {
        let id = profile_id(&input.account_id)
            .ok_or_else(|| failure(400, "invalid_management_arguments"))?
            .to_owned();
        let (context, session) = self.authorized().await?;
        session
            .authorize(&context, Permission::ReadAccountModels)
            .map_err(store_error)?;
        let (paths, binding, _) = self
            .octos_witness(&context, &session, &id, Some(&input.expected_revision))
            .await?;
        let lookup = observe(&paths.codex_home, &id, session.valid_until().min(Instant::now() + Duration::from_secs(30)))?;
        if revision(&lookup, ui_key()?)? != input.expected_revision {
            return Err(failure(409, "provider_account_changed"));
        }
        let (enrolled, shared_quota) = session.octos_model_overview(&context, lookup).await.map_err(store_error)?;
        let models = binding.models().map_err(|_| failure(409, "provider_account_changed"))?;
        let models = models.into_iter().enumerate().map(|(index, model)| json!({
            "id":model.model,"model":model.model,"label":format!("{} · {}", model.family, model.model),
            "family":model.family,"default":index==0,"enrolled":enrolled.iter().any(|(name,_)| name==&model.model),
            "reasoning":[],"sharedReasoning":[null],"sharedDefaultReasoning":null
        })).collect::<Vec<_>>();
        let (fresh, live) = self.authorized().await?;
        if fresh != context {
            return Err(failure(401, "management_scope_changed"));
        }
        live.authorize(&context, Permission::ReadAccountModels)
            .map_err(store_error)?;
        if !same_paths(&paths, &self.octos_paths(&fresh).await?) {
            return Err(failure(409, "provider_account_changed"));
        }
        binding
            .check()
            .map_err(|_| failure(409, "provider_account_changed"))?;
        Ok(
            json!({"protocol":hagency_store::management::PROTOCOL,"accountId":input.account_id,"revision":input.expected_revision,
            "enrollmentReady":cfg!(target_os = "macos") && self.account_enrollment_ready(),
            "unavailableReason":if cfg!(target_os = "macos") { Value::Null } else { json!("unsupported_platform") },
            "provider":{"runtime":"octos","connected":true,"accountVerified":false,"sharedQuota":shared_quota,"models":models}}),
        )
    }
    #[cfg(feature = "resource-adoption")]
    pub(super) async fn enroll_octos_resource(
        &self,
        input: NamespaceEnrollment,
        confirmed_until: Instant,
    ) -> Result<OwnerResourceReceipt, NativeError> {
        if !cfg!(target_os = "macos") {
            return Err(failure(409, "octos_pool_platform_unsupported"));
        }
        let id = profile_id(&input.account.account_id)
            .ok_or_else(|| failure(400, "invalid_management_arguments"))?;
        let _guard = self.owner.0.calls.lock().await;
        let (context, session) = self.authorized().await?;
        let (registry, profile) = session
            .namespace_enrollment_registry(&context)
            .map_err(store_error)?;
        if let Some(receipt) = registry
            .recover_octos_account_enrollment(profile.clone(), input.clone())
            .await
            .map_err(store_error)?
        {
            return self.finish(&context, Ok(receipt), false).await;
        }
        if Instant::now() >= confirmed_until {
            return Err(failure(403, "provider_confirmation_expired"));
        }
        let paths = self.octos_paths(&context).await?;
        let deadline = session
            .valid_until()
            .min(confirmed_until)
            .min(Instant::now() + Duration::from_secs(30));
        let binding = ProfileBinding::observe_model(&paths.codex_home, id, &input.model, deadline)
            .map_err(|_| failure(409, "provider_model_changed"))?;
        if revision(&binding, ui_key()?)? != input.account.expected_revision {
            return Err(failure(409, "provider_account_changed"));
        }
        if binding.model().model != input.model || input.reasoning.is_some() {
            return Err(failure(409, "provider_model_changed"));
        }
        let proof = registry
            .observe_octos_profile(binding)
            .await
            .map_err(store_error)?;
        let (fresh, live) = self.authorized().await?;
        if fresh != context {
            return Err(failure(401, "management_scope_changed"));
        }
        live.authorize(&context, Permission::EnrollResources)
            .map_err(store_error)?;
        if !same_paths(&paths, &self.octos_paths(&context).await?) {
            return Err(failure(409, "provider_account_changed"));
        }
        if Instant::now() >= confirmed_until {
            return Err(failure(403, "provider_confirmation_expired"));
        }
        let consent = registry
            .prepare_octos_account_enrollment(profile, input, proof, deadline)
            .map_err(store_error)?;
        let result = consent.approve().await;
        self.finish(&context, result, true).await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn public_revision_binds_the_home_profile_and_all_configuration_bytes() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().canonicalize().unwrap();
        std::fs::create_dir(home.join("profiles")).unwrap();
        let write = |id: &str, secret: &str| {
            std::fs::write(home.join("profiles").join(format!("{id}.json")),json!({"id":id,"config":{"llm":{"primary":{"family_id":"local","model_id":"model"}},"env_vars":{"SECRET":secret}}}).to_string()).unwrap()
        };
        write("one", "first");
        write("two", "first");
        let witness = |id| observe(&home, id, Instant::now() + Duration::from_secs(30)).unwrap();
        let before = witness("one");
        let first = revision(&before, &[7; 32]).unwrap();
        assert_ne!(first, revision(&witness("two"), &[7; 32]).unwrap());
        assert_ne!(first, revision(&witness("one"), &[8; 32]).unwrap());
        write("one", "rotated");
        assert!(revision(&before, &[7; 32]).is_err());
        assert_ne!(first, revision(&witness("one"), &[7; 32]).unwrap());
        assert!(!first.contains("first") && profile_id("account_octos_../escape").is_none());
    }
}
