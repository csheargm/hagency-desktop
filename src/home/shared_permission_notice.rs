//! Permission cards read through authenticated Hagency management, never chat text.
use makepad_widgets::*;
use serde_json::Value;
use crate::hagency::backend::Service;
use ::hagency::native_owner::ManagementCommand;

#[derive(Debug)]
struct PermissionResult {identity:String,room:String,thread:String,nonce:u64,value:Result<Value,String>}
fn execution_row<'a>(value:&'a Value,room:&str,thread:&str)->Option<&'a Value> {
    value["executions"].as_array()?.iter().find(|r|
        r["roomId"]==room && (thread.is_empty() || r["threadRoot"]==thread))
}
fn execution_busy(value:&Value,room:&str,thread:&str)->bool {
    execution_row(value,room,thread).is_some_and(|row|
        row["reason"]!="budget_blocked" && matches!(row["state"].as_str(),Some("started"|"pending"|"queued"|"acknowledged"|"bound"|"reply_pending")))
}
fn execution_notice(value:&Value,room:&str,thread:&str)->Option<&'static str> {
    let row=execution_row(value,room,thread)?;
    if row["reason"]=="owner_daemon_update_required" {
        return Some("Message has not started. The resource owner needs to update Hagency and start its service; the same message can then continue.");
    }
    if row["reason"]=="owner_daemon_unavailable" {
        return Some("Message has not started. Waiting for the resource owner's Hagency service to reconnect.");
    }
    if row["reason"]=="budget_blocked" {
        return Some(match row["budgetState"].as_str() {
            Some("usage_unresolved") => "The previous call has not been settled. Its reserved allowance is unavailable until usage is confirmed.",
            Some("exhausted") => "The Agent's allowance has been used. Check its next renewal period with the resource owner.",
            _ => "No allowance is currently available for this message. Check the Agent's allowance and unsettled calls.",
        });
    }
    if row["reason"]=="usage_unrepresentable" {return Some("The previous call has not been settled. Its reserved allowance is unavailable until usage is confirmed.");}
    match row["state"].as_str()? {
        "started"=>Some("Agent is working…"),
        "pending"|"queued"|"acknowledged"|"bound"=>Some("Message queued; waiting for Agent."),
        "unknown"=>Some("Run interrupted; no final answer confirmed. Send the message again if you still need it."),
        "failed"=>Some("Agent run failed."),
        "blocked"=>Some("Message could not start. Check the resource and Agent connection."),
        "paused"=>Some("Agent is paused."),
        "cancelled"|"expired"=>Some("This message was not processed; it will not retry automatically."),
        "reply_pending"=>Some("Answer generated; waiting for delivery."),
        "reply_unknown"=>Some("Answer delivery could not be confirmed."),
        _=>None,
    }
}
script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*
    mod.widgets.SharedPermissionNotice = #(SharedPermissionNotice::register_widget(vm)) {
        ..mod.widgets.View
        width: Fill height: Fit flow: Down spacing: 8 padding: 12
        visible: false show_bg: true draw_bg.color: mod.widgets.RINX_HOVER
        status := View {width: Fill height: Fit flow: Right spacing: 8 align: Align{y: 0.5}
            activity_spinner := PacedLoadingSpinner {width: 20 height: 20 visible: false draw_bg +: {color: mod.widgets.RINX_ACCENT stroke_width: 2.0}}
            title := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_INK}
        }
        context := Label {width: Fill height: Fit draw_text.color: mod.widgets.RINX_MUTED}
        request_details := ScrollYView {width: Fill height: 130
            details := Label {width: Fill height: Fit draw_text.color: mod.widgets.RINX_INK}
        }
        hint := Label {width: Fill height: Fit draw_text.color: mod.widgets.RINX_MUTED}
        actions := View {width: Fill height: Fit flow: Down spacing: 4
            once := Button {width: Fill text: #(crate::i18n::tr("Approve once")) i18n_text: "Approve once"}
            session := Button {width: Fill text: #(crate::i18n::tr("Always approve for this session")) i18n_text: "Always approve for this session"}
            deny := Button {width: Fill text: #(crate::i18n::tr("Deny")) i18n_text: "Deny"}
        }
        next := Button {text: #(crate::i18n::tr("Next request")) i18n_text: "Next request" visible: false}
    }
}
#[derive(Script,ScriptHook,Widget)]
pub struct SharedPermissionNotice {
    #[deref] view:View,
    #[rust] identity:String,
    #[rust] room:String,
    #[rust] thread:String,
    #[rust] error:String,
    #[rust] nonce:u64,
    #[rust] loading:bool,
    #[rust] timer:Timer,
    #[rust] active:bool,
    #[rust] rows:Vec<Value>,
    #[rust] selected:usize,
    #[rust] execution_text:String,
    #[rust] execution_busy:bool,
}
impl SharedPermissionNotice {
    /// Shows the spinner while the Agent works. The spinner updates its retained
    /// draw call without re-running the draw of the timeline that contains it.
    fn sync_animation(&mut self,cx:&mut Cx) {
        let animate=self.active && self.execution_busy && self.rows.is_empty() && self.error.is_empty();
        self.view.widget(cx,ids!(activity_spinner)).set_visible(cx,animate);
    }
    fn render(&mut self,cx:&mut Cx) {
        self.sync_animation(cx);
        let has_request=self.rows.get(self.selected).is_some();
        for id in [ids!(context),ids!(request_details),ids!(hint),ids!(actions)] {
            self.view.widget(cx,id).set_visible(cx,has_request);
        }
        let Some(row)=self.rows.get(self.selected) else {
            self.view.set_visible(cx,!self.execution_text.is_empty() || !self.error.is_empty());
            self.view.widget(cx,ids!(next)).set_visible(cx,false);
            self.view.label(cx,ids!(title)).set_text(cx,if self.error.is_empty(){crate::i18n::tr(&self.execution_text)}else{crate::i18n::tr(&self.error)});
            self.view.redraw(cx);return;
        };
        let a=&row["approval"];
        let pending=row["decision"].is_null();let can=row["canDecide"]==true && pending && !self.loading && self.error.is_empty();
        self.view.set_visible(cx,true);
        self.view.label(cx,ids!(title)).set_text(cx,&crate::i18n::format("{agent} · Permission request", &[("agent",a["agentName"].as_str().unwrap_or("Agent").to_owned())]));
        self.view.label(cx,ids!(context)).set_text(cx,&format!("{} · {}",a["requesterMxid"].as_str().unwrap_or(""),a["eventId"].as_str().unwrap_or("")));
        self.view.label(cx,ids!(details)).set_text(cx,&if row["encryptedPending"]==true {crate::i18n::tr("Waiting for encrypted permission details…").into()}else{serde_json::to_string_pretty(&a["request"]).unwrap_or_default()});
        let hint=if !pending {crate::i18n::tr("Decision sent; waiting for Agent").to_owned()}
        else if row["encryptedPending"]==true {crate::i18n::tr("Approval becomes available after the encrypted details arrive.").into()}
        else if row["canDecide"]!=true {crate::i18n::format("Waiting for resource owner {owner}", &[("owner",a["ownerMxid"].as_str().unwrap_or("").to_owned())])}
        else {crate::i18n::tr("Session approval covers this Codex session only. It ends when this run stops.").into()};
        self.view.label(cx,ids!(hint)).set_text(cx,if self.error.is_empty(){&hint}else{crate::i18n::tr(&self.error)});
        self.view.button(cx,ids!(once)).set_enabled(cx,can);
        self.view.button(cx,ids!(session)).set_enabled(cx,can && a["request"]["sessionAllowed"]==true);
        self.view.button(cx,ids!(deny)).set_enabled(cx,can);
        self.view.widget(cx,ids!(next)).set_visible(cx,self.rows.len()>1);
        self.view.redraw(cx);
    }
    fn request(&mut self,cx:&mut Cx,decision:Option<&str>) {
        if self.loading || !self.active || self.room.is_empty(){return;}
        let Ok(service)=Service::for_current() else {return};
        if service.identity()!=self.identity{return;}
        let command=if let Some(decision)=decision {
            let Some(row)=self.rows.get(self.selected) else {return};
            if row["canDecide"]!=true || !row["decision"].is_null() {return;}
            let (Some(id),Some(digest))=(row["approval"]["id"].as_str(),row["digest"].as_str()) else{return};
            ManagementCommand::SharedApprovalDecide{id:id.into(),digest:digest.into(),decision:decision.into()}
        }else{ManagementCommand::SharedApprovals{room_id:Some(self.room.clone())}};
        self.loading=true;self.nonce=self.nonce.wrapping_add(1);self.render(cx);
        let identity=self.identity.clone();let room=self.room.clone();let thread=self.thread.clone();let nonce=self.nonce;
        crate::sliding_sync::spawn_async_task(async move {
            let value=async {
                let result=service.shared_permissions(command).await?;
                if result.get("approvals").is_some(){Ok(result)}else{service.shared_permissions(ManagementCommand::SharedApprovals{room_id:Some(room.clone())}).await}
            }.await;
            Cx::post_action(PermissionResult{identity,room,thread,nonce,value});
        });
    }
}
impl Widget for SharedPermissionNotice {
    fn handle_event(&mut self,cx:&mut Cx,event:&Event,scope:&mut Scope) {
        if !Service::for_current().is_ok_and(|s|s.identity()==self.identity){self.execution_busy=false;self.sync_animation(cx);self.view.set_visible(cx,false);self.rows.clear();return;}
        if self.timer.is_event(event).is_some(){self.timer=Timer::default();self.request(cx,None);}
        if let Event::Actions(actions)=event {
            for action in actions {
                if let Some(done)=action.downcast_ref::<PermissionResult>() && self.loading && done.identity==self.identity && done.room==self.room && done.thread==self.thread && done.nonce==self.nonce {
                    self.loading=false;
                    match &done.value {
                        Ok(value)=>{self.error.clear();self.execution_busy=execution_busy(value,&self.room,&self.thread);self.execution_text=execution_notice(value,&self.room,&self.thread).unwrap_or_default().into();self.rows=value["approvals"].as_array().cloned().unwrap_or_default().into_iter().filter(|r|r["approval"]["roomId"]==self.room && (self.thread.is_empty() || r["approval"]["threadRoot"]==self.thread)).take(16).collect();if self.selected>=self.rows.len(){self.selected=0;}self.render(cx);},
                        Err(_)=>{self.error="Could not confirm permission status; retrying.".into();self.render(cx);},
                    }
                    if self.active{self.timer=cx.start_timeout(2.0);}
                }
            }
            for (id,decision) in [(ids!(once),"approve_once"),(ids!(session),"approve_session"),(ids!(deny),"deny")] {
                if self.view.button(cx,id).clicked(actions){self.request(cx,Some(decision));}
            }
            if self.view.button(cx,ids!(next)).clicked(actions) && !self.loading && !self.rows.is_empty(){self.selected=(self.selected+1)%self.rows.len();self.render(cx);}
        }
        self.view.handle_event(cx,event,scope);
    }
    fn draw_walk(&mut self,cx:&mut Cx2d,scope:&mut Scope,walk:Walk)->DrawStep{self.view.draw_walk(cx,scope,walk)}
}
impl SharedPermissionNoticeRef {
    pub fn set_active(&self,cx:&mut Cx,active:bool){if let Some(mut this)=self.borrow_mut(){if this.active!=active{this.active=active;this.sync_animation(cx);}if !active{cx.stop_timer(this.timer);this.timer=Timer::default();}}}
    pub fn set_room(&self,cx:&mut Cx,room:&str,thread:&str){
        let Some(mut this)=self.borrow_mut()else{return};
        let identity=Service::for_current().map(|s|s.identity()).unwrap_or_default();
        if this.identity!=identity || this.room!=room || this.thread!=thread {
            cx.stop_timer(this.timer);this.timer=Timer::default();this.execution_busy=false;this.sync_animation(cx);this.identity=identity;this.room=room.into();this.thread=thread.into();this.error.clear();this.execution_text.clear();this.nonce=this.nonce.wrapping_add(1);this.loading=false;this.rows.clear();this.selected=0;this.active=true;this.view.set_visible(cx,false);this.request(cx,None);
        }else if this.active && !this.loading && this.timer.is_empty(){this.request(cx,None);}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn shared_protocol_blocked_notice_keeps_the_original_message_and_stops_activity() {
        for (reason, explanation) in [
            ("owner_daemon_update_required", "update Hagency and start its service"),
            ("owner_daemon_unavailable", "service to reconnect"),
        ] {
            let value = json!({"executions":[
                {"roomId":"!other","threadRoot":"$thread","state":"started"},
                {"roomId":"!room","threadRoot":"$thread","eventId":"$original","state":"blocked","reason":reason}
            ]});
            let notice = execution_notice(&value, "!room", "$thread").unwrap();
            assert!(notice.starts_with("Message has not started."));
            assert!(notice.contains(explanation));
            assert!(!notice.contains("Send the message again"));
            assert!(!execution_busy(&value, "!room", "$thread"));
            assert!(execution_notice(&value, "!room", "$unrelated").is_none());
            assert!(!execution_busy(&value, "!room", "$unrelated"));
        }
        // The same retained event can become eligible when its daemon returns.
        let current = json!({"executions":[{"roomId":"!room","threadRoot":"$thread","eventId":"$original","state":"pending","reason":null}]});
        assert_eq!(execution_notice(&current, "!room", "$thread"), Some("Message queued; waiting for Agent."));
        assert!(execution_busy(&current, "!room", "$thread"));
    }

    #[test]
    fn shared_execution_notice_tracks_latest_message_and_exact_room_thread() {
        let v=json!({"executions":[
            {"roomId":"!room","threadRoot":"$new","state":"blocked","reason":"budget_blocked"},
            {"roomId":"!room","threadRoot":"$old","state":"unknown"},
            {"roomId":"!room","threadRoot":"$done","state":"replied"}
        ]});
        assert!(execution_notice(&v,"!room","").unwrap().contains("unsettled calls"));
        assert!(execution_notice(&v,"!room","$old").unwrap().contains("interrupted"));
        assert!(execution_notice(&v,"!room","$done").is_none());
        assert!(execution_notice(&v,"!room","$foreign").is_none());
        assert!(execution_notice(&v,"!foreign","").is_none());
        for (state, expected) in [("usage_unresolved","previous call"),("exhausted","used")] {
            let v=json!({"executions":[{"roomId":"!room","state":"blocked","reason":"budget_blocked","budgetState":state}]});
            assert!(execution_notice(&v,"!room","").unwrap().contains(expected));
            assert!(!execution_busy(&v,"!room",""));
        }
    }

    #[test]
    fn shared_activity_animation_stops_on_terminal_permission_and_scope_changes() {
        for state in ["pending","queued","bound","started","reply_pending"] {
            let v=json!({"executions":[{"roomId":"!room","threadRoot":"$thread","state":state}]});
            assert!(execution_busy(&v,"!room","$thread"));
            assert!(!execution_busy(&v,"!room","$other"));
            assert!(!execution_busy(&v,"!other",""));
        }
        for state in ["unknown","failed","blocked","paused","expired","cancelled","replied","reply_unknown"] {
            let v=json!({"executions":[{"roomId":"!room","threadRoot":"$thread","state":state}]});
            assert!(!execution_busy(&v,"!room","$thread"));
        }
        assert!(!execution_busy(&json!({"executions":[{"roomId":"!room","state":"pending","reason":"budget_blocked"}]}),"!room",""));
        // Latest delivered reply must not resurrect an older interrupted run.
        assert!(!execution_busy(&json!({"executions":[{"roomId":"!room","state":"replied"},{"roomId":"!room","state":"started"}]}),"!room",""));
    }

    /// How many timers `f` starts: `Cx` hands out timer ids in sequence.
    fn timers_started(cx:&mut Cx,f:impl FnOnce(&mut Cx))->u64 {
        let probe=|cx:&mut Cx|{let t=cx.start_timeout(60.0);cx.stop_timer(t);t.0};
        let before=probe(cx);f(cx);probe(cx)-before-1
    }

    #[test]
    fn native_activity_spinner_shows_only_while_busy_and_starts_no_redraw_clock() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let widget=cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors=Some(Vec::new());
            let value=script_eval!(vm,{mod.widgets.SharedPermissionNotice {}});
            let widget=WidgetRef::script_from_value(vm,value);
            widget.register_as_ui_root(vm);
            assert!(vm.take_errors().is_empty());
            widget.as_shared_permission_notice()
        });
        let mut inner=widget.borrow_mut().unwrap();
        // (step, change, card shown, spinner shown)
        let steps:[(&str,fn(&mut SharedPermissionNotice),bool,bool);7]=[
            ("agent working",|n|{n.active=true;n.execution_busy=true;n.execution_text="Agent is working…".into();},true,true),
            ("permission requested",|n|n.rows.push(json!({"approval":{},"decision":null})),true,false),
            ("request answered",|n|n.rows.clear(),true,true),
            ("offline",|n|n.error="offline".into(),true,false),
            ("room deselected",|n|{n.error.clear();n.active=false;},true,false),
            ("room selected again",|n|n.active=true,true,true),
            ("agent finished",|n|{n.execution_busy=false;n.execution_text.clear();},false,false),
        ];
        for (step,change,card,spinner) in steps {
            change(&mut inner);
            let started=timers_started(&mut cx,|cx|inner.render(cx));
            assert_eq!(inner.view.visible(),card,"{step}: card");
            assert_eq!(inner.view.widget(&mut cx,ids!(activity_spinner)).visible(),spinner,"{step}: spinner");
            // The notice is the timeline's last row. Rendering the status must
            // not start a clock that redraws every visible message on each tick.
            assert_eq!(started,0,"{step}: the notice must not start its own redraw clock");
        }
    }
}
