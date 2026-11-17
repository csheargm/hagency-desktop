//! "Remove from room" on a Members row: shown only for members the signed-in
//! account may kick under the room's power levels (any member, other people's
//! Agents included), and confirmed in the moderation modal, which checks the
//! power levels again before it kicks.
use std::collections::HashSet;
use makepad_widgets::*;
use matrix_sdk::{Room, room::RoomMember, ruma::OwnedUserId};
use crate::moderation::{MemberAbilities, MemberAction, MemberModeration};
use crate::moderation_modal::{ModerationModalAction, ModerationRequest};
use crate::profile::user_profile::UserProfile;
use crate::utils::RoomNameId;

/// The members the signed-in account may remove from `room`.
pub(super) async fn removable(room: &Room, members: &[RoomMember]) -> HashSet<OwnedUserId> {
    let Ok(power_levels) = room.power_levels().await else { return HashSet::new() };
    let message = crate::moderation::get_message_event_type(room);
    members.iter().filter(|member| MemberAbilities::from_power_levels(
        &power_levels, room.own_user_id(), member.user_id(), Some(member.membership()), message.clone(),
    ).permits(MemberAction::Kick)).map(|member| member.user_id().to_owned()).collect()
}

/// Shows the row's Remove button only for a removable member.
pub(super) fn show(cx: &mut Cx, row: &WidgetRef, removable: &HashSet<OwnedUserId>, profile: &UserProfile) {
    row.button(cx, ids!(remove_member)).set_visible(cx, removable.contains(&profile.user_id));
}

/// The confirmation the Remove button asks for, if it was clicked.
pub(super) fn clicked(cx: &mut Cx, row: &WidgetRef, actions: &Actions, room: &RoomNameId,
    removable: &HashSet<OwnedUserId>, profile: &UserProfile) -> Option<ModerationModalAction> {
    (removable.contains(&profile.user_id) && row.button(cx, ids!(remove_member)).clicked(actions))
        .then(|| ModerationModalAction::Open(request(room, profile)))
}

fn request(room: &RoomNameId, profile: &UserProfile) -> ModerationRequest {
    ModerationRequest::Member(MemberModeration {
        room_id: room.room_id().clone(), room_name: room.to_string(), user_id: profile.user_id.clone(),
        user_name: profile.displayable_name().to_owned(), action: MemberAction::Kick,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live finding: the Members list had no way to take a member out.
    #[test]
    fn remove_asks_to_kick_that_member_from_this_room() {
        let room = RoomNameId::new(matrix_sdk::RoomDisplayName::Named("Ops".into()), "!ops:s".try_into().unwrap());
        let profile = UserProfile { user_id: "@bot:s".try_into().unwrap(), username: Some("Bot".into()), avatar_state: crate::shared::avatar::AvatarState::Known(None) };
        let ModerationRequest::Member(m) = request(&room, &profile) else { panic!("a member request") };
        assert_eq!((m.room_id.as_str(), m.user_id.as_str(), m.user_name.as_str()), ("!ops:s", "@bot:s", "Bot"));
        assert!(matches!(m.action, MemberAction::Kick));
    }

    #[test]
    fn the_member_row_has_a_hidden_remove_button() {
        let src = include_str!("../mobile_chat_info.rs");
        assert!(src.contains("remove_member := RobrixNegativeIconButton {visible: false"), "the row's Remove starts hidden");
    }
}
