//! What the hover card's buttons do: "View profile" uses the same routing
//! as an avatar click ([`profile_route::open_in_side`]), and "Message" sends
//! the same DM request as the profile panel's Message button.

use makepad_widgets::Cx;

use crate::{
    home::{room_screen::UNNAMED_ROOM, rooms_list::RoomsListRef},
    shared::popup_list::{enqueue_popup_notification, PopupKind},
    sliding_sync::{submit_async_request, MatrixRequest},
};
use super::{profile_route, user_profile::{UserProfile, UserProfileAndRoomId, UserProfilePaneInfo}};

/// How long the "no side profile" notice stays, in seconds.
const NOTICE_SECS: f64 = 4.0;

/// A card button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardButton {
    ViewProfile,
    Message,
}

/// What a card button asks the app to do.
#[derive(Debug)]
pub enum CardRequest {
    /// Open the right-hand profile (see [`profile_route::open_in_side`]).
    OpenProfile(UserProfilePaneInfo),
    /// Open (or offer to create) the DM, as the profile panel's Message does.
    Message(UserProfile),
}

/// The request for `button` on the card showing `info` in `room_name`.
pub fn request_for(button: CardButton, info: &UserProfileAndRoomId, room_name: String) -> CardRequest {
    match button {
        CardButton::ViewProfile => CardRequest::OpenProfile(UserProfilePaneInfo {
            profile_and_room_id: info.clone(),
            room_name,
            room_member: None,
        }),
        CardButton::Message => CardRequest::Message(info.user_profile.clone()),
    }
}

pub(super) fn dispatch(cx: &mut Cx, request: CardRequest) {
    match request {
        CardRequest::OpenProfile(pane) => {
            if !profile_route::open_in_side(cx, pane) {
                // No right-hand region (mobile/embedded): say so rather than do nothing.
                enqueue_popup_notification(crate::i18n::tr("Click the avatar to open this profile"), PopupKind::Info, Some(NOTICE_SECS));
            }
        }
        CardRequest::Message(user_profile) => submit_async_request(MatrixRequest::OpenOrCreateDirectMessage {
            user_profile,
            // Ask for confirmation before creating a new DM room.
            allow_create: false,
        }),
    }
}

/// The display name of the room the hovered avatar is in.
pub(super) fn room_name(cx: &mut Cx, info: &UserProfileAndRoomId) -> String {
    if !cx.has_global::<RoomsListRef>() {
        return UNNAMED_ROOM.to_string();
    }
    cx.get_global::<RoomsListRef>()
        .get_room_name(&info.room_id)
        .map_or_else(|| UNNAMED_ROOM.to_string(), |name| name.to_string())
}

