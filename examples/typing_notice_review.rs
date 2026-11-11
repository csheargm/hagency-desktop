//! Offline exercise of the production typing timer. No Matrix session is started.
pub use makepad_widgets;
use makepad_widgets::*;
use rinx::room::typing_notice::TypingNoticeWidgetRefExt;
app_main!(App);
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {main_window := Window {
            window.inner_size: vec2(600,400)
            window.title: "Hagency · Typing expiry review"
            body +: {flow: Down padding: 20 spacing: 16
                RinxPageTitle {text: "共读活动 · 筹备讨论"}
                RinxLabel {width: Fill flow: Flow.Right{wrap: true} text: "A fresh typing update renews the 30-second display lease. With no more updates, the notice clears automatically."}
                View {width: Fill height: Fit flow: Right spacing: 12
                    start := RinxButton {text: "Typing update"}
                    stop := RinxButton {text: "Stop typing"}
                    reset := RinxButton {text: "Switch room"}
                }
                View {width: Fill height: Fill}
                notice := TypingNotice {}
                RinxInput {width: Fill text: "输入消息"}
            }
        }}
    }
}
#[derive(Script,ScriptHook)]
struct App {#[live] ui: WidgetRef}
impl AppMain for App {
    fn script_mod(vm:&mut ScriptVm)->ScriptValue {
        makepad_widgets::theme_mod(vm);rinx::theme::init_standalone(vm);
        #[cfg(any(target_os="macos",target_os="ios"))] article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm);desktop_style::apply_widgets(vm);rinx::app::register_widgets(vm);self::script_mod(vm)
    }
    fn handle_event(&mut self,cx:&mut Cx,event:&Event) {
        self.ui.handle_event(cx,event,&mut Scope::empty());
        if let Event::Actions(actions)=event {
            let notice=self.ui.typing_notice(cx,ids!(notice));
            if self.ui.button(cx,ids!(start)).clicked(actions) {notice.show_or_hide(cx,&["林岚 · 项目负责人".into()]);}
            if self.ui.button(cx,ids!(stop)).clicked(actions) {notice.show_or_hide(cx,&[]);}
            if self.ui.button(cx,ids!(reset)).clicked(actions) {notice.clear(cx);}
        }
    }
}
