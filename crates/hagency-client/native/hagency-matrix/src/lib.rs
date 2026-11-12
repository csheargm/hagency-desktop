//! Bounded authenticated account/room observations and scoped SDK event intake.
//! Host-owned frozen sends require verified crypto. Explicit fresh-account
//! enrollment establishes anchored identities and original recipient sessions;
//! general account recovery and production service cutover remain separate.
mod attachments;
pub use attachments::AttachmentHandle;
mod collector;
mod config;
mod enrollment;
mod event_batch;
mod http;
pub use http::UploadResponse;
mod identity_polish;
mod intake;
mod invites;
pub mod join_backfill;
mod media_download;
mod media_upload;
mod membership_sweep;
mod outgoing;
mod receive;
mod representative_sync;
mod retire;
mod room_trust;
mod upload;
pub use upload::{FilePublicationAdmissionFailure, FilePublicationOperation};
pub use upload::{StagedUpload, UploadAdmissionFailure, UploadOperation};
mod provisioning;
mod sdk;
mod token_provision;
mod wire;
pub use collector::{Collector, ObservationSummary};
pub use config::{HostConfig, HostIdentity, HostRoom, Limits};
pub use http::RequestPacing;
pub use intake::{HostIntakePlan, IntakeStatus, IntakeSummary};
pub use media_download::{MediaDownloadError, MediaDownloadLimits, MediaDownloader, MediaId};
pub use media_upload::{
    MediaUploadError, MediaUploadLimits, MediaUploader, UploadAttempt, UploadState,
};
pub use membership_sweep::{MEMBERSHIP_SWEEP_INTERVAL, MembershipSweep, SweepOutcome};
pub use outgoing::{OutgoingState, OutgoingSummary};
pub use provisioning::{PassReport, ProvisionedAgent, TokenProvisioningHost};
pub use receive::{ReceiveError, ReceivedAttachment, ReceivedScope};
pub use representative_sync::{
    BoxFuture, CircuitBreak, EventMeta, HistoryPage, PageReader, PageSink, PendingVerdict,
    RepresentativeHttp, RepresentativeSync, SyncBatch, SyncDriver, SyncError, SyncHooks, SyncState,
    SyncStats, reconcile_timeline,
};
pub use retire::{AgentRetirement, RetireClient, RetireVerdict};
pub use room_trust::{RoomTrust, RoomTrustReason, TrustMode};
pub use token_provision::{
    ApplicationServiceCredential, ProvisionedTokenAccount, TokenAccountProvision,
};
pub use tokio_util::sync::CancellationToken;
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid host Matrix configuration")]
    Config,
    #[error("Matrix collector or SDK owner is busy")]
    Busy,
    #[error("Matrix operation cancelled")]
    Cancelled,
    #[error("Matrix operation timed out")]
    Timeout,
    /// The new agent's rooms exist and the agent is in them; the owner has not
    /// joined the agent's DM yet. Not a failure: the provision stays Started and
    /// is resumed on a later turn. Waiting for a human has no deadline.
    #[error("the owner has not joined the new agent's room yet")]
    AwaitingOwner,
    #[error("Matrix transport request failed")]
    Transport,
    #[error("Matrix redirect refused")]
    Redirect,
    #[error("Matrix headers exceed bounds or use unsupported framing")]
    Headers,
    #[error("Matrix body exceeds its byte limit")]
    BodyTooLarge,
    #[error("Matrix response is not one bounded unambiguous JSON document")]
    InvalidJson,
    #[error("Matrix response does not satisfy the observation contract")]
    Wire,
    #[error("Matrix authenticated account or device differs from host binding")]
    Identity,
    #[error("Matrix recipient verification changed or is unavailable")]
    Recipients,
    #[error("Matrix generation is stale or unavailable")]
    Generation,
    #[error("Matrix authentication refused")]
    Unauthorized,
    #[error("Matrix service returned HTTP {0}")]
    Remote(u16),
    #[error("protected SDK state is unavailable; no keys were reset")]
    Storage,
    #[error("SDK outcome is unknown; retained state requires host inspection")]
    OutcomeUnknown,
    #[error("Matrix durable or observation capacity is exhausted")]
    Capacity,
    #[error("Matrix receipt identity was replayed with different content")]
    Conflict,
    #[error("Matrix event intake retains unsupported or quarantined custody")]
    Unsupported,
    #[error("room snapshot is unsafe and was refused: {0}")]
    UnsafeSnapshot(String),
    /// A store precondition refused the observation, carrying the fixed label
    /// of the rule that refused it. Board #117: the live log repeated a bare
    /// `error=Domain` forever with no rule named, because every unmapped store
    /// variant collapsed into this word and lost its cause.
    #[error("domain refused: {0}")]
    Domain(&'static str),
}
/// The fixed word for a store refusal (ADR-175 pattern: a bounded category,
/// never the store's own error text, which can name host paths).
pub fn store_error_label(error: &hagency_store::Error) -> &'static str {
    use hagency_store::Error::*;
    match error {
        LocalAuthority => "local_authority",
        ResourceSetupRequired => "resource_setup_required",
        RunnerAuthority => "runner_authority",
        AlreadyConsumed => "already_consumed",
        NotConsumable => "not_consumable",
        UnsafeSnapshot(_) => "unsafe_snapshot",
        Quarantined => "quarantined",
        WorkspaceInspectionRequired => "workspace_inspection_required",
        WorkspaceActive => "workspace_active",
        StoppedNotResolvable => "stopped_not_resolvable",
        StoppedReviewExpired => "stopped_review_expired",
        Invalid(_) => "invalid",
        Conflict => "conflict",
        Generation => "generation",
        OwnerGrantReviewChanged => "owner_grant_review_changed",
        OperatorTaskRevisionChanged => "management_task_revision_changed",
        ResourceNameRevisionChanged => "management_resource_name_changed",
        ResourceEditRevisionChanged => "management_resource_edit_changed",
        ResourceConfigurationSourceChanged => "management_resource_source_changed",
        ResourceEditNotAvailable => "management_resource_edit_not_available",
        CeilingAmendmentRefused(_) => "management_ceiling_amendment_refused",
        NotFound => "not_found",
        Unqualified => "unqualified",
        InsufficientCapacity => "insufficient_capacity",
        NoCeiling => "no_ceiling",
        OverCommit { .. } => "over_commit",
        State => "state",
        ManagementChangeCancelled => "management_change_cancelled",
        Locked => "locked",
        Schema => "schema",
        Private => "private",
        PlatformUnavailable => "platform_unavailable",
        Capacity => "capacity",
        Busy => "busy",
        Unavailable => "unavailable",
        OutcomeUnknown => "outcome_unknown",
        Sqlite(_) => "sqlite",
        Io(_) => "io",
        Json(_) => "json",
    }
}
impl From<hagency_store::Error> for Error {
    fn from(e: hagency_store::Error) -> Self {
        match &e {
            hagency_store::Error::Generation => Self::Generation,
            hagency_store::Error::OutcomeUnknown => Self::OutcomeUnknown,
            hagency_store::Error::Capacity => Self::Capacity,
            hagency_store::Error::Busy => Self::Busy,
            hagency_store::Error::Conflict => Self::Conflict,
            // A safety refusal is neither domain authority nor a wire-format
            // failure; it is its own word, carrying the digest-bound reason.
            hagency_store::Error::UnsafeSnapshot(reason) => Self::UnsafeSnapshot(reason.clone()),
            // Every other store refusal names its own rule instead of being
            // erased to a bare word.
            _ => Self::Domain(store_error_label(&e)),
        }
    }
}

#[cfg(test)]
extern crate self as hagency_matrix;

mod approval_batch;

mod presence;
pub use presence::AgentWork;
pub use presence::{
    AGENT_ACK_REACTION, AGENT_TYPING_MAX_MS, AGENT_TYPING_REFRESH_MS, AGENT_TYPING_TIMEOUT_MS,
};
pub use presence::{ack_request, typing_request};

mod approval_intake;
mod approval_markers;
pub use approval_intake::{
    ApprovalCollector, ApprovalCustodyStage, ApprovalCustodyStatus, ApprovalIntakeSummary,
    ApprovalServiceTurn, FleetApprovalAnchor, HostApprovalConfig, HostApprovalPlan,
};

mod approval_delivery;
pub use approval_delivery::{
    ApprovalRoomRecipients, PrivateApprovalDeliveryStage, PrivateApprovalDeliveryState,
    PrivateApprovalDeliveryStatus, PrivateApprovalDeliverySummary,
};
