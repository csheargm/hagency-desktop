//! A single SQLite owner on a dedicated bounded worker; no IO in async handlers.
pub use domain::owner_resources;
pub use domain::resource_configuration::{
    CeilingAmendmentResult, CeilingChange, ProfileChange, ResourceConfigurationAccess,
    ResourceConfigurationCommand, ResourceConfigurationResult, ResourceEngagementChange,
};
pub mod agent_home;
pub mod backup;
mod database;
mod domain;
pub mod installation_authority;
pub use domain::DOMAIN_SCHEMA_VERSION;
pub use domain::coordinator;
pub use domain::joined_rooms::{JoinedRoom, JoinedRoomState, MAX_JOINED_ROOMS};
pub use domain::owner_anchors::OwnerAnchor;
mod domain_worker;
pub mod management;
pub mod private;
pub mod task_context;
pub use domain::PrivateApprovalCard;
pub use domain::StaleMatrixSessionReceipt;
pub use domain::resource_publication::{
    ResourcePublicationAccess, ResourcePublicationCommand, ResourcePublicationResult,
    ResourcePublicationRetirement, resource_publication_revision,
};
pub use domain::uploads::UploadSettlement;
pub use domain::{
    ALERT_KIND_CEILING_OVERRUN, ALERT_KIND_OTHER, ALERT_KINDS, ALERT_STATUSES, ActivityEvent, ActivityUpdate, AgentDefinition, AgentDetail, AgentDetailRoom,
    AgentFence, AgentRosterRow, AlertListFilter, AlertNote, AlertPatch, AlertStats,
    AlertTransition, AttachmentTicket, AttemptClock, AttemptClockRow, AttemptEvent,
    AttemptEventRow, AttemptPhase, CeilingAlert, CeilingReport, Contribution, CorpusSweepOutcome,
    Credential, DeliveryEventRow, DomainRepository, ENDED_LIMIT, EXECUTION_RETENTION_BATCH,
    EXECUTION_RETENTION_DISPATCHES, EXECUTION_RETENTION_ROWS, Effect, EffectOutcome, EffectState,
    EngagementLabel, EngagementPruneOutcome, EngagementRetentionStatus, ExecutionPruneOutcome,
    FenceReason, IssueSideRegistration, IssueSideRegistrationRequest, KnownTokens,
    MAX_ENGAGEMENT_USAGE_PERIODS, MAX_ENGAGEMENT_USAGE_SOURCES, MAX_OPEN_CEILING_ALERTS,
    MAX_SOURCE_USAGE_RECEIPTS, MAX_TASK_COMMENTS, MAX_TASK_PAGE, MAX_USAGE_PERIODS,
    MAX_USAGE_RECEIPTS, MAX_USAGE_SOURCES, MESSAGE_RETENTION_FLOOR, NewOperatorMessage,
    OVER_BUDGET_NOTICE_KIND, OfferBook, OfferResource, OfferRole, OfferServing, OperatorMessage,
    OperatorTask, OperatorTaskComment, OverBudgetNotice, OwnedClaimProfile, OwnedClaimRoom,
    OwnedCompletion, OwnedDispatchScope, OwnedFailure, OwnedObservation, OwnedProvisionScope,
    OwnedRuntimeLease, PEER_RECEIPT_CEILING, PEER_RETENTION_CEILING, PEER_RETENTION_FLOOR,
    PeerRetentionStatus, PeerSweepOutcome, PendingInvite, Preview, ProjectSide, QuotaStatus,
    Reminder, ReminderReceipt, ReminderSweep, Representative, RetentionStatus, RoleOffer,
    RuntimeProfile, RuntimeProfileRole, SideBudget, SideCommitment, SideCredential, SideProject,
    SideProjectRecord, SideRecord, SourceUsage, SuppressOutcome, Suppression, SweepOutcome,
    TASK_GRANULARITIES, TASK_PRIORITIES, TASK_STATUSES, TaskFilters, Tombstone, UploadAdmission,
    UploadClaim, UploadIdentity, UploadPreparation, UploadSend, UsageCeiling, UsageEvidence,
    UsagePeriod, UsagePeriodKind, UsageReceipt, UsageReport, UsageSource, UsageSummary,
    UsageTotals, WhitelistEntry, allowed_transitions, operator_transitions,
    over_budget_notice_body,
};
pub use domain::{DeliveryFeedback, DeliveryWarning, DirectTarget, MentionState, MentionTarget};
pub use domain::{OutcomeAction, OutcomeResolution};
pub use domain::{PublishedCatalog, publication_fingerprint};
pub use domain::{
    SessionOverrides, THREAD_DIRECTIVE_OPERATOR_REFUSAL, ThreadDirective, ThreadMode, confirmation,
    parse,
};
pub use domain_worker::DomainStore;
pub mod outbound;
mod repository;
mod shutdown;
mod worker;
pub use repository::Repository;
pub use shutdown::{
    NativeWriterObservation, NativeWriterUnavailable, ShutdownOutcome, ShutdownSnapshot,
};
pub use worker::Store;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("local management session is expired or retired")]
    LocalAuthority,
    #[error("local resources require device setup")]
    ResourceSetupRequired,
    #[error("runner capability is missing, stale or outside the task scope")]
    RunnerAuthority,
    #[error("approval was already consumed")]
    AlreadyConsumed,
    #[error("approval is not consumable in its settled state")]
    NotConsumable,
    #[error("room snapshot is unsafe and was refused: {0}")]
    UnsafeSnapshot(String),
    #[error("session or resource requires inspected recovery")]
    Quarantined,
    #[error("workspace has unresolved execution requiring inspection")]
    WorkspaceInspectionRequired,
    #[error("workspace has an active execution lease")]
    WorkspaceActive,
    #[error("stopped execution cannot currently be resolved")]
    StoppedNotResolvable,
    #[error("stopped execution review expired")]
    StoppedReviewExpired,
    #[error("invalid input: {0}")]
    Invalid(#[from] hagency_core::InvalidInput),
    #[error("request identifier was reused with different content")]
    Conflict,
    #[error("registration generation differs from its durable binding")]
    Generation,
    /// Owner grant review budget changed; emitted only before any ledger effect.
    #[error("the budget shown in the owner grant review changed")]
    OwnerGrantReviewChanged,
    /// Emitted before a local-name transaction effect; not a general conflict.
    #[error("the local resource name revision changed")]
    ResourceNameRevisionChanged,
    #[error("the resource configuration revision changed")]
    ResourceEditRevisionChanged,
    /// Exact original receipt was absent and the source CAS failed before
    /// creating anything; distinct from command collision or uncertain commit.
    #[error("the source resource configuration changed")]
    ResourceConfigurationSourceChanged,
    #[error("resource configuration requires an amendment or history recovery")]
    ResourceEditNotAvailable,
    /// A monthly-maximum amendment was refused before any ledger effect.
    #[error("the monthly maximum amendment was refused: {0:?}")]
    CeilingAmendmentRefused(management::CeilingAmendmentRefusal),
    #[error("operator task revision changed")]
    OperatorTaskRevisionChanged,
    #[error("domain object does not exist")]
    NotFound,
    #[error("selected resource is withdrawn or does not qualify")]
    Unqualified,
    #[error("selected resource or declared shared seat has insufficient capacity")]
    InsufficientCapacity,
    #[error("cannot allocate against an agent with no declared ceiling")]
    NoCeiling,
    #[error("{message}")]
    OverCommit { message: String },
    #[error("domain operation is not valid in its current state")]
    State,
    #[error("the saved management change was cancelled before application")]
    ManagementChangeCancelled,
    #[error("state is owned by another process")]
    Locked,
    #[error("state format is corrupt or newer than this binary")]
    Schema,
    #[error("state must be an owner-private directory containing regular files")]
    Private,
    #[error("private state on this platform has not passed the native permission gate")]
    PlatformUnavailable,
    #[error("durable store capacity is exhausted; pending records were retained")]
    Capacity,
    #[error("worker queue is full; retry the same request identifier")]
    Busy,
    #[error("worker stopped; inspect or retry the same request identifier")]
    Unavailable,
    #[error("processing outcome is unknown; reconcile the original command before retrying")]
    OutcomeUnknown,
    #[error("storage error")]
    Sqlite(#[from] rusqlite::Error),
    #[error("filesystem error")]
    Io(#[from] std::io::Error),
    #[error("serialization error")]
    Json(#[from] serde_json::Error),
}

// Separate file-event custody; no Matrix acknowledgement proof constructor.
pub use domain::file_delivery::{
    FileDeliveryAdmission, FileDeliveryIdentity, FileDeliverySettlement, FilePublicationClaim,
    FilePublicationSend,
};

// Original local cache association; none of these values is SDK/file proof.
pub use domain::received_files::{
    ReceiveAdmission, ReceiveIdentity, ReceiveReservation, ReceiveWrite,
};

// Original one-shot router response authority; no native application proof.
pub use domain::{
    ApprovalResponseGrant, ApprovalResponseObservation, ApprovalResponseState,
    ApprovalResponseSummary,
};

pub use domain::{OwnedApprovalScope, OwnedApprovalStatus};

pub use domain::accounts::{
    ACCOUNT_PROFILE, AccountChoice, AccountEnrollmentAccess, AccountEnrollmentCommand,
    AccountReadiness, AccountReadinessMode, AccountState, DEFAULT_READINESS_TTL, LoginAttempt,
    LoginOutcome, LoginVerdict, LogoutObservation, LogoutReadiness, ManagedAccount, ManagedLaunch,
};

/// Scoped OS credentials and authenticated credential documents.
pub mod credential_vault;
