//! Cross-feature UI checks for the compatible Robrix ports.
use makepad_widgets::{makepad_draw::cx_draw::CxDraw, *};
use crate::home::{
    new_message_context_menu::{MessageAbilities, MessageDetails},
    reaction_picker::ReactionPickerWidgetRefExt,
};
use crate::moderation_modal::{ModerationModalWidgetRefExt, ModerationRequest};

fn fixture() -> (Cx, WidgetRef) {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let ui = cx.with_vm(|vm| {
        crate::theme::tests::install(vm, Default::default());
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {use mod.prelude.widgets.*
            mod.widgets.View {
            width: Fill height: Fill flow: Overlay
            picker := mod.widgets.Modal {can_dismiss: false content := mod.widgets.ReactionPicker {}}
            moderation := mod.widgets.Modal {can_dismiss: false content := mod.widgets.ModerationModal {}}
        }});
        let ui = WidgetRef::script_from_value(vm, value);
        ui.register_as_ui_root(vm);
        let errors = vm.take_errors(); assert!(errors.is_empty(), "script errors: {errors:?}");
        ui
    });
    (cx, ui)
}

fn draw(cx: &mut Cx, ui: &WidgetRef, size: DVec2) {
    cx.redraw_all();
    let pass = DrawPass::new(cx);
    pass.set_size(cx, size);
    let mut list = DrawList2d::new(cx);
    let mut overlay = cx.with_vm(|vm| Overlay::script_new(vm));
    let event = DrawEvent::default();
    let mut draw = CxDraw::new(cx, &event);
    let mut cx2d = Cx2d::new(&mut draw);
    cx2d.begin_pass(&pass, None);
    list.begin_always(&mut cx2d);
    overlay.begin(&mut cx2d);
    cx2d.begin_root_turtle(size, Layout::flow_down());
    ui.draw_walk_all(&mut cx2d, &mut Scope::empty(), Walk::fixed(size.x, size.y));
    cx2d.end_pass_sized_turtle();
    overlay.end(&mut cx2d);
    list.end(&mut cx2d);
    cx2d.end_pass(&pass);
}

#[test]
fn picker_and_moderation_modals_fit_small_windows_and_large_desktops() {
    let (mut cx, ui) = fixture();
    let details = MessageDetails {
        item_id: 0,
        timeline_event_id: matrix_sdk_ui::timeline::TimelineEventItemId::EventId(
            ruma::event_id!("$event").to_owned(),
        ),
        related_event_id: None,
        thread_root_event_id: None,
        room_screen_widget_uid: ui.widget_uid(),
        should_be_highlighted: false,
        abilities: MessageAbilities::CanReact,
        selected_text: Some("keep selection".into()),
    };
    ui.reaction_picker(&mut cx, ids!(picker.content))
        .show(&mut cx, details, &[]);
    ui.modal(&mut cx, ids!(picker)).open(&mut cx);
    for size in [dvec2(360.0, 480.0), dvec2(1400.0, 900.0)] {
        draw(&mut cx, &ui, size);
        let rect = ui.widget(&mut cx, ids!(picker.content)).area().rect(&cx);
        assert!(
            rect.size.x > 200.0 && rect.size.x <= 420.5 && rect.size.y <= 460.5,
            "{rect:?}"
        );
        assert!(
            rect.pos.x >= -0.5
                && rect.pos.y >= -0.5
                && rect.pos.x + rect.size.x <= size.x + 0.5
                && rect.pos.y + rect.size.y <= size.y + 0.5,
            "{rect:?}"
        );
    }
    ui.modal(&mut cx, ids!(picker)).close(&mut cx);
    ui.moderation_modal(&mut cx, ids!(moderation.content))
        .set_info(
            &mut cx,
            ModerationRequest::Report(crate::moderation::ReportTarget::User {
                user_id: ruma::user_id!("@report:test").to_owned(),
                user_name: "Member with a long display name ".repeat(8),
            }),
        );
    ui.modal(&mut cx, ids!(moderation)).open(&mut cx);
    draw(&mut cx, &ui, dvec2(360.0, 480.0));
    let rect = ui
        .widget(&mut cx, ids!(moderation.content))
        .area()
        .rect(&cx);
    assert!(
        rect.size.x > 200.0 && rect.size.x <= 360.0 && rect.size.y <= 480.0,
        "{rect:?}"
    );
    cx.with_vm(|vm| {
        let errors = vm.take_errors();
        assert!(errors.is_empty(), "draw errors: {errors:?}");
    });
}
