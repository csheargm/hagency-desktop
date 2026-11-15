//! "+" → New room: one screen with no space requirement, encryption off by
//! default, and Create enabled by a name alone.
use super::*;

fn dialog(cx: &mut Cx) -> NewRoomDialog {
    cx.with_vm(|vm| {
        makepad_widgets::script_mod(vm);
        crate::app::register_widgets(vm);
        vm.bx.captured_errors = Some(Vec::new());
        let value = script_eval!(vm, {mod.widgets.NewRoomDialog {}});
        let dialog = NewRoomDialog::script_from_value(vm, value);
        assert!(vm.take_errors().is_empty(), "the dialog's DSL evaluates without errors");
        dialog
    })
}

#[test]
fn a_new_room_is_a_plain_unencrypted_room_without_a_parent_space() {
    let config = new_room_config("  Design review ", " Weekly ", false, false);
    assert_eq!((config.name.as_str(), config.topic.as_str()), ("Design review", "Weekly"));
    assert!(!config.space && config.parent.is_none() && !config.encrypted && !config.public);
    let request = config.request().expect("no space is needed to create a room");
    assert!(request.initial_state.is_empty(), "no encryption state unless asked for");
    assert!(request.creation_content.is_none(), "a room, not a space");
}

#[test]
fn encryption_is_opt_in_and_never_combined_with_public() {
    let encrypted = new_room_config("Secrets", "", false, true);
    assert!(encrypted.encrypted && encrypted.request().unwrap().initial_state.len() == 1);
    let public = new_room_config("Lobby", "", true, true);
    assert!(public.public && !public.encrypted, "public wins over the encrypted preset");
    assert!(public.request().is_ok());
    assert!(new_room_config("   ", "", false, false).request().is_err(), "a name is required");
}

#[test]
fn the_form_opens_blank_with_both_toggles_off_and_create_waits_for_a_name() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut dialog = dialog(&mut cx);
    dialog.view.check_box(&mut cx, ids!(encrypted)).set_active(&mut cx, true, Animate::No);
    dialog.view.text_input(&mut cx, ids!(room_name)).set_text(&mut cx, "Old");
    dialog.open(&mut cx);
    assert!(!dialog.view.check_box(&mut cx, ids!(encrypted)).active(&cx), "encryption is off by default");
    assert!(!dialog.view.check_box(&mut cx, ids!(public)).active(&cx));
    assert!(!dialog.view.button(&mut cx, ids!(create)).borrow().is_some_and(|b| b.enabled()), "no name yet");
    dialog.view.text_input(&mut cx, ids!(room_name)).set_text(&mut cx, "Team");
    dialog.render_create(&mut cx);
    assert!(dialog.view.button(&mut cx, ids!(create)).borrow().is_some_and(|b| b.enabled()));
    let config = dialog.config(&mut cx);
    assert!(!config.space && config.parent.is_none() && !config.encrypted);
}

#[test]
fn a_failed_creation_keeps_the_form_open_with_the_reason() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    let mut dialog = dialog(&mut cx);
    dialog.open(&mut cx);
    let emitted = cx.capture_actions(|cx| dialog.finish(cx, &Err("M_FORBIDDEN".into())));
    assert!(emitted.iter().all(|a| a.downcast_ref::<NewRoomAction>().is_none()), "stays open");
    assert!(dialog.view.label(&mut cx, ids!(status)).text().ends_with("M_FORBIDDEN"));
}
