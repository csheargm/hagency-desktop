//! How the Members tab lays out its rows: people (with their Agents under
//! them) first, then a collapsed "Service accounts" section for Hagency's
//! system accounts, and how a member's departure takes their row out.
//!
//! The list is `data.members` in that order: [`services_last`] keeps the
//! service accounts at the tail, so a list row maps to a member index with
//! one offset for the section header.
use matrix_sdk::ruma::{OwnedUserId, UserId};
use matrix_sdk_ui::timeline::MembershipChange;
use crate::profile::user_profile::UserProfile;

/// A Hagency system account (`@_hagency_service:…` and other reserved
/// non-Agent accounts): shown in the "Service accounts" section.
pub(super) fn is_service(user: &UserId) -> bool {
    crate::shared::mention_candidates::is_hagency_service_account(user)
}

/// Moves the service accounts to the end, keeping both groups' order;
/// returns how many there are.
pub(super) fn services_last(members: &mut Vec<UserProfile>) -> usize {
    let (people, services): (Vec<_>, Vec<_>) = members.drain(..).partition(|p| !is_service(&p.user_id));
    let count = services.len();
    members.extend(people);
    members.extend(services);
    count
}

/// The Members tab's row counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct MemberRows {
    pub people: usize,
    pub services: usize,
    /// Whether the "Service accounts" section is open.
    pub expanded: bool,
}

/// What one Members list row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MemberRow {
    /// The member at this index of `data.members`.
    Member(usize),
    /// The "Service accounts (n)" header that opens or closes the section.
    ServicesHeader,
}

impl MemberRows {
    /// The rows of `members`, already ordered by [`services_last`].
    pub(super) fn of(members: &[UserProfile], expanded: bool) -> Self {
        let services = members.iter().filter(|p| is_service(&p.user_id)).count();
        Self { people: members.len() - services, services, expanded }
    }
    #[cfg(test)]
    pub(super) fn people(people: usize) -> Self {
        Self { people, ..Self::default() }
    }
    pub(super) fn count(self) -> usize {
        let section = if self.services == 0 { 0 } else if self.expanded { 1 + self.services } else { 1 };
        self.people + section
    }
    pub(super) fn row(self, index: usize) -> Option<MemberRow> {
        if index >= self.count() { return None; }
        Some(match index.cmp(&self.people) {
            std::cmp::Ordering::Less => MemberRow::Member(index),
            std::cmp::Ordering::Equal => MemberRow::ServicesHeader,
            std::cmp::Ordering::Greater => MemberRow::Member(index - 1),
        })
    }
}

/// The badge on an Agent's row: whose Agent it is, or just "agent" when
/// the owner is not known.
pub(super) fn agent_badge(owner_name: Option<&str>) -> String {
    match owner_name.filter(|name| !name.is_empty()) {
        Some(name) => crate::i18n::format("{owner}'s agent", &[("owner", name.to_owned())]),
        None => crate::i18n::tr("agent").to_owned(),
    }
}

/// Whether a membership change takes the member out of the room's list
/// (left, kicked, or an invitation withdrawn). A ban keeps a "Banned" row.
pub(crate) fn is_departure(change: Option<MembershipChange>) -> bool {
    matches!(change, Some(MembershipChange::Left | MembershipChange::Kicked
        | MembershipChange::InvitationRejected | MembershipChange::InvitationRevoked
        | MembershipChange::KnockRetracted | MembershipChange::KnockDenied))
}

/// Takes the rows of members who left out of the list; returns whether any did.
pub(super) fn remove_departed(members: &mut Vec<UserProfile>, departed: &[OwnedUserId]) -> bool {
    let before = members.len();
    members.retain(|p| !departed.contains(&p.user_id));
    members.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::avatar::AvatarState;

    fn member(id: &str) -> UserProfile {
        UserProfile { user_id: OwnedUserId::try_from(id).unwrap(), username: None, avatar_state: AvatarState::Known(None) }
    }
    fn ids(members: &[UserProfile]) -> Vec<&str> { members.iter().map(|p| p.user_id.as_str()).collect() }

    /// Chris's screenshot: two "Hage…" service rows sat among the people.
    #[test]
    fn service_accounts_go_last_in_a_collapsed_section() {
        let mut members = vec![member("@_hagency_service:s"), member("@alice:s"), member("@_hagency_service:other"), member("@_hagency_agt_01x:s")];
        assert_eq!(services_last(&mut members), 2);
        assert_eq!(ids(&members), ["@alice:s", "@_hagency_agt_01x:s", "@_hagency_service:s", "@_hagency_service:other"], "Agents stay with the people");
        let closed = MemberRows::of(&members, false);
        assert_eq!((closed.count(), closed.row(1), closed.row(2), closed.row(3)), (3, Some(MemberRow::Member(1)), Some(MemberRow::ServicesHeader), None));
        let open = MemberRows { expanded: true, ..closed };
        assert_eq!((open.count(), open.row(3), open.row(4)), (5, Some(MemberRow::Member(2)), Some(MemberRow::Member(3))));
        assert_eq!(open.row(5), None);
    }

    #[test]
    fn a_room_without_service_accounts_has_no_section() {
        let rows = MemberRows::people(2);
        assert_eq!((rows.count(), rows.row(1), rows.row(2)), (2, Some(MemberRow::Member(1)), None));
    }

    #[test]
    fn agent_badge_names_the_owner() {
        assert_eq!(agent_badge(Some("Anna")), "Anna's agent");
        assert_eq!(agent_badge(Some("")), "agent");
        assert_eq!(agent_badge(None), "agent");
    }

    /// Live finding: a kicked Agent stayed in the Members list.
    #[test]
    fn leaving_and_kicks_remove_the_row_but_a_ban_keeps_it() {
        for change in [MembershipChange::Left, MembershipChange::Kicked, MembershipChange::InvitationRevoked] {
            assert!(is_departure(Some(change)), "{change:?}");
        }
        for change in [MembershipChange::Joined, MembershipChange::Banned, MembershipChange::KickedAndBanned, MembershipChange::None] {
            assert!(!is_departure(Some(change)), "{change:?}");
        }
        assert!(!is_departure(None));
        let mut members = vec![member("@alice:s"), member("@_hagency_agt_01x:s")];
        assert!(remove_departed(&mut members, &[OwnedUserId::try_from("@_hagency_agt_01x:s").unwrap()]));
        assert_eq!(ids(&members), ["@alice:s"]);
        assert!(!remove_departed(&mut members, &[OwnedUserId::try_from("@bob:s").unwrap()]));
    }
}
