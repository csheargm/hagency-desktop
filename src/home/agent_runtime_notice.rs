//! Read-only owner-DM processing status. Sending a message never starts a runtime.
use makepad_widgets::*;
use serde_json::Value;
use crate::hagency::{backend::Service,model::{Command,LocalAction},ui::HagencyAction};

#[derive(Debug)]
struct NoticeResult { identity:String, room:String, request:u64, target:Option<(String,String)>, text:String, attention:bool, failed:bool }
/// Normal or self-resolving states. The owner has nothing to do, so the notice
/// stays hidden; Open Service remains available from the chat info panel.
const QUIET: [&str; 8] = [
    "Processing requests on this device.",
    "Running in all connected Rooms on this device. New Rooms are included automatically; configured budgets and policy permissions still apply.",
    "Running in previously selected Rooms. Stop and start to enable automatic Room discovery.",
    "Starting service. Discovering connected Rooms…",
    "Checking this device's local runtime connection…",
    "Waiting for the previous runtime lease to be released…",
    "Service enabled. Waiting for an available Room.",
    "Processing is assigned to another device; this device does not run it.",
];
/// Show the notice only for states that need the owner, or when diagnostics follow the label.
fn needs_attention(label:&str,text:&str)->bool {!QUIET.contains(&label) || text.contains('\n')}
fn exact_direct_binding<'a>(bindings:&'a Value,agent:&str,room:&str)->Option<&'a Value> {
    bindings["bindings"].as_array()?.iter().find(|b|b["agentId"]==agent && b["roomId"]==room && b["scopeKind"]=="owner_direct" && b["projectId"].is_null() && b["state"]=="active" && b["id"].as_str().is_some_and(|id|!id.is_empty()))
}
fn room_runtime(value:&Value,agent:&str,binding:&str)->Result<Value,String> {
    let r=&value["runtime"];
    if r.is_null() {return Ok(serde_json::json!({"runtime":null}));}
    if r["agentId"]!=agent {return Err("runtime_status_unavailable".into());}
    if r["phase"]=="waiting_for_lease" {return Ok(value.clone());}
    if let Some(b)=r["bindings"].as_array().and_then(|bs|bs.iter().find(|b|b["bindingId"]==binding)) {
        let mut status=b.clone(); status["agentId"]=Value::String(agent.into());
        return Ok(serde_json::json!({"runtime":status}));
    }
    if r["bindingId"]==binding {return Ok(value.clone());}
    Ok(serde_json::json!({"runtime":null}))
}
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.AgentRuntimeNotice = #(AgentRuntimeNotice::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill height: Fit flow: Right spacing: 12 align: Align{y: 0.5} padding: Inset{left: 16 right: 12 top: 8 bottom: 8}
        visible: false show_bg: true draw_bg.color: mod.widgets.RINX_HOVER
        status := Label {width: Fill height: Fit draw_text +: {color: mod.widgets.RINX_MUTED text_style: theme.font_regular{font_size: (11 * mod.widgets.RINX_TEXT_SCALE)}}}
        View {
            width: 18 height: 18
            loading_status := PacedLoadingSpinner {visible: false width: 18 height: 18 draw_bg +: {color: mod.widgets.RINX_MUTED stroke_width: 2.0}}
        }
        open := RobrixNeutralIconButton {
            text: #(crate::i18n::tr("Open Service")) i18n_text: "Open Service"
            height: 32 padding: Inset{left: 12 right: 12 top: 0 bottom: 0}
            icon_walk: Walk{width: 0 height: 0} spacing: 0
        }
    }
}
#[derive(Script,ScriptHook,Widget)]
pub struct AgentRuntimeNotice {
    #[deref] view:View,
    #[rust] identity:String,
    #[rust] room:String,
    #[rust] request:u64,
    #[rust] loading:bool,
    #[rust] timer:Timer,
    #[rust] target:Option<(String,String)>,
    #[rust] active:bool,
}
impl AgentRuntimeNotice {
    fn set_loading(&mut self,cx:&mut Cx,loading:bool) {
        self.loading=loading;
        self.view.widget(cx,ids!(loading_status)).set_visible(cx,loading);
    }
    fn refresh(&mut self,cx:&mut Cx) {
        if self.loading || !self.active || self.room.is_empty() {return;}
        let Ok(service)=Service::for_current() else {self.view.set_visible(cx,false);return;};
        if service.identity()!=self.identity {self.view.set_visible(cx,false);return;}
        // Keep the last status and reserve the spinner's width so polling never
        // changes line wrapping or moves the timeline below the notice.
        self.set_loading(cx,true);
        if self.target.is_some() {self.view.button(cx,ids!(open)).set_enabled(cx,false);self.view.redraw(cx);}
        self.request=self.request.wrapping_add(1);let request=self.request;
        let identity=self.identity.clone(); let room=self.room.clone();
        crate::sliding_sync::spawn_async_task(async move {
            let result=tokio::time::timeout(std::time::Duration::from_secs(30),async {
                let roster=service.execute(Command::Agents).await?;
                if roster["ownerMxid"].as_str()!=crate::sliding_sync::current_user_id().as_ref().map(|id|id.as_str()) {return Err("matrix_account_changed".into());}
                let Some(agent)=roster["agents"].as_array().and_then(|a|a.iter().find(|a|a["ownerDirectRoomId"]==room)) else {return Ok(None);};
                let agent_id=agent["id"].as_str().ok_or("invalid_agent")?.to_owned();
                let bindings=service.execute(Command::Bindings {agent_id:agent_id.clone()}).await?;
                let binding=exact_direct_binding(&bindings,&agent_id,&room).ok_or("owner_direct_unavailable")?;
                let binding_id=binding["id"].as_str().ok_or("invalid_binding")?.to_owned();
                let current=roster["currentDeviceId"].as_str().filter(|id|!id.is_empty());
                let here=current.is_some_and(|id|agent["executionDeviceId"]==id && roster["devices"].as_array().is_some_and(|ds|ds.iter().any(|d|d["id"]==id && d["revoked"]==false)));
                let (label,text)=if here {
                    let result=service.execute(Command::Local {agent_id:agent_id.clone(),action:LocalAction::Runtime,input:None,binding_id:None,requester:None}).await.and_then(|v|room_runtime(&v,&agent_id,&binding_id));
                    (crate::hagency::ui::runtime_list_label(Some(&result),&agent_id),crate::hagency::ui::runtime_service_description(Some(&result),&agent_id))
                } else {
                    let label=if current.is_none() {"Local processing could not be verified. Refresh to retry."}
                    else if agent["executionDeviceId"].as_str().is_none_or(|id|id.is_empty()) {"No available execution device. Open Agent settings to assign this device."}
                    else {"Processing is assigned to another device; this device does not run it."};
                    (label,label.to_owned())
                };
                Ok::<_,String>(Some((agent_id,binding_id,needs_attention(label,&text),text)))
            }).await;
            let (target,text,attention,failed)=match result {
                Ok(Ok(Some((agent,binding,attention,text))))=>(Some((agent,binding)),text,attention,false),
                Ok(Ok(None))=>(None,String::new(),false,false),
                _=>(None,"Local processing could not be verified. Refresh to retry.".into(),true,true),
            };
            Cx::post_action(NoticeResult {identity,room,request,target,text,attention,failed});
        });
    }
}
impl Widget for AgentRuntimeNotice {
    fn handle_event(&mut self,cx:&mut Cx,event:&Event,scope:&mut Scope) {
        if !Service::for_current().is_ok_and(|s|s.identity()==self.identity) {self.view.set_visible(cx,false);self.view.widget(cx,ids!(loading_status)).set_visible(cx,false);self.target=None;return;}
        if self.timer.is_event(event).is_some() {self.timer=Timer::default();if self.active {self.refresh(cx);}}
        if let Event::Actions(actions)=event {
            for action in actions {
                if let Some(done)=action.downcast_ref::<NoticeResult>()
                    && done.identity==self.identity && done.room==self.room && done.request==self.request {
                    self.set_loading(cx,false);if !done.failed {self.target=done.target.clone();}
                    self.view.label(cx,ids!(status)).set_text(cx,crate::i18n::tr(&done.text));
                    self.view.set_visible(cx,self.target.is_some() && done.attention);self.view.button(cx,ids!(open)).set_enabled(cx,self.target.is_some() && !done.failed);
                    self.timer=cx.start_timeout(10.0);self.view.redraw(cx);
                }
            }
            if self.view.button(cx,ids!(open)).clicked(actions) && let Some((agent,binding))=&self.target {
                cx.action(HagencyAction::AgentRun {agent_id:agent.clone(),binding_id:binding.clone()});
            }
        }
        self.view.handle_event(cx,event,scope);
    }
    fn draw_walk(&mut self,cx:&mut Cx2d,scope:&mut Scope,walk:Walk)->DrawStep {self.view.draw_walk(cx,scope,walk)}
}
impl AgentRuntimeNoticeRef {
    pub fn set_active(&self,cx:&mut Cx,active:bool) {
        if let Some(mut this)=self.borrow_mut() {
            this.active=active;
            if !active {cx.stop_timer(this.timer);this.timer=Timer::default();}
        }
    }
    pub fn set_room(&self,cx:&mut Cx,room:&str) {
        let Some(mut this)=self.borrow_mut() else {return;};
        this.active=true;
        let identity=Service::for_current().map(|s|s.identity()).unwrap_or_default();
        if identity!=this.identity || room!=this.room {
            cx.stop_timer(this.timer);this.timer=Timer::default();
            this.identity=identity;this.room=room.into();this.request=this.request.wrapping_add(1);this.set_loading(cx,false);this.target=None;
            this.view.set_visible(cx,false);
            if !this.identity.is_empty() {this.refresh(cx);}
        } else if this.timer.is_empty() && !this.loading && !this.identity.is_empty() {this.refresh(cx);}
    }
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[test]
    fn polling_spinner_preserves_status_wrapping_and_notice_height() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        let mut cx=Cx::new(Box::new(|_,_| {}));
        let mut notice=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);
            vm.bx.captured_errors=Some(Vec::new());
            let value=script_eval!(vm,{mod.widgets.AgentRuntimeNotice {}});
            let notice=AgentRuntimeNotice::script_from_value(vm,value);
            assert!(vm.take_errors().is_empty());notice
        });
        let text="Running in all connected Rooms on this device. New Rooms are included automatically; configured budgets and policy permissions still apply.";
        notice.view.set_visible(&mut cx,true);
        notice.view.label(&mut cx,ids!(status)).set_text(&mut cx,text);
        for width in [360.0,560.0,900.0] {
            let mut geometry=Vec::new();
            for loading in [false,true,false] {
                notice.set_loading(&mut cx,loading);
                cx.redraw_all();
                let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(width,300.0));
                let mut list=DrawList2d::new(&mut cx);
                let event=DrawEvent::default();let mut draw=CxDraw::new(&mut cx,&event);let mut cx2d=Cx2d::new(&mut draw);
                cx2d.begin_pass(&pass,None);list.begin_always(&mut cx2d);cx2d.begin_root_turtle(dvec2(width,300.0),Layout::flow_down());
                notice.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk {width:Size::Fixed(width),height:Size::fit(),..Walk::default()});
                cx2d.end_pass_sized_turtle();list.end(&mut cx2d);cx2d.end_pass(&pass);
                drop(cx2d);drop(draw);
                assert_eq!(notice.view.label(&mut cx,ids!(status)).text(),text);
                assert_eq!(notice.view.widget(&mut cx,ids!(loading_status)).visible(),loading);
                geometry.push((notice.view.area().rect(&cx),notice.view.label(&mut cx,ids!(status)).area().rect(&cx),notice.view.button(&mut cx,ids!(open)).area().rect(&cx)));
            }
            assert!(geometry[0].0.size.y>0.0);
            assert_eq!(geometry[0],geometry[1],"polling must not resize the notice or move Open Service at width {width}");
            assert_eq!(geometry[0],geometry[2]);
        }
    }
    #[test] fn notice_shows_only_states_that_need_the_owner() {
        for quiet in QUIET {assert!(!needs_attention(quiet,quiet),"{quiet}");}
        assert!(needs_attention("Processing requests on this device.","Processing requests on this device.\nRetry in 5s"));
        for loud in ["Not running on this device. Open Service to configure and start.","Waiting for your approval before continuing.",
            "The runtime stopped with an error. Check the details below.","Local processing could not be verified. Refresh to retry.",
            "No available execution device. Open Agent settings to assign this device."] {assert!(needs_attention(loud,loud),"{loud}");}
        // Every quiet label is one runtime_list_label or the assigned-elsewhere text can produce.
        let source=include_str!("../hagency/ui.rs");
        for quiet in &QUIET[..7] {assert!(source.contains(&format!("\"{quiet}\"")),"{quiet}");}
    }
    #[test] fn direct_notice_requires_exact_owner_direct_scope_and_active_binding() {
        let good=json!({"bindings":[{"id":"b","agentId":"a","roomId":"!r:s","scopeKind":"owner_direct","projectId":null,"state":"active"}]});
        assert!(exact_direct_binding(&good,"a","!r:s").is_some());
        for (key,value) in [("scopeKind",json!("project")),("state",json!("paused")),("agentId",json!("other")),("roomId",json!("!other:s")),("projectId",json!("p"))] {
            let mut bad=good.clone();bad["bindings"][0][key]=value;assert!(exact_direct_binding(&bad,"a","!r:s").is_none());
        }
    }
    #[test] fn direct_status_never_uses_other_room_runtime_or_foreign_agent() {
        let v=json!({"runtime":{"agentId":"a","phase":"online","bindings":[{"bindingId":"other","phase":"online"}]}});
        assert!(room_runtime(&v,"a","b").unwrap()["runtime"].is_null());
        assert!(room_runtime(&v,"foreign","b").is_err());
        let waiting=json!({"runtime":{"agentId":"a","phase":"waiting_for_lease"}});
        assert_eq!(room_runtime(&waiting,"a","b").unwrap()["runtime"]["phase"],"waiting_for_lease");
        let v=json!({"runtime":{"agentId":"a","phase":"online","bindings":[{"bindingId":"b","phase":"stopped"}]}});
        assert_eq!(room_runtime(&v,"a","b").unwrap()["runtime"]["phase"],"stopped");
    }
}
