//! Private owner-grant intent custody. No mini-app capability or source-adoption
//! initializer is enabled. Native hosts own the explicit publication confirmation.
use super::{NativeError, NativeManagement, failure};
use crate::console::server_login::matrix_creations::service::Scope as MatrixScope;
use crate::console::{AuthorizedDevice, device_execution::DeviceIdentity};
use hagency_store::{
    management::{HostContext, Permission, Session},
    owner_resources::*,
};
use serde::{Deserialize, Serialize};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
include!("owner_grant/types.rs");
include!("owner_grant/journal.rs");
mod enrollment;
mod admission;
pub(super) mod reoffer;
mod cancellation;
pub use cancellation::{OwnerGrantCancellationReview, OwnerGrantCancellationResult};
mod succession;
mod recurring;
mod publication;
pub use publication::{OwnerGrantPublicationProgress, OwnerGrantPublicationReview, OwnerGrantPublicationResult, OwnerGrantPublicationState};
use publication::PublicationIntent;

fn history_error() -> NativeError {
    failure(409, "owner_grant_history_unavailable")
}
fn storage(e: hagency_store::Error) -> NativeError {
    super::management::store_error(e)
}
fn positive(value: u64) -> Result<(), NativeError> {
    if value == 0 || value > hagency_core::JSON_SAFE_MAX {
        Err(history_error())
    } else {
        Ok(())
    }
}
fn hash(value: &str) -> Result<(), NativeError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Err(history_error())
    } else {
        Ok(())
    }
}
fn profile_digest(identity: &Identity) -> Result<String, NativeError> {
    crate::console::owned_agents::profile_identity(
        &identity.origin,
        &identity.issuer,
        &identity.subject,
        &identity.owner,
    )
    .map_err(|_| history_error())
}
fn same_server_user(user: &str, owner: &str) -> Result<(), NativeError> {
    if user.len() > 255
        || user.chars().any(|c| c.is_whitespace() || c.is_control())
        || !user.split_once(':').is_some_and(|(name, server)| {
            name.starts_with('@')
                && name.len() > 1
                && Some(server) == owner.split_once(':').map(|p| p.1)
        })
    {
        Err(history_error())
    } else {
        Ok(())
    }
}
fn digest(value: &impl Serialize) -> Result<String, NativeError> {
    hagency_core::canonical::digest(&serde_json::to_value(value).map_err(|_| history_error())?)
        .map_err(|_| history_error())
}
fn random_id() -> Result<String, NativeError> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|_| failure(503, "native_random_unavailable"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
fn now_ms() -> Result<u64, NativeError> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| history_error())?
        .as_millis();
    u64::try_from(ms).map_err(|_| history_error())
}

struct NativeIo<'a> {
    manager: &'a NativeManagement,
    matrix: MatrixScope<'a>,
    context: HostContext,
    session: Session,
    device: AuthorizedDevice,
    profile: ProfileAuthority,
    identity: Identity,
    path: std::path::PathBuf,
    permission: Permission,
    deadline: Instant,
    host_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl NativeIo<'_> {
    fn gate(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        let session = self.session.clone();
        let context = self.context.clone();
        let device = self.device.clone();
        let permission = self.permission;
        let host_gate = self.host_gate.clone();
        move || {
            host_gate.as_ref().is_none_or(|gate| gate())
                && device.bearer().is_ok()
                && session
                    .authorize(&context, Permission::ReadResources)
                    .is_ok()
                && session.authorize(&context, permission).is_ok()
        }
    }
    async fn check(&self) -> Result<(), NativeError> {
        if Instant::now() >= self.deadline || !(self.gate())() {
            return Err(failure(401, "management_scope_changed"));
        }
        self.matrix
            .check()
            .await
            .map_err(|e| failure(e.status, &e.code))?;
        let (context, session) = self.manager.authorized().await?;
        if context != self.context {
            return Err(failure(401, "management_scope_changed"));
        }
        session
            .authorize(&context, self.permission)
            .map_err(storage)?;
        session
            .authorize(&context, Permission::ReadResources)
            .map_err(storage)?;
        let device = self
            .manager
            .owner
            .0
            .host
            .console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        if DeviceIdentity::from(&device) != DeviceIdentity::from(&self.device)
            || Instant::now() >= self.deadline
            || !(self.gate())()
        {
            return Err(failure(401, "management_scope_changed"));
        }
        Ok(())
    }
    fn accepts(&self, binding: &Binding, registry: &OwnerResourceRegistry) -> bool {
        binding.registry_id == registry.id()
            && profile_digest(&self.identity).is_ok_and(|digest| digest == binding.profile_digest)
            && binding.owner_mxid == self.identity.owner
            && binding.executor_device_id == self.identity.device
    }
}

impl<'a> NativeIo<'a> {
    async fn open(
        manager: &'a NativeManagement,
        cookie: &'a str,
        permission: Permission,
        host_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    ) -> Result<Self, NativeError> {
        let (context, session) = manager.authorized().await?;
        session.authorize(&context, permission).map_err(storage)?;
        session
            .authorize(&context, Permission::ReadResources)
            .map_err(storage)?;
        let device = manager
            .owner
            .0
            .host
            .console
            .authorized_device()
            .await
            .map_err(|_| failure(401, "owner_authorization_required"))?;
        let gate_session = session.clone();
        let gate_context = context.clone();
        let gate_device = device.clone();
        let matrix_host_gate = host_gate.clone();
        let matrix = MatrixScope::management(
            &manager.owner.0.host.console,
            cookie,
            &device,
            Box::new(move || {
                matrix_host_gate.as_ref().is_none_or(|gate| gate())
                    && gate_device.bearer().is_ok()
                    && gate_session.authorize(&gate_context, permission).is_ok()
                    && gate_session
                        .authorize(&gate_context, Permission::ReadResources)
                        .is_ok()
            }),
        )
        .await
        .map_err(|e| failure(e.status, &e.code))?;
        let identity: Identity = matrix.management_identity(device.device_id()).into();
        let path = matrix
            .management_path()
            .with_file_name("owner-grant-intents-v1.json");
        let deadline = device
            .valid_until()
            .min(Instant::now() + Duration::from_secs(60));
        let profile =
            ProfileAuthority::new(profile_digest(&identity)?, identity.owner.clone(), deadline)
                .map_err(storage)?;
        let mut io = NativeIo {
            manager,
            matrix,
            context,
            session,
            device,
            profile,
            identity,
            path,
            permission,
            deadline,
            host_gate: host_gate.clone(),
        };
        io.profile = io
            .profile
            .clone()
            .with_native_gate(io.gate())
            .map_err(storage)?;
        io.check().await?;
        Ok(io)
    }

}

async fn recover_record(
        io: &NativeIo<'_>,
        registry: &OwnerResourceRegistry,
        history: &mut History,
        index: usize,
    ) -> Result<OwnerGrantProgress, NativeError> {
        let original = &history.data.records[index].original;
        if !io.accepts(&original.binding, registry) {
            return Err(failure(401, "owner_grant_binding_changed"));
        }
        io.check().await?;
        let receipt = registry
            .recover_capacity_grant(
                io.profile.clone(),
                original.binding.executor_device_id.clone(),
                original.selection.grant_id.clone(),
            )
            .await
            .map_err(storage)?;
        io.check().await?;
        let record = &mut history.data.records[index];
        if let Some(receipt) = receipt {
            if record.state == State::Declined {
                return Err(history_error());
            }
            record.record(&receipt)?;
            let mut progress = record.progress(OwnerGrantState::Recorded);
            history.save()?;
            if original_is_successor(&history.data.records[index].original) {
                let state = registry
                    .capacity_grant_succession(
                        io.profile.clone(),
                        io.identity.device.clone(),
                        receipt.grant_id.clone(),
                    )
                    .await
                    .map_err(storage)?
                    .ok_or_else(history_error)?;
                io.check().await?;
                progress.succession_state = Some(OwnerGrantSuccessionState::from_native(&state.state)?);
            }
            let closure = registry.capacity_grant_closure(io.profile.clone(),
                io.identity.device.clone(), receipt).await.map_err(storage)?;
            io.check().await?;
            if let Some(closure) = closure {
                progress.closure = Some(OwnerGrantClosure {
                    retained_tokens: closure.retained_tokens,
                    known_consumed_tokens: closure.known_consumed_tokens,
                    unresolved_tokens: closure.unresolved_tokens,
                    observed_at_ms: now_ms()?,
                });
            }
            return Ok(progress);
        }
        let state = match record.state {
            State::Applying | State::Recorded => OwnerGrantState::OutcomeUnknown,
            State::Declined => OwnerGrantState::Declined,
            State::Review if now_ms()? >= record.original.period.end_ms => {
                OwnerGrantState::ReviewExpired
            }
            State::Review => OwnerGrantState::ReviewRequired,
        };
        Ok(record.progress(state))
    }


fn original_is_successor(original: &Original) -> bool {
    original.succession.is_some()
}

pub struct OwnerGrantIntents {
    manager: Arc<NativeManagement>,
    host_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl OwnerGrantIntents {
    pub fn new(manager: Arc<NativeManagement>) -> Self {
        Self {
            manager,
            host_gate: None,
        }
    }
    fn check_host(&self) -> Result<(), NativeError> {
        if self.host_gate.as_ref().is_some_and(|gate| !gate()) {
            return Err(failure(401, "management_scope_changed"));
        }
        Ok(())
    }
    async fn cookie(&self) -> Result<String, NativeError> {
        self.check_host()?;
        self.manager.authorized().await?;
        self.check_host()?;
        let cookie = self
            .manager
            .owner
            .0
            .cookie
            .lock()
            .await
            .strip_prefix("hagency_console=")
            .map(str::to_owned)
            .ok_or_else(|| failure(401, "owner_authorization_required"))?;
        self.check_host()?;
        Ok(cookie)
    }
    async fn io<'a>(
        &'a self,
        cookie: &'a str,
        permission: Permission,
    ) -> Result<NativeIo<'a>, NativeError> {
        NativeIo::open(&self.manager, cookie, permission, self.host_gate.clone()).await
    }

    async fn review_context(
        &self,
        io: &NativeIo<'_>,
        registry: &OwnerResourceRegistry,
        consent: OwnerGrantConsent,
    ) -> Result<(OwnerGrantConsent, OwnerGrantReviewContext), NativeError> {
        let binding = consent.binding().map_err(storage)?;
        let resource = registry
            .resource(&io.profile, binding.resource_id.clone())
            .await
            .map_err(storage)?;
        io.check().await?;
        if resource.revision != binding.resource_revision
            || !io.accepts(&Binding::from_native(&binding), registry)
        {
            return Err(failure(409, "owner_grant_review_changed"));
        }
        let mut context = OwnerGrantReviewContext {
            server_origin: io.identity.origin.clone(),
            profile_id: binding.profile_digest,
            resource_revision: resource.revision,
            framework: resource.framework,
            model: resource.model,
            remaining_tokens: resource.budget.remaining_tokens.map(u64::from),
            resource_total_tokens: resource.budget.pool.ceiling.map(u64::from),
            shared_account_total_tokens: resource.budget.seat.quota.map(u64::from),
            capacity_blocker: capacity_blocker(&resource.budget),
            observed_at_ms: now_ms()?,
        };
        if let Some(calendar) = &consent.selection().calendar {
            let cadence = match calendar.cadence {
                hagency_core::recurring_quota::Cadence::Daily => "daily",
                hagency_core::recurring_quota::Cadence::Weekly => "weekly",
                hagency_core::recurring_quota::Cadence::Monthly => "monthly",
            };
            if resource.budget.seat.quota.is_some()
                && (resource.budget.seat.period.as_deref()!=Some(cadence) || calendar.time_zone != "UTC")
            {
                context.remaining_tokens=None;
                context.capacity_blocker=Some("account_period_mismatch".into());
            }
        }
        let consent = consent
            .with_reviewed_budget(&resource.budget)
            .map_err(storage)?;
        Ok((consent, context))
    }

    pub async fn prepare(
        &self,
        registry: &OwnerResourceRegistry,
        enrollment: VerifiedManagementEnrollment,
        choices: OwnerGrantChoices,
    ) -> Result<OwnerGrantReview, NativeError> {
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        if choices.coordinator_mxid != io.identity.owner {
            return Err(failure(409, "resource_owner_is_coordinator"));
        }
        let selection = OwnerGrantSelection { renewal_mode: choices.renewal_mode, per_request_tokens: choices.per_request_tokens,
            calendar: choices.calendar,
            command_id: random_id()?,
            grant_id: random_id()?,
            coordinator_mxid: choices.coordinator_mxid,
            delegation_expires_at_ms: choices.delegation_expires_at_ms,
            allow_self_approval: choices.allow_self_approval,
            eligible_managers: choices.eligible_managers,
            allocated_tokens: choices.allocated_tokens,
        };
        let consent = registry
            .prepare_capacity_grant(enrollment, selection, io.deadline)
            .map_err(storage)?
            .require_unissued_contribution()
            .map_err(storage)?
            .with_native_confirmation_gate(io.gate())
            .map_err(storage)?;
        let binding = Binding::from_native(&consent.binding().map_err(storage)?);
        // Keep the transport future out of the enclosing adoption/consent
        // state machine, which also runs on ordinary small worker stacks.
        let admission = Box::pin(self.observe_grant_admission(&io, &binding)).await?;
        let receipt_version = if consent.selection().renewal_mode.is_some() {
            if !Box::pin(self.supports_pool_renewal(&io)).await? { return Err(failure(409,"management_pool_renewal_policy_required")); }
            6
        } else if consent.selection().per_request_tokens.is_some() {
            if !Box::pin(self.supports_split_limits(&io)).await? { return Err(failure(409,"management_pool_limits_required")); }
            5
        } else if consent.selection().calendar.is_some() {
            if !Box::pin(self.supports_calendar(&io)).await? { return Err(failure(409,"management_calendar_grants_required")); }
            4
        } else if Box::pin(self.supports_admission_receipts(&io)).await? { 2 } else { 1 };
        let void = Box::pin(self.void_successors(&io, registry, &binding.contribution_id)).await?;
        let regrant = receipt_version >= 2
            && History::load(&io.path, io.identity.clone())?.admission_month_ended(
                &binding.contribution_id,
                &admission.request_id,
                now_ms()?,
                &void,
            );
        let (consent, prior) = if regrant {
            // A new UTC month on the same admission, after every earlier month ended.
            if !Box::pin(self.supports_monthly_regrant(&io)).await? {
                return Err(failure(409, "management_monthly_regrant_required"));
            }
            let prior = Box::pin(self.reconcile_regrant(
                &io,
                registry,
                &binding.contribution_id,
                &admission.request_id,
            ))
            .await?;
            (
                consent
                    .with_admission_request(admission.request_id.clone())
                    .map_err(storage)?
                    .require_regrant_after_period_end()
                    .map_err(storage)?,
                prior,
            )
        } else if receipt_version >= 2 {
            let closed = Box::pin(self.reconcile_prior_admission(&io, registry,
                &binding.contribution_id, &admission.request_id)).await?;
            (consent.with_admission_request(admission.request_id.clone()).map_err(storage)?
                .require_closed_prior_admissions().map_err(storage)?, closed)
        } else { (consent, Vec::new()) };
        let original = Original {
            intent_id: random_id()?,
            binding,
            admission: Some(admission.clone()),
            receipt_version,
            selection: Selection::from_native(consent.selection()),
            period: Period::from_native(consent.period()),
            succession: None,
        };
        if !io.accepts(&original.binding, registry)
            || original.binding.executor_generation != io.device.generation()
        {
            return Err(failure(401, "owner_grant_binding_changed"));
        }
        if receipt_version == 1 && registry
            .has_capacity_grant_for_contribution(
                io.profile.clone(),
                original.binding.contribution_id.clone(),
            )
            .await
            .map_err(storage)?
        {
            return Err(failure(409, "owner_grant_original_intent_required"));
        }
        io.check().await?;
        let (consent, context) = self.review_context(&io, registry, consent).await?;
        let mut history = History::load(&io.path, io.identity.clone())?;
        history.insert_after(original.clone(), &prior, regrant, &void)?;
        io.check().await?;
        consent.binding().map_err(storage)?;
        Ok(OwnerGrantReview {
            manager: self.manager.clone(),
            identity: io.identity,
            original,
            admission,
            consent,
            context,
            regrant,
        })
    }

    async fn recovered(
        &self,
        io: &NativeIo<'_>,
        registry: &OwnerResourceRegistry,
        history: &mut History,
        index: usize,
    ) -> Result<OwnerGrantProgress, NativeError> {
        recover_record(io, registry, history, index).await
    }

    pub async fn recover(
        &self,
        registry: &OwnerResourceRegistry,
        intent_id: String,
    ) -> Result<OwnerGrantProgress, NativeError> {
        super::key(&intent_id)?;
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ReadResources).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        let mut history = History::load(&io.path, io.identity.clone())?;
        let index = history.index(&intent_id)?;
        self.recovered(&io, registry, &mut history, index).await
    }

    /// Descriptive original intents only. No saved state is projected as a
    /// successful hold; callers must recover each result from the registry.
    pub async fn list_existing(
        &self,
        registry: &OwnerResourceRegistry,
    ) -> Result<Vec<OwnerGrantSummary>, NativeError> {
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ReadResources).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        let history = History::load(&io.path, io.identity.clone())?;
        let mut summaries = Vec::with_capacity(history.data.records.len());
        for record in history.data.records {
            if !io.accepts(&record.original.binding, registry) {
                return Err(failure(401, "owner_grant_binding_changed"));
            }
            summaries.push(record.original.summary());
        }
        io.check().await?;
        Ok(summaries)
    }

    pub async fn review_existing(
        &self,
        registry: &OwnerResourceRegistry,
        enrollment: VerifiedManagementEnrollment,
        intent_id: String,
    ) -> Result<OwnerGrantReviewResult, NativeError> {
        super::key(&intent_id)?;
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        let mut history = History::load(&io.path, io.identity.clone())?;
        let index = history.index(&intent_id)?;
        let progress = self.recovered(&io, registry, &mut history, index).await?;
        if !matches!(progress.state, OwnerGrantState::ReviewRequired) {
            return Ok(OwnerGrantReviewResult::Progress(Box::new(progress)));
        }
        let original = history.data.records[index].original.clone();
        if original.succession.is_some() {
            return self
                .review_existing_successor(&io, registry, enrollment, original)
                .await;
        }
        let consent = registry
            .prepare_capacity_grant(enrollment, original.selection.native(), io.deadline)
            .map_err(storage)?
            .require_unissued_contribution()
            .map_err(storage)?
            .with_native_confirmation_gate(io.gate())
            .map_err(storage)?;
        if Binding::from_native(&consent.binding().map_err(storage)?) != original.binding
            || Period::from_native(consent.period()) != original.period
            || original.binding.executor_generation != io.device.generation()
        {
            return Err(failure(409, "owner_grant_original_review_expired"));
        }
        let admission = Box::pin(self.check_grant_admission(&io, &original)).await?;
        let void = Box::pin(self.void_successors(&io, registry, &original.binding.contribution_id)).await?;
        let regrant = original.receipt_version >= 2
            && history.admission_month_ended(
                &original.binding.contribution_id,
                &admission.request_id,
                now_ms()?,
                &void,
            );
        let consent = if original.receipt_version >= 2 {
            if !Box::pin(self.supports_admission_receipts(&io)).await? {
                return Err(failure(409, "management_admission_receipts_required"));
            }
            let consent = consent
                .with_admission_request(admission.request_id.clone())
                .map_err(storage)?;
            if regrant {
                if !Box::pin(self.supports_monthly_regrant(&io)).await? {
                    return Err(failure(409, "management_monthly_regrant_required"));
                }
                consent
                    .require_regrant_after_period_end()
                    .map_err(storage)?
            } else {
                consent.require_closed_prior_admissions().map_err(storage)?
            }
        } else { consent };
        io.check().await?;
        let (consent, context) = self.review_context(&io, registry, consent).await?;
        Ok(OwnerGrantReviewResult::Review(Box::new(OwnerGrantReview {
            manager: self.manager.clone(),
            identity: io.identity,
            original,
            admission,
            consent,
            context,
            regrant,
        })))
    }

    async fn review_existing_successor(
        &self,
        io: &NativeIo<'_>,
        registry: &OwnerResourceRegistry,
        enrollment: VerifiedManagementEnrollment,
        original: Original,
    ) -> Result<OwnerGrantReviewResult, NativeError> {
        if !Box::pin(self.supports_succession(io)).await? {
            return Err(failure(409, "management_grant_succession_required"));
        }
        let (consent, max_tokens) = self
            .successor_consent(io, registry, enrollment, &original)
            .await?;
        if Binding::from_native(&consent.binding().map_err(storage)?) != original.binding
            || Period::from_native(consent.period()) != original.period
            || original.binding.executor_generation != io.device.generation()
        {
            return Err(failure(409, "owner_grant_original_review_expired"));
        }
        let admission = Box::pin(self.check_grant_admission(io, &original)).await?;
        io.check().await?;
        let (consent, mut context) = self.review_context(io, registry, consent).await?;
        context.remaining_tokens = max_tokens;
        Ok(OwnerGrantReviewResult::Review(Box::new(OwnerGrantReview {
            manager: self.manager.clone(),
            identity: io.identity.clone(),
            original,
            admission,
            consent,
            context,
            regrant: false,
        })))
    }

    pub async fn confirm(
        &self,
        registry: &OwnerResourceRegistry,
        review: OwnerGrantReview,
    ) -> Result<OwnerGrantProgress, NativeError> {
        if !Arc::ptr_eq(&self.manager, &review.manager) {
            return Err(failure(401, "owner_grant_review_changed"));
        }
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        if io.identity != review.identity {
            return Err(failure(401, "owner_grant_review_changed"));
        }
        let mut history = History::load(&io.path, io.identity.clone())?;
        let index = history.index(&review.original.intent_id)?;
        if history.data.records[index].original != review.original {
            return Err(history_error());
        }
        let progress = self.recovered(&io, registry, &mut history, index).await?;
        if !matches!(progress.state, OwnerGrantState::ReviewRequired) {
            return Ok(progress);
        }
        if Binding::from_native(&review.consent.binding().map_err(storage)?)
            != review.original.binding
            || review.original.binding.executor_generation != io.device.generation()
        {
            return Err(failure(401, "owner_grant_review_changed"));
        }
        if !review.capacity_available() {
            return Err(failure(409, "owner_grant_capacity_unavailable"));
        }
        let admission = Box::pin(self.check_grant_admission(&io, &review.original)).await?;
        if admission != review.admission {
            return Err(failure(409, "management_enrollment_changed"));
        }
        // Disabling fresh allocations fences pending v1 and v2 consent alike;
        // recovery above still returns an existing original receipt unchanged.
        let admission_receipts = Box::pin(self.supports_admission_receipts(&io)).await?;
        if review.original.receipt_version >= 2 && !admission_receipts {
            return Err(failure(409, "management_admission_receipts_required"));
        }
        if review.regrant && !Box::pin(self.supports_monthly_regrant(&io)).await? {
            return Err(failure(409, "management_monthly_regrant_required"));
        }
        if review.original.receipt_version == 6 && !Box::pin(self.supports_pool_renewal(&io)).await? { return Err(failure(409,"management_pool_renewal_policy_required")); }
        if review.original.receipt_version >= 5 && !Box::pin(self.supports_split_limits(&io)).await? { return Err(failure(409,"management_pool_limits_required")); }
        if review.original.receipt_version == 4 && !Box::pin(self.supports_calendar(&io)).await? {
            return Err(failure(409,"management_calendar_grants_required"));
        }
        let successor = review.original.succession.is_some();
        if successor && !Box::pin(self.supports_succession(&io)).await? {
            return Err(failure(409, "management_grant_succession_required"));
        }
        history.data.records[index].state = State::Applying;
        history.save()?;
        io.check().await?;
        let result = review.consent.approve().await;
        let authorized = io.check().await.is_ok();
        // This dedicated error is emitted only by the budget comparison before
        // any new effect. Unknown/retired/post-commit outcomes never use it.
        if authorized && matches!(result, Err(hagency_store::Error::OwnerGrantReviewChanged)) {
            history.data.records[index].state = State::Review;
            history.save()?;
            io.check().await?;
            return Err(failure(409, "owner_grant_review_changed"));
        }
        // The successor writer reports a missing floor or headroom only from
        // inside its rolled-back transaction: nothing was written.
        if authorized
            && successor
            && matches!(
                result,
                Err(hagency_store::Error::InsufficientCapacity | hagency_store::Error::NoCeiling)
            )
        {
            history.data.records[index].state = State::Review;
            history.save()?;
            io.check().await?;
            return Err(failure(409, "owner_grant_capacity_unavailable"));
        }
        if let Ok(receipt) = result {
            history.data.records[index].record(&receipt)?;
            history.save()?;
            if authorized && io.check().await.is_ok() {
                let mut progress = history.data.records[index].progress(OwnerGrantState::Recorded);
                if successor {
                    let state = registry
                        .capacity_grant_succession(
                            io.profile.clone(),
                            io.identity.device.clone(),
                            receipt.grant_id.clone(),
                        )
                        .await
                        .map_err(|_| failure(409, "management_outcome_unknown"))?
                        .ok_or_else(|| failure(409, "management_outcome_unknown"))?;
                    progress.succession_state =
                        Some(OwnerGrantSuccessionState::from_native(&state.state)?);
                }
                return Ok(progress);
            }
            return Err(failure(409, "management_outcome_unknown"));
        }
        if !authorized {
            return Err(failure(409, "management_outcome_unknown"));
        }
        Ok(history.data.records[index].progress(OwnerGrantState::OutcomeUnknown))
    }

    pub async fn decline(
        &self,
        review: OwnerGrantReview,
    ) -> Result<OwnerGrantProgress, NativeError> {
        if !Arc::ptr_eq(&self.manager, &review.manager) {
            return Err(failure(401, "owner_grant_review_changed"));
        }
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ConsentGrants).await?;
        let _lane = self.manager.owner.0.management.mutations.lock().await;
        io.check().await?;
        if io.identity != review.identity {
            return Err(failure(401, "owner_grant_review_changed"));
        }
        let mut history = History::load(&io.path, io.identity.clone())?;
        let index = history.index(&review.original.intent_id)?;
        let record = &mut history.data.records[index];
        if record.original != review.original || record.state != State::Review {
            return Err(failure(409, "owner_grant_already_attempted"));
        }
        record.state = State::Declined;
        let result = record.progress(OwnerGrantState::Declined);
        history.save()?;
        io.check().await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests;

/// A native-only registry binding. Construction verifies the exact management
/// session; it never opens a path, imports legacy state or grants permission.
#[derive(Clone)]
pub struct OwnerGrantAccess {
    manager: Arc<NativeManagement>,
    registry: OwnerResourceRegistry,
    host_gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl OwnerGrantIntents {
    pub async fn bind_registry(
        self,
        registry: OwnerResourceRegistry,
    ) -> Result<OwnerGrantAccess, NativeError> {
        let cookie = self.cookie().await?;
        let io = self.io(&cookie, Permission::ReadResources).await?;
        io.check().await?;
        drop(io);
        Ok(OwnerGrantAccess {
            manager: self.manager,
            registry,
            host_gate: self.host_gate,
        })
    }
}
impl OwnerGrantAccess {
    #[cfg(any(unix, test))]
    pub(crate) fn bind_controller_lifetime(self, lifetime: Arc<dyn Fn()->Option<Instant>+Send+Sync>)->Result<Self,NativeError> {
        self.manager.bind_controller_lifetime(lifetime)?;
        Ok(self)
    }
    /// The native host's package lifetime also fences queued grant writes.
    pub fn restrict_to_deadline(self, until: Instant) -> Result<Self, NativeError> {
        self.manager.restrict_to_deadline(until)?;
        self.restrict_to_host(move || Instant::now() < until)
    }
    /// Inspect exactly the admitted resource before the owner chooses how much
    /// to publish. The actual write still re-verifies provider custody and budget.
    pub async fn publication_form(&self, contribution: String) -> Result<OwnerGrantForm, NativeError> {
        self.service().publication_form(&self.registry, contribution).await
    }
    /// Adds a native host restriction. It cannot replace SDK/profile/provider
    /// authority; the composed check also runs inside the real SQLite writer.
    pub fn restrict_to_host(
        mut self,
        gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, NativeError> {
        if self.retired() || !gate() {
            return Err(failure(401, "management_scope_changed"));
        }
        let prior = self.host_gate.take();
        self.host_gate = Some(Arc::new(move || {
            prior.as_ref().is_none_or(|check| check()) && gate()
        }));
        Ok(self)
    }
    fn service(&self) -> OwnerGrantIntents {
        OwnerGrantIntents {
            manager: self.manager.clone(),
            host_gate: self.host_gate.clone(),
        }
    }
    pub fn retired(&self) -> bool {
        self.manager.is_retired() || self.host_gate.as_ref().is_some_and(|gate| !gate())
    }
    pub fn retire(&self) {
        self.manager.retire();
    }
    pub async fn prepare(
        &self,
        contribution: String,
        choices: OwnerGrantChoices,
    ) -> Result<OwnerGrantReview, NativeError> {
        let service = self.service();
        let proof = service
            .verified_enrollment(&self.registry, contribution)
            .await?;
        service.prepare(&self.registry, proof, choices).await
    }
    pub async fn review(&self, id: String) -> Result<OwnerGrantReviewResult, NativeError> {
        let service = self.service();
        let progress = service.recover(&self.registry, id.clone()).await?;
        if !matches!(progress.state, OwnerGrantState::ReviewRequired) {
            return Ok(OwnerGrantReviewResult::Progress(Box::new(progress)));
        }
        let proof = service
            .verified_enrollment(&self.registry, progress.summary.contribution_id)
            .await?;
        service.review_existing(&self.registry, proof, id).await
    }
    pub async fn confirm(
        &self,
        review: OwnerGrantReview,
    ) -> Result<OwnerGrantProgress, NativeError> {
        self.service().confirm(&self.registry, review).await
    }
    pub async fn recover(&self, id: String) -> Result<OwnerGrantProgress, NativeError> {
        self.service().recover(&self.registry, id).await
    }
    pub async fn list_existing(&self) -> Result<Vec<OwnerGrantSummary>, NativeError> {
        self.service().list_existing(&self.registry).await
    }
    /// Refresh the approved admission before opening new choices from a closed
    /// historical grant, or from grants of this admission whose UTC month has
    /// ended (a new month on the same admission). Prepare/confirm still
    /// independently recheck authority.
    pub async fn check_new_admission(&self, contribution: String) -> Result<(), NativeError> {
        self.service().check_new_admission(&self.registry, contribution).await
    }

}
