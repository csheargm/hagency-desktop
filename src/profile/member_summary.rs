//! A user's standing in one room (membership and power-level role), shared
//! by the room's profile pane, the right-hand profile tab and the avatar
//! hover card, so they never disagree.

use std::borrow::Cow;
use makepad_widgets::Cx;
use matrix_sdk::{room::{RoomMember, RoomMemberRole}, ruma::{events::room::member::MembershipState, OwnedRoomId, UserId}};
use crate::i18n::tr;
use super::{profile_route, user_profile_cache::{self, RoomMemberEntry}};

/// What is known about a user's membership of a room.
#[derive(Clone, Debug)]
pub enum MemberState {
    /// Not looked up yet, or the lookup is in flight.
    Pending,
    /// The lookup finished without member info: not a member.
    Absent,
    Member(RoomMember),
}

impl MemberState {
    /// The state for a member already in hand, else for a cache `entry`.
    fn from_entry(entry: Option<&RoomMemberEntry>) -> Self {
        match entry {
            Some(RoomMemberEntry::Loaded(member)) => Self::Member(member.clone()),
            Some(RoomMemberEntry::Failed) => Self::Absent,
            Some(RoomMemberEntry::Requested) | None => Self::Pending,
        }
    }
}

/// The cached member state of `user` in `room`, first applying any profile
/// updates the background tasks have queued. With `fetch`, a missing entry
/// is requested from the server (and stays `Pending` until it arrives).
pub fn lookup(cx: &mut Cx, user: &UserId, room: &OwnedRoomId, fetch: bool) -> MemberState {
    user_profile_cache::process_user_profile_updates(cx);
    user_profile_cache::with_user_profile(cx, user.to_owned(), Some(room), fetch, |_, rooms| {
        MemberState::from_entry(rooms.get(room))
    }).unwrap_or(MemberState::Pending)
}

/// The membership status line, e.g. "Status: Joined".
pub fn membership_label(state: &MembershipState) -> &'static str {
    tr(match state {
        MembershipState::Join => "Status: Joined",
        MembershipState::Leave => "Status: Left",
        MembershipState::Ban => "Status: Banned",
        MembershipState::Invite => "Status: Invited",
        MembershipState::Knock => "Status: Knocking",
        _ => "Status: Unknown",
    })
}

/// The role line for a power-level `role`; an Agent without moderation
/// rights is shown as an Agent rather than a standard user.
pub fn role_label(role: RoomMemberRole, agent: bool) -> &'static str {
    tr(match role {
        RoomMemberRole::Creator => "Role: Creator",
        RoomMemberRole::Administrator => "Role: Admin",
        RoomMemberRole::Moderator => "Role: Moderator",
        RoomMemberRole::User if agent => "Role: Agent",
        RoomMemberRole::User => "Role: Standard User",
    })
}

/// The role line of a loaded `member`.
pub fn member_role(member: &RoomMember) -> &'static str {
    role_label(member.suggested_role_for_power_level(), profile_route::is_agent(member.user_id()))
}

/// The (membership, role) lines for `state`: both empty while loading, so a
/// profile never claims "Not a Member" before the lookup has finished.
pub fn standing_lines(state: &MemberState) -> (Cow<'static, str>, Cow<'static, str>) {
    match state {
        MemberState::Pending => (Cow::Borrowed(""), Cow::Borrowed("")),
        MemberState::Absent => (tr("Not a Member").into(), Cow::Borrowed("")),
        MemberState::Member(member) => (membership_label(member.membership()).into(), member_role(member).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_follow_power_levels_and_plain_agents_read_as_agents() {
        assert_eq!(role_label(RoomMemberRole::Creator, false), "Role: Creator");
        assert_eq!(role_label(RoomMemberRole::Administrator, false), "Role: Admin");
        assert_eq!(role_label(RoomMemberRole::Moderator, true), "Role: Moderator");
        assert_eq!(role_label(RoomMemberRole::User, false), "Role: Standard User");
        assert_eq!(role_label(RoomMemberRole::User, true), "Role: Agent");
    }

    #[test]
    fn a_pending_lookup_shows_nothing_and_a_finished_miss_says_not_a_member() {
        assert_eq!(standing_lines(&MemberState::Pending), ("".into(), "".into()));
        assert_eq!(standing_lines(&MemberState::Absent), ("Not a Member".into(), "".into()));
        assert_eq!(membership_label(&MembershipState::Join), "Status: Joined");
    }

    #[test]
    fn cache_entries_map_to_member_states() {
        assert!(matches!(MemberState::from_entry(None), MemberState::Pending));
        assert!(matches!(MemberState::from_entry(Some(&RoomMemberEntry::Requested)), MemberState::Pending));
        assert!(matches!(MemberState::from_entry(Some(&RoomMemberEntry::Failed)), MemberState::Absent));
    }
}
