//! Pure helper behind the profile's "Rooms in common" list: which joined
//! rooms a user shares with us.

use std::collections::BTreeSet;
use matrix_sdk::ruma::{OwnedRoomId, OwnedUserId, UserId};

/// One joined room as this device knows it: no network involved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownRoom {
    pub room_id: OwnedRoomId,
    pub name: String,
    /// Members whose `join` membership is in the local store.
    pub joined_members: BTreeSet<OwnedUserId>,
    /// Whether the store holds this room's complete member list.
    pub members_complete: bool,
    /// Rooms the panel never lists, such as the Moments feed.
    pub hidden: bool,
}

/// A room listed under "Rooms in common".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomInCommon {
    pub room_id: OwnedRoomId,
    pub name: String,
}

/// What the Rooms in common tab shows for one user.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoomsInCommon {
    pub rooms: Vec<RoomInCommon>,
    /// Joined rooms whose member list isn't fully loaded, so the user may
    /// also be in some of them without the list showing it.
    pub incomplete: usize,
}

/// The joined rooms that `user` is known to be a member of, sorted by name
/// (case-insensitive, then room ID). A room without a name shows its ID.
pub fn rooms_in_common(user: &UserId, joined: &[KnownRoom]) -> RoomsInCommon {
    let visible = joined.iter().filter(|room| !room.hidden);
    let mut rooms: Vec<RoomInCommon> = visible.clone()
        .filter(|room| room.joined_members.iter().any(|member| member == user))
        .map(|room| RoomInCommon {
            room_id: room.room_id.clone(),
            name: if room.name.trim().is_empty() { room.room_id.to_string() } else { room.name.clone() },
        })
        .collect();
    rooms.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.room_id.cmp(&b.room_id)));
    rooms.dedup_by(|a, b| a.room_id == b.room_id);
    let incomplete = visible
        .filter(|room| !room.members_complete && !room.joined_members.iter().any(|member| member == user))
        .count();
    RoomsInCommon { rooms, incomplete }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn user(id: &str) -> OwnedUserId { OwnedUserId::try_from(id).unwrap() }
    fn room(id: &str, name: &str, members: &[&str], complete: bool) -> KnownRoom {
        KnownRoom {
            room_id: OwnedRoomId::try_from(id).unwrap(),
            name: name.into(),
            joined_members: members.iter().map(|m| user(m)).collect(),
            members_complete: complete,
            hidden: false,
        }
    }

    #[test]
    fn lists_only_rooms_where_the_user_is_a_known_member_sorted_by_name() {
        let joined = [
            room("!b:s", "beta", &["@me:s", "@ann:s"], true),
            room("!a:s", "Alpha", &["@me:s", "@ann:s", "@bob:s"], true),
            room("!c:s", "Gamma", &["@me:s", "@bob:s"], true),
            room("!d:s", "", &["@ann:s"], true),
        ];
        let found = rooms_in_common(&user("@ann:s"), &joined);
        let names: Vec<&str> = found.rooms.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["!d:s", "Alpha", "beta"], "unnamed rooms show their ID; names sort case-insensitively");
        assert_eq!(found.incomplete, 0);
    }

    #[test]
    fn hidden_rooms_are_skipped_and_unloaded_member_lists_are_counted() {
        let mut moments = room("!m:s", "Moments", &["@ann:s"], true);
        moments.hidden = true;
        let joined = [moments, room("!x:s", "Big", &["@me:s"], false), room("!y:s", "Ops", &["@ann:s"], false)];
        let found = rooms_in_common(&user("@ann:s"), &joined);
        assert_eq!(found.rooms, vec![RoomInCommon { room_id: OwnedRoomId::try_from("!y:s").unwrap(), name: "Ops".into() }]);
        assert_eq!(found.incomplete, 1, "only rooms that may still hide the user count as incomplete");
        assert_eq!(rooms_in_common(&user("@zed:s"), &[]), RoomsInCommon::default());
    }

}
