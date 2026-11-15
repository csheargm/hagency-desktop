use super::*;
use matrix_sdk::ruma::owned_event_id;
use matrix_sdk_ui::timeline::TimelineEventItemId;

fn details(abilities: MessageAbilities) -> MessageDetails {
    MessageDetails {
        item_id: 7,
        timeline_event_id: TimelineEventItemId::EventId(owned_event_id!("$toolbar:example.org")),
        related_event_id: None,
        thread_root_event_id: None,
        room_screen_widget_uid: WidgetUid(42),
        should_be_highlighted: false,
        abilities,
        selected_text: None,
    }
}

#[test]
fn each_button_maps_to_an_existing_message_action() {
    let pos = dvec2(10.0, 20.0);
    let d = || details(MessageAbilities::all());
    assert!(matches!(
        toolbar_action(ToolbarButton::React, d(), pos),
        MessageAction::OpenMessageContextMenu { details, abs_pos, .. } if details.item_id == 7 && abs_pos == pos
    ));
    assert!(matches!(
        toolbar_action(ToolbarButton::More, d(), pos),
        MessageAction::OpenMessageContextMenu { abs_pos, .. } if abs_pos == pos
    ));
    assert!(matches!(toolbar_action(ToolbarButton::ReplyInThread, d(), pos), MessageAction::ReplyInThread(x) if x.item_id == 7));
    assert!(matches!(toolbar_action(ToolbarButton::CopyLink, d(), pos), MessageAction::CopyLink(x) if x.item_id == 7));
    assert!(matches!(toolbar_action(ToolbarButton::Forward, d(), pos), MessageAction::SelectForward(x) if x.item_id == 7));
}

#[test]
fn buttons_follow_the_context_menu_ability_flags() {
    let all = MessageAbilities::all();
    assert!(ToolbarButton::ALL.iter().all(|b| b.is_shown(all)));

    // A local echo with no abilities still offers "…", and nothing else.
    let none = MessageAbilities::empty();
    let shown: Vec<_> = ToolbarButton::ALL.into_iter().filter(|b| b.is_shown(none)).collect();
    assert_eq!(shown, vec![ToolbarButton::More]);

    let thread_view = all - MessageAbilities::CanReplyInThread;
    assert!(!ToolbarButton::ReplyInThread.is_shown(thread_view));
    assert!(!ToolbarButton::CopyLink.is_shown(all - MessageAbilities::HasEventId));
    assert!(!ToolbarButton::Forward.is_shown(all - MessageAbilities::CanForward));
    assert!(!ToolbarButton::React.is_shown(all - MessageAbilities::CanReact));
}

#[test]
fn toolbar_sits_inside_the_message_top_right_corner() {
    let size = toolbar_size(5);
    assert_eq!(size, dvec2(5.0 * 28.0 + 4.0 * 2.0 + 6.0, 34.0));
    assert_eq!(toolbar_size(1).x, 28.0 + 6.0);

    let message = Rect { pos: dvec2(100.0, 50.0), size: dvec2(600.0, 80.0) };
    let origin = toolbar_origin(message, size);
    assert_eq!(origin, dvec2(700.0 - 16.0 - size.x, 54.0));

    // A message narrower than the toolbar keeps it at the message's left edge.
    let narrow = Rect { pos: dvec2(100.0, 50.0), size: dvec2(60.0, 80.0) };
    assert_eq!(toolbar_origin(narrow, size).x, 100.0);
}

fn draw_message(cx: &mut Cx, message: &mut super::super::Message, size: DVec2) {
    use makepad_widgets::makepad_draw::cx_draw::CxDraw;
    cx.redraw_all();
    let pass = DrawPass::new(cx);
    pass.set_size(cx, size);
    let mut list = DrawList2d::new(cx);
    let event = DrawEvent::default();
    let mut draw = CxDraw::new(cx, &event);
    let mut cx2d = Cx2d::new(&mut draw);
    cx2d.begin_pass(&pass, None);
    list.begin_always(&mut cx2d);
    cx2d.begin_root_turtle(size, Layout::flow_overlay());
    let walk = Walk { width: Size::Fixed(size.x), height: Size::Fit { min: None, max: None }, ..Walk::default() };
    let _ = message.draw_walk(&mut cx2d, &mut Scope::empty(), walk);
    cx2d.end_pass_sized_turtle();
    list.end(&mut cx2d);
    cx2d.end_pass(&pass);
}

fn mouse_move(abs: DVec2) -> Event {
    Event::MouseMove(MouseMoveEvent {
        abs,
        lock_delta: DVec2::default(),
        window_id: WindowId(0, 0),
        modifiers: KeyModifiers::default(),
        time: 0.0,
        handled: Default::default(),
    })
}

/// The user flow: the mouse enters a message, the toolbar floats at its top-right,
/// and it disappears when the mouse leaves.
#[test]
fn hovering_a_message_shows_the_toolbar_at_its_top_right_and_leaving_hides_it() {
    use super::super::Message;
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut message = cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.Message {}});
        let message = Message::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty());
        message
    });
    let size = dvec2(900.0, 600.0);
    draw_message(&mut cx, &mut message, size);
    let message_rect = message.view.area().rect(&cx);
    assert!(!message.is_toolbar_shown);

    let inside = message_rect.pos + dvec2(40.0, 10.0);
    message.handle_event(&mut cx, &mouse_move(inside), &mut Scope::empty());
    assert!(message.is_toolbar_shown, "hovering must show the toolbar");
    draw_message(&mut cx, &mut message, size);

    // With no message details, only "…" applies: one button, inside the top-right corner.
    let toolbar = message.hover_toolbar.as_message_hover_toolbar();
    let drawn = toolbar.borrow().unwrap().view.area().rect(&cx);
    let expected = toolbar_size(1);
    assert_eq!(drawn.size, expected, "the DSL sizes must match toolbar_size()");
    assert_eq!(drawn.pos, toolbar_origin(message_rect, expected));
    assert!(toolbar.contains(&cx, drawn.pos + dvec2(1.0, 1.0)));

    let outside = message_rect.pos + message_rect.size + dvec2(10.0, 10.0);
    message.handle_event(&mut cx, &mouse_move(outside), &mut Scope::empty());
    assert!(!message.is_toolbar_shown, "leaving must hide the toolbar");
    cx.with_vm(|vm| assert!(vm.take_errors().is_empty()));
}
