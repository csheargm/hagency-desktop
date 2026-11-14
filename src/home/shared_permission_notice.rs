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
        context := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_MUTED}
        request_details := ScrollYView {width: Fill height: Fit{max: FitBound.Abs(130.0)}
            details := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_INK}
        }
        hint := Label {width: Fill height: Fit flow: Flow.Right{wrap: true} draw_text.color: mod.widgets.RINX_MUTED}
        actions := View {width: Fill height: Fit flow: Down spacing: 4
            once := Button {width: Fill text: #(crate::i18n::tr("Approve once")) i18n_text: "Approve once"}
            session := Button {width: Fill text: #(crate::i18n::tr("Always approve for this session")) i18n_text: "Always approve for this session"}
            deny := Button {width: Fill text: #(crate::i18n::tr("Deny")) i18n_text: "Deny"}
        }
        next := Button {text: #(crate::i18n::tr("Next request")) i18n_text: "Next request" visible: false}
    }
}
/// What the notice shows, worked out from the room's requests and from this
/// client's own decisions (shared with the card on the message itself).
#[derive(Debug,Default,PartialEq)]
struct Display {
    visible:bool,
    /// The spinner beside the title: the Agent works, or a decision is on its way.
    waiting:bool,
    title:String,
    /// Which request this is, after an earlier one; empty hides it.
    context:String,
    /// The command, file or address the request is about; empty hides it.
    details:String,
    hint:String,
    once:bool,
    session:bool,
    deny:bool,
    next:bool,
}
#[derive(Script,ScriptHook,Widget)]
pub struct SharedPermissionNotice {
    #[deref] view:View,
    #[rust] identity:String,
    #[rust] room:String,
    #[rust] thread:String,
    #[rust] error:String,
    #[rust] nonce:u64,
    /// A management read is in flight. It never blocks the buttons: a click
    /// during a read is sent at once.
    #[rust] loading:bool,
    #[rust] timer:Timer,
    #[rust] active:bool,
    #[rust] rows:Vec<Value>,
    #[rust] selected:usize,
    #[rust] execution_text:String,
    #[rust] execution_busy:bool,
    #[rust] waiting:bool,
    /// Request ids shown in this room or thread, in order, to number them.
    #[rust] seen:Vec<String>,
    /// The last request this client decided here, and how.
    #[rust] last_decision:Option<(String,String)>,
}
impl SharedPermissionNotice {
    /// Shows the spinner while the Agent works or a decision is on its way.
    /// The spinner updates its retained draw call without re-running the draw
    /// of the timeline that contains it.
    fn sync_animation(&mut self,cx:&mut Cx) {
        self.view.widget(cx,ids!(activity_spinner)).set_visible(cx,self.active && self.waiting);
    }
    fn display(&mut self)->Display {
        use crate::hagency::pending_approvals::{self as approvals,LocalDecision};
        use crate::i18n::{format,tr};
        let Some(row)=self.rows.get(self.selected) else {
            // No request: the run's state, and what was last decided here.
            let hint=match &self.last_decision {
                Some((_,decision)) if self.execution_busy && self.error.is_empty() =>
                    format("Last request: {result}. The Agent is continuing.",&[("result",tr(approvals::decided_label(decision)).to_owned())]),
                _=>String::new(),
            };
            return Display {
                visible:!self.execution_text.is_empty() || !self.error.is_empty(),
                waiting:self.execution_busy && self.error.is_empty(),
                title:if self.error.is_empty(){tr(&self.execution_text).to_owned()}else{tr(&self.error).to_owned()},
                hint,
                ..Display::default()
            };
        };
        let a=&row["approval"];
        let id=a["id"].as_str().unwrap_or_default().to_owned();
        if !self.seen.contains(&id) {self.seen.push(id.clone());}
        let number=self.seen.iter().position(|seen|*seen==id).map_or(1,|i|i+1);
        let local=approvals::local_decision(&id);
        let (asked,details)=approvals::describe(row);
        let recorded=row["decision"].as_str();
        let open=recorded.is_none() && matches!(local,LocalDecision::None|LocalDecision::Failed(..));
        let can=open && row["canDecide"]==true && row["encryptedPending"]!=true && self.error.is_empty();
        // Once answered, the title says so at once; the Agent's next request
        // then replaces it and is marked as a new one.
        let title=match (&local,recorded) {
            (LocalDecision::Sending(decision),_)=>format("Sending: {decision}…",&[("decision",tr(approvals::decision_label(decision)).to_owned())]),
            (LocalDecision::Sent(decision),_)=>tr(approvals::sent_status(decision)).to_owned(),
            (_,Some(decision))=>tr(approvals::sent_status(decision)).to_owned(),
            _=>asked,
        };
        let mut context=Vec::new();
        match self.last_decision.as_ref().filter(|(previous,_)|*previous!=id) {
            Some((_,decision))=>context.push(format("Previous request: {result}. This is a new request ({n}).",
                &[("result",tr(approvals::decided_label(decision)).to_owned()),("n",number.to_string())])),
            None if number>1=>context.push(format("Request {n}",&[("n",number.to_string())])),
            None=>{},
        }
        // When the owner decides for someone else, say who asked.
        if let Some(requester)=a["requesterMxid"].as_str() && a["deciderMxid"].as_str()!=Some(requester) {
            context.push(format("Requested by {requester}",&[("requester",requester.to_owned())]));
        }
        let hint=if !self.error.is_empty() {tr(&self.error).to_owned()}
            else if matches!(local,LocalDecision::Failed(..)) {tr("Could not send the decision. Try again.").to_owned()}
            else if !open {String::new()}
            else if row["encryptedPending"]==true {tr("Approval becomes available after the encrypted details arrive.").to_owned()}
            else if row["canDecide"]!=true {format("Waiting for resource owner {owner}",&[("owner",a["ownerMxid"].as_str().unwrap_or("").to_owned())])}
            else if matches!(a["request"]["method"].as_str(),Some("claude/can_use_tool"|"octos/can_use_tool")) {tr("Each approval covers this one tool call.").to_owned()}
            else {tr("Session approval covers this Codex session only. It ends when this run stops.").to_owned()};
        Display {
            visible:true,
            waiting:!open && !matches!(local,LocalDecision::Failed(..)),
            title,
            context:context.join("\n"),
            details,
            hint,
            once:can,
            session:can && a["request"]["sessionAllowed"]==true,
            deny:can,
            next:self.rows.len()>1,
        }
    }
    fn render(&mut self,cx:&mut Cx) {
        let shown=self.display();
        self.waiting=shown.waiting;
        self.sync_animation(cx);
        self.view.set_visible(cx,shown.visible);
        self.view.label(cx,ids!(title)).set_text(cx,&shown.title);
        for (id,text) in [(ids!(context),&shown.context),(ids!(details),&shown.details),(ids!(hint),&shown.hint)] {
            self.view.label(cx,id).set_text(cx,text);
            self.view.widget(cx,id).set_visible(cx,!text.is_empty());
        }
        self.view.widget(cx,ids!(request_details)).set_visible(cx,!shown.details.is_empty());
        // Buttons that cannot be used are hidden, not merely disabled: the two
        // look alike here.
        for (id,offered) in [(ids!(once),shown.once),(ids!(session),shown.session),(ids!(deny),shown.deny)] {
            let button=self.view.button(cx,id);
            if offered && !self.view.widget(cx,id).visible() {button.reset_hover(cx);}
            button.set_visible(cx,offered);
            button.set_enabled(cx,offered);
        }
        self.view.widget(cx,ids!(actions)).set_visible(cx,shown.once || shown.session || shown.deny);
        self.view.widget(cx,ids!(next)).set_visible(cx,shown.next);
        self.view.redraw(cx);
    }
    /// Read the room's requests and the run's state again.
    fn poll(&mut self,_cx:&mut Cx) {
        if self.loading || !self.active || self.room.is_empty(){return;}
        let Ok(service)=Service::for_current() else {return};
        if service.identity()!=self.identity{return;}
        self.loading=true;self.nonce=self.nonce.wrapping_add(1);
        let identity=self.identity.clone();let room=self.room.clone();let thread=self.thread.clone();let nonce=self.nonce;
        crate::sliding_sync::spawn_async_task(async move {
            let value=service.shared_permissions(ManagementCommand::SharedApprovals{room_id:Some(room.clone())}).await;
            Cx::post_action(PermissionResult{identity,room,thread,nonce,value});
        });
    }
    /// Send the decision for the request on screen, through the same store as
    /// the card on the message, so both show it.
    fn decide(&mut self,cx:&mut Cx,decision:&str) {
        if !self.active || !self.error.is_empty() {return;}
        let Some(row)=self.rows.get(self.selected) else {return};
        let id=row["approval"]["id"].as_str().unwrap_or_default().to_owned();
        if crate::hagency::pending_approvals::decide(row,decision) {
            self.last_decision=Some((id,decision.to_owned()));
            cx.action(crate::hagency::pending_approvals::PendingApprovalsChanged);
            self.render(cx);
        }
    }
}
impl Widget for SharedPermissionNotice {
    fn handle_event(&mut self,cx:&mut Cx,event:&Event,scope:&mut Scope) {
        use crate::hagency::pending_approvals::{PendingApprovalDecided,PendingApprovalsChanged};
        if !Service::for_current().is_ok_and(|s|s.identity()==self.identity){self.execution_busy=false;self.waiting=false;self.sync_animation(cx);self.view.set_visible(cx,false);self.rows.clear();return;}
        if self.timer.is_event(event).is_some(){self.timer=Timer::default();self.poll(cx);}
        if let Event::Actions(actions)=event {
            for action in actions {
                if let Some(done)=action.downcast_ref::<PermissionResult>() && self.loading && done.identity==self.identity && done.room==self.room && done.thread==self.thread && done.nonce==self.nonce {
                    self.loading=false;
                    match &done.value {
                        Ok(value)=>{
                            self.error.clear();
                            self.execution_busy=execution_busy(value,&self.room,&self.thread);
                            self.execution_text=execution_notice(value,&self.room,&self.thread).unwrap_or_default().into();
                            self.rows=value["approvals"].as_array().cloned().unwrap_or_default().into_iter().filter(|r|r["approval"]["roomId"]==self.room && (self.thread.is_empty() || r["approval"]["threadRoot"]==self.thread)).take(16).collect();
                            if self.selected>=self.rows.len(){self.selected=0;}
                            // The cards on the messages read the same requests.
                            if crate::hagency::pending_approvals::merge_room(&self.identity,&self.room,value,self.active){cx.action(PendingApprovalsChanged);}
                            self.render(cx);
                        },
                        Err(_)=>{self.error="Could not confirm permission status; retrying.".into();self.render(cx);},
                    }
                    if self.active{self.timer=cx.start_timeout(2.0);}
                }
                // A decision from either card: show its outcome now, and read
                // again soon for the Agent's next request.
                if let Some(decided)=action.downcast_ref::<PendingApprovalDecided>()
                    && (self.rows.iter().any(|r|r["approval"]["id"]==decided.id.as_str()) || self.last_decision.as_ref().is_some_and(|(id,_)|*id==decided.id)) {
                    self.render(cx);
                    if self.active && !self.loading {cx.stop_timer(self.timer);self.timer=cx.start_timeout(0.5);}
                }
                if action.downcast_ref::<PendingApprovalsChanged>().is_some(){self.render(cx);}
            }
            for (id,decision) in [(ids!(once),"approve_once"),(ids!(session),"approve_session"),(ids!(deny),"deny")] {
                if self.view.button(cx,id).clicked(actions){self.decide(cx,decision);}
            }
            if self.view.button(cx,ids!(next)).clicked(actions) && !self.rows.is_empty(){self.selected=(self.selected+1)%self.rows.len();self.render(cx);}
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
            cx.stop_timer(this.timer);this.timer=Timer::default();this.execution_busy=false;this.waiting=false;this.sync_animation(cx);this.identity=identity;this.room=room.into();this.thread=thread.into();this.error.clear();this.execution_text.clear();this.nonce=this.nonce.wrapping_add(1);this.loading=false;this.rows.clear();this.selected=0;this.seen.clear();this.last_decision=None;this.active=true;this.view.set_visible(cx,false);this.poll(cx);
        }else if this.active && !this.loading && this.timer.is_empty(){this.poll(cx);}
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

    fn notice(cx:&mut Cx)->SharedPermissionNoticeRef {
        cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            vm.bx.captured_errors=Some(Vec::new());
            let value=script_eval!(vm,{mod.widgets.SharedPermissionNotice {}});
            let widget=WidgetRef::script_from_value(vm,value);
            widget.register_as_ui_root(vm);
            assert!(vm.take_errors().is_empty());
            widget.as_shared_permission_notice()
        })
    }

    #[test]
    fn native_activity_spinner_shows_only_while_busy_and_starts_no_redraw_clock() {
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let widget=notice(&mut cx);
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

    /// The person who sent the message approves an Agent's tool calls one after
    /// another. Each click must show at once, and the next request must not
    /// look like the one just answered.
    #[test]
    fn a_click_shows_its_decision_at_once_and_the_next_request_reads_as_new() {
        use crate::hagency::pending_approvals::{forget_identity,record_for_test,LocalDecision,TEST_TURN};
        let _turn=TEST_TURN.lock().unwrap_or_else(|e|e.into_inner());
        forget_identity();
        let mut cx=Cx::new(Box::new(|_,_|{}));
        let widget=notice(&mut cx);
        let mut n=widget.borrow_mut().unwrap();
        let request=|id:&str,command:&str| json!({"approval":{"id":id,"agentName":"Claude","requesterMxid":"@e:x","deciderMxid":"@e:x",
            "request":{"method":"claude/can_use_tool","params":{"toolName":"Bash","input":{"command":command,"description":"d"}},"sessionAllowed":false}},
            "digest":"d","decision":null,"canDecide":true});
        n.active=true;n.execution_busy=true;n.execution_text="Agent is working…".into();
        n.rows=vec![request("sap_1","ls -la")];
        let first=n.display();
        assert_eq!((first.title.as_str(),first.details.as_str()),("Claude wants to use Bash","ls -la"));
        assert!(first.once && first.deny && !first.session,"a Claude tool call offers no session approval");
        assert!(first.context.is_empty() && !first.waiting);
        assert_eq!(first.hint,"Each approval covers this one tool call.");
        // A read in flight never takes the buttons away.
        n.loading=true;
        assert!(n.display().once);
        n.loading=false;
        // The click: the card says it is sending and offers no buttons.
        n.last_decision=Some(("sap_1".into(),"approve_once".into()));
        record_for_test("sap_1",LocalDecision::Sending("approve_once".into()));
        let sending=n.display();
        assert_eq!(sending.title,"Sending: Approve once…");
        assert!(!sending.once && !sending.deny && sending.waiting && sending.hint.is_empty());
        record_for_test("sap_1",LocalDecision::Sent("approve_once".into()));
        assert_eq!(n.display().title,"Approved once. Waiting for the Agent to continue.");
        // Used: the Agent runs the tool.
        n.rows.clear();
        let between=n.display();
        assert_eq!(between.title,"Agent is working…");
        assert_eq!(between.hint,"Last request: Approved once. The Agent is continuing.");
        assert!(between.waiting);
        // Its next request is a new one, and says so.
        n.rows=vec![request("sap_2","find . -type f")];
        let second=n.display();
        assert_eq!((second.title.as_str(),second.details.as_str()),("Claude wants to use Bash","find . -type f"));
        assert_eq!(second.context,"Previous request: Approved once. This is a new request (2).");
        assert!(second.once && second.deny && !second.waiting);
        // A send that failed offers the buttons again.
        record_for_test("sap_2",LocalDecision::Failed("deny".into(),"approval_expired".into()));
        let failed=n.display();
        assert!(failed.once && failed.deny && !failed.waiting);
        assert_eq!(failed.hint,"Could not send the decision. Try again.");
        // A request decided on another device shows that, without buttons.
        let mut elsewhere=request("sap_3","pwd");
        elsewhere["decision"]=json!("deny");
        n.rows=vec![elsewhere];
        let decided=n.display();
        assert_eq!(decided.title,"Denied. The Agent will not run this call.");
        assert!(!decided.once && !decided.deny);
        // The owner deciding for someone else sees who asked.
        let mut owners=request("sap_4","pwd");
        owners["approval"]["deciderMxid"]=Value::Null;
        owners["approval"]["requesterMxid"]=json!("@other:x");
        n.rows=vec![owners];
        assert!(n.display().context.ends_with("Requested by @other:x"));
        drop(n);
        forget_identity();
    }
}
