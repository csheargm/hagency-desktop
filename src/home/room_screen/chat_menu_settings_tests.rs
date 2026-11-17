//! The Chats menu's Settings in a room that is not an Agent's private chat.
use super::*;
use super::desktop_information_columns_tests::{draw, screen};
use crate::home::chat_menu_routes::{TEST_REQUEST_LOCK, take_room_info_request, tests::{details, menu}};

/// The user flow: right-click a room -> Settings -> the room is shown with its
/// information pane open on Overview, the panel the header's button opens.
#[test]
fn settings_in_a_room_shows_its_information_pane_on_overview() {
    let _serial = TEST_REQUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let room = matrix_sdk::ruma::room_id!("!team:test").to_owned();
    let (mut cx, mut screen) = screen();
    screen.room_name_id = Some(RoomNameId::empty(room.clone()));
    screen.timeline_kind = Some(TimelineKind::MainRoom { room_id: room.clone() });
    draw(&mut cx, &mut screen, 1280.0);
    assert!(!screen.view.view(&mut cx, ids!(desktop_chat_info_panel)).visible(), "closed before Settings");
    screen.view.mobile_chat_info(&mut cx, ids!(desktop_chat_info)).select_members_for_test(&mut cx);

    let mut menu = menu(&mut cx);
    menu.show_with_agent(&mut cx, details(&room), None);
    assert!(menu.button(&cx, ids!(invite_button)).visible(), "Invite stays in an ordinary room");
    let emitted = cx.capture_actions(|cx| menu.open_settings(cx));
    assert!(emitted.iter().any(|a| matches!(a.downcast_ref(), Some(crate::app::AppStateAction::NavigateToRoom { destination_room, .. }) if destination_room.room_id() == &room)), "the room is shown");
    assert!(!emitted.iter().any(|a| a.downcast_ref::<crate::hagency::ui::HagencyAction>().is_some()));
    assert!(!menu.is_currently_shown(&mut cx));

    draw(&mut cx, &mut screen, 1280.0);
    assert!(screen.view.view(&mut cx, ids!(desktop_chat_info_panel)).visible(), "the information pane opens");
    assert!(screen.view.mobile_chat_info(&mut cx, ids!(desktop_chat_info)).shows_overview_for_test(), "on its Overview tab");
    assert!(!take_room_info_request(&room), "the request is used once");
}

/// A thread of the room does not take the room's request.
#[test]
fn a_thread_view_leaves_the_rooms_request_alone() {
    let _serial = TEST_REQUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let room = matrix_sdk::ruma::room_id!("!threaded:test").to_owned();
    let (mut cx, mut screen) = screen();
    screen.room_name_id = Some(RoomNameId::empty(room.clone()));
    screen.timeline_kind = Some(TimelineKind::Thread { room_id: room.clone(), thread_root_event_id: matrix_sdk::ruma::event_id!("$root:test").to_owned() });
    let mut menu = menu(&mut cx);
    menu.show_with_agent(&mut cx, details(&room), None);
    let _ = cx.capture_actions(|cx| menu.open_settings(cx));
    draw(&mut cx, &mut screen, 1280.0);
    assert!(!screen.chat_info_requested);
    assert!(take_room_info_request(&room), "still waiting for the room itself");
}
