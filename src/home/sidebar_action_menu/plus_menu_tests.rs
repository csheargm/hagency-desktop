//! The "+" menu: which rows it lists, with and without an open room, and that
//! each row runs the action of its existing entry point.
use super::*;
use crate::home::sidebar_action_menu::new_or_join_tests::{menu, visible_rows, PLUS, WINDOW};
use crate::home::sidebar_action_menu::{new_or_join_menu_pos, SidebarMenuKind};
use crate::i18n::{text_for, Language};
use crate::shared::context_menu::ContextMenuClosed;
use crate::theme::Appearance;
use matrix_sdk::ruma::{event_id, room_id, OwnedRoomId};

fn team_room() -> RoomNameId {
    RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Team".into()), room_id!("!team:server").to_owned())
}

/// Delivers `action` to [`observe`] as the app's event loop would.
fn deliver(cx: &mut Cx, action: impl ActionTrait + 'static) {
    let actions = cx.capture_actions(|cx| cx.action(action));
    observe(&actions);
}

fn rows(room_open: bool) -> Vec<LiveId> {
    items(room_open).into_iter().map(PlusItem::row).collect()
}

/// What one emitted action is, as the row it should come from.
fn command(action: &Action) -> Option<(PlusItem, Option<OwnedRoomId>)> {
    use crate::home::navigation_tab_bar::NavigationBarAction;
    match action.downcast_ref() {
        Some(NavigationBarAction::NewChat) => return Some((PlusItem::NewChat, None)),
        Some(NavigationBarAction::GoToAddRoom) => return Some((PlusItem::JoinRoom, None)),
        _ => {}
    }
    if matches!(action.downcast_ref(), Some(crate::home::new_room_dialog::NewRoomAction::Open)) {
        return Some((PlusItem::NewRoom, None));
    }
    if matches!(action.downcast_ref(), Some(crate::home::space_management::SpaceManagementAction::Open { parent: None })) {
        return Some((PlusItem::NewSpace, None));
    }
    if matches!(action.downcast_ref(), Some(crate::hagency::create_agent::CreateAgentAction::Open)) {
        return Some((PlusItem::CreateAgent, None));
    }
    if let Some(crate::home::invite_modal::InviteModalAction::Open(room)) = action.downcast_ref() {
        return Some((PlusItem::AddAgentToRoom, Some(room.room_id().clone())));
    }
    None
}

#[test]
fn rows_are_the_five_creation_entries_plus_add_agent_only_with_an_open_room() {
    use PlusItem::*;
    assert_eq!(items(false), vec![NewChat, NewRoom, NewSpace, JoinRoom, CreateAgent]);
    assert_eq!(items(true), vec![NewChat, NewRoom, NewSpace, JoinRoom, CreateAgent, AddAgentToRoom]);
    let ids: std::collections::HashSet<LiveId> = PlusItem::ALL.into_iter().map(PlusItem::row).collect();
    assert_eq!(ids.len(), PlusItem::ALL.len(), "each row has its own widget");
}

#[test]
fn only_a_joined_room_or_thread_is_a_room_an_agent_can_join() {
    let room = team_room();
    let joined = SelectedRoom::JoinedRoom { room_name_id: room.clone() };
    let thread = SelectedRoom::Thread { room_name_id: room.clone(), thread_root_event_id: event_id!("$root:server").to_owned() };
    assert_eq!(agent_room(&joined).map(|r| r.room_id().clone()), Some(room.room_id().clone()));
    assert_eq!(agent_room(&thread).map(|r| r.room_id().clone()), Some(room.room_id().clone()));
    assert!(agent_room(&SelectedRoom::InvitedRoom { room_name_id: room.clone() }).is_none());
    assert!(agent_room(&SelectedRoom::Space { space_name_id: room }).is_none());
}

#[test]
fn the_open_room_follows_focus_and_is_forgotten_on_sign_out() {
    let mut cx = Cx::new(Box::new(|_, _| {}));
    set_focused(None);
    deliver(&mut cx, AppStateAction::RoomFocused(SelectedRoom::JoinedRoom { room_name_id: team_room() }));
    assert_eq!(open_room().map(|r| r.room_id().clone()), Some(team_room().room_id().clone()));
    deliver(&mut cx, AppStateAction::FocusNone);
    assert!(open_room().is_none());
    deliver(&mut cx, AppStateAction::RoomFocused(SelectedRoom::JoinedRoom { room_name_id: team_room() }));
    deliver(&mut cx, AppStateAction::RoomFocused(SelectedRoom::Space { space_name_id: team_room() }));
    assert!(open_room().is_none(), "a Space takes no Agent from this menu");
    deliver(&mut cx, AppStateAction::RoomFocused(SelectedRoom::JoinedRoom { room_name_id: team_room() }));
    deliver(&mut cx, LogoutAction::ClearAppState { on_clear_appstate: std::sync::Arc::new(tokio::sync::Notify::new()) });
    assert!(open_room().is_none(), "sign-out forgets the room");
}

/// Regression (Ben, 2026-11-14): "fix-it" was opened as an invitation and
/// joined in place, and in the single-pane layout a room comes from the rooms
/// list, not dock focus. Both must make "Add agent to this room" appear.
#[test]
fn an_accepted_invitation_and_a_rooms_list_selection_open_the_room() {
    use crate::home::rooms_list::{AcceptedInviteKind, RoomsListAction};
    let mut cx = Cx::new(Box::new(|_, _| {}));
    set_focused(None);
    deliver(&mut cx, AppStateAction::RoomFocused(SelectedRoom::InvitedRoom { room_name_id: team_room() }));
    assert!(open_room().is_none(), "an invitation takes no Agent yet");
    deliver(&mut cx, AppStateAction::UpgradedInviteToJoinedRoom { room_id: team_room().room_id().clone(), is_space: false });
    assert_eq!(open_room().map(|r| r.room_id().clone()), Some(team_room().room_id().clone()));
    assert_eq!(items(open_room().is_some()).last(), Some(&PlusItem::AddAgentToRoom));

    set_focused(Some(SelectedRoom::InvitedRoom { room_name_id: team_room() }));
    let accepted = cx.capture_actions(|cx| cx.widget_action(WidgetUid(7), RoomsListAction::InviteAccepted { room_name_id: team_room(), kind: AcceptedInviteKind::Room }));
    observe(&accepted);
    assert!(open_room().is_some(), "an invitation accepted from the list opens the room");

    set_focused(None);
    let selected = cx.capture_actions(|cx| cx.widget_action(WidgetUid(7), RoomsListAction::Selected(SelectedRoom::JoinedRoom { room_name_id: team_room() })));
    observe(&selected);
    assert!(open_room().is_some(), "a rooms-list selection opens the room");
}

/// Opens the "+" menu, clicks `item` and returns what it emitted.
fn click(cx: &mut Cx, menu: &mut super::super::SidebarActionMenu, item: PlusItem) -> Vec<Action> {
    let anchor = new_or_join_menu_pos(cx, PLUS);
    menu.show(cx, SidebarMenuKind::Add, anchor, Rect { pos: DVec2::default(), size: WINDOW });
    let uid = menu.view.button(cx, &[item.row()]).widget_uid();
    let clicked = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default())));
    let emitted = cx.capture_actions(|cx| menu.handle_event(cx, &Event::Actions(clicked), &mut Scope::empty()));
    assert!(!menu.visible, "{item:?} closes the menu");
    assert!(emitted.iter().any(|action| action.downcast_ref::<ContextMenuClosed>().is_some()));
    emitted.into_iter().collect()
}

#[test]
fn the_menu_lists_its_rows_and_each_runs_its_existing_action() {
    let (mut cx, mut menu) = menu(Appearance::Light, 1.0);
    let window = Rect { pos: DVec2::default(), size: WINDOW };
    for room_open in [false, true] {
        set_focused(room_open.then(|| SelectedRoom::JoinedRoom { room_name_id: team_room() }));
        let anchor = new_or_join_menu_pos(&mut cx, PLUS);
        menu.show(&mut cx, SidebarMenuKind::Add, anchor, window);
        assert_eq!(visible_rows(&mut cx, &menu), rows(room_open), "room open: {room_open}");
        for item in items(room_open) {
            let emitted = click(&mut cx, &mut menu, item);
            let commands: Vec<_> = emitted.iter().filter_map(command).collect();
            let room = (item == PlusItem::AddAgentToRoom).then(|| team_room().room_id().clone());
            assert_eq!(commands, vec![(item, room)], "{item:?} runs one command, its own");
        }
    }
    // The room is the one open when the menu opened, even if focus moves on.
    set_focused(Some(SelectedRoom::JoinedRoom { room_name_id: team_room() }));
    let anchor = new_or_join_menu_pos(&mut cx, PLUS);
    menu.show(&mut cx, SidebarMenuKind::Add, anchor, window);
    deliver(&mut cx, AppStateAction::FocusNone);
    let uid = menu.view.button(&mut cx, &[PlusItem::AddAgentToRoom.row()]).widget_uid();
    let clicked = cx.capture_actions(|cx| cx.widget_action(uid, ButtonAction::Clicked(KeyModifiers::default())));
    let emitted = cx.capture_actions(|cx| menu.handle_event(cx, &Event::Actions(clicked), &mut Scope::empty()));
    assert_eq!(emitted.iter().filter_map(command).collect::<Vec<_>>(), vec![(PlusItem::AddAgentToRoom, Some(team_room().room_id().clone()))]);

    // The other sidebar menus never show a "+" row; Escape runs nothing.
    for kind in [SidebarMenuKind::Projects, SidebarMenuKind::Chats, SidebarMenuKind::Agents, SidebarMenuKind::Tools] {
        menu.show(&mut cx, kind, dvec2(200.0, 100.0), window);
        let shown = visible_rows(&mut cx, &menu);
        assert!(!shown.is_empty() && PlusItem::ALL.iter().all(|item| !shown.contains(&item.row())), "{kind:?}: {shown:?}");
    }
    set_focused(Some(SelectedRoom::JoinedRoom { room_name_id: team_room() }));
    let anchor = new_or_join_menu_pos(&mut cx, PLUS);
    menu.show(&mut cx, SidebarMenuKind::Add, anchor, window);
    let escape = Event::KeyDown(KeyEvent { key_code: KeyCode::Escape, ..Default::default() });
    let emitted = cx.capture_actions(|cx| menu.handle_event(cx, &escape, &mut Scope::empty()));
    assert!(!menu.visible && emitted.iter().filter_map(command).next().is_none());
}

#[test]
fn rows_are_labelled_and_the_existing_labels_are_translated() {
    let (mut cx, mut menu) = menu(Appearance::Light, 1.0);
    set_focused(Some(SelectedRoom::JoinedRoom { room_name_id: team_room() }));
    let anchor = new_or_join_menu_pos(&mut cx, PLUS);
    menu.show(&mut cx, SidebarMenuKind::Add, anchor, Rect { pos: DVec2::default(), size: WINDOW });
    for (item, english, chinese) in [
        (PlusItem::NewChat, "New chat", Some("新聊天")),
        (PlusItem::NewRoom, "New room", Some("新建聊天室")),
        (PlusItem::NewSpace, "New space", Some("新建空间")),
        (PlusItem::JoinRoom, "Join or explore rooms", Some("加入或探索聊天室")),
        (PlusItem::CreateAgent, "Create agent", Some("创建 Agent")),
        // New string: its catalog entry is added separately.
        (PlusItem::AddAgentToRoom, "Add agent to this room", None),
    ] {
        assert_eq!(menu.view.button(&mut cx, &[item.row()]).text(), english);
        if let Some(chinese) = chinese {
            assert_eq!(text_for(Language::Chinese, english), chinese);
        }
    }
}
