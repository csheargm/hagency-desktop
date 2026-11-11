//! A notice that slides into view when someone is typing.

use makepad_widgets::*;

use crate::shared::bouncing_dots::BouncingDotsWidgetExt;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*


    mod.widgets.TYPING_NOTICE_ANIMATION_DURATION_SECS = 0.3

    mod.widgets.TypingNotice = set_type_default() do #(TypingNotice::register_widget(vm)) {
        ..mod.widgets.SolidView

        visible: false
        width: Fill
        height: 30
        flow: Right
        padding: Inset{left: 12.0, top: 8.0, bottom: 8.0, right: 10.0}

        show_bg: true
        draw_bg +: {
            color: mod.widgets.RINX_SELECTED
        }

        typing_label := Label {
            align: Align{x: 0.0, y: 0.5},
            padding: Inset{left: 5.0, right: 0.0, top: 0.0, bottom: 0.0}
            draw_text +: {
                color: (TYPING_NOTICE_TEXT_COLOR),
                text_style: REGULAR_TEXT {font_size: (10.5 * mod.widgets.RINX_TEXT_SCALE)}
            }
            text: #(crate::i18n::tr("Someone is typing")) i18n_text: "Someone is typing"
        }

        bouncing_dots := BouncingDots {
            margin: Inset{top: 1.1, left: -4 }
            padding: 0.0,
            draw_bg.color: (TYPING_NOTICE_TEXT_COLOR)
        }

        animator: Animator{
            typing_notice_animator: {
                default: @show
                show: AnimatorState{
                    redraw: true,
                    from: { all: Forward { duration: (mod.widgets.TYPING_NOTICE_ANIMATION_DURATION_SECS) } }
                    apply: { height: 30 }
                }
                hide: AnimatorState{
                    redraw: true,
                    from: { all: Forward { duration: (mod.widgets.TYPING_NOTICE_ANIMATION_DURATION_SECS) } }
                    apply: { height: 0 }
                }
            }
        }
    }
}

// Matrix typing updates contain user IDs, not their remaining server TTL.
// Bound stale UI when a stop/expiry update is lost. Fresh updates renew this lease.
const STALE_TYPING_SECS: f64 = 30.0;

/// A notice that slides into view when someone is typing.
#[derive(Script, ScriptHook, Widget, Animator)]
pub struct TypingNotice {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[apply_default] animator: Animator,
    #[rust] expiry: Timer,
    #[rust] expires_at: Option<std::time::Instant>,
}

impl Widget for TypingNotice {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.expiry.is_event(event).is_some() { self.clear(cx); }
        if matches!(event, Event::Background | Event::Shutdown) { self.clear(cx); }
        if self.animator_handle_event(cx, event).must_redraw() {
            self.redraw(cx);
        }
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        // A room can temporarily stop forwarding timer events while its board
        // is visible. Recheck the lease when the notice is drawn again.
        if self.expires_at.is_some_and(|at| std::time::Instant::now() >= at) { self.clear(cx); }
        self.view.draw_walk(cx, scope, walk)
    }
}

fn typing_text(users: &[String]) -> Option<String> {
    let (key, values) = match users {
        [] => return None,
        [user] => ("{user} is typing", vec![("user", user.clone())]),
        [user1, user2] => ("{user1} and {user2} are typing", vec![("user1", user1.clone()), ("user2", user2.clone())]),
        [user1, user2, user3] => ("{user1}, {user2}, and {user3} are typing", vec![("user1", user1.clone()), ("user2", user2.clone()), ("user3", user3.clone())]),
        [user1, user2, others @ ..] => ("{user1}, {user2}, and {count} others are typing", vec![("user1", user1.clone()), ("user2", user2.clone()), ("count", others.len().to_string())]),
    };
    Some(crate::i18n::format(key, &values) + " ")
}

impl TypingNotice {
    /// Shows or hides the typing notice based on whether there are any typing users.
    fn clear(&mut self, cx: &mut Cx) {
        cx.stop_timer(self.expiry);
        self.expiry = Timer::empty();
        self.expires_at = None;
        self.animator_cut(cx, ids!(typing_notice_animator.hide));
        self.view.bouncing_dots(cx, ids!(bouncing_dots)).stop_animation(cx);
        self.view.label(cx, ids!(typing_label)).set_text(cx, "");
        self.view.set_visible(cx, false);
        self.redraw(cx);
    }
    fn show_or_hide(&mut self, cx: &mut Cx, typing_users: &[String]) {
        cx.stop_timer(self.expiry);
        self.expiry = Timer::empty();
        let Some(typing_notice_text) = typing_text(typing_users) else {
            self.clear(cx);
            return;
        };
        self.expiry = cx.start_timeout(STALE_TYPING_SECS);
        self.expires_at = Some(std::time::Instant::now() + std::time::Duration::from_secs_f64(STALE_TYPING_SECS));
        // Set the typing notice text and make its view visible.
        self.view.label(cx, ids!(typing_label)).set_text(cx, &typing_notice_text);
        self.view.set_visible(cx, true);
        // Animate in the typing notice view (sliding it up from the bottom).
        self.animator_play(cx, ids!(typing_notice_animator.show));
        // Start the typing notice text animation of bouncing dots.
        self.view.bouncing_dots(cx, ids!(bouncing_dots)).start_animation(cx);
    }
}

impl TypingNoticeRef {
    /// Clear the old room/session before reusing its widget.
    pub fn clear(&self, cx: &mut Cx) {
        if let Some(mut inner) = self.borrow_mut() { inner.clear(cx); }
    }

    /// Reset the notice when its room is no longer shown.
    pub fn reset(&self, cx: &mut Cx) {
        self.clear(cx);
    }

    /// Shows or hides the typing notice based on whether there are any typing users.
    pub fn show_or_hide(&self, cx: &mut Cx, typing_users: &[String]) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.show_or_hide(cx, typing_users);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn typing_names_and_other_count_cover_one_two_three_and_many_people() {
        let users = ["Alice", "Bob", "Carol", "Dora"].map(str::to_owned);
        assert_eq!(super::typing_text(&[]), None);
        assert_eq!(super::typing_text(&users[..1]).unwrap(), "Alice is typing ");
        assert_eq!(super::typing_text(&users[..2]).unwrap(), "Alice and Bob are typing ");
        assert_eq!(super::typing_text(&users[..3]).unwrap(), "Alice, Bob, and Carol are typing ");
        assert_eq!(super::typing_text(&users).unwrap(), "Alice, Bob, and 2 others are typing ");
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;
    fn fixture() -> (Cx, WidgetRef) {
        let mut cx = Cx::new(Box::new(|_,_|{}));
        let widget = cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);crate::app::register_widgets(vm);
            vm.bx.captured_errors=Some(Vec::new());
            let value=script_eval!(vm,{mod.widgets.TypingNotice {}});
            let widget=WidgetRef::script_from_value(vm,value);
            assert!(vm.take_errors().is_empty());widget
        });(cx,widget)
    }
    fn timeout(cx:&mut Cx,widget:&WidgetRef,id:Timer) {
        widget.handle_event(cx,&Event::Timer(TimerEvent {time:None,timer_id:id.0}),&mut Scope::empty());
    }
    #[test]
    fn missing_stop_expires_but_old_timer_cannot_erase_a_fresh_update() {
        let (mut cx,widget)=fixture();
        let mut notice=widget.borrow_mut::<TypingNotice>().unwrap();
        notice.show_or_hide(&mut cx,&["Alice".into()]);let old=notice.expiry;
        notice.show_or_hide(&mut cx,&["Bob".into()]);let current=notice.expiry;
        assert_ne!(old.0,current.0);drop(notice);
        timeout(&mut cx,&widget,old);
        assert!(widget.visible());
        assert_eq!(widget.label(&mut cx,ids!(typing_label)).text(),"Bob is typing ");
        timeout(&mut cx,&widget,current);
        assert!(!widget.visible());
        assert_eq!(widget.label(&mut cx,ids!(typing_label)).text(),"");
        assert_eq!(widget.borrow::<TypingNotice>().unwrap().expiry.0,Timer::empty().0);
    }
    #[test]
    fn returning_from_a_hidden_view_clears_an_expired_notice_even_if_its_timer_was_not_delivered() {
        use makepad_widgets::makepad_draw::cx_draw::CxDraw;
        let (mut cx,widget)=fixture();
        {
            let mut notice=widget.borrow_mut::<TypingNotice>().unwrap();
            notice.show_or_hide(&mut cx,&["Old room".into()]);
            notice.expires_at=Some(std::time::Instant::now()-std::time::Duration::from_secs(1));
        }
        // No timer event is sent. The first resumed draw must expire it.
        let pass=DrawPass::new(&mut cx);pass.set_size(&mut cx,dvec2(430.0,100.0));
        let mut list=DrawList2d::new(&mut cx);let event=DrawEvent::default();
        let mut draw=CxDraw::new(&mut cx,&event);let mut cx2d=Cx2d::new(&mut draw);
        cx2d.begin_pass(&pass,None);list.begin_always(&mut cx2d);
        cx2d.begin_root_turtle(dvec2(430.0,100.0),Layout::flow_down());
        widget.draw_walk_all(&mut cx2d,&mut Scope::empty(),Walk{width:Size::Fixed(430.0),height:Size::Fit{min:None,max:None},..Walk::default()});
        cx2d.end_pass_sized_turtle();list.end(&mut cx2d);cx2d.end_pass(&pass);
        drop(cx2d);drop(draw);
        assert!(!widget.visible());
        assert!(widget.borrow::<TypingNotice>().unwrap().expires_at.is_none());
    }

    #[test]
    fn stop_room_reset_and_background_clear_the_timer_and_old_identity() {
        let (mut cx,widget)=fixture();
        for mode in 0..3 {
            let mut notice=widget.borrow_mut::<TypingNotice>().unwrap();
            notice.show_or_hide(&mut cx,&["Old room".into()]);let old=notice.expiry;
            if mode==0 {notice.show_or_hide(&mut cx,&[]);} else if mode==1 {notice.clear(&mut cx);}
            drop(notice);
            if mode==2 {widget.handle_event(&mut cx,&Event::Background,&mut Scope::empty());}
            assert!(!widget.visible());
            assert_eq!(widget.borrow::<TypingNotice>().unwrap().expiry.0,Timer::empty().0);
            widget.borrow_mut::<TypingNotice>().unwrap().show_or_hide(&mut cx,&["New room".into()]);
            timeout(&mut cx,&widget,old);
            assert!(widget.visible());
            assert_eq!(widget.label(&mut cx,ids!(typing_label)).text(),"New room is typing ");
        }
    }
}
