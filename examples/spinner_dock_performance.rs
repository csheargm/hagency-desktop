//! Offline native fixture for retained spinner activity across Dock tab switches.
use makepad_widgets::*;
use rinx::shared::bouncing_dots::BouncingDotsWidgetRefExt;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(720, 480)
                body +: {
                    dock := RobrixDock {
                        width: Fill height: Fill
                        tab_bar +: {PermanentTab := RobrixTab {closeable: false}}
                        root := DockTabs {tabs: [@activity_tab @other_tab] selected: 0}
                        activity_tab := DockTab {name: "Activity" kind: @activity template: @PermanentTab}
                        other_tab := DockTab {name: "Other" kind: @other template: @PermanentTab}
                        activity := View {
                            width: Fill height: Fill flow: Down new_batch: true
                            Label {text: "Activity tab"}
                            activity_spinner := PacedLoadingSpinner {
                                width: 20 height: 20
                                draw_bg +: {color: mod.widgets.RINX_ACCENT stroke_width: 2.0}
                            }
                            bouncing_dots := BouncingDots {}
                        }
                        other := View {width: Fill height: Fill Label {text: "Other tab"}}
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
struct App {
    #[live] ui: WidgetRef,
    #[rust] measured_draws: u64,
    #[rust] measured_timers: u64,
    #[rust] block_timers: bool,
}

impl MatchEvent for App {
    fn handle_draw(&mut self, cx: &mut Cx, event: &DrawEvent) {
        self.measured_draws += 1;
        let mut draw = CxDraw::new(cx, event);
        let mut cx = Cx2d::new(&mut draw);
        self.ui.draw_all(&mut cx, &mut Scope::empty());
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::theme_mod(vm);
        script_eval!(vm, {mod.theme = mod.themes.light});
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm);
        rinx::app::register_widgets(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if matches!(event, Event::Timer(_)) { self.measured_timers += 1; }
        if let Event::Custom(command) = event {
            match command.as_str() {
                "dock:other" => self.ui.dock(cx, ids!(dock)).select_tab(cx, id!(other_tab)),
                "dock:activity" => self.ui.dock(cx, ids!(dock)).select_tab(cx, id!(activity_tab)),
                "dock:block-timers" => self.block_timers = true,
                "dock:unblock-timers" => self.block_timers = false,
                "dock:use-dots" => {
                    let item = self.ui.dock(cx, ids!(dock)).item(id!(activity_tab));
                    item.widget(cx, ids!(activity_spinner)).set_visible(cx, false);
                    item.bouncing_dots(cx, ids!(bouncing_dots)).start_animation(cx);
                }
                "perf:start" => {
                    self.measured_draws = 0;
                    self.measured_timers = 0;
                    cx.perf_monitor = Default::default();
                    cx.perf_monitor.set_enabled(true);
                }
                "perf:stop" => {
                    let frames = cx.perf_monitor.frames_painted();
                    cx.perf_monitor.set_enabled(false);
                    if let Some(path) = std::env::var_os("HAGENCY_PERF_OUTPUT") {
                        std::fs::write(path, serde_json::json!({
                            "frames_painted": frames, "draw_events": self.measured_draws,
                            "timer_events": self.measured_timers,
                        }).to_string()).unwrap();
                    }
                }
                _ => {}
            }
        }
        if self.block_timers && matches!(event, Event::Timer(_)) { return; }
        self.match_event(cx, event);
        if !matches!(event, Event::Draw(_)) {
            self.ui.handle_event(cx, event, &mut Scope::empty());
        }
    }
}
