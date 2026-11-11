//! Offline real HomeScreen drag/collapse fixture. Never signs in.
pub use makepad_widgets;
use makepad_widgets::*;
use rinx::app::AppState;
use rinx::miniapps::side::MiniAppsSideRegionWidgetRefExt;

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
            window.title: "Hagency · Rooms collapse review"
            body +: {home := HomeScreen {}}
        }}
    }
}
#[derive(Script, ScriptHook)]
struct App {
    #[live]
    ui: WidgetRef,
    #[rust] state: AppState,
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
            if command=="rooms:open-apps" {
                self.ui.mini_apps_side_region(cx, ids!(mini_apps_side)).open(cx, &rinx::miniapps::MiniAppsAction::Open);
            }
            if let Some(value) = command.strip_prefix("rooms:open-web:") {
                let url: url::Url = value.parse().expect("fixture URL");
                assert_eq!(url.host_str(), Some("127.0.0.1"));
                self.ui.mini_apps_side_region(cx, ids!(mini_apps_side))
                    .web_action(cx, &rinx::shared::web_browser::WebBrowserAction::Open(url));
            }
            if let Some(value) = command.strip_prefix("rooms:down-without-move:") {
                let (x, y) = value.split_once(':').expect("fixture coordinates");
                let window_id = self.ui.window(cx, ids!(main_window)).window_id().unwrap();
                cx.fingers.first_mouse_button = Some((MouseButton::PRIMARY, window_id));
                let event = Event::MouseDown(MouseDownEvent {
                    abs: dvec2(x.parse().unwrap(), y.parse().unwrap()),
                    button: MouseButton::PRIMARY,
                    window_id,
                    modifiers: KeyModifiers::default(),
                    handled: std::cell::Cell::new(Area::Empty),
                    time: Cx::monotonic_now(),
                });
                self.ui.handle_event(cx, &event, &mut Scope::with_data(&mut self.state));
            }
            if command=="rooms:reload" {
                assert_eq!(self.state.saved_dock_state_home.selected_room.as_ref().map(|room|room.room_id().as_str()),Some("!review:example.invalid"));
                cx.action(rinx::home::main_desktop_ui::MainDesktopUiAction::LoadDockFromAppState);
            }
            if command=="rooms:open-chat" {
                let room=rinx::app::SelectedRoom::JoinedRoom {room_name_id:rinx::utils::RoomNameId::new(
                    matrix_sdk::RoomDisplayName::Named("Test chat".into()),"!review:example.invalid".try_into().unwrap())};
                let uid=self.ui.widget(cx,ids!(rooms_list)).widget_uid();
                let actions=cx.capture_actions(|cx|cx.widget_action(uid,rinx::home::rooms_list::RoomsListAction::Selected(room)));
                self.ui.widget(cx,ids!(home)).handle_event(cx,&Event::Actions(actions),&mut Scope::with_data(&mut self.state));
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::with_data(&mut self.state));
    }
}
