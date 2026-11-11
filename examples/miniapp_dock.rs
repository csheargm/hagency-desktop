//! Offline visual exercise for native readers/editors in the production mini-app dock.
//! Uses an isolated profile; no Matrix client, executor or publication is started.
pub use makepad_widgets;
use makepad_widgets::*;
use rinx::miniapps::{MiniAppsAction, side::{MiniAppsSideAction, MiniAppsSideRegionWidgetRefExt}, window::{MiniAppsWindowAction, MiniAppsWindowHostWidgetRefExt}};
use rinx::shared::web_browser::WebBrowserAction;
use rinx::article_app::ArticleAction;
use rinx::home::sidebar_action_menu::{SidebarActionMenuWidgetRefExt, SidebarMenuKind, new_or_join_menu_pos};
use rinx::shared::navigation_bar_button::NavigationBarButtonWidgetRefExt;
use rinx::theme::{self, Accent, Appearance, Selection};
#[cfg(all(feature = "onboarding-test-support", target_os = "macos"))]
#[path = "support/native_webview_probe.rs"]
mod native_webview_probe;
app_main!(App);
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {
            popped := MiniAppsWindowHost {}
            main_window := Window {
                window.inner_size: vec2(1200, 850)
                window.title: "Hagency · Dock review · Offline"
                body +: {flow: Overlay
                    main := View {width: Fill height: Fill flow: Right show_bg: true draw_bg.color: RINX_PAGE
                    navigation := NavigationTabBar {}
                    View {width: Fill height: Fill flow: Down padding: 16 spacing: 12
                    View {width: Fill height: Fit flow: Right spacing: 12
                        apps := RinxPrimaryButton {text: "Mini apps"}
                        web := RinxPrimaryButton {text: "Web page"}
                        read := RinxPrimaryButton {text: "Read article"}
                        edit := RinxPrimaryButton {text: "Article editor"}
                        plus := NewOrJoinButton {}
                    }
                    View {width: Fill height: Fill flow: Right
                        View {width: Fill height: Fill flow: Down spacing: 16
                            RinxPageTitle {text: "Conversation"}
                            RinxLabel {text: "The conversation remains open beside native tabs."}
                            draft := RinxInput {text: "Unsent chat draft"}
                        }
                        dock := MiniAppsSideRegion {}
                    }
                    }
                    }
                    menu := SidebarActionMenu {}
                }
            }
        }
    }
}
#[derive(Script, ScriptHook)]
struct App {
    #[live] ui: WidgetRef,
}
impl App {
    fn prepare_dock(&self, cx: &mut Cx) {
        let width = self.ui.window(cx, ids!(main_window)).get_inner_size(cx).x;
        let navigation = self.ui.widget(cx, ids!(navigation)).area().rect(cx).size.x;
        let page = (width - navigation - 32.0).max(1.0);
        self.ui.mini_apps_side_region(cx, ids!(dock)).prepare(cx, page, 0.0, true,
            Some(std::env::var("HAGENCY_OWNER_REVIEW_WIDTH").ok().and_then(|v| v.parse().ok()).unwrap_or(760.0)));
    }
    #[cfg(feature = "onboarding-test-support")]
    fn resize_fixture(&self, cx: &mut Cx, size: &str) {
        if let Some((w,h)) = size.split_once('x').and_then(|(w,h)| Some((w.parse::<f64>().ok()?,h.parse::<f64>().ok()?))) {
            if w.is_finite() && h.is_finite() && (300.0..=2400.0).contains(&w) && (400.0..=1800.0).contains(&h) {
                self.ui.window(cx, ids!(main_window)).resize(cx, dvec2(w,h));
                self.ui.redraw(cx);
            }
        }
    }
    #[cfg(feature = "onboarding-test-support")]
    fn owner_fixture(&self, cx: &mut Cx, scenario: &str) {
        let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
        dock.open(cx, &MiniAppsAction::OpenAgent);
        if let Some(state) = owner_dashboard::fixtures::scenario(scenario) {
            if let Some(id) = dock.selected() {
                dock.surface(cx, id).owner_fixture(cx, state);
            }
        }
    }
    /// A synthetic hosting state through the Hosting page's production
    /// mapping: `setup`, `setup-replace`, `setup-other-owner`,
    /// `setup-signing-in`, `running`, `running-keep-off` or `stopped`.
    #[cfg(feature = "onboarding-test-support")]
    fn hosting_fixture(&self, cx: &mut Cx, state: &str) {
        use rinx::hagency::hosting::{Execution, JobKind, Progress, SetupReason, View};
        let setup = |reason| Some(Ok(Execution::Setup(reason)));
        let daemon = || Some(Ok(Execution::Daemon { home: "/fixture/owner-daemon".into() }));
        let (execution, job, scenario, keep_running) = match state {
            "setup" => (setup(SetupReason::NotEnrolled), None, None, true),
            "setup-replace" => (setup(SetupReason::StaleRoot), None, None, true),
            "setup-other-owner" => (setup(SetupReason::OtherOwner), None, None, true),
            "setup-signing-in" => (setup(SetupReason::NotEnrolled), Some((JobKind::Setup, Progress::SigningIn)), None, true),
            "setup-registering" => (setup(SetupReason::NotEnrolled), Some((JobKind::Setup, Progress::Registering)), None, true),
            "running" => (daemon(), None, Some("running"), true),
            "running-keep-off" => (daemon(), None, Some("running"), false),
            "stopped" => (daemon(), None, Some("stopped"), true),
            _ => return,
        };
        let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
        dock.open(cx, &MiniAppsAction::OpenAgent);
        let Some(id) = dock.selected() else { return };
        let surface = dock.surface(cx, id);
        if let Some(state) = scenario.and_then(owner_dashboard::fixtures::scenario) {
            surface.owner_fixture(cx, state);
        }
        let view = View { execution, service: Some(rinx::hagency::hosting::Service::ThisApp), job, generation: 1 };
        surface.owner_fixture_hosting(cx, &view, keep_running);
    }
}
impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
        let host = self.ui.mini_apps_window_host(cx, ids!(popped));
        let plus = self.ui.navigation_bar_button(cx, ids!(plus));
        if plus.clicked(actions) {
            let position = new_or_join_menu_pos(cx, plus.area().rect(cx));
            self.ui.sidebar_action_menu(cx, ids!(menu)).show(cx, SidebarMenuKind::Add, position,
                Rect {pos: dvec2(0.0, 0.0), size: dvec2(1200.0, 850.0)});
        }
        if self.ui.button(cx, ids!(apps)).clicked(actions) {
            if host.is_open() {host.action(cx, &MiniAppsAction::Open);} else {dock.open(cx, &MiniAppsAction::Open);}
        }
        if self.ui.button(cx, ids!(web)).clicked(actions) {
            let url = std::env::var("HAGENCY_DOCK_URL").unwrap_or_else(|_| "https://example.com/".into());
            let action = WebBrowserAction::Open(url.parse().unwrap());
            if host.is_open() {host.web_action(cx, &action);} else {dock.web_action(cx, &action);}
        }
        if self.ui.button(cx, ids!(read)).clicked(actions) {
            let action = WebBrowserAction::OpenMarkdown {title: "Docked article".into(), source: "# Docked article\n\nReading keeps the conversation and editor draft open.\n\n## Native Markdown\n\n- Switch tabs\n- Pop out\n- Dock back\n".into(), account: None, attachment: None};
            if host.is_open() {host.web_action(cx, &action);} else {dock.web_action(cx, &action);}
        }
        if self.ui.button(cx, ids!(edit)).clicked(actions) {
            if host.is_open() {host.article_action(cx, &ArticleAction::Open);} else {dock.article_action(cx, &ArticleAction::Open);}
        }
        for action in actions {
            if let Some(action) = action.downcast_ref::<MiniAppsAction>() {
                if host.is_open() {
                    host.action(cx, action);
                } else {
                    dock.open(cx, action);
                }
            }
            if let Some(action) = action.downcast_ref::<ArticleAction>() {
                if host.is_open() {host.article_action(cx, action);} else {dock.article_action(cx, action);}
            }
            if matches!(action.downcast_ref(), Some(MiniAppsSideAction::PopOut)) {
                if let Some(workspace) = dock.take_workspace(cx) {host.adopt_workspace(cx, workspace);}
            }
            if let Some(MiniAppsWindowAction::DockBack(id)) = action.downcast_ref() {
                if let Some(workspace) = host.take_for_dock(cx, *id) {dock.restore_workspace(cx, workspace);}
            }
        }
        self.prepare_dock(cx);
        self.ui.redraw(cx);
    }
}
impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        #[cfg(feature = "onboarding-test-support")]
        if std::env::var("HAGENCY_DOCK_OFFLINE_ACCOUNT").as_deref() == Ok("1") {
            rinx::article_app::review::install_offline_account().unwrap();
        }
        makepad_widgets::theme_mod(vm); theme::init_standalone(vm);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm); desktop_style::apply_widgets(vm);
        #[cfg(feature = "onboarding-test-support")]
        if let Ok(scale) = std::env::var("HAGENCY_OWNER_REVIEW_HOST_SCALE").unwrap_or_default().parse::<f64>() {
            let scale = scale.clamp(0.75, 3.0);
            script_eval!(vm, { mod.theme.octo_typography_scale = #(scale) });
        }
        #[cfg(feature = "onboarding-test-support")]
        if let Ok(scale) = std::env::var("HAGENCY_OWNER_REVIEW_SCALE").unwrap_or_default().parse::<f64>() {
            vm.cx_mut().global::<owner_dashboard::theme::DashboardTextScale>().0 = scale;
        }
        rinx::app::register_widgets(vm);
        let _ = vm.cx_mut().global::<rinx::home::rooms_list::RoomsListRef>();
        self::script_mod(vm)
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if matches!(event, Event::Startup) {
            rinx::hagency::owner_dashboard::register_host_accessibility(cx);
            #[cfg(feature = "onboarding-test-support")]
            if let Ok(size) = std::env::var("HAGENCY_DOCK_REVIEW_SIZE") { self.resize_fixture(cx, &size); }
            #[cfg(feature = "onboarding-test-support")]
            if std::env::var("HAGENCY_PERMISSION_REVIEW").as_deref() == Ok("1") {
                self.prepare_dock(cx);
                self.ui.mini_apps_side_region(cx, ids!(dock)).open(cx, &MiniAppsAction::OpenAgent);
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Ok(state) = std::env::var("HAGENCY_HOSTING_REVIEW") { self.hosting_fixture(cx, &state); }
        }
        if owner_dashboard::keyboard::embedded(&self.ui, cx, event) { return; }
        if let Event::Custom(command) = event {
            #[cfg(all(feature = "onboarding-test-support", target_os = "macos"))]
            if command == "dock:inspect-native" {
                let main = self.ui.window(cx, ids!(main_window)).window_id();
                let floating = self.ui.mini_apps_window_host(cx, ids!(popped)).window_id();
                native_webview_probe::inspect(cx, [main, floating].into_iter().flatten().collect(),
                    rinx::app_data_dir().join("dock-native.json"));
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(url) = command.strip_prefix("dock:review-web:") {
                let url = url::Url::parse(url).unwrap();
                assert_eq!(url.host_str(), Some("127.0.0.1"));
                let host = self.ui.mini_apps_window_host(cx, ids!(popped));
                let action = WebBrowserAction::Open(url);
                if host.is_open() {host.web_action(cx, &action);}
                else {self.ui.mini_apps_side_region(cx, ids!(dock)).web_action(cx, &action);}
            }
            #[cfg(feature = "onboarding-test-support")]
            if command == "dock:inspect" {
                let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
                let host = self.ui.mini_apps_window_host(cx, ids!(popped));
                let floating = host.workspace(cx);
                let mut snapshot = |region: &rinx::miniapps::side::MiniAppsSideRegionRef| {
                    let tabs: Vec<_> = region.tabs().into_iter().map(|id| serde_json::json!({
                        "id": format!("{id:?}"), "surface": format!("{:?}", region.surface(cx, id).widget_uid()),
                        "title": region.surface(cx, id).title(cx),
                    })).collect();
                    serde_json::json!({"tabs":tabs,"selected":region.selected().map(|id|format!("{id:?}")),
                        "reader":region.reader(cx).session(), "browser":format!("{:?}",region.reader(cx).browser_id())})
                };
                let docked = snapshot(&dock);
                let floating = snapshot(&floating);
                let icon = self.ui.widget(cx, ids!(agent_button.icon));
                let label = self.ui.label(cx, ids!(agent_button.label));
                let palette = cx.with_vm(|vm| {
                    let mut value = icon.current_to_value(vm);
                    for key in [id!(draw_icon), id!(color)] {
                        value = value.as_object().map(|object| vm.bx.heap.value(object, key.into(), NoTrap)).unwrap_or(ScriptValue::NIL);
                    }
                    let widgets = vm.module(id!(widgets));
                    let colors: Vec<_> = [id!(RBX_NAV_BG), id!(RBX_NAV_FG), id!(RINX_INK)].into_iter().map(|key| {
                        let value = vm.bx.heap.value(widgets, key.into(), NoTrap);
                        rinx::theme::argb(Vec4f::script_from_value(vm, value))
                    }).collect();
                    serde_json::json!({"icon":rinx::theme::argb(Vec4f::script_from_value(vm,value)),
                        "label":label.borrow().map(|label|rinx::theme::argb(label.draw_text.color)),
                        "nav_bg":colors[0],"nav_fg":colors[1],"page_ink":colors[2]})
                });
                let data = serde_json::json!({"docked":docked,"floating":floating,"agent_palette":palette,
                    "window":host.window_id().map(|id|id.id()),"open":host.is_open()});
                std::fs::write(rinx::app_data_dir().join("dock-inspection.json"), data.to_string()).unwrap();
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(size) = command.strip_prefix("dock:window-size:") { self.resize_fixture(cx, size); }
            #[cfg(feature = "onboarding-test-support")]
            if command == "dock:owner-dump" {
                let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
                let view = dock.selected().and_then(|id| dock.surface(cx,id).owner_fixture_view(cx));
                log!("od-embedded-view {}", serde_json::json!({"view":view}));
            }
            #[cfg(feature = "onboarding-test-support")]
            if command == "dock:owner-ax" {
                log!("od-ax {}", owner_dashboard::accessibility::diagnostics(cx));
            }
            #[cfg(all(feature = "owner-accessibility", feature = "onboarding-test-support", target_os = "macos"))]
            if let Some(arg) = command.strip_prefix("dock:owner-ax-native:") {
                log!("od-ax-native {}", owner_dashboard::accessibility::native_fixture_probe(cx, arg));
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(zoom) = command.strip_prefix("dock:owner-zoom:").and_then(|value| value.parse::<f64>().ok()) {
                if let Some(window) = self.ui.window(cx, ids!(main_window)).window_id() {
                    let dpi = cx.windows[window].native_dpi_factor() * zoom.clamp(0.75, 2.0);
                    cx.set_window_dpi_override(window, Some(dpi));
                    self.ui.redraw(cx);
                }
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(owner) = command.strip_prefix("dock:owner-scope:") {
                let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
                if let Some(id) = dock.selected() { dock.surface(cx, id).owner_fixture_scope(cx, (!owner.is_empty()).then_some(owner)); }
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(scenario) = command.strip_prefix("dock:owner:") {
                self.owner_fixture(cx, scenario);
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(state) = command.strip_prefix("dock:hosting:") {
                self.hosting_fixture(cx, state);
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(step) = command.strip_prefix("dock:owner-refresh:").and_then(|n| n.parse::<u64>().ok()) {
                let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
                if let Some(id) = dock.selected() {
                    dock.surface(cx, id).update_owner_fixture(cx, owner_dashboard::fixtures::refresh_step(step));
                }
            }
            #[cfg(feature = "onboarding-test-support")]
            if let Some(locale) = command.strip_prefix("dock:owner-locale:") {
                let dock = self.ui.mini_apps_side_region(cx, ids!(dock));
                if let Some(id) = dock.selected() { dock.surface(cx, id).owner_fixture_locale(cx, owner_dashboard::Locale::from_tag(locale)); }
            }
            for (name, appearance) in [("dock:dark", Appearance::Dark), ("dock:light", Appearance::Light)] {
                if command == name {theme::select(cx, Selection {appearance, accent: Accent::Teal}).unwrap();}
            }
        }
        self.match_event(cx, event);
        // Production HomeScreen prepares the region on each frame, including restyles.
        self.prepare_dock(cx);
        self.ui.handle_event(cx, event, &mut Scope::empty());
        owner_dashboard::accessibility::after_event(&self.ui, cx, event);
    }
}
