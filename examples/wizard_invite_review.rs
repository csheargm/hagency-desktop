//! Offline review of invitations over the actual production overlay hierarchy.
//! The real App routes every event. Only creation results are seeded; no login,
//! room creation, invitation or model request can run in this fixture.
pub use makepad_widgets;
use makepad_widgets::*;
use rinx::app::App as RinxApp;
use rinx::home::invite_modal::{InviteModalAction, InviteModalWidgetRefExt};
use rinx::home::space_management::SpaceManagementPanelWidgetRefExt;
use rinx::shared::web_browser::{WebBrowserAction, WebBrowserWidgetRefExt};
#[cfg(target_os = "macos")]
#[path = "support/native_webview_probe.rs"]
mod native_webview_probe;

app_main!(App);

fn review_size() -> Vec2d {
    Vec2d {
        x: std::env::var("WIZARD_REVIEW_WIDTH").ok().and_then(|s| s.parse().ok()).unwrap_or(720.0),
        y: std::env::var("WIZARD_REVIEW_HEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(780.0),
    }
}

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        app: #(RinxApp::script_component(vm)) {
            ui: Root {main_window := Window {
                window.inner_size: #(review_size())
                window.title: "Hagency · Wizard invitation review"
                body +: {content := RinxContent {}}
            }}
        }
    }
}

#[derive(Script, ScriptHook)]
struct App {
    #[live] app: RinxApp,
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::theme_mod(vm);
        rinx::theme::init_standalone(vm);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm);
        desktop_style::apply_widgets(vm);
        rinx::app::register_widgets(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if matches!(event, Event::Startup) {
            self.app.prepare_offline_review();
        }
        let ui = self.app.content();
        if let Event::Custom(command) = event {
            if command == "webview:open" {
                let url = std::env::var("HAGENCY_DOCK_URL").unwrap().parse::<url::Url>().unwrap();
                assert_eq!(url.host_str(), Some("127.0.0.1"));
                cx.action(WebBrowserAction::Open(url));
            }
            if command == "webview:close" { cx.action(WebBrowserAction::Close); }
            if command == "miniapp:open" { cx.action(rinx::miniapps::MiniAppsAction::Open); }
            if command == "management:open" { cx.action(rinx::hagency::ui::HagencyAction::OpenAgents); }
            if command == "miniapp:back" {
                self.app.handle_event(cx, &Event::BackPressed {handled: std::cell::Cell::new(false)});
                return;
            }
            #[cfg(target_os = "macos")]
            if command == "webview:inspect-native" {
                let windows = ui.window(cx, ids!(main_window)).window_id().into_iter().collect();
                native_webview_probe::inspect(cx, windows,
                    rinx::app_data_dir().join("overlay-native.json"));
            }
            if command == "webview:inspect" {
                let browser = ui.web_browser(cx, ids!(web_browser_modal.content));
                let data = serde_json::json!({"reader":browser.session(),"browser":format!("{:?}",browser.browser_id()),
                    "reader_open":ui.modal(cx, ids!(web_browser_modal)).is_open(),
                    "miniapp_open":ui.modal(cx, ids!(octoscript_apps_modal)).is_open(),
                    "management_open":ui.modal(cx, ids!(hagency_management_modal)).is_open()});
                std::fs::write(rinx::app_data_dir().join("overlay-inspection.json"), data.to_string()).unwrap();
            }
            if matches!(command.as_str(), "theme:dark" | "theme:light") {
                rinx::theme::select(cx, rinx::theme::Selection {
                    appearance: if command == "theme:dark" {rinx::theme::Appearance::Dark} else {rinx::theme::Appearance::Light},
                    accent: rinx::theme::Accent::Teal,
                }).unwrap();
            }
            if command == "theme:large-text" {
                let mut package = rinx::theme::packages::export_current(cx).unwrap();
                for dark in [false, true] {
                    package.set(Some(dark), "typography.scale", octosense_theme_contract::number(1.3));
                }
                let mut preferences = rinx::theme::packages::current(cx).unwrap();
                preferences.package = Some(package);
                rinx::theme::packages::apply(cx, preferences).unwrap();
            }
            if let Some(kind) = command.strip_prefix("completed:") {
                ui.space_management_panel(cx, ids!(space_management_modal.content))
                    .review(cx, kind.parse().unwrap(), 2, false);
                ui.modal(cx, ids!(space_management_modal)).open(cx);
            }
        }
        self.app.handle_event(cx, event);
        if let Event::Actions(actions) = event {
            if actions.iter().any(|a| matches!(a.downcast_ref(), Some(InviteModalAction::Open(_)))) {
                ui.invite_modal(cx, ids!(invite_modal.content)).review_result(cx, "ready");
            }
        }
    }
}
