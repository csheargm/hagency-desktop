//! Offline review of the real message templates. No Matrix client is started.
pub use makepad_widgets;
use makepad_widgets::*;
use rinx::shared::html_or_plaintext::HtmlOrPlaintextWidgetRefExt;
app_main!(App);
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root { main_window := Window {
            window.title: "Hagency · Message metadata review"
            window.inner_size: vec2(430, 800)
            body +: {flow: Down padding: 16 spacing: 12
                Label {text: "Desktop message · edited metadata"}
                normal := mod.widgets.Message {
                    body +: {content +: {
                        username_view +: {username +: {text: "周予 · 内容协作者"}}
                    }}
                }
                Label {text: "Consecutive message"}
                condensed := mod.widgets.CondensedMessage {}
                Label {text: "Mobile message"}
                mobile := mod.widgets.MobileMessage {
                    body +: {content +: {
                        username_view +: {username +: {text: "周予 · 内容协作者"}}
                    }}
                }
            }
        }}
    }
}
#[derive(Script, ScriptHook)]
struct App { #[live] ui: WidgetRef }
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
            let items: [(&[LiveId], &[LiveId]); 3] = [(ids!(normal), ids!(body.content.message)), (ids!(condensed), ids!(body.content.message)), (ids!(mobile), ids!(body.content.bubble.message))];
            for (id, body) in items {
                let item = self.ui.widget(cx, id);
                item.html_or_plaintext(cx, body).show_plaintext(cx, "周末共读活动安排已更新，雨天转至室内。欢迎参与！");
                item.widget(cx, ids!(edited_indicator)).set_visible(cx, true);
            }
        }
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
