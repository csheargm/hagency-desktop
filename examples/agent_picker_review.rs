//! Isolated real Makepad wizard rendering. This process never signs in.
pub use makepad_widgets;
use makepad_widgets::*;
use rinx::home::mobile_chat_info::MobileChatInfoWidgetRefExt;
app_main!(App);
fn review_size() -> Vec2d {
    Vec2d {
        x: std::env::var("WIZARD_REVIEW_WIDTH").ok().and_then(|s|s.parse().ok()).unwrap_or(720.0),
        y: std::env::var("WIZARD_REVIEW_HEIGHT").ok().and_then(|s|s.parse().ok()).unwrap_or(780.0),
    }
}
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {main_window := Window {
            window.inner_size: #(review_size())
            window.title: "Hagency · Agent picker review"
            body +: {picker := MobileChatInfo {}}
        }}
    }
}
#[derive(Script, ScriptHook)]
struct App {
    #[live]
    ui: WidgetRef,
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
        if matches!(event, Event::Startup)
            && let Ok(scenario) = std::env::var("HAGENCY_PICKER_REVIEW") {
            self.ui.mobile_chat_info(cx, ids!(picker)).review_agent_picker(cx, &scenario);
            self.ui.drop_down(cx, ids!(picker.my_agents)).set_selected_item(cx, 0);
        }
        if let Event::Custom(command) = event {
            if matches!(command.as_str(), "theme:dark" | "theme:light") {
                rinx::theme::select(cx, rinx::theme::Selection {
                    appearance: if command == "theme:dark" {rinx::theme::Appearance::Dark} else {rinx::theme::Appearance::Light},
                    accent: rinx::theme::Accent::Teal,
                }).unwrap();
            }
            if command == "theme:large-text" {
                let mut package = rinx::theme::packages::export_current(cx).unwrap();
                for dark in [false,true] {package.set(Some(dark), "typography.scale", octosense_theme_contract::number(1.3));}
                let mut preferences = rinx::theme::packages::current(cx).unwrap(); preferences.package = Some(package);
                rinx::theme::packages::apply(cx, preferences).unwrap();
            }
            if let Some(scenario) = command.strip_prefix("picker:") {
                self.ui.mobile_chat_info(cx, ids!(picker)).review_agent_picker(cx, scenario);
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
