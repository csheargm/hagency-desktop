//! Hagency Hosting: the standalone owner controller (ADR 0012).
//!
//! A small macOS app around the shared `OwnerDashboard` widget. It attaches
//! to the per-user owner daemon over its private socket and never hosts or
//! executes anything itself: closing or quitting it only detaches, and
//! hosting continues in the daemon. It starts and works while Hagency is
//! closed and links no Rinx, Matrix SDK or Hagency engine code.
//!
//! Options:
//! `--daemon-home=PATH` (or `HAGENCY_OWNER_DAEMON_HOME`): the daemon home
//! containing `control.sock`.
//! With the `fixtures` feature only: `--fixture=NAME`, `--size=WxH`,
//! `--locale=en|zh`, `--appearance=light|dark`, `--text-scale=F`,
//! `--host=embedded`, `--reduced-motion`; see `fixture.rs`.
pub use makepad_widgets;
use makepad_widgets::desktop_style::{self, DesktopStyle, StyleSheet};
use makepad_widgets::*;
use owner_dashboard::dto::period::UsagePeriod;
use owner_dashboard::i18n::{Msg, tr};
use owner_dashboard::handover::Host;
use owner_dashboard::handover_bridge::{Bridge, Event as HandoverEvent};
use owner_dashboard::model::{Glyph, Status, Tone};
use owner_dashboard::transport::{ConnectorEvent, DaemonConnector, DaemonEndpoint, Job, OwnerScope};
use owner_dashboard::{CommandResult, DashboardAction, HostMode, Locale, OwnerDashboardRef, OwnerDashboardWidgetRefExt};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

mod host;
mod system;
mod setup;
#[cfg(target_os = "macos")]
#[path = "../../../crates/article-makepad/src/apple_fonts.rs"]
mod apple_fonts;
#[cfg(feature = "fixtures")]
mod fixture;

app_main!(App);

/// Snapshot polling while attached. The daemon has no push subscription yet;
/// reads are cheap and coalesced, and provider polling stays in the daemon.
const POLL_SECONDS: f64 = 2.0;
const PREFS_SECONDS: f64 = 2.0;

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub daemon_home: Option<PathBuf>,
    pub adopt_root: Option<PathBuf>,
    pub owner: Option<String>,
    pub server: Option<String>,
    pub setup: bool,
    pub setup_request: Option<PathBuf>,
    pub locale: Option<Locale>,
    pub dark: Option<bool>,
    pub text_scale: Option<f64>,
    pub size: Option<(f64, f64)>,
    pub host: Option<HostMode>,
    pub fixture: Option<String>,
    pub reduced_motion: Option<bool>,
}

pub fn options() -> &'static Options {
    static OPTIONS: OnceLock<Options> = OnceLock::new();
    OPTIONS.get_or_init(|| {
        let mut o = Options::default();
        let mut args = std::env::args().skip(1).peekable();
        while let Some(arg) = args.next() {
            let (key, inline) = arg.split_once('=').unwrap_or((arg.as_str(), ""));
            let value = if inline.is_empty() && key != "--setup" && key != "--reduced-motion" {
                args.next_if(|next| !next.starts_with("--")).unwrap_or_default()
            } else { inline.to_owned() };
            let value = value.as_str();
            match key {
                "--daemon-home" if !value.is_empty() => o.daemon_home = Some(PathBuf::from(value)),
                "--adopt-root" if !value.is_empty() => o.adopt_root = Some(PathBuf::from(value)),
                "--owner" if !value.is_empty() => o.owner = Some(value.into()),
                "--server" if !value.is_empty() => o.server = Some(value.into()),
                "--setup" => o.setup = true,
                "--setup-request" if !value.is_empty() => o.setup_request = Some(PathBuf::from(value)),
                #[cfg(feature = "fixtures")]
                _ => fixture::parse_option(&mut o, key, value),
                #[cfg(not(feature = "fixtures"))]
                _ => {}
            }
        }
        if o.daemon_home.is_none() {
            o.daemon_home = std::env::var_os("HAGENCY_OWNER_DAEMON_HOME").map(PathBuf::from);
        }
        o
    })
}

/// Appearance chosen for the next (re)registration of the widget styles.
static DARK: AtomicBool = AtomicBool::new(false);

fn initial_size() -> Vec2d {
    let (w, h) = options().size.unwrap_or((520.0, 720.0));
    dvec2(w, h)
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    let app = startup() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.title: "Hagency Hosting"
                window.inner_size: #(initial_size())
                body +: {
                    flow: Down
                    View {
                        width: Fill height: Fit flow: Right align: Align{y: 0.5}
                        padding: Inset{left: 20 right: 20 top: 8 bottom: 4} spacing: 10
                        show_bg: true draw_bg.color: OD_PAGE
                        View {width: Fill height: 1}
                        language_label := OdCaption {width: Fit text: "Language"}
                        hosting_language := OdDropDown {
                            width: 172 height: 36 align: Align{y: 0.5}
                            labels: ["System default" "English" "简体中文"]
                            draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_CAPTION}}
                        }
                    }
                    View {
                    width: Fill height: Fill flow: Overlay
                    dashboard := OwnerDashboard {}
                    setup_page := OdScrollY {
                        visible: false width: Fill height: Fill
                        show_bg: true draw_bg.color: OD_PAGE
                        flow: Down spacing: 20 padding: Inset{left: 24 right: 24 top: 28 bottom: 24}
                        setup_title := OdLargeTitle {text: "Keep your Agents available"}
                        setup_description := OdText {text: "Host resources in the background, even when Hagency is closed."}
                        OdCard {
                            setup_server_label := OdHeadline {text: "Matrix server"}
                            setup_server := TextInput {width: Fill height: OD_CONTROL empty_text: "https://your-server.example" draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_BODY}}}
                            setup_server_error := OdCaption {visible: false draw_text.color: OD_CRI_FG}
                            setup_owner_label := OdHeadline {text: "Owner account (optional)"}
                            setup_owner := TextInput {width: Fill height: OD_CONTROL empty_text: "@name:your-server.example" draw_text +: {text_style: theme.font_regular{font_size: OD_SIZE_BODY}}}
                            setup_owner_error := OdCaption {visible: false draw_text.color: OD_CRI_FG}
                            setup_privacy := OdCaption {text: "Your browser handles sign-in. This app never asks for your password."}
                        }
                        OdCard {
                            setup_background := OdHeadline {text: "Background hosting"}
                            setup_explanation := OdText {text: "Enabling hosting starts a service when you sign in to this Mac. Closing this dashboard leaves your Agents available. You can pause or stop hosting at any time."}
                        }
                        setup_status := OdText {text: ""}
                        setup_enable := OdPrimaryButton {width: Fill text: "Enable hosting"}
                        setup_back := OdButton {width: Fill text: "Back"}
                    }
                    }
                }
            }
        }
    }
    app
}

impl App {
    pub fn dashboard(&self, cx: &Cx) -> OwnerDashboardRef {
        self.ui.owner_dashboard(cx, ids!(dashboard))
    }

    fn submit(&mut self, job: Job) -> bool {
        self.connector.as_ref().is_some_and(|c| c.submit(job))
    }

    fn refresh(&mut self, cx: &Cx) {
        let usage_filter = Some(self.dashboard(cx).view_state(cx).usage_filter());
        self.submit(Job::Snapshot { period: self.period, usage_filter });
    }

    /// Install a new appearance and re-register styles in place.
    pub fn set_dark(&mut self, cx: &mut Cx, dark: bool) {
        if DARK.swap(dark, Ordering::Relaxed) != dark {
            cx.with_vm(|vm| desktop_style::install(vm, StyleSheet::load_with_appearance(DesktopStyle::Macos, dark)));
            cx.request_style_reload();
        }
    }

    fn check_prefs(&mut self, cx: &mut Cx) {
        let prefs = system::SystemPrefs::read();
        let o = options();
        self.set_dark(cx, o.dark.unwrap_or(prefs.dark));
        let dashboard = self.dashboard(cx);
        dashboard.set_reduced_motion(cx, o.reduced_motion.unwrap_or(prefs.reduced_motion));
        let locale = o.locale.unwrap_or(prefs.locale);
        if dashboard.locale() != locale {
            dashboard.set_locale(cx, locale);
            self.localize_setup(cx);
            self.ui.window(cx, ids!(main_window)).set_title(cx, tr(locale, Msg::WindowTitle));
        }
        self.sync_language_control(cx, prefs.language, locale);
    }

    fn sync_language_control(&mut self, cx: &mut Cx, choice: system::LanguagePreference, locale: Locale) {
        if self.language_control == Some((choice, locale)) { return; }
        let zh = locale == Locale::ZhHans;
        self.ui.label(cx, ids!(language_label)).set_text(cx, if zh { "语言" } else { "Language" });
        let control = self.ui.drop_down(cx, ids!(hosting_language));
        control.set_labels(cx, vec![if zh { "跟随系统" } else { "System default" }.into(), "English".into(), "简体中文".into()]);
        control.set_selected_item(cx, choice.index());
        self.language_control = Some((choice, locale));
    }

    fn drain(&mut self, cx: &mut Cx) {
        let Some(connector) = self.connector.as_ref() else { return };
        let mut events = Vec::new();
        while let Some(event) = connector.try_recv() {
            events.push(event);
        }
        let dashboard = self.dashboard(cx);
        for event in events {
            match event {
                ConnectorEvent::Owner(owner) => {
                    if owner != self.owner {
                        dashboard.set_owner_scope(cx, owner.as_deref());
                        self.bridge = Bridge::spawn(self.home(), Host::Standalone, SignalToUI::set_ui_signal).ok();
                        if let Some(bridge) = &self.bridge { bridge.set_owner(owner.clone()); }
                        self.view_generation = self.view_generation.wrapping_add(1);
                        if let (Some(store), Some(owner)) = (&self.view_store, &owner) { store.load(owner.clone(), self.view_generation); }
                        self.owner = owner;
                        self.dismiss_after_handover = None;
                    }
                }
                ConnectorEvent::Connection(status) => dashboard.set_connection(cx, status),
                ConnectorEvent::PendingCommand(command) => dashboard.recover_command(cx, command),
                ConnectorEvent::Snapshot { snapshot, period } => {
                    if self.setup_waiting_since.is_some_and(|since| snapshot.observed_at_ms >= since)
                        && snapshot.owner.as_ref().is_some_and(|owner|
                            self.setup_expected_owner.as_ref().is_none_or(|expected| expected == &owner.user_id)
                            && self.setup_expected_server.as_ref().is_some_and(|expected| expected.trim_end_matches('/') == owner.server.trim_end_matches('/')))
                    {
                        use owner_dashboard::dto::HostingState;
                        if matches!(snapshot.hosting.state, HostingState::Running | HostingState::Paused) {
                            self.setup_waiting_since = None;
                            dashboard.set_notice(cx, Some(Status::new(Tone::Positive, Glyph::Check,
                                if dashboard.locale() == Locale::ZhHans { "后台托管已确认启用。" } else { "Background hosting is confirmed." })));
                        } else if matches!(snapshot.hosting.state, HostingState::Failed | HostingState::Offline) {
                            self.setup_waiting_since = None;
                            dashboard.set_notice(cx, None);
                        }
                    }
                    dashboard.set_snapshot_for_period(cx, snapshot, period);
                }
                ConnectorEvent::CommandResult { request_id, result } => {
                    log!("od-command-result {request_id} {result:?}");
                    dashboard.command_result(cx, &request_id, result);
                }
            }
        }
    }

    fn show_setup(&mut self, cx: &mut Cx) {
        self.ui.widget(cx, ids!(dashboard)).set_visible(cx, false);
        self.ui.widget(cx, ids!(setup_page)).set_visible(cx, true);
        if !self.setup_initialized {
            if let Some(server) = &options().server { self.ui.text_input(cx, ids!(setup_server)).set_text(cx, server); }
            if let Some(owner) = options().owner.as_ref().or(self.owner.as_ref()) { self.ui.text_input(cx, ids!(setup_owner)).set_text(cx, owner); }
            self.setup_initialized = true;
        }
        self.localize_setup(cx);
        self.lock_setup_fields(cx);
        self.ui.redraw(cx);
    }

    fn lock_setup_fields(&self, cx: &mut Cx) {
        let locked = self.setup_job.is_some() || self.setup_install_only || options().adopt_root.is_some();
        self.ui.text_input(cx, ids!(setup_server)).set_is_read_only(cx, locked);
        self.ui.text_input(cx, ids!(setup_owner)).set_is_read_only(cx, locked || options().owner.is_some());
    }

    fn localize_setup(&mut self, cx: &mut Cx) {
        let zh = self.dashboard(cx).locale() == Locale::ZhHans;
        for (path, en, cn) in [
            (ids!(setup_title), "Keep your Agents available", "让 Agent 持续在线"),
            (ids!(setup_description), "Host resources in the background, even when Hagency is closed.", "在后台托管资源，关闭 Hagency 后仍可提供服务。"),
            (ids!(setup_server_label), "Matrix server", "Matrix 服务器"),
            (ids!(setup_owner_label), "Owner account (optional)", "资源主人账号（可选）"),
            (ids!(setup_privacy), "Your browser handles sign-in. This app never asks for your password.", "在浏览器中安全登录，无需在此输入密码。"),
            (ids!(setup_background), "Background hosting", "后台托管"),
            (ids!(setup_explanation), "Enabling hosting starts a service when you sign in to this Mac. Closing this dashboard leaves your Agents available. You can pause or stop hosting at any time.", "启用后，服务会在您登录 Mac 时启动。关闭控制面板后 Agent 仍可响应。您可以随时暂停或停止托管。"),
            (ids!(setup_server_error), "Enter a valid HTTPS server address (HTTP only for localhost).", "请输入有效的 HTTPS 服务器地址（仅 localhost 可用 HTTP）。"),
            (ids!(setup_owner_error), "Use @name:server, or leave this empty to choose in the browser.", "请使用 @name:server 格式，或留空以在浏览器中选择账号。"),
        ] { self.ui.label(cx, path).set_text(cx, if zh { cn } else { en }); }
        self.ui.button(cx, ids!(setup_enable)).set_text(cx, match (self.setup_install_only, zh) {
            (true, true) => "重试启动服务", (true, false) => "Retry starting service",
            (false, true) => "启用托管", (false, false) => "Enable hosting",
        });
        self.ui.button(cx, ids!(setup_back)).set_text(cx, if zh { "返回" } else { "Back" });
        if options().adopt_root.is_some() {
            self.ui.label(cx, ids!(setup_explanation)).set_text(cx, if zh {
                "启用时会将当前账号的 Agent 和资源移交给后台服务，短暂暂停接收新任务。原有身份和数据会保留。若登录未完成，请回到这里重试以完成移交。"
            } else {
                "Enable transfers this account’s existing Agents and resources to background hosting, briefly pausing new work. Their identity and data are preserved. If sign-in is interrupted, return here and retry to finish the transfer."
            });
        }
    }

    fn begin_setup(&mut self, cx: &mut Cx) {
        if self.setup_job.is_some() { return; }
        let server = self.ui.text_input(cx, ids!(setup_server)).text().trim().to_owned();
        let owner = self.ui.text_input(cx, ids!(setup_owner)).text().trim().to_owned();
        let zh = self.dashboard(cx).locale() == Locale::ZhHans;
        let server_valid = setup::valid_server(&server);
        let owner_valid = setup::valid_owner(&owner);
        self.ui.widget(cx, ids!(setup_server_error)).set_visible(cx, !server_valid);
        self.ui.widget(cx, ids!(setup_owner_error)).set_visible(cx, !owner_valid);
        if !server_valid || !owner_valid {
            let field = self.ui.widget(cx, if !server_valid { ids!(setup_server) } else { ids!(setup_owner) });
            owner_dashboard::keyboard::focus(cx, field.area());
            self.ui.redraw(cx);
            return;
        }
        self.setup_expected_owner = (!owner.is_empty()).then_some(owner.clone());
        self.setup_expected_server = url::Url::parse(&server).ok().map(|url| url.to_string());
        #[cfg(feature = "fixtures")]
        if fixture::active() {
            self.ui.label(cx, ids!(setup_status)).set_text(cx, if zh { "请在浏览器中完成登录…" } else { "Complete sign-in in your browser…" });
            return;
        }
        self.setup_job = Some(setup::Setup::start(self.home(), options().adopt_root.clone(), server, (!owner.is_empty()).then_some(owner), self.setup_install_only, SignalToUI::set_ui_signal));
        self.lock_setup_fields(cx);
        self.ui.button(cx, ids!(setup_enable)).set_enabled(cx, false);
    }

    fn drain_setup(&mut self, cx: &mut Cx) {
        let mut events = Vec::new();
        if let Some(job) = &self.setup_job { while let Some(event) = job.try_recv() { events.push(event); } }
        let zh = self.dashboard(cx).locale() == Locale::ZhHans;
        for event in events {
            let install_failure = matches!(&event, setup::Progress::InstallFailed(_));
            let text = match event {
                setup::Progress::SigningIn => if zh { "请在浏览器中完成登录…" } else { "Complete sign-in in your browser…" },
                setup::Progress::FinishingCancellation => if zh { "设置已超时，正在安全结束原操作。完成后即可重试…" } else { "Setup timed out. Finishing the original operation safely before retry is available…" },
                setup::Progress::Enabling => if zh { "正在启用后台服务…" } else { "Enabling background hosting…" },
                setup::Progress::Installed => {
                    self.setup_job = None;
                    self.setup_waiting_since = Some(owner_dashboard::Clock::system().now_ms);
                    let dashboard = self.dashboard(cx);
                    dashboard.set_notice(cx, Some(Status::new(Tone::Progress, Glyph::Clock,
                        if zh { "服务已安装，正在确认托管状态…" } else { "Service installed. Confirming hosting status…" })));
                    self.ui.button(cx, ids!(setup_enable)).set_enabled(cx, true);
                    self.ui.widget(cx, ids!(setup_page)).set_visible(cx, false);
                    self.ui.widget(cx, ids!(dashboard)).set_visible(cx, true);
                    self.refresh(cx);
                    if zh { "服务已安装，正在确认托管状态…" } else { "Service installed. Confirming hosting status…" }
                }
                setup::Progress::Failed(code) | setup::Progress::InstallFailed(code) => {
                    self.setup_job = None;
                    self.setup_install_only = install_failure;
                    self.localize_setup(cx);
                    self.ui.button(cx, ids!(setup_enable)).set_enabled(cx, true);
                    match (code, zh) {
                        ("helper_missing", true) => "安装不完整，找不到后台服务。请重新安装 Hagency Hosting。",
                        ("helper_missing", false) => "The background service is missing. Reinstall Hagency Hosting.",
                        ("service_start_failed", true) => "已登录，但尚未确认服务启动结果。请检查系统设置中的登录项或重试。",
                        ("service_start_failed", false) => "Signed in, but service startup was not confirmed. Check Login Items in System Settings or retry.",
                        (_, zh) => setup::failure_message(code, zh),
                    }
                }
            };
            self.ui.label(cx, ids!(setup_status)).set_text(cx, text);
            self.lock_setup_fields(cx);
        }
    }

    fn home(&self) -> PathBuf { options().daemon_home.clone().unwrap_or_else(host::default_daemon_home) }

    fn drain_handover(&mut self, cx: &mut Cx) {
        let dashboard = self.dashboard(cx);
        if let Some(store) = &self.view_store {
            while let Some((generation, view)) = store.try_recv() {
                if generation == self.view_generation && view.owner_id == self.owner && dashboard.restore_view_state(cx, view.clone()) {
                    self.period = view.period;
                }
            }
        }
        if let Some(bridge) = &self.bridge {
            while let Some(event) = bridge.try_recv() {
                match event {
                    HandoverEvent::Restoring { id, view } => {
                        if view.intent.is_none() && view.owner_id == self.owner && dashboard.restore_view_state(cx, view.clone()) {
                            self.view_generation = self.view_generation.wrapping_add(1);
                            self.period = view.period;
                            bridge.restored(id);
                        } else { bridge.rejected(id); }
                    }
                    HandoverEvent::Opening => dashboard.set_notice(cx, Some(Status::new(Tone::Progress, Glyph::Clock, tr(dashboard.locale(), Msg::OpeningHagency)))),
                    HandoverEvent::Completed => {
                        dashboard.set_notice(cx, None);
                        if self.dismiss_after_handover.take() == Some(true) {
                            // The other process has restored this owner's view.
                            // Quitting this controller never stops the daemon.
                            cx.request_quit(QuitReason::App);
                        }
                    },
                    HandoverEvent::Failed(_) => {
                        self.dismiss_after_handover = None;
                        dashboard.set_notice(cx, Some(Status::new(Tone::Critical, Glyph::Cross,
                            if dashboard.locale() == Locale::ZhHans { "Hagency 未确认接收。当前页面已保留，请重试。" } else { "Hagency did not confirm the move. Your view is still here. Please retry." })));
                    },
                }
            }
        }
    }

    fn perform(&mut self, cx: &mut Cx, action: DashboardAction) {
        log!("od-action {}", action_summary(&action));
        #[cfg(feature = "fixtures")]
        if fixture::active() {
            return fixture::perform(self, cx, action);
        }
        let dashboard = self.dashboard(cx);
        match action {
            DashboardAction::None => {}
            DashboardAction::Refresh { period } => {
                self.period = period;
                self.refresh(cx);
            }
            DashboardAction::RefreshProviders => {
                self.submit(Job::RefreshProviders);
            }
            DashboardAction::PauseHosting(ref ticket)
            | DashboardAction::ResumeHosting(ref ticket)
            | DashboardAction::StopHosting(ref ticket) => {
                let kind = match &action {
                    DashboardAction::PauseHosting(_) => owner_dashboard::CommandKind::Pause,
                    DashboardAction::ResumeHosting(_) => owner_dashboard::CommandKind::Resume,
                    _ => owner_dashboard::CommandKind::Stop,
                };
                let id = ticket.request_id.clone();
                if !self.submit(Job::Command { kind, ticket: ticket.clone() }) {
                    // Never queued, so never sent: definitive and retryable.
                    let busy = CommandResult::NotSubmitted { code: "busy".into(), retryable: true };
                    dashboard.command_result(cx, &id, busy);
                }
            }
            DashboardAction::CheckCommand { kind, ticket } => {
                let id = ticket.request_id.clone();
                if !self.submit(Job::Command { kind, ticket }) {
                    dashboard.command_result(cx, &id, CommandResult::Uncertain { message: String::new() });
                }
            }
            DashboardAction::OpenInHagency(view) => self.offer_handover(cx, view, true),
            DashboardAction::ReviewApprovals => {
                let mut view = dashboard.view_state(cx);
                if view.owner_id.is_some() && view.owner_id == self.owner {
                    view.intent = Some(owner_dashboard::NavigationIntent::ReviewApprovals);
                    self.offer_handover(cx, view, false);
                }
            }
            DashboardAction::OpenSetup => {
                self.show_setup(cx);
            }
            // Already the standalone app.
            DashboardAction::OpenStandalone(_) => {}
            DashboardAction::Navigated(view) => {
                self.view_generation = self.view_generation.wrapping_add(1);
                if let Some(owner) = &self.owner {
                    if let Some(store) = &self.view_store { store.save(owner.clone(), view); }
                }
            }
        }
    }

    fn offer_handover(&mut self, cx: &mut Cx, view: owner_dashboard::ViewState, dismiss: bool) {
        if self.dismiss_after_handover.is_some() { return; }
        if self.bridge.as_ref().is_some_and(|bridge| bridge.offer(view)) {
            self.dismiss_after_handover = Some(dismiss);
        } else {
            let dashboard = self.dashboard(cx);
            dashboard.set_notice(cx, Some(Status::new(Tone::Critical, Glyph::Cross,
                if dashboard.locale() == Locale::ZhHans { "无法打开 Hagency。当前页面已保留，请重试。" }
                else { "Hagency could not be opened. Your view is still here. Please retry." })));
        }
    }
}

/// Action kind and request ID only, for instrumented evidence in the log.
pub fn action_summary(action: &DashboardAction) -> String {
    match action {
        DashboardAction::None => "none".into(),
        DashboardAction::Refresh { period } => format!("refresh period={period:?}"),
        DashboardAction::RefreshProviders => "refresh_providers".into(),
        DashboardAction::PauseHosting(t) => format!("pause_hosting id={} rev={:?}", t.request_id, t.expected_revision),
        DashboardAction::ResumeHosting(t) => format!("resume_hosting id={} rev={:?}", t.request_id, t.expected_revision),
        DashboardAction::StopHosting(t) => format!("stop_hosting id={} rev={:?}", t.request_id, t.expected_revision),
        DashboardAction::CheckCommand { kind, ticket } => format!("check_command kind={kind:?} id={}", ticket.request_id),
        DashboardAction::OpenInHagency(v) => format!("open_in_hagency tab={:?}", v.tab),
        DashboardAction::OpenStandalone(v) => format!("open_standalone tab={:?}", v.tab),
        DashboardAction::OpenSetup => "open_setup".into(),
        DashboardAction::ReviewApprovals => "review_approvals".into(),
        DashboardAction::Navigated(v) => format!("navigated tab={:?} resource={:?} filter={:?}", v.tab, v.selected_resource, v.account_filter),
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[rust]
    connector: Option<DaemonConnector>,
    #[rust]
    bridge: Option<Bridge>,
    #[rust]
    dismiss_after_handover: Option<bool>,
    #[rust]
    view_store: Option<host::ViewStore>,
    #[rust]
    view_generation: u64,
    #[rust]
    setup_job: Option<setup::Setup>,
    #[rust]
    setup_initialized: bool,
    #[rust]
    setup_install_only: bool,
    #[rust]
    setup_waiting_since: Option<u64>,
    #[rust]
    setup_expected_owner: Option<String>,
    #[rust]
    setup_expected_server: Option<String>,
    #[rust]
    owner: Option<String>,
    #[rust]
    period: Option<UsagePeriod>,
    #[rust]
    poll_timer: Timer,
    #[rust]
    prefs_timer: Timer,
    #[rust]
    language_control: Option<(system::LanguagePreference, Locale)>,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let prefs = system::SystemPrefs::read();
        let o = options();
        let dashboard = self.dashboard(cx);
        dashboard.set_host(cx, o.host.unwrap_or(HostMode::Standalone));
        let locale = o.locale.unwrap_or(prefs.locale);
        dashboard.set_locale(cx, locale);
        self.sync_language_control(cx, prefs.language, locale);
        self.ui.window(cx, ids!(main_window)).set_title(cx, tr(locale, Msg::WindowTitle));
        dashboard.set_reduced_motion(cx, o.reduced_motion.unwrap_or(prefs.reduced_motion));
        self.set_dark(cx, o.dark.unwrap_or(prefs.dark));
        #[cfg(feature = "fixtures")]
        if fixture::start(self, cx) {
            return;
        }
        // System appearance, motion and language are followed live.
        self.prefs_timer = cx.start_interval(PREFS_SECONDS);
        let home = o.daemon_home.clone().unwrap_or_else(host::default_daemon_home);
        self.view_store = Some(host::ViewStore::spawn(home.clone(), SignalToUI::set_ui_signal));
        self.connector = Some(DaemonConnector::spawn(DaemonEndpoint::new(home), options().owner.clone().map(OwnerScope::Fixed).unwrap_or(OwnerScope::Discover), SignalToUI::set_ui_signal));
        if o.setup {
            self.show_setup(cx);
            if let (Some(request), Some(owner), Some(server), Some(root)) =
                (o.setup_request.clone(), o.owner.clone(), o.server.clone(), o.adopt_root.clone())
            {
                let home = self.home();
                std::thread::spawn(move || {
                    let _ = owner_dashboard::setup_launch::acknowledge(home, request, owner, server, root, owner_dashboard::Clock::system().now_ms);
                });
            }
        }
        self.refresh(cx);
        self.poll_timer = cx.start_interval(POLL_SECONDS);
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if let Some(index) = self.ui.drop_down(cx, ids!(hosting_language)).changed(actions) {
            if system::LanguagePreference::from_index(index).save() {
                self.check_prefs(cx);
            } else {
                let dashboard = self.dashboard(cx);
                dashboard.set_notice(cx, Some(Status::new(Tone::Critical, Glyph::Warning,
                    if dashboard.locale() == Locale::ZhHans { "无法保存语言设置，请重试。" }
                    else { "Could not save the language. Please try again." })));
                self.language_control = None;
                self.check_prefs(cx);
            }
        }
        if self.ui.button(cx, ids!(setup_enable)).clicked(actions) { self.begin_setup(cx); }
        if self.ui.button(cx, ids!(setup_back)).clicked(actions) {
            self.setup_job = None;
            self.lock_setup_fields(cx);
            self.ui.button(cx, ids!(setup_enable)).set_enabled(cx, true);
            self.ui.widget(cx, ids!(setup_page)).set_visible(cx, false);
            self.ui.widget(cx, ids!(dashboard)).set_visible(cx, true);
        }
        let dashboard = self.dashboard(cx);
        for action in dashboard.actions(actions) {
            self.perform(cx, action);
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        static APPEARANCE_INIT: OnceLock<()> = OnceLock::new();
        APPEARANCE_INIT.get_or_init(|| DARK.store(options().dark.unwrap_or_else(|| system::SystemPrefs::read().dark), Ordering::Relaxed));
        let dark = DARK.load(Ordering::Relaxed);
        makepad_widgets::theme_mod(vm);
        desktop_style::install(vm, StyleSheet::load_with_appearance(DesktopStyle::Macos, dark));
        #[cfg(target_os = "macos")]
        apple_fonts::install(vm);
        if let Some(scale) = options().text_scale {
            vm.cx_mut().global::<owner_dashboard::theme::DashboardTextScale>().0 = scale.clamp(0.75, 3.0);
        }
        makepad_widgets::widgets_mod(vm);
        desktop_style::apply_widgets(vm);
        #[cfg(feature = "fixtures")]
        if options().fixture.is_some() {
            if let Ok(scale) = std::env::var("HAGENCY_OWNER_REVIEW_HOST_SCALE").unwrap_or_default().parse::<f64>() {
                let scale = scale.clamp(0.75, 3.0);
                // widgets_mod applies the stylesheet theme; override its
                // fixture typography only after that replacement completes.
                script_eval!(vm, { mod.theme.octo_typography_scale = #(scale) });
            }
        }
        owner_dashboard::script_mod(vm);
        self::script_mod(vm)
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Event::KeyDown(key) = event {
            if owner_dashboard::keyboard::handle(&self.ui, cx, key, true) { return; }
        }
        match event {
            Event::Signal => { self.drain(cx); self.drain_handover(cx); self.drain_setup(cx); },
            Event::WindowGotFocus(_) => self.refresh(cx),
            #[cfg(feature = "fixtures")]
            Event::Custom(data) => fixture::command(self, cx, data),
            _ => {}
        }
        if self.poll_timer.is_event(event).is_some() {
            if self.setup_waiting_since.is_some_and(|since| owner_dashboard::Clock::system().now_ms.saturating_sub(since) >= 45_000) {
                self.setup_waiting_since = None;
                let dashboard = self.dashboard(cx);
                dashboard.set_notice(cx, Some(Status::new(Tone::Caution, Glyph::Clock,
                    if dashboard.locale() == Locale::ZhHans { "尚未确认后台服务就绪。请刷新查看最新状态，或重试设置。" }
                    else { "Hosting readiness is not confirmed. Refresh to check its status, or retry setup." })));
            }
            self.refresh(cx);
        }
        if self.prefs_timer.is_event(event).is_some() {
            self.check_prefs(cx);
        }
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
        owner_dashboard::accessibility::after_event(&self.ui, cx, event);
    }
}
