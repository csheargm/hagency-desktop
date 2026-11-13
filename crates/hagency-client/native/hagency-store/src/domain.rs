//! Allocation and canonical task/dispatch state share this one database owner.
pub mod owner_resources;
pub(crate) mod resource_ceiling;
pub(crate) mod account_ceiling;
mod resource_detail;
use crate::{Error, database};
use hagency_core::{
    InvalidInput, JSON_SAFE_MAX,
    allocation::{self, Budget, Tokens},
    authority::{Registration, VerifiedRequest},
    canonical,
    ceiling::{self as ceiling_wording, SpendContext},
    project::{
        self, CatalogResource, CleanupState, ConfiguredResource, Engagement, EngagementState,
        Resource, Seat,
    },
    qualification,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs::File, path::Path};
pub(crate) mod accounts;
mod management_accounts;
mod management_account_login;
mod management_account_models;
mod activity;
pub use activity::{ActivityEvent, ActivityUpdate};
mod agent_fences;
mod agent_lifecycle;
mod agent_message_leftovers;
mod console_feed;
mod web_catalog;
pub mod coordinator;
pub use agent_fences::{AgentFence, FenceReason};
pub use agent_message_leftovers::{
    DeliveryEventRow, NewOperatorMessage, OperatorMessage, SuppressOutcome, Suppression, Tombstone,
};
mod approvals;
mod engagement_retention;
mod engagement_terms;
pub use approvals::card::PrivateApprovalCard;
mod attachments;
mod attempt_events;
mod catalog_publication;
pub use attachments::AttachmentTicket;
pub use attempt_events::{
    AttemptClock, AttemptClockRow, AttemptEvent, AttemptEventRow, AttemptPhase,
    OVER_BUDGET_NOTICE_KIND, OverBudgetNotice, over_budget_notice_body,
};
pub use catalog_publication::{PublishedCatalog, publication_fingerprint};
mod ceiling_alerts;
pub use ceiling_alerts::{
    ALERT_KIND_CEILING_OVERRUN, ALERT_KIND_OTHER, ALERT_KINDS, ALERT_STATUSES, AlertListFilter, AlertNote, AlertPatch, AlertStats, AlertTransition,
    CeilingAlert, MAX_OPEN_CEILING_ALERTS, SweepOutcome, allowed_transitions,
};
mod command_notices;
mod conversation_lifecycle;
mod conversations;
mod delivery_feedback;
mod directives;
pub use directives::{
    SessionOverrides, THREAD_DIRECTIVE_OPERATOR_REFUSAL, ThreadDirective, ThreadMode, confirmation,
    parse,
};
mod exec_policy;
mod execution;
mod graphs;
mod matrix_routes;
mod messages;
pub use engagement_retention::{ENDED_LIMIT, EngagementPruneOutcome, EngagementRetentionStatus};
pub use engagement_terms::{AgentDefinition, RoleOffer, WhitelistEntry};
pub use execution::{
    EXECUTION_RETENTION_BATCH, EXECUTION_RETENTION_DISPATCHES, EXECUTION_RETENTION_ROWS,
    ExecutionPruneOutcome,
};
pub use messages::{CorpusSweepOutcome, MESSAGE_RETENTION_FLOOR, RetentionStatus};
mod notice_custody;
mod operator_task_views;
mod operator_tasks;
pub use operator_tasks::{
    MAX_TASK_COMMENTS, MAX_TASK_PAGE, OperatorTask, OperatorTaskComment, TASK_GRANULARITIES,
    TASK_PRIORITIES, TASK_STATUSES, TaskFilters, operator_transitions,
};
mod invites;
pub use invites::PendingInvite;
mod offer_book;
pub use offer_book::{Contribution, OfferBook, OfferResource, OfferRole, OfferServing, Preview};
mod outcome_resolution;
mod owned_completion;
mod owned_dispatch;
mod stopped_inspection;
pub use outcome_resolution::{OutcomeAction, OutcomeResolution};
pub(crate) mod joined_rooms;
pub(crate) mod owner_anchors;
mod provision_runtime;
mod quota_holds;
mod reminders;
mod room_trust;
pub use reminders::{Reminder, ReminderReceipt, ReminderSweep};
mod side_registration;
pub use owned_completion::OwnedCompletion;
pub use owned_dispatch::{
    OwnedClaimProfile, OwnedClaimRoom, OwnedDispatchScope, OwnedFailure, OwnedObservation,
};
pub use peers::{
    PEER_RECEIPT_CEILING, PEER_RETENTION_CEILING, PEER_RETENTION_FLOOR, PeerRetentionStatus,
    PeerSweepOutcome,
};
pub use provision_runtime::{OwnedProvisionScope, OwnedRuntimeLease};
pub use side_registration::{IssueSideRegistration, IssueSideRegistrationRequest, SideCredential};
pub(crate) mod file_delivery;
mod peers;
pub(crate) mod received_files;
mod replies;
pub(crate) mod resource_configuration;
pub(crate) mod resource_publication;
mod side_budget;
pub use side_budget::{SideBudget, SideCommitment, UsageTotals};
mod side_lifecycle;
pub use side_lifecycle::{Credential, Representative, SideProjectRecord, SideRecord};
mod task_intents;
pub(crate) mod uploads;
mod usage;
pub use uploads::{UploadAdmission, UploadClaim, UploadIdentity, UploadPreparation, UploadSend};
mod verified_ingress;
pub use delivery_feedback::{
    DeliveryFeedback, DeliveryWarning, DirectTarget, MentionState, MentionTarget,
};
pub use usage::{
    CeilingReport, KnownTokens, MAX_ENGAGEMENT_USAGE_PERIODS, MAX_ENGAGEMENT_USAGE_SOURCES,
    MAX_SOURCE_USAGE_RECEIPTS, MAX_USAGE_PERIODS, MAX_USAGE_RECEIPTS, MAX_USAGE_SOURCES,
    SourceUsage, UsageCeiling, UsageEvidence, UsagePeriod, UsagePeriodKind, UsageReceipt,
    UsageReport, UsageSource, UsageSummary,
};
pub use verified_ingress::StaleMatrixSessionReceipt;

pub struct DomainRepository {
    db: Connection,
    accounts: accounts::Registry,
    _ownership: File,
    approval_owner: std::sync::Arc<()>,
    warm_scopes: std::collections::BTreeMap<String, OwnedProvisionScope>,
    _legacy_writer: Option<hagency_platform::legacy_writer::LegacyWriterAdmission>,
}
/// Current domain schema version (the last sequential migration).
pub const DOMAIN_SCHEMA_VERSION: i32 = 113;
const DOMAIN_MIGRATIONS: &[(i32, &str)] = &[
    (2, include_str!("migrations/002-role-publication.sql")),
    (3, include_str!("migrations/003-task-dispatch.sql")),
    (4, include_str!("migrations/004-message-inputs.sql")),
    (5, include_str!("migrations/005-task-intents.sql")),
    (6, include_str!("migrations/006-internal-conversations.sql")),
    (7, include_str!("migrations/007-peer-inputs.sql")),
    (8, include_str!("migrations/008-recovery-reports.sql")),
    (9, include_str!("migrations/009-conversation-lifecycle.sql")),
    (10, include_str!("migrations/010-task-graphs.sql")),
    (11, include_str!("migrations/011-final-replies.sql")),
    (12, include_str!("migrations/012-verified-ingress.sql")),
    (13, include_str!("migrations/013-owner-approvals.sql")),
    (14, include_str!("migrations/014-notice-custody.sql")),
    (15, include_str!("migrations/015-matrix-transport.sql")),
    (
        16,
        include_str!("migrations/016-owned-task-completions.sql"),
    ),
    (17, include_str!("migrations/017-usage-ledger.sql")),
    (18, include_str!("migrations/018-attachment-visibility.sql")),
    (19, include_str!("migrations/019-file-uploads.sql")),
    (20, include_str!("migrations/020-file-deliveries.sql")),
    (21, include_str!("migrations/021-received-files.sql")),
    (22, include_str!("migrations/022-approval-responses.sql")),
    (23, include_str!("migrations/023-managed-accounts.sql")),
    (24, include_str!("migrations/024-ceiling-alerts.sql")),
    (25, include_str!("migrations/025-alert-transitions.sql")),
    (26, include_str!("migrations/026-corpus-retention.sql")),
    (27, include_str!("migrations/027-peer-corpus-retention.sql")),
    (
        28,
        include_str!("migrations/028-account-login-readiness.sql"),
    ),
    (
        29,
        include_str!("migrations/029-account-logout-receipt.sql"),
    ),
    (30, include_str!("migrations/030-engagement-retention.sql")),
    (31, include_str!("migrations/031-execution-retention.sql")),
    // The number follows landing order: base head 31 + 1.
    // Integration renumbers again if another slice lands first.
    (
        32,
        include_str!("migrations/032-approval-denial-reason.sql"),
    ),
    // PC-C1 landed first and holds 32 (integration 3580f3bb);
    // MA-S2 takes the next free number, 033, by landing order.
    (33, include_str!("migrations/033-dispatch-park-reason.sql")),
    (
        34,
        include_str!("migrations/034-owned-stop-inspections.sql"),
    ),
    (35, include_str!("migrations/035-outcome-resolutions.sql")),
    (36, include_str!("migrations/036-dispatch-discussion.sql")),
    (
        37,
        include_str!("migrations/037-runner-attempt-evidence.sql"),
    ),
    (38, include_str!("migrations/038-agent-fences.sql")),
    (39, include_str!("migrations/039-attempt-over-budget.sql")),
    // Task #13's migration number is 046 (the board's
    // assignment); the walker requires the next sequential
    // list version, so the file keeps 046 and the tuple
    // carries 40. Integration renumbers on merge.
    (40, include_str!("migrations/046-side-registrations.sql")),
    (41, include_str!("migrations/040-command-notices.sql")),
    // Integration of lane/agentctl: its board-assigned number
    // was 049; it lands as the next sequential tuple 42 (file
    // name kept).
    (42, include_str!("migrations/049-agent-lifecycle.sql")),
    // Integration of lane/taskmgmt: its board-assigned number
    // was 044; it lands as the next sequential tuple 43 (file
    // name kept).
    (43, include_str!("migrations/044-operator-tasks.sql")),
    // Integration of lane/offers: its board-assigned number
    // was 043; its 040-042 reserved placeholders are deleted
    // (the board instruction) and the real migration lands as
    // the next sequential tuple 44 (file name kept).
    (
        44,
        include_str!("migrations/043-offers-whitelist-agent-definitions.sql"),
    ),
    // Integration of lane/alerts: its board-assigned number
    // was 045 (already the next sequential tuple; landing
    // order made them coincide).
    (45, include_str!("migrations/045-alert-parity.sql")),
    // Integration of ../nav task/20: its board-assigned number
    // was 061; it lands as the next sequential tuple 46 (file
    // name kept).
    (46, include_str!("migrations/061-side-allocations.sql")),
    // Integration of ../firstres task/27: its board-assigned
    // number was 040; it lands as the next sequential tuple 47
    // (file name kept).
    (
        47,
        include_str!("migrations/040-workspace-dirty-release.sql"),
    ),
    // Integration of lane/verdict: its board-assigned number
    // was 047; its 040-046 walker placeholders are deleted
    // (the board instruction) and the real migration lands as
    // the next sequential tuple 48 (file name kept).
    (
        48,
        include_str!("migrations/047-engagement-verdict-audit.sql"),
    ),
    // Integration of ../regissue task/12: its board-assigned
    // number was 060; it lands as the next sequential tuple 49
    // (file name kept).
    (49, include_str!("migrations/060-pending-invites.sql")),
    // Integration of lane/sidelife: the addition notice named
    // migration 042; the branch file carries 040 — it lands as
    // the next sequential tuple 50 (file name kept).
    (50, include_str!("migrations/040-side-credentials.sql")),
    // Integration of ../provision task/53: its board-assigned
    // number was 066; it lands as the next sequential tuple 51
    // (file name kept).
    (51, include_str!("migrations/066-reminders.sql")),
    // Integration of lane/activity task/1: its board-assigned
    // number was 040; it lands as the next sequential tuple 52
    // (file name kept).
    (52, include_str!("migrations/040-dispatch-activity.sql")),
    // Task #80's migration number is 073 (the board's
    // assignment); the walker requires the next sequential
    // list version, so the file keeps 073 and the tuple
    // carries 53.
    (53, include_str!("migrations/073-room-trust.sql")),
    // Task #73's migration number is 069 (the board's
    // assignment); the walker requires the next sequential
    // list version, so the file keeps 069 and the tuple
    // carries 54.
    (54, include_str!("migrations/069-thread-directives.sql")),
    // Board #49's migration number is 067 (the board's
    // assignment); integrated as the next sequential tuple 55.
    // The file keeps its assigned 067 name.
    (
        55,
        include_str!("migrations/067-agent-message-leftovers.sql"),
    ),
    // Task #61's migration number is 065 (the board's
    // assignment); the walker requires the next sequential
    // list version, so the file keeps 065 and the tuple
    // carries 56.
    (
        56,
        include_str!("migrations/065-final-reply-incidental.sql"),
    ),
    // ADR-186 §A: no board number; the file carries its list
    // version.
    (57, include_str!("migrations/057-engagement-allocation.sql")),
    // ADR-186 §B: no board number; the file carries its list
    // version.
    (58, include_str!("migrations/058-quota-holds.sql")),
    // ADR-187 §C: owner anchors pinned on first use.
    (59, include_str!("migrations/059-owner-anchors.sql")),
    // ADR-188: rooms an agent joined by invitation.
    (60, include_str!("migrations/074-joined-rooms.sql")),
    (
        61,
        include_str!("migrations/075-coordinator-engagements.sql"),
    ),
    (
        62,
        include_str!("migrations/062-coordinator-deliveries.sql"),
    ),
    (
        63,
        include_str!("migrations/063-coordinator-delegations.sql"),
    ),
    (64, include_str!("migrations/064-coordinator-refusals.sql")),
    (
        65,
        include_str!("migrations/065-coordinator-settlements.sql"),
    ),
    (
        66,
        include_str!("migrations/066-coordinator-agent-profiles.sql"),
    ),
    (
        67,
        include_str!("migrations/067-coordinator-project-setup.sql"),
    ),
    (
        68,
        include_str!("migrations/068-coordinator-legacy-adoption.sql"),
    ),
    // ADR-193: no board number; the file carries its list
    // version.
    (69, include_str!("migrations/069-octos-usage-sources.sql")),
    (70, include_str!("migrations/070-management-commands.sql")),
    (71, include_str!("migrations/071-management-intents.sql")),
    (
        72,
        include_str!("migrations/072-owner-resource-registry.sql"),
    ),
    (
        73,
        include_str!("migrations/073-owner-capacity-reservations.sql"),
    ),
    (
        74,
        include_str!("migrations/076-owner-parent-settlements.sql"),
    ),
    (75, include_str!("migrations/077-owner-managed-agents.sql")),
    (76, include_str!("migrations/078-owner-managed-custody.sql")),
    (77, include_str!("migrations/079-owner-managed-fences.sql")),
    (
        78,
        include_str!("migrations/080-owner-resource-adoptions.sql"),
    ),
    (79, include_str!("migrations/081-owner-managed-runtime.sql")),
    (
        80,
        include_str!("migrations/082-owner-shared-execution.sql"),
    ),
    (
        81,
        include_str!("migrations/083-owner-shared-successors.sql"),
    ),
    (
        82,
        include_str!("migrations/084-owner-shared-start-intents.sql"),
    ),
    (
        83,
        include_str!("migrations/085-owner-resource-display-names.sql"),
    ),
    (84, include_str!("migrations/086-owner-operator-tasks.sql")),
    (85, include_str!("migrations/087-owner-shared-demands.sql")),
    (
        86,
        include_str!("migrations/088-owner-shared-stop-intents.sql"),
    ),
    (87,include_str!("migrations/089-owner-shared-context.sql")),
    (88, include_str!("migrations/090-owner-operator-task-transfer.sql")),
    (89, include_str!("migrations/091-owner-managed-budget.sql")),
    (90, include_str!("migrations/092-owner-shared-budget-execution.sql")),
    (91, include_str!("migrations/093-management-account-profiles.sql")),
    (92, include_str!("migrations/094-owner-resource-namespaces.sql")),
    (93, include_str!("migrations/095-owner-resource-provenance.sql")),
    (94, include_str!("migrations/096-approval-binding-incarnations.sql")),
    (95, include_str!("migrations/097-owned-dispatch-execution-policy.sql")),
    (96, include_str!("migrations/098-owner-operator-graph-archives.sql")),
    (97, include_str!("migrations/099-owner-operator-graph-journal.sql")),
    (98, include_str!("migrations/100-owner-operator-graph-recipients.sql")),
    (99, include_str!("migrations/101-owner-web-catalog-settings.sql")),
    (100, include_str!("migrations/102-owner-pool-agents.sql")),
    (101, include_str!("migrations/103-owner-shared-tool-policy.sql")),
    (102, include_str!("migrations/104-owner-unstarted-shared-attempts.sql")),
    (103, include_str!("migrations/105-owner-shared-long-replies.sql")),
    (104, include_str!("migrations/106-owner-capacity-grant-succession.sql")),
    // Board migration number 107 (track C) lands as the next sequential tuple 105.
    (105, include_str!("migrations/107-owner-resource-ceiling-amendments.sql")),
    (106, include_str!("migrations/108-owner-shared-decrypted-inputs.sql")),
    (107, include_str!("migrations/109-owner-agent-periods.sql")),
    (108, include_str!("migrations/110-owner-resource-calendars.sql")),
    (109, include_str!("migrations/111-owner-period-starts.sql")),
    (110, include_str!("migrations/112-owner-unstarted-runtimes.sql")),
    (111, include_str!("migrations/113-owner-shared-native-sessions.sql")),
    (112, include_str!("migrations/114-owner-octos-profiles.sql")),
    (113, include_str!("migrations/115-owner-octos-model-resources.sql")),
];

impl DomainRepository {
    pub(crate) fn management_intent(
        &mut self,
        command: crate::management::JournalCommand,
    ) -> Result<Vec<crate::management::PendingIntent>, Error> {
        crate::management::execute_intent(&mut self.db, command)
    }
    pub(super) fn drop_observed(self, probe: &std::sync::Arc<crate::shutdown::Probe>) {
        use crate::shutdown::{Phase, SqliteCloseScope};
        // Match the declared field drop order. Ownership is still a local
        // guard, so unwinding from connection destruction also releases it.
        let Self {
            db,
            _ownership,
            accounts,
            _legacy_writer,
            ..
        } = self;
        drop(accounts);
        let close_scope = SqliteCloseScope::install(&db, probe);
        probe.observe_domain_writer();
        probe.mark(Phase::ConnectionDropStarted);
        drop(db);
        probe.mark(Phase::ConnectionDropFinished);
        drop(close_scope);
        probe.mark(Phase::OwnershipDropStarted);
        drop(_ownership);
        probe.mark(Phase::OwnershipDropFinished);
        drop(_legacy_writer);
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Pending,
    Started,
    Uncertain,
    Complete,
    Failed,
    Cancelled,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub engagement_id: String,
    pub kind: String,
    pub state: EffectState,
    pub fence: u64,
    /// Adapter-only. Never project this into the operator console or catalog.
    pub payload: Value,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EffectOutcome {
    /// The adapter observed the exact intended effect and supplies a stable receipt.
    Applied {
        receipt: String,
    },
    /// Definitive observation that nothing was applied. A timeout is not this case.
    NotApplied {
        receipt: String,
    },
    Unknown,
}
fn serialize<T: Serialize>(value: &T) -> Result<String, Error> {
    Ok(serde_json::to_string(value)?)
}
fn state_name(state: &EngagementState) -> &'static str {
    match state {
        EngagementState::Pending => "pending",
        EngagementState::Reserved => "reserved",
        EngagementState::Active => "active",
        EngagementState::Rejected => "rejected",
        EngagementState::Revoked => "revoked",
        EngagementState::Failed => "failed",
    }
}

/// The seven named observation columns (ADR-138), shared by the list and
/// single reads so neither can drift from the other. No `SELECT *`, no
/// `description`/`config`/`application`/`observation`, no owner or room.
const APPROVAL_SELECT: &str = "SELECT a.id,a.state,a.choice,a.scope_key IS NOT NULL,a.expires_at,c.engagement_id,p.room_id \
             FROM owner_approvals a \
             JOIN approval_contexts c ON c.id=a.context_id \
             JOIN engagements e ON e.id=c.engagement_id \
             LEFT JOIN projects p ON p.fleet_id=e.fleet_id AND p.id=e.project_id \
             AND p.generation=e.generation";

type ApprovalTuple = (
    String,
    String,
    Option<String>,
    bool,
    i64,
    String,
    Option<String>,
);

fn approval_tuple(r: &rusqlite::Row<'_>) -> rusqlite::Result<ApprovalTuple> {
    Ok((
        r.get::<_, String>(0)?,
        r.get::<_, String>(1)?,
        r.get::<_, Option<String>>(2)?,
        r.get::<_, bool>(3)?,
        r.get::<_, i64>(4)?,
        r.get::<_, String>(5)?,
        r.get::<_, Option<String>>(6)?,
    ))
}

fn approval_row(
    (id, state, choice, reusable_scope, expires_at, engagement_id, project_room_id): ApprovalTuple,
) -> Result<Value, Error> {
    Ok(json!({
        "id": id,
        "state": state,
        "choice": choice
            .map(|c| serde_json::from_str::<String>(&c))
            .transpose()?,
        "reusableScope": reusable_scope,
        "expiresAt": expires_at,
        "engagementId": engagement_id,
        "projectRoomId": project_room_id,
    }))
}
fn read_engagement(db: &Connection, id: &str) -> Result<Engagement, Error> {
    let value: String = db
        .query_row(
            "SELECT projection FROM engagements WHERE id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    Ok(serde_json::from_str(&value)?)
}
fn write_engagement(tx: &Transaction<'_>, value: &Engagement) -> Result<(), Error> {
    tx.execute(
        "UPDATE engagements SET state=?2,projection=?3,allocated_tokens=?4 WHERE id=?1",
        params![
            value.id,
            state_name(&value.state),
            serialize(value)?,
            value.allocated_tokens.map(u64::from)
        ],
    )?;
    Ok(())
}
/// One project of a side, in the project-sides projection (ADR-132):
/// exactly these two keys — `owner_mxid` and `owner_room_id` are
/// deliberately withheld (the owner's DM room is non-public, ADR-112, and
/// the retained `publicSide` serves neither).
#[derive(Debug, Clone, Serialize)]
pub struct SideProject {
    pub id: String,
    pub room_id: String,
}
/// One row of the read-only project-sides projection (ADR-132): the fleet
/// registration read as a side — the id IS the server name (ADR-016) —
/// joined to its projects. Exactly these six keys; no credential key
/// exists at all, and the read extracts only the named config paths
/// (`json_extract`), never a parse-and-strip of the whole config, so a
/// credential-shaped value seeded into the config cannot travel inside an
/// otherwise-allowed key. `registered` compares the row's generation
/// column against the config's own generation field — true by
/// construction today, but computed, so drift is visible rather than
/// assumed away.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectSide {
    pub id: String,
    pub representative: String,
    pub generation: u64,
    pub reception_room_id: String,
    pub registered: bool,
    pub projects: Vec<SideProject>,
}
fn read_resource(db: &Connection, id: &str) -> Result<Resource, Error> {
    let value: String = db
        .query_row("SELECT config FROM resources WHERE id=?1", [id], |r| {
            r.get(0)
        })
        .optional()?
        .ok_or(Error::NotFound)?;
    Ok(serde_json::from_str(&value)?)
}
/// One row of the read-only agent roster (ADR-126, widened by board #22):
/// one row per AGENT — the TS roster is keyed by agent name
/// (`backend-v2.js:11696` serializes every record), so the derivation groups
/// the engagement projections by `agent_name` and every agent the service
/// knows appears, including one whose engagements all ended. Exactly these
/// nine scalar keys — no credential home, workspace path, tmux target or
/// nested object can travel inside one. `online` is REAL worker state: a
/// live dispatch (`leased`/`started`/`parked`) in one of the agent's
/// sessions. `last_seen_ms` is the newest `runner_attempts.created_at` the
/// agent produced — null, never zero, when it never attempted.
#[derive(Debug, Clone, Serialize)]
pub struct AgentRosterRow {
    pub name: String,
    pub framework: String,
    pub role: String,
    pub state: EngagementState,
    pub engagement_id: String,
    pub requested_tokens: u64,
    pub online: bool,
    pub last_seen_ms: Option<u64>,
    pub last_activity_ms: Option<u64>,
    /// The live dispatch's own word, separate from `state` (the engagement
    /// lifecycle word): `running` (started), `waiting_approval` (parked),
    /// `starting` (leased). `None` means native's dispatch record shows no
    /// live dispatch — said as unknown, never guessed as `idle`.
    pub liveness: Option<String>,
    /// Tokens the agent's engagements were observed to consume:
    /// `usage_sources.latest_counts` display volume summed the way the usage
    /// report sums it. `None` when nothing was measured — unknown, not zero.
    pub consumed: Option<u64>,
    /// ADR-186 §B: the engagement holds an open quota hold — its allocation
    /// is used up, the running turn finishes and nothing new is dispatched.
    pub quota_paused: bool,
    /// The operator's durable stop (`agent_lifecycle`, TS's `manualDown`):
    /// the engagement's newest lifecycle row is stopped-and-not-restarted.
    /// Roster-internal — it shapes `liveness`, and is deliberately NOT a
    /// wire key (the console client refuses any key outside its eleven).
    pub manual_down: bool,
}
/// One console engagement row (board #60 item 3). The label the triage list
/// already rendered, plus the figures TS's `/api/engagements`
/// (`backend-v2.js:14964-14975`) carries and native was dropping:
/// `remainingTokens` (what is LEFT on the resource behind the engagement),
/// `ownerBindingRequired` (a pending request with no verified owner binding
/// yet — TS's readiness rule), and the record's own timestamps.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EngagementLabel {
    pub coordinator_managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix_profile: Option<Value>,
    pub id: String,
    pub agent_name: String,
    pub project_name: Option<String>,
    pub role: String,
    pub requested_tokens: u64,
    pub state: EngagementState,
    pub cleanup: CleanupState,
    /// What is LEFT on the resource behind the agent, so the queue can show
    /// over-commitment BEFORE the decision (TS `:14974`
    /// `agentRemainingTokens`). The same `min` of the non-null limits the
    /// admission decision uses. `None` when no ceiling is declared: unknown,
    /// never rendered as a zero allowance.
    pub agent_remaining_tokens: Option<u64>,
    /// TS `:14972`: a PENDING request has no owner yet unless a verified
    /// binding exists for it. Only pending rows can require one; a decided
    /// row's readiness is no longer a question the queue asks.
    pub owner_binding_required: bool,
    /// When the request was observed (`evidence.observed_at_ms`, recorded at
    /// admission) and, for an ended engagement, when it reached its terminal
    /// state (`engagement_ends.ended_at`). Both optional: null is unknown.
    pub created_at_ms: Option<u64>,
    pub ended_at_ms: Option<u64>,
    /// ADR-186 §A4: the tokens the engagement holds — the granted amount,
    /// raised by any top-up, else the request.
    pub allocated_tokens: u64,
    /// ADR-186 §B1/§B4: known fresh spend (input + output + cache writes)
    /// over every period; `None` while unknown — no complete observation —
    /// rendered as unknown, never as zero.
    pub spent_tokens: Option<u64>,
    /// ADR-186 §B2: an open quota hold — "paused: quota".
    pub quota_paused: bool,
}
/// ADR-186 §B: one engagement's quota, as the pause reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaStatus {
    pub allocated_tokens: u64,
    /// Known fresh spend; `None` while unknown, which never pauses.
    pub spent_tokens: Option<u64>,
    pub paused: bool,
}
/// One session (room) of the agent detail read: the room the session's
/// binding names plus its live dispatch state, when one exists. Exactly
/// these four scalar keys — the room id is already what the engagement
/// and sessions reads serve; no binding payload, credential or workspace
/// path travels.
/// One role of the launch runtime profile (board #49, TS
/// `normalizeRuntimeProfileRole`, `backend-v2.js:831-862`): the four fields
/// native can source from the resource. `extraArgs`, `apiBaseUrl` and
/// `apiKey` are TS-only enrichment of the record's own profile object and
/// have no native source, so they are omitted — the same "unknown, never
/// fabricated" rule the roster's `unavailable` list states.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeProfileRole {
    pub framework: String,
    pub provider: Option<String>,
    pub model: String,
    pub reasoning: Option<String>,
}
/// The launch runtime profile (board #49, TS `normalizeRuntimeProfile`,
/// `backend-v2.js:864-876`): `{primary, supervisor}`. The port stores no
/// supervisor profile, so `supervisor` is null — the TS shape when the
/// stored record carries none.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeProfile {
    pub primary: Option<RuntimeProfileRole>,
    pub supervisor: Option<RuntimeProfileRole>,
}
/// One row of the read-only agent detail (board #22, TS `backend-v2.js:12155`
/// `GET /api/agents/:name`): the agent-keyed identity plus the resource it
/// works from, the rooms its sessions bind, its current (live) dispatch and
/// its recent tasks. Scalar keys and two bounded lists of flat objects —
/// no config payload, credential home or workspace path can travel inside.
/// `engagements` counts every engagement the agent ever held, so an agent
/// whose work all ended is still fully described.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDetailRoom {
    pub session_id: String,
    pub room_id: String,
    pub dispatch_state: Option<String>,
    pub dispatch_id: Option<String>,
}
/// The read-only agent detail (board #22, TS `backend-v2.js:12155`
/// `GET /api/agents/:name`): the agent-keyed identity plus the resource it
/// works from, the rooms its sessions bind, its current (live) dispatch and
/// its recent tasks. Scalar keys and two bounded lists of flat objects —
/// no config payload, credential home or workspace path can travel inside.
/// `engagements` counts every engagement the agent ever held, so an agent
/// whose work all ended is still fully described.
#[derive(Debug, Clone, Serialize)]
pub struct AgentDetail {
    pub name: String,
    pub framework: String,
    pub role: String,
    pub state: EngagementState,
    pub engagement_id: String,
    pub requested_tokens: u64,
    pub online: bool,
    pub last_seen_ms: Option<u64>,
    pub resource_id: String,
    pub project_id: String,
    pub engagements: u64,
    pub rooms: Vec<AgentDetailRoom>,
    pub dispatch: Option<AgentDetailRoom>,
    pub tasks: Vec<hagency_core::tasks::Task>,
    /// Board #53: this agent's self-reminders (every engagement the agent name
    /// holds), oldest first. The reminder's `msg` is the agent's own text; no
    /// credential or workspace path travels inside.
    pub reminders: Vec<Reminder>,
}
fn role_available(db: &Connection, role: &str, fleet: Option<&str>) -> Result<bool, Error> {
    qualification::check_role(role)?;
    let publication: Option<bool> = db
        .query_row(
            "SELECT published FROM role_publications WHERE role=?1",
            [role],
            |r| r.get(0),
        )
        .optional()?;
    if publication == Some(false) {
        return Ok(false);
    }
    if !qualification::cross_family(role) {
        return Ok(true);
    }
    let mut query=db.prepare("SELECT json_extract(f.payload,'$.resource') FROM engagements e JOIN registrations r ON r.fleet_id=e.fleet_id JOIN effects f ON f.engagement_id=e.id AND f.kind='provision' WHERE e.state='active' AND e.generation=r.generation AND (?1 IS NULL OR e.fleet_id=?1)")?;
    let rows = query.query_map([fleet], |r| r.get::<_, String>(0))?;
    let mut families = std::collections::BTreeSet::new();
    let need =
        qualification::default_tier(role).ok_or(hagency_core::InvalidInput("unknown role"))?;
    for row in rows {
        let resource: Resource = serde_json::from_str(&row?)?;
        let (tier, family) = qualification::model(&resource.profile());
        if tier.is_some_and(|got| got >= need)
            && let Some(family) = family
        {
            families.insert(family);
        }
        if families.len() >= 2 {
            return Ok(true);
        }
    }
    Ok(false)
}
fn authority(db: &Connection, proof: &VerifiedRequest, now: u64) -> Result<(), Error> {
    proof.check_fresh(now)?;
    let value: String = db
        .query_row(
            "SELECT config FROM registrations WHERE fleet_id=?1",
            [&proof.request().fleet_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    if serde_json::from_str::<Registration>(&value)? != *proof.registration() {
        return Err(Error::Generation);
    }
    Ok(())
}

fn project_authority(db: &Connection, proof: &VerifiedRequest) -> Result<(), Error> {
    let request = proof.request();
    let existing: Option<(u64,String,String,String)> = db.query_row("SELECT generation,room_id,owner_mxid,owner_room_id FROM projects WHERE fleet_id=?1 AND id=?2",
        params![request.fleet_id,request.target_project_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if let Some((generation, room, owner, owner_room)) = existing
        && (generation != proof.registration().generation
            || room != request.target_room_id
            || owner != request.owner_mxid
            || owner_room != request.owner_dm_room_id)
    {
        return Err(Error::Generation);
    }
    Ok(())
}

fn bounded_row(
    db: &Connection,
    table: &'static str,
    key: &str,
    value: &str,
    limit: i64,
) -> Result<(), Error> {
    // Callers supply only literal table/column names. Values remain SQL parameters.
    let exists: bool = db.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {key}=?1)"),
        [value],
        |r| r.get(0),
    )?;
    if !exists {
        let count: i64 =
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
        if count >= limit {
            return Err(Error::Capacity);
        }
    }
    Ok(())
}
fn decision_digest(kind: &str, engagement: &str) -> Result<String, Error> {
    Ok(canonical::digest(&json!([kind, engagement]))?)
}
fn replay_decision(db: &Connection, id: &str, digest: &str) -> Result<Option<Engagement>, Error> {
    project::identifier(id, 128)?;
    let prior: Option<(String, String)> = db
        .query_row(
            "SELECT digest,result FROM decisions WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    prior
        .map(|(old, result)| {
            if old != digest {
                Err(Error::Conflict)
            } else {
                Ok(serde_json::from_str(&result)?)
            }
        })
        .transpose()
}
/// The decision-receipt bound (ADR-095 amendment, retention Slice 3): the
/// newest `DECISION_RETENTION_LIMIT` verdicts by `rowid` replay idempotently;
/// older rows are trimmed in-write, inside the deciding command's own
/// transaction — never a phase, no period, no bootstrap constant.
const DECISION_RETENTION_LIMIT: u64 = 500;
/// Per-command catch-up bound: at most this many rows are removed by one
/// deciding command, so a deeply over-window corpus drains over later writes
/// rather than lengthening a single verdict's writer hold.
const DECISION_PRUNE_BATCH: u64 = 512;
/// The kind word whose decisions are excluded from the bound (ADR-095 §2):
/// `retry_cleanup` mutates before it records, so its replay lookup on a fresh
/// command id finds no row by design, and pruning it would refuse a legitimate
/// first retry. The exclusion is recomputed in Rust over each candidate's
/// stored result, never by a `LIKE` on a digest.
const DECISION_PRUNE_RETRY_KIND: &str = "retry_cleanup";
/// Receipt trim bound (tick contract §3.1), applied by the same writer that
/// inserts a `phase='decisions'` receipt row.
const RETENTION_RECEIPT_LIMIT: u64 = 100;

/// The audit's own facts ride the decisions row itself (board #16, parity
/// lib/engagement-store.js:299-321 `record`): the kind word is the deciding
/// command's own — the call sites pass the retained `engagement.*` word —
/// and the clock is the deciding moment. Rows written before 047 keep NULL
/// and render as unknown.
fn record_decision(
    tx: &Transaction<'_>,
    id: &str,
    digest: &str,
    value: &Engagement,
    audit_word: Option<&str>,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO decisions(id,digest,result,kind,at) VALUES(?1,?2,?3,?4,?5)",
        params![id, digest, serialize(value)?, audit_word, graphs::now_ms()?],
    )?;
    // The in-write trim runs after the insert, in the same transaction, so a
    // rolled-back verdict carries neither the prune nor a receipt.
    trim_decisions(tx)?;
    Ok(())
}

/// Trim `decisions` to the newest `DECISION_RETENTION_LIMIT` rows by `rowid`,
/// oldest first, excluding every `retry_cleanup` decision (recomputed in Rust
/// over the stored result) and never the maximum-`rowid` row. Writes one
/// `retention_prune_receipts` row with `phase='decisions'` when the phase did
/// work (`pruned > 0`) or the corpus still stands over the window
/// (`remaining > 0`), and trims the receipt table to `RETENTION_RECEIPT_LIMIT`
/// in the same step.
fn trim_decisions(tx: &Transaction<'_>) -> Result<(), Error> {
    let max_rowid: Option<i64> = tx
        .query_row("SELECT MAX(rowid) FROM decisions", [], |r| r.get(0))
        .optional()?;
    let Some(max_rowid) = max_rowid else {
        return Ok(());
    };
    // Rows at or below (max - limit) are past the newest-LIMIT window; the
    // subquery keeps the window correct while the batch catches up.
    let threshold = max_rowid.saturating_sub(DECISION_RETENTION_LIMIT as i64);
    if threshold <= 0 {
        return Ok(());
    }
    let started = std::time::Instant::now();
    let candidates: Vec<(String, i64, String, String)> = {
        let mut statement = tx.prepare(
            "SELECT id,rowid,digest,result FROM decisions \
             WHERE rowid <= ?1 AND rowid < ?2 ORDER BY rowid",
        )?;
        let rows = statement
            .query_map(params![threshold, max_rowid], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        rows
    };
    let mut pruned = 0u64;
    let mut oldest: Option<i64> = None;
    let mut newest: Option<i64> = None;
    for (_id, rowid, digest, result) in candidates {
        // Exclude retry_cleanup by recomputing its digest over the stored
        // engagement — exact comparison, never a LIKE on a digest (ADR-095 §2).
        let is_retry = serde_json::from_str::<Engagement>(&result)
            .ok()
            .and_then(|e| decision_digest(DECISION_PRUNE_RETRY_KIND, &e.id).ok())
            .is_some_and(|d| d == digest);
        if is_retry {
            continue;
        }
        if pruned >= DECISION_PRUNE_BATCH {
            break;
        }
        tx.execute("DELETE FROM decisions WHERE rowid = ?1", [rowid])?;
        oldest = Some(oldest.map_or(rowid, |o| o.min(rowid)));
        newest = Some(newest.map_or(rowid, |n| n.max(rowid)));
        pruned += 1;
    }
    let total: i64 = tx.query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get(0))?;
    let remaining = (total as u64).saturating_sub(DECISION_RETENTION_LIMIT);
    if pruned > 0 || remaining > 0 {
        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX / 2);
        tx.execute(
            "INSERT INTO retention_prune_receipts\
             (phase,pruned,oldest_ref,newest_ref,remaining,elapsed_ms,at_ms) \
             VALUES('decisions',?1,?2,?3,?4,?5,?6)",
            params![
                pruned,
                oldest.map(|v| v.to_string()).unwrap_or_default(),
                newest.map(|v| v.to_string()).unwrap_or_default(),
                remaining,
                elapsed_ms,
                graphs::now_ms()?,
            ],
        )?;
        tx.execute(
            "DELETE FROM retention_prune_receipts WHERE sequence NOT IN (\
             SELECT sequence FROM retention_prune_receipts \
             ORDER BY sequence DESC LIMIT ?1)",
            [RETENTION_RECEIPT_LIMIT],
        )?;
    }
    Ok(())
}
fn budget(
    db: &Connection,
    resource: &Resource,
    exclude_engagement_id: Option<&str>,
    for_auto_join: bool,
) -> Result<Budget, Error> {
    budget_at(
        db,
        resource,
        exclude_engagement_id,
        for_auto_join,
        graphs::now_ms()?,
    )
}
/// The budget at native time `at`. An owner grant whose UTC month ended
/// before `at` keeps only its unresolved liability against the monthly
/// ceiling (`owner_resources::additional_commitments`).
fn budget_at(
    db: &Connection,
    resource: &Resource,
    exclude_engagement_id: Option<&str>,
    for_auto_join: bool,
    at: u64,
) -> Result<Budget, Error> {
    budget_with_grant(db, resource, exclude_engagement_id, for_auto_join, None, at)
}
fn budget_with_grant(
    db: &Connection,
    resource: &Resource,
    exclude_engagement_id: Option<&str>,
    for_auto_join: bool,
    within_grant: Option<&str>,
    at: u64,
) -> Result<Budget, Error> {
    budget_with_options(
        db,
        resource,
        exclude_engagement_id,
        for_auto_join,
        within_grant,
        at,
        None,
    )
}
/// `succession`: the capacity chain whose own carried in-flight usage counts
/// as held instead of making the figures unknown (E20 for successors only).
fn budget_with_options(
    db: &Connection,
    resource: &Resource,
    exclude_engagement_id: Option<&str>,
    for_auto_join: bool,
    within_grant: Option<&str>,
    at: u64,
    succession: Option<&std::collections::BTreeSet<String>>,
) -> Result<Budget, Error> {
    let declaration = account_ceiling::declaration(db, &resource.seat_id)?;
    let account_calendars_match = resource_ceiling::shared_account_calendars_match(
        db,
        &resource.seat_id,
        declaration.as_ref(),
        None,
    )?;
    // Aggregate in SQLite, not by cloning or scanning the whole lifetime store in Rust.
    // SQLite SUM fails rather than wrapping; Tokens also enforces JSON-safe precision.
    // ADR-186 §A4: an engagement holds its granted amount when one is set.
    let mut statement = db.prepare("SELECT preset_id,seat_id,SUM(COALESCE(allocated_tokens,tokens)) FROM engagements WHERE state IN ('reserved','active') AND (preset_id=?1 OR seat_id=?2) GROUP BY preset_id,seat_id")?;
    let rows = statement.query_map(params![resource.preset_id, resource.seat_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, u64>(2)?,
        ))
    })?;
    let mut commitments = Vec::new();
    for row in rows {
        let (preset, seat, tokens) = row?;
        commitments.push(allocation::Commitment {
            id: format!("{}", commitments.len()),
            preset_id: Some(preset),
            seat_id: Some(seat),
            allocated_tokens: Some(tokens.try_into()?),
            state: "active".into(),
            fulfillment: None,
        });
    }
    commitments.extend(coordinator::additional_commitments(
        db,
        resource,
        within_grant,
    )?);
    commitments.extend(owner_resources::additional_commitments(db, resource, at)?);
    let mut result = allocation::resource_budget(&allocation::Input {
        preset: allocation::Preset {
            id: resource.preset_id.clone(),
            ceiling: resource_ceiling::effective_ceiling(db, resource)?,
        },
        seat_id: resource.seat_id.clone(),
        declaration,
        commitments,
        exclude_engagement_id: exclude_engagement_id.map(str::to_owned),
        for_auto_join,
    })?;
    if !account_calendars_match {
        result.seat.status = allocation::SeatStatus::PeriodMismatch;
        result.seat.remaining = None;
        result.remaining_tokens = None;
    }
    if unsettled_for(db, resource, succession)? {
        result.remaining_tokens = None;
        result.pool.remaining = None;
        result.seat.remaining = None;
    }
    Ok(result)
}
fn unsettled_for(
    db: &Connection,
    resource: &Resource,
    succession: Option<&std::collections::BTreeSet<String>>,
) -> Result<bool, Error> {
    match succession {
        None => owner_resources::unsettled_resource(db, resource),
        Some(chain) => owner_resources::unsettled_resource_except(db, resource, chain),
    }
}

/// The headroom an approval or a top-up is checked against (ADR-186 §A2):
/// the drawn ceiling (`drawn = max(reserved, spent)`, unknown spend falling
/// back to the commitment figure, saturating at zero) beside the declared
/// seat and the resource pool, the smallest non-null figure binding.
/// Commitments are counted as they stand, so an engagement that already holds
/// tokens is inside them: its own allocation is not headroom.
struct Headroom {
    report: CeilingReport,
    by_ceiling: Option<u64>,
    /// `None` when no limit is declared or the seat's period mismatches.
    remaining: Option<u64>,
    period_mismatch: bool,
}
fn headroom(db: &Connection, resource: &Resource, at: u64) -> Result<Headroom, Error> {
    headroom_with_grant(db, resource, at, None)
}
fn headroom_with_grant(
    db: &Connection,
    resource: &Resource,
    at: u64,
    within_grant: Option<&str>,
) -> Result<Headroom, Error> {
    headroom_with_options(db, resource, at, within_grant, None)
}
/// The headroom a capacity grant successor is checked against at `at`: the
/// same figure as an approval's, except that in-flight usage of the
/// successor's own chain counts as held (E20). `None` when it is unknown.
pub(crate) fn successor_remaining(
    db: &Connection,
    resource: &Resource,
    at: u64,
    chain: &std::collections::BTreeSet<String>,
) -> Result<Option<u64>, Error> {
    Ok(headroom_with_options(db, resource, at, None, Some(chain))?.remaining)
}
fn headroom_with_options(
    db: &Connection,
    resource: &Resource,
    at: u64,
    within_grant: Option<&str>,
    succession: Option<&std::collections::BTreeSet<String>>,
) -> Result<Headroom, Error> {
    let report = usage::ceiling_report(db, &resource.id(), at)?;
    let spent_budget =
        budget_with_options(db, resource, None, false, within_grant, at, succession)?;
    // backend-v2.js:14057: a seat declaration whose period mismatches the
    // pool's nulls the whole figure rather than falling back to the pool.
    let period_mismatch = spent_budget.seat.status == allocation::SeatStatus::PeriodMismatch;
    let credit = within_grant
        .map(|id| coordinator::unused_grant(db, id, resource))
        .transpose()?
        .unwrap_or(0);
    let effective_draw = report
        .reserved
        .saturating_sub(credit)
        .max(report.spent.unwrap_or(0));
    let unsettled = unsettled_for(db, resource, succession)?;
    let by_ceiling = report
        .ceiling_tokens
        .map(|c| c.saturating_sub(effective_draw))
        .filter(|_| !unsettled);
    let remaining = [
        by_ceiling,
        spent_budget.seat.remaining.map(u64::from),
        spent_budget.pool.remaining.map(u64::from),
    ]
    .into_iter()
    .flatten()
    .min()
    .filter(|_| !period_mismatch && !unsettled);
    Ok(Headroom {
        report,
        by_ceiling,
        remaining,
        period_mismatch,
    })
}
/// Refuse `granted` tokens the resource cannot give (ADR-186 §A2), with the
/// retained refusal identities: `over_commit` carrying the human message
/// when the ceiling binds, `insufficient_capacity` when the seat or pool
/// does, `no_ceiling` when nothing is declared.
fn check_grant(
    tx: &Connection,
    resource: &Resource,
    agent: &str,
    granted: u64,
    now: u64,
) -> Result<(), Error> {
    check_grant_within(tx, resource, agent, granted, now, None)
}

fn check_grant_within(
    tx: &Connection,
    resource: &Resource,
    agent: &str,
    granted: u64,
    now: u64,
    within_grant: Option<&str>,
) -> Result<(), Error> {
    // Admission uses the drawn ceiling (backend-v2.js:14036-14060), and
    // approve is the operator verdict path, so `for_auto_join` is false —
    // auto-join is the other remainingFor caller, not this one.
    let Headroom {
        report,
        by_ceiling,
        remaining,
        period_mismatch,
    } = headroom_with_grant(tx, resource, now, within_grant)?;
    if period_mismatch {
        return Err(Error::NoCeiling);
    }
    let remaining = remaining.ok_or(Error::NoCeiling)?;
    if remaining >= granted {
        return Ok(());
    }
    if by_ceiling.is_some_and(|b| b < granted) {
        // The ceiling side is binding: the refusal names both draws,
        // the binding one, the measurement's period key and the
        // cache-read discrepancy, exactly as the JavaScript does.
        let period_name = match report.period {
            UsagePeriodKind::Daily => "daily",
            UsagePeriodKind::Monthly => "monthly",
            UsagePeriodKind::Weekly => "weekly",
        };
        let message = ceiling_wording::over_commit_message(
            agent,
            granted,
            remaining,
            Some(&SpendContext {
                period: Some(period_name.into()),
                reserved: Some(report.reserved),
                spent: report.spent,
                consumed: report.consumed,
                ceiling_tokens: report.ceiling_tokens,
                preset_name: Some(report.preset_name),
                spend_period_key: report.spend_period_key,
            }),
        );
        return Err(Error::OverCommit { message });
    }
    // The declared shared seat is the binding side: the resource-pool
    // refusal keeps its pre-existing identity and shape.
    Err(Error::InsufficientCapacity)
}

impl DomainRepository {
    pub fn set_role_publication(&mut self, role: &str, published: bool) -> Result<(), Error> {
        qualification::check_role(role)?;
        self.db.execute("INSERT INTO role_publications(role,published) VALUES(?1,?2) ON CONFLICT(role) DO UPDATE SET published=excluded.published",params![role,published])?;
        Ok(())
    }
    pub fn role_publications(&self) -> Result<Vec<Value>, Error> {
        let mut query = self
            .db
            .prepare("SELECT config FROM resources WHERE json_extract(config,'$.published')=1")?;
        let resources: Vec<Resource> = query
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_, Error>>()?;
        qualification::roles().map(|role| {
            let explicit:Option<bool>=self.db.query_row("SELECT published FROM role_publications WHERE role=?1",[role],|r|r.get(0)).optional()?;
            let available=role_available(&self.db,role,None)? && resources.iter().any(|r|r.qualifies(role));
            // G5 (ADR-108 amendment): three derived keys over the SAME set
            // `available` samples — qualifying(role) = published resources
            // passing Resource::qualifies (provisionable ∧ ceiling ∧ policy
            // tier). `qualification::resources_for_role` is deliberately
            // NOT used: it omits published and provisionable, so a resource
            // could count toward fillable and not toward available — two
            // answers for one question. `families` is the MODEL family
            // (model().1), never the framework, matching the retained
            // catalogue's cross-family count (derive.js:91,100); sorted for
            // a stable wire. A model matching no policy tier yields
            // (None,None) and cannot qualify at all, so it contributes
            // nothing — unknown tier never reads as stronger.
            let qualifying: Vec<&Resource> = resources.iter().filter(|r| r.qualifies(role)).collect();
            let families: std::collections::BTreeSet<&str> =
                qualifying.iter().filter_map(|r| qualification::model(&r.profile()).1).collect();
            let fillable = qualifying.len();
            let default = qualification::default_tier(role);
            let over_tier = default.map_or(0, |needed| {
                qualifying
                    .iter()
                    .filter(|r| {
                        qualification::model(&r.profile())
                            .0
                            .is_some_and(|tier| tier > needed)
                    })
                    .count()
            });
            Ok(json!({"role":role,"explicitPublication":explicit,"available":available,"crossFamily":qualification::cross_family(role),"defaultTier":qualification::default_tier(role),"families":families.into_iter().collect::<Vec<&str>>(),"fillable":fillable,"overTier":over_tier}))
        }).collect()
    }
    pub fn open(directory: &Path) -> Result<Self, Error> {
        Self::open_mode(directory, true, None, None)
    }
    /// Bind a fresh embedded ledger to the authenticated server/subject/owner
    /// profile. Validate copied data before recovery can alter execution state.
    /// Populated unscoped data is refused; current profile setup never imports it.
    pub fn open_managed(directory: &Path, profile: &str) -> Result<Self, Error> {
        if profile.len() != 64
            || !profile
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::LocalAuthority);
        }
        Self::open_mode(directory, true, Some(profile), None)
    }
    fn open_canonical(
        directory: &Path,
        authority: owner_resources::CanonicalStoreAuthority,
    ) -> Result<Self, Error> {
        Self::open_mode(directory, false, None, Some(authority))
    }
    fn open_mode(
        directory: &Path,
        recover: bool,
        profile: Option<&str>,
        canonical: Option<owner_resources::CanonicalStoreAuthority>,
    ) -> Result<Self, Error> {
        #[cfg(test)]
        hagency_platform::legacy_writer::test_support::initialize_process_root()?;
        let legacy_writer = if let Some(canonical) = canonical {
            canonical.check(directory)?;
            None
        } else {
            Some(
                hagency_platform::legacy_writer::LegacyWriterAdmission::acquire()?,
            )
        };
        let mut database = database::open(
            directory,
            database::Schema {
                name: "domain.sqlite3",
                lock: "domain.lock",
                application_id: 0x48414732,
                version: DOMAIN_SCHEMA_VERSION,
                migrations: DOMAIN_MIGRATIONS,
                sql: include_str!("domain.sql"),
                verify: &[
                    "SELECT singleton,profile_digest FROM management_identity LIMIT 0",
                    "SELECT singleton,id,root_identity,revision FROM owner_resource_identity LIMIT 0",
                    "SELECT provider_digest,seat_id FROM owner_resource_accounts LIMIT 0",
                    "SELECT profile_digest,model,resource_id,configuration_digest FROM owner_octos_model_resources LIMIT 0",
                    "SELECT profile,resource_id,revision,active FROM owner_resource_grants LIMIT 0",
                    "SELECT command_id,digest,result FROM owner_resource_receipts LIMIT 0",
                    "SELECT resource_id,registry_id,creation_command_id,creation_digest,creation_profile,first_offer_kind,first_offer_profile,first_offer_command_id,first_offer_revision,first_offer_at_ms FROM owner_resource_provenance LIMIT 0",
                    "SELECT resource_id,sequence,profile,command_id,base_revision,previous_tokens,tokens,period,created_at_ms FROM owner_resource_ceiling_amendments LIMIT 0",
                    "SELECT id,profile,resource_id,preset_id,seat_id,allocated_tokens,receipt,active FROM owner_capacity_grants LIMIT 0",
                    "SELECT id,grant_id,project_id,project_revision,reserve_command_id,reserve_digest,requested_tokens,retained_tokens,state,binding_digest FROM owner_project_reservations LIMIT 0",
                    "SELECT command_id,profile,executor_device_id,digest,result FROM owner_capacity_receipts LIMIT 0",
                    "SELECT accounting_mode FROM owner_capacity_grants LIMIT 0",
                    "SELECT accounting_mode FROM owner_project_reservations LIMIT 0",
                    "SELECT grant_id,fence_revision,retained_tokens,receipt,receipt_digest,manifest FROM owner_parent_settlements LIMIT 0",
                    "SELECT allocation_id,reserve_command,reserve_digest,manager_mxid,definition_digest FROM owner_project_authorities LIMIT 0",
                    "SELECT id,profile,agent_id,request_id,project_reservation_id,original_command_id,original_command_digest,original_command,definition_digest,allocated_tokens,budget_revision,state,permanent_fence FROM owner_managed_agents LIMIT 0",
                    "SELECT id,child_id,kind,state,fence,payload_digest,original_result,original_result_digest FROM owner_managed_effects LIMIT 0",
                    "SELECT id,child_id,execution_id,immutable_digest,assignment_generation,state,reserved_tokens,known_consumed_tokens,usage_complete,unrepresentable_usage,original_result_digest FROM owner_managed_calls LIMIT 0",
                    "SELECT child_id,assignment_id,assignment_generation,binding_digest,provider_digest FROM owner_managed_assignment_pins LIMIT 0",
                    "SELECT call_id,assignment_digest,execution_digest,prepare_revision,issue_revision FROM owner_managed_call_pins LIMIT 0",
                    "SELECT id,child_id,target_id,kind,content_digest,evidence,result,ledger_revision FROM owner_managed_observations LIMIT 0",
                    "SELECT profile,agent_id,request_id,original_command_id,original_command_digest,original_command,project_reservation_id,executor_device_id,fence_revision FROM owner_managed_fences LIMIT 0",
                    "SELECT id,profile,plan_digest,plan,phase,source_evidence,custody_evidence,result FROM owner_resource_adoptions LIMIT 0",
                    "SELECT child_id,profile,assignment_id,command_id,command_payload,command_digest,binding_digest,provider_digest,issued_revision,incarnation,source_effect_state FROM owner_managed_runtime_lifetimes LIMIT 0",
                    "SELECT child_id,profile,assignment_id,command_id,command_payload,command_digest,binding_digest,provider_digest,issued_revision,incarnation FROM owner_shared_runtime_lifetimes LIMIT 0",
                    "SELECT id,child_id,payload,digest,ledger_revision FROM owner_shared_process_observations LIMIT 0",
                    "SELECT id,child_id,execution_id,payload,digest,received_revision FROM owner_shared_work_inbox LIMIT 0",
                    "SELECT id,work_id,kind,payload,digest,ledger_revision FROM owner_shared_work_events LIMIT 0",
                    "SELECT id,work_id,payload,digest,ledger_revision FROM owner_shared_work_observations LIMIT 0",
                    "SELECT id,child_id,requested_epoch,requested_revision FROM owner_shared_activity_renewals LIMIT 0",
                    "SELECT id,child_id,profile,process_attempt,predecessor_id,predecessor_digest,predecessor_stop_digest,start_intent_id,command_payload,command_digest,original_runtime_digest,binding_digest,provider_digest,observed_revision,issued_revision,incarnation FROM owner_shared_successor_attempts LIMIT 0",
                    "SELECT id,attempt_id,payload,digest,ledger_revision FROM owner_shared_successor_observations LIMIT 0",
                    "SELECT call_id,attempt_id,work_id,execution_digest FROM owner_shared_successor_call_pins LIMIT 0",
                    "SELECT command_id,child_id,profile,process_attempt,predecessor_id,start_intent_id,command_payload,command_digest,fence_revision,claimed_revision FROM owner_shared_successor_fences LIMIT 0",
                    "SELECT id,child_id,profile,device_id,device_generation,predecessor_digest,original_runtime_digest,original_runtime_payload,predecessor_payload,input_payload,input_digest,ledger_revision FROM owner_shared_start_intents LIMIT 0",
                    "SELECT command_id,child_id,profile,device_id,device_generation,actor_mxid,assignment_id,selected_revision,input_payload,input_digest,ledger_revision FROM owner_shared_stop_intents LIMIT 0",
                    "SELECT command_id,applied_revision,ledger_revision FROM owner_shared_stop_receipts LIMIT 0",
                    "SELECT command_id,child_id,profile,executor_device_id,top_up_id,command_digest,command_payload,receipt_payload,receipt_digest,ledger_revision FROM owner_managed_budget_commands LIMIT 0",
                    "SELECT command_id,child_id,budget_revision,previous_digest,budget_digest,applied_tokens,effective_ceiling_tokens,ledger_revision FROM owner_managed_budget_deltas LIMIT 0",
                    "SELECT migration_id,task_id,payload_digest,payload FROM owner_operator_task_transfer_tasks LIMIT 0",
                    "SELECT migration_id,sequence,task_id,ordinal,payload_digest,payload FROM owner_operator_task_transfer_comments LIMIT 0",
                    "SELECT work_id,mode,ledger_revision FROM owner_shared_input_modes LIMIT 0",
                    "SELECT work_id,assignment_id,room_id,thread_root,ordinal,payload,digest,input_digest,ledger_revision FROM owner_shared_contexts LIMIT 0",
                    "SELECT work_id,payload,digest,ledger_revision FROM owner_shared_reply_deliveries LIMIT 0",
                    "SELECT call_id,work_id,context_digest,input_digest,ledger_revision FROM owner_shared_context_call_pins LIMIT 0",
                    "SELECT child_id,profile,control_revision,stopped_through FROM owner_shared_demand_controls LIMIT 0",
                    "SELECT id,child_id,profile,device_id,device_generation,event_id,offer_payload,offer_digest,ledger_revision FROM owner_shared_demands LIMIT 0",
                    "SELECT demand_id,disposition,start_intent_id,input_payload,input_digest,budget_payload,ledger_revision FROM owner_shared_demand_decisions LIMIT 0",
                    "SELECT demand_id,work_id,binding_payload,binding_digest,ledger_revision FROM owner_shared_demand_bindings LIMIT 0",
                    "SELECT child_id,command_id,command_payload,command_digest,fence_revision FROM owner_managed_runtime_stops LIMIT 0",
                    "SELECT id,child_id,command_id,payload,digest,ledger_revision FROM owner_managed_runtime_observations LIMIT 0",
                    "SELECT actor_scope,command_id,operation,digest,result FROM management_commands LIMIT 0",
                    "SELECT actor_scope,command_id,operation,digest,input,reply,state FROM management_intents LIMIT 0",
                    "SELECT id,engagement_id,digest,receipt,accepted_at FROM coordinator_migrations LIMIT 0",
                    "SELECT account_id,profile FROM management_account_profiles LIMIT 0",
                    "SELECT resource_id,account_id,generation,profile FROM owner_resource_namespaces LIMIT 0",
                    "SELECT command_id,profile,input_digest,resource_id FROM owner_namespace_enrollments LIMIT 0",
                    "SELECT id,engagement_id,project_id,digest,command,result FROM coordinator_project_setup_attempts LIMIT 0",
                    "SELECT engagement_id,project_id,attempt_id,observation FROM coordinator_project_setup LIMIT 0",
                    "SELECT id,digest,command,definition,state,reason,agent_id,received_at,updated_at FROM coordinator_deliveries LIMIT 0",
                    "SELECT engagement_id,revision,digest,change,authority,accepted_at FROM coordinator_delegations LIMIT 0",
                    "SELECT id,engagement_id,digest,receipt,refused_at FROM coordinator_refusals LIMIT 0",
                    "SELECT agent_id,command_id,digest,receipt,accepted_at FROM coordinator_settlements LIMIT 0",
                    "SELECT agent_id,command_id,desired_name,confirmed_name,last_error,updated_at,observed_at FROM coordinator_agent_profiles LIMIT 0",
                    "SELECT allocated_tokens FROM engagements LIMIT 0",
                    "SELECT id,engagement_id,dispatch_id,spend,allocation,began_at,lifted_at,lifted_allocation FROM quota_holds LIMIT 0",
                    "SELECT owner_mxid,master_key,source,pinned_at,mismatch_key,mismatch_at FROM owner_anchors LIMIT 0",
                    "SELECT engagement_id,room_id,state,joined_at,updated_at,notice_at FROM joined_rooms LIMIT 0",
                    "SELECT fleet_id,allocated_tokens,updated_at FROM side_allocations LIMIT 0",
                    "SELECT engagement_id,stopped_at,reason,operator,started_at FROM agent_lifecycle LIMIT 0",
                    "SELECT server_name,label,api_base_url,credential,pending_credential,pending_issued_at,representative,access_state,access_detail,access_checked_at,access_issued_at,allocated_tokens,active,created_at,updated_at FROM side_records LIMIT 0",
                    "SELECT server_name,id,name,room_id,note,archived,archived_at,created_at,updated_at FROM side_projects LIMIT 0",
                    "SELECT id,engagement_id,dispatch_id,fence,reason,created_at,cleared_at,cleared_by FROM agent_fences LIMIT 0",
                    "SELECT dispatch_id,phase,kind,tools,finished,started_at,updated_at,queued_at,revision,anchor FROM dispatch_activity LIMIT 0",
                    "SELECT dispatch_id,event_key FROM dispatch_activity_events LIMIT 0",
                    "SELECT id,session_id,transaction_id,digest,body,html,route,source_event_id,state,cancel_requested,fence,claim_hash,claim_until,event_id,observation,created_at,updated_at FROM command_notices LIMIT 0",
                    "SELECT id FROM current_command_notices LIMIT 0",
                    "SELECT id,dirty FROM workspace_resources LIMIT 0",
                    "SELECT resource_id,inspected_at FROM workspace_dirty_releases LIMIT 0",
                    "SELECT engagement_id,yolo,updated_at FROM agent_execution_policies LIMIT 0",
                    "SELECT room_id,agent,inviter,project_server,mode,since_ts,state,join_pending,leave_pending,seen_at,decided_at,decided_by FROM pending_invites LIMIT 0",
                    "SELECT id,engagement_id,session_id,msg,created_at,fire_at,fired_at FROM reminders LIMIT 0",
                    "SELECT dispatch_id,fence,seq,at_ms,phase,detail FROM runner_attempt_events LIMIT 0",
                    "SELECT dispatch_id,fence,started_at,parked_at,last_renew_at,settled_at,terminal_reason FROM runner_attempts LIMIT 0",
                    "SELECT dispatch_id,message_sequence,addressed FROM dispatch_inputs LIMIT 0",
                    "SELECT dispatch_id,read_parts FROM dispatch_conversation_reads LIMIT 0",
                    "SELECT id,dispatch_id,fence,receipt_digest,snapshot_digest,token_hash,created_at,expires_at,consumed_at FROM outcome_inspections LIMIT 0",
                    "SELECT request_id,dispatch_id,inspection_id,request_digest,action,response,resolved_at FROM outcome_resolutions LIMIT 0",
                    "SELECT dispatch_id,fence,digest,config,observed_at FROM owned_stop_inspections LIMIT 0",
                    "SELECT sequence,engagement_id,source_key,scope_digest,digest,config,source_session_id,wake,pruned_at_ms FROM retained_message_archive LIMIT 0",
                    "SELECT source_key,digest,sequence,pruned_at_ms FROM retained_peer_index LIMIT 0",
                    "SELECT sequence,phase,pruned,oldest_ref,newest_ref,remaining,elapsed_ms,at_ms,payload FROM retention_prune_receipts LIMIT 0",
                    "SELECT engagement_id,ended_at FROM engagement_ends LIMIT 0",
                    "SELECT id,account_id,account_generation,attempt,observed_at_ms,expires_at_ms,mode,provider_state,outcome FROM account_login_observations LIMIT 0",
                    "SELECT account_id,attempt,started_at_ms,deadline_ms,state,receipt_id FROM account_login_attempts LIMIT 0",
                    "SELECT id,account_id,retired_at_ms,readiness,logout_detail FROM account_logout_receipts LIMIT 0",
                    "SELECT dedupe_key,resource_id,summary,detail,runbook,impact,recovery_condition,occurrences,first_seen_ms,last_seen_ms,resolved_at_ms,resolved_by,status,note,transitioned_at_ms,transitioned_by,alert_type,severity,source,source_agent,assignee,suppress_until_ms,linked_task_id,original_severity,missing_actionable_fields,owner,tags FROM ceiling_alerts LIMIT 0",
                    "SELECT dedupe_key,seq,author,text,ts_ms FROM ceiling_alert_notes LIMIT 0",
                    "SELECT id,title,description,status,priority,granularity,assignee,created_by,created_at,updated_at,started_at,completed_at,heartbeat_at,waiting_reason,waiting_until,parent_id,labels FROM operator_tasks LIMIT 0",
                    "SELECT sequence,task_id,author,body,created_at FROM operator_task_comments LIMIT 0",
                    "SELECT k.secret,k.deployment,k.root_identity,a.id,a.ordinal,a.generation,a.state,a.namespace_identity,a.identity_tuple,a.seat_id,r.preset_id,r.account_id,r.binding_generation FROM account_identity_key k CROSS JOIN managed_accounts a CROSS JOIN resource_accounts r LIMIT 0",
                    "SELECT request_id,context_id,capability_digest,decision_digest,state,write_accepted,authorized_at,response_started_at FROM approval_responses LIMIT 0",
                    "SELECT id,capability_digest,event_id,workspace_id,binding,binding_digest,byte_limit,facts,state,failure FROM received_files LIMIT 0",
                    "SELECT id,upload_id,dispatch_id,call_id,request,request_hash,captured,event_state,claim_fence,claim_hash,claim_until,transaction_id,publication,cancel_requested,failure,acceptance,created_at,updated_at FROM file_deliveries LIMIT 0",
                    "SELECT id,dispatch_id,call_id,request_digest,capability_digest,scope_fingerprint,route,preparation_hash,stage,stage_state,upload_state,claim_fence,claim_hash,claim_until,cancel_requested,outcome_unknown,acceptance,created_at,updated_at FROM file_uploads LIMIT 0",
                    "SELECT a.digest,a.content_digest,a.metadata,a.sdk_identity,a.manifest_id,v.projection_sequence,w.source_cutoff,w.projection_cutoff FROM matrix_attachments a CROSS JOIN session_attachment_visibility v CROSS JOIN dispatch_attachment_windows w LIMIT 0",
                    "SELECT id,dispatch_id,fence,engagement_id,identity_digest,framework,attribution,high_water,latest_counts,latest_observation,latest_incomplete,latest_regressed,historical_incomplete,regressions,observations,observed_at FROM usage_sources LIMIT 0",
                    "SELECT source_id,call_id,digest,observation,response FROM usage_receipts LIMIT 0",
                    "SELECT engagement_id,granularity,period_key,observed_growth,known_growth,incomplete,observations FROM usage_periods LIMIT 0",
                    "SELECT singleton,observed_at FROM usage_clock LIMIT 0",
                    "SELECT id,fingerprint,deadline,reply_id FROM owned_task_completions LIMIT 0",
                    "SELECT available,invalidation FROM matrix_transports LIMIT 0",
                    "SELECT n.send_fence,n.cancel_requested,n.task_epoch,n.source_event_id,i.digest,i.observation FROM task_notices n CROSS JOIN notice_send_inspections i LIMIT 0",
                    "SELECT r.available,r.config,b.incarnation,c.digest,a.state,g.context_key,v.digest FROM approval_rooms r CROSS JOIN approval_bindings b CROSS JOIN approval_contexts c CROSS JOIN owner_approvals a CROSS JOIN approval_grants g CROSS JOIN approval_verdict_receipts v LIMIT 0",
                    "SELECT engagement_id FROM current_approval_bindings LIMIT 0",
                    "SELECT engagement_id,incarnation FROM approval_binding_incarnations LIMIT 0",
                    "SELECT dispatch_id,engagement_id,yolo,captured_at FROM owned_dispatch_execution_policies LIMIT 0",
                    "SELECT a.command_id,a.registry_id,a.profile,a.owner_mxid,a.source_binding,a.manifest,a.state,c.document,c.chunk_index,c.bytes FROM owner_operator_graph_archives a LEFT JOIN owner_operator_graph_archive_chunks c ON c.command_id=a.command_id LIMIT 0",
                    "SELECT registry_id,profile,owner_mxid,scope_id,graph_id,revision,document,document_digest FROM owner_operator_graphs LIMIT 0",
                    "SELECT profile,command_id,registry_id,owner_mxid,scope_id,graph_id,expected_revision,input_digest,request_digest,receipt FROM owner_operator_graph_commands LIMIT 0",
                    "SELECT registry_id,profile,owner_mxid,scope_id,graph_id,node_id,message_id,payload,payload_digest,state FROM owner_operator_graph_outbox LIMIT 0",
                    "SELECT registry_id,profile,owner_mxid,scope_id,graph_id,node_id,revision,snapshot,snapshot_digest FROM owner_operator_graph_recipients LIMIT 0",
                    "SELECT e.id,e.context,e.evidence,e.projection,f.payload,p.owner_mxid,r.config,s.config,d.result,g.config,rp.role,rt.config,rd.capability_hash,ro.task,mi.digest,si.wake,di.message_sequence,ti.anchor_event_id,tn.delivery,tf.dispatch_id,tin.message_sequence,tir.digest,ic.digest,ip.session_id,pm.digest,psi.wake,pdi.message_sequence,lpi.session_id,tdpr.dispatch_id,drr.task_id,crr.execution_epoch,co.digest,ds.fence,ud.id,ic.revision FROM engagements e LEFT JOIN effects f ON f.engagement_id=e.id CROSS JOIN projects p CROSS JOIN resources r CROSS JOIN seats s CROSS JOIN decisions d CROSS JOIN registrations g CROSS JOIN role_publications rp CROSS JOIN canonical_tasks rt CROSS JOIN runner_dispatches rd CROSS JOIN task_outbox ro CROSS JOIN admitted_messages mi CROSS JOIN session_inputs si CROSS JOIN dispatch_inputs di CROSS JOIN task_intents ti CROSS JOIN task_notices tn CROSS JOIN task_followup_ready tf CROSS JOIN task_inputs tin CROSS JOIN task_input_receipts tir CROSS JOIN internal_conversations ic CROSS JOIN internal_participants ip CROSS JOIN peer_messages pm CROSS JOIN peer_session_inputs psi CROSS JOIN peer_dispatch_inputs pdi CROSS JOIN live_peer_inputs lpi CROSS JOIN task_dispatch_input_ready tdpr CROSS JOIN dispatch_recovery_reports drr CROSS JOIN current_recovery_reports crr CROSS JOIN conversation_operations co CROSS JOIN dispatch_stops ds CROSS JOIN unresolved_dispatches ud CROSS JOIN runner_sessions sc INDEXED BY canonical_runner_session LIMIT 0",
                    "SELECT tg.config,gn.state,gn.result_value,gc.digest,gd.digest FROM task_graphs tg CROSS JOIN graph_nodes gn CROSS JOIN graph_commands gc CROSS JOIN graph_dependencies gd LIMIT 0",
                    "SELECT id FROM current_graph_scopes LIMIT 0",
                    "SELECT dispatch_id FROM graph_dispatch_ready LIMIT 0",
                    "SELECT dispatch_id FROM graph_dispatch_scope LIMIT 0",
                    "SELECT message_sequence FROM admissible_dispatch_peer_inputs LIMIT 0",
                    "SELECT t.device_id,s.privacy,r.retired,f.digest,c.reply_id FROM matrix_transports t CROSS JOIN matrix_room_scopes s CROSS JOIN matrix_session_routes r CROSS JOIN final_replies f CROSS JOIN final_reply_calls c LIMIT 0",
                    "SELECT session_id FROM current_matrix_routes LIMIT 0",
                    "SELECT s.joined,s.invite_only,s.available,s.invalidation,m.transport_generation,f.cancel_requested,i.digest FROM matrix_room_scopes s CROSS JOIN matrix_room_memberships m CROSS JOIN final_replies f CROSS JOIN final_reply_inspections i LIMIT 0",
                    "SELECT id FROM current_final_replies LIMIT 0",
                    "SELECT incidental FROM final_replies LIMIT 0",
                    "SELECT e.scope_digest,e.config,r.digest,s.ingress_since,s.parent_session_id,t.observed_at,room.visibility_since,si.config,ti.config,ti.wake,n.verified_route,n.content_digest FROM matrix_ingress_events e CROSS JOIN verified_task_requests r CROSS JOIN matrix_session_routes s CROSS JOIN matrix_transports t CROSS JOIN matrix_room_scopes room CROSS JOIN session_inputs si CROSS JOIN task_inputs ti CROSS JOIN task_notices n LIMIT 0",
                    "SELECT id,sender,recipient,kind,priority,summary,full,mentions,attachments,created_at,reply_to,group_id,source,source_room,source_event_id,sender_mxid,room_recipients,default_recipient,schema_kind,schema_version,schema_payload,suppressed FROM operator_messages LIMIT 0",
                    "SELECT id,agent,message_id,kind,source,reason,context,created_at FROM delivery_events LIMIT 0",
                    "SELECT name,deleted_at,reason FROM agent_tombstones LIMIT 0",
                    "SELECT id,agent,regenerate,custom,mime,requested_at FROM avatar_requests LIMIT 0",
                ],
            },
        )?;
        let transport_trigger: bool = database.connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND name='matrix_transport_retire_approvals' AND tbl_name='matrix_transports')", [], |r|r.get(0)).map_err(|_|Error::Schema)?;
        if !transport_trigger {
            return Err(Error::Schema);
        }
        if let Some(profile) = profile {
            let tx = database
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let stored: Option<String> = tx
                .query_row(
                    "SELECT profile_digest FROM management_identity WHERE singleton=1",
                    [],
                    |r| r.get(0),
                )
                .optional()?;
            match stored {
                Some(stored) if stored != profile => return Err(Error::LocalAuthority),
                Some(_) => {}
                None => {
                    let tables = tx.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name<>'management_identity'")?
                        .query_map([], |r| r.get::<_, String>(0))?
                        .collect::<Result<Vec<_>, _>>()?;
                    for table in tables {
                        let quoted = table.replace('"', "\"\"");
                        let populated: bool = tx.query_row(
                            &format!("SELECT EXISTS(SELECT 1 FROM \"{quoted}\")"),
                            [],
                            |r| r.get(0),
                        )?;
                        if populated {
                            return Err(Error::LocalAuthority);
                        }
                    }
                    tx.execute(
                        "INSERT INTO management_identity(singleton,profile_digest) VALUES(1,?1)",
                        [profile],
                    )?;
                }
            }
            tx.commit()?;
        }
        if recover {
            // A previous owner died after an intent became externally executable. Inspection,
            // not automatically repeating that effect, is the only safe default.
            let tx = database
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            tx.execute("UPDATE engagements SET projection=json_set(projection,'$.cleanup','uncertain') WHERE id IN (SELECT engagement_id FROM effects WHERE kind='retire' AND state='started')",[])?;
            tx.execute(
                "UPDATE effects SET state='uncertain' WHERE state='started'",
                [],
            )?;
            graphs::reconcile(&tx, graphs::now_ms()?)?;
            replies::reconcile(&tx, graphs::now_ms()?, true)?;
            notice_custody::reconcile(&tx, graphs::now_ms()?, true)?;
            execution::recover_all(&tx, graphs::now_ms()?)?;
            approvals::recover(&tx)?;
            tx.execute("UPDATE approval_responses SET state='outcome_unknown' WHERE state IN ('authorized','response_may_send')", [])?;
            tx.execute("UPDATE received_files SET state='outcome_unknown',failure='outcome_unknown' WHERE state IN ('reserved','write_possible')", [])?;
            tx.execute(
                "UPDATE managed_accounts SET state='uncertain' WHERE state='preparing'",
                [],
            )?;
            accounts::reconcile_login_attempts(&tx, graphs::now_ms()?)?;
            tx.commit()?;
        }
        let accounts = accounts::Registry::open(&database.connection, directory)?;
        Ok(Self {
            accounts,
            db: database.connection,
            _ownership: database.ownership,
            approval_owner: std::sync::Arc::new(()),
            warm_scopes: std::collections::BTreeMap::new(),
            _legacy_writer: legacy_writer,
        })
    }
    pub fn register(&mut self, registration: &Registration) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::register_transaction(&tx, registration)?;
        tx.commit()?;
        Ok(())
    }
    fn register_transaction(
        tx: &Transaction<'_>,
        registration: &Registration,
    ) -> Result<(), Error> {
        registration.validate()?;
        bounded_row(
            tx,
            "registrations",
            "fleet_id",
            &registration.fleet_id,
            1024,
        )?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT config FROM registrations WHERE fleet_id=?1",
                [&registration.fleet_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            let old: Registration = serde_json::from_str(&previous)?;
            if old == *registration {
                return Ok(());
            }
            if registration.generation <= old.generation {
                return Err(Error::Generation);
            }
            // Rotation fences execution immediately. Existing allocations stay observable
            // and reserved until explicit revoke/reconciliation; rotation cannot erase spend.
        }
        tx.execute("INSERT INTO registrations(fleet_id,generation,config) VALUES(?1,?2,?3) ON CONFLICT(fleet_id) DO UPDATE SET generation=excluded.generation,config=excluded.config",
            params![registration.fleet_id,registration.generation,serialize(registration)?])?;
        graphs::reconcile(tx, graphs::now_ms()?)?;
        matrix_routes::reconcile(tx, graphs::now_ms()?)?;
        Ok(())
    }
    /// Bind the fleet's reception room after a verified connection probe (TS
    /// lib/fleet-protocol.js sets `receptionRoomId` on the fleet record). This is
    /// not a rotation: same generation, only an unbound reception may be set,
    /// and a different already-bound reception is a conflict.
    pub fn bind_reception(
        &mut self,
        fleet_id: &str,
        generation: u64,
        room: &str,
    ) -> Result<(), Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: String = tx
            .query_row(
                "SELECT config FROM registrations WHERE fleet_id=?1",
                [fleet_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        let mut registration: Registration = serde_json::from_str(&previous)?;
        if registration.generation != generation {
            return Err(Error::Generation);
        }
        if registration.reception_room_id == room {
            coordinator::verified_after_probe(&tx, fleet_id, generation)?;
            tx.commit()?;
            return Ok(());
        }
        if !registration.reception_room_id.is_empty() {
            return Err(Error::Conflict);
        }
        registration.reception_room_id = room.to_owned();
        registration.validate()?;
        tx.execute(
            "UPDATE registrations SET config=?2 WHERE fleet_id=?1",
            params![fleet_id, serialize(&registration)?],
        )?;
        coordinator::verified_after_probe(&tx, fleet_id, generation)?;
        matrix_routes::reconcile(&tx, graphs::now_ms()?)?;
        tx.commit()?;
        Ok(())
    }
    pub fn put_resource(&mut self, resource: &Resource) -> Result<CatalogResource, Error> {
        self.edit_resource(resource, Some(resource.published))
    }
    /// Create a local source without advertising it or replacing a concurrent
    /// operator's configuration. Publication and server allocation are separate.
    pub fn create_resource_source(
        &mut self,
        resource: &Resource,
    ) -> Result<CatalogResource, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let resource = prepare_resource_write(&tx, resource, Some(false), true)?;
        self.accounts.check_resource(&tx, &resource)?;
        write_resource_configuration(&tx, &resource, true)?;
        let catalog = resource_ceiling::catalog_entry(&tx, &resource)?;
        tx.commit()?;
        Ok(catalog)
    }
    pub fn edit_resource(
        &mut self,
        resource: &Resource,
        publication: Option<bool>,
    ) -> Result<CatalogResource, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let resource = prepare_resource_write(&tx, resource, publication, false)?;
        self.accounts.check_resource(&tx, &resource)?;
        write_resource_configuration(&tx, &resource, false)?;
        let catalog = resource_ceiling::catalog_entry(&tx, &resource)?;
        tx.commit()?;
        Ok(catalog)
    }
    pub fn put_seat(&mut self, seat: &Seat) -> Result<(), Error> {
        seat.validate()?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if seat.id.starts_with("seat_native_") {
            return Err(Error::Unqualified);
        }
        bounded_row(&tx, "seats", "id", &seat.id, 2048)?;
        tx.execute("INSERT INTO seats(id,config) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET config=excluded.config", params![seat.id,serialize(seat)?])?;
        tx.commit()?;
        Ok(())
    }
    pub fn catalog(&self, after: &str, limit: usize) -> Result<Vec<CatalogResource>, Error> {
        self.catalog_for(None, after, limit)
    }
    pub fn catalog_for(
        &self,
        fleet: Option<&str>,
        after: &str,
        limit: usize,
    ) -> Result<Vec<CatalogResource>, Error> {
        if limit == 0 || limit > 100 {
            return Err(InvalidInput("page limit must be 1..100").into());
        }
        // At most 2,048 configurations exist. Iterate until enough *qualified*
        // rows are found; filtering a short SQL page would incorrectly hide later IDs.
        let mut query = self.db.prepare("SELECT config FROM resources WHERE id>?1 AND json_extract(config,'$.published')=1 ORDER BY id")?;
        let rows = query.query_map([after], |r| r.get::<_, String>(0))?;
        let allowed: std::collections::BTreeSet<_> = qualification::roles()
            .filter_map(|role| match role_available(&self.db, role, fleet) {
                Ok(true) => Some(Ok(role)),
                Ok(false) => None,
                Err(e) => Some(Err(e)),
            })
            .collect::<Result<_, _>>()?;
        let mut output = Vec::new();
        for row in rows {
            let resource: Resource = serde_json::from_str(&row?)?;
            match self.accounts.check_resource(&self.db, &resource) {
                Ok(()) => {}
                Err(Error::LocalAuthority | Error::Unqualified) => continue,
                Err(error) => return Err(error),
            }
            let mut public = resource_ceiling::catalog_entry(&self.db, &resource)?;
            public.roles.retain(|role| allowed.contains(role.as_str()));
            if !public.roles.is_empty() {
                output.push(public);
            }
            if output.len() == limit {
                break;
            }
        }
        Ok(output)
    }
    pub fn resource_configuration(&self, id: &str) -> Result<Resource, Error> {
        read_resource(&self.db, id)
    }
    /// The stored definition and the monthly maximum in force, one snapshot.
    pub fn configured_resource(&self, id: &str) -> Result<ConfiguredResource, Error> {
        let config = read_resource(&self.db, id)?;
        let effective_ceiling = resource_ceiling::effective_ceiling(&self.db, &config)?;
        Ok(ConfiguredResource {
            id: id.to_owned(),
            config,
            effective_ceiling,
        })
    }
    pub fn resource_configurations(
        &self,
        after: &str,
        limit: usize,
    ) -> Result<Vec<ConfiguredResource>, Error> {
        if limit == 0 || limit > 100 {
            return Err(InvalidInput("page limit must be 1..100").into());
        }
        let mut query = self
            .db
            .prepare("SELECT id,config FROM resources WHERE id>?1 ORDER BY id LIMIT ?2")?;
        query
            .query_map(params![after, limit as i64], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .map(|row| {
                let (id, config) = row?;
                let mut config: Resource = serde_json::from_str(&config)?;
                config.roles = config.eligible_roles();
                let effective_ceiling = resource_ceiling::effective_ceiling(&self.db, &config)?;
                Ok(ConfiguredResource {
                    id,
                    config,
                    effective_ceiling,
                })
            })
            .collect()
    }
    pub fn seats(&self, after: &str, limit: usize) -> Result<Vec<Seat>, Error> {
        if limit == 0 || limit > 100 {
            return Err(InvalidInput("page limit must be 1..100").into());
        }
        let mut query = self
            .db
            .prepare("SELECT config FROM seats WHERE id>?1 ORDER BY id LIMIT ?2")?;
        query
            .query_map(params![after, limit as i64], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    pub fn engagements(&self, after: &str, limit: usize) -> Result<Vec<Engagement>, Error> {
        if limit == 0 || limit > 100 {
            return Err(InvalidInput("page limit must be 1..100").into());
        }
        let mut query = self
            .db
            .prepare("SELECT projection FROM engagements WHERE id>?1 ORDER BY id LIMIT ?2")?;
        query
            .query_map(params![after, limit as i64], |r| r.get::<_, String>(0))?
            .map(|s| Ok(serde_json::from_str(&s?)?))
            .collect()
    }
    /// Transport pages are scoped by registration, never by server hostname.
    pub fn fleet_engagements(
        &self,
        fleet: &str,
        after: &str,
        limit: usize,
    ) -> Result<Vec<Engagement>, Error> {
        if limit == 0 || limit > 100 {
            return Err(InvalidInput("page limit must be 1..100").into());
        }
        let mut query = self.db.prepare(
            "SELECT projection FROM engagements WHERE fleet_id=?1 AND id>?2 ORDER BY id LIMIT ?3",
        )?;
        query
            .query_map(params![fleet, after, limit], |r| r.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect()
    }
    /// The console engagements list (board #60 item 3): the label the triage
    /// list already rendered, widened to the figures TS's `/api/engagements`
    /// (`backend-v2.js:14964-14975`) carries and native was dropping, and a
    /// server-side `state` filter (`:14965` `?state=`).
    ///
    /// `agent_remaining_tokens` is computed the way the ADMISSION decision
    /// computes it — `min` of the non-null limits (`ceiling - drawn`, the
    /// seat's remaining, the pool's remaining) — so the queue shows the
    /// over-commitment the decision would refuse, before the decision. The
    /// engagement's own reservation is NOT excluded (TS `remainingFor(e.agent)`
    /// passes no exclusion here), so the figure is the agent's, not a
    /// self-forgiving one.
    ///
    /// `owner_binding_required` is TS `:14972`: a PENDING request with no
    /// owner binding yet. It reads `approval_bindings` directly rather than
    /// the `current_approval_bindings` view, because that view is scoped to
    /// `state='active'` engagements and so could never answer a pending row.
    ///
    /// `created_at_ms` is `evidence.observed_at_ms` — the request's own
    /// observation instant, recorded at admission. Native keeps no separate
    /// creation clock, so this is the closest true instant, never invented.
    /// `ended_at_ms` is `engagement_ends.ended_at`, absent while live.
    pub fn engagement_labels(
        &self,
        after: &str,
        state: Option<&str>,
        limit: usize,
    ) -> Result<Vec<EngagementLabel>, Error> {
        if limit == 0 || limit > 100 {
            return Err(InvalidInput("page limit must be 1..100").into());
        }
        let at = graphs::now_ms()?;
        let mut query = self.db.prepare(
            "SELECT e.projection,e.resource_id, \
             (SELECT ended_at FROM engagement_ends WHERE engagement_id=e.id), \
             EXISTS(SELECT 1 FROM approval_bindings b JOIN approval_rooms room \
              ON room.server_name=b.server_name AND room.room_id=b.room_id \
               AND room.generation=b.room_generation AND room.available=1 \
              WHERE b.engagement_id=e.id), \
             json_extract(e.evidence,'$.observed_at_ms') \
             FROM engagements e WHERE e.id>?1 AND (?2 IS NULL OR e.state=?2) \
             ORDER BY e.id LIMIT ?3",
        )?;
        #[allow(clippy::type_complexity)]
        let rows: Vec<(String, String, Option<i64>, bool, Option<i64>)> = query
            .query_map(params![after, state, limit as i64], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<_, _>>()?;
        let mut labels = Vec::with_capacity(rows.len());
        for (projection, resource_id, ended_at, has_binding, observed_at) in rows {
            let engagement: Engagement = serde_json::from_str(&projection)?;
            let resource = read_resource(&self.db, &resource_id)?;
            let report = usage::ceiling_report(&self.db, &resource.id(), at)?;
            let spent = budget(&self.db, &resource, None, false)?;
            let by_ceiling = report
                .ceiling_tokens
                .map(|c| c.saturating_sub(report.drawn));
            let agent_remaining_tokens = [
                by_ceiling,
                spent.seat.remaining.map(u64::from),
                spent.pool.remaining.map(u64::from),
            ]
            .into_iter()
            .flatten()
            .min();
            // Compare before the state moves into the label.
            let pending = engagement.state == EngagementState::Pending;
            let allocated_tokens = u64::from(engagement.allocation());
            let observed_tokens = quota_holds::spend(&self.db, &engagement.id)?;
            let accounted_tokens: Option<u64> = self.db.query_row(
                "SELECT json_extract(receipt,'$.consumedTokens') FROM coordinator_settlements WHERE agent_id=?1",
                [&engagement.id], |row| row.get(0)).optional()?;
            let spent_tokens = match (observed_tokens, accounted_tokens) {
                (Some(observed), Some(accounted)) => Some(observed.max(accounted)),
                (observed, accounted) => observed.or(accounted),
            };
            let quota_paused = quota_holds::paused(&self.db, &engagement.id)?;
            labels.push(EngagementLabel {
                matrix_profile: {let profile=self.matrix_agent_profile(&engagement.id)?; (profile["state"]!="default").then_some(profile)},
                coordinator_managed:self.db.query_row("SELECT EXISTS(SELECT 1 FROM coordinator_engagements c JOIN engagements e ON e.fleet_id=c.id WHERE e.id=?1)",[&engagement.id],|r|r.get(0))?,
                id: engagement.id.clone(),
                agent_name: engagement.agent_name.as_str().to_owned(),
                project_name: engagement.project_name.clone(),
                role: engagement.role.clone(),
                requested_tokens: u64::from(engagement.requested_tokens),
                state: engagement.state,
                cleanup: engagement.cleanup,
                agent_remaining_tokens,
                owner_binding_required: pending && !has_binding,
                created_at_ms: observed_at.and_then(|v| u64::try_from(v).ok()),
                ended_at_ms: ended_at.and_then(|v| u64::try_from(v).ok()),
                allocated_tokens,
                spent_tokens,
                quota_paused,
            });
        }
        Ok(labels)
    }
    /// The bounded approval observation read (ADR-138, C2a): pages
    /// `owner_approvals` by the opaque id cursor with the same hard cap as
    /// `engagements`, and names its `SELECT` columns so the projection cannot
    /// widen silently — no `description`, no `config`/`application`/
    /// `observation` JSON, no owner or room column is ever read, so none can
    /// cross. The item is exactly the console's seven camelCase keys.
    pub fn approvals(&self, after: &str, limit: usize) -> Result<Vec<Value>, Error> {
        if limit == 0 || limit > 100 {
            return Err(InvalidInput("page limit must be 1..100").into());
        }
        let mut query = self.db.prepare(&format!(
            "{APPROVAL_SELECT} WHERE a.id>?1 ORDER BY a.id LIMIT ?2"
        ))?;
        let rows = query
            .query_map(params![after, limit as i64], approval_tuple)?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        rows.into_iter().map(approval_row).collect()
    }
    /// The single-row half of the observation read: the same seven named
    /// columns as the list, keyed by the approval's own opaque id. Read-only.
    pub fn approval(&self, id: &str) -> Result<Value, Error> {
        project::identifier(id, 128)?;
        let row = self
            .db
            .query_row(
                &format!("{APPROVAL_SELECT} WHERE a.id=?1"),
                [id],
                approval_tuple,
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        approval_row(row)
    }
    pub fn get(&self, id: &str) -> Result<Engagement, Error> {
        read_engagement(&self.db, id)
    }
    /// The console verdict audit (parity: backend-v2.js:14980-14983 →
    /// lib/engagement-store.js:804-806 `listAudit`): the newest `limit`
    /// recorded decisions, newest-first. Entry shape is the retained
    /// `{type, at, ...detail}` — `type` is the deciding command's own word,
    /// `at` the deciding moment, and the detail carries the engagement id and
    /// the state the verdict produced. Rows recorded before migration 047
    /// have no word/clock and surface as unknown, never invented.
    /// The console verdict audit (parity: backend-v2.js:14980-14983 →
    /// lib/engagement-store.js:804-806 `listAudit`): the newest `limit`
    /// recorded decisions, newest-first. Entry shape is the retained
    /// `{type, at, ...detail}` — `type` is the deciding command's own word,
    /// `at` the deciding moment, and the detail carries the engagement id and
    /// the state the verdict produced. Rows recorded before migration 047
    /// have no word/clock and surface as unknown, never invented. The limit
    /// clamps exactly like the retained store — `Math.max(1, Math.min(limit,
    /// AUDIT_LIMIT))` — no failure state TS did not have.
    pub fn decisions_audit(&self, limit: usize) -> Result<Vec<serde_json::Value>, Error> {
        let limit = limit.clamp(1, 2000);
        let mut query = self
            .db
            .prepare("SELECT kind,at,result FROM decisions ORDER BY rowid DESC LIMIT ?1")?;
        let rows = query
            .query_map([limit as i64], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<i64>>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        rows.into_iter()
            .map(|(kind, at, result)| {
                let engagement: Engagement =
                    serde_json::from_str(&result).map_err(|_| Error::Schema)?;
                Ok(serde_json::json!({
                    "type": kind,
                    "at": at,
                    "engagementId": engagement.id,
                    "state": engagement.state,
                }))
            })
            .collect()
    }
    /// The read-only project-sides projection (ADR-132): one row per fleet
    /// registration — the id IS the server name (ADR-016) — LEFT JOINed to
    /// its projects. `SELECT`-named columns only, and the side fields are
    /// extracted by path from the config JSON (`json_extract`), never by
    /// parsing the whole config and stripping: a credential-shaped value
    /// seeded into the config has no path into this projection. Projects
    /// are capped at 64 per side; the fleet cap is `registrations`' own
    /// 1024-row bound. `registered` compares the row's generation column
    /// against the config's own generation field — true by construction
    /// today, computed rather than assumed.
    pub fn project_sides(&self) -> Result<Vec<ProjectSide>, Error> {
        // ONE statement, one snapshot (F3): the LEFT JOIN is evaluated
        // inside a single implicit transaction, so a project written
        // between two reads cannot tear the view — the way `agent_roster`
        // joins in one statement. The ordering guarantee is this
        // statement's ORDER BY. The per-side cap (F2) is INSIDE the
        // statement as a ROW_NUMBER window — never a truncation of an
        // unbounded fetch in Rust. Bound arithmetic: `register()` bounds
        // `registrations` at 1024 rows and the window bounds projects at
        // 64 per side, so the joined statement yields at most 65,536 rows.
        let mut query = self.db.prepare(
            "SELECT r.fleet_id, \
             json_extract(r.config,'$.serverName'), \
             json_extract(r.config,'$.representativeMxid'), \
             json_extract(r.config,'$.receptionRoomId'), \
             json_extract(r.config,'$.generation'), r.generation, \
             p.id, p.room_id \
             FROM registrations r \
             LEFT JOIN (SELECT fleet_id,id,room_id, \
             ROW_NUMBER() OVER (PARTITION BY fleet_id ORDER BY id) AS rn \
             FROM projects) p ON p.fleet_id=r.fleet_id AND p.rn<=64 \
             ORDER BY r.fleet_id,p.id",
        )?;
        let rows = query.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, rusqlite::types::Value>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
            ))
        })?;
        let mut sides: Vec<ProjectSide> = Vec::new();
        let mut current_fleet: Option<String> = None;
        for row in rows {
            let (
                fleet,
                id,
                representative,
                reception,
                config_generation,
                row_generation,
                project,
                room,
            ) = row?;
            let generation = u64::try_from(row_generation).map_err(|_| Error::Schema)?;
            // F4: a config whose `$.generation` is not a JSON integer is
            // corrupt store state, not a value to coerce — the conversion
            // failure is mapped to Error::Schema AT THE READ (a named
            // corrupt-state failure) instead of surfacing as the raw
            // FromSqlConversionFailure/Sqlite variant.
            let config_generation = match config_generation {
                rusqlite::types::Value::Integer(value) => {
                    u64::try_from(value).map_err(|_| Error::Schema)?
                }
                _ => return Err(Error::Schema),
            };
            if current_fleet.as_deref() == Some(fleet.as_str()) {
                // LEFT JOIN NULL arm (a side with no project) contributes
                // nothing to the fold.
                if let (Some(project), Some(room)) = (project, room) {
                    sides
                        .last_mut()
                        .expect("the fold key implies a pushed side")
                        .projects
                        .push(SideProject {
                            id: project,
                            room_id: room,
                        });
                }
            } else {
                current_fleet = Some(fleet);
                sides.push(ProjectSide {
                    id,
                    representative,
                    generation,
                    reception_room_id: reception,
                    registered: generation == config_generation,
                    projects: match (project, room) {
                        (Some(project), Some(room)) => vec![SideProject {
                            id: project,
                            room_id: room,
                        }],
                        _ => Vec::new(),
                    },
                });
            }
        }
        Ok(sides)
    }
    /// The read-only agent roster (ADR-126, widened by board #22): one row
    /// per AGENT NAME — the TS roster serializes every agent record
    /// (`backend-v2.js:11696`), so the derivation groups the engagement
    /// projections (ANY state, including ended ones) by `agent_name` and an
    /// agent whose engagements all ended still appears. The representative
    /// engagement is the most-live one (active > reserved > pending > ended),
    /// newest id among ties, so `engagement_id` targets a live engagement
    /// whenever the agent has one and the lifecycle routes keep working.
    /// `online` is REAL worker state: a live dispatch
    /// (`leased`/`started`/`parked`, mirroring the TS delivery-state's
    /// started/parked/leased online rule, `backend-v2.js:6854-6855`) in one
    /// of the agent's sessions. `last_seen_ms` is the newest
    /// `runner_attempts.created_at` across ALL the agent's engagements —
    /// null, never zero, when the agent never attempted; `last_activity_ms`
    /// keeps the representative engagement's own newest attempt clock.
    /// Bounded to one read of at most 100 agents, ordered by name.
    pub fn agent_roster(&self) -> Result<Vec<AgentRosterRow>, Error> {
        let mut query = self.db.prepare(
            "SELECT e.projection,r.config, \
             EXISTS(SELECT 1 FROM runner_sessions s JOIN runner_dispatches d ON d.session_id=s.id \
              JOIN engagements e2 ON e2.id=s.engagement_id \
              WHERE json_extract(e2.projection,'$.agentName')=json_extract(e.projection,'$.agentName') \
              AND d.state IN ('leased','started','parked')), \
             (SELECT MAX(a.created_at) FROM runner_sessions s \
              JOIN runner_dispatches d ON d.session_id=s.id \
              JOIN runner_attempts a ON a.dispatch_id=d.id \
              JOIN engagements e2 ON e2.id=s.engagement_id \
              WHERE json_extract(e2.projection,'$.agentName')=json_extract(e.projection,'$.agentName')), \
             (SELECT MAX(a.created_at) FROM runner_sessions s \
              JOIN runner_dispatches d ON d.session_id=s.id \
              JOIN runner_attempts a ON a.dispatch_id=d.id WHERE s.engagement_id=e.id), \
             (SELECT EXISTS(SELECT 1 FROM agent_lifecycle l JOIN engagements e4 ON e4.id=l.engagement_id \
               WHERE json_extract(e4.projection,'$.agentName')=json_extract(e.projection,'$.agentName') \
               AND l.stopped_at IS NOT NULL AND l.started_at IS NULL)), \
             (SELECT d.state FROM runner_sessions s JOIN runner_dispatches d ON d.session_id=s.id \
               JOIN engagements e2 ON e2.id=s.engagement_id \
               WHERE json_extract(e2.projection,'$.agentName')=json_extract(e.projection,'$.agentName') \
               AND d.state IN ('leased','started','parked') \
               ORDER BY CASE d.state WHEN 'started' THEN 3 WHEN 'parked' THEN 2 ELSE 1 END DESC, d.id LIMIT 1), \
             (SELECT CASE \
               WHEN COUNT(*)=0 THEN NULL \
               WHEN MIN(CASE WHEN json_extract(u.latest_counts,'$.input') IS NULL \
                              OR json_extract(u.latest_counts,'$.output') IS NULL \
                              OR json_extract(u.latest_counts,'$.cacheWrite') IS NULL \
                              OR json_extract(u.latest_counts,'$.cacheRead') IS NULL \
                             THEN 0 ELSE 1 END)=0 THEN NULL \
               ELSE SUM(json_extract(u.latest_counts,'$.input') + json_extract(u.latest_counts,'$.output') \
               + json_extract(u.latest_counts,'$.cacheWrite') + json_extract(u.latest_counts,'$.cacheRead')) END \
              FROM usage_sources u JOIN engagements e3 ON e3.id=u.engagement_id \
              WHERE json_extract(e3.projection,'$.agentName')=json_extract(e.projection,'$.agentName')) \
             FROM engagements e JOIN resources r ON r.id=e.resource_id \
             WHERE NOT EXISTS (SELECT 1 FROM engagements b \
              WHERE json_extract(b.projection,'$.agentName')=json_extract(e.projection,'$.agentName') \
              AND (CASE b.state WHEN 'active' THEN 3 WHEN 'reserved' THEN 2 WHEN 'pending' THEN 1 ELSE 0 END,b.id) \
               > (CASE e.state WHEN 'active' THEN 3 WHEN 'reserved' THEN 2 WHEN 'pending' THEN 1 ELSE 0 END,e.id)) \
             ORDER BY json_extract(e.projection,'$.agentName') LIMIT 100",
        )?;
        query
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, bool>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })?
            .map(|row| {
                let (
                    projection,
                    config,
                    online,
                    last_seen,
                    last_activity,
                    manual_down,
                    dispatch,
                    consumed,
                ) = row?;
                let engagement: Engagement = serde_json::from_str(&projection)?;
                let resource: Resource = serde_json::from_str(&config)?;
                // TS `serializeAgent` (`backend-v2.js:6846-6848`) picks the
                // agent-machine word: `agent.manualDown ? 'stopped' : ... :
                // queued ? 'queued' : 'idle'`. Native's durable equivalent of
                // `manualDown` is the `agent_lifecycle` row
                // (`agent_lifecycle.rs:134`, read as `started_at IS NULL`),
                // and the live dispatch supplies the working words. A stopped
                // agent that still has a live dispatch row is reported
                // stopped: the operator's decision outranks work the stop is
                // still fencing.
                // A deployed, unstopped engagement is SERVING — TS's
                // `machine.online`, the word the retained roster shows for it
                // (`workforce/page.jsx:446`). The live run reported exactly
                // these agents as Unknown (board #106), so the serving case is
                // named `running` rather than left null; an engagement that is
                // not `active` (pending/reserved) has no worker yet and stays
                // unknown — never an invented word.
                let serving = engagement.state == EngagementState::Active && !manual_down;
                let liveness = match (manual_down, dispatch.as_deref()) {
                    (true, _) => Some("stopped".to_owned()),
                    (false, Some("started")) => Some("running".to_owned()),
                    (false, Some("parked")) => Some("waiting_approval".to_owned()),
                    (false, Some("leased")) => Some("starting".to_owned()),
                    (false, None) if serving => Some("running".to_owned()),
                    // A live row native has no word for (queued, or an
                    // outcome the operator still owns), or an engagement with
                    // no worker at all: said as unknown, never guessed.
                    (false, _) => None,
                };
                // TS `online` is worker liveness, not "a dispatch happens to
                // be live" (`backend-v2.js:6789,6802`): a serving agent
                // between dispatches is Online, which is the other half of
                // what the live run reported (board #106).
                let online = online || serving;
                let quota_paused = quota_holds::paused(&self.db, &engagement.id)?;
                Ok(AgentRosterRow {
                    name: engagement.agent_name.as_str().to_owned(),
                    framework: resource.framework,
                    role: engagement.role,
                    state: engagement.state,
                    engagement_id: engagement.id,
                    requested_tokens: u64::from(engagement.requested_tokens),
                    online,
                    last_seen_ms: last_seen.and_then(|v| u64::try_from(v).ok()),
                    last_activity_ms: last_activity.and_then(|v| u64::try_from(v).ok()),
                    liveness,
                    manual_down,
                    consumed: consumed.and_then(|v| u64::try_from(v).ok()),
                    quota_paused,
                })
            })
            .collect()
    }
    /// The read-only agent detail (board #22, TS `backend-v2.js:12155`):
    /// `None` when no engagement names the agent (the route's 404), else the
    /// agent-keyed identity — the same most-live representative engagement
    /// the roster picks — plus the resource id, project id, the rooms its
    /// sessions bind (each with its live dispatch state when one exists),
    /// the current live dispatch across ALL the agent's sessions, and the
    /// agent's ten most recently touched tasks. Same online/last-seen
    /// derivation as the roster: one transaction-consistent read, never a
    /// second arithmetic path.
    pub fn agent_detail(&self, name: &str) -> Result<Option<AgentDetail>, Error> {
        let Some(rep) = self
            .db
            .query_row(
                "SELECT e.projection,r.config,e.resource_id,e.project_id FROM engagements e \
                 JOIN resources r ON r.id=e.resource_id \
                 WHERE json_extract(e.projection,'$.agentName')=?1 \
                 ORDER BY CASE e.state WHEN 'active' THEN 3 WHEN 'reserved' THEN 2 \
                  WHEN 'pending' THEN 1 ELSE 0 END DESC, e.id DESC LIMIT 1",
                [name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        let (projection, config, resource_id, project_id) = rep;
        let engagement: Engagement = serde_json::from_str(&projection)?;
        let resource: Resource = serde_json::from_str(&config)?;
        let count: i64 = self.db.query_row(
            "SELECT COUNT(*) FROM engagements WHERE json_extract(projection,'$.agentName')=?1",
            [name],
            |row| row.get(0),
        )?;
        let online: bool = self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM runner_sessions s JOIN runner_dispatches d ON d.session_id=s.id \
             JOIN engagements e ON e.id=s.engagement_id \
             WHERE json_extract(e.projection,'$.agentName')=?1 AND d.state IN ('leased','started','parked'))",
            [name],
            |row| row.get(0),
        )?;
        let last_seen_ms: Option<u64> = self
            .db
            .query_row(
                "SELECT MAX(a.created_at) FROM runner_sessions s JOIN runner_dispatches d ON d.session_id=s.id \
                 JOIN runner_attempts a ON a.dispatch_id=d.id JOIN engagements e ON e.id=s.engagement_id \
                 WHERE json_extract(e.projection,'$.agentName')=?1",
                [name],
                |row| row.get::<_, Option<i64>>(0),
            )?
            .and_then(|v| u64::try_from(v).ok());
        let mut rooms_query = self.db.prepare(
            "SELECT s.id,json_extract(s.binding,'$.room_id'), \
             (SELECT d.state FROM runner_dispatches d WHERE d.session_id=s.id \
              AND d.state IN ('leased','started','parked') LIMIT 1), \
             (SELECT d.id FROM runner_dispatches d WHERE d.session_id=s.id \
              AND d.state IN ('leased','started','parked') LIMIT 1) \
             FROM runner_sessions s JOIN engagements e ON e.id=s.engagement_id \
             WHERE json_extract(e.projection,'$.agentName')=?1 ORDER BY s.id LIMIT 100",
        )?;
        let rooms: Vec<AgentDetailRoom> = rooms_query
            .query_map([name], |row| {
                Ok(AgentDetailRoom {
                    session_id: row.get(0)?,
                    room_id: row.get(1)?,
                    dispatch_state: row.get(2)?,
                    dispatch_id: row.get(3)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let dispatch: Option<AgentDetailRoom> = rooms
            .iter()
            .find(|room| room.dispatch_state.is_some())
            .cloned();
        let mut tasks_query = self.db.prepare(
            "SELECT t.config FROM canonical_tasks t JOIN runner_sessions s ON s.id=t.session_id \
             JOIN engagements e ON e.id=s.engagement_id \
             WHERE json_extract(e.projection,'$.agentName')=?1 \
             ORDER BY json_extract(t.config,'$.updated_at') DESC, t.id LIMIT 10",
        )?;
        let tasks: Vec<hagency_core::tasks::Task> = tasks_query
            .query_map([name], |row| row.get::<_, String>(0))?
            .map(|row| Ok(serde_json::from_str(&row?)?))
            .collect::<Result<_, Error>>()?;
        // Board #53: the agent's own reminders, oldest first, across every
        // engagement the agent name holds. Bounded to 100 rows, matching the
        // rooms list's own bound.
        let mut reminders_query = self.db.prepare(
            "SELECT r.id,r.engagement_id,r.session_id,r.msg,r.created_at,r.fire_at,r.fired_at \
             FROM reminders r JOIN engagements e ON e.id=r.engagement_id \
             WHERE json_extract(e.projection,'$.agentName')=?1 ORDER BY r.id LIMIT 100",
        )?;
        let reminders: Vec<Reminder> = reminders_query
            .query_map([name], |row| {
                Ok(Reminder {
                    id: row.get(0)?,
                    engagement_id: row.get(1)?,
                    session_id: row.get(2)?,
                    msg: row.get(3)?,
                    created_at: row.get(4)?,
                    fire_at: row.get(5)?,
                    fired_at: row.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(Some(AgentDetail {
            name: engagement.agent_name.as_str().to_owned(),
            framework: resource.framework,
            role: engagement.role,
            state: engagement.state,
            engagement_id: engagement.id,
            requested_tokens: u64::from(engagement.requested_tokens),
            online,
            last_seen_ms,
            resource_id,
            project_id,
            engagements: u64::try_from(count).unwrap_or_default(),
            rooms,
            dispatch,
            tasks,
            reminders,
        }))
    }
    /// The agent's launch runtime profile (board #49, TS `backend-v2.js:12344`
    /// `GET /api/agents/:name/launch-env`): the profile of the resource the
    /// agent's representative engagement works from, projected into the TS
    /// `normalizeRuntimeProfile` shape (`{primary, supervisor}`). The port
    /// stores no supervisor profile, so `supervisor` is null — the TS shape
    /// when the stored profile carries none. `None` is the route's 404: no
    /// engagement names the agent, the same key `agent_detail` selects on.
    pub fn agent_launch_env(&self, name: &str) -> Result<Option<RuntimeProfile>, Error> {
        let config: Option<String> = self
            .db
            .query_row(
                "SELECT r.config FROM engagements e JOIN resources r ON r.id=e.resource_id \
                 WHERE json_extract(e.projection,'$.agentName')=?1 \
                 ORDER BY CASE e.state WHEN 'active' THEN 3 WHEN 'reserved' THEN 2 \
                  WHEN 'pending' THEN 1 ELSE 0 END DESC, e.id DESC LIMIT 1",
                [name],
                |row| row.get(0),
            )
            .optional()?;
        let Some(config) = config else {
            return Ok(None);
        };
        let resource: Resource = serde_json::from_str(&config)?;
        Ok(Some(RuntimeProfile {
            primary: Some(RuntimeProfileRole {
                framework: resource.framework,
                provider: resource.provider,
                model: resource.model,
                reasoning: resource.reasoning,
            }),
            supervisor: None,
        }))
    }
    /// The agent's ACTIVE engagement ids, newest first — the list a force
    /// delete revokes (TS `backend-v2.js:12231-12238`: `engagementStore.list(
    /// {state:'active'})` filtered to this agent, then revoked one by one).
    /// An agent is a derived projection, so this is the only way to find the
    /// commitments it holds; a commitment lives in the engagement's own state
    /// (`pool_commitments`/`seat_commitments`), so revoking releases it.
    /// Bounded, like the roster read.
    pub fn agent_active_engagements(&self, name: &str) -> Result<Vec<String>, Error> {
        let mut query = self.db.prepare(
            "SELECT id FROM engagements \
             WHERE json_extract(projection,'$.agentName')=?1 AND state='active' \
             ORDER BY id LIMIT 100",
        )?;
        query
            .query_map([name], |row| row.get::<_, String>(0))?
            .map(|row| Ok(row?))
            .collect::<Result<_, Error>>()
    }
    pub fn resource_budget(&self, id: &str) -> Result<Budget, Error> {
        budget(&self.db, &read_resource(&self.db, id)?, None, false)
    }
    /// The budget the console publishes (brief 18): the commitments budget
    /// AND the ceiling draw in ONE transaction-consistent read, so a page
    /// can never render figures from two different writer jobs. The draw
    /// figures (`spent`/`consumed` unknown-when-unmeasured, `drawn =
    /// max(reserved, spent)`, `backend-v2.js:14052-14053`) are the same ones
    /// the alarm sweep and admission publish — never a second arithmetic
    /// path (ADR-121).
    pub fn resource_headroom(&self, id: &str, at: u64) -> Result<(Budget, CeilingReport), Error> {
        let budget = budget_at(&self.db, &read_resource(&self.db, id)?, None, false, at)?;
        let report = usage::ceiling_report(&self.db, id, at)?;
        Ok((budget, report))
    }

    pub fn admit(&mut self, proof: &VerifiedRequest, now: u64) -> Result<Engagement, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        authority(&tx, proof, now)?;
        project_authority(&tx, proof)?;
        let request = proof.request();
        let id = request.engagement_id()?;
        let digest = request.digest()?;
        let previous: Option<String> = tx
            .query_row("SELECT digest FROM engagements WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(old) = previous {
            if old != digest {
                return Err(Error::Conflict);
            }
            return read_engagement(&tx, &id); // Exact replay survives withdrawal.
        }
        bounded_row(&tx, "engagements", "id", &id, 10_000)?;
        let resource = read_resource(&tx, &request.agent_definition.resource_id)?;
        self.accounts.check_resource(&tx, &resource)?;
        if !resource.qualifies(&request.role)
            || !role_available(&tx, &request.role, Some(&request.fleet_id))?
        {
            return Err(Error::Unqualified);
        }
        let collision: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM engagements WHERE fleet_id=?1 AND project_id=?2 AND name=?3 AND state IN ('pending','reserved','active'))",
            params![request.fleet_id,request.target_project_id,request.agent_definition.name.as_str()], |r| r.get(0))?;
        if collision {
            return Err(Error::Conflict);
        }
        /*
         * Task #19 TS parity (lib/engagement-store.js:546-566): the routing
         * verdict is RECORDED, never used to refuse — TS stores the request
         * with its `route` and `autoJoined` so the queue can show why it did
         * not auto-join. `remainingTokens` is computed exactly like the
         * approve() check but with `for_auto_join=true` (the retained JS
         * `remainingFor(agent, { forAutoJoin: true })`), and a seat period
         * mismatch nulls the whole figure (backend-v2.js:14057) rather than
         * erroring, because routing must name `overCeiling`, not refuse.
         */
        let whitelisted = tx
            .query_row(
                "SELECT 1 FROM room_whitelist WHERE project_room_id=?1",
                [&request.target_room_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let cross_family_ok = role_available(&tx, &request.role, Some(&request.fleet_id))?;
        let offer = engagement_terms::read_offer(&tx, &request.role)?;
        let report = usage::ceiling_report(&tx, &resource.id(), now)?;
        let spent_budget = budget(&tx, &resource, None, true)?;
        let seat_ok = spent_budget.seat.status != allocation::SeatStatus::PeriodMismatch;
        let by_ceiling = report
            .ceiling_tokens
            .map(|c| c.saturating_sub(report.drawn));
        let remaining = [
            by_ceiling,
            seat_ok
                .then_some(spent_budget.seat.remaining)
                .flatten()
                .map(u64::from),
            spent_budget.pool.remaining.map(u64::from),
        ]
        .into_iter()
        .flatten()
        .min();
        // TS holdsAllocation: an engagement that has a live allocation — in
        // this store that is reserved/active with a non-failed provision.
        // `role` lives in the context JSON, not a column.
        let active_for_role: i64 = tx.query_row(
            "SELECT COUNT(*) FROM engagements e WHERE json_extract(e.context,'$.role')=?1 \
             AND e.state IN ('reserved','active') \
             AND NOT EXISTS(SELECT 1 FROM effects f WHERE f.engagement_id=e.id AND f.kind='provision' AND f.state='failed')",
            [&request.role],
            |r| r.get(0),
        )?;
        let (route, auto_joined) = engagement_terms::route_request(
            whitelisted,
            cross_family_ok,
            offer.as_ref(),
            u64::from(request.requested_tokens),
            request.rate_per_day.map(u64::from),
            remaining,
            active_for_role,
        );
        let value = Engagement {
            route: Some(route.to_owned()),
            auto_joined,
            allocated_tokens: None,
            id,
            request_id: request.request_id.clone(),
            project_id: request.target_project_id.clone(),
            project_room_id: request.target_room_id.clone(),
            project_name: proof.project_name().map(str::to_owned),
            agent_name: request.agent_definition.name.clone(),
            runtime_name: request.agent_definition.runtime_name(
                &request.fleet_id,
                &request.target_project_id,
                &request.request_id,
            )?,
            resource_id: resource.id(),
            role: request.role.clone(),
            requested_tokens: request.requested_tokens,
            state: EngagementState::Pending,
            cleanup: CleanupState::NotRequired,
            // Never the request's: `verify_request` refuses a request that
            // names any workspace setting.
            workspace_mode: "shared".into(),
            worktrees_dir: None,
            worktree_bootstrap: Vec::new(),
        };
        tx.execute("INSERT INTO projects(fleet_id,id,generation,room_id,owner_mxid,owner_room_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(fleet_id,id) DO NOTHING",
            params![request.fleet_id,request.target_project_id,proof.registration().generation,request.target_room_id,request.owner_mxid,request.owner_dm_room_id])?;
        // The request row itself is the domain inbox marker: same transaction and unique key.
        tx.execute("INSERT INTO engagements(id,fleet_id,generation,request_id,digest,context,evidence,project_id,name,resource_id,tokens,state,projection) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'pending',?12)",
            params![value.id,request.fleet_id,proof.registration().generation,request.request_id,digest,serialize(request)?,serialize(proof.audit())?,request.target_project_id,request.agent_definition.name.as_str(),resource.id(),u64::from(request.requested_tokens),serialize(&value)?])?;
        tx.commit()?;
        Ok(value)
    }
    /// Only an authenticated operator command calls this, after the Matrix adapter
    /// re-verifies current owner/room authority. Grants exactly the requested
    /// amount; `approve_allocating` is the console's amount-choosing form.
    pub fn approve(
        &mut self,
        command_id: &str,
        proof: &VerifiedRequest,
        now: u64,
    ) -> Result<Engagement, Error> {
        self.approve_allocating(command_id, proof, now, None)
    }
    /// ADR-186 §A: the approval grants `allocated` tokens instead of the
    /// request when the operator chose an amount. The amount is checked
    /// against exactly the headroom the plain approval checks, and an amount
    /// of `None` is the plain approval, byte for byte (same decision digest,
    /// no `allocated_tokens` written), so a replayed pre-ADR command id still
    /// matches its receipt.
    pub fn approve_allocating(
        &mut self,
        command_id: &str,
        proof: &VerifiedRequest,
        now: u64,
        allocated: Option<u64>,
    ) -> Result<Engagement, Error> {
        self.approve_allocating_inner(command_id, proof, now, allocated, None)
    }
    fn approve_allocating_inner(
        &mut self,
        command_id: &str,
        proof: &VerifiedRequest,
        now: u64,
        allocated: Option<u64>,
        coordinator: Option<&coordinator::AgentApproval>,
    ) -> Result<Engagement, Error> {
        let allocated = allocated
            .map(|value| {
                if value == 0 {
                    return Err(Error::from(InvalidInput(
                        "allocated tokens must be positive",
                    )));
                }
                Ok(Tokens::try_from(value)?)
            })
            .transpose()?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        authority(&tx, proof, now)?;
        project_authority(&tx, proof)?;
        let request = proof.request();
        if let Some(command) = coordinator {
            coordinator::check_agent(&tx, command, proof, now)?;
            if let Some(value) =
                coordinator::replay(&tx, command_id, &coordinator::command_digest(command)?)?
            {
                return Ok(value);
            }
        } else if coordinator::binding(&tx, &request.fleet_id)?.is_some() {
            // An old console endpoint cannot add a second verdict or bypass the
            // explicit coordinator once this engagement has migrated.
            return Err(Error::LocalAuthority);
        }
        let id = request.engagement_id()?;
        let (stored_digest, generation): (String, u64) = tx
            .query_row(
                "SELECT digest,generation FROM engagements WHERE id=?1",
                [&id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or(Error::NotFound)?;
        if stored_digest != request.digest()? {
            return Err(Error::Conflict);
        }
        if generation != proof.registration().generation {
            return Err(Error::Generation);
        }
        let digest = if let Some(command) = coordinator {
            coordinator::command_digest(command)?
        } else {
            match allocated {
                None => decision_digest("approve", &id)?,
                Some(amount) => canonical::digest(&json!(["approve", id, u64::from(amount)]))?,
            }
        };
        if let Some(value) = replay_decision(&tx, command_id, &digest)? {
            return Ok(value);
        }
        let mut value = read_engagement(&tx, &id)?;
        if value.state != EngagementState::Pending {
            return Err(Error::State);
        }
        let resource = read_resource(&tx, &value.resource_id)?;
        self.accounts.check_resource(&tx, &resource)?;
        if !resource.qualifies(&value.role)
            || !role_available(&tx, &value.role, Some(&request.fleet_id))?
        {
            return Err(Error::Unqualified);
        }
        let granted = u64::from(allocated.unwrap_or(value.requested_tokens));
        // The engagement being decided is still pending, so it holds nothing
        // yet and the headroom is exactly the retained decide() figure with
        // `excludeEngagementId: id`.
        check_grant_within(
            &tx,
            &resource,
            value.agent_name.as_str(),
            granted,
            now,
            coordinator.map(|c| c.request.resource_allocation_id.as_str()),
        )?;
        value.state = EngagementState::Reserved;
        value.project_name = proof.project_name().map(str::to_owned);
        value.allocated_tokens = allocated;
        write_engagement(&tx, &value)?;
        tx.execute(
            "UPDATE engagements SET preset_id=?2,seat_id=?3 WHERE id=?1",
            params![id, resource.preset_id, resource.seat_id],
        )?;
        let payload = json!({"request":request,"registrationGeneration":generation,"runtimeName":value.runtime_name,"resource":resource,"approvalEvidence":proof.audit()});
        tx.execute("INSERT INTO effects(id,engagement_id,kind,state,payload) VALUES(?1,?2,'provision','pending',?3)", params![format!("provision_{id}"),id,serialize(&payload)?])?;
        record_decision(
            &tx,
            command_id,
            &digest,
            &value,
            Some("engagement.approved"),
        )?;
        if let Some(command) = coordinator {
            coordinator::commit_agent(&tx, command, &value)?;
        }
        tx.commit()?;
        Ok(value)
    }
    /// ADR-186 §A3: what the engagement's resource can still give, as the
    /// smallest of the ceiling, seat and pool headroom — the very figure an
    /// approval is checked against, and what "All remaining" fills in. `None`
    /// when no limit is declared or the seat's period mismatches the pool's
    /// (approval refuses `no_ceiling` then): unknown, never a zero.
    pub fn engagement_headroom(&self, id: &str, at: u64) -> Result<Option<u64>, Error> {
        let engagement = read_engagement(&self.db, id)?;
        let resource = read_resource(&self.db, &engagement.resource_id)?;
        Ok(headroom(&self.db, &resource, at)?.remaining)
    }
    /// ADR-186 §C: raise an active (or still-provisioning) engagement's
    /// allocation by `add` tokens. Checked exactly like an approval: `add`
    /// must fit the smallest of ceiling, seat and pool headroom as it stands,
    /// with this engagement's current allocation already counted in it — so
    /// the new allocation fits the headroom left without it. Idempotent by
    /// command id through the decision receipts; a reused id with another
    /// amount conflicts. When the new allocation is above the spend, the
    /// quota hold lifts in the same transaction and the agent says
    /// "Resumed"; queued work is claimed on the next claim, no restart.
    pub fn raise_allocation(
        &mut self,
        command_id: &str,
        id: &str,
        add: u64,
        now: u64,
    ) -> Result<Engagement, Error> {
        project::identifier(id, 128)?;
        if add == 0 {
            return Err(InvalidInput("added tokens must be positive").into());
        }
        let add = Tokens::try_from(add)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = canonical::digest(&json!(["allocation", id, u64::from(add)]))?;
        if let Some(value) = replay_decision(&tx, command_id, &digest)? {
            return Ok(value);
        }
        let mut value = read_engagement(&tx, id)?;
        let fleet: String =
            tx.query_row("SELECT fleet_id FROM engagements WHERE id=?1", [id], |r| {
                r.get(0)
            })?;
        if coordinator::binding(&tx, &fleet)?.is_some() {
            return Err(Error::LocalAuthority);
        }
        if !matches!(
            value.state,
            EngagementState::Reserved | EngagementState::Active
        ) {
            return Err(Error::State);
        }
        let resource = read_resource(&tx, &value.resource_id)?;
        check_grant(
            &tx,
            &resource,
            value.agent_name.as_str(),
            u64::from(add),
            now,
        )?;
        let raised = u64::from(value.allocation())
            .checked_add(u64::from(add))
            .ok_or(InvalidInput("token count overflow"))?;
        value.allocated_tokens = Some(Tokens::try_from(raised)?);
        write_engagement(&tx, &value)?;
        quota_holds::lift(&tx, id, now)?;
        record_decision(
            &tx,
            command_id,
            &digest,
            &value,
            Some("engagement.allocation_raised"),
        )?;
        tx.commit()?;
        Ok(value)
    }
    /// ADR-186 §B: the engagement's allocation, its known spend (`None`
    /// while unknown) and whether it holds an open quota hold.
    pub fn quota_status(&self, id: &str) -> Result<QuotaStatus, Error> {
        let engagement = read_engagement(&self.db, id)?;
        Ok(QuotaStatus {
            allocated_tokens: u64::from(engagement.allocation()),
            spent_tokens: quota_holds::spend(&self.db, id)?,
            paused: quota_holds::paused(&self.db, id)?,
        })
    }
    pub fn reject(&mut self, command_id: &str, id: &str) -> Result<Engagement, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Like approve and top-up: once a coordinator decides this fleet's
        // requests, a console rejection would be a second verdict on them.
        let fleet: Option<String> = tx
            .query_row("SELECT fleet_id FROM engagements WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(fleet) = fleet
            && coordinator::binding(&tx, &fleet)?.is_some()
        {
            return Err(Error::LocalAuthority);
        }
        let value = end_in_transaction(&tx, command_id, id, false)?;
        tx.commit()?;
        Ok(value)
    }
    pub fn revoke(&mut self, command_id: &str, id: &str) -> Result<Engagement, Error> {
        self.end(command_id, id, true)
    }
    /// A definitive retirement failure can be retried explicitly. An uncertain
    /// retirement must first be inspected via observe_effect, never blindly replayed.
    pub fn retry_cleanup(&mut self, command_id: &str, id: &str) -> Result<Engagement, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let digest = decision_digest("retry_cleanup", id)?;
        if let Some(value) = replay_decision(&tx, command_id, &digest)? {
            return Ok(value);
        }
        let value = read_engagement(&tx, id)?;
        if value.state != EngagementState::Revoked {
            return Err(Error::State);
        }
        let changed=tx.execute("UPDATE effects SET state='pending',outcome_digest=NULL WHERE engagement_id=?1 AND kind='retire' AND state='failed'",[id])?;
        if changed != 1 {
            return Err(Error::State);
        }
        record_decision(&tx, command_id, &digest, &value, None)?;
        tx.commit()?;
        Ok(value)
    }
    fn end(&mut self, command_id: &str, id: &str, revoke: bool) -> Result<Engagement, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = end_in_transaction(&tx, command_id, id, revoke)?;
        tx.commit()?;
        Ok(value)
    }
    pub fn effect(&self, id: &str) -> Result<Effect, Error> {
        read_effect(&self.db, id)
    }
    pub fn claim_effect(&mut self) -> Result<Option<Effect>, Error> {
        self.claim_matching_effect(None)
    }
    /// Host-only inline claim. Selecting first and filtering the returned ID
    /// afterwards would leave an unrelated engagement Started on mismatch.
    pub fn claim_effect_for(&mut self, id: &str) -> Result<Option<Effect>, Error> {
        project::identifier(id, 128)?;
        self.claim_matching_effect(Some(id))
    }
    /// The engagements of `fleet_id` approved but never provisioned: a pending
    /// provision effect on a reserved engagement of the current registration
    /// generation — exactly what `claim_matching_effect` would claim. A console
    /// approval leaves these behind (TS provisioned on the operator's verdict,
    /// backend-v2.js fulfillment); the host claims them on its next turn.
    pub fn pending_provisions(&self, fleet_id: &str) -> Result<Vec<String>, Error> {
        project::identifier(fleet_id, 128)?;
        let mut statement = self.db.prepare(
            "SELECT e.id FROM effects f JOIN engagements e ON e.id=f.engagement_id JOIN registrations r ON r.fleet_id=e.fleet_id WHERE e.fleet_id=?1 AND f.kind='provision' AND f.state='pending' AND e.state='reserved' AND e.generation=r.generation ORDER BY f.id LIMIT 16",
        )?;
        let rows = statement.query_map([fleet_id], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
    /// Candidates only; encrypted Matrix custody decides whether any can resume.
    pub fn uncertain_provisions(&self, fleet_id: &str) -> Result<Vec<String>, Error> {
        project::identifier(fleet_id, 128)?;
        let mut statement = self.db.prepare(
            "SELECT e.id FROM effects f JOIN engagements e ON e.id=f.engagement_id JOIN registrations r ON r.fleet_id=e.fleet_id WHERE e.fleet_id=?1 AND f.kind='provision' AND f.state='uncertain' AND e.state='reserved' AND e.generation=r.generation ORDER BY f.id LIMIT 16")?;
        Ok(statement
            .query_map([fleet_id], |r| r.get(0))?
            .collect::<Result<_, _>>()?)
    }
    /// Revoked engagements of one fleet whose retirement is still pending and
    /// whose agent never published a Matrix transport (read-only). No worker
    /// will ever run for them; the provisioning host settles each one whose
    /// credential was never stored (TS `lib/matrix-work-executor.js`: a logout
    /// with no stored credential is already done).
    /// ADR-187: the owner the engagement's request named (read-only).
    pub fn engagement_owner(&self, id: &str) -> Result<Option<String>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT json_extract(context,'$.ownerMxid') FROM engagements WHERE id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    /// ADR-187: the owner and their private approval room, as the engagement's
    /// request named them (read-only).
    pub fn engagement_owner_room(&self, id: &str) -> Result<Option<(String, String)>, Error> {
        Ok(self
            .db
            .query_row(
                "SELECT json_extract(context,'$.ownerMxid'),json_extract(context,'$.ownerDmRoomId') FROM engagements WHERE id=?1",
                [id],
                |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()?
            .and_then(|(owner, room)| owner.zip(room)))
    }
    /// ADR-187 §C: an owner's pinned anchor, if any (read-only).
    pub fn owner_anchor(&self, owner: &str) -> Result<Option<owner_anchors::OwnerAnchor>, Error> {
        owner_anchors::get(&self.db, owner)
    }
    pub fn owner_anchors(&self) -> Result<Vec<owner_anchors::OwnerAnchor>, Error> {
        owner_anchors::list(&self.db)
    }
    /// ADR-187 §C: the key the homeserver reports now, against the pin.
    pub fn observe_owner_anchor(
        &mut self,
        owner: &str,
        key: &str,
        now: u64,
    ) -> Result<owner_anchors::OwnerAnchor, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = owner_anchors::observe(&tx, owner, key, now);
        // A refused key still records the mismatch for the operator.
        tx.commit()?;
        result
    }
    /// ADR-188: the engagement's joined rooms that are not retired.
    pub fn joined_rooms(&self, engagement: &str) -> Result<Vec<joined_rooms::JoinedRoom>, Error> {
        joined_rooms::live(&self.db, engagement)
    }
    pub fn joined_room(
        &self,
        engagement: &str,
        room: &str,
    ) -> Result<Option<joined_rooms::JoinedRoom>, Error> {
        joined_rooms::get(&self.db, engagement, room)
    }
    /// ADR-188 §2: the agent joined `room` by invitation.
    pub fn record_joined_room(
        &mut self,
        engagement: &str,
        room: &str,
        now: u64,
    ) -> Result<joined_rooms::JoinedRoom, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = joined_rooms::record(&tx, engagement, room, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn set_joined_room_state(
        &mut self,
        engagement: &str,
        room: &str,
        state: joined_rooms::JoinedRoomState,
        now: u64,
    ) -> Result<joined_rooms::JoinedRoom, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = joined_rooms::set_state(&tx, engagement, room, state, now)?;
        tx.commit()?;
        Ok(result)
    }
    /// ADR-188 §3: a repeat of the notice, at most once per gap.
    pub fn claim_joined_room_renotice(
        &mut self,
        engagement: &str,
        room: &str,
        now: u64,
        not_before: u64,
    ) -> Result<bool, Error> {
        joined_rooms::claim_renotice(&self.db, engagement, room, now, not_before)
    }
    /// ADR-188 §3: true only for the first caller; that caller posts the notice.
    pub fn claim_joined_room_notice(
        &mut self,
        engagement: &str,
        room: &str,
        now: u64,
    ) -> Result<bool, Error> {
        joined_rooms::claim_notice(&self.db, engagement, room, now)
    }
    /// ADR-187 §C: the operator's explicit re-pin.
    pub fn repin_owner_anchor(
        &mut self,
        owner: &str,
        key: &str,
        now: u64,
    ) -> Result<owner_anchors::OwnerAnchor, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let result = owner_anchors::repin(&tx, owner, key, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn pending_unattached_retirements(&self, fleet_id: &str) -> Result<Vec<String>, Error> {
        self.pending_retirements(fleet_id, true)
    }
    /// Appservice identity cleanup also survives a lost live worker or restart.
    /// Completing it never settles runner custody or refunds unknown usage.
    pub fn pending_identity_retirements(&self, fleet_id: &str) -> Result<Vec<String>, Error> {
        self.pending_retirements(fleet_id, false)
    }
    fn pending_retirements(&self, fleet_id: &str, unattached: bool) -> Result<Vec<String>, Error> {
        project::identifier(fleet_id, 128)?;
        let mut statement = self.db.prepare(
            "SELECT e.id FROM effects f JOIN engagements e ON e.id=f.engagement_id JOIN registrations r ON r.fleet_id=e.fleet_id WHERE e.fleet_id=?1 AND f.kind='retire' AND (f.state='pending' OR (?2=0 AND f.state='uncertain')) AND e.state='revoked' AND e.generation=r.generation AND (?2=0 OR NOT EXISTS(SELECT 1 FROM matrix_transports t WHERE t.engagement_id=e.id)) ORDER BY f.id LIMIT 16",
        )?;
        let rows = statement.query_map(params![fleet_id, unattached], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
    /// Inspect the original uncertain retirement for an idempotent remote
    /// identity check. This cannot rearm provisioning, execution or local IO.
    pub fn inspect_retirement_effect(&self, id: &str) -> Result<Option<Effect>, Error> {
        project::identifier(id, 128)?;
        let effect: Option<String> = self.db.query_row("SELECT f.id FROM effects f JOIN engagements e ON e.id=f.engagement_id JOIN registrations r ON r.fleet_id=e.fleet_id WHERE e.id=?1 AND e.state='revoked' AND f.kind='retire' AND f.state='uncertain' AND e.generation=r.generation",[id],|r|r.get(0)).optional()?;
        effect.map(|id| read_effect(&self.db, &id)).transpose()
    }
    fn claim_matching_effect(&mut self, expected: Option<&str>) -> Result<Option<Effect>, Error> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id: Option<String> = tx.query_row("SELECT f.id FROM effects f JOIN engagements e ON e.id=f.engagement_id JOIN registrations r ON r.fleet_id=e.fleet_id WHERE (?1 IS NULL OR f.id=?1) AND f.state='pending' AND e.generation=r.generation AND ((f.kind='provision' AND e.state='reserved') OR (f.kind='retire' AND e.state='revoked')) ORDER BY f.id LIMIT 1", [expected], |r| r.get(0)).optional()?;
        let Some(id) = id else {
            return Ok(None);
        };
        tx.execute(
            "UPDATE effects SET state='started',fence=fence+1 WHERE id=?1 AND fence<?2",
            params![id, JSON_SAFE_MAX],
        )?;
        let effect = read_effect(&tx, &id)?;
        if effect.state != EffectState::Started {
            return Err(Error::State);
        }
        tx.commit()?;
        Ok(Some(effect))
    }
    pub fn observe_effect(
        &mut self,
        id: &str,
        fence: u64,
        outcome: &EffectOutcome,
    ) -> Result<Engagement, Error> {
        match outcome {
            EffectOutcome::Applied { receipt } | EffectOutcome::NotApplied { receipt }
                if receipt.is_empty()
                    || receipt.len() > 2048
                    || receipt.chars().any(char::is_control) =>
            {
                return Err(InvalidInput("observed effect receipt required").into());
            }
            _ => {}
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let value = observe_effect_transaction(&tx, id, fence, outcome)?;
        tx.commit()?;
        Ok(value)
    }
}
// Shared effect kernel: scoped factory activation and ordinary observations
// differ in admission only, never in their durable transition implementation.
fn observe_effect_transaction(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    fence: u64,
    outcome: &EffectOutcome,
) -> Result<Engagement, Error> {
    let effect = read_effect(tx, id)?;
    let digest = canonical::digest(&serde_json::to_value(outcome)?)?;
    if effect.fence != fence {
        return Err(Error::Generation);
    }
    let old: Option<String> = tx.query_row(
        "SELECT outcome_digest FROM effects WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if old.as_ref() == Some(&digest) {
        return read_engagement(tx, &effect.engagement_id);
    }
    if !matches!(effect.state, EffectState::Started | EffectState::Uncertain) {
        return Err(Error::State);
    }
    let mut value = read_engagement(tx, &effect.engagement_id)?;
    let current: bool = tx.query_row("SELECT e.generation=r.generation FROM engagements e JOIN registrations r ON r.fleet_id=e.fleet_id WHERE e.id=?1", [&value.id], |r| r.get(0))?;
    if !current {
        return Err(Error::Generation);
    }
    let state = match outcome {
        EffectOutcome::Applied { .. } => {
            if effect.kind == "provision" {
                exec_policy::inherit_resource_policy(tx, &value.id, &value.resource_id)?;
                value.state = EngagementState::Active;
            } else {
                value.cleanup = CleanupState::Complete;
            }
            "complete"
        }
        EffectOutcome::NotApplied { .. } => {
            if effect.kind == "provision" {
                value.state = EngagementState::Failed;
                // ADR-095 Slice 6: first terminal transition records the
                // ended-at instant (advisory metadata, side table).
                tx.execute(
                    "INSERT INTO engagement_ends(engagement_id,ended_at) VALUES(?1,?2) \
                         ON CONFLICT(engagement_id) DO NOTHING",
                    params![value.id, graphs::now_ms()?],
                )?;
                "failed"
            } else {
                value.cleanup = CleanupState::Pending;
                "failed"
            }
        }
        EffectOutcome::Unknown => {
            if effect.kind == "retire" {
                value.cleanup = CleanupState::Uncertain;
            }
            "uncertain"
        }
    };
    tx.execute(
        "UPDATE effects SET state=?2,outcome_digest=?3 WHERE id=?1",
        params![id, state, digest],
    )?;
    write_engagement(tx, &value)?;
    Ok(value)
}
pub(super) fn end_in_transaction(
    tx: &Transaction<'_>,
    command_id: &str,
    id: &str,
    revoke: bool,
) -> Result<Engagement, Error> {
    let digest = decision_digest(if revoke { "revoke" } else { "reject" }, id)?;
    if let Some(value) = replay_decision(tx, command_id, &digest)? {
        return Ok(value);
    }
    let mut value = read_engagement(tx, id)?;
    if !matches!(
        value.state,
        EngagementState::Pending | EngagementState::Reserved | EngagementState::Active
    ) || !revoke && value.state != EngagementState::Pending
    {
        return Err(Error::State);
    }
    let effect: Option<(String, String)> = tx
        .query_row(
            "SELECT state,payload FROM effects WHERE engagement_id=?1 AND kind='provision'",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((state, payload)) = effect {
        tx.execute("UPDATE effects SET state='cancelled',fence=fence+1 WHERE engagement_id=?1 AND kind='provision'", [id])?;
        if state != "pending" {
            value.cleanup = CleanupState::Pending;
            tx.execute("INSERT INTO effects(id,engagement_id,kind,state,payload) VALUES(?1,?2,'retire','pending',?3)", params![format!("retire_{id}"),id,payload])?;
        }
    }
    value.state = if revoke {
        EngagementState::Revoked
    } else {
        EngagementState::Rejected
    };
    write_engagement(tx, &value)?;
    // ADR-095 Slice 6: the ended-at instant is advisory metadata on the
    // side table (never a column here), read by the engagements phase's
    // receipt payload. First terminal transition wins.
    tx.execute(
        "INSERT INTO engagement_ends(engagement_id,ended_at) VALUES(?1,?2) \
             ON CONFLICT(engagement_id) DO NOTHING",
        params![value.id, graphs::now_ms()?],
    )?;
    graphs::reconcile(tx, graphs::now_ms()?)?;
    matrix_routes::reconcile(tx, graphs::now_ms()?)?;
    record_decision(
        tx,
        command_id,
        &digest,
        &value,
        Some(if revoke {
            "engagement.revoked"
        } else {
            "engagement.rejected"
        }),
    )?;
    Ok(value)
}
fn read_effect(db: &Connection, id: &str) -> Result<Effect, Error> {
    let row: (String, String, String, u64, String) = db
        .query_row(
            "SELECT engagement_id,kind,state,fence,payload FROM effects WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?
        .ok_or(Error::NotFound)?;
    Ok(Effect {
        id: id.into(),
        engagement_id: row.0,
        kind: row.1,
        state: serde_json::from_value(Value::String(row.2))?,
        fence: row.3,
        payload: serde_json::from_str(&row.4)?,
    })
}

pub use approvals::responses::{
    ApprovalResponseGrant, ApprovalResponseObservation, ApprovalResponseState,
    ApprovalResponseSummary,
};

pub use approvals::owned::{OwnedApprovalScope, OwnedApprovalStatus};

/// Validation shared by operator edits and concrete console commands inside their
/// original transaction. No write occurs until the caller rechecks its authority.
fn prepare_resource_write(
    tx: &Transaction<'_>,
    resource: &Resource,
    publication: Option<bool>,
    create_only: bool,
) -> Result<Resource, Error> {
    resource.validate()?;
    accounts::association(tx, resource)?;
    let mut resource = resource.clone();
    bounded_row(tx, "resources", "id", &resource.id(), 2048)?;
    let previous = match read_resource(tx, &resource.id()) {
        Ok(old) => Some(old),
        Err(Error::NotFound) => None,
        Err(error) => return Err(error),
    };
    if create_only && previous.is_some() {
        return Err(Error::Conflict);
    }
    resource.published = publication
        .or_else(|| previous.as_ref().map(|r| r.published))
        .unwrap_or(true);
    if let Some(old) = previous
        && (old.seat_id != resource.seat_id
            || old.framework != resource.framework
            || old.model != resource.model
            || old.provider != resource.provider
            || old.reasoning != resource.reasoning
            || old.octos_profile != resource.octos_profile)
    {
        let count:i64=tx.query_row("SELECT COUNT(*) FROM engagements WHERE resource_id=?1 AND state IN ('reserved','active')",[resource.id()],|r|r.get(0))?;
        if count != 0 {
            return Err(Error::State);
        }
    }
    resource.roles = resource.eligible_roles();
    Ok(resource)
}
fn write_resource_configuration(
    tx: &Transaction<'_>,
    resource: &Resource,
    create_only: bool,
) -> Result<(), Error> {
    if create_only {
        tx.execute(
            "INSERT INTO resources(id,preset_id,config) VALUES(?1,?2,?3)",
            params![resource.id(), resource.preset_id, serialize(resource)?],
        )?;
    } else {
        tx.execute("INSERT INTO resources(id,preset_id,config) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET config=excluded.config", params![resource.id(),resource.preset_id,serialize(resource)?])?;
    }
    Ok(())
}

#[cfg(test)]
mod legacy_writer_tests;
