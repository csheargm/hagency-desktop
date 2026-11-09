//! Management enrollment adapter. No legacy registration, room or agent is
//! fabricated. The wire DTOs contain data; only a trusted native observation
//! and an explicit local-owner consent can create an account-level hold.
#[cfg(test)]
mod grant_period_tests;
mod managed_agents;
pub use managed_agents::{budget, shared_execution};
mod parent;
mod publication;
mod readmission;
mod regrant;
mod succession;
mod successor;
#[cfg(test)]
mod succession_tests;
pub use successor::{CapacityGrantChainHead, CapacityGrantSuccession, CapacityGrantSuccessionFacts};
use super::*;
use crate::resource_publication_revision;
use hagency_core::project::identifier;
pub(in super::super) use managed_agents::unsettled_resource;
pub(in super::super) use managed_agents::unsettled_resource_except;
pub(in super::super) use managed_agents::unsettled_exact_resource;
pub use managed_agents::{
    InterruptedCall, InterruptedCallPage, InterruptedCallQuery, InterruptedCallReconciliation,
    ReconcileInterruptedCall,
    ManagedAgentPeriodRenewCommand, ManagedAgentPeriodReceipt, VerifiedManagedAgentPeriodDelivery,
    AGENT_SETTLEMENT_CAPABILITY, ManagedAgentSettlementCommand, ManagedAgentSettlementReceipt,
    VerifiedManagedAgentSettlement, AGENT_CAPABILITY, AGENT_FENCE_CAPABILITY,
    AGENT_MATRIX_CAPABILITY, AGENT_RUNTIME_CAPABILITY, ManagedAgentDefinition,
    ManagedAgentFenceCommand, ManagedAgentFenceReceipt, ManagedAgentMatrixChallenge,
    ManagedAgentMatrixObservation, ManagedAgentReservationReceipt, ManagedAgentReserveCommand,
    ManagedAssignmentAuthority, ManagedCallAuthority, ManagedCallClaim, ManagedCallCustody,
    ManagedCallHistory, ManagedCleanupAuthority, ManagedObservationReceipt, ManagedProvisionClaim,
    ManagedProvisionCustody, ManagedProvisionHistory, ManagedProvisionObservation,
    ManagedRuntimeAcknowledgement, ManagedRuntimeClaim, ManagedRuntimeCommand,
    ManagedRuntimeCustody, ManagedRuntimeDelivery, ManagedRuntimeEvidenceSink,
    ManagedRuntimeHistory, ManagedRuntimeProvisionCommand, ManagedRuntimeReadiness,
    ManagedRuntimeReceipt, ManagedRuntimeRecordedProcess, ManagedRuntimeRecoverInput,
    ManagedRuntimeResultInput, ManagedRuntimeStopCommand, ManagedUsageObservation,
    VerifiedManagedAgentDelivery, VerifiedManagedAgentFence, VerifiedManagedMatrixCheck,
    VerifiedManagedRuntimeDelivery,
};
pub use parent::{
    GrantClosureCommand, NativeGrantSettlementReceipt, PARENT_CAPABILITY, VerifiedParentDelivery,
};

pub const CAPABILITY: &str = "hagency.project_reservations.v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGrantReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal_mode: Option<hagency_core::recurring_quota::RenewalMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_request_tokens: Option<u64>,

    pub version: u16,
    pub capability: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_request_id: Option<String>,
    pub enrollment_id: String,
    pub grant_id: String,
    pub grant_revision: u64,
    pub contribution_id: String,
    pub resource_id: String,
    pub resource_revision: String,
    pub executor_device_id: String,
    pub executor_generation: u64,
    pub owner_mxid: String,
    pub coordinator_mxid: String,
    pub delegation_revision: u64,
    pub delegation_expires_at_ms: u64,
    pub allow_self_approval: bool,
    pub eligible_managers: Vec<String>,
    pub allocated_tokens: u64,
    pub period_key: String,
    pub period_start_ms: u64,
    pub period_end_ms: u64,
    pub ledger_revision: u64,
    /// Present exactly on v3 successor receipts. Omitted otherwise, so every
    /// stored v1/v2 receipt keeps its canonical digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub succession: Option<GrantSuccession>,
    /// V4 owner calendar. Absent from every historical v1–v3 digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar: Option<hagency_core::recurring_quota::Calendar>,
}
pub const SUCCESSION_RENEWAL: &str = "renewal";
pub const SUCCESSION_AMENDMENT: &str = "amendment";
/// The immutable link of a v3 receipt to the chain head it succeeds: a renewal
/// (next UTC month) or an amendment (same month). Identical to the server's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GrantSuccession {
    pub kind: String,
    pub predecessor_grant_id: String,
    /// Canonical digest of the predecessor's receipt.
    pub predecessor_receipt_digest: String,
    /// Live Agent children carried at native creation, and their hold.
    pub carried_children: u64,
    pub carried_retained_tokens: u64,
}
impl NativeGrantReceipt {
    /// Maximum new Agent allocation, independent of the pool total.
    pub fn request_limit(&self) -> u64 { self.per_request_tokens.unwrap_or(self.allocated_tokens) }
    /// Closed version shapes shared with the server. V4 also binds an exact
    /// owner calendar; old receipt digests never contain that field.
    pub fn supported_shape(&self) -> bool {
        if (self.version < 5 && (self.version == 4) != self.calendar.is_some())
            || (self.version >= 5) != self.per_request_tokens.is_some()
            || (self.version == 6) != self.renewal_mode.is_some()
            || (self.version == 6 && self.calendar.is_none())
            || self.per_request_tokens.is_some_and(|n| n == 0 || n > self.allocated_tokens || n > JSON_SAFE_MAX) {
            return false;
        }
        if let Some(calendar) = &self.calendar {
            let period = hagency_core::recurring_quota::Period {
                key: self.period_key.clone(),
                start_ms: self.period_start_ms,
                end_ms: self.period_end_ms,
            };
            if calendar.validate_period(&period).is_err() {
                return false;
            }
        }
        match (
            self.version,
            self.admission_request_id.as_ref(),
            self.succession.as_ref(),
        ) {
            (1, None, None) | (2 | 4 | 5 | 6, Some(_), None) => true,
            (3 | 4 | 5 | 6, Some(_), Some(s)) => {
                matches!(s.kind.as_str(), SUCCESSION_RENEWAL | SUCCESSION_AMENDMENT)
                    && identifier(&s.predecessor_grant_id, 128).is_ok()
                    && s.predecessor_grant_id != self.grant_id
                    && hash(&s.predecessor_receipt_digest).is_ok()
                    && s.carried_children <= JSON_SAFE_MAX
                    && s.carried_retained_tokens <= JSON_SAFE_MAX
                    && s.carried_retained_tokens <= self.allocated_tokens
                    && self.grant_revision >= 2
            }
            _ => false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AllocationCommand {
    pub version: u16,
    pub command_id: String,
    pub operation: String,
    pub allocation_id: String,
    pub project_id: String,
    pub project_revision: u64,
    pub project_definition_digest: String,
    pub request_id: String,
    pub request_revision: u64,
    pub decision_actor: String,
    pub manager_mxid: String,
    pub grant_receipt: NativeGrantReceipt,
    pub requested_tokens: u64,
    pub reserve_command_id: Option<String>,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeAllocationReceipt {
    pub version: u16,
    pub command_id: String,
    pub allocation_id: String,
    pub state: String,
    pub ledger_revision: u64,
    pub reserved_tokens: u64,
    pub retained_tokens: u64,
    pub period_key: String,
    pub reason: Option<String>,
}

/// A current SDK-authorized observation of the exact accepted enrollment.
/// Construct only after native transport verifies owner, installation, current
/// execution generation and canonical contribution/resource binding. Never
/// deserialize this type or synthesize it from a mini-app "approved" flag.
pub struct VerifiedManagementEnrollment {
    profile: ProfileAuthority,
    enrollment_id: String,
    contribution_id: String,
    resource_id: String,
    resource_revision: String,
    admitted_coordinator: String,
    executor_device_id: String,
    executor_generation: u64,
    admission_request_id: Option<String>,
    expires: Instant,
    verified_resource: Option<VerifiedOwnerResourceBinding>,
}

/// Current central lease for one exact executable delivery. Historical recovery
/// responses (executionAllowed=false) cannot construct this proof. Native
/// transport, not a mini app, owns the authenticated response and lease clock.
pub struct VerifiedAllocationDelivery {
    enrollment: VerifiedManagementEnrollment,
    command: AllocationCommand,
    digest: String,
    lease_expires: Instant,
}
impl VerifiedAllocationDelivery {
    pub fn new(
        enrollment: VerifiedManagementEnrollment,
        command: AllocationCommand,
        digest: String,
        execution_allowed: bool,
        lease_expires: Instant,
    ) -> Result<Self, Error> {
        enrollment.check()?;
        hash(&digest)?;
        let value = serde_json::to_value(&command)?;
        if !execution_allowed
            || lease_expires <= Instant::now()
            || lease_expires > Instant::now() + Duration::from_secs(60)
            || serde_json::to_vec(&value)?.len() > 32 * 1024
        {
            return Err(Error::LocalAuthority);
        }
        if canonical::digest(&value)? != digest {
            return Err(Error::Conflict);
        }
        Ok(Self {
            enrollment,
            command,
            digest,
            lease_expires,
        })
    }
}
impl VerifiedManagementEnrollment {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile: ProfileAuthority,
        enrollment_id: String,
        contribution_id: String,
        resource_id: String,
        resource_revision: String,
        admitted_coordinator: String,
        executor_device_id: String,
        executor_generation: u64,
        expires: Instant,
    ) -> Result<Self, Error> {
        for id in [
            &enrollment_id,
            &contribution_id,
            &resource_id,
            &executor_device_id,
        ] {
            identifier(id, 128)?;
        }
        hash(&resource_revision)?;
        local_user(&admitted_coordinator, &profile.account)?;
        exact(executor_generation)?;
        if expires > Instant::now() + Duration::from_secs(60) {
            return Err(Error::LocalAuthority);
        }
        let value = Self {
            profile,
            enrollment_id,
            contribution_id,
            resource_id,
            resource_revision,
            admitted_coordinator,
            executor_device_id,
            executor_generation,
            admission_request_id: None,
            expires,
            verified_resource: None,
        };
        value.check()?;
        Ok(value)
    }
    fn check(&self) -> Result<(), Error> {
        self.profile.check()?;
        if let Some(binding) = &self.verified_resource {
            binding.check()?;
        }
        if Instant::now() >= self.expires {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    /// Bind a native authenticated observation to its immutable admission
    /// request. This proof is never deserialized from a mini-app payload.
    pub fn with_admission_request(mut self, request_id: String) -> Result<Self, Error> {
        self.check()?;
        identifier(&request_id, 128)?;
        if self.admission_request_id.as_ref().is_some_and(|old| old != &request_id) {
            return Err(Error::Conflict);
        }
        self.admission_request_id = Some(request_id);
        Ok(self)
    }
    /// Restrict a transport observation to the actual native provider/resource
    /// binding. No JSON snapshot can construct this proof.
    pub fn with_verified_resource(
        mut self,
        binding: VerifiedOwnerResourceBinding,
    ) -> Result<Self, Error> {
        self.check()?;
        binding.check()?;
        if self.profile.digest != binding.profile.digest
            || self.profile.account != binding.profile.account
            || self.resource_id != binding.resource_id
            || self.resource_revision != binding.resource_revision
        {
            return Err(Error::LocalAuthority);
        }
        self.expires = self.expires.min(binding.expires);
        self.verified_resource = Some(binding);
        self.check()?;
        Ok(self)
    }
    fn matches(&self, receipt: &NativeGrantReceipt) -> bool {
        self.matches_cleanup(receipt)
            && self.resource_revision == receipt.resource_revision
            && self.executor_generation == receipt.executor_generation
            && self.admitted_coordinator == receipt.coordinator_mxid
    }
    fn matches_cleanup(&self, receipt: &NativeGrantReceipt) -> bool {
        receipt.supported_shape()
            && receipt.capability == CAPABILITY
            && self.admission_request_id == receipt.admission_request_id
            && self.enrollment_id == receipt.enrollment_id
            && self.contribution_id == receipt.contribution_id
            && self.resource_id == receipt.resource_id
            && self.executor_device_id == receipt.executor_device_id
            && self.profile.account == receipt.owner_mxid
    }
}

/// Native-owned fields displayed by the trusted confirmation. Scripts cannot
/// deserialize or approve this selection. The stored receipt is immutable;
/// grant amendment/renewal needs a separate, future revisioned consent flow.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerGrantSelection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub renewal_mode: Option<hagency_core::recurring_quota::RenewalMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_request_tokens: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub calendar: Option<hagency_core::recurring_quota::Calendar>,
    pub command_id: String,
    pub grant_id: String,
    pub coordinator_mxid: String,
    pub delegation_expires_at_ms: u64,
    pub allow_self_approval: bool,
    pub eligible_managers: Vec<String>,
    pub allocated_tokens: u64,
}
/// Frozen accounting period shown by native owner confirmation. Historical
/// grants use UTC months; V4 receipts bind their owner's calendar explicitly.
/// Constructed by the registry clock, never accepted from script input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerGrantPeriod {
    period_key: String,
    period_start_ms: u64,
    period_end_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    calendar: Option<hagency_core::recurring_quota::Calendar>,
}
impl OwnerGrantPeriod {
    pub fn calendar(&self) -> Option<&hagency_core::recurring_quota::Calendar> { self.calendar.as_ref() }
    pub fn period_key(&self) -> &str {
        &self.period_key
    }
    pub fn period_start_ms(&self) -> u64 {
        self.period_start_ms
    }
    pub fn period_end_ms(&self) -> u64 {
        self.period_end_ms
    }
    fn at(now: u64) -> Result<Self, Error> {
        let (period_key, period_start_ms, period_end_ms) = month(now)?;
        Ok(Self {
            period_key,
            period_start_ms,
            period_end_ms,
            calendar: None,
        })
    }
    fn at_calendar(now: u64, calendar: &hagency_core::recurring_quota::Calendar) -> Result<Self, Error> {
        let period = calendar.period_at(now).map_err(|_| Error::LocalAuthority)?;
        Ok(Self {
            period_key: period.key,
            period_start_ms: period.start_ms,
            period_end_ms: period.end_ms,
            calendar: Some(calendar.clone()),
        })
    }
    fn require_current(&self, now: u64) -> Result<(), Error> {
        let current = match &self.calendar {
            Some(calendar) => Self::at_calendar(now, calendar)?,
            None => Self::at(now)?,
        };
        if self != &current {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
    /// The exact period a receipt covers, rechecked against its calendar.
    fn of_receipt(receipt: &NativeGrantReceipt) -> Result<Self, Error> {
        if !receipt.supported_shape() {
            return Err(Error::LocalAuthority);
        }
        let period = match &receipt.calendar {
            Some(calendar) => Self::at_calendar(receipt.period_start_ms, calendar)?,
            None => Self::at(receipt.period_start_ms)?,
        };
        if period.period_key != receipt.period_key
            || period.period_start_ms != receipt.period_start_ms
            || period.period_end_ms != receipt.period_end_ms
        {
            return Err(Error::LocalAuthority);
        }
        Ok(period)
    }
    /// The next natural period in the same calendar.
    fn next(&self) -> Result<Self, Error> {
        match &self.calendar {
            Some(calendar) => Self::at_calendar(self.period_end_ms, calendar),
            None => Self::at(self.period_end_ms),
        }
    }
}
/// A renewal may be scheduled during the last 7 days of the current month.
pub const RENEWAL_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// Immutable native review data. This snapshot is not execution authority and
/// must never be accepted as a deserialized enrollment proof.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnerGrantBinding {
    pub registry_id: String,
    pub profile_digest: String,
    pub owner_mxid: String,
    pub enrollment_id: String,
    pub contribution_id: String,
    pub resource_id: String,
    pub resource_revision: String,
    pub admitted_coordinator: String,
    pub executor_device_id: String,
    pub executor_generation: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_resource_binding: Option<String>,
}
impl OwnerGrantConsent {
    pub fn binding(&self) -> Result<OwnerGrantBinding, Error> {
        self.check()?;
        let e = &self.enrollment;
        Ok(OwnerGrantBinding {
            registry_id: self.registry.id().into(),
            profile_digest: e.profile.digest.clone(),
            owner_mxid: e.profile.account.clone(),
            enrollment_id: e.enrollment_id.clone(),
            contribution_id: e.contribution_id.clone(),
            resource_id: e.resource_id.clone(),
            resource_revision: e.resource_revision.clone(),
            admitted_coordinator: e.admitted_coordinator.clone(),
            executor_device_id: e.executor_device_id.clone(),
            executor_generation: e.executor_generation,
            verified_resource_binding: e
                .verified_resource
                .as_ref()
                .map(|b| b.binding_digest())
                .transpose()?,
        })
    }
    /// Adds a restriction to the original proof. The native confirmation gate
    /// cannot replace or revive the enrollment/profile/expiry/period fences.
    /// It must not block or perform IO and is never supplied by a script.
    pub fn with_native_confirmation_gate(
        mut self,
        gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        self.check()?;
        let original = self.enrollment.profile.native_gate.take();
        self.enrollment.profile.native_gate = Some(Arc::new(move || {
            original.as_ref().is_none_or(|prior| prior()) && gate()
        }));
        self.check()?;
        Ok(self)
    }

    /// Restrict this consent to a contribution with no original grant history.
    /// The check is repeated inside the writer transaction; losing a native
    /// review journal cannot turn a previous grant into a fresh allocation.
    /// Restricts confirmation to the budget facts shown by the trusted native
    /// review. This can only remove authority; it neither changes original IDs
    /// nor replaces provider/profile/period checks. Receipts still replay first.
    pub fn with_reviewed_budget(
        mut self,
        budget: &hagency_core::allocation::Budget,
    ) -> Result<Self, Error> {
        self.check()?;
        self.reviewed_budget = Some(reviewed_budget_digest(budget)?);
        Ok(self)
    }

    pub fn require_unissued_contribution(mut self) -> Result<Self, Error> {
        self.check()?;
        self.require_unissued_contribution = true;
        Ok(self)
    }

    /// The native review supplies the admission it just verified. Existing v1
    /// originals never call this: their command digest and receipt stay v1.
    pub fn with_admission_request(mut self, request_id: String) -> Result<Self, Error> {
        self.enrollment = self.enrollment.with_admission_request(request_id)?;
        self.check()?;
        Ok(self)
    }

    /// A distinct v2 admission may follow permanently fenced earlier grants.
    /// Repeat the complete canonical check in the writer transaction; never
    /// replace the original unissued-history restriction with a flag-only test.
    pub fn require_closed_prior_admissions(mut self) -> Result<Self, Error> {
        self.check()?;
        if self.enrollment.admission_request_id.is_none() || self.regrant_after_period_end {
            return Err(Error::LocalAuthority);
        }
        self.require_unissued_contribution = true;
        self.closed_prior_admissions = true;
        Ok(self)
    }

    /// A new UTC month on the same v2 admission. Every earlier grant of the
    /// contribution must be history: this admission's grants only after their
    /// month ended, another admission's only after canonical closure. The
    /// writer repeats the complete check against the ledger rows; this flag
    /// alone never permits a second grant in the same month.
    pub fn require_regrant_after_period_end(mut self) -> Result<Self, Error> {
        self.check()?;
        if self.enrollment.admission_request_id.is_none() || self.closed_prior_admissions {
            return Err(Error::LocalAuthority);
        }
        self.require_unissued_contribution = true;
        self.regrant_after_period_end = true;
        Ok(self)
    }
}

pub struct OwnerGrantConsent {
    registry: OwnerResourceRegistry,
    enrollment: VerifiedManagementEnrollment,
    selection: OwnerGrantSelection,
    period: OwnerGrantPeriod,
    expires: Instant,
    require_unissued_contribution: bool,
    closed_prior_admissions: bool,
    regrant_after_period_end: bool,
    reviewed_budget: Option<String>,
    /// A v3 successor of this exact predecessor receipt; `None` for a root.
    succession: Option<successor::Plan>,
}
impl OwnerGrantConsent {
    pub fn selection(&self) -> &OwnerGrantSelection {
        &self.selection
    }
    pub fn period(&self) -> &OwnerGrantPeriod {
        &self.period
    }
    pub fn account(&self) -> &str {
        &self.enrollment.profile.account
    }
    pub async fn approve(self) -> Result<NativeGrantReceipt, Error> {
        self.check()?;
        let store = self.registry.0.store.clone();
        let profile = self.enrollment.profile.clone();
        let fence = self.registry.0.clone();
        let expires = self.expires.min(self.enrollment.expires);
        let result = store
            .call(32 * 1024, move |db| db.owner_capacity_grant(self))
            .await?;
        if profile.check().is_err()
            || fence.closed.load(Ordering::Acquire)
            || Instant::now() >= expires
        {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }
    fn check(&self) -> Result<(), Error> {
        self.enrollment.check()?;
        if self
            .enrollment
            .verified_resource
            .as_ref()
            .is_some_and(|b| b.registry_id != self.registry.id())
        {
            return Err(Error::LocalAuthority);
        }
        if self.registry.0.closed.load(Ordering::Acquire) || Instant::now() >= self.expires {
            return Err(Error::LocalAuthority);
        }
        Ok(())
    }
}
impl OwnerResourceRegistry {
    /// Read-only owner-calendar projection for native publication forms.
    pub async fn resource_calendar(&self, profile: ProfileAuthority, resource_id: String) -> Result<Option<hagency_core::recurring_quota::Calendar>, Error> {
        identifier(&resource_id,128)?;
        self.read(profile, move |db, profile| {
            authorize_resource(db, profile, &resource_id)?;
            crate::domain::resource_ceiling::calendar(db, &resource_id)
        }).await
    }

    /// Read-only history check for native pre-consent custody. A positive
    /// result is not a current capacity or execution-authority projection.
    pub async fn has_capacity_grant_for_contribution(
        &self,
        profile: ProfileAuthority,
        contribution_id: String,
    ) -> Result<bool, Error> {
        profile.check()?;
        identifier(&contribution_id, 128)?;
        let caller = profile.clone();
        let fence = self.0.clone();
        let result = self
            .0
            .store
            .call(1024, move |db| {
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                let result = contribution_grant_exists(&db.db, &profile, &contribution_id)?;
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                Ok(result)
            })
            .await?;
        caller.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }

    /// Recover the original immutable parent grant after a lost reply or
    /// restart. This is not a live-authority projection or a new owner consent.
    /// It cannot allocate, renew, amend or revive the parent hold.
    pub async fn recover_capacity_grant(
        &self,
        profile: ProfileAuthority,
        executor_device_id: String,
        grant_id: String,
    ) -> Result<Option<NativeGrantReceipt>, Error> {
        profile.check()?;
        identifier(&executor_device_id, 128)?;
        identifier(&grant_id, 128)?;
        let caller = profile.clone();
        let fence = self.0.clone();
        let result = self
            .0
            .store
            .call(16 * 1024, move |db| {
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                let row: Option<(String, String)> = db
                    .db
                    .query_row(
                        "SELECT profile,receipt FROM owner_capacity_grants WHERE id=?1",
                        [grant_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let result = if let Some((owner, value)) = row {
                    let receipt: NativeGrantReceipt = serde_json::from_str(&value)?;
                    if owner != profile.digest
                        || receipt.executor_device_id != executor_device_id
                        || receipt.owner_mxid != profile.account
                    {
                        return Err(Error::LocalAuthority);
                    }
                    Some(receipt)
                } else {
                    None
                };
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                Ok(result)
            })
            .await?;
        caller.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }

    pub fn prepare_capacity_grant(
        &self,
        enrollment: VerifiedManagementEnrollment,
        selection: OwnerGrantSelection,
        expires: Instant,
    ) -> Result<OwnerGrantConsent, Error> {
        self.prepare_capacity_grant_at(enrollment, selection, expires, graphs::now_ms()?)
    }

    // Private clock seam for exact-boundary store tests. Production callers
    // cannot choose a period: the public entrypoint always reads native UTC.
    fn prepare_capacity_grant_at(
        &self,
        enrollment: VerifiedManagementEnrollment,
        selection: OwnerGrantSelection,
        expires: Instant,
        now: u64,
    ) -> Result<OwnerGrantConsent, Error> {
        enrollment.check()?;
        identifier(&selection.command_id, 128)?;
        identifier(&selection.grant_id, 128)?;
        exact(selection.allocated_tokens)?;
        if selection.renewal_mode.is_some() && (selection.calendar.is_none() || selection.per_request_tokens.is_none()) { return Err(Error::LocalAuthority); }
        if selection.per_request_tokens.is_some_and(|n| n == 0 || n > selection.allocated_tokens || n > JSON_SAFE_MAX) { return Err(Error::LocalAuthority); }
        exact(selection.delegation_expires_at_ms)?;
        local_user(&selection.coordinator_mxid, &enrollment.profile.account)?;
        if selection.coordinator_mxid != enrollment.admitted_coordinator {
            return Err(Error::LocalAuthority);
        }
        if selection.eligible_managers.is_empty()
            || selection.eligible_managers.len() > 64
            || selection.eligible_managers.windows(2).any(|w| w[0] >= w[1])
        {
            return Err(Error::LocalAuthority);
        }
        for user in &selection.eligible_managers {
            local_user(user, &enrollment.profile.account)?;
        }
        if expires > Instant::now() + Duration::from_secs(60) {
            return Err(Error::LocalAuthority);
        }
        let period = match &selection.calendar {
            Some(calendar) => OwnerGrantPeriod::at_calendar(now, calendar)?,
            None => OwnerGrantPeriod::at(now)?,
        };
        let consent = OwnerGrantConsent {
            registry: self.clone(),
            enrollment,
            selection,
            period,
            expires,
            require_unissued_contribution: false,
            closed_prior_admissions: false,
            regrant_after_period_end: false,
            reviewed_budget: None,
            succession: None,
        };
        consent.check()?;
        Ok(consent)
    }

    /// The native server transport must verify the leased command came from the
    /// enrollment's authenticated server. A parsed command alone is no authority.
    pub async fn apply_project_allocation(
        &self,
        delivery: VerifiedAllocationDelivery,
    ) -> Result<NativeAllocationReceipt, Error> {
        let VerifiedAllocationDelivery {
            enrollment,
            command,
            digest: command_digest,
            lease_expires,
        } = delivery;
        enrollment.check()?;
        if Instant::now() >= lease_expires {
            return Err(Error::LocalAuthority);
        }
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        let fence = self.0.clone();
        let profile = enrollment.profile.clone();
        let result = self
            .0
            .store
            .call(32 * 1024, move |db| {
                db.owner_project_allocation(
                    enrollment,
                    command,
                    command_digest,
                    fence,
                    lease_expires,
                    graphs::now_ms()?,
                )
            })
            .await?;
        if profile.check().is_err() || self.0.closed.load(Ordering::Acquire) {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }

    /// Historical delivery recovery uses fresh authority for the same profile
    /// and installation. It cannot apply work, re-create a hold or revive a grant.
    pub async fn recover_project_allocation(
        &self,
        profile: ProfileAuthority,
        executor_device_id: String,
        command_id: String,
        command_digest: String,
    ) -> Result<Option<NativeAllocationReceipt>, Error> {
        profile.check()?;
        identifier(&executor_device_id, 128)?;
        identifier(&command_id, 128)?;
        hash(&command_digest)?;
        let fence = self.0.clone();
        let caller_profile = profile.clone();
        let result = self
            .0
            .store
            .call(8192, move |db| {
                profile.check()?;
                if fence.closed.load(Ordering::Acquire) {
                    return Err(Error::LocalAuthority);
                }
                let result = receipt(
                    &db.db,
                    &profile.digest,
                    &executor_device_id,
                    &command_id,
                    &command_digest,
                )?;
                profile.check()?;
                Ok(result)
            })
            .await?;
        caller_profile.check()?;
        if self.0.closed.load(Ordering::Acquire) {
            return Err(Error::LocalAuthority);
        }
        Ok(result)
    }
}

fn contribution_grant_exists(
    db: &rusqlite::Connection,
    profile: &ProfileAuthority,
    contribution: &str,
) -> Result<bool, Error> {
    // The writer caps the complete parent table at 2048 rows. Preserve a
    // bounded fail-closed read even for an externally altered database.
    let mut query = db.prepare(
        "SELECT receipt FROM owner_capacity_grants WHERE profile=?1 ORDER BY id LIMIT 2049",
    )?;
    let mut rows = query.query([&profile.digest])?;
    for index in 0..=2048 {
        let Some(row) = rows.next()? else {
            return Ok(false);
        };
        profile.check()?;
        if index == 2048 {
            return Err(Error::Capacity);
        }
        let bytes: String = row.get(0)?;
        if bytes.len() > 12 * 1024 {
            return Err(Error::Capacity);
        }
        let receipt: NativeGrantReceipt = serde_json::from_str(&bytes)?;
        if receipt.owner_mxid != profile.account {
            return Err(Error::LocalAuthority);
        }
        if receipt.contribution_id == contribution {
            return Ok(true);
        }
    }
    Err(Error::Capacity)
}

fn exact(value: u64) -> Result<(), Error> {
    if value == 0 || value > JSON_SAFE_MAX {
        Err(InvalidInput("invalid allocation integer").into())
    } else {
        Ok(())
    }
}
fn local_user(user: &str, owner: &str) -> Result<(), Error> {
    palpo_hagency_contract::MatrixUserId::try_from(user.to_owned())
        .map_err(|_| Error::LocalAuthority)?;
    if user.split_once(':').map(|v| v.1) != owner.split_once(':').map(|v| v.1) {
        return Err(Error::LocalAuthority);
    }
    Ok(())
}
fn revision(db: &Connection) -> Result<u64, Error> {
    let old: u64 = db.query_row(
        "SELECT revision FROM owner_resource_identity WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    if old >= JSON_SAFE_MAX {
        return Err(Error::Capacity);
    }
    db.execute(
        "UPDATE owner_resource_identity SET revision=revision+1 WHERE singleton=1",
        [],
    )?;
    Ok(old + 1)
}
fn month(now: u64) -> Result<(String, u64, u64), Error> {
    // Legacy receipts remain UTC monthly. New recurring authorizations use the
    // same calendar implementation with their explicitly approved owner zone.
    let period = hagency_core::recurring_quota::Calendar {
        cadence: hagency_core::recurring_quota::Cadence::Monthly,
        time_zone: "UTC".into(),
    }
    .period_at(now)
    .map_err(|_| Error::LocalAuthority)?;
    Ok((period.key, period.start_ms, period.end_ms))
}

fn resource_revision(db: &Connection, id: &str) -> Result<String, Error> {
    resource_publication_revision(&read_resource(db, id)?)
}
fn receipt<T: serde::de::DeserializeOwned>(
    db: &Connection,
    profile: &str,
    device: &str,
    command: &str,
    digest: &str,
) -> Result<Option<T>, Error> {
    let old:Option<(String,String,String,String)>=db.query_row("SELECT profile,executor_device_id,digest,result FROM owner_capacity_receipts WHERE command_id=?1",[command],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    match old {
        None => Ok(None),
        Some((p, d, h, v)) => {
            if p != profile || d != device {
                return Err(Error::LocalAuthority);
            }
            if h != digest {
                return Err(Error::Conflict);
            }
            Ok(Some(serde_json::from_str(&v)?))
        }
    }
}
fn save<T: Serialize>(
    db: &Connection,
    profile: &str,
    device: &str,
    command: &str,
    digest: &str,
    value: &T,
) -> Result<(), Error> {
    bounded_row(
        db,
        "owner_capacity_receipts",
        "command_id",
        command,
        100_000,
    )?;
    db.execute(
        "INSERT INTO owner_capacity_receipts VALUES(?1,?2,?3,?4,?5)",
        params![
            command,
            profile,
            device,
            digest,
            serde_json::to_string(value)?
        ],
    )?;
    Ok(())
}
fn reviewed_budget_digest(budget: &hagency_core::allocation::Budget) -> Result<String, Error> {
    canonical::digest(&json!({
        "remaining": budget.remaining_tokens,
        "resourceTotal": budget.pool.ceiling,
        "resourcePeriod": budget.pool.period,
        "accountTotal": budget.seat.quota,
        "accountPeriod": budget.seat.period,
        "accountStatus": budget.seat.status,
    }))
    .map_err(Into::into)
}

impl DomainRepository {
    fn owner_capacity_grant(
        &mut self,
        consent: OwnerGrantConsent,
    ) -> Result<NativeGrantReceipt, Error> {
        self.owner_capacity_grant_with_clock(consent, graphs::now_ms)
    }

    fn owner_capacity_grant_with_clock(
        &mut self,
        consent: OwnerGrantConsent,
        mut now_ms: impl FnMut() -> Result<u64, Error>,
    ) -> Result<NativeGrantReceipt, Error> {
        if consent.succession.is_some() {
            return self.owner_capacity_grant_successor_with_clock(consent, now_ms);
        }
        consent.check()?;
        let e = &consent.enrollment;
        let s = &consent.selection;
        let mut input = json!({"operation":"owner.capacity.grant","profile":e.profile.digest,"enrollment":e.enrollment_id,"contribution":e.contribution_id,"resource":e.resource_id,"resourceRevision":e.resource_revision,"device":e.executor_device_id,"generation":e.executor_generation,"selection":s,"period":consent.period});
        if let Some(request) = &e.admission_request_id {
            input["admissionRequestId"] = json!(request);
        }
        let digest = canonical::digest(&input)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = receipt(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &s.command_id,
            &digest,
        )? {
            consent.check()?;
            return Ok(old);
        }
        if consent.closed_prior_admissions {
            readmission::closed_grants(&tx, &e.profile, &e.executor_device_id,
                &e.contribution_id, Some(e.admission_request_id.as_deref().ok_or(Error::LocalAuthority)?), Some(&e.enrollment_id))
                .map_err(|error| if matches!(error, Error::Conflict) { Error::OwnerGrantReviewChanged } else { error })?;
        } else if consent.require_unissued_contribution
            && !consent.regrant_after_period_end
            && contribution_grant_exists(&tx, &e.profile, &e.contribution_id)?
        {
            return Err(Error::Conflict);
        }
        authorize_resource(&tx, &e.profile, &e.resource_id)?;
        if let Some(binding) = &e.verified_resource {
            binding.check_canonical(&tx, consent.registry.id())?;
        }
        if resource_revision(&tx, &e.resource_id)? != e.resource_revision {
            return Err(Error::Generation);
        }
        let resource = read_resource(&tx, &e.resource_id)?;
        let now = now_ms()?;
        consent.period.require_current(now)?;
        if consent.regrant_after_period_end {
            regrant::prior_periods_ended(
                &tx,
                &e.profile,
                &e.executor_device_id,
                &e.contribution_id,
                e.admission_request_id
                    .as_deref()
                    .ok_or(Error::LocalAuthority)?,
                Some(&e.enrollment_id),
                &consent.period,
                now,
            )
            .map_err(|error| {
                if matches!(error, Error::Conflict) {
                    Error::OwnerGrantReviewChanged
                } else {
                    error
                }
            })?;
        }
        if let Some(reviewed) = &consent.reviewed_budget {
            let current = super::super::budget_at(&tx, &resource, None, false, now)?;
            if &reviewed_budget_digest(&current)? != reviewed {
                return Err(Error::OwnerGrantReviewChanged);
            }
        }
        if s.delegation_expires_at_ms <= now
            || s.delegation_expires_at_ms > now.saturating_add(366 * 24 * 60 * 60 * 1000)
        {
            return Err(Error::LocalAuthority);
        }
        if (s.calendar.is_some() || s.per_request_tokens.is_some()) && e.admission_request_id.is_none() { return Err(Error::LocalAuthority); }
        crate::domain::resource_ceiling::bind_calendar(&tx, &e.resource_id, &e.profile.digest, &s.grant_id, s.calendar.as_ref(), now)?;
        check_grant(&tx, &resource, &s.grant_id, s.allocated_tokens, now)?;
        // The monthly maximum in force, including an owner's native amendment.
        if s.calendar.is_none() && serde_json::to_value(
            crate::domain::resource_ceiling::effective_ceiling(&tx, &resource)?
                .ok_or(Error::NoCeiling)?
                .period,
        )? != json!("monthly")
        {
            return Err(Error::NoCeiling);
        }
        let ledger_revision = revision(&tx)?;
        let result = NativeGrantReceipt { renewal_mode: s.renewal_mode, per_request_tokens: s.per_request_tokens,
            version: if s.renewal_mode.is_some() { 6 } else if s.per_request_tokens.is_some() { 5 } else if s.calendar.is_some() { 4 } else if e.admission_request_id.is_some() { 2 } else { 1 },
            calendar: s.calendar.clone(),
            capability: CAPABILITY.into(),
            admission_request_id: e.admission_request_id.clone(),
            enrollment_id: e.enrollment_id.clone(),
            grant_id: s.grant_id.clone(),
            grant_revision: 1,
            contribution_id: e.contribution_id.clone(),
            resource_id: e.resource_id.clone(),
            resource_revision: e.resource_revision.clone(),
            executor_device_id: e.executor_device_id.clone(),
            executor_generation: e.executor_generation,
            owner_mxid: e.profile.account.clone(),
            coordinator_mxid: s.coordinator_mxid.clone(),
            delegation_revision: 1,
            delegation_expires_at_ms: s.delegation_expires_at_ms,
            allow_self_approval: s.allow_self_approval,
            eligible_managers: s.eligible_managers.clone(),
            allocated_tokens: s.allocated_tokens,
            period_key: consent.period.period_key.clone(),
            period_start_ms: consent.period.period_start_ms,
            period_end_ms: consent.period.period_end_ms,
            ledger_revision,
            succession: None,
        };
        // Shared with the central 16-KiB request envelope; leave space for its
        // command/revision wrapper and for the later executor command fields.
        if serde_json::to_vec(&result)?.len() > 12 * 1024 {
            return Err(Error::Capacity);
        }
        super::provenance::offered(&tx, consent.registry.id(), &e.profile.digest, &e.resource_id, &s.command_id, &e.resource_revision, super::provenance::OfferKind::CapacityGrant)?;
        bounded_row(&tx, "owner_capacity_grants", "id", &s.grant_id, 2048)?;
        tx.execute(
            "INSERT INTO owner_capacity_grants(id,profile,resource_id,preset_id,seat_id,allocated_tokens,receipt,active,accounting_mode) VALUES(?1,?2,?3,?4,?5,?6,?7,1,?8)",
            params![
                s.grant_id,
                e.profile.digest,
                e.resource_id,
                resource.preset_id,
                resource.seat_id,
                s.allocated_tokens,
                serde_json::to_string(&result)?,
                parent::RESERVATION_ONLY
            ],
        )?;
        save(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &s.command_id,
            &digest,
            &result,
        )?;
        consent.period.require_current(now_ms()?)?;
        consent.check()?;
        tx.commit()?;
        consent.check().map_err(|_| Error::OutcomeUnknown)?;
        Ok(result)
    }

    #[allow(clippy::too_many_arguments)] // All values are native-owned; one transaction rechecks every fence.
    pub(super) fn owner_project_allocation(
        &mut self,
        e: VerifiedManagementEnrollment,
        c: AllocationCommand,
        digest: String,
        fence: Arc<Inner>,
        lease_expires: Instant,
        now: u64,
    ) -> Result<NativeAllocationReceipt, Error> {
        e.check()?;
        if fence.closed.load(Ordering::Acquire) || Instant::now() >= lease_expires {
            return Err(Error::LocalAuthority);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(old) = receipt(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &c.command_id,
            &digest,
        )? {
            e.check()?;
            if fence.closed.load(Ordering::Acquire) || Instant::now() >= lease_expires {
                return Err(Error::LocalAuthority);
            }
            return Ok(old);
        }
        let invalid = validate_command(&c).is_err();
        let reason = if invalid {
            Some("invalid_request")
        } else if !(if c.operation == "project.release" {
            e.matches_cleanup(&c.grant_receipt)
        } else {
            e.matches(&c.grant_receipt)
        }) {
            Some("authority_changed")
        } else {
            None
        };
        let reason = if let Some(reason) = reason {
            Some(reason)
        } else {
            check_command_authority(&tx, &e, &c, now)?
        };
        let mut result = NativeAllocationReceipt {
            version: 1,
            command_id: c.command_id.clone(),
            allocation_id: c.allocation_id.clone(),
            state: "refused".into(),
            ledger_revision: 0,
            reserved_tokens: 0,
            retained_tokens: 0,
            period_key: c.grant_receipt.period_key.clone(),
            reason: reason.map(str::to_owned),
        };
        if result.reason.is_none() {
            apply_child(&tx, &c, &digest, &mut result)?;
        }
        result.ledger_revision = revision(&tx)?;
        save(
            &tx,
            &e.profile.digest,
            &e.executor_device_id,
            &c.command_id,
            &digest,
            &result,
        )?;
        e.check()?;
        if fence.closed.load(Ordering::Acquire) || Instant::now() >= lease_expires {
            return Err(Error::LocalAuthority);
        }
        tx.commit()?;
        if e.check().is_err() || fence.closed.load(Ordering::Acquire) {
            return Err(Error::OutcomeUnknown);
        }
        Ok(result)
    }
}
fn validate_command(c: &AllocationCommand) -> Result<(), Error> {
    for id in [
        &c.command_id,
        &c.allocation_id,
        &c.project_id,
        &c.request_id,
    ] {
        identifier(id, 128)?;
    }
    for n in [
        c.project_revision,
        c.request_revision,
        c.requested_tokens,
        c.issued_at_ms,
        c.expires_at_ms,
    ] {
        exact(n)?;
    }
    hash(&c.project_definition_digest)?;
    if c.version != 1
        || !matches!(c.operation.as_str(), "project.reserve" | "project.release")
        || c.expires_at_ms <= c.issued_at_ms
    {
        return Err(Error::LocalAuthority);
    }
    if c.operation == "project.reserve" {
        if c.reserve_command_id.is_some() {
            return Err(Error::LocalAuthority);
        }
    } else {
        identifier(
            c.reserve_command_id
                .as_deref()
                .ok_or(Error::LocalAuthority)?,
            128,
        )?;
    }
    Ok(())
}
fn check_command_authority(
    tx: &Connection,
    e: &VerifiedManagementEnrollment,
    c: &AllocationCommand,
    now: u64,
) -> Result<Option<&'static str>, Error> {
    let g = &c.grant_receipt;
    let stored: Option<(String, String, bool, String)> = tx
        .query_row(
            "SELECT profile,receipt,active,accounting_mode FROM owner_capacity_grants WHERE id=?1",
            [&g.grant_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((profile, stored, active, accounting_mode)) = stored else {
        return Ok(Some("authority_changed"));
    };
    if profile != e.profile.digest
        || canonical::digest(&serde_json::from_str::<serde_json::Value>(&stored)?)?
            != canonical::digest(&serde_json::to_value(g)?)?
    {
        return Ok(Some("authority_changed"));
    }
    // Cleanup uses the immutable original binding even after a local access
    // revoke or grant expiry. It can only release this child's unused hold.
    if c.operation == "project.release" {
        return Ok(None);
    }
    let closed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM owner_parent_settlements WHERE grant_id=?1)",
        [&g.grant_id],
        |r| r.get(0),
    )?;
    if !active
        || closed
        || !matches!(
            accounting_mode.as_str(),
            parent::RESERVATION_ONLY | managed_agents::MANAGED
        )
        || authorize_resource(tx, &e.profile, &e.resource_id).is_err()
        || resource_revision(tx, &e.resource_id)? != e.resource_revision
        || now < c.issued_at_ms
        || now >= c.expires_at_ms
        || now < g.period_start_ms
        || now >= g.period_end_ms
        || now >= g.delegation_expires_at_ms
        // The owner retains authority over capacity they supplied. The recorded
        // coordinator remains valid for commands accepted before the role merge.
        || (c.decision_actor != g.owner_mxid && c.decision_actor != g.coordinator_mxid)
        || g.eligible_managers.binary_search(&c.manager_mxid).is_err()
        || (!g.allow_self_approval && c.decision_actor == c.manager_mxid)
    {
        return Ok(Some("authority_changed"));
    }
    if managed_agents::unsettled_resource(tx, &read_resource(tx, &e.resource_id)?)? {
        return Ok(Some("unsettled_usage"));
    }
    Ok(None)
}
fn child_binding(c: &AllocationCommand) -> Result<String, Error> {
    canonical::digest(&json!({"grantId":c.grant_receipt.grant_id,"projectId":c.project_id,"projectRevision":c.project_revision,"definitionDigest":c.project_definition_digest,"manager":c.manager_mxid,"tokens":c.requested_tokens})).map_err(Into::into)
}
fn apply_child(
    tx: &Connection,
    c: &AllocationCommand,
    digest: &str,
    result: &mut NativeAllocationReceipt,
) -> Result<(), Error> {
    let binding = child_binding(c)?;
    let other:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM owner_project_reservations WHERE grant_id=?1 AND project_id=?2 AND project_revision=?3 AND id<>?4)",params![c.grant_receipt.grant_id,c.project_id,c.project_revision,c.allocation_id],|r|r.get(0))?;
    if other {
        result.reason = Some("request_conflict".into());
        return Ok(());
    }
    let old:Option<(String,String,u64,String,String)>=tx.query_row("SELECT reserve_command_id,state,retained_tokens,binding_digest,accounting_mode FROM owner_project_reservations WHERE id=?1",[&c.allocation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
    if c.operation == "project.reserve" {
        if let Some((_, state, _, old_binding, _)) = old {
            result.reason = Some(
                if state == "released" && old_binding == binding {
                    "project_unavailable"
                } else {
                    "request_conflict"
                }
                .into(),
            );
            return Ok(());
        }
        let held = managed_agents::grant_floor(tx, &c.grant_receipt.grant_id)?.retained;
        if c.requested_tokens > c.grant_receipt.allocated_tokens.saturating_sub(held) {
            result.reason = Some("insufficient_capacity".into());
            return Ok(());
        }
        bounded_row(
            tx,
            "owner_project_reservations",
            "id",
            &c.allocation_id,
            100_000,
        )?;
        tx.execute(
            "INSERT INTO owner_project_reservations(id,grant_id,project_id,project_revision,reserve_command_id,reserve_digest,requested_tokens,retained_tokens,state,binding_digest,accounting_mode) VALUES(?1,?2,?3,?4,?5,?6,?7,?7,'reserved',?8,?9)",
            params![
                c.allocation_id,
                c.grant_receipt.grant_id,
                c.project_id,
                c.project_revision,
                c.command_id,
                digest,
                c.requested_tokens,
                binding,
                parent::RESERVATION_ONLY
            ],
        )?;
        managed_agents::save_project_authority(tx, c, digest)?;
        result.state = "reserved".into();
        result.reserved_tokens = c.requested_tokens;
        result.retained_tokens = c.requested_tokens;
    } else {
        let original = c
            .reserve_command_id
            .as_deref()
            .ok_or(Error::LocalAuthority)?;
        // Zero-consumption cleanup is valid only for the explicit schema mode
        // whose complete API has no execution/agent attachment path. Migration
        // defaults and future modes cannot establish absence of liabilities.
        let parent_mode: String = tx.query_row(
            "SELECT accounting_mode FROM owner_capacity_grants WHERE id=?1",
            [&c.grant_receipt.grant_id],
            |r| r.get(0),
        )?;
        if let Some((reserve, _, _, old_binding, mode)) = &old {
            if reserve != original || old_binding != &binding {
                result.reason = Some("request_conflict".into());
                return Ok(());
            }
            if mode == managed_agents::MANAGED
                && matches!(
                    parent_mode.as_str(),
                    parent::RESERVATION_ONLY | managed_agents::MANAGED
                )
            {
                return managed_agents::release_project(tx, c, result);
            }
        }
        if !matches!(
            parent_mode.as_str(),
            parent::RESERVATION_ONLY | managed_agents::MANAGED
        ) || old.as_ref().is_some_and(|(_, state, retained, _, mode)| {
            mode != parent::RESERVATION_ONLY
                || (state == "reserved" && *retained != c.requested_tokens)
                || (state == "released" && *retained != 0)
        }) {
            result.reason = Some("unsettled_usage".into());
            return Ok(());
        }
        if let Some((reserve, _, _, old_binding, _)) = old {
            if reserve != original || old_binding != binding {
                result.reason = Some("request_conflict".into());
                return Ok(());
            }
            // No execution or agent attachment API exists for these children.
            // When added, it must atomically retain consumed/unknown usage here.
            tx.execute("UPDATE owner_project_reservations SET state='released',retained_tokens=0 WHERE id=?1",[&c.allocation_id])?;
        } else {
            bounded_row(
                tx,
                "owner_project_reservations",
                "id",
                &c.allocation_id,
                100_000,
            )?;
            tx.execute("INSERT INTO owner_project_reservations(id,grant_id,project_id,project_revision,reserve_command_id,reserve_digest,requested_tokens,retained_tokens,state,binding_digest,accounting_mode) VALUES(?1,?2,?3,?4,?5,NULL,?6,0,'released',?7,?8)",params![c.allocation_id,c.grant_receipt.grant_id,c.project_id,c.project_revision,original,c.requested_tokens,binding,parent::RESERVATION_ONLY])?;
        }
        result.state = "released".into();
        result.reserved_tokens = c.requested_tokens;
    }
    Ok(())
}

/// Only parent holds are added to the existing allocation engine. Project
/// children spend that already-counted capacity; ACL changes do not silently
/// free it or erase unknown/legacy commitments. The ceiling is monthly, and a
/// pool's capacity is a chain of grants (section 3.5): a grant counts in its
/// own UTC month; once that month ended before `at` it keeps only its
/// children's unresolved liability, or their whole retained hold while any of
/// their usage is unknown. Each child counts in exactly one grant: the
/// effective grant of its chain at `min(at, fenced_at)`. A chain of length 1
/// gives exactly the numbers of a single grant.
pub(in super::super) fn additional_commitments(
    db: &Connection,
    resource: &Resource,
    at: u64,
) -> Result<Vec<allocation::Commitment>, Error> {
    let mut q=db.prepare("SELECT g.id,g.preset_id,g.seat_id,COALESCE(p.retained_tokens,g.allocated_tokens),CASE WHEN json_valid(g.receipt) AND json_type(g.receipt,'$.periodEndMs')='integer' THEN json_extract(g.receipt,'$.periodEndMs') END,CASE WHEN json_valid(g.receipt) AND json_type(g.receipt,'$.periodStartMs')='integer' THEN json_extract(g.receipt,'$.periodStartMs') END FROM owner_capacity_grants g LEFT JOIN owner_parent_settlements p ON p.grant_id=g.id WHERE g.preset_id=?1 OR g.seat_id=?2")?;
    let rows = q.query_map(params![resource.preset_id, resource.seat_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, u64>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
        ))
    })?;
    rows.map(|row| {
        let (id, preset, seat, tokens, period_end, period_start) = row?;
        let tokens = grant_commitment(db, &id, tokens, period_start, period_end, at)?;
        Ok(allocation::Commitment {
            id: format!("owner_grant_{id}"),
            preset_id: Some(preset),
            seat_id: Some(seat),
            allocated_tokens: Some(tokens.try_into()?),
            state: "active".into(),
            fulfillment: None,
        })
    })
    .collect()
}
/// One grant's hold at `at` (section 3.5, with the same-period floor). `tokens`
/// is its allocation, or its settled retained amount once canonically closed.
/// Totals never increase when an amendment flips: while it is pending the
/// head counts `max(A_g, A_s, floor)` and the successor 0; afterwards the
/// successor heads the month with `max(A_s, floor)` and the old head counts 0.
fn grant_commitment(
    db: &Connection,
    id: &str,
    tokens: u64,
    period_start: Option<i64>,
    period_end: Option<i64>,
    at: u64,
) -> Result<u64, Error> {
    if let Some(link) = succession::row(db, id)? {
        // Rejected successors and cancelled renewals never held capacity.
        if link.state.void() {
            return Ok(0);
        }
        // A renewal counts only from its own month. A receipt without a
        // readable start is never treated as future.
        if link.renewal()
            && period_start.is_some_and(|start| u64::try_from(start).is_ok_and(|start| start > at))
        {
            return Ok(0);
        }
        // An amendment not yet effective at `at` counts inside its
        // predecessor's hold.
        if link.amendment()
            && (link.state == succession::State::Pending || link.effective_from_ms > at)
        {
            return Ok(0);
        }
    }
    // A receipt without a readable period end is never treated as ended.
    let ended = period_end.is_some_and(|end| u64::try_from(end).is_ok_and(|end| end <= at));
    if ended {
        let floor = managed_agents::accounted_floor(db, id, at)?.total;
        return Ok(if floor.complete {
            floor.unresolved
        } else {
            floor.retained
        });
    }
    let next = succession::live_successor(db, id)?;
    // Superseded within its month: the grant now heading the month counts
    // this grant's children in its same-period floor.
    if next.as_ref().is_some_and(|s| {
        s.amendment() && s.state == succession::State::Effective && s.effective_from_ms <= at
    }) {
        return Ok(0);
    }
    let current = tokens.max(managed_agents::period_floor(db, id, at)?.total.retained);
    Ok(match next {
        Some(s)
            if s.amendment()
                && (s.state == succession::State::Pending || s.effective_from_ms > at) =>
        {
            let successor: u64 = db.query_row(
                "SELECT allocated_tokens FROM owner_capacity_grants WHERE id=?1",
                [&s.grant_id],
                |r| r.get(0),
            )?;
            current.max(successor)
        }
        _ => current,
    })
}
