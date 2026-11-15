//! The chat list's one "+" menu (UX item A8), drawn by
//! [`super::SidebarActionMenu`] as [`super::SidebarMenuKind::Add`].
//!
//! Rows, top to bottom: New chat, New room, New space, Join a room, Create agent and,
//! only while a room is open, Add agent to this room. Every row emits the
//! action its existing entry point already emits, so this file adds no flow
//! of its own. "Add agent to this room" opens the room's invite picker, which
//! lists the owner's Agents first; picking one binds it through
//! `Service::invite_agent_to_room`, and that registers any joined room in the
//! owner's Agent space first.
use std::cell::RefCell;

use makepad_widgets::*;

use crate::app::{AppStateAction, SelectedRoom};
use crate::logout::logout_confirm_modal::LogoutAction;
use crate::utils::RoomNameId;

/// One row of the "+" menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlusItem {
    NewChat,
    NewRoom,
    NewSpace,
    JoinRoom,
    CreateAgent,
    AddAgentToRoom,
}

impl PlusItem {
    /// Every row the menu can show, top to bottom.
    pub const ALL: [PlusItem; 6] = [
        PlusItem::NewChat,
        PlusItem::NewRoom,
        PlusItem::NewSpace,
        PlusItem::JoinRoom,
        PlusItem::CreateAgent,
        PlusItem::AddAgentToRoom,
    ];

    /// The row's widget id in the menu's DSL.
    pub fn row(self) -> LiveId {
        match self {
            PlusItem::NewChat => id!(plus_new_chat),
            PlusItem::NewRoom => id!(plus_new_room),
            PlusItem::NewSpace => id!(plus_new_space),
            PlusItem::JoinRoom => id!(plus_join_room),
            PlusItem::CreateAgent => id!(plus_create_agent),
            PlusItem::AddAgentToRoom => id!(plus_add_agent),
        }
    }
}

/// The rows shown, top to bottom: "Add agent to this room" needs an open room.
pub fn items(room_open: bool) -> Vec<PlusItem> {
    PlusItem::ALL
        .into_iter()
        .filter(|item| room_open || *item != PlusItem::AddAgentToRoom)
        .collect()
}

thread_local! {
    /// The room, invitation or Space the app shows now. Kept whole so an
    /// accepted invitation turns into a joined room the menu can act on.
    static FOCUSED: RefCell<Option<SelectedRoom>> = const { RefCell::new(None) };
}

/// The open room "Add agent to this room" would act on, if any.
pub fn open_room() -> Option<RoomNameId> {
    FOCUSED.with(|room| room.borrow().as_ref().and_then(agent_room))
}

fn set_focused(room: Option<SelectedRoom>) {
    FOCUSED.with(|open| *open.borrow_mut() = room);
}

/// The room an Agent can join from the menu: a joined room, or the room of an
/// open thread. An invitation or a Space is not one.
pub fn agent_room(selected: &SelectedRoom) -> Option<RoomNameId> {
    match selected {
        SelectedRoom::JoinedRoom { room_name_id } | SelectedRoom::Thread { room_name_id, .. } => {
            Some(room_name_id.clone())
        }
        SelectedRoom::InvitedRoom { .. } | SelectedRoom::Space { .. } => None,
    }
}

/// Follows the room the app shows: the desktop dock's focus, a rooms-list
/// selection (the single-pane layout emits no dock focus), an invitation
/// accepted in place (the dock keeps its tab, so no new focus comes), and
/// sign-out.
pub fn observe(actions: &Actions) {
    use crate::home::rooms_list::RoomsListAction;
    for action in actions {
        match action.downcast_ref() {
            Some(AppStateAction::RoomFocused(selected)) => set_focused(Some(selected.clone())),
            Some(AppStateAction::FocusNone) => set_focused(None),
            Some(AppStateAction::UpgradedInviteToJoinedRoom { room_id, is_space }) => FOCUSED.with(|open| {
                if let Some(room) = open.borrow_mut().as_mut() { room.upgrade_invite_to_joined(room_id, *is_space); }
            }),
            _ => {}
        }
        match action.as_widget_action().cast() {
            RoomsListAction::Selected(selected) => set_focused(Some(selected)),
            RoomsListAction::InviteAccepted { room_name_id, kind } => FOCUSED.with(|open| {
                if let Some(room) = open.borrow_mut().as_mut() { room.upgrade_invite_to_joined(room_name_id.room_id(), kind.is_space()); }
            }),
            _ => {}
        }
        if matches!(action.downcast_ref(), Some(LogoutAction::ClearAppState { .. })) {
            set_focused(None);
        }
    }
}

/// Runs `item` exactly as its existing entry point does. `room` is the room
/// that was open when the menu opened.
pub fn run(cx: &mut Cx, item: PlusItem, room: Option<&RoomNameId>) {
    use crate::home::navigation_tab_bar::NavigationBarAction;
    match item {
        // The welcome screen's and the old Chats menu's "New chat".
        PlusItem::NewChat => cx.action(NavigationBarAction::NewChat),
        // One screen, no parent space needed (`new_room_dialog`).
        PlusItem::NewRoom => cx.action(crate::home::new_room_dialog::NewRoomAction::Open),
        // The Explore Rooms page's "Create a space or room…" wizard.
        PlusItem::NewSpace => {
            cx.action(crate::home::space_management::SpaceManagementAction::Open { parent: None })
        }
        // The Explore Rooms page: join by name, alias, ID or link.
        PlusItem::JoinRoom => cx.action(NavigationBarAction::GoToAddRoom),
        PlusItem::CreateAgent => cx.action(crate::hagency::create_agent::CreateAgentAction::Open),
        // The room's invite picker, as Members → Invite and /invite open it.
        PlusItem::AddAgentToRoom => {
            if let Some(room) = room {
                cx.action(crate::home::invite_modal::InviteModalAction::Open(room.clone()));
            }
        }
    }
}

#[cfg(test)]
#[path = "plus_menu_tests.rs"]
mod tests;
