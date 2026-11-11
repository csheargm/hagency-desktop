//! Offline selection fixture using the production chat timeline and message bodies.
use makepad_widgets::*;
use rinx::shared::html_or_plaintext::HtmlOrPlaintextWidgetRefExt;
#[cfg(feature = "palpo-instrument")]
use rinx::room::typing_notice::TypingNoticeWidgetRefExt;

app_main!(App);

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {
            main_window := Window {
                window.inner_size: vec2(720, 640)
                window.title: "Rinx · timeline selection test"
                body +: {
                    flow: Down
                    controls := View {
                        width: Fill height: Fit flow: Right
                        inspect := Button {text: "Inspect" grab_key_focus: false}
                        reset := Button {text: "Reset position" grab_key_focus: false}
                        spinner_container := View {
                            width: 24 height: 24 flow: Right align: Align{x: 0.5 y: 0.5}
                            activity_spinner := PacedLoadingSpinner {
                                width: 20 height: 20 visible: false
                                draw_bg +: {color: mod.widgets.RINX_ACCENT stroke_width: 2.0}
                            }
                        }
                    }
                    result := Label {width: Fill height: 65 text: "Ready"}
                    timeline := Timeline {
                        list +: {
                            Fixture := View {
                                width: Fill height: 160 flow: Down
                                padding: Inset{left: 70 right: 70 top: 12 bottom: 12}
                                row_id := Label {text: "Row"}
                                body := HtmlOrPlaintext {selectable: true}
                            }
                        }
                    }
                    typing_container := View {
                        width: Fill height: Fit flow: Down
                        typing_notice := TypingNotice {}
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook)]
struct App {
    #[live] ui: WidgetRef,
    #[rust] link_clicks: usize,
    #[cfg(feature = "palpo-instrument")]
    #[rust] measured_draws: u64,
    #[cfg(feature = "palpo-instrument")]
    #[rust] measured_timers: u64,
    #[cfg(feature = "palpo-instrument")]
    #[rust] block_timers: bool,
}

impl MatchEvent for App {
    fn handle_startup(&mut self, cx: &mut Cx) {
        let list = self.ui.portal_list(cx, ids!(timeline.list));
        list.set_tail_range(false);
        list.set_first_id_and_scroll(5, 0.0);
    }

    fn handle_draw(&mut self, cx: &mut Cx, event: &DrawEvent) {
        #[cfg(feature = "palpo-instrument")]
        { self.measured_draws += 1; }
        let mut draw = CxDraw::new(cx, event);
        let mut cx = Cx2d::new(&mut draw);
        while let Some(widget) = self.ui.draw(&mut cx, &mut Scope::empty()).step() {
            let mut list = widget.borrow_mut::<PortalList>().unwrap();
            list.set_item_range(&mut cx, 0, 30);
            while let Some(index) = list.next_visible_item(&mut cx) {
                let row = list.item(&mut cx, index, id!(Fixture));
                row.label(&mut cx, ids!(row_id)).set_text(&mut cx, &format!("Row {index}"));
                let body = row.html_or_plaintext(&mut cx, ids!(body));
                if index % 2 == 0 {
                    body.show_plaintext(&mut cx, "Alpha 中文 👩‍💻 selectable text.\nSecond line continues selection.\nThird line ends here.");
                } else {
                    body.show_html(&mut cx, "<p><a href='https://example.org/'>Selectable link text</a> and <b>bold 中文</b>.</p><p>Second line continues selection.</p><p>Third line ends here.</p>");
                }
                row.draw_all(&mut cx, &mut Scope::empty());
            }
        }
    }

    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        for action in actions {
            if matches!(action.as_widget_action().cast::<HtmlLinkAction>(), HtmlLinkAction::Clicked {..}) {
                self.link_clicks += 1;
            }
        }
        let list = self.ui.portal_list(cx, ids!(timeline.list));
        if self.ui.button(cx, ids!(reset)).clicked(actions) {
            list.set_tail_range(false);
            list.set_first_id_and_scroll(5, 0.0);
            list.redraw(cx);
        }
        if self.ui.button(cx, ids!(inspect)).clicked(actions) {
            let mut selections = Vec::new();
            for index in 4..10 {
                if let Some((_, row)) = list.get_item(index) {
                    let body = row.html_or_plaintext(cx, ids!(body));
                    selections.push(serde_json::json!({"index": index, "text": body.selected_text(cx)}));
                }
            }
            let state = serde_json::json!({
                "first_id": list.first_id(), "scroll": list.scroll_position(),
                "selections": selections, "link_clicks": self.link_clicks,
            });
            self.ui.label(cx, ids!(result)).set_text(cx, &state.to_string());
        }
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
        #[cfg(feature = "palpo-instrument")]
        if matches!(event, Event::Timer(_)) { self.measured_timers += 1; }
        #[cfg(feature = "palpo-instrument")]
        if let Event::Custom(command) = event {
            match command.as_str() {
                "perf:typing-show" => self.ui.typing_notice(cx, ids!(typing_notice))
                    .show_or_hide(cx, &["Alice".to_owned()]),
                "perf:typing-hide" => self.ui.typing_notice(cx, ids!(typing_notice))
                    .show_or_hide(cx, &[]),
                "perf:typing-switch" => self.ui.typing_notice(cx, ids!(typing_notice)).reset(cx),
                "perf:typing-reapply" => cx.request_style_reload(),
                "perf:typing-obscure" | "perf:typing-obscure-blocked" => {
                    self.ui.view(cx, ids!(typing_container)).set_visible(cx, false);
                    self.block_timers = command == "perf:typing-obscure-blocked";
                }
                "perf:typing-reveal" => {
                    self.block_timers = false;
                    self.ui.view(cx, ids!(typing_container)).set_visible(cx, true);
                }
                "perf:spinner-show" => self.ui.widget(cx, ids!(activity_spinner)).set_visible(cx, true),
                "perf:spinner-hide" => self.ui.widget(cx, ids!(activity_spinner)).set_visible(cx, false),
                "perf:spinner-obscure" | "perf:spinner-obscure-blocked" => {
                    self.ui.view(cx, ids!(spinner_container)).set_visible(cx, false);
                    self.block_timers = command == "perf:spinner-obscure-blocked";
                }
                "perf:spinner-reveal" => {
                    self.block_timers = false;
                    self.ui.view(cx, ids!(spinner_container)).set_visible(cx, true);
                }
                "perf:spinner-reapply" => cx.request_style_reload(),
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
        self.match_event(cx, event);
        if !matches!(event, Event::Draw(_)) {
            #[cfg(feature = "palpo-instrument")]
            if self.block_timers && matches!(event, Event::Timer(_)) { return; }
            self.ui.handle_event(cx, event, &mut Scope::empty());
        }
    }
}
