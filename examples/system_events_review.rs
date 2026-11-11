//! Offline native timeline fixture; no Matrix session or executor is started.
pub use makepad_widgets;
use makepad_widgets::*;
app_main!(App);
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    startup() do #(App::script_component(vm)) {
        ui: Root {main_window := Window {
            window.inner_size: vec2(720, 800)
            window.title: "Hagency · System updates review"
            body +: {flow: Down padding: 16 spacing: 12
                RinxPageTitle {text: "共读文案助手"}
                list := PortalList {width: Fill height: Fill auto_tail: false
                    State := SmallStateEvent {}
                    Empty := mod.widgets.Empty {}
                    Task := View {width: Fill height: Fit flow: Down padding: 16 spacing: 8
                        RinxPageTitle {text: "林岚 · 项目负责人"}
                        RinxLabel {width: Fill flow: Flow.Right{wrap: true} text: "请根据活动目标，起草社区共读的邀请文案。"}
                    }
                }
                RinxInput {width: Fill text: "输入消息"}
            }
        }}
    }
}
#[derive(Script,ScriptHook)]
struct App {#[live] ui: WidgetRef, #[rust] open: bool}
const UPDATES: [&str;16] = ["林岚创建了会话。","林岚加入了会话。","林岚更改了会话名称。","林岚设置了会话主题。","林岚更改了历史消息可见性。","林岚启用了端到端加密。","林岚更改了权限级别。","林岚更改了访客访问设置。","林岚更改了加入规则。","林岚邀请了执行服务。","执行服务加入了会话。","林岚邀请了共读文案助手。","共读文案助手加入了会话。","林岚更改了会话头像。","林岚更改了会话目录状态。","林岚更改了权限级别。"];
impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        makepad_widgets::theme_mod(vm);rinx::theme::init_standalone(vm);
        #[cfg(any(target_os="macos",target_os="ios"))] article_makepad::apple_fonts::install(vm);
        makepad_widgets::widgets_mod(vm);desktop_style::apply_widgets(vm);rinx::app::register_widgets(vm);self::script_mod(vm)
    }
    fn handle_event(&mut self,cx:&mut Cx,event:&Event) {
        if let Event::Draw(e) = event {
            let mut draw=CxDraw::new(cx,e);let mut cx=Cx2d::new(&mut draw);
            while let Some(child)=self.ui.draw(&mut cx,&mut Scope::empty()).step() {
                let list=child.as_portal_list();let mut list=list.borrow_mut().unwrap();list.set_item_range(&mut cx,0,17);
                list.set_skipped_ranges(if self.open {&[]} else {&[1..16]});
                while let Some(index)=list.next_visible_item(&mut cx) {
                    if index>=17 {list.item(&mut cx,index,id!(Empty));continue;}
                    let item=if index==16 {list.item(&mut cx,index,id!(Task))}
                    else if index>0 && !self.open {list.item(&mut cx,index,id!(Empty))}
                    else {
                        let item=list.item(&mut cx,index,id!(State));
                        item.label(&mut cx,ids!(content)).set_text(&mut cx,UPDATES[index]);
                        rinx::home::room_screen::review_system_update_group(&mut cx,&item,(index==0).then_some((16,self.open)));
                        item
                    };item.draw_all(&mut cx,&mut Scope::empty());
                }
            }return;
        }
        self.ui.handle_event(cx,event,&mut Scope::empty());
        if let Event::Actions(actions)=event {
            for (_,item) in self.ui.portal_list(cx,ids!(list)).items_with_actions(actions) {
                if item.button(cx,ids!(system_updates)).clicked(actions) {self.open=!self.open;cx.redraw_all();}
            }
        }
    }
}
