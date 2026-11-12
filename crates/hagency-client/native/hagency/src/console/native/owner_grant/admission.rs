//! A review retains its exact central admission. Checking only its resource
//! hash allows an old window to approve after withdrawal and re-admission.
use super::*;
use serde_json::Value;
use crate::console::native::allocation_transport::{AllocationTransport, ConnectionReply, Operation};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct AdmissionRound {
    pub(super) request_id: String,
    request_revision: u64,
    contribution_revision: u64,
}
impl AdmissionRound {
    pub(super) fn validate(&self) -> Result<(), NativeError> {
        super::super::key(&self.request_id)?;
        positive(self.request_revision)?;
        positive(self.contribution_revision)
    }
    /// Two grants of one chain stand on the same admission when the request
    /// is the same. Its revisions move on the server (a pool published before
    /// the administrator's decision is approved afterwards), so a chain link
    /// compares the request alone, as `check_grant_admission` does. An
    /// unbound historical intent only continues another unbound one.
    pub(super) fn same_request(a: &Option<Self>, b: &Option<Self>) -> bool {
        match (a, b) {
            (Some(a), Some(b)) => a.request_id == b.request_id,
            (None, None) => true,
            _ => false,
        }
    }
}
impl OwnerGrantIntents {
    pub(super) async fn check_new_admission(
        &self, registry: &OwnerResourceRegistry, contribution: String,
    ) -> Result<(), NativeError> {
        super::super::key(&contribution)?;
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        if !Box::pin(self.supports_admission_receipts(&io)).await? {
            return Err(failure(409, "management_admission_receipts_required"));
        }
        let transport = AllocationTransport::new(self.manager.clone());
        let scope = transport.scope(io.permission).await?;
        let reply: ConnectionReply = transport.request(&scope,
            Operation::Contribution(contribution.clone()), false).await?;
        io.check().await?;
        enrollment::validate_admission(&reply, &io, &contribution)?;
        let current = &reply.admission.contribution;
        let history = History::load(&io.path, io.identity.clone())?;
        if history.data.records.iter().any(|r| r.original.binding.contribution_id == contribution
            && r.state == State::Recorded
            && r.original.admission.as_ref().is_some_and(|a| a.request_id == current.request_id)) {
            // The same admission may be granted again only for a new UTC month,
            // once every one of its recorded grants covers a month that ended.
            let void = Box::pin(self.void_successors(&io, registry, &contribution)).await?;
            if !history.admission_month_ended(&contribution, &current.request_id, now_ms()?, &void) {
                return Err(failure(409, "owner_grant_new_admission_required"));
            }
            if !Box::pin(self.supports_monthly_regrant(&io)).await? {
                return Err(failure(409, "management_monthly_regrant_required"));
            }
            Box::pin(self.reconcile_regrant(&io, registry, &contribution, &current.request_id))
                .await?;
        } else {
            Box::pin(self.reconcile_prior_admission(
                &io,
                registry,
                &contribution,
                &current.request_id,
            ))
            .await?;
        }
        let resource = registry.resource(&io.profile, current.resource_id.clone()).await.map_err(storage)?;
        if resource.revision != current.resource_revision {
            return Err(failure(409, "management_enrollment_changed"));
        }
        io.check().await
    }

    pub(super) async fn session_features(&self, io: &NativeIo<'_>) -> Result<Vec<Value>, NativeError> {
        let transport = AllocationTransport::new(self.manager.clone());
        let scope = transport.scope(io.permission).await?;
        let reply: Value = transport.request(&scope, Operation::Session, false).await?;
        io.check().await?;
        let features = reply.get("features").and_then(Value::as_array)
            .filter(|features| features.len() <= 64)
            .ok_or_else(|| failure(503, "management_features_invalid"))?;
        if features.iter().any(|v| v.as_str().is_none_or(|s| s.len() > 128)) {
            return Err(failure(503, "management_features_invalid"));
        }
        Ok(features.clone())
    }
    /// The server accepts one grant per v2 admission per UTC month. Without it,
    /// a second grant on an admission is refused even after its month ended.
    pub(super) async fn supports_monthly_regrant(
        &self,
        io: &NativeIo<'_>,
    ) -> Result<bool, NativeError> {
        let features = self.session_features(io).await?;
        Ok(features
            .iter()
            .any(|v| v.as_str() == Some("pool-monthly-regrant-v1")))
    }
    /// The server accepts v3 successors (`grants/{id}/successors`): an
    /// amendment of this month's grant or a renewal for the next month.
    pub(super) async fn supports_pool_renewal(&self, io: &NativeIo<'_>) -> Result<bool, NativeError> {
        Ok(self.session_features(io).await?.iter().any(|v| v.as_str() == Some("pool-renewal-policy-v1")))
    }
    pub(super) async fn supports_split_limits(&self, io: &NativeIo<'_>) -> Result<bool, NativeError> {
        Ok(self.session_features(io).await?.iter().any(|v| v.as_str() == Some("pool-grant-limits-v1")))
    }
    pub(super) async fn supports_calendar(&self, io: &NativeIo<'_>) -> Result<bool, NativeError> {
        Ok(self.session_features(io).await?.iter().any(|v| v.as_str() == Some("calendar-grants-v1")))
    }
    pub(super) async fn supports_succession(
        &self,
        io: &NativeIo<'_>,
    ) -> Result<bool, NativeError> {
        let features = self.session_features(io).await?;
        Ok(features
            .iter()
            .any(|v| v.as_str() == Some("pool-grant-succession-v1")))
    }
    pub(super) async fn supports_admission_receipts(
        &self,
        io: &NativeIo<'_>,
    ) -> Result<bool, NativeError> {
        let features = self.session_features(io).await?;
        // Issuance is supported before an owner has published their first grant.
        // Project reservation discovery additionally requires available capacity.
        let admission_bound = features.iter().any(|v| v.as_str() == Some("admission-bound-grants-v2"));
        if !admission_bound && !features.iter().any(|v| v.as_str() == Some("project-reservations-v1")) {
            return Err(failure(409, "management_admission_receipts_required"));
        }
        Ok(admission_bound)
    }
    pub(super) async fn observe_grant_admission(
        &self,
        io: &NativeIo<'_>,
        binding: &Binding,
    ) -> Result<AdmissionRound, NativeError> {
        io.check().await?;
        let transport = AllocationTransport::new(self.manager.clone());
        let scope = transport.scope(io.permission).await?;
        let reply: ConnectionReply = transport.request(
            &scope, Operation::Contribution(binding.contribution_id.clone()), false,
        ).await?;
        io.check().await?;
        enrollment::validate_admission(&reply, io, &binding.contribution_id)?;
        let c = &reply.admission.contribution;
        if c.resource_id != binding.resource_id
            || c.resource_revision != binding.resource_revision
            || c.coordinator_mxid != binding.admitted_coordinator
            || c.executor_generation != binding.executor_generation
            || c.owner_mxid != binding.owner_mxid
            || c.executor_device_id != binding.executor_device_id
        {
            return Err(failure(409, "management_enrollment_changed"));
        }
        enrollment::check_grant_enrollment(io, binding, &reply)?;
        Ok(AdmissionRound {
            request_id: c.request_id.clone(),
            request_revision: reply.admission.request.revision,
            contribution_revision: c.revision,
        })
    }

    pub(super) async fn check_grant_admission(
        &self,
        io: &NativeIo<'_>,
        original: &Original,
    ) -> Result<AdmissionRound, NativeError> {
        let observed = self.observe_grant_admission(io, &original.binding).await?;
        let same = match &original.admission {
            // The durable original pins the admission identity. A fresh review
            // may observe later verification revisions of that SAME admission;
            // each live confirmation still compares its own exact snapshot.
            Some(saved) => saved.request_id == observed.request_id,
            // Never assign today's request to an unbound historical intent.
            None => observed.contribution_revision == 2 && observed.request_revision == 2,
        };
        if !same {
            return Err(failure(409, "management_enrollment_changed"));
        }
        io.check().await?;
        Ok(observed)
    }
}
