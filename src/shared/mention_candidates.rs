//! Who the "@" picker offers: the room's current joined members, read when the
//! picker matches, minus the user and Hagency's service accounts.
//!
//! The composer's member snapshot is refreshed only on room open and on newly
//! observed membership timeline items, so an Agent that joined while those
//! paths missed it never became mentionable. Reading the SDK's member list at
//! match time (the list the room information panel shows) closes that gap.

use std::sync::Arc;
use matrix_sdk::{room::RoomMember, ruma::{RoomId, UserId}, RoomMemberships};

/// Every Hagency-reserved localpart starts with this.
const HAGENCY_RESERVED_PREFIX: &str = "_hagency";

/// A Hagency service account on any server (`_hagency_service` or any other
/// `_hagency*` account that is not an Agent puppet): system plumbing, not
/// someone to @ or pick. Agent puppets, old `_hagency_agt_…` and readable
/// `_hagency_<owner>_<name>` alike, stay mentionable.
pub(crate) fn is_hagency_service_account(user: &UserId) -> bool {
    let localpart = user.localpart();
    localpart.starts_with(HAGENCY_RESERVED_PREFIX) && !hagency_core::puppet::is_puppet_localpart(localpart)
}

/// Whether `user` belongs in the "@" suggestions.
pub(super) fn is_mention_candidate(user: &UserId, current_user: Option<&UserId>) -> bool {
    current_user != Some(user) && !is_hagency_service_account(user)
}

/// Prefer the member list read now; fall back to the composer's snapshot
/// when the read failed. `None` means nothing is known yet (keep loading).
pub(super) fn choose_members<T>(fresh: Option<Vec<T>>, snapshot: Option<Arc<Vec<T>>>) -> Option<Arc<Vec<T>>> {
    fresh.map(Arc::new).or(snapshot)
}

/// The room's joined members as the SDK knows them right now. This fetches
/// from the homeserver only until the room's member list has been synced once.
pub(super) async fn current_members(room_id: &RoomId) -> Option<Vec<RoomMember>> {
    let room = crate::sliding_sync::get_client()?.get_room(room_id)?;
    // A failed read is not fatal: the caller falls back to its snapshot.
    room.members(RoomMemberships::JOIN).await.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    #[test]
    fn service_accounts_on_every_server_are_hidden_but_agents_stay() {
        let me = user_id!("@ann:a.server");
        assert!(!is_mention_candidate(user_id!("@_hagency_service:a.server"), Some(me)));
        assert!(!is_mention_candidate(user_id!("@_hagency_service:b.server"), Some(me)));
        assert!(!is_mention_candidate(me, Some(me)));
        assert!(is_mention_candidate(user_id!("@_hagency_agt_ann5agent:a.server"), Some(me)));
        assert!(is_mention_candidate(user_id!("@_hagency_ann_helper-2:a.server"), Some(me)), "readable puppet IDs");
        assert!(!is_mention_candidate(user_id!("@_hagencybot:a.server"), Some(me)));
        assert!(is_mention_candidate(user_id!("@webster2:b.server"), Some(me)));
        assert!(is_mention_candidate(user_id!("@hagency_service:a.server"), None));
        assert!(!is_mention_candidate(user_id!("@_hagency_bridge:a.server"), Some(me)));
        assert!(!is_mention_candidate(user_id!("@_hagency:a.server"), Some(me)));
    }

    #[test]
    fn a_member_who_joined_after_the_snapshot_is_offered() {
        let snapshot = Some(Arc::new(vec!["ann", "ann4agent"]));
        let fresh = Some(vec!["ann", "ann4agent", "ann5agent"]);
        assert_eq!(choose_members(fresh, snapshot.clone()).as_deref(), Some(&vec!["ann", "ann4agent", "ann5agent"]));
        assert_eq!(choose_members(None, snapshot).as_deref(), Some(&vec!["ann", "ann4agent"]), "a failed read keeps the snapshot");
        assert_eq!(choose_members::<&str>(None, None), None);
    }
}
