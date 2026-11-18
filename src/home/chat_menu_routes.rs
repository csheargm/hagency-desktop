//! Where the Chats list menu's Settings leads, and the wording its Leave and
//! Delete confirmations use in an Agent's private chat.
//!
//! An Agent's private chat is a DM whose only other member is one of this
//! account's own Agents (see `agent_ask_badge::dm_own_agent`).
use std::{sync::Mutex, time::{Duration, Instant}};
use makepad_widgets::Cx;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, RoomId, UserId};

/// An Agent's private chat has the owner and the Agent only.
const PRIVATE_CHAT_MEMBERS: u64 = 2;
use crate::{app::AppStateAction, hagency::ui::HagencyAction, room::BasicRoomDetails, utils::RoomNameId};

/// How long a Settings request waits for its room to be shown. A room that is
/// not shown by then (an invite, or the mobile layout) drops the request, so a
/// later visit never pops the information pane open unasked.
const ROOM_INFO_REQUEST_TTL: Duration = Duration::from_secs(10);
/// The room, the account that asked, and when.
static ROOM_INFO_REQUEST: Mutex<Option<(OwnedRoomId, Option<OwnedUserId>, Instant)>> = Mutex::new(None);
/// Serializes the tests that use the one pending request.
#[cfg(test)]
pub(crate) static TEST_REQUEST_LOCK: Mutex<()> = Mutex::new(());

/// The owner's Agent whose private chat `room` is, as its Agent ID.
pub(crate) fn own_agent_dm(room: &RoomId) -> Option<String> {
    let room = crate::sliding_sync::get_client()?.get_room(room)?;
    // DM metadata outlives a third member joining; such a room is a group now.
    if room.joined_members_count() > PRIVATE_CHAT_MEMBERS { return None; }
    let targets: Vec<OwnedUserId> = room.direct_targets().iter()
        .filter_map(|t| UserId::parse(t.as_str()).ok()).collect();
    match super::agent_ask_badge::dm_own_agent(crate::sliding_sync::current_user_id().as_ref(), &targets)? {
        HagencyAction::OpenAgent { agent_id } => Some(agent_id),
        _ => None,
    }
}

/// The Chats menu's Settings: an Agent's private chat opens that Agent's
/// Settings tab; any other room opens and shows its information pane.
pub(crate) fn open_settings(cx: &mut Cx, room: &RoomNameId, agent_id: Option<&str>) {
    let mut request = ROOM_INFO_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(agent_id) = agent_id {
        *request = None;
        cx.action(HagencyAction::OpenAgentSettings { agent_id: agent_id.to_owned() });
        return;
    }
    *request = Some((room.room_id().clone(), crate::sliding_sync::current_user_id(), Instant::now()));
    drop(request);
    cx.action(AppStateAction::NavigateToRoom {
        room_to_close: None,
        destination_room: BasicRoomDetails::Name(room.clone()),
    });
}

/// Whether Settings asked to show `room`'s information pane; answers `true` once.
pub(crate) fn take_room_info_request(room: &RoomId) -> bool {
    let mut request = ROOM_INFO_REQUEST.lock().unwrap_or_else(|e| e.into_inner());
    let account = crate::sliding_sync::current_user_id();
    if request.as_ref().is_some_and(|(_, owner, asked)| *owner != account || asked.elapsed() > ROOM_INFO_REQUEST_TTL) {
        *request = None;
    }
    if request.as_ref().is_some_and(|(id, _, _)| id == room) {
        *request = None;
        return true;
    }
    false
}

/// Delete Chat in an Agent's private chat: it only leaves this device's list,
/// so the Agent's Message button opens it again.
pub(crate) fn agent_chat_delete_body() -> &'static str {
    crate::i18n::tr(AGENT_CHAT_DELETE_BODY)
}
const AGENT_CHAT_DELETE_BODY: &str = "Remove your private chat with this Agent from this device and hide its existing history here? You stay in the chat, so the Message button in the Agent's details opens it again; earlier messages stay hidden on this device. Messages remain on the server and other devices.";
const AGENT_CHAT_LEAVE_TITLE: &str = "Leave your private chat with this Agent?";
const AGENT_CHAT_LEAVE_BODY: &str = "Leaving \"{room}\" ends your private chat with this Agent for now. The Agent's Message button reopens it: the Agent invites you back, or starts a new private chat if this one can no longer be used.\n\nTo only remove it from your Chats list, use Delete Chat instead: Message brings it back.";

/// Leave in an Agent's private chat: the room is invite-only, so the Agent's
/// Message button reopens it by having the Agent invite the owner back, or by
/// starting a replacement chat (server ADR 0003).
pub(crate) fn agent_chat_leave_text(room: &str) -> (&'static str, String) {
    (AGENT_CHAT_LEAVE_TITLE, crate::i18n::format(AGENT_CHAT_LEAVE_BODY, &[("room", room.to_owned())]))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::home::room_context_menu::{RoomContextMenu, RoomContextMenuDetails};
    use makepad_widgets::*;

    pub(crate) fn menu(cx: &mut Cx) -> RoomContextMenu {
        cx.with_vm(|vm| {
            makepad_widgets::script_mod(vm);
            crate::app::register_widgets(vm);
            let value = script_eval!(vm, {mod.widgets.RoomContextMenu {}});
            RoomContextMenu::script_from_value(vm, value)
        })
    }
    pub(crate) fn details(room: &OwnedRoomId) -> RoomContextMenuDetails {
        RoomContextMenuDetails {
            room_name_id: RoomNameId::empty(room.clone()), can_invite: true,
            notification_mode: None, is_favorite: false, is_low_priority: false, has_unreads: false,
        }
    }

    /// The user flow in an Agent's private chat: right-click -> no Invite;
    /// Settings -> that Agent's panel on its Settings tab, never the old popup.
    #[test]
    fn settings_in_an_agent_chat_opens_the_agents_settings_and_hides_invite() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut menu = menu(&mut cx);
        let room = matrix_sdk::ruma::room_id!("!dm:test").to_owned();
        menu.show_with_agent(&mut cx, details(&room), Some("agt_1".into()));
        assert!(!menu.button(&cx, ids!(invite_button)).visible(), "Invite is hidden in an Agent's private chat");
        assert!(menu.button(&cx, ids!(agent_details_button)).visible());
        let _serial = TEST_REQUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        *ROOM_INFO_REQUEST.lock().unwrap() = Some((matrix_sdk::ruma::room_id!("!earlier:test").to_owned(), None, Instant::now()));
        let emitted = cx.capture_actions(|cx| menu.open_settings(cx));
        assert!(ROOM_INFO_REQUEST.lock().unwrap().is_none(), "an earlier room's request is dropped");
        assert!(emitted.iter().any(|a| matches!(a.downcast_ref(), Some(HagencyAction::OpenAgentSettings { agent_id }) if agent_id == "agt_1")));
        assert!(!emitted.iter().any(|a| a.downcast_ref::<AppStateAction>().is_some()), "the chat itself stays as it is");
        assert!(!take_room_info_request(&room), "no information pane is requested");
        assert!(!menu.is_currently_shown(&mut cx), "the menu closes");
    }

    #[test]
    fn a_room_info_request_is_for_one_room_and_expires() {
        let _serial = TEST_REQUEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (a, b) = (matrix_sdk::ruma::room_id!("!a:test").to_owned(), matrix_sdk::ruma::room_id!("!b:test").to_owned());
        *ROOM_INFO_REQUEST.lock().unwrap() = Some((a.clone(), None, Instant::now()));
        assert!(!take_room_info_request(&b), "another room never takes it");
        assert!(take_room_info_request(&a));
        let Some(stale) = Instant::now().checked_sub(ROOM_INFO_REQUEST_TTL * 2) else { return };
        *ROOM_INFO_REQUEST.lock().unwrap() = Some((a.clone(), None, stale));
        assert!(!take_room_info_request(&a), "a request the room never used expires");
        let other = Some(matrix_sdk::ruma::user_id!("@other:test").to_owned());
        *ROOM_INFO_REQUEST.lock().unwrap() = Some((a.clone(), other, Instant::now()));
        assert!(!take_room_info_request(&a), "another account never takes it");
    }

    #[test]
    fn agent_chat_confirmations_say_what_leaving_and_deleting_do() {
        let (title, body) = agent_chat_leave_text("yue-claude");
        assert!(title.contains("private chat"));
        assert!(body.contains("\"yue-claude\"") && body.contains("ends your private chat") && body.contains("Message button reopens it") && body.contains("invites you back") && !body.contains("will not reopen"));
        assert!(agent_chat_delete_body().contains("You stay in the chat") && agent_chat_delete_body().contains("Message button") && agent_chat_delete_body().contains("stay hidden"));
        for key in [AGENT_CHAT_LEAVE_TITLE, AGENT_CHAT_DELETE_BODY, AGENT_CHAT_LEAVE_BODY] {
            assert_ne!(crate::i18n::text_for(crate::i18n::Language::Chinese, key), key, "{key} has a Chinese translation");
        }
    }
}
