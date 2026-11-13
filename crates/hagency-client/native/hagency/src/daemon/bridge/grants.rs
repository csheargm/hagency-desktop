//! Sealed grant and setup custody across the native daemon boundary.
use super::{ControllerClient, Package, decode, encode, error};
use crate::native_owner::{self as native, NativeError};
#[cfg(any(unix, test))]
use crate::native_owner::NativeOwner;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
#[cfg(any(unix, test))]
use std::{collections::HashMap, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OwnerOperation {
    AttachGrants,
    PublicationForm,
    SupportsSuccession,
    SuccessionForm,
    PrepareGrant,
    PrepareSuccessor,
    ReviewGrant,
    ConfirmGrant,
    RecoverGrant,
    ListGrants,
    CheckAdmission,
    ObservePublication,
    ReviewPublication,
    ConfirmPublication,
    RecoverPublication,
    ReviewCancellation,
    ConfirmCancellation,
    DeviceSetupContext,
    ReviewDeviceSetup,
    DeviceSetupState,
    ConfirmDeviceSetup,
    ListSuccessors,
    AssessSuccessor,
    SuccessorDescription,
    StartSuccessor,
    ResumeSuccessor,
}
impl OwnerOperation {
    pub fn is_mutation(self) -> bool {
        matches!(
            self,
            Self::PrepareGrant
                | Self::PrepareSuccessor
                | Self::ConfirmGrant
                | Self::ReviewPublication
                | Self::ConfirmPublication
                | Self::RecoverPublication
                | Self::ConfirmCancellation
                | Self::ConfirmDeviceSetup
                | Self::StartSuccessor
                | Self::ResumeSuccessor
        )
    }
}
pub struct PreparedSharedSuccessor {
    package: Package,
    handle: String,
    review: native::SharedSuccessorReview,
    until: Instant,
    gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
pub enum SuccessorAssessment {
    Ready(PreparedSharedSuccessor),
    Blocked(native::SharedSuccessorBudgetReview),
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum SuccessorView {
    Ready {
        offered: Offered<native::SharedSuccessorReview>,
    },
    Blocked {
        review: native::SharedSuccessorBudgetReview,
    },
}
impl PreparedSharedSuccessor {
    pub fn expired(&self) -> bool {
        Instant::now() >= self.until || self.gate.as_ref().is_some_and(|g| !g())
    }
    pub fn review(&self) -> &native::SharedSuccessorReview {
        &self.review
    }
    pub fn restrict_to_host(
        mut self,
        gate: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Result<Self, NativeError> {
        if self.expired() || !gate() {
            return Err(error(401, "management_scope_changed"));
        }
        let prior = self.gate.take();
        self.gate = Some(Arc::new(move || {
            prior.as_ref().is_none_or(|g| g()) && gate()
        }));
        Ok(self)
    }
}
impl ControllerClient {
    pub async fn list_shared_successors(
        &self,
        package: native::ManagementPackage,
        after: Option<String>,
    ) -> Result<native::SharedSuccessorPage, NativeError> {
        decode(
            self.owner_access(
                package.into(),
                OwnerOperation::ListSuccessors,
                encode(after)?,
            )
            .await?,
        )
    }
    pub async fn assess_shared_successor(
        &self,
        package: native::ManagementPackage,
        id: String,
        until: Instant,
    ) -> Result<SuccessorAssessment, NativeError> {
        let package = Package::from(package).with_deadline(until)?;
        match decode(
            self.owner_access(
                package.clone(),
                OwnerOperation::AssessSuccessor,
                encode(id)?,
            )
            .await?,
        )? {
            SuccessorView::Ready { offered } => {
                Ok(SuccessorAssessment::Ready(PreparedSharedSuccessor {
                    package,
                    handle: offered.handle,
                    review: offered.view,
                    until: until.min(Instant::now() + Duration::from_secs(60)),
                    gate: None,
                }))
            }
            SuccessorView::Blocked { review } => Ok(SuccessorAssessment::Blocked(review)),
        }
    }
    pub async fn shared_successor_description(
        &self,
        package: native::ManagementPackage,
        id: String,
    ) -> Result<native::SharedSuccessorDescription, NativeError> {
        decode(
            self.owner_access(
                package.into(),
                OwnerOperation::SuccessorDescription,
                encode(id)?,
            )
            .await?,
        )
    }
    pub async fn start_shared_successor(
        &self,
        package: native::ManagementPackage,
        prepared: PreparedSharedSuccessor,
    ) -> Result<native::SharedExecutionProgress, NativeError> {
        if prepared.expired() {
            return Err(error(409, "shared_successor_preparation_changed"));
        }
        if !same_package(&prepared.package, &package) {
            return Err(error(401, "management_scope_changed"));
        }
        decode(
            self.owner_access(
                prepared.package,
                OwnerOperation::StartSuccessor,
                encode(prepared.handle)?,
            )
            .await?,
        )
    }
    pub async fn resume_shared_successor(
        &self,
        package: native::ManagementPackage,
        id: String,
        until: Instant,
    ) -> Result<Option<native::SharedExecutionProgress>, NativeError> {
        decode(
            self.owner_access(
                Package::from(package).with_deadline(until)?,
                OwnerOperation::ResumeSuccessor,
                encode(id)?,
            )
            .await?,
        )
    }
}
#[derive(Clone)]
pub enum CurrentSelection {
    Local(native::CurrentProfileSelection),
    Remote,
}
pub struct DeviceSetupReview {
    view: DeviceView,
    custody: DeviceCustody,
}
enum DeviceCustody {
    Local(native::DeviceSetupReview),
    Remote {
        package: Package,
        handle: String,
        until: Instant,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeviceView {
    context: native::DeviceSetupContext,
}
impl DeviceSetupReview {
    pub fn local(review: native::DeviceSetupReview) -> Self {
        Self {
            view: DeviceView {
                context: review.context().clone(),
            },
            custody: DeviceCustody::Local(review),
        }
    }
    pub fn context(&self) -> &native::DeviceSetupContext {
        &self.view.context
    }
    pub fn expired(&self) -> bool {
        match &self.custody {
            DeviceCustody::Local(r) => r.expired(),
            DeviceCustody::Remote { until, .. } => Instant::now() >= *until,
        }
    }
    pub fn into_local(self) -> Result<native::DeviceSetupReview, NativeError> {
        match self.custody {
            DeviceCustody::Local(r) => Ok(r),
            _ => Err(error(401, "management_scope_changed")),
        }
    }
}
impl ControllerClient {
    pub async fn device_setup_context(
        &self,
        package: native::ManagementPackage,
        until: Instant,
    ) -> Result<native::DeviceSetupContext, NativeError> {
        decode(
            self.owner_access(
                Package::from(package).with_deadline(until)?,
                OwnerOperation::DeviceSetupContext,
                Value::Null,
            )
            .await?,
        )
    }
    pub async fn review_device_setup(
        &self,
        package: native::ManagementPackage,
        until: Instant,
    ) -> Result<DeviceSetupReview, NativeError> {
        let package = Package::from(package).with_deadline(until)?;
        let offered: Offered<DeviceView> = decode(
            self.owner_access(
                package.clone(),
                OwnerOperation::ReviewDeviceSetup,
                Value::Null,
            )
            .await?,
        )?;
        Ok(DeviceSetupReview {
            view: offered.view,
            custody: DeviceCustody::Remote {
                package,
                handle: offered.handle,
                until: until.min(Instant::now() + Duration::from_secs(60)),
            },
        })
    }
    pub async fn device_setup_state(
        &self,
        package: native::ManagementPackage,
    ) -> Result<&'static str, NativeError> {
        let state: String = decode(
            self.owner_access(
                package.into(),
                OwnerOperation::DeviceSetupState,
                Value::Null,
            )
            .await?,
        )?;
        match state.as_str() {
            "active" => Ok("active"),
            "uninitialized" => Ok("uninitialized"),
            _ => Err(error(503, "daemon_bridge_invalid_reply")),
        }
    }
    pub async fn confirm_device_setup(
        &self,
        package: native::ManagementPackage,
        review: DeviceSetupReview,
    ) -> Result<CurrentSelection, NativeError> {
        let DeviceCustody::Remote {
            package: original,
            handle,
            until,
        } = review.custody
        else {
            return Err(error(401, "management_scope_changed"));
        };
        if until <= Instant::now() {
            return Err(error(409, "current_profile_review_expired"));
        }
        if !same_package(&original, &package) {
            return Err(error(401, "management_scope_changed"));
        }
        self.owner_access(
            original,
            OwnerOperation::ConfirmDeviceSetup,
            encode(handle)?,
        )
        .await?;
        Ok(CurrentSelection::Remote)
    }
}
fn same_package(a: &Package, b: &native::ManagementPackage) -> bool {
    a.digest == b.digest
        && a.instance == b.instance
        && a.account_epoch == b.account_epoch
        && a.permissions == b.permissions
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Choices {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    renewal_mode: Option<hagency_core::recurring_quota::RenewalMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    per_request_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    calendar: Option<hagency_core::recurring_quota::Calendar>,
    coordinator_mxid: String,
    delegation_expires_at_ms: u64,
    allow_self_approval: bool,
    eligible_managers: Vec<String>,
    allocated_tokens: u64,
}
impl From<native::OwnerGrantChoices> for Choices {
    fn from(c: native::OwnerGrantChoices) -> Self {
        Self {
            calendar: c.calendar,
            renewal_mode: c.renewal_mode,
            per_request_tokens: c.per_request_tokens,
            coordinator_mxid: c.coordinator_mxid,
            delegation_expires_at_ms: c.delegation_expires_at_ms,
            allow_self_approval: c.allow_self_approval,
            eligible_managers: c.eligible_managers,
            allocated_tokens: c.allocated_tokens,
        }
    }
}
impl From<Choices> for native::OwnerGrantChoices {
    fn from(c: Choices) -> Self {
        Self {
            calendar: c.calendar,
            renewal_mode: c.renewal_mode,
            per_request_tokens: c.per_request_tokens,
            coordinator_mxid: c.coordinator_mxid,
            delegation_expires_at_ms: c.delegation_expires_at_ms,
            allow_self_approval: c.allow_self_approval,
            eligible_managers: c.eligible_managers,
            allocated_tokens: c.allocated_tokens,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ReviewView {
    summary: native::OwnerGrantSummary,
    context: native::OwnerGrantReviewContext,
    capacity_available: bool,
}
#[derive(Serialize, Deserialize)]
struct PublicationView {
    summary: native::OwnerGrantSummary,
    server_origin: String,
    contribution_revision: u64,
    observed_at_ms: u64,
}
#[derive(Serialize, Deserialize)]
struct CancellationView {
    summary: native::OwnerGrantSummary,
    server_origin: String,
}
#[derive(Serialize, Deserialize)]
struct Offered<T> {
    handle: String,
    view: T,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
enum ReviewResult<R, P> {
    Review { offered: Offered<R> },
    Progress { progress: P },
}
enum Custody<T> {
    Local(T),
    Remote {
        handle: String,
        access: String,
        until: Instant,
    },
}
pub struct OwnerGrantReview {
    view: ReviewView,
    custody: Custody<native::OwnerGrantReview>,
}
impl OwnerGrantReview {
    pub fn summary(&self) -> native::OwnerGrantSummary {
        self.view.summary.clone()
    }
    pub fn context(&self) -> &native::OwnerGrantReviewContext {
        &self.view.context
    }
    pub fn capacity_available(&self) -> bool {
        self.view.capacity_available
    }
    pub fn expired(&self) -> bool {
        match &self.custody {
            Custody::Local(r) => r.expired(),
            Custody::Remote { until, .. } => Instant::now() >= *until,
        }
    }
    fn local(review: native::OwnerGrantReview) -> Self {
        Self {
            view: ReviewView {
                summary: review.summary(),
                context: review.context().clone(),
                capacity_available: review.capacity_available(),
            },
            custody: Custody::Local(review),
        }
    }
}
pub struct OwnerGrantPublicationReview {
    view: PublicationView,
    custody: Custody<native::OwnerGrantPublicationReview>,
}
impl OwnerGrantPublicationReview {
    pub fn summary(&self) -> native::OwnerGrantSummary {
        self.view.summary.clone()
    }
    pub fn server_origin(&self) -> &str {
        &self.view.server_origin
    }
    pub fn contribution_revision(&self) -> u64 {
        self.view.contribution_revision
    }
    pub fn observed_at_ms(&self) -> u64 {
        self.view.observed_at_ms
    }
    pub fn expired(&self) -> bool {
        match &self.custody {
            Custody::Local(r) => r.expired(),
            Custody::Remote { until, .. } => Instant::now() >= *until,
        }
    }
    fn local(review: native::OwnerGrantPublicationReview) -> Self {
        Self {
            view: PublicationView {
                summary: review.summary(),
                server_origin: review.server_origin().into(),
                contribution_revision: review.contribution_revision(),
                observed_at_ms: review.observed_at_ms(),
            },
            custody: Custody::Local(review),
        }
    }
}
pub struct OwnerGrantCancellationReview {
    view: CancellationView,
    custody: Custody<native::OwnerGrantCancellationReview>,
}
impl OwnerGrantCancellationReview {
    pub fn summary(&self) -> native::OwnerGrantSummary {
        self.view.summary.clone()
    }
    pub fn server_origin(&self) -> &str {
        &self.view.server_origin
    }
    pub fn expired(&self) -> bool {
        match &self.custody {
            Custody::Local(r) => r.expired(),
            Custody::Remote { until, .. } => Instant::now() >= *until,
        }
    }
    fn local(review: native::OwnerGrantCancellationReview) -> Self {
        Self {
            view: CancellationView {
                summary: review.summary(),
                server_origin: review.server_origin().into(),
            },
            custody: Custody::Local(review),
        }
    }
}
pub enum OwnerGrantReviewResult {
    Review(Box<OwnerGrantReview>),
    Progress(Box<native::OwnerGrantProgress>),
}
pub enum OwnerGrantPublicationResult {
    Review(Box<OwnerGrantPublicationReview>),
    Progress(Box<native::OwnerGrantPublicationProgress>),
}
pub enum OwnerGrantCancellationResult {
    Review(Box<OwnerGrantCancellationReview>),
    Progress(Box<native::OwnerGrantProgress>),
}

#[derive(Clone)]
pub struct OwnerGrantAccess {
    inner: Access,
    gate: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}
#[derive(Clone)]
enum Access {
    Local(native::OwnerGrantAccess),
    Remote(Arc<RemoteAccess>),
}
struct RemoteAccess {
    controller: Arc<ControllerClient>,
    package: Package,
    id: Mutex<String>,
    calls:tokio::sync::Mutex<()>,
    retired: AtomicBool,
}
impl RemoteAccess {
    fn id(&self)->String {self.id.lock().unwrap_or_else(|p|p.into_inner()).clone()}
    async fn call<T: Serialize>(&self, op: OwnerOperation, args: T) -> Result<Value, NativeError> {
        if self.retired.load(Ordering::Acquire) {
            return Err(error(401, "management_scope_changed"));
        }
        let _call=self.calls.lock().await;
        let arguments=encode(args)?;
        let mut result=self.controller.owner_access(self.package.clone(),op,encode((self.id(),arguments.clone()))?).await;
        // A read may reopen access after a daemon/controller restart. Native
        // confirmations never migrate their old sealed review to a new access.
        if !op.is_mutation() && result.as_ref().is_err_and(|e|e.code=="daemon_bridge_lease_expired") {
            let fresh:String=decode(self.controller.owner_access(self.package.clone(),OwnerOperation::AttachGrants,Value::Null).await?)?;
            if self.retired.load(Ordering::Acquire) {
                self.controller.retire_owner_access(fresh);
                return Err(error(401,"management_scope_changed"));
            }
            *self.id.lock().unwrap_or_else(|p|p.into_inner())=fresh;
            result=self.controller.owner_access(self.package.clone(),op,encode((self.id(),arguments))?).await;
        }
        if self.retired.load(Ordering::Acquire) {
            return Err(error(401, "management_scope_changed"));
        }
        result
    }
}
impl OwnerGrantAccess {
    pub fn local(value: native::OwnerGrantAccess) -> Self {
        Self {
            inner: Access::Local(value),
            gate: None,
        }
    }
    pub async fn remote(
        controller: Arc<ControllerClient>,
        package: native::ManagementPackage,
    ) -> Result<Self, NativeError> {
        let package: Package = package.into();
        let id = decode(
            controller
                .owner_access(package.clone(), OwnerOperation::AttachGrants, Value::Null)
                .await?,
        )?;
        Ok(Self {
            inner: Access::Remote(Arc::new(RemoteAccess {
                controller,
                package,
                id:Mutex::new(id),
                calls:tokio::sync::Mutex::new(()),
                retired: AtomicBool::new(false),
            })),
            gate: None,
        })
    }
    pub async fn remote_until(
        controller: Arc<ControllerClient>,
        package: native::ManagementPackage,
        until: Instant,
    ) -> Result<Self, NativeError> {
        let package = Package::from(package).with_deadline(until)?;
        let id = decode(
            controller
                .owner_access(package.clone(), OwnerOperation::AttachGrants, Value::Null)
                .await?,
        )?;
        Ok(Self {
            inner: Access::Remote(Arc::new(RemoteAccess {
                controller,
                package,
                id:Mutex::new(id),
                calls:tokio::sync::Mutex::new(()),
                retired: AtomicBool::new(false),
            })),
            gate: Some(Arc::new(move || Instant::now() < until)),
        })
    }
    pub fn retired(&self) -> bool {
        self.gate.as_ref().is_some_and(|gate| !gate())
            || match &self.inner {
                Access::Local(a) => a.retired(),
                Access::Remote(a) => a.retired.load(Ordering::Acquire),
            }
    }
    pub fn retire(&self) {
        match &self.inner {
            Access::Local(a) => a.retire(),
            Access::Remote(a) => {
                if !a.retired.swap(true, Ordering::AcqRel) {
                    a.controller.retire_owner_access(a.id());
                }
            }
        }
    }
    fn check(&self) -> Result<(), NativeError> {
        if self.retired() {
            self.retire();
            Err(error(401, "management_scope_changed"))
        } else {
            Ok(())
        }
    }
    pub fn restrict_to_host(
        mut self,
        gate: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self, NativeError> {
        self.check()?;
        if !gate() {
            return Err(error(401, "management_scope_changed"));
        }
        let gate = Arc::new(gate);
        if let Access::Local(a) = &self.inner {
            let g = gate.clone();
            self.inner = Access::Local(a.clone().restrict_to_host(move || g())?);
        }
        let prior = self.gate.take();
        self.gate = Some(Arc::new(move || {
            prior.as_ref().is_none_or(|p| p()) && gate()
        }));
        Ok(self)
    }
    pub async fn prepare(
        &self,
        contribution: String,
        choices: native::OwnerGrantChoices,
    ) -> Result<OwnerGrantReview, NativeError> {
        self.check()?;
        let result = match &self.inner {
            Access::Local(a) => OwnerGrantReview::local(a.prepare(contribution, choices).await?),
            Access::Remote(a) => {
                let offered: Offered<ReviewView> = decode(
                    a.call(
                        OwnerOperation::PrepareGrant,
                        (contribution, Choices::from(choices)),
                    )
                    .await?,
                )?;
                OwnerGrantReview {
                    view: offered.view,
                    custody: remote_custody(a, &offered.handle),
                }
            }
        };
        self.check()?;
        Ok(result)
    }
    pub async fn prepare_successor(
        &self,
        contribution: String,
        kind: native::OwnerGrantSuccessionKind,
        choices: native::OwnerGrantChoices,
    ) -> Result<OwnerGrantReview, NativeError> {
        self.check()?;
        let result = match &self.inner {
            Access::Local(a) => {
                OwnerGrantReview::local(a.prepare_successor(contribution, kind, choices).await?)
            }
            Access::Remote(a) => {
                let offered: Offered<ReviewView> = decode(
                    a.call(
                        OwnerOperation::PrepareSuccessor,
                        (contribution, kind, Choices::from(choices)),
                    )
                    .await?,
                )?;
                OwnerGrantReview {
                    view: offered.view,
                    custody: remote_custody(a, &offered.handle),
                }
            }
        };
        self.check()?;
        Ok(result)
    }
    pub async fn review(&self, id: String) -> Result<OwnerGrantReviewResult, NativeError> {
        self.check()?;
        let result = match &self.inner {
            Access::Local(a) => match a.review(id).await? {
                native::OwnerGrantReviewResult::Review(r) => {
                    OwnerGrantReviewResult::Review(Box::new(OwnerGrantReview::local(*r)))
                }
                native::OwnerGrantReviewResult::Progress(p) => OwnerGrantReviewResult::Progress(p),
            },
            Access::Remote(a) => match decode(a.call(OwnerOperation::ReviewGrant, (id,)).await?)? {
                ReviewResult::Review {
                    offered: Offered { handle, view },
                } => OwnerGrantReviewResult::Review(Box::new(OwnerGrantReview {
                    view,
                    custody: remote_custody(a, &handle),
                })),
                ReviewResult::Progress { progress } => {
                    OwnerGrantReviewResult::Progress(Box::new(progress))
                }
            },
        };
        self.check()?;
        Ok(result)
    }
    pub async fn confirm(
        &self,
        review: OwnerGrantReview,
    ) -> Result<native::OwnerGrantProgress, NativeError> {
        self.check()?;
        let result = match (&self.inner, review.custody) {
            (Access::Local(a), Custody::Local(r)) => a.confirm(r).await,
            (
                Access::Remote(a),
                Custody::Remote {
                    handle,
                    access,
                    until,
                },
            ) if access == a.id() && until > Instant::now() => {
                decode(a.call(OwnerOperation::ConfirmGrant, (handle,)).await?)
            }
            _ => Err(error(409, "daemon_review_expired")),
        };
        self.check()?;
        result
    }
    pub async fn review_publication(
        &self,
        id: String,
    ) -> Result<OwnerGrantPublicationResult, NativeError> {
        self.publication_review(id, false).await
    }
    pub async fn recover_publication(
        &self,
        id: String,
    ) -> Result<OwnerGrantPublicationResult, NativeError> {
        self.publication_review(id, true).await
    }
    async fn publication_review(
        &self,
        id: String,
        recover: bool,
    ) -> Result<OwnerGrantPublicationResult, NativeError> {
        self.check()?;
        let result = match &self.inner {
            Access::Local(a) => {
                let result = if recover {
                    a.recover_publication(id).await?
                } else {
                    a.review_publication(id).await?
                };
                match result {
                    native::OwnerGrantPublicationResult::Review(r) => {
                        OwnerGrantPublicationResult::Review(Box::new(
                            OwnerGrantPublicationReview::local(*r),
                        ))
                    }
                    native::OwnerGrantPublicationResult::Progress(p) => {
                        OwnerGrantPublicationResult::Progress(p)
                    }
                }
            }
            Access::Remote(a) => match decode(
                a.call(
                    if recover {
                        OwnerOperation::RecoverPublication
                    } else {
                        OwnerOperation::ReviewPublication
                    },
                    (id,),
                )
                .await?,
            )? {
                ReviewResult::Review {
                    offered: Offered { handle, view },
                } => OwnerGrantPublicationResult::Review(Box::new(OwnerGrantPublicationReview {
                    view,
                    custody: remote_custody(a, &handle),
                })),
                ReviewResult::Progress { progress } => {
                    OwnerGrantPublicationResult::Progress(Box::new(progress))
                }
            },
        };
        self.check()?;
        Ok(result)
    }
    pub async fn confirm_publication(
        &self,
        review: OwnerGrantPublicationReview,
    ) -> Result<native::OwnerGrantPublicationProgress, NativeError> {
        self.check()?;
        let result = match (&self.inner, review.custody) {
            (Access::Local(a), Custody::Local(r)) => a.confirm_publication(r).await,
            (
                Access::Remote(a),
                Custody::Remote {
                    handle,
                    access,
                    until,
                },
            ) if access == a.id() && until > Instant::now() => decode(
                a.call(OwnerOperation::ConfirmPublication, (handle,))
                    .await?,
            ),
            _ => Err(error(409, "daemon_review_expired")),
        };
        self.check()?;
        result
    }
    pub async fn review_cancellation(
        &self,
        id: String,
    ) -> Result<OwnerGrantCancellationResult, NativeError> {
        self.check()?;
        let result = match &self.inner {
            Access::Local(a) => match a.review_cancellation(id).await? {
                native::OwnerGrantCancellationResult::Review(r) => {
                    OwnerGrantCancellationResult::Review(Box::new(
                        OwnerGrantCancellationReview::local(*r),
                    ))
                }
                native::OwnerGrantCancellationResult::Progress(p) => {
                    OwnerGrantCancellationResult::Progress(p)
                }
            },
            Access::Remote(a) => {
                match decode(a.call(OwnerOperation::ReviewCancellation, (id,)).await?)? {
                    ReviewResult::Review {
                        offered: Offered { handle, view },
                    } => OwnerGrantCancellationResult::Review(Box::new(
                        OwnerGrantCancellationReview {
                            view,
                            custody: remote_custody(a, &handle),
                        },
                    )),
                    ReviewResult::Progress { progress } => {
                        OwnerGrantCancellationResult::Progress(Box::new(progress))
                    }
                }
            }
        };
        self.check()?;
        Ok(result)
    }
    pub async fn confirm_cancellation(
        &self,
        review: OwnerGrantCancellationReview,
    ) -> Result<native::OwnerGrantProgress, NativeError> {
        self.check()?;
        let result = match (&self.inner, review.custody) {
            (Access::Local(a), Custody::Local(r)) => a.confirm_cancellation(r).await,
            (
                Access::Remote(a),
                Custody::Remote {
                    handle,
                    access,
                    until,
                },
            ) if access == a.id() && until > Instant::now() => decode(
                a.call(OwnerOperation::ConfirmCancellation, (handle,))
                    .await?,
            ),
            _ => Err(error(409, "daemon_review_expired")),
        };
        self.check()?;
        result
    }
}
fn remote_custody<T>(access: &RemoteAccess, handle: &str) -> Custody<T> {
    Custody::Remote {
        handle: handle.into(),
        access: access.id(),
        until: Instant::now() + Duration::from_secs(60),
    }
}
macro_rules! access_reads {
    ($($name:ident($($arg:ident:$ty:ty),*)->$result:ty = $op:ident;)*)=>{impl OwnerGrantAccess{$(pub async fn $name(&self,$($arg:$ty),*)->Result<$result,NativeError>{self.check()?;let result=match &self.inner{Access::Local(a)=>a.$name($($arg),*).await,Access::Remote(a)=>decode(a.call(OwnerOperation::$op,($($arg,)* )).await?)};self.check()?;result})*}}
}
access_reads! {
    publication_form(contribution:String)->native::OwnerGrantForm=PublicationForm;
    supports_succession()->bool=SupportsSuccession;
    succession_form(contribution:String,kind:native::OwnerGrantSuccessionKind)->native::OwnerGrantSuccessionForm=SuccessionForm;
    recover(id:String)->native::OwnerGrantProgress=RecoverGrant;
    list_existing()->Vec<native::OwnerGrantSummary>=ListGrants;
    check_new_admission(contribution:String)->()=CheckAdmission;
    observe_publication(id:String)->Option<native::OwnerGrantPublicationProgress>=ObservePublication;
}

#[cfg(any(unix, test))]
enum Retained {
    Grant(native::OwnerGrantReview),
    Publication(native::OwnerGrantPublicationReview),
    Cancellation(native::OwnerGrantCancellationReview),
}
#[cfg(any(unix, test))]
struct RetainedAccess {
    controller: String,
    package: Package,
    access: native::OwnerGrantAccess,
}
#[cfg(any(unix, test))]
struct RetainedReview {
    access: String,
    until: Instant,
    review: Retained,
}
#[cfg(any(unix, test))]
struct RetainedDevice {
    controller: String,
    package: Package,
    until: Instant,
    review: native::DeviceSetupReview,
}
#[cfg(any(unix, test))]
struct RetainedSuccessor {
    controller: String,
    package: Package,
    until: Instant,
    executor: Arc<native::CurrentSharedExecutor>,
    prepared: Box<native::PreparedSharedSuccessor>,
}
#[derive(Default)]
#[cfg(any(unix, test))]
pub(super) struct GrantState {
    accesses: HashMap<String, RetainedAccess>,
    reviews: HashMap<String, RetainedReview>,
    devices: HashMap<String, RetainedDevice>,
    successors: HashMap<String, RetainedSuccessor>,
}
#[cfg(any(unix, test))]
impl GrantState {
    pub(super) fn revoke_all(&mut self) {
        for (_, a) in self.accesses.drain() {
            a.access.retire()
        }
        self.reviews.clear();
        self.devices.clear();
        self.successors.clear();
    }
    pub(super) fn revoke_controller(&mut self, controller: &str) {
        self.accesses.retain(|_, a| {
            if a.controller == controller {
                a.access.retire();
                false
            } else {
                true
            }
        });
        self.devices.retain(|_, d| d.controller != controller);
        self.successors.retain(|_, d| d.controller != controller);
        self.prune();
    }
    pub(super) fn retire(&mut self, controller: &str, access: &str) {
        if self
            .accesses
            .get(access)
            .is_some_and(|a| a.controller == controller)
        {
            if let Some(a) = self.accesses.remove(access) {
                a.access.retire();
            }
        }
        self.prune();
    }
    fn prune(&mut self) {
        let now = Instant::now();
        self.accesses.retain(|_, a| {
            let keep = !a.access.retired();
            if !keep {
                a.access.retire()
            }
            keep
        });
        self.reviews
            .retain(|_, r| r.until > now && self.accesses.contains_key(&r.access));
        self.devices
            .retain(|_, d| d.until > now && !d.review.expired());
        self.successors
            .retain(|_, s| s.until > now && !s.prepared.expired());
    }
    fn access(
        &mut self,
        controller: &str,
        package: &Package,
        id: &str,
    ) -> Result<native::OwnerGrantAccess, NativeError> {
        self.prune();
        let a = self
            .accesses
            .get_mut(id)
            .filter(|a| a.controller == controller && a.package == *package)
            .ok_or_else(|| error(401, "daemon_bridge_lease_expired"))?;
        Ok(a.access.clone())
    }
    fn offer<T: Serialize>(
        &mut self,
        access: &str,
        review: Retained,
        view: T,
    ) -> Result<Value, NativeError> {
        self.prune();
        if self.reviews.len() >= 256 {
            return Err(error(503, "daemon_review_limit"));
        }
        let handle = super::super::new_request_id();
        self.reviews.insert(
            handle.clone(),
            RetainedReview {
                access: access.into(),
                until: Instant::now() + Duration::from_secs(60),
                review,
            },
        );
        encode(Offered { handle, view })
    }
    fn take(&mut self, access: &str, handle: &str) -> Result<Retained, NativeError> {
        self.prune();
        if !self.reviews.get(handle).is_some_and(|r| r.access == access) {
            return Err(error(409, "daemon_review_expired"));
        }
        Ok(self.reviews.remove(handle).unwrap().review)
    }
}
#[cfg(any(unix, test))]
fn lock(state: &Mutex<GrantState>) -> Result<std::sync::MutexGuard<'_, GrantState>, NativeError> {
    state
        .lock()
        .map_err(|_| error(503, "daemon_bridge_unavailable"))
}
#[cfg(any(unix, test))]
fn offer_grant(
    state: &Mutex<GrantState>,
    access: &str,
    r: native::OwnerGrantReview,
) -> Result<Value, NativeError> {
    let view = ReviewView {
        summary: r.summary(),
        context: r.context().clone(),
        capacity_available: r.capacity_available(),
    };
    lock(state)?.offer(access, Retained::Grant(r), view)
}
#[cfg(any(unix, test))]
fn offer_publication(
    state: &Mutex<GrantState>,
    access: &str,
    r: native::OwnerGrantPublicationReview,
) -> Result<Value, NativeError> {
    let view = PublicationView {
        summary: r.summary(),
        server_origin: r.server_origin().into(),
        contribution_revision: r.contribution_revision(),
        observed_at_ms: r.observed_at_ms(),
    };
    lock(state)?.offer(access, Retained::Publication(r), view)
}
#[cfg(any(unix, test))]
fn offer_cancellation(
    state: &Mutex<GrantState>,
    access: &str,
    r: native::OwnerGrantCancellationReview,
) -> Result<Value, NativeError> {
    let view = CancellationView {
        summary: r.summary(),
        server_origin: r.server_origin().into(),
    };
    lock(state)?.offer(access, Retained::Cancellation(r), view)
}
#[cfg(any(unix, test))]
pub(super) async fn dispatch(
    state: &Mutex<GrantState>,
    controller: &str,
    package: &Package,
    root: &Path,
    origin: &str,
    owner: &str,
    native: &NativeOwner,
    shared: Option<Arc<native::CurrentSharedExecutor>>,
    live: Arc<AtomicBool>,
    controller_lifetime: Arc<dyn Fn()->Option<Instant>+Send+Sync>,
    operation: OwnerOperation,
    arguments: Value,
) -> Result<Value, NativeError> {
    let deadline=package.deadline()?;
    if matches!(
        operation,
        OwnerOperation::ListSuccessors
            | OwnerOperation::AssessSuccessor
            | OwnerOperation::SuccessorDescription
            | OwnerOperation::StartSuccessor
            | OwnerOperation::ResumeSuccessor
    ) {
        let executor = shared.ok_or_else(|| error(503, "shared_execution_unavailable"))?;
        return match operation {
            OwnerOperation::ListSuccessors => {
                encode(executor.list_successors(decode(arguments)?).await?)
            }
            OwnerOperation::SuccessorDescription => {
                encode(executor.successor_description(decode(arguments)?).await?)
            }
            OwnerOperation::ResumeSuccessor => {
                encode(executor.resume_successor(decode(arguments)?).await?)
            }
            OwnerOperation::AssessSuccessor => {
                match executor.assess_successor(decode(arguments)?).await? {
                    native::SharedSuccessorAssessment::Blocked(review) => {
                        encode(SuccessorView::Blocked { review })
                    }
                    native::SharedSuccessorAssessment::Ready(prepared) => {
                        let prepared =
                            Box::new((*prepared).restrict_to_host(Arc::new(move || {
                                live.load(Ordering::Acquire) && controller_lifetime().is_some()
                                    && deadline.is_none_or(|until| Instant::now() < until)
                            }))?);
                        let view = prepared.review().clone();
                        let mut state = lock(state)?;
                        state.prune();
                        if state.successors.len() >= 64 {
                            return Err(error(503, "daemon_review_limit"));
                        }
                        let handle = super::super::new_request_id();
                        state.successors.insert(
                            handle.clone(),
                            RetainedSuccessor {
                                controller: controller.into(),
                                package: package.clone(),
                                until: Instant::now() + Duration::from_secs(60),
                                executor,
                                prepared,
                            },
                        );
                        encode(SuccessorView::Ready {
                            offered: Offered { handle, view },
                        })
                    }
                }
            }
            OwnerOperation::StartSuccessor => {
                let handle: String = decode(arguments)?;
                let retained = {
                    let mut state = lock(state)?;
                    state.prune();
                    if !state.successors.get(&handle).is_some_and(|s| {
                        s.controller == controller
                            && s.package == *package
                            && Arc::ptr_eq(&s.executor, &executor)
                    }) {
                        return Err(error(409, "shared_successor_preparation_changed"));
                    }
                    state.successors.remove(&handle).unwrap()
                };
                encode(executor.start_successor(*retained.prepared).await?)
            }
            _ => unreachable!(),
        };
    }
    if matches!(
        operation,
        OwnerOperation::DeviceSetupContext
            | OwnerOperation::ReviewDeviceSetup
            | OwnerOperation::DeviceSetupState
            | OwnerOperation::ConfirmDeviceSetup
    ) {
        let selected =
            native::CurrentProfileSelection::load_for_host(root, origin, owner).await?;
        return match operation {
            OwnerOperation::DeviceSetupContext => {
                let (): () = decode(arguments)?;
                encode(
                    native
                        .device_setup_context(package.native(), selected.as_ref())
                        .await?,
                )
            }
            OwnerOperation::DeviceSetupState => {
                let (): () = decode(arguments)?;
                let phase = if let Some(selected) = selected {
                    selected.device_setup_phase(native).await?
                } else {
                    "uninitialized"
                };
                encode(phase)
            }
            OwnerOperation::ReviewDeviceSetup => {
                let (): () = decode(arguments)?;
                let review = native
                    .review_device_setup(package.native(), selected)
                    .await?;
                let view = DeviceView {
                    context: review.context().clone(),
                };
                let mut state = lock(state)?;
                state.prune();
                if state.devices.len() >= 32 {
                    return Err(error(503, "daemon_review_limit"));
                }
                let handle = super::super::new_request_id();
                state.devices.insert(
                    handle.clone(),
                    RetainedDevice {
                        controller: controller.into(),
                        package: package.clone(),
                        until: deadline.map_or(Instant::now() + Duration::from_secs(60), |until| {
                            until.min(Instant::now() + Duration::from_secs(60))
                        }),
                        review,
                    },
                );
                encode(Offered { handle, view })
            }
            OwnerOperation::ConfirmDeviceSetup => {
                let handle: String = decode(arguments)?;
                let retained = {
                    let mut state = lock(state)?;
                    state.prune();
                    if !state
                        .devices
                        .get(&handle)
                        .is_some_and(|d| d.controller == controller && d.package == *package)
                    {
                        return Err(error(409, "daemon_review_expired"));
                    }
                    state.devices.remove(&handle).unwrap()
                };
                native
                    .confirm_device_setup(retained.review, package.native(), move || {
                        live.load(Ordering::Acquire) && controller_lifetime().is_some() && Instant::now() < retained.until
                    })
                    .await?;
                Ok(Value::Null)
            }
            _ => unreachable!(),
        };
    }
    if operation == OwnerOperation::AttachGrants {
        if arguments != Value::Null {
            return Err(error(400, "invalid_management_arguments"));
        }
        let selected = native::CurrentProfileSelection::load_for_host(root, origin, owner)
            .await?
            .ok_or_else(|| error(409, "current_profile_required"))?;
        let mut a = selected
            .grant_access(native, package.native())
            .await?
            .bind_controller_lifetime(controller_lifetime.clone())?
            .restrict_to_host(move || {
                live.load(Ordering::Acquire) && controller_lifetime().is_some() && deadline.is_none_or(|until| Instant::now() < until)
            })?;
        if let Some(deadline) = deadline {
            a = a.restrict_to_deadline(deadline)?;
        }
        let mut state = lock(state)?;
        state.prune();
        if state.accesses.len() >= 64 {
            a.retire();
            return Err(error(503, "daemon_bridge_busy"));
        }
        let id = super::super::new_request_id();
        state.accesses.insert(
            id.clone(),
            RetainedAccess {
                controller: controller.into(),
                package: package.clone(),
                access: a,
            },
        );
        return encode(id);
    }
    let (id, args): (String, Value) = decode(arguments)?;
    let a = lock(state)?.access(controller, package, &id)?;
    match operation {
        OwnerOperation::PublicationForm => {
            let (c,): (String,) = decode(args)?;
            encode(a.publication_form(c).await?)
        }
        OwnerOperation::SupportsSuccession => {
            let (): () = decode(args)?;
            encode(a.supports_succession().await?)
        }
        OwnerOperation::SuccessionForm => {
            let (c, k): (String, native::OwnerGrantSuccessionKind) = decode(args)?;
            encode(a.succession_form(c, k).await?)
        }
        OwnerOperation::PrepareGrant => {
            let (c, choices): (String, Choices) = decode(args)?;
            offer_grant(state, &id, a.prepare(c, choices.into()).await?)
        }
        OwnerOperation::PrepareSuccessor => {
            let (c, k, choices): (String, native::OwnerGrantSuccessionKind, Choices) =
                decode(args)?;
            offer_grant(state, &id, a.prepare_successor(c, k, choices.into()).await?)
        }
        OwnerOperation::ReviewGrant => {
            let (original,): (String,) = decode(args)?;
            match a.review(original).await? {
                native::OwnerGrantReviewResult::Review(r) => {
                    let offered: Offered<ReviewView> = decode(offer_grant(state, &id, *r)?)?;
                    encode(ReviewResult::<_, native::OwnerGrantProgress>::Review { offered })
                }
                native::OwnerGrantReviewResult::Progress(p) => {
                    encode(ReviewResult::<ReviewView, _>::Progress { progress: *p })
                }
            }
        }
        OwnerOperation::ConfirmGrant => {
            let (handle,): (String,) = decode(args)?;
            let Retained::Grant(review) = lock(state)?.take(&id, &handle)? else {
                return Err(error(400, "daemon_review_kind_mismatch"));
            };
            encode(a.confirm(review).await?)
        }
        OwnerOperation::RecoverGrant => {
            let (original,): (String,) = decode(args)?;
            encode(a.recover(original).await?)
        }
        OwnerOperation::ListGrants => {
            let (): () = decode(args)?;
            encode(a.list_existing().await?)
        }
        OwnerOperation::CheckAdmission => {
            let (c,): (String,) = decode(args)?;
            encode(a.check_new_admission(c).await?)
        }
        OwnerOperation::ObservePublication => {
            let (original,): (String,) = decode(args)?;
            encode(a.observe_publication(original).await?)
        }
        OwnerOperation::ReviewPublication | OwnerOperation::RecoverPublication => {
            let (original,): (String,) = decode(args)?;
            let result = if operation == OwnerOperation::RecoverPublication {
                a.recover_publication(original).await?
            } else {
                a.review_publication(original).await?
            };
            match result {
                native::OwnerGrantPublicationResult::Review(r) => {
                    let offered: Offered<PublicationView> =
                        decode(offer_publication(state, &id, *r)?)?;
                    encode(
                        ReviewResult::<_, native::OwnerGrantPublicationProgress>::Review {
                            offered,
                        },
                    )
                }
                native::OwnerGrantPublicationResult::Progress(p) => {
                    encode(ReviewResult::<PublicationView, _>::Progress { progress: *p })
                }
            }
        }
        OwnerOperation::ConfirmPublication => {
            let (handle,): (String,) = decode(args)?;
            let Retained::Publication(review) = lock(state)?.take(&id, &handle)? else {
                return Err(error(400, "daemon_review_kind_mismatch"));
            };
            encode(a.confirm_publication(review).await?)
        }
        OwnerOperation::ReviewCancellation => {
            let (original,): (String,) = decode(args)?;
            match a.review_cancellation(original).await? {
                native::OwnerGrantCancellationResult::Review(r) => {
                    let offered: Offered<CancellationView> =
                        decode(offer_cancellation(state, &id, *r)?)?;
                    encode(ReviewResult::<_, native::OwnerGrantProgress>::Review { offered })
                }
                native::OwnerGrantCancellationResult::Progress(p) => {
                    encode(ReviewResult::<CancellationView, _>::Progress { progress: *p })
                }
            }
        }
        OwnerOperation::ConfirmCancellation => {
            let (handle,): (String,) = decode(args)?;
            let Retained::Cancellation(review) = lock(state)?.take(&id, &handle)? else {
                return Err(error(400, "daemon_review_kind_mismatch"));
            };
            encode(a.confirm_cancellation(review).await?)
        }
        OwnerOperation::AttachGrants
        | OwnerOperation::DeviceSetupContext
        | OwnerOperation::ReviewDeviceSetup
        | OwnerOperation::DeviceSetupState
        | OwnerOperation::ConfirmDeviceSetup
        | OwnerOperation::ListSuccessors
        | OwnerOperation::AssessSuccessor
        | OwnerOperation::SuccessorDescription
        | OwnerOperation::StartSuccessor
        | OwnerOperation::ResumeSuccessor => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn package() -> native::ManagementPackage {
        native::ManagementPackage {
            digest: "a".repeat(64),
            instance: 1,
            account_epoch: 1,
            permissions: vec![],
        }
    }
    fn summary() -> native::OwnerGrantSummary {
        serde_json::from_value(serde_json::json!({"intentId":"original","grantId":"grant","ownerMxid":"@owner:test","contributionId":"pool","resourceId":"resource","coordinatorMxid":"@coordinator:test","allocatedTokens":100,"eligibleManagers":["@owner:test"],"allowSelfApproval":false,"delegationExpiresAtMs":1000,"periodKey":"2026-11","periodStartMs":0,"periodEndMs":1000})).unwrap()
    }
    #[test]
    fn native_choices_wire_refuses_caller_supplied_originals_and_authority() {
        let mut value = serde_json::json!({"coordinatorMxid":"@coordinator:test","delegationExpiresAtMs":1000,"allowSelfApproval":false,"eligibleManagers":["@owner:test"],"allocatedTokens":100});
        assert!(serde_json::from_value::<Choices>(value.clone()).is_ok());
        for forbidden in ["intentId", "grantId", "receipt", "registry", "ownerMxid"] {
            value[forbidden] = Value::String("forged".into());
            assert!(serde_json::from_value::<Choices>(value.clone()).is_err());
            value.as_object_mut().unwrap().remove(forbidden);
        }
    }
    #[tokio::test]
    async fn review_cannot_move_between_access_handles_or_outlive_its_deadline() {
        let temp = tempfile::tempdir().unwrap();
        let controller = ControllerClient::new(
            temp.path().into(),
            "@owner:test".into(),
            "https://test/".into(),
            1,
        );
        let remote = Arc::new(RemoteAccess {
            controller,
            package: package().into(),
            id: Mutex::new("access-original".into()),
            calls:tokio::sync::Mutex::new(()),
            retired: AtomicBool::new(false),
        });
        let access = OwnerGrantAccess {
            inner: Access::Remote(remote.clone()),
            gate: None,
        };
        for (binding, until) in [
            ("access-other", Instant::now() + Duration::from_secs(30)),
            ("access-original", Instant::now() - Duration::from_secs(1)),
        ] {
            let review = OwnerGrantCancellationReview {
                view: CancellationView {
                    summary: summary(),
                    server_origin: "https://test/".into(),
                },
                custody: Custody::Remote {
                    handle: "sealed".into(),
                    access: binding.into(),
                    until,
                },
            };
            assert_eq!(
                access.confirm_cancellation(review).await.unwrap_err().code,
                "daemon_review_expired"
            );
        }
        // A successful ordinary-read reattachment changes access identity;
        // an earlier still-time-valid sealed confirmation cannot follow it.
        *remote.id.lock().unwrap()="access-replacement".into();
        let old=OwnerGrantCancellationReview {
            view:CancellationView{summary:summary(),server_origin:"https://test/".into()},
            custody:Custody::Remote{handle:"sealed-original".into(),access:"access-original".into(),until:Instant::now()+Duration::from_secs(30)},
        };
        assert_eq!(access.confirm_cancellation(old).await.unwrap_err().code,"daemon_review_expired");
        assert!(
            !temp.path().join("controllers").exists(),
            "invalid review must fail before IPC or durable mutation admission"
        );
    }
    #[test]
    fn native_review_closure_does_not_generate_replacement_commands() {
        let deadline = Instant::now() + Duration::from_secs(30);
        let prepared = PreparedSharedSuccessor {
            package: package().into(),
            handle: "original-proof".into(),
            review: native::SharedSuccessorReview {
                agent_name: "agent".into(),
                project_id: "project".into(),
                assignment_id: "assignment".into(),
                predecessor_command_id: "original-command".into(),
                process_attempt: 2,
                allocated_tokens: 100,
                known_consumed_tokens: 20,
                retained_tokens: 0,
                remaining_tokens: 80,
                usage_complete: true,
            },
            until: deadline,
            gate: None,
        };
        let live = Arc::new(AtomicBool::new(true));
        let check = live.clone();
        let prepared = prepared
            .restrict_to_host(Arc::new(move || check.load(Ordering::Acquire)))
            .unwrap();
        assert!(!prepared.expired());
        live.store(false, Ordering::Release);
        assert!(prepared.expired());
        assert_eq!(prepared.handle, "original-proof");
        assert_eq!(prepared.review.predecessor_command_id, "original-command");
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn ordinary_grant_read_reattaches_expired_controller_over_private_ipc() {
        use crate::daemon::{dispatch::{Runtime,BoxFuture,OwnerScope,Observation,Phase,SnapshotParts,Controller},protocol::{Period,DaemonError},control::ControlStore,ipc::Server};
        use std::sync::atomic::AtomicU64;
        struct Probe {controller:AtomicU64,access:AtomicU64,expire:AtomicBool}
        impl Runtime for Probe {
            fn owner(&self)->Option<OwnerScope>{Some(OwnerScope{user_id:"@owner:test".into(),server:"https://test/".into(),display_name:"Owner".into()})}
            fn observe(&self)->Observation{Observation{phase:Phase::Running,detail:None,active_runs:Some(0),pending_approvals:None}}
            fn close_admission(&self)->BoxFuture<'_,bool>{Box::pin(async{true})}
            fn open_admission(&self){}
            fn start(&self){}
            fn stop(&self)->BoxFuture<'_,Result<(),&'static str>>{Box::pin(async{Ok(())})}
            fn snapshot_parts(&self,_:Option<Period>,_:Option<hagency_metering::dashboard::UsageFilter>)->BoxFuture<'_,SnapshotParts>{Box::pin(async{SnapshotParts::default()})}
            fn refresh_providers(&self)->BoxFuture<'_,Result<(),DaemonError>>{Box::pin(async{Ok(())})}
            fn bridge(&self,_:String,call:super::super::Call)->BoxFuture<'_,Result<Value,super::super::Failure>> {
                Box::pin(async move {
                    let result:Result<Value,NativeError>=async {
                    match call {
                        super::super::Call::Attach{..}=>{
                            let n=self.controller.fetch_add(1,Ordering::AcqRel)+1;
                            encode(super::super::Lease{id:format!("controller-{n}"),generation:1})
                        }
                        super::super::Call::OwnerAccess{controller,operation,arguments,..}=>{
                            if self.expire.swap(false,Ordering::AcqRel) {return Err(error(401,"daemon_bridge_lease_expired"));}
                            assert_eq!(controller,format!("controller-{}",self.controller.load(Ordering::Acquire)));
                            match operation {
                                OwnerOperation::AttachGrants=>{let n=self.access.fetch_add(1,Ordering::AcqRel)+1;encode(format!("access-{n}"))},
                                OwnerOperation::SupportsSuccession=>{
                                    let (id,_):(String,Value)=decode(arguments)?;
                                    assert_eq!(id,format!("access-{}",self.access.load(Ordering::Acquire)));
                                    encode(true)
                                }
                                _=>panic!("read recovery must not submit native mutations"),
                            }
                        }
                        _=>Err(error(400,"fixture_unexpected_operation")),
                    }
                    }.await;
                    result.map_err(Into::into)
                })
            }
        }
        let temp=tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(temp.path(),std::fs::Permissions::from_mode(0o700)).unwrap();
        let probe=Arc::new(Probe{controller:AtomicU64::new(0),access:AtomicU64::new(0),expire:AtomicBool::new(false)});
        let server_controller=Controller::new(ControlStore::open(&temp.path().join(crate::daemon::CONTROL)).unwrap(),probe.clone());
        let mut server=Server::start(temp.path(),server_controller).unwrap();
        let controller=ControllerClient::new(temp.path().into(),"@owner:test".into(),"https://test/".into(),1);
        let access=OwnerGrantAccess::remote(controller,package()).await.unwrap();
        assert!(access.supports_succession().await.unwrap());
        probe.expire.store(true,Ordering::Release);
        assert!(access.supports_succession().await.unwrap());
        assert_eq!(probe.controller.load(Ordering::Acquire),2);
        assert_eq!(probe.access.load(Ordering::Acquire),2);
        server.stop().await;
    }

}
