//! The Hosting page of the Agent mini-app (ADR 0012, ADR-192): the owner
//! dashboard of this computer's background hosting, the setup that replaces
//! it until the owner daemon hosts this account, and Desktop's hosting
//! controls (keep running after quit, start and uninstall the service).
//! Execution and filesystem IO belong to background workers, never this widget.
use crate::hagency::hosting::{self, Execution, JobKind, Progress, SetupReason};
use makepad_widgets::*;
use owner_dashboard::{
    CommandKind, CommandResult, ConnectionStatus, DashboardAction, HostOptions, Locale, OwnerDashboardRef,
    OwnerDashboardWidgetExt, SetupNeed, SetupProgress, SetupView,
    i18n::{Msg, phrase, tr},
    model::{Glyph, Status, Tone},
    transport::{ConnectorEvent, DaemonConnector, DaemonEndpoint, Job, OwnerScope},
};
#[cfg(feature = "onboarding-test-support")]
use owner_dashboard::ViewState;

/// Include the app's sidebar in the same native accessibility tree as Hosting.
pub fn register_host_accessibility(cx: &mut Cx) {
    use crate::shared::navigation_bar_button::{NavigationBarButton, NavigationBarButtonAction};
    use owner_dashboard::accessibility::{Control, ControlAdapter, register_control_adapter};
    register_control_adapter(cx, ControlAdapter {
        name: "desktop-navigation",
        describe: |widget, cx| {
            let button = widget.borrow::<NavigationBarButton>()?;
            let label = button.tooltip_text().to_owned();
            (!label.is_empty()).then(|| Control { label, selected: Some(button.is_selected(cx)), disabled: false })
        },
        activate: |widget, cx| cx.widget_action(widget.widget_uid(), NavigationBarButtonAction::Clicked),
    });
}

pub fn register(vm: &mut ScriptVm) {
    owner_dashboard::script_mod(vm);
    self::script_mod(vm);
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.OwnerDashboardHost = #(OwnerDashboardHost::register_widget(vm)) {
        width: Fill height: Fill flow: Down
        dashboard := OwnerDashboard {}
    }
}

/// Hosting changes for the app to apply and save with its settings.
#[derive(Clone, Debug, PartialEq)]
pub enum HostingAction {
    /// The Hosting page's "keep running after quit" switch changed.
    KeepRunning(bool),
    /// A resume of hosting paused at the last quit finished. `settled` means
    /// nothing remains to resume for `owner`.
    Resumed { owner: String, settled: bool },
}

fn locale() -> Locale {
    match crate::i18n::language() {
        crate::i18n::Language::Chinese => Locale::ZhHans,
        crate::i18n::Language::English => Locale::En,
    }
}

/// The signed-in account, without creating a backend service.
fn current_account() -> Option<hosting::Account> {
    let client = crate::sliding_sync::get_client()?;
    Some(hosting::Account { origin: client.homeserver().to_string(), owner: client.user_id()?.to_string() })
}

fn setup_need(reason: SetupReason) -> SetupNeed {
    match reason {
        SetupReason::NotEnrolled => SetupNeed::NotSetUp,
        SetupReason::StaleRoot => SetupNeed::Replace,
        SetupReason::OtherOwner => SetupNeed::OtherOwner,
    }
}

fn setup_progress(progress: &Progress) -> SetupProgress {
    match progress {
        Progress::Preparing => SetupProgress::Preparing,
        Progress::SigningIn => SetupProgress::SigningIn,
        Progress::Registering => SetupProgress::Registering,
        Progress::Cancelling => SetupProgress::Cancelling,
        Progress::Enabling | Progress::Removing => SetupProgress::Enabling,
        Progress::Confirming | Progress::Done => SetupProgress::Confirming,
        Progress::Unconfirmed | Progress::Cancelled => SetupProgress::Idle,
        Progress::Failed(code) => SetupProgress::Failed { code: (*code).into() },
    }
}

/// What the page shows for one hosting state.
#[derive(Clone, Debug, PartialEq)]
struct Page {
    setup: Option<SetupView>,
    /// The daemon hosts this owner: connect, show the dashboard and options.
    hosted: bool,
    /// This platform executes in the app; no background service exists.
    in_app: bool,
    busy: bool,
    /// The installed service runs another program (an earlier Hosting app).
    foreign_service: bool,
}

fn page(view: &hosting::View, last_need: SetupNeed) -> Page {
    let job = view.job.as_ref();
    let busy = job.is_some_and(|(_, progress)| !progress.finished());
    let need = match &view.execution {
        Some(Ok(Execution::Setup(reason))) => Some(setup_need(*reason)),
        Some(Err(_)) => Some(last_need),
        _ => None,
    };
    // A running setup keeps its page until it finishes, even once the
    // enrollment already hands execution to the daemon.
    let setup = match (job, need) {
        (Some((JobKind::Setup, progress)), need) if !progress.finished() => {
            Some(SetupView { need: need.unwrap_or(last_need), progress: setup_progress(progress) })
        }
        (Some((JobKind::Setup, progress @ Progress::Failed(_))), Some(need)) => {
            Some(SetupView { need, progress: setup_progress(progress) })
        }
        (_, Some(need)) => Some(SetupView {
            need,
            progress: match &view.execution {
                Some(Err(code)) => SetupProgress::Failed { code: code.clone() },
                _ => SetupProgress::Idle,
            },
        }),
        _ => None,
    };
    let hosted = setup.is_none() && matches!(view.execution, Some(Ok(Execution::Daemon { .. })));
    let in_app = matches!(view.execution, Some(Ok(Execution::InProcess)));
    let foreign_service = view.service == Some(hosting::Service::OtherProgram);
    Page { setup, hosted, in_app, busy, foreign_service }
}

/// A notice for a job that just changed, outside the setup page.
fn job_notice(job: &(JobKind, Progress), setup_shown: bool, locale: Locale) -> Option<Status> {
    let (kind, progress) = job;
    Some(match (kind, progress) {
        (JobKind::StartService, p) if !p.finished() => Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::ServiceStarting)),
        (JobKind::Uninstall, p) if !p.finished() => Status::new(Tone::Progress, Glyph::Clock, tr(locale, Msg::ServiceRemoving)),
        (JobKind::Uninstall, Progress::Done) => Status::new(Tone::Neutral, Glyph::Info, tr(locale, Msg::ServiceRemoved)),
        (_, Progress::Done) => Status::new(Tone::Positive, Glyph::Check, tr(locale, Msg::HostingConfirmed)),
        (_, Progress::Unconfirmed) => Status::new(Tone::Caution, Glyph::Clock, phrase::setup_failure(locale, "hosting_not_confirmed")),
        (JobKind::Setup, Progress::Failed(_)) if setup_shown => return None,
        (_, Progress::Failed(code)) => Status::new(Tone::Critical, Glyph::Cross, phrase::setup_failure(locale, code)),
        _ => return None,
    })
}

#[derive(Script, ScriptHook, Widget)]
pub struct OwnerDashboardHost {
    #[deref] view: View,
    #[rust] active: bool,
    /// The account was read at least once; `account` is meaningful.
    #[rust] started: bool,
    #[rust] account: Option<hosting::Account>,
    #[rust] connector: Option<DaemonConnector>,
    #[rust] refresh: Timer,
    #[rust] language: Option<Locale>,
    #[rust] fixture: bool,
    /// The hosting state last applied, by generation.
    #[rust] applied: Option<u64>,
    #[rust] job: Option<(JobKind, Progress)>,
    #[rust] page: Option<Page>,
    #[rust] last_need: Option<SetupNeed>,
    /// The switch value the app has not applied yet.
    #[rust] pending_keep: Option<bool>,
}

impl OwnerDashboardHost {
    fn dashboard(&self, cx: &mut Cx) -> OwnerDashboardRef { self.view.owner_dashboard(cx, ids!(dashboard)) }

    fn synchronize(&mut self, cx: &mut Cx) {
        if self.fixture { return }
        let dashboard = self.dashboard(cx);
        let language = locale();
        if self.language != Some(language) {
            dashboard.set_locale(cx, language);
            self.language = Some(language);
        }
        let account = current_account();
        if !self.started || account != self.account {
            self.started = true;
            // Drop the old event queue before changing identities. Late
            // replies cannot populate the next account's dashboard.
            self.connector = None;
            self.applied = None;
            self.job = None;
            self.page = None;
            self.last_need = None;
            self.account = account.clone();
            dashboard.set_owner_scope(cx, account.as_ref().map(|a| a.owner.as_str()));
            if account.is_none() {
                // Once per change: every set_connection redraws the dashboard.
                dashboard.set_connection(cx, ConnectionStatus::AuthExpired);
            }
        }
        let Some(account) = account else { return };
        if !self.active { return }
        let view = hosting::view(&account);
        if self.applied != Some(view.generation) {
            self.applied = Some(view.generation);
            self.apply(cx, &view);
        }
        let keep_running = cx.global::<crate::settings::app_preferences::AppPreferencesGlobal>().0.keep_hosting_after_quit;
        self.options(cx, keep_running);
        if self.page.as_ref().is_some_and(|page| page.hosted) && self.connector.is_none() {
            self.connector = Some(DaemonConnector::spawn(DaemonEndpoint::new(self.daemon_home()), OwnerScope::Fixed(account.owner.clone()), SignalToUI::set_ui_signal));
            let view = dashboard.view_state(cx);
            self.submit(cx, Job::Snapshot { period: view.period, usage_filter: Some(view.usage_filter()) });
        }
    }

    fn daemon_home(&self) -> std::path::PathBuf {
        ::hagency::service::default_daemon_dir().unwrap_or_default()
    }

    /// Present one hosting state: the setup page, the hosted dashboard or the
    /// in-app note, and a notice when a service step changed.
    fn apply(&mut self, cx: &mut Cx, view: &hosting::View) {
        let dashboard = self.dashboard(cx);
        let page = page(view, self.last_need.unwrap_or(SetupNeed::NotSetUp));
        if let Some(setup) = &page.setup {
            self.last_need = Some(setup.need);
        }
        if !page.hosted {
            self.connector = None;
        }
        if page.in_app {
            dashboard.set_connection(cx, ConnectionStatus::UnsupportedPlatform);
        }
        dashboard.set_setup(cx, page.setup.clone());
        if view.job != self.job {
            self.job = view.job.clone();
            if let Some(job) = &view.job {
                let notice = job_notice(job, page.setup.is_some(), dashboard.locale());
                if notice.is_some() || job.1 == Progress::Cancelled {
                    dashboard.set_notice(cx, notice);
                }
            }
        }
        self.page = Some(page);
    }

    fn options(&mut self, cx: &mut Cx, saved: bool) {
        // Until the app applies a switch change, show what the owner chose.
        let keep_running = match self.pending_keep {
            Some(pending) if pending != saved => pending,
            _ => { self.pending_keep = None; saved }
        };
        let Some(page) = &self.page else { return };
        let options = page.hosted.then(|| HostOptions { keep_running, busy: page.busy, foreign_service: page.foreign_service });
        self.dashboard(cx).set_options(cx, options);
    }

    fn submit(&self, cx: &mut Cx, job: Job) {
        let command = match &job { Job::Command { ticket, .. } => Some(ticket.request_id.clone()), _ => None };
        if self.fixture {
            if let Some(id) = command {
                self.dashboard(cx).command_result(cx, &id, CommandResult::Uncertain { message: "Offline review fixture".into() });
            }
            return;
        }
        if !self.connector.as_ref().is_some_and(|c| c.submit(job)) {
            let dashboard = self.dashboard(cx);
            if let Some(id) = command {
                dashboard.command_result(cx, &id, CommandResult::NotSubmitted { code: "not_connected".into(), retryable: true });
            } else if self.page.as_ref().is_some_and(|page| page.hosted) {
                dashboard.set_connection(cx, ConnectionStatus::DaemonUnavailable);
            }
        }
    }

    fn drain(&self, cx: &mut Cx) {
        let Some(connector) = &self.connector else { return };
        let dashboard = self.dashboard(cx);
        let owner = self.account.as_ref().map(|a| a.owner.clone());
        while let Some(event) = connector.try_recv() {
            match event {
                ConnectorEvent::Owner(reported) if reported != owner => dashboard.set_connection(cx, ConnectionStatus::OwnerMismatch),
                ConnectorEvent::Connection(status) => dashboard.set_connection(cx, status),
                ConnectorEvent::PendingCommand(command) => dashboard.recover_command(cx, command),
                ConnectorEvent::Snapshot { snapshot, period } => { dashboard.set_snapshot_for_period(cx, snapshot, period); },
                ConnectorEvent::CommandResult { request_id, result } => dashboard.command_result(cx, &request_id, result),
                _ => {},
            }
        }
    }

    /// Start a hosting step for the signed-in account.
    fn start(&mut self, cx: &mut Cx, kind: JobKind) {
        if self.fixture { return }
        let Some(account) = self.account.clone() else { return };
        if !hosting::start(kind, &account) {
            let dashboard = self.dashboard(cx);
            let text = tr(dashboard.locale(), Msg::HostingBusy);
            dashboard.set_notice(cx, Some(Status::new(Tone::Caution, Glyph::Clock, text)));
        }
        self.applied = None;
        self.synchronize(cx);
    }

    fn perform(&mut self, cx: &mut Cx, action: DashboardAction) {
        let job = match action {
            DashboardAction::Refresh { period } => Some(Job::Snapshot { period, usage_filter: Some(self.dashboard(cx).view_state(cx).usage_filter()) }),
            DashboardAction::RefreshProviders => Some(Job::RefreshProviders),
            DashboardAction::PauseHosting(ticket) => Some(Job::Command { kind: CommandKind::Pause, ticket }),
            DashboardAction::ResumeHosting(ticket) => Some(Job::Command { kind: CommandKind::Resume, ticket }),
            DashboardAction::StopHosting(ticket) => Some(Job::Command { kind: CommandKind::Stop, ticket }),
            DashboardAction::CheckCommand { kind, ticket } => Some(Job::Command { kind, ticket }),
            DashboardAction::OpenSetup => { self.start(cx, JobKind::Setup); None }
            DashboardAction::StartService => { self.start(cx, JobKind::StartService); None }
            DashboardAction::UninstallService => { self.start(cx, JobKind::Uninstall); None }
            DashboardAction::CancelSetup => { if !self.fixture { hosting::cancel(); } None }
            DashboardAction::KeepRunning(keep) => {
                self.pending_keep = Some(keep);
                cx.action(HostingAction::KeepRunning(keep));
                None
            }
            DashboardAction::ReviewApprovals => {
                if !self.fixture && let Some(account) = &self.account
                    && self.dashboard(cx).view_state(cx).owner_id.as_deref() == Some(account.owner.as_str())
                {
                    cx.action(crate::miniapps::MiniAppsAction::ReviewOwnerApprovals { owner: account.owner.clone() });
                }
                None
            }
            DashboardAction::None | DashboardAction::Navigated(_) => None,
        };
        if let Some(job) = job { self.submit(cx, job); }
    }
}

impl Widget for OwnerDashboardHost {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.active || self.connector.is_some() { self.synchronize(cx); self.drain(cx); }
        if self.active && self.refresh.is_event(event).is_some() {
            let view = self.dashboard(cx).view_state(cx);
            self.submit(cx, Job::Snapshot { period: view.period, usage_filter: Some(view.usage_filter()) });
        }
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            for action in self.dashboard(cx).actions(actions) {
                self.perform(cx, action);
            }
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep { self.view.draw_walk(cx, scope, walk) }
}

impl OwnerDashboardHostRef {
    #[cfg(feature = "onboarding-test-support")]
    pub fn fixture_view(&self, cx: &mut Cx) -> Option<ViewState> {
        self.borrow().filter(|inner| inner.fixture).map(|inner| inner.dashboard(cx).view_state(cx))
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn load_fixture(&self, cx: &mut Cx, mut state: owner_dashboard::DashboardState) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.fixture = true;
            inner.connector = None;
            state.locale = locale();
            let dashboard = inner.dashboard(cx);
            dashboard.set_clock(cx, Some(owner_dashboard::fixtures::clock()));
            dashboard.load_state(cx, state);
        }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn update_fixture(&self, cx: &mut Cx, snapshot: owner_dashboard::dto::DashboardSnapshot) {
        if let Some(inner) = self.borrow() {
            if inner.fixture {
                let dashboard = inner.dashboard(cx);
                let period = dashboard.state().and_then(|state| state.snapshot_period);
                dashboard.set_snapshot_for_period(cx, snapshot, period);
            }
        }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn fixture_scope(&self, cx: &mut Cx, owner: Option<&str>) {
        if let Some(inner) = self.borrow() { inner.dashboard(cx).set_owner_scope(cx, owner); }
    }
    #[cfg(feature = "onboarding-test-support")]
    pub fn fixture_locale(&self, cx: &mut Cx, locale: Locale) {
        if let Some(inner) = self.borrow() {
            if inner.fixture { inner.dashboard(cx).set_locale(cx, locale); }
        }
    }
    /// Present a synthetic hosting state through the production mapping
    /// (instrumented harness and tests only).
    #[cfg(any(test, feature = "onboarding-test-support"))]
    pub fn fixture_hosting(&self, cx: &mut Cx, view: &hosting::View, keep_running: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.fixture = true;
            inner.apply(cx, view);
            inner.options(cx, keep_running);
        }
    }
    pub fn set_host_visible(&self, cx: &mut Cx, visible: bool) {
        if let Some(mut inner) = self.borrow_mut() {
            if inner.active != visible {
                inner.active = visible;
                cx.stop_timer(inner.refresh);
                if visible { inner.refresh = cx.start_interval(2.0); }
            }
            // Keep an in-flight command's receiver until it has an outcome.
            // An idle hidden dashboard needs neither a socket nor a worker.
            if !visible && inner.dashboard(cx).state().is_none_or(|state| state.command.as_ref().is_none_or(|command| !command.in_flight())) {
                inner.connector = None;
            }
            if visible { inner.synchronize(cx); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use owner_dashboard::{fixtures, model::CommandPhase};

    fn host() -> (Cx, WidgetRef) {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let widget = cx.with_vm(|vm| {
            makepad_widgets::theme_mod(vm);
            crate::theme::init_standalone(vm);
            makepad_widgets::widgets_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, { mod.widgets.OwnerDashboardHost{} });
            WidgetRef::script_from_value(vm, value)
        });
        (cx, widget)
    }

    #[test]
    fn disconnected_embedded_check_keeps_the_original_uncertain_ticket() {
        let (mut cx, widget) = host();
        let host = widget.borrow::<OwnerDashboardHost>().unwrap();
        let mut state = fixtures::scenario("uncertain").unwrap();
        state.command.as_mut().unwrap().phase = CommandPhase::Checking;
        let original = state.command.as_ref().unwrap().clone();
        let dashboard = host.dashboard(&mut cx);
        dashboard.load_state(&mut cx, state);
        // Exercises the actual absent/full-queue fallback used by the host.
        host.submit(&mut cx, Job::Command { kind: original.kind, ticket: original.ticket.clone() });
        let command = dashboard.state().unwrap().command.unwrap();
        assert_eq!(command.phase, CommandPhase::Uncertain);
        assert_eq!(command.ticket, original.ticket);
    }

    #[test]
    fn hiding_an_idle_host_retires_its_connector_but_retains_uncertain_commands() {
        for uncertain in [false, true] {
            let (mut cx, widget) = host();
            {
                let mut host = widget.borrow_mut::<OwnerDashboardHost>().unwrap();
                host.dashboard(&mut cx).load_state(&mut cx, fixtures::scenario(if uncertain { "uncertain" } else { "running" }).unwrap());
                host.connector = Some(DaemonConnector::spawn(DaemonEndpoint::new(std::env::temp_dir().join("unused-owner-host-test")), OwnerScope::Fixed(fixtures::OWNER.into()), || {}));
            }
            OwnerDashboardHostRef(widget.clone()).set_host_visible(&mut cx, false);
            assert_eq!(widget.borrow::<OwnerDashboardHost>().unwrap().connector.is_some(), uncertain);
        }
    }

    fn state(execution: Option<Result<Execution, String>>, job: Option<(JobKind, Progress)>, generation: u64) -> hosting::View {
        hosting::View { execution, service: Some(hosting::Service::ThisApp), job, generation }
    }

    #[test]
    fn hosting_states_map_to_setup_the_dashboard_or_the_in_app_note() {
        let daemon = || Some(Ok(Execution::Daemon { home: "/h".into() }));
        let setup = |reason| Some(Ok(Execution::Setup(reason)));
        let need = |p: Page| p.setup.map(|s| (s.need, s.progress));
        assert_eq!(need(page(&state(setup(SetupReason::NotEnrolled), None, 1), SetupNeed::NotSetUp)), Some((SetupNeed::NotSetUp, SetupProgress::Idle)));
        assert_eq!(need(page(&state(setup(SetupReason::StaleRoot), None, 1), SetupNeed::NotSetUp)), Some((SetupNeed::Replace, SetupProgress::Idle)));
        assert_eq!(need(page(&state(setup(SetupReason::OtherOwner), None, 1), SetupNeed::NotSetUp)), Some((SetupNeed::OtherOwner, SetupProgress::Idle)));
        let hosted = page(&state(daemon(), None, 1), SetupNeed::NotSetUp);
        assert!(hosted.hosted && hosted.setup.is_none() && !hosted.in_app);
        let in_app = page(&state(Some(Ok(Execution::InProcess)), None, 1), SetupNeed::NotSetUp);
        assert!(in_app.in_app && !in_app.hosted && in_app.setup.is_none());
        // Before the first read nothing is decided.
        assert_eq!(page(&state(None, None, 0), SetupNeed::NotSetUp), Page { setup: None, hosted: false, in_app: false, busy: false, foreign_service: false });
        // A running setup keeps its page after the enrollment flips execution.
        let confirming = page(&state(daemon(), Some((JobKind::Setup, Progress::Confirming)), 2), SetupNeed::Replace);
        assert_eq!(need(confirming.clone()), Some((SetupNeed::Replace, SetupProgress::Confirming)));
        assert!(confirming.busy && !confirming.hosted);
        // A failed sign-in stays on the setup page; a failed service start
        // after enrollment shows the dashboard with a notice instead.
        let failed = page(&state(setup(SetupReason::NotEnrolled), Some((JobKind::Setup, Progress::Failed("daemon_sign_in_cancelled"))), 3), SetupNeed::NotSetUp);
        assert_eq!(need(failed), Some((SetupNeed::NotSetUp, SetupProgress::Failed { code: "daemon_sign_in_cancelled".into() })));
        let installed = page(&state(daemon(), Some((JobKind::Setup, Progress::Failed("service_start_failed"))), 3), SetupNeed::NotSetUp);
        assert!(installed.hosted && installed.setup.is_none());
        assert!(job_notice(&(JobKind::Setup, Progress::Failed("service_start_failed")), false, Locale::En).is_some());
        assert!(job_notice(&(JobKind::Setup, Progress::Failed("daemon_sign_in_cancelled")), true, Locale::En).is_none());
        // A registry or journal error explains itself on the setup page.
        let error = page(&state(Some(Err("owner_root_record_invalid".into())), None, 4), SetupNeed::NotSetUp);
        assert_eq!(need(error), Some((SetupNeed::NotSetUp, SetupProgress::Failed { code: "owner_root_record_invalid".into() })));
    }

    /// The headless check of the Hosting page: setup, running and stopped
    /// states and the keep-running switch, through the production widget.
    #[test]
    fn the_hosting_page_shows_setup_running_and_stopped_states_and_the_switch() {
        let (mut cx, widget) = host();
        let host = OwnerDashboardHostRef(widget.clone());
        let dashboard = widget.borrow::<OwnerDashboardHost>().unwrap().dashboard(&mut cx);
        dashboard.set_clock(&mut cx, Some(fixtures::clock()));
        dashboard.set_owner_scope(&mut cx, Some(fixtures::OWNER));
        let visible = |cx: &Cx, path: &[LiveId]| widget.widget(cx, path).visible();
        // Not set up: the setup page replaces the dashboard.
        host.fixture_hosting(&mut cx, &state(Some(Ok(Execution::Setup(SetupReason::NotEnrolled))), None, 1), true);
        assert!(visible(&cx, ids!(od_setup_page)));
        assert!(!visible(&cx, ids!(od_overview)) && !visible(&cx, ids!(od_options_card)));
        assert!(widget.button(&cx, ids!(od_setup_start)).visible());
        assert_eq!(widget.label(&cx, ids!(od_setup_title)).text(), "Set up background hosting");
        // Signing in: progress and Cancel, no second start.
        host.fixture_hosting(&mut cx, &state(Some(Ok(Execution::Setup(SetupReason::NotEnrolled))), Some((JobKind::Setup, Progress::SigningIn)), 2), true);
        assert!(widget.button(&cx, ids!(od_setup_cancel)).visible() && !widget.button(&cx, ids!(od_setup_start)).visible());
        assert_eq!(widget.label(&cx, ids!(od_setup_progress_text)).text(), "Complete sign-in in your browser…");
        // OAuth is complete: native registration has its own status and cannot
        // be mistaken for another browser prompt or a cancellable OAuth flow.
        host.fixture_hosting(&mut cx, &state(Some(Ok(Execution::Setup(SetupReason::NotEnrolled))), Some((JobKind::Setup, Progress::Registering)), 3), true);
        assert!(!widget.button(&cx, ids!(od_setup_cancel)).visible());
        assert!(!widget.button(&cx, ids!(od_setup_start)).visible());
        assert_eq!(widget.label(&cx, ids!(od_setup_progress_text)).text(), "Signed in. Registering this Mac’s executor…");
        // Another account hosts this Mac: an explanation without a setup button.
        host.fixture_hosting(&mut cx, &state(Some(Ok(Execution::Setup(SetupReason::OtherOwner))), None, 3), true);
        assert!(!widget.button(&cx, ids!(od_setup_start)).visible());
        assert!(widget.label(&cx, ids!(od_setup_body)).text().contains("one account's resource pool"));
        // Running: the dashboard, Stop, and the switch on by default.
        host.fixture_hosting(&mut cx, &state(Some(Ok(Execution::Daemon { home: "/h".into() })), Some((JobKind::Setup, Progress::Done)), 4), true);
        dashboard.set_snapshot(&mut cx, fixtures::ordinary(owner_dashboard::dto::HostingState::Running));
        assert!(!visible(&cx, ids!(od_setup_page)) && visible(&cx, ids!(od_overview)) && visible(&cx, ids!(od_options_card)));
        assert_eq!(widget.label(&cx, ids!(od_hosting_state)).text(), "Hosting is on");
        assert!(widget.button(&cx, ids!(od_stop)).visible() && !widget.button(&cx, ids!(od_resume)).visible());
        assert!(widget.check_box(&cx, ids!(od_keep_running)).active(&cx));
        assert_eq!(widget.label(&cx, ids!(od_keep_running_title)).text(), "Keep the resource pool running after Desktop quits");
        assert!(widget.button(&cx, ids!(od_uninstall)).visible());
        assert!(widget.widget(&cx, ids!(od_notice)).visible(), "setup completion is announced");
        // The saved choice drives the switch.
        host.fixture_hosting(&mut cx, &state(Some(Ok(Execution::Daemon { home: "/h".into() })), None, 5), false);
        assert!(!widget.check_box(&cx, ids!(od_keep_running)).active(&cx));
        // Stopped: Resume replaces Stop; the options stay.
        let mut stopped = fixtures::ordinary(owner_dashboard::dto::HostingState::Stopped);
        stopped.revision += 1;
        dashboard.set_snapshot(&mut cx, stopped);
        assert_eq!(widget.label(&cx, ids!(od_hosting_state)).text(), "Hosting is stopped");
        assert!(widget.button(&cx, ids!(od_resume)).visible() && !widget.button(&cx, ids!(od_stop)).visible());
        assert!(visible(&cx, ids!(od_options_card)));
        // Chinese follows Desktop's language.
        dashboard.set_locale(&mut cx, Locale::ZhHans);
        assert_eq!(widget.label(&cx, ids!(od_keep_running_title)).text(), "关闭桌面端后继续运行资源池");
    }
}
