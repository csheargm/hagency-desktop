//! Initial native-owned enrollment identity, not a server-issued credential.
use super::super::{
    ManagementRuntime,
    allocation_transport::{AllocationTransport, ConnectionReply, Operation},
};
use super::*;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EnrollmentBinding {
    registry_id: String,
    contribution_id: String,
    // Optional only for previously written journals. Omission preserves their
    // original digest and bytes; a current admission must never relabel them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admission_request_id: Option<String>,
    resource_id: String,
    resource_revision: String,
    provider_binding: String,
    coordinator_mxid: String,
    executor_generation: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Entry {
    enrollment_id: String,
    binding: EnrollmentBinding,
    binding_digest: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Enrollments {
    version: u32,
    identity: Identity,
    entries: Vec<Entry>,
}
impl Enrollments {
    fn marker(path: &Path) -> PathBuf {
        path.with_file_name("management-enrollments-v1.identity")
    }
    fn load(path: &Path, identity: &Identity) -> Result<Self, NativeError> {
        let marker = Self::marker(path);
        match (path.exists(), marker.exists()) {
            (false, false) => {
                return Ok(Self {
                    version: 1,
                    identity: identity.clone(),
                    entries: vec![],
                });
            }
            (true, true) => {}
            _ => return Err(history_error()),
        }
        let read = |path: &Path, limit| {
            crate::console::server_login::read_private_json(path, limit)
                .map_err(|_| history_error())
        };
        if serde_json::from_slice::<Identity>(&read(&marker, 8192)?).map_err(|_| history_error())?
            != *identity
        {
            return Err(history_error());
        }
        let value: Self =
            serde_json::from_slice(&read(path, 256 * 1024)?).map_err(|_| history_error())?;
        if value.version != 1 || value.identity != *identity || value.entries.len() > 128 {
            return Err(history_error());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut contributions = std::collections::BTreeSet::new();
        for entry in &value.entries {
            if let Some(request) = &entry.binding.admission_request_id {
                super::super::key(request)?;
            }
            for id in [
                &entry.enrollment_id,
                &entry.binding.contribution_id,
                &entry.binding.resource_id,
            ] {
                super::super::key(id)?;
            }
            for hash_value in [
                &entry.binding.registry_id,
                &entry.binding.resource_revision,
                &entry.binding.provider_binding,
            ] {
                hash(hash_value)?;
            }
            positive(entry.binding.executor_generation)?;
            same_server_user(&entry.binding.coordinator_mxid, &identity.owner)?;
            if !ids.insert(&entry.enrollment_id)
                || !contributions.insert((&entry.binding.contribution_id, &entry.binding.admission_request_id))
                || entry.binding_digest != digest(&entry.binding)?
            {
                return Err(history_error());
            }
        }
        Ok(value)
    }
    fn save(&self, path: &Path) -> Result<(), NativeError> {
        let data = serde_json::to_vec(self).map_err(|_| history_error())?;
        if data.len() > 256 * 1024 || self.entries.len() > 128 {
            return Err(failure(409, "management_enrollment_history_full"));
        }
        let marker = Self::marker(path);
        if !marker.exists() {
            if path.exists() {
                return Err(history_error());
            }
            hagency_store::private::replace(
                &marker,
                &serde_json::to_vec(&self.identity).map_err(|_| history_error())?,
            )
            .map_err(|_| history_error())?;
        }
        hagency_store::private::replace(path, &data).map_err(|_| history_error())
    }
}

fn prior_intent(error: hagency_store::Error) -> NativeError {
    if matches!(error, hagency_store::Error::Conflict) {
        failure(409, "owner_grant_prior_intent_required")
    } else {
        storage(error)
    }
}

pub(super) fn validate_admission(
    reply: &ConnectionReply,
    io: &NativeIo<'_>,
    id: &str,
) -> Result<ManagementRuntime, NativeError> {
    let c = &reply.admission.contribution;
    let r = &reply.admission.request;
    positive(reply.observed_at_ms)?;
    positive(c.revision)?;
    positive(r.revision)?;
    hash(&c.resource_revision)?;
    super::super::key(&c.resource_id)?;
    super::super::key(&c.request_id)?;
    same_server_user(&c.coordinator_mxid, &io.identity.owner)?;
    // A new pool has its owner's exact terms before administrator approval.
    // This grants only local reservation/publication authority; every execution
    // path still requires the central contribution to be admitted.
    let admitted = c.state == "admitted" && r.state == "approved";
    let staged = c.publication_first && ((c.state == "draft" && r.state == "draft")
        || (c.state == "pending_admission" && r.state == "pending"));
    if c.id != id
        || c.owner_mxid != io.identity.owner
        || c.executor_device_id != io.identity.device
        || c.executor_generation != io.device.generation()
        || !(admitted || staged)
        || c.request_id != r.id
        || r.contribution_id != c.id
        || r.requester_mxid != c.owner_mxid
        || r.kind != "resource_admission"
        || r.project_id.is_some()
        || r.allocation_id.is_some()
    {
        return Err(failure(409, "management_enrollment_changed"));
    }
    match c.runtime.as_str() {
        "codex" => Ok(ManagementRuntime::Codex),
        "claude" => Ok(ManagementRuntime::Claude),
        _ => Err(failure(409, "provider_identity_unavailable")),
    }
}

/// Check the proof's original native ID against the current admission. This
/// also fences an already returned proof if the server changes before prepare.
pub(super) fn check_grant_enrollment(
    io: &NativeIo<'_>,
    binding: &Binding,
    reply: &ConnectionReply,
) -> Result<(), NativeError> {
    let history = Enrollments::load(
        &io.path.with_file_name("management-enrollments-v1.json"),
        &io.identity,
    )?;
    let c = &reply.admission.contribution;
    let original = history.entries.iter().find(|entry| {
        entry.binding.contribution_id == binding.contribution_id && entry.enrollment_id == binding.enrollment_id
    });
    let Some(entry) = original else {
        // Legacy native proofs did not have a provider-bound enrollment
        // journal. They may only describe the original revision2 admission.
        if history.entries.iter().any(|e| e.binding.contribution_id == binding.contribution_id)
            || binding.verified_resource_binding.is_some()
            || c.revision != 2 || reply.admission.request.revision != 2
        {
            return Err(failure(409, "management_enrollment_history_required"));
        }
        return Ok(());
    };
    let saved = &entry.binding;
    if entry.enrollment_id != binding.enrollment_id
        || saved.registry_id != binding.registry_id
        || saved.resource_id != binding.resource_id
        || saved.resource_revision != binding.resource_revision
        || binding.verified_resource_binding.as_ref() != Some(&saved.provider_binding)
        || saved.coordinator_mxid != binding.admitted_coordinator
        || saved.executor_generation != binding.executor_generation
        || saved.admission_request_id.as_ref().is_some_and(|id| id != &c.request_id)
    {
        return Err(failure(409, "management_enrollment_changed"));
    }
    if saved.admission_request_id.is_none()
        && (c.revision != 2 || reply.admission.request.revision != 2)
    {
        return Err(failure(409, "management_enrollment_history_required"));
    }
    Ok(())
}
impl OwnerGrantIntents {
    /// Native form preview only. Preparing an immutable grant and reserving its
    /// budget happen after the owner submits the publication form.
    pub async fn publication_form(
        &self, registry: &OwnerResourceRegistry, contribution_id: String,
    ) -> Result<OwnerGrantForm, NativeError> {
        super::super::key(&contribution_id)?;
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        io.session.authorize(&io.context, Permission::PublishGrants).map_err(storage)?;
        let transport = AllocationTransport::new(self.manager.clone());
        let scope = transport.scope(Permission::ConsentGrants).await?;
        let reply: ConnectionReply = transport.request(
            &scope, Operation::Contribution(contribution_id.clone()), false,
        ).await?;
        io.check().await?;
        validate_admission(&reply, &io, &contribution_id)?;
        let c = reply.admission.contribution;
        let resource = registry.resource(&io.profile, c.resource_id.clone()).await.map_err(storage)?;
        io.check().await?;
        if resource.revision != c.resource_revision || resource.framework != c.runtime {
            return Err(failure(409, "management_enrollment_changed"));
        }
        let calendar = registry.resource_calendar(io.profile.clone(), c.resource_id.clone()).await.map_err(storage)?;
        io.check().await?;
        Ok(OwnerGrantForm {
            awaiting_admission: c.state != "admitted",
            calendar,
            contribution_id, resource_id: c.resource_id, label: c.label,
            context: OwnerGrantReviewContext {
                server_origin: io.identity.origin.clone(), profile_id: profile_digest(&io.identity)?,
                resource_revision: resource.revision, framework: resource.framework, model: resource.model,
                remaining_tokens: resource.budget.remaining_tokens.map(u64::from),
                resource_total_tokens: resource.budget.pool.ceiling.map(u64::from),
                shared_account_total_tokens: resource.budget.seat.quota.map(u64::from),
                capacity_blocker: capacity_blocker(&resource.budget),
                observed_at_ms: now_ms()?,
            },
        })
    }
    /// Reconcile private intent outcomes with committed canonical closure.
    /// A missing receipt for an attempted intent is unknown, never zero effect.
    pub(super) async fn reconcile_prior_admission(
        &self, io: &NativeIo<'_>, registry: &OwnerResourceRegistry,
        contribution: &str, request: &str,
    ) -> Result<Vec<String>, NativeError> {
        let closed = registry.closed_prior_admission_grants(io.profile.clone(),
            io.identity.device.clone(), contribution.into(), request.into()).await.map_err(prior_intent)?;
        self.reconcile_originals(io, registry, contribution, closed).await
    }
    /// Before a new UTC month on the same admission: every earlier original is
    /// declined, or recorded and canonically history. This admission's grants
    /// count as history only after their month ended; others must be closed.
    pub(super) async fn reconcile_regrant(
        &self,
        io: &NativeIo<'_>,
        registry: &OwnerResourceRegistry,
        contribution: &str,
        request: &str,
    ) -> Result<Vec<String>, NativeError> {
        let prior = registry
            .regrant_prior_grants(
                io.profile.clone(),
                io.identity.device.clone(),
                contribution.into(),
                request.into(),
            )
            .await
            .map_err(prior_intent)?;
        self.reconcile_originals(io, registry, contribution, prior)
            .await
    }
    async fn reconcile_originals(
        &self,
        io: &NativeIo<'_>,
        registry: &OwnerResourceRegistry,
        contribution: &str,
        prior: Vec<String>,
    ) -> Result<Vec<String>, NativeError> {
        io.check().await?;
        let mut grants = History::load(&io.path, io.identity.clone())?;
        for index in 0..grants.data.records.len() {
            if grants.data.records[index].original.binding.contribution_id != contribution { continue }
            let progress = self.recovered(io, registry, &mut grants, index).await?;
            let record = &grants.data.records[index];
            match progress.state {
                OwnerGrantState::Declined => {},
                OwnerGrantState::Recorded if prior.contains(&record.original.selection.grant_id) => {},
                _ => return Err(failure(409, "owner_grant_prior_intent_required")),
            }
        }
        io.check().await?;
        Ok(prior)
    }
    /// The caller selects only an existing admitted contribution. Native code
    /// observes the real provider and canonical resource, then owns the ID.
    pub async fn verified_enrollment(
        &self,
        registry: &OwnerResourceRegistry,
        contribution_id: String,
    ) -> Result<VerifiedManagementEnrollment, NativeError> {
        self.verified_enrollment_observe(registry, contribution_id, |runtime,profile,resource,revision| {
            self.manager.verified_resource_provider_account(registry,profile,runtime,resource,revision)
        })
        .await
    }
    #[cfg(test)]
    pub(super) async fn verified_enrollment_with<F,Fut>(
        &self, registry:&OwnerResourceRegistry, contribution_id:String, observe:F,
    )->Result<VerifiedManagementEnrollment,NativeError>
    where F:FnOnce(ManagementRuntime)->Fut,
          Fut:std::future::Future<Output=Result<VerifiedProviderAccount,NativeError>>,
    {
        self.verified_enrollment_observe(registry,contribution_id,|runtime,_,_,_|observe(runtime)).await
    }
    async fn verified_enrollment_observe<F, Fut>(
        &self,
        registry: &OwnerResourceRegistry,
        contribution_id: String,
        observe: F,
    ) -> Result<VerifiedManagementEnrollment, NativeError>
    where
        F: FnOnce(ManagementRuntime, ProfileAuthority, String, String) -> Fut,
        Fut: std::future::Future<Output = Result<VerifiedProviderAccount, NativeError>>,
    {
        super::super::key(&contribution_id)?;
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        for permission in [Permission::ReadProviders, Permission::ConnectProviders] {
            io.session
                .authorize(&io.context, permission)
                .map_err(storage)?;
        }
        let transport = AllocationTransport::new(self.manager.clone());
        let scope = transport.scope(Permission::ConsentGrants).await?;
        io.check().await?;
        let before: ConnectionReply = transport
            .request(
                &scope,
                Operation::Contribution(contribution_id.clone()),
                false,
            )
            .await?;
        io.check().await?;
        let runtime = validate_admission(&before, &io, &contribution_id)?;
        // The central record must name the tool of the local resource before
        // any provider is observed on its word.
        let local = registry
            .resource(&io.profile, before.admission.contribution.resource_id.clone())
            .await
            .map_err(storage)?;
        io.check().await?;
        if local.framework != before.admission.contribution.runtime {
            return Err(failure(409, "management_enrollment_changed"));
        }
        let provider = observe(runtime,io.profile.clone(),before.admission.contribution.resource_id.clone(),before.admission.contribution.resource_revision.clone()).await?;
        io.check().await?;
        let resource = registry
            .verify_provider_resource(
                io.profile.clone(),
                before.admission.contribution.resource_id.clone(),
                provider,
            )
            .await
            .map_err(storage)?;
        io.check().await?;
        if resource.resource_revision() != before.admission.contribution.resource_revision
            || resource.runtime() != before.admission.contribution.runtime
        {
            return Err(failure(409, "management_enrollment_changed"));
        }
        let lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        let latest: ConnectionReply = transport
            .request(
                &scope,
                Operation::Contribution(contribution_id.clone()),
                false,
            )
            .await?;
        io.check().await?;
        validate_admission(&latest, &io, &contribution_id)?;
        let c = &latest.admission.contribution;
        let previous = &before.admission.contribution;
        if c.request_id != previous.request_id
            || c.revision != previous.revision
            || latest.admission.request.revision != before.admission.request.revision
            || c.resource_id != previous.resource_id
            || c.resource_revision != previous.resource_revision
            || c.runtime != previous.runtime
            || c.coordinator_mxid != previous.coordinator_mxid
        {
            return Err(failure(409, "management_enrollment_changed"));
        }
        let binding = EnrollmentBinding {
            registry_id: registry.id().into(),
            contribution_id: contribution_id.clone(),
            admission_request_id: Some(c.request_id.clone()),
            resource_id: c.resource_id.clone(),
            resource_revision: c.resource_revision.clone(),
            provider_binding: resource.binding_digest().map_err(storage)?,
            coordinator_mxid: c.coordinator_mxid.clone(),
            executor_generation: c.executor_generation,
        };
        let path = io.path.with_file_name("management-enrollments-v1.json");
        let mut history = Enrollments::load(&path, &io.identity)?;
        let enrollment_id = if let Some(entry) = history
            .entries
            .iter()
            .find(|e| e.binding.contribution_id == contribution_id
                && (e.binding.admission_request_id.as_ref() == Some(&c.request_id)
                    || (e.binding.admission_request_id.is_none() && c.revision == 2
                        && latest.admission.request.revision == 2)))
        {
            let mut observed = binding.clone();
            if entry.binding.admission_request_id.is_none() {
                // In this protocol creation is revision1 and its first approval
                // is revision2. Withdrawal/re-admission never resets revisions.
                // A legacy record may continue only at that original decision;
                // later rounds need explicit history reconciliation, not a
                // guessed request binding or a rewritten enrollment identity.
                if c.revision != 2 || latest.admission.request.revision != 2 {
                    return Err(failure(409, "management_enrollment_history_required"));
                }
                observed.admission_request_id = None;
            }
            if entry.binding != observed {
                return Err(failure(409, "management_enrollment_changed"));
            }
            entry.enrollment_id.clone()
        } else {
            let earlier = history.entries.iter().any(|e| e.binding.contribution_id == contribution_id);
            if earlier {
                if !Box::pin(self.supports_admission_receipts(&io)).await? {
                    let legacy = history.entries.iter().any(|e| e.binding.contribution_id == contribution_id
                        && e.binding.admission_request_id.is_none());
                    return Err(failure(409, if legacy { "management_enrollment_history_required" }
                        else { "management_enrollment_changed" }));
                }
                Box::pin(self.reconcile_prior_admission(&io, registry, &contribution_id, &c.request_id)).await?;
            } else if registry
                .has_capacity_grant_for_contribution(io.profile.clone(), contribution_id.clone())
                .await
                .map_err(storage)?
            {
                return Err(failure(409, "management_enrollment_history_required"));
            }
            io.check().await?;
            // The separate grant journal can retain an original identity even
            // when no canonical hold was committed. Never mint its replacement.
            let grants = History::load(&io.path, io.identity.clone())?;
            if !earlier && grants
                .data
                .records
                .iter()
                .any(|r| r.original.binding.contribution_id == contribution_id)
            {
                return Err(failure(409, "management_enrollment_history_required"));
            }
            let enrollment_id = random_id()?;
            history.entries.push(Entry {
                enrollment_id: enrollment_id.clone(),
                binding_digest: digest(&binding)?,
                binding,
            });
            io.check().await?;
            history.save(&path)?;
            enrollment_id
        };
        io.check().await?;
        drop(lane);
        // The provider observation may expire sooner than the SDK session.
        // Finish every await before constructing the returned finite proof.
        io.check().await?;
        let proof = VerifiedManagementEnrollment::new(
            io.profile.clone(),
            enrollment_id,
            contribution_id,
            c.resource_id.clone(),
            c.resource_revision.clone(),
            c.coordinator_mxid.clone(),
            io.identity.device.clone(),
            c.executor_generation,
            io.deadline,
        )
        .map_err(storage)?
        .with_verified_resource(resource)
        .map_err(storage)?;
        Ok(proof)
    }
}
