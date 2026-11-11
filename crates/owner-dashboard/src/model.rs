//! Dashboard state machine and presentation, independent of Makepad.
//!
//! The widget is a thin renderer over [`DashboardState`]. All decisions about
//! which data is current, which command may be sent, and how unknown, stale,
//! partial, uncertain and failed states read live here so they are testable
//! without a window.
use crate::format::{self, Clock};
use crate::i18n::{Locale, Msg, phrase, tr};
use hagency_metering::TokenCounts;
use hagency_metering::dashboard::{
    AccountState, ActivityState, DASHBOARD_API_VERSION, DashboardSnapshot, HostingState, ProviderAccount,
    ResourceState, ResourceUsage, period::UsagePeriod,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// A snapshot older than this (by its own `observed_at_ms`) reads as stale.
pub const STALE_AFTER_MS: u64 = 5 * 60_000;
/// Plan windows at or above this utilization are called out.
pub const NEAR_LIMIT_PERCENT: f64 = 90.0;
const CAUTION_PERCENT: f64 = 70.0;
const MAX_DAEMON_TEXT: usize = 240;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tab {
    #[default]
    Overview,
    Resources,
    Usage,
    Activity,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Overview, Tab::Resources, Tab::Usage, Tab::Activity];
    pub fn label(self, locale: Locale) -> &'static str {
        tr(locale, match self {
            Tab::Overview => Msg::TabOverview,
            Tab::Resources => Msg::TabResources,
            Tab::Usage => Msg::TabUsage,
            Tab::Activity => Msg::TabActivity,
        })
    }
}

/// Why hosting must be set up on this computer before anything runs for the
/// signed-in owner (ADR-192). The host decides it from durable state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupNeed {
    /// Hosting has not been set up for this account on this computer.
    NotSetUp,
    /// An earlier hosting sign-in names another owner root. Setting up again
    /// signs it out and hosts the account's one registered root.
    Replace,
    /// The hosting service on this computer serves another account. One user
    /// of a computer can host one account's resource pool at a time.
    OtherOwner,
}

/// Progress of the host's setup job.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SetupProgress {
    #[default]
    Idle,
    /// Registering the owner root and clearing an earlier sign-in.
    Preparing,
    /// The owner signs this computer in through the browser. Cancellable.
    SigningIn,
    /// Browser sign-in completed; accepted native registration is in progress.
    Registering,
    /// Cancellation was requested; the helper finishes it safely first.
    Cancelling,
    /// Installing and starting the background service.
    Enabling,
    /// The service is installed; waiting for it to report hosting.
    Confirming,
    /// A static failure code, presented by [`crate::i18n::phrase::setup_failure`].
    Failed { code: String },
}

/// The setup page shown instead of the dashboard while `need` applies, or
/// while the host's setup job is still running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupView {
    pub need: SetupNeed,
    pub progress: SetupProgress,
}

impl SetupView {
    pub fn running(&self) -> bool {
        matches!(self.progress, SetupProgress::Preparing | SetupProgress::SigningIn | SetupProgress::Registering | SetupProgress::Cancelling
            | SetupProgress::Enabling | SetupProgress::Confirming)
    }
}

/// Host controls shown with a hosted owner's dashboard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostOptions {
    /// Keep the resource pool running after the host app quits.
    pub keep_running: bool,
    /// A service installation or removal is in progress.
    pub busy: bool,
    /// The installed service runs another program than this app (for
    /// example an earlier separate Hosting app). Reinstalling moves it here.
    pub foreign_service: bool,
}

/// The host's view of its daemon connection. Reported by the host; the
/// widget never infers readiness from the presence of data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConnectionStatus {
    UnsupportedPlatform,
    #[default]
    Connecting,
    Connected,
    /// No daemon socket, or the daemon refused because nothing is enrolled.
    DaemonUnavailable,
    /// The daemon speaks another API version (or none we understand).
    Incompatible { daemon_version: Option<u32> },
    /// The daemon reports that the owner's sign-in must be renewed.
    AuthExpired,
    /// The daemon hosts a different owner than this host's account.
    OwnerMismatch,
    /// Transport or protocol failure. Message is shown bounded.
    Failed { message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    Pause,
    Resume,
    Stop,
}

impl CommandKind {
    pub fn label(self, locale: Locale) -> &'static str {
        tr(locale, match self {
            CommandKind::Pause => Msg::PauseHosting,
            CommandKind::Resume => Msg::ResumeHosting,
            CommandKind::Stop => Msg::StopHosting,
        })
    }
    fn reached(self, state: HostingState) -> bool {
        match self {
            CommandKind::Pause => state == HostingState::Paused,
            CommandKind::Resume => matches!(state, HostingState::Running),
            CommandKind::Stop => state == HostingState::Stopped,
        }
    }
}

/// Identity of one command attempt. Retrying an uncertain outcome reuses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandTicket {
    pub request_id: String,
    pub expected_revision: Option<u64>,
}

/// Result of a command as reported by the host's connector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandResult {
    /// The daemon accepted the command; the effect is confirmed by a later snapshot.
    Accepted,
    /// The daemon confirmed the effect.
    Completed,
    /// The daemon confirmed nothing needed to change.
    Unchanged,
    /// Definitive refusal. `retryable` allows a new attempt with a new ID.
    Rejected { code: String, message: String, retryable: bool },
    /// This attempt did not reach the daemon. It says nothing about an earlier
    /// attempt with the same ID, so Checking must remain uncertain.
    NotSubmitted { code: String, retryable: bool },
    /// No definitive answer (timeout, lost response, `outcome_unknown`).
    Uncertain { message: String },
}

/// These failures are produced only after the daemon's durable receipt lookup
/// under its mutation lock. Unlike busy/scope/transport refusals, they prove
/// the original hosting command has no applied receipt and did not execute.
pub(crate) fn rejection_settles_original(code: &str) -> bool {
    matches!(code, "revision_conflict" | "hosting_stopped")
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CommandPhase {
    Submitting,
    Accepted,
    Confirmed { at_ms: u64 },
    Superseded,
    Unchanged,
    Rejected { code: String, retryable: bool },
    Uncertain,
    Checking,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PendingCommand {
    pub kind: CommandKind,
    pub ticket: CommandTicket,
    pub phase: CommandPhase,
}

impl PendingCommand {
    /// Only a definitive acceptance can settle from a later authoritative
    /// snapshot. Unknown outcomes always retain their original request ID.
    pub fn observe(&mut self, snapshot: &DashboardSnapshot) {
        if self.phase != CommandPhase::Accepted { return; }
        if self.kind.reached(snapshot.hosting.state) {
            self.phase = CommandPhase::Confirmed { at_ms: snapshot.observed_at_ms };
        } else if self.ticket.expected_revision.is_some_and(|revision| snapshot.revision > revision)
            && !matches!(snapshot.hosting.state, HostingState::Starting | HostingState::Draining)
        {
            self.phase = CommandPhase::Superseded;
        }
    }

    /// True while the outcome is not definitively known; new hosting
    /// commands are blocked so an uncertain one is never duplicated.
    pub fn in_flight(&self) -> bool {
        matches!(
            self.phase,
            CommandPhase::Submitting | CommandPhase::Accepted | CommandPhase::Uncertain | CommandPhase::Checking
        )
    }
}

/// Scoped, credential-free UI state (navigation, filters and paging).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ViewState {
    pub version: u32,
    pub owner_id: Option<String>,
    pub tab: Tab,
    pub selected_resource: Option<String>,
    pub account_filter: Option<String>,
    pub resource_filter: Option<String>,
    pub model_filter: Option<String>,
    pub requester_filter: Option<String>,
    pub filters_open: bool,
    pub date_start: String,
    pub date_end: String,
    pub period: Option<UsagePeriod>,
    pub scroll_y: f64,
    pub pages: std::collections::BTreeMap<String, usize>,
}

impl ViewState {
    pub fn usage_filter(&self) -> crate::dto::UsageFilter {
        crate::dto::UsageFilter { account_id: self.account_filter.clone(), resource_id: self.resource_filter.clone(),
            model: self.model_filter.clone(), requester_id: self.requester_filter.clone() }
    }
}

/// Semantic actions emitted by the widget. The host performs all IO.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum DashboardAction {
    #[default]
    None,
    /// Fetch a new snapshot (for `period` if set).
    Refresh { period: Option<UsagePeriod> },
    /// Ask the daemon to refresh provider observations (rate-limit aware).
    RefreshProviders,
    PauseHosting(CommandTicket),
    ResumeHosting(CommandTicket),
    StopHosting(CommandTicket),
    /// Re-send the original command ID to look up an uncertain outcome.
    CheckCommand { kind: CommandKind, ticket: CommandTicket },
    /// Set up hosting for this owner (or retry it), including a renewed sign-in.
    OpenSetup,
    /// Stop waiting for the browser sign-in of a running setup.
    CancelSetup,
    /// Install and start the background service of an enrolled owner.
    StartService,
    /// Keep the resource pool running after the host app quits.
    KeepRunning(bool),
    /// Remove the background service; its sign-in and resources are kept.
    UninstallService,
    /// Open the owner's shared tool approvals and resource Agent requests.
    ReviewApprovals,
    /// Read-only navigation notifications for hosts that mirror location.
    Navigated(ViewState),
}

/// Visual tone. Always paired with a glyph and text; never color alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tone {
    Positive,
    Caution,
    Critical,
    Neutral,
    Progress,
    Accent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Glyph {
    Check,
    Pause,
    Stop,
    Warning,
    Cross,
    Clock,
    Info,
    Question,
    Offline,
    Setup,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub tone: Tone,
    pub glyph: Glyph,
    pub text: String,
}

impl Status {
    pub fn new(tone: Tone, glyph: Glyph, text: impl Into<String>) -> Self {
        Self { tone, glyph, text: text.into() }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AttentionTarget { Activity, Approvals, Account(String), Usage, Resources, Command }
#[derive(Clone, Debug, PartialEq)]
pub struct AttentionItem {
    pub key: String,
    pub status: Status,
    pub target: AttentionTarget,
}
impl std::ops::Deref for AttentionItem {
    type Target = Status;
    fn deref(&self) -> &Status { &self.status }
}

fn next_request_id(now_ms: u64) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    // [A-Za-z0-9_-]{1,64}, unique per process run and wall clock.
    format!("odui-{now_ms:x}-{:x}-{n:x}", std::process::id())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotOutcome {
    Applied,
    /// Older than the displayed revision (late or reordered reply).
    Stale,
    /// Belongs to a different owner than this view's scope.
    WrongOwner,
    /// Version mismatch; nothing applied.
    Incompatible,
}

/// The complete dashboard state.
#[derive(Clone, Debug, Default)]
pub struct DashboardState {
    pub snapshot: Option<DashboardSnapshot>,
    /// Period the displayed snapshot was requested for, when the host knows it.
    pub snapshot_period: Option<UsagePeriod>,
    pub connection: ConnectionStatus,
    pub refreshing: bool,
    pub owner_scope: Option<String>,
    pub command: Option<PendingCommand>,
    pub confirm_stop: bool,
    pub view: ViewState,
    pub locale: Locale,
    /// `Some` while hosting must be set up (or setup is running): the setup
    /// page replaces the dashboard.
    pub setup: Option<SetupView>,
    /// Host controls for a hosted owner, when the host offers them.
    pub options: Option<HostOptions>,
    pub confirm_uninstall: bool,
    /// Deterministic request IDs for tests and fixtures.
    pub request_ids: Option<Vec<String>>,
}

impl DashboardState {
    pub fn new(locale: Locale) -> Self {
        Self { locale, view: ViewState { version: 1, ..Default::default() }, ..Default::default() }
    }

    fn new_ticket(&mut self, now_ms: u64) -> CommandTicket {
        let request_id = match self.request_ids.as_mut() {
            Some(ids) if !ids.is_empty() => ids.remove(0),
            _ => next_request_id(now_ms),
        };
        CommandTicket { request_id, expected_revision: self.snapshot.as_ref().map(|s| s.revision) }
    }

    /// Apply a snapshot. Older revisions and other owners are ignored, so a
    /// late reply can never replace newer data or another account's view.
    pub fn apply_snapshot(&mut self, snapshot: DashboardSnapshot, period: Option<UsagePeriod>) -> SnapshotOutcome {
        if !snapshot.is_compatible() {
            self.connection = ConnectionStatus::Incompatible { daemon_version: Some(snapshot.version) };
            self.refreshing = false;
            return SnapshotOutcome::Incompatible;
        }
        if let (Some(scope), Some(owner)) = (&self.owner_scope, &snapshot.owner) {
            if &owner.user_id != scope {
                self.refreshing = false;
                return SnapshotOutcome::WrongOwner;
            }
        }
        if let Some(current) = &self.snapshot {
            let same_owner = current.owner.as_ref().map(|o| &o.user_id) == snapshot.owner.as_ref().map(|o| &o.user_id);
            if same_owner && snapshot.revision < current.revision {
                self.refreshing = false;
                return SnapshotOutcome::Stale;
            }
        }
        let state = snapshot.hosting.state;
        if let Some(command) = self.command.as_mut() {
            command.observe(&snapshot);
        }
        if self.confirm_stop && !self.can(CommandKind::Stop, Some(state)) {
            self.confirm_stop = false;
        }
        self.snapshot = Some(snapshot);
        self.snapshot_period = period;
        self.connection = ConnectionStatus::Connected;
        self.refreshing = false;
        SnapshotOutcome::Applied
    }

    pub fn set_connection(&mut self, status: ConnectionStatus) {
        if status != ConnectionStatus::Connected {
            self.refreshing = false;
            self.confirm_stop = false;
        }
        self.connection = status;
    }

    pub fn connection_failed(&mut self, message: &str) {
        self.set_connection(ConnectionStatus::Failed { message: format::bounded(message, MAX_DAEMON_TEXT) });
    }

    /// Switching owner clears all private view state and in-flight commands.
    /// Returns true when the scope changed.
    pub fn set_owner_scope(&mut self, owner: Option<&str>) -> bool {
        if self.owner_scope.as_deref() == owner {
            return false;
        }
        self.owner_scope = owner.map(str::to_owned);
        let keep_tab = self.view.tab;
        self.snapshot = None;
        self.snapshot_period = None;
        self.command = None;
        self.confirm_stop = false;
        self.refreshing = false;
        self.connection = ConnectionStatus::Connecting;
        self.setup = None;
        self.options = None;
        self.confirm_uninstall = false;
        self.view = ViewState { version: 1, owner_id: self.owner_scope.clone(), tab: keep_tab, ..Default::default() };
        true
    }

    /// Show the setup page (`Some`) or the dashboard (`None`).
    pub fn set_setup(&mut self, setup: Option<SetupView>) -> bool {
        if self.setup == setup {
            return false;
        }
        if setup.is_some() {
            self.confirm_stop = false;
            self.confirm_uninstall = false;
        }
        self.setup = setup;
        true
    }

    pub fn set_options(&mut self, options: Option<HostOptions>) -> bool {
        if self.options == options {
            return false;
        }
        if options.as_ref().is_none_or(|o| o.busy) {
            self.confirm_uninstall = false;
        }
        self.options = options;
        true
    }

    /// Ask for confirmation before removing the background service.
    pub fn request_uninstall(&mut self) -> bool {
        if self.options.as_ref().is_none_or(|o| o.busy) || self.setup.is_some() {
            return false;
        }
        self.confirm_stop = false;
        self.confirm_uninstall = true;
        true
    }

    pub fn cancel_uninstall(&mut self) {
        self.confirm_uninstall = false;
    }

    /// The confirmed removal, or `None` when it is no longer allowed.
    pub fn confirm_uninstall(&mut self) -> Option<DashboardAction> {
        let allowed = self.confirm_uninstall && self.options.as_ref().is_some_and(|o| !o.busy) && self.setup.is_none();
        self.confirm_uninstall = false;
        allowed.then_some(DashboardAction::UninstallService)
    }

    pub fn recover_command(&mut self, mut command: PendingCommand) {
        if self.command.as_ref().is_some_and(|current| current.in_flight()
            && !(current.ticket == command.ticket && current.kind == command.kind && command.phase == CommandPhase::Accepted)) { return; }
        if matches!(command.phase, CommandPhase::Submitting | CommandPhase::Checking) {
            command.phase = CommandPhase::Uncertain;
        }
        if let Some(snapshot) = &self.snapshot { command.observe(snapshot); }
        self.command = Some(command);
    }

    pub fn begin_refresh(&mut self) -> DashboardAction {
        self.refreshing = true;
        DashboardAction::Refresh { period: self.view.period }
    }

    fn connected(&self) -> bool {
        self.connection == ConnectionStatus::Connected && self.snapshot.is_some()
    }

    /// Whether `kind` may be requested now.
    pub fn can(&self, kind: CommandKind, state: Option<HostingState>) -> bool {
        let state = state.or(self.snapshot.as_ref().map(|s| s.hosting.state));
        let Some(state) = state else { return false };
        if !self.connected() || self.command.as_ref().is_some_and(PendingCommand::in_flight) {
            return false;
        }
        match kind {
            CommandKind::Pause => matches!(state, HostingState::Running | HostingState::Starting),
            CommandKind::Resume => matches!(state, HostingState::Paused | HostingState::Draining | HostingState::Stopped | HostingState::Failed | HostingState::Offline),
            CommandKind::Stop => matches!(
                state,
                HostingState::Running | HostingState::Starting | HostingState::Paused | HostingState::Draining
            ),
        }
    }

    /// Request a hosting command. Stop first asks for confirmation.
    pub fn request(&mut self, kind: CommandKind, now_ms: u64) -> Option<DashboardAction> {
        if !self.can(kind, None) {
            return None;
        }
        if kind == CommandKind::Stop && !self.confirm_stop {
            self.confirm_stop = true;
            return None;
        }
        self.confirm_stop = false;
        let ticket = self.new_ticket(now_ms);
        self.command = Some(PendingCommand { kind, ticket: ticket.clone(), phase: CommandPhase::Submitting });
        Some(match kind {
            CommandKind::Pause => DashboardAction::PauseHosting(ticket),
            CommandKind::Resume => DashboardAction::ResumeHosting(ticket),
            CommandKind::Stop => DashboardAction::StopHosting(ticket),
        })
    }

    pub fn cancel_stop(&mut self) {
        self.confirm_stop = false;
    }

    /// Re-send the same request ID after an uncertain outcome.
    pub fn check_command(&mut self) -> Option<DashboardAction> {
        let command = self.command.as_mut()?;
        if command.phase != CommandPhase::Uncertain {
            return None;
        }
        command.phase = CommandPhase::Checking;
        Some(DashboardAction::CheckCommand { kind: command.kind, ticket: command.ticket.clone() })
    }

    /// Retry a definitively rejected, retryable command with a new ID.
    pub fn retry_rejected(&mut self, now_ms: u64) -> Option<DashboardAction> {
        let command = self.command.as_ref()?;
        let CommandPhase::Rejected { retryable: true, .. } = command.phase else { return None };
        let kind = command.kind;
        self.command = None;
        self.confirm_stop = kind == CommandKind::Stop;
        self.request(kind, now_ms)
    }

    pub fn dismiss_command(&mut self) {
        if self.command.as_ref().is_some_and(|c| !c.in_flight()) {
            self.command = None;
        }
    }

    /// Record the host's command outcome. Results for other request IDs
    /// (late replies, another controller's commands) are ignored.
    /// Returns a follow-up action (refresh) when the result calls for one.
    pub fn command_result(&mut self, request_id: &str, result: CommandResult, now_ms: u64) -> Option<DashboardAction> {
        let command = self.command.as_mut()?;
        if command.ticket.request_id != request_id {
            return None;
        }
        if matches!(result, CommandResult::NotSubmitted { .. })
            && !matches!(command.phase, CommandPhase::Submitting | CommandPhase::Checking | CommandPhase::Uncertain)
        {
            return None;
        }
        // Accepted waits for a later snapshot to confirm the effect, even if
        // the current one already shows the target state (another controller).
        command.phase = match result {
            // A refusal of the lookup/replay attempt (busy, scope/version
            // failure, etc.) is not proof that the original never executed.
            CommandResult::Rejected { ref code, .. }
                if matches!(command.phase, CommandPhase::Checking | CommandPhase::Uncertain)
                    && !rejection_settles_original(code) => CommandPhase::Uncertain,
            CommandResult::NotSubmitted { .. }
                if !matches!(command.phase, CommandPhase::Submitting) => CommandPhase::Uncertain,
            CommandResult::NotSubmitted { code, retryable } => {
                CommandPhase::Rejected { code: format::bounded(&code, 64), retryable }
            }
            CommandResult::Accepted => CommandPhase::Accepted,
            CommandResult::Completed => CommandPhase::Confirmed { at_ms: now_ms },
            CommandResult::Unchanged => CommandPhase::Unchanged,
            CommandResult::Rejected { code, retryable, .. } => {
                CommandPhase::Rejected { code: format::bounded(&code, 64), retryable }
            }
            CommandResult::Uncertain { .. } => CommandPhase::Uncertain,
        };
        match command.phase {
            CommandPhase::Uncertain => None,
            _ => {
                self.refreshing = true;
                Some(DashboardAction::Refresh { period: self.view.period })
            }
        }
    }

    pub fn select_tab(&mut self, tab: Tab) -> bool {
        if self.view.tab == tab {
            return false;
        }
        self.view.tab = tab;
        self.view.scroll_y = 0.0;
        true
    }

    pub fn select_resource(&mut self, id: Option<String>) {
        self.view.selected_resource = id;
    }

    pub fn set_account_filter(&mut self, account: Option<String>) {
        self.view.account_filter = account;
    }

    /// Select a reporting period; the host fetches a new snapshot.
    pub fn select_period(&mut self, period: UsagePeriod) -> DashboardAction {
        self.view.period = Some(period);
        self.refreshing = true;
        DashboardAction::Refresh { period: Some(period) }
    }

    /// Restore handover/persisted view state for the same owner only.
    pub fn restore_view(&mut self, state: ViewState) -> bool {
        if state.owner_id.is_some() && state.owner_id != self.owner_scope {
            return false;
        }
        self.view = ViewState { owner_id: self.owner_scope.clone(), version: 1, ..state };
        true
    }

    // ----- presentation -------------------------------------------------

    pub fn present(&self, clock: Clock) -> Presentation {
        present(self, clock)
    }
}

/// Everything the widget draws, as resolved strings and tones.
#[derive(Clone, Debug, PartialEq)]
pub struct Presentation {
    pub owner_line: String,
    pub sync_line: String,
    pub sync_stale: bool,
    pub banner: Option<Banner>,
    /// `None` while there is no snapshot to show (initial connect).
    pub hosting: Option<HostingCard>,
    pub command: Option<CommandLine>,
    pub stop_confirm: bool,
    pub attention: Vec<AttentionItem>,
    pub accounts: Vec<AccountCard>,
    pub unassigned: Vec<ResourceRow>,
    pub usage: UsageCard,
    pub overview_usage: UsageCard,
    pub resources: Vec<ResourceRow>,
    pub activities: Vec<ActivityRow>,
    pub detail: Option<ResourceDetail>,
    pub empty_page: Option<EmptyPage>,
    pub setup_page: Option<SetupPage>,
    pub options: Option<OptionsCard>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerAction {
    Retry,
    Setup,
    StartService,
    SignIn,
    None,
}

impl BannerAction {
    /// The host action this banner button requests, if any.
    pub fn action(self) -> Option<DashboardAction> {
        match self {
            Self::Setup | Self::SignIn => Some(DashboardAction::OpenSetup),
            Self::StartService => Some(DashboardAction::StartService),
            Self::Retry | Self::None => None,
        }
    }
}

/// The page shown instead of the dashboard while hosting must be set up.
#[derive(Clone, Debug, PartialEq)]
pub struct SetupPage {
    pub status: Status,
    pub title: String,
    pub body: String,
    /// Progress or failure of the running setup.
    pub progress: Option<Status>,
    pub busy: bool,
    /// Label of the start/retry button, when setup may start.
    pub start: Option<&'static str>,
    pub can_cancel: bool,
}

/// Host controls under the hosting card.
#[derive(Clone, Debug, PartialEq)]
pub struct OptionsCard {
    pub keep_running: bool,
    pub keep_running_title: &'static str,
    pub keep_running_body: &'static str,
    pub busy: bool,
    pub foreign_service: Option<&'static str>,
    pub uninstall_confirm: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Banner {
    pub status: Status,
    pub title: String,
    pub body: String,
    pub primary: BannerAction,
    pub secondary: BannerAction,
}

/// A full-page state shown instead of content when there is no snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct EmptyPage {
    pub status: Status,
    pub title: String,
    pub body: String,
    pub busy: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HostingCard {
    pub status: Status,
    pub title: String,
    pub body: String,
    pub detail: Option<String>,
    pub active_runs: String,
    pub pending_approvals: String,
    pub can_pause: bool,
    pub can_resume: bool,
    pub can_stop: bool,
    pub show_pause: bool,
    pub show_resume: bool,
    pub show_stop: bool,
    pub show_setup: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CommandLine {
    pub status: Status,
    pub can_check: bool,
    pub can_retry: bool,
    pub can_dismiss: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuotaRow {
    pub id: String,
    pub label: String,
    pub value: String,
    pub fraction: Option<f32>,
    pub tone: Tone,
    pub reset: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AccountCard {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub status: Status,
    pub observed: String,
    pub note: Option<String>,
    pub quotas: Vec<QuotaRow>,
    pub resources: Vec<ResourceRow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceRow {
    pub id: String,
    pub name: String,
    pub line: String,
    pub status: Status,
    pub tokens: String,
    pub budget: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TokenLine {
    pub label: &'static str,
    pub value: String,
    pub known: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageCard {
    pub title: String,
    pub total: String,
    pub total_known: bool,
    pub lines: Vec<TokenLine>,
    pub cache_ratio: String,
    pub completeness: Status,
    pub cost: String,
    pub cost_note: &'static str,
    pub observed: String,
    pub by_resource: Vec<UsageRow>,
    pub filters: Vec<(Option<String>, String, bool)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    pub id: String,
    pub name: String,
    pub input: String,
    pub output: String,
    pub cache_read: String,
    pub cache_write: String,
    pub total: String,
    pub cache_ratio: String,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActivityRow {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub when: String,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourceDetail {
    pub row: ResourceRow,
    pub present: bool,
    pub facts: Vec<(&'static str, String)>,
    pub tokens: Vec<TokenLine>,
    pub cache_ratio: String,
}

fn hosting_status(state: HostingState, locale: Locale) -> (Status, Msg, Msg) {
    use HostingState::*;
    let (tone, glyph, title, body) = match state {
        Unconfigured => (Tone::Neutral, Glyph::Setup, Msg::StateUnconfiguredTitle, Msg::StateUnconfiguredBody),
        Starting => (Tone::Progress, Glyph::Clock, Msg::StateStartingTitle, Msg::StateStartingBody),
        Running => (Tone::Positive, Glyph::Check, Msg::StateRunningTitle, Msg::StateRunningBody),
        Paused => (Tone::Caution, Glyph::Pause, Msg::StatePausedTitle, Msg::StatePausedBody),
        Draining => (Tone::Progress, Glyph::Clock, Msg::StateDrainingTitle, Msg::StateDrainingBody),
        Stopped => (Tone::Neutral, Glyph::Stop, Msg::StateStoppedTitle, Msg::StateStoppedBody),
        Offline => (Tone::Caution, Glyph::Offline, Msg::StateOfflineTitle, Msg::StateOfflineBody),
        Failed => (Tone::Critical, Glyph::Cross, Msg::StateFailedTitle, Msg::StateFailedBody),
    };
    (Status::new(tone, glyph, tr(locale, title)), title, body)
}

pub fn account_status(state: &str, locale: Locale) -> Status {
    let Some(state) = AccountState::parse(state) else {
        return Status::new(Tone::Neutral, Glyph::Question, tr(locale, Msg::StatusUnrecognized));
    };
    let (tone, glyph, msg) = match state {
        AccountState::Fresh => (Tone::Positive, Glyph::Check, Msg::AccountFresh),
        AccountState::Stale => (Tone::Caution, Glyph::Clock, Msg::AccountStale),
        AccountState::Partial => (Tone::Caution, Glyph::Info, Msg::AccountPartial),
        AccountState::Limited => (Tone::Critical, Glyph::Warning, Msg::AccountLimited),
        AccountState::Unsupported => (Tone::Neutral, Glyph::Info, Msg::AccountUnsupported),
        AccountState::Unknown => (Tone::Neutral, Glyph::Question, Msg::AccountUnknown),
        AccountState::Unverified => (Tone::Caution, Glyph::Question, Msg::AccountUnverified),
    };
    Status::new(tone, glyph, tr(locale, msg))
}

pub fn resource_status(state: &str, locale: Locale) -> Status {
    let Some(state) = ResourceState::parse(state) else {
        return Status::new(Tone::Neutral, Glyph::Question, tr(locale, Msg::StatusUnrecognized));
    };
    let (tone, glyph, msg) = match state {
        ResourceState::Measured => (Tone::Positive, Glyph::Check, Msg::ResourceMeasured),
        ResourceState::Partial => (Tone::Caution, Glyph::Info, Msg::ResourcePartial),
        ResourceState::Unmeasured => (Tone::Neutral, Glyph::Question, Msg::ResourceUnmeasured),
        ResourceState::Idle => (Tone::Neutral, Glyph::Pause, Msg::ResourceIdle),
    };
    Status::new(tone, glyph, tr(locale, msg))
}

pub fn activity_status(state: &str, locale: Locale) -> Status {
    let Some(state) = ActivityState::parse(state) else {
        return Status::new(Tone::Neutral, Glyph::Question, tr(locale, Msg::StatusUnrecognized));
    };
    let (tone, glyph, msg) = match state {
        ActivityState::Running => (Tone::Progress, Glyph::Clock, Msg::ActivityRunning),
        ActivityState::Pending => (Tone::Caution, Glyph::Clock, Msg::ActivityPending),
        ActivityState::Succeeded => (Tone::Positive, Glyph::Check, Msg::ActivitySucceeded),
        ActivityState::Failed => (Tone::Critical, Glyph::Cross, Msg::ActivityFailed),
        ActivityState::Uncertain => (Tone::Caution, Glyph::Question, Msg::ActivityUncertain),
        ActivityState::Info => (Tone::Neutral, Glyph::Info, Msg::ActivityInfo),
        ActivityState::Warning => (Tone::Caution, Glyph::Warning, Msg::ActivityWarning),
    };
    Status::new(tone, glyph, tr(locale, msg))
}

/// Sum where any unknown operand makes the result unknown.
fn sum_known(values: &[Option<u64>]) -> Option<u64> {
    values.iter().try_fold(0u64, |acc, v| acc.checked_add((*v)?))
}

pub fn total_tokens(counts: &TokenCounts) -> Option<u64> {
    sum_known(&[counts.input, counts.output, counts.cache_read, counts.cache_write])
}

fn token_lines(counts: &TokenCounts, locale: Locale) -> Vec<TokenLine> {
    [
        (Msg::TokensInput, counts.input),
        (Msg::TokensOutput, counts.output),
        (Msg::TokensCacheRead, counts.cache_read),
        (Msg::TokensCacheWrite, counts.cache_write),
    ]
    .into_iter()
    .map(|(msg, v)| TokenLine { label: tr(locale, msg), value: format::tokens_or_unknown(v, locale), known: v.is_some() })
    .collect()
}

fn ratio_text(ratio: Option<f64>, locale: Locale) -> String {
    match ratio {
        Some(r) => format::percent(r * 100.0),
        None => tr(locale, Msg::Unavailable).into(),
    }
}

fn quota_tone(percent: Option<f64>) -> Tone {
    match percent {
        Some(p) if p >= NEAR_LIMIT_PERCENT => Tone::Critical,
        Some(p) if p >= CAUTION_PERCENT => Tone::Caution,
        Some(_) => Tone::Accent,
        None => Tone::Neutral,
    }
}

fn resource_row(resource: &ResourceUsage, selected: Option<&String>, locale: Locale) -> ResourceRow {
    let total = total_tokens(&resource.counts);
    let used = match total {
        Some(n) => format::compact(n, locale),
        None => tr(locale, Msg::Unknown).into(),
    };
    let budget = match resource.budget_tokens {
        Some(b) => phrase::budget_of(locale, &used, &format::compact(b, locale)),
        None => tr(locale, Msg::NoBudget).into(),
    };
    ResourceRow {
        id: resource.id.clone(),
        name: display_name(&resource.name, &resource.id),
        line: phrase::resource_line(locale, &resource.provider, &resource.model, resource.agent_count),
        status: resource_status(&resource.state, locale),
        tokens: used,
        budget,
        selected: selected == Some(&resource.id),
    }
}

/// Friendly name; the technical ID is only used when no name exists.
fn display_name(name: &str, id: &str) -> String {
    let name = format::bounded(name, 120);
    if name.is_empty() { format::bounded(id, 120) } else { name }
}

fn account_card(account: &ProviderAccount, snapshot: &DashboardSnapshot, view: &ViewState, clock: Clock, locale: Locale) -> AccountCard {
    let quotas = account
        .quota_windows
        .iter()
        .map(|w| {
            let length = w.window_minutes.map(|m| format::window_length(m, locale));
            let reset = match w.resets_at_ms {
                Some(at) if at > clock.now_ms => phrase::resets(
                    locale,
                    &format::duration(at - clock.now_ms, locale),
                    &format::local_time(clock, at, locale),
                ),
                Some(at) => phrase::reset_due(locale, &format::local_time(clock, at, locale)),
                None => tr(locale, Msg::ResetNotReported).into(),
            };
            QuotaRow {
                id: w.id.clone(),
                label: phrase::window_label(locale, &display_name(&w.label, &w.id), length.as_deref()),
                value: match w.used_percent.filter(|p| p.is_finite()) {
                    Some(p) => phrase::used_percent(locale, &format::percent(p)),
                    None => tr(locale, Msg::NotReported).into(),
                },
                fraction: format::meter_fraction(w.used_percent),
                tone: quota_tone(w.used_percent.filter(|p| p.is_finite())),
                reset,
            }
        })
        .collect();
    let subtitle = match &account.plan {
        Some(plan) if !plan.trim().is_empty() => format!("{} · {}", account.provider, format::bounded(plan, 60)),
        _ => account.provider.clone(),
    };
    let observed = match account.observed_at_ms {
        Some(at) => phrase::observed(locale, &format::age(clock, at, locale)),
        None => tr(locale, Msg::NeverObserved).into(),
    };
    let note = (AccountState::parse(&account.state) == Some(AccountState::Unverified))
        .then(|| tr(locale, Msg::AccountUnverifiedNote).to_owned());
    AccountCard {
        id: account.id.clone(),
        title: display_name(&account.label, &account.id),
        subtitle,
        status: account_status(&account.state, locale),
        observed,
        note,
        quotas,
        resources: snapshot
            .resources
            .iter()
            .filter(|r| r.account_id.as_deref() == Some(account.id.as_str()))
            .map(|r| resource_row(r, view.selected_resource.as_ref(), locale))
            .collect(),
    }
}

fn period_title(period: Option<UsagePeriod>, locale: Locale) -> String {
    let title = tr(locale, Msg::UsageTitle);
    let period = match period {
        Some(UsagePeriod::Day) => tr(locale, Msg::PeriodDay),
        Some(UsagePeriod::Week) => tr(locale, Msg::PeriodWeek),
        Some(UsagePeriod::Month) => tr(locale, Msg::PeriodMonth),
        Some(UsagePeriod::Range { start_ms, end_ms }) => return format!("{title} · {} – {} (UTC)", crate::date_range::label(start_ms), crate::date_range::label(end_ms.saturating_sub(1))),
        Some(UsagePeriod::All) | None => tr(locale, Msg::PeriodUnreported),
    };
    format!("{title} · {period}")
}

fn usage_card(state: &DashboardState, snapshot: &DashboardSnapshot, clock: Clock, locale: Locale) -> UsageCard {
    let filter = state.view.account_filter.as_ref();
    let requested = state.view.usage_filter();
    let report = snapshot.usage_breakdown.as_ref().filter(|details| details.filter == requested).map(|details| &details.report);
    let detailed_filter = requested.resource_id.is_some() || requested.model.is_some() || requested.requester_id.is_some();
    // Account totals come from the execution ledger. Resource rows may be
    // incomplete or may have changed accounts during this reporting period.
    let (counts, complete, observed_at) = if let Some(report) = report {
        (report.total.counts, report.total.complete, report.total.observed_at_ms)
    } else if detailed_filter {
        (hagency_metering::dashboard::UNKNOWN_COUNTS, false, None)
    } else { match filter {
        None => (snapshot.usage.counts, snapshot.usage.complete, snapshot.usage.observed_at_ms),
        Some(id) => match snapshot.accounts.iter().find(|a| &a.id == id).and_then(|a| a.usage.as_ref()) {
            Some(usage) => (usage.counts, usage.complete, usage.observed_at_ms),
            None => (hagency_metering::dashboard::UNKNOWN_COUNTS, false, None),
        },
    }};
    let rows: Vec<_> = snapshot.resources.iter().filter_map(|row| {
        if let Some(report) = report {
            let usage = report.by_resource.iter().find(|(id,_)| id.as_ref()==Some(&row.id))?.1.clone();
            let mut row = row.clone();
            row.counts = usage.counts;
            row.state = crate::dto::collector::resource_state(&usage).as_str().into();
            Some(row)
        } else if !detailed_filter && filter.is_none_or(|id| row.account_id.as_ref() == Some(id)) {
            Some(row.clone())
        } else { None }
    }).collect();
    let total = total_tokens(&counts);
    let any_known = [counts.input, counts.output, counts.cache_read, counts.cache_write].iter().any(Option::is_some);
    let completeness = if !any_known {
        Status::new(Tone::Neutral, Glyph::Question, tr(locale, Msg::UsageNoData))
    } else if complete {
        Status::new(Tone::Positive, Glyph::Check, tr(locale, Msg::UsageComplete))
    } else {
        Status::new(Tone::Caution, Glyph::Info, tr(locale, Msg::UsagePartial))
    };
    let cost = match (requested.is_empty(), snapshot.usage.estimated_cost_usd) {
        (true, Some(v)) => phrase::approx_usd(locale, &format::usd(v)),
        _ => tr(locale, Msg::NotEstimated).into(),
    };
    let mut filters = vec![(None, tr(locale, Msg::AllAccounts).to_owned(), filter.is_none())];
    for account in &snapshot.accounts {
        filters.push((Some(account.id.clone()), display_name(&account.label, &account.id), filter == Some(&account.id)));
    }
    UsageCard {
        title: if requested.is_empty() { period_title(state.snapshot_period, locale) }
            else { format!("{} · {}",period_title(state.snapshot_period, locale),tr(locale,Msg::Filtered)) },
        total: match total {
            Some(n) => format::grouped(n),
            None => tr(locale, Msg::Unknown).into(),
        },
        total_known: total.is_some(),
        lines: token_lines(&counts, locale),
        cache_ratio: ratio_text(hagency_metering::dashboard::cache_hit_ratio(&counts), locale),
        completeness,
        cost,
        cost_note: tr(locale, Msg::EstimatedCostNote),
        observed: match observed_at {
            Some(at) => phrase::observed(locale, &format::age(clock, at, locale)),
            None => tr(locale, Msg::NeverObserved).into(),
        },
        by_resource: rows
            .iter()
            .map(|r| UsageRow {
                id: r.id.clone(),
                name: display_name(&r.name, &r.id),
                input: format::tokens_or_unknown(r.counts.input, locale),
                output: format::tokens_or_unknown(r.counts.output, locale),
                cache_read: format::tokens_or_unknown(r.counts.cache_read, locale),
                cache_write: format::tokens_or_unknown(r.counts.cache_write, locale),
                total: format::tokens_or_unknown(total_tokens(&r.counts), locale),
                cache_ratio: ratio_text(r.cache_hit_ratio(), locale),
                status: resource_status(&r.state, locale),
            })
            .collect(),
        filters,
    }
}

fn command_line(state: &DashboardState, clock: Clock, locale: Locale) -> Option<CommandLine> {
    let command = state.command.as_ref()?;
    let action = command.kind.label(locale);
    let running = state.snapshot.as_ref().and_then(|s| s.hosting.active_runs).unwrap_or(0);
    let draining = state.snapshot.as_ref().is_some_and(|s| s.hosting.state == HostingState::Draining);
    let (status, can_check, can_retry, can_dismiss) = match &command.phase {
        CommandPhase::Submitting => (Status::new(Tone::Progress, Glyph::Clock, phrase::submitting(locale, action)), false, false, false),
        CommandPhase::Accepted if draining && running > 0 => (
            Status::new(Tone::Progress, Glyph::Clock, phrase::awaiting_drain(locale, action, running)),
            false, false, false,
        ),
        CommandPhase::Accepted => (Status::new(Tone::Progress, Glyph::Clock, phrase::awaiting(locale, action)), false, false, false),
        CommandPhase::Confirmed { at_ms } => (
            Status::new(Tone::Positive, Glyph::Check, phrase::confirmed(locale, action, &format::local_time(clock, *at_ms, locale))),
            false, false, true,
        ),
        CommandPhase::Superseded => (Status::new(Tone::Neutral, Glyph::Info,
            if locale == Locale::ZhHans { "命令已接受；托管状态随后发生变化。请查看当前状态。" }
            else { "Command accepted; hosting changed afterward. Review the current status." }), false, false, true),
        CommandPhase::Unchanged => (Status::new(Tone::Neutral, Glyph::Info, phrase::unchanged(locale, action)), false, false, true),
        CommandPhase::Rejected { code, retryable } => (
            Status::new(Tone::Critical, Glyph::Cross, phrase::rejected(locale, action, rejection_reason(code, locale), code)),
            false,
            *retryable && state.connected(),
            true,
        ),
        CommandPhase::Uncertain => (
            Status::new(Tone::Caution, Glyph::Question, phrase::uncertain(locale, action)),
            state.connection == ConnectionStatus::Connected,
            false,
            false,
        ),
        CommandPhase::Checking => (Status::new(Tone::Progress, Glyph::Clock, phrase::checking(locale, action)), false, false, false),
    };
    Some(CommandLine { status, can_check, can_retry, can_dismiss })
}

/// Localized reason for engine-defined error codes. Unknown codes get a
/// generic reason; the code itself is still shown.
pub fn rejection_reason(code: &str, locale: Locale) -> &'static str {
    if code == "command_recovery_unavailable" {
        return if locale == Locale::ZhHans { "无法保存或读取原命令记录，本次操作未发送。请检查数据目录权限后重试。" }
        else { "The command record could not be saved or read. This attempt was not sent. Check data-folder access and retry." };
    }
    if code == "pending_command_exists" {
        return if locale == Locale::ZhHans { "另一个已保存的托管命令尚待确认，请先检查原命令状态。" }
        else { "Another saved hosting command needs confirmation. Check that original command first." };
    }
    tr(locale, match code {
        "revision_conflict" => Msg::ReasonRevisionConflict,
        "owner_scope_mismatch" | "owner_not_adopted" | "peer_rejected" => Msg::ReasonOwnerScope,
        "hosting_stopped" => Msg::ReasonStopped,
        "busy" => Msg::ReasonBusy,
        "not_supported" => Msg::ReasonNotSupported,
        "daemon_stopping" => Msg::ReasonShuttingDown,
        "unsupported_version" => Msg::ReasonVersion,
        "not_connected" => Msg::ReasonNotConnected,
        _ => Msg::ReasonUnknown,
    })
}

fn banner(state: &DashboardState, locale: Locale) -> Option<Banner> {
    // The setup page explains a missing setup itself.
    if state.setup.is_some() {
        return None;
    }
    let has_data = state.snapshot.is_some();
    let last_known = if has_data { format!(" {}", tr(locale, Msg::ShowingLastKnown)) } else { String::new() };
    let make = |tone, glyph, title: Msg, body: String, primary, secondary| Banner {
        status: Status::new(tone, glyph, tr(locale, title)),
        title: tr(locale, title).into(),
        body,
        primary,
        secondary,
    };
    match &state.connection {
        ConnectionStatus::Connected => None,
        ConnectionStatus::Connecting if !has_data => None,
        ConnectionStatus::Connecting => None,
        ConnectionStatus::DaemonUnavailable => Some(make(
            Tone::Caution, Glyph::Offline, Msg::DaemonAbsentTitle,
            format!("{}{last_known}", tr(locale, Msg::DaemonAbsentBody)),
            BannerAction::StartService, BannerAction::Retry,
        )),
        ConnectionStatus::Incompatible { daemon_version } => Some(make(
            Tone::Critical, Glyph::Warning, Msg::IncompatibleTitle,
            format!("{}{last_known}", phrase::incompatible(locale, DASHBOARD_API_VERSION, *daemon_version)),
            BannerAction::Retry, BannerAction::None,
        )),
        ConnectionStatus::AuthExpired => Some(make(
            Tone::Caution, Glyph::Warning, Msg::AuthExpiredTitle,
            format!("{}{last_known}", tr(locale, Msg::AuthExpiredBody)),
            BannerAction::SignIn, BannerAction::Retry,
        )),
        ConnectionStatus::OwnerMismatch => Some(make(
            Tone::Caution, Glyph::Warning, Msg::OwnerMismatchTitle,
            tr(locale, Msg::OwnerMismatchBody).to_owned(),
            BannerAction::Retry, BannerAction::None,
        )),
        ConnectionStatus::UnsupportedPlatform => Some(make(
            Tone::Neutral, Glyph::Info, Msg::UnsupportedPlatformTitle,
            tr(locale, Msg::UnsupportedPlatformBody).to_owned(),
            BannerAction::None, BannerAction::None,
        )),
        ConnectionStatus::Failed { message } => Some(make(
            Tone::Critical, Glyph::Cross, Msg::FailedTitle,
            if message.is_empty() { last_known.trim().to_owned() } else { format!("{message}{last_known}") },
            BannerAction::Retry, BannerAction::None,
        )),
    }
}

fn empty_page(state: &DashboardState, locale: Locale) -> Option<EmptyPage> {
    if state.snapshot.is_some() || state.setup.is_some() {
        return None;
    }
    // With a banner (daemon absent, incompatible, sign-in, failure) the banner
    // carries the explanation and recovery; the page only reserves space.
    match state.connection {
        ConnectionStatus::Connecting | ConnectionStatus::Connected => Some(EmptyPage {
            status: Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::ConnectingTitle)),
            title: tr(locale, Msg::ConnectingTitle).into(),
            body: tr(locale, Msg::ConnectingBody).into(),
            busy: true,
        }),
        _ => None,
    }
}

fn attention(state: &DashboardState, snapshot: &DashboardSnapshot, locale: Locale) -> Vec<AttentionItem> {
    let mut items = Vec::new();
    let mut add = |key: String, target, status| items.push(AttentionItem { key, target, status });
    if let Some(n) = snapshot.hosting.pending_approvals.filter(|n| *n > 0) {
        add("approvals".into(), AttentionTarget::Approvals, Status::new(Tone::Caution, Glyph::Clock, phrase::approvals_waiting(locale, n)));
    }
    if let Some(command) = &state.command {
        if command.phase == CommandPhase::Uncertain {
            add("command".into(), AttentionTarget::Command, Status::new(Tone::Caution, Glyph::Question, phrase::uncertain(locale, command.kind.label(locale))));
        }
    }
    for account in &snapshot.accounts {
        let label = display_name(&account.label, &account.id);
        if AccountState::parse(&account.state) == Some(AccountState::Limited) {
            add(format!("account:{}", account.id), AttentionTarget::Account(account.id.clone()), Status::new(Tone::Critical, Glyph::Warning, phrase::account_limited(locale, &label)));
        } else if let Some(max) = account
            .quota_windows
            .iter()
            .filter_map(|w| w.used_percent.filter(|p| p.is_finite()))
            .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))))
            .filter(|p| *p >= NEAR_LIMIT_PERCENT)
        {
            add(format!("account:{}", account.id), AttentionTarget::Account(account.id.clone()), Status::new(Tone::Caution, Glyph::Warning, phrase::account_near_limit(locale, &label, &format::percent(max))));
        }
    }
    let failed = snapshot
        .activities
        .iter()
        .filter(|a| matches!(ActivityState::parse(&a.state), Some(ActivityState::Failed | ActivityState::Uncertain)))
        .count();
    if failed > 0 {
        add("failed".into(), AttentionTarget::Activity, Status::new(Tone::Critical, Glyph::Cross, phrase::failed_activities(locale, failed)));
    }
    let any_known = [snapshot.usage.counts.input, snapshot.usage.counts.output, snapshot.usage.counts.cache_read, snapshot.usage.counts.cache_write]
        .iter()
        .any(Option::is_some);
    if any_known && !snapshot.usage.complete {
        add("partial".into(), AttentionTarget::Usage, Status::new(Tone::Caution, Glyph::Info, tr(locale, Msg::AttentionPartialUsage)));
    }
    let known_accounts: Vec<&str> = snapshot.accounts.iter().map(|a| a.id.as_str()).collect();
    if snapshot.resources.iter().any(|r| r.account_id.as_deref().is_none_or(|id| !known_accounts.contains(&id))) {
        add("unassigned".into(), AttentionTarget::Resources, Status::new(Tone::Neutral, Glyph::Question, tr(locale, Msg::AttentionUnassigned)));
    }
    items
}

fn present(state: &DashboardState, clock: Clock) -> Presentation {
    let locale = state.locale;
    let snapshot = state.snapshot.as_ref();
    let owner_line = match snapshot.and_then(|s| s.owner.as_ref()) {
        Some(owner) => {
            let name = display_name(&owner.display_name, &owner.user_id);
            format!("{} · {name}", tr(locale, Msg::SignedInAs))
        }
        None if snapshot.is_some() => tr(locale, Msg::NoOwner).into(),
        None => String::new(),
    };
    let (sync_line, sync_stale) = match snapshot {
        _ if state.refreshing => (tr(locale, Msg::Refreshing).to_owned(), false),
        Some(s) => {
            let stale = clock.now_ms.saturating_sub(s.observed_at_ms) > STALE_AFTER_MS
                || state.connection != ConnectionStatus::Connected;
            (phrase::updated(locale, &format::age(clock, s.observed_at_ms, locale)), stale)
        }
        None => (String::new(), false),
    };
    let hosting = snapshot.map(|s| {
        let (status, _, body) = hosting_status(s.hosting.state, locale);
        let body = match s.hosting.state {
            HostingState::Draining => match s.hosting.active_runs {
                Some(n) => phrase::draining(locale, n),
                None => tr(locale, body).to_owned(),
            },

            _ => tr(locale, body).to_owned(),
        };
        let st = s.hosting.state;
        HostingCard {
            title: status.text.clone(),
            status,
            body,
            detail: s.hosting.detail.as_deref().map(|d| format::bounded(d, MAX_DAEMON_TEXT)).filter(|d| !d.is_empty()),
            active_runs: format::tokens_or_unknown(s.hosting.active_runs, locale),
            pending_approvals: format::tokens_or_unknown(s.hosting.pending_approvals, locale),
            can_pause: state.can(CommandKind::Pause, None),
            can_resume: state.can(CommandKind::Resume, None),
            can_stop: state.can(CommandKind::Stop, None),
            show_pause: matches!(st, HostingState::Running | HostingState::Starting),
            show_resume: matches!(st, HostingState::Paused | HostingState::Draining | HostingState::Stopped | HostingState::Failed | HostingState::Offline),
            show_stop: matches!(st, HostingState::Running | HostingState::Starting | HostingState::Paused | HostingState::Draining),
            show_setup: st == HostingState::Unconfigured,
        }
    });
    let accounts = snapshot
        .map(|s| s.accounts.iter().map(|a| account_card(a, s, &state.view, clock, locale)).collect())
        .unwrap_or_default();
    let unassigned = snapshot
        .map(|s| {
            let known: Vec<&str> = s.accounts.iter().map(|a| a.id.as_str()).collect();
            s.resources
                .iter()
                .filter(|r| r.account_id.as_deref().is_none_or(|id| !known.contains(&id)))
                .map(|r| resource_row(r, state.view.selected_resource.as_ref(), locale))
                .collect()
        })
        .unwrap_or_default();
    let resources = snapshot
        .map(|s| s.resources.iter().map(|r| resource_row(r, state.view.selected_resource.as_ref(), locale)).collect())
        .unwrap_or_default();
    let activities = snapshot
        .map(|s| {
            let mut list: Vec<_> = s.activities.iter().collect();
            list.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| a.id.cmp(&b.id)));
            list.into_iter()
                .map(|a| {
                    let (title, detail) = crate::activity_text::localized(a, locale);
                    ActivityRow {
                    id: a.id.clone(),
                    title: display_name(&title, &a.id),
                    detail: format::bounded(&detail, MAX_DAEMON_TEXT),
                    when: format::age(clock, a.at_ms, locale),
                    status: activity_status(&a.state, locale),
                }})
                .collect()
        })
        .unwrap_or_default();
    let detail = state.view.selected_resource.as_ref().and_then(|id| {
        let s = snapshot?;
        match s.resources.iter().find(|r| &r.id == id) {
            Some(r) => Some(ResourceDetail {
                row: resource_row(r, Some(id), locale),
                present: true,
                facts: vec![
                    (tr(locale, Msg::Provider), r.provider.clone()),
                    (tr(locale, Msg::Model), r.model.clone()),
                    (tr(locale, Msg::Agents), format::tokens_or_unknown(r.agent_count, locale)),
                    (tr(locale, Msg::Budget), match r.budget_tokens {
                        Some(b) => format::grouped(b),
                        None => tr(locale, Msg::NoBudget).into(),
                    }),
                    (tr(locale, Msg::ResourceId), r.id.clone()),
                    (tr(locale, Msg::AccountId), r.account_id.clone().unwrap_or_else(|| tr(locale, Msg::UnassignedAccount).into())),
                ],
                tokens: token_lines(&r.counts, locale),
                cache_ratio: ratio_text(r.cache_hit_ratio(), locale),
            }),
            None => Some(ResourceDetail {
                row: ResourceRow {
                    id: id.clone(),
                    name: format::bounded(id, 120),
                    line: String::new(),
                    status: Status::new(Tone::Neutral, Glyph::Question, tr(locale, Msg::ResourceGone)),
                    tokens: tr(locale, Msg::Unknown).into(),
                    budget: String::new(),
                    selected: true,
                },
                present: false,
                facts: vec![(tr(locale, Msg::ResourceId), id.clone())],
                tokens: Vec::new(),
                cache_ratio: tr(locale, Msg::Unavailable).into(),
            }),
        }
    });
    Presentation {
        owner_line,
        sync_line,
        sync_stale,
        banner: banner(state, locale),
        hosting,
        command: command_line(state, clock, locale),
        stop_confirm: state.confirm_stop,
        attention: snapshot.map(|s| attention(state, s, locale)).unwrap_or_default(),
        accounts,
        unassigned,
        usage: match snapshot {
            Some(s) => usage_card(state, s, clock, locale),
            None => usage_card(state, &DashboardSnapshot::default(), clock, locale),
        },
        overview_usage: {
            let mut unfiltered = state.clone();
            unfiltered.view.account_filter = None;
            unfiltered.view.resource_filter = None;
            unfiltered.view.model_filter = None;
            unfiltered.view.requester_filter = None;
            usage_card(&unfiltered, snapshot.unwrap_or(&DashboardSnapshot::default()), clock, locale)
        },
        resources,
        activities,
        detail,
        empty_page: empty_page(state, locale),
        setup_page: setup_page(state, locale),
        options: options_card(state, locale),
    }
}

fn setup_page(state: &DashboardState, locale: Locale) -> Option<SetupPage> {
    let setup = state.setup.as_ref()?;
    let (glyph, tone, title, body) = match setup.need {
        SetupNeed::NotSetUp => (Glyph::Setup, Tone::Neutral, Msg::SetupTitle, Msg::SetupBody),
        SetupNeed::Replace => (Glyph::Setup, Tone::Caution, Msg::SetupReplaceTitle, Msg::SetupReplaceBody),
        SetupNeed::OtherOwner => (Glyph::Warning, Tone::Caution, Msg::SetupOtherOwnerTitle, Msg::SetupOtherOwnerBody),
    };
    let progress = match &setup.progress {
        SetupProgress::Idle => None,
        SetupProgress::Preparing => Some(Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::SetupPreparing))),
        SetupProgress::SigningIn => Some(Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::SetupSigningIn))),
        SetupProgress::Registering => Some(Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::SetupRegistering))),
        SetupProgress::Cancelling => Some(Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::SetupCancelling))),
        SetupProgress::Enabling => Some(Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::SetupEnabling))),
        SetupProgress::Confirming => Some(Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::SetupConfirming))),
        SetupProgress::Failed { code } => Some(Status::new(Tone::Critical, Glyph::Cross, phrase::setup_failure(locale, code))),
    };
    let start = match (&setup.need, &setup.progress) {
        (SetupNeed::OtherOwner, _) => None,
        (_, SetupProgress::Idle) => Some(tr(locale, Msg::SetUpHosting)),
        (_, SetupProgress::Failed { .. }) => Some(tr(locale, Msg::TryAgain)),
        _ => None,
    };
    Some(SetupPage {
        status: Status::new(tone, glyph, tr(locale, title)),
        title: tr(locale, title).into(),
        body: tr(locale, body).into(),
        busy: setup.running(),
        progress,
        start,
        can_cancel: setup.progress == SetupProgress::SigningIn,
    })
}

fn options_card(state: &DashboardState, locale: Locale) -> Option<OptionsCard> {
    let options = state.options.as_ref().filter(|_| state.setup.is_none() && state.snapshot.is_some())?;
    Some(OptionsCard {
        keep_running: options.keep_running,
        keep_running_title: tr(locale, Msg::KeepRunningTitle),
        keep_running_body: tr(locale, if options.keep_running { Msg::KeepRunningOn } else { Msg::KeepRunningOff }),
        busy: options.busy,
        foreign_service: options.foreign_service.then(|| tr(locale, Msg::ForeignService)),
        uninstall_confirm: state.confirm_uninstall,
    })
}

#[cfg(test)]
mod tests;
