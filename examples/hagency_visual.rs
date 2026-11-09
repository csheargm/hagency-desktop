//! Offline native visual review of the shipped Hagency Splash bundle.
//! Uses an isolated profile and explicit JSON read fixtures; never starts the
//! Matrix client, local executor or a management backend. Mutations fail closed.
pub use makepad_widgets;
use makepad_widgets::*;
use makepad_widgets::splash_host::{splash_host_respond, take_splash_host_requests_for};
use rinx::theme::{self, Accent, Appearance, Selection};
use serde_json::Value;

app_main!(App);
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.title: "Hagency · Visual review · Offline fixtures"
                window.inner_size: #(if std::env::args().any(|a| a == "--narrow") {dvec2(430., 850.)} else {dvec2(1000., 900.)})
                body +: {
                    flow: Down padding: 20 spacing: 12 show_bg: true draw_bg.color: RINX_PAGE
                    bundle := View {width: Fill height: Fill
                        app := Splash {width: Fill height: Fill}
                    }
                    // The shipped native confirmation panel, shown only for
                    // HAGENCY_NATIVE_PANEL previews (ux:native-panel).
                    panel := MiniAppsPanel {visible: false}
                }
            }
        }
    }
}

#[derive(Script)]
struct App {
    #[live] ui: WidgetRef,
    #[rust] fixtures: Value,
    #[rust] retained_script_layout: Option<Layout>,
    #[rust] reply_redraw: NextFrame,
    #[rust] source: String,
    #[rust] theme_revision: u64,
    #[rust] restyle_card: bool,
}
// Match the production MiniAppsPanel's ownership of Splash's root layout.
impl ScriptHook for App {
    fn on_before_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _: &mut Scope, _: ScriptValue) {
        if matches!(apply, Apply::Rebake | Apply::ScriptReapply) {
            self.retained_script_layout = self.ui.splash(vm.cx_mut(), ids!(app)).borrow().map(|card| card.view.layout);
        }
    }
    fn on_after_apply(&mut self, vm: &mut ScriptVm, _: &Apply, _: &mut Scope, _: ScriptValue) {
        if let Some(layout) = self.retained_script_layout.take() {
            if let Some(mut card) = self.ui.splash(vm.cx_mut(), ids!(app)).borrow_mut() {card.view.layout = layout;}
        }
        let revision = theme::snapshot_for_vm(vm).revision;
        self.restyle_card |= self.theme_revision != revision && !self.source.is_empty();
        self.theme_revision = revision;
    }
}
impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let profile = std::path::PathBuf::from(std::env::var_os("HAGENCY_DESKTOP_DATA_DIR").expect("isolated profile required"));
        let fixtures = std::env::var_os("HAGENCY_UX_FIXTURES").expect("explicit offline fixtures required");
        self.fixtures = serde_json::from_slice(&std::fs::read(fixtures).unwrap()).unwrap();
        let manifest = octosense_app_contract::parse(include_str!("../apps/hagency/bundle/manifest.json")).unwrap();
        let splash = self.ui.splash(cx, ids!(app));
        splash.set_sandbox_dir(cx, Some(profile.join("sandbox")));
        splash.set_storage_quota(cx, Some(1024 * 1024));
        splash.set_host_caps(cx, manifest.capabilities);
        splash.set_policy(cx, Some(vec![]), Some(50_000_000));
        let source = std::env::var_os("HAGENCY_UX_SOURCE").map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("apps/hagency/bundle/main.splash"));
        self.source = std::fs::read_to_string(source).unwrap();
        // As MiniAppsPanel mounts a system app. The time is printed so a
        // bundle creeping toward the budget shows up in every probe run.
        splash.set_time_budget(cx, Some(rinx::miniapps::ui::SYSTEM_APP_TIME_BUDGET));
        let started = std::time::Instant::now();
        splash.set_text(cx, &self.source);
        eprintln!("hagency bundle evaluated in {} ms", started.elapsed().as_millis());
        // MiniAppsPanel refuses a source whose root sections did not install.
        // Refuse it here too rather than review screens the app never shows.
        assert!(rinx::miniapps::ui::hagency_root_installed(&splash), "required root was not installed");
    }
}
impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::theme_mod(vm);
        theme::init_standalone(vm);
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm);
        desktop_style::apply_widgets(vm);
        rinx::app::register_widgets(vm);
        self::script_mod(vm)
    }
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        // Match MiniAppsPanel's source reapplication after host style reload.
        // Reapplying only the shell would leave cached Splash colors stale.
        if std::mem::take(&mut self.restyle_card) {
            self.ui.splash(cx, ids!(app)).set_text(cx, &self.source);
        }
        if self.reply_redraw.is_event(event).is_some() {cx.redraw_all();}
        if let Event::Custom(command) = event {
            if command == "ux:large-text" {
                let mut package = theme::packages::export_current(cx).unwrap();
                for dark in [false, true] {
                    package.set(Some(dark), "typography.scale", octosense_theme_contract::number(1.5));
                }
                let mut preferences = theme::packages::current(cx).unwrap();
                preferences.package = Some(package);
                theme::packages::apply(cx, preferences).unwrap();
            }
            // Show one native confirmation (grant panel or monthly maximum)
            // from the JSON file named by HAGENCY_NATIVE_PANEL, through the
            // production layout and text. It reads no fixtures and replies to nothing.
            #[cfg(feature = "onboarding-test-support")]
            if command == "ux:native-panel" {
                let path = std::env::var_os("HAGENCY_NATIVE_PANEL").expect("HAGENCY_NATIVE_PANEL names the preview JSON");
                let preview: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                self.ui.widget(cx, ids!(bundle)).set_visible(cx, false);
                self.ui.widget(cx, ids!(panel)).set_visible(cx, true);
                if let Some(mut panel) = self.ui.widget(cx, ids!(panel)).borrow_mut::<rinx::miniapps::ui::MiniAppsPanel>() {
                    if let Err(error) = panel.preview_confirmation(cx, &preview) {
                        eprintln!("native panel preview failed: {error}");
                    }
                }
                cx.redraw_all();
            }
            if command == "ux:reload-fixtures" {
                let path = std::env::var_os("HAGENCY_UX_FIXTURES").unwrap();
                self.fixtures = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
            }
            for (name, appearance, accent) in [
                ("ux:light", Appearance::Light, Accent::Teal),
                ("ux:dark", Appearance::Dark, Accent::Teal),
                ("ux:violet", Appearance::Light, Accent::Violet),
            ] {
                if command == name {theme::select(cx, Selection {appearance, accent}).unwrap();}
            }
        }
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
        let owned: Vec<_> = self.ui.splash(cx, ids!(app)).isolate_heap_key(cx).into_iter().collect();
        for request in take_splash_host_requests_for(&owned) {
            // Only predeclared read responses are available. This host has no backend.
            if let Some(value) = self.fixtures.get(&request.service) {
                // A held request never answers, so its pending state can be captured.
                if value["_hold"] == true {continue;}
                let mut value = value.clone();
                let args: Value = serde_json::from_str(&request.args_json).unwrap();
                if let Some(cases) = value.get("_cases").and_then(Value::as_array) {
                    let matched = cases.iter().find(|case| case["when"].as_object().is_some_and(|when|
                        when.iter().all(|(key, expected)| args.get(key).unwrap_or(&Value::Null) == expected)));
                    if let Some(case) = matched {
                        if let Some(error) = case.get("error").and_then(Value::as_str) {
                            splash_host_respond(cx, request.heap_key, request.req_id, Err(error));
                            self.reply_redraw = cx.new_next_frame();
                            continue;
                        }
                        value = case["response"].clone();
                    } else {
                        splash_host_respond(cx, request.heap_key, request.req_id, Err("offline_visual_fixture_case_missing"));
                        self.reply_redraw = cx.new_next_frame();
                        continue;
                    }
                }
                if request.service == "hagency.resources.list" {
                    if let Some(capability) = args["capability"].as_str() {
                        value["resources"].as_array_mut().unwrap().retain(|r| r["capabilities"].as_array().unwrap().iter().any(|v| v == capability));
                    }
                }
                splash_host_respond(cx, request.heap_key, request.req_id, Ok(&value.to_string()));
            } else {
                splash_host_respond(cx, request.heap_key, request.req_id, Err("offline_visual_fixture_unavailable"));
            }
            // Host replies enqueue script work; draw after the task pump, as
            // MiniAppsPanel does, rather than capturing its loading frame.
            self.reply_redraw = cx.new_next_frame();
        }
    }
}
