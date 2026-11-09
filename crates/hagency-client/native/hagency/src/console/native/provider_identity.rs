//! Trusted provider-account proof for the installation-level owner registry.
use super::{ManagementRuntime, NativeError, NativeManagement, failure};
use hagency_store::{management::Permission, owner_resources::VerifiedProviderAccount};

impl NativeManagement {
    /// Obtain a short-lived, non-deserializable identity from an already
    /// connected provider. This does not enroll a resource, merge old ledgers,
    /// approve capacity or initiate provider login. Claude reuses its selected
    /// native OAuth credential transiently for the fixed provider profile API;
    /// neither the credential nor public account metadata leaves the adapter.
    /// The native host must still obtain owner consent for registry enrollment.
    pub async fn verified_provider_account(
        &self,
        runtime: ManagementRuntime,
    ) -> Result<VerifiedProviderAccount, NativeError> {
        let _guard = self.owner.0.calls.lock().await;
        let (context, session) = self.authorized().await?;
        for permission in [Permission::ReadProviders, Permission::ConnectProviders] {
            session
                .authorize(&context, permission)
                .map_err(|_| failure(403, "management_permission_required"))?;
        }
        if runtime == ManagementRuntime::Octos {
            return Err(failure(409, "provider_identity_unavailable"));
        }
        let cookie = self.owner.0.cookie.lock().await.clone();
        let raw_cookie = cookie
            .strip_prefix("hagency_console=")
            .ok_or_else(|| failure(401, "owner_authorization_required"))?;
        let result = crate::console::owner_provider::identity::verify_runtime(
            &self.owner.0.host.console,
            raw_cookie,
            if runtime == ManagementRuntime::Claude { hagency_agent_local::runtime::Kind::Claude }
            else { hagency_agent_local::runtime::Kind::Codex },
        )
        .await;
        let (fresh, live) = self.authorized().await?;
        if fresh != context {
            return Err(failure(401, "management_scope_changed"));
        }
        for permission in [Permission::ReadProviders, Permission::ConnectProviders] {
            live.authorize(&context, permission)
                .map_err(|_| failure(403, "management_permission_required"))?;
        }
        let (proof, deadline) = result.map_err(|error| failure(error.status, error.code))?;
        if std::time::Instant::now() >= deadline {
            return Err(failure(409, "provider_identity_unavailable"));
        }
        Ok(proof)
    }
}

impl NativeManagement {
    pub(super) async fn namespace_provider(
        &self,
        namespace: &hagency_store::owner_resources::ResourceNamespace,
        deadline: std::time::Instant,
    ) -> Result<VerifiedProviderAccount, NativeError> {
        namespace.check().await.map_err(super::management::store_error)?;
        let (proof, _) = self.observe_account_namespace(namespace.launch(), deadline).await?;
        namespace.check().await.map_err(super::management::store_error)?;
        Ok(proof)
    }
    pub(super) async fn observe_account_namespace(
        &self, launch: &hagency_store::ManagedLaunch, deadline: std::time::Instant,
    ) -> Result<(VerifiedProviderAccount, super::super::owner_provider::ProviderStatus), NativeError> {
        #[cfg(test)]
        let fixture=self.owner.0.account_logins.executable.lock().unwrap().clone();
        #[cfg(test)]
        let executable=match fixture {Some(p)=>p,None=>super::super::owner_provider::executable().map_err(|e|failure(e.status,e.code))?};
        #[cfg(not(test))]
        let executable=super::super::owner_provider::executable().map_err(|e|failure(e.status,e.code))?;
        super::super::owner_provider::managed::inspect_resource(
            launch,&self.owner.0.host.console.0.owner_provider,&executable,deadline,
        ).await.map_err(|e|failure(e.status,e.code))
    }
    pub(super) async fn verified_resource_provider_account(
        &self,
        registry: &hagency_store::owner_resources::OwnerResourceRegistry,
        profile: hagency_store::owner_resources::ProfileAuthority,
        runtime: ManagementRuntime,
        resource: String,
        revision: String,
    ) -> Result<VerifiedProviderAccount, NativeError> {
        let namespace=registry.resource_namespace(profile,resource,revision).await.map_err(super::management::store_error)?;
        let Some(namespace)=namespace else {return self.verified_provider_account(runtime).await};
        if runtime!=ManagementRuntime::Codex {return Err(failure(409,"provider_identity_unavailable"));}
        let _guard=self.owner.0.calls.lock().await;
        let (context,session)=self.authorized().await?;
        for p in [Permission::ReadProviders,Permission::ConnectProviders] {session.authorize(&context,p).map_err(super::management::store_error)?;}
        let deadline=session.valid_until().min(std::time::Instant::now()+std::time::Duration::from_secs(30));
        let result=self.namespace_provider(&namespace,deadline).await;
        let (fresh,live)=self.authorized().await?;
        if fresh!=context {return Err(failure(401,"management_scope_changed"));}
        for p in [Permission::ReadProviders,Permission::ConnectProviders] {live.authorize(&context,p).map_err(super::management::store_error)?;}
        result
    }
}

impl NativeManagement {
    #[cfg(feature="resource-adoption")]
    async fn original_quota_paths(
        &self, context:&hagency_store::management::HostContext,
    )->Result<super::super::owner_provider::ProviderPaths,NativeError> {
        let device=self.owner.0.host.console.authorized_device().await
            .map_err(|_|failure(401,"owner_authorization_required"))?;
        device.bearer().map_err(|_|failure(401,"owner_authorization_required"))?;
        self.check_observation_device(context,&device)?;
        // Same authenticated original profile and saved provider selection as
        // legacy-resource execution. No resource-supplied path or ambient-home
        // fallback: a shared home requires the existing explicit choice file.
        super::super::owner_provider::paths(self.owner.0.host.state_directory(),
            device.origin(),device.issuer(),device.subject(),device.owner_mxid())
            .map_err(|e|failure(e.status,e.code))
    }

    /// Daemon-only read observation, bound to the current local resource and
    /// its original managed account namespace. Never opens an ambient home.
    pub(crate) async fn refresh_resource_quota(
        &self, resource:&hagency_store::management::ResourceSummary,
    )->Result<(),NativeError> {
        #[cfg(not(feature="resource-adoption"))]
        {let _=resource;Err(failure(409,"provider_identity_unavailable"))}
        #[cfg(feature="resource-adoption")]
        {
            if resource.framework!="codex" {return Err(failure(409,"provider_identity_unavailable"));}
            let telemetry=self.owner.telemetry();
            telemetry.forget_current_account(&resource.id);
            // A background read must not hold the global command mutex.
            // Namespace/session gates are rechecked after the bounded probe;
            // provider TaskCustody owns child cleanup during owner shutdown.
            let (context,session)=self.authorized().await?;
            let (registry,profile)=session.resource_observation_registry(&context).map_err(super::management::store_error)?;
            let namespace=registry.resource_namespace(profile.clone(),resource.id.clone(),resource.revision.clone()).await
                .map_err(super::management::store_error)?;
            let original=if namespace.is_none() {Some(self.original_quota_paths(&context).await?)} else {None};
            let deadline=session.valid_until().min(std::time::Instant::now()+std::time::Duration::from_secs(30));
            #[cfg(test)]
            let fixture=self.owner.0.account_logins.executable.lock().unwrap().clone();
            #[cfg(test)]
            let executable=match fixture {Some(path)=>path,None=>super::super::owner_provider::executable().map_err(|e|failure(e.status,e.code))?};
            #[cfg(not(test))]
            let executable=super::super::owner_provider::executable().map_err(|e|failure(e.status,e.code))?;
            let (observation,proof)=match (&namespace,&original) {
                (Some(namespace),_)=>super::super::owner_provider::managed::inspect_quota(namespace.launch(),&self.owner.0.host.console.0.owner_provider,&executable,deadline).await,
                (None,Some(paths))=>super::super::owner_provider::managed::inspect_original_quota(paths,&self.owner.0.host.console.0.owner_provider,&executable,deadline).await,
                _=>return Err(failure(409,"resource_namespace_required")),
            }.map_err(|e|failure(e.status,e.code))?;
            if let Some(namespace)=&namespace {namespace.check().await.map_err(super::management::store_error)?;}
            let (fresh,live)=self.authorized().await?;
            if fresh!=context {return Err(failure(401,"management_scope_changed"));}
            for permission in [Permission::ReadResources,Permission::ReadProviders] {live.authorize(&context,permission).map_err(super::management::store_error)?;}
            if let Some(original)=original {
                let current=self.original_quota_paths(&context).await?;
                if original.home!=current.home || original.codex_home!=current.codex_home
                    || original.shared!=current.shared || original.credential_ref!=current.credential_ref
                    || registry.resource_namespace(profile.clone(),resource.id.clone(),resource.revision.clone()).await
                        .map_err(super::management::store_error)?.is_some() {
                    return Err(failure(409,"provider_account_changed"));
                }
            }
            // Read-only canonical binding rejects a different stable account,
            // revoked access, or changed definition even if the probe itself
            // observed internally consistent provider metadata.
            let binding=registry.verify_provider_resource(profile,resource.id.clone(),proof).await
                .map_err(super::management::store_error)?;
            if binding.resource_revision()!=resource.revision {return Err(failure(409,"management_resource_source_changed"));}
            if std::time::Instant::now()>=deadline {return Err(failure(409,"provider_identity_unavailable"));}
            telemetry.observe_codex_quota(&resource.id,observation).map_err(|_|failure(503,"provider_telemetry_unavailable"))?;
            Ok(())
        }
    }
}
