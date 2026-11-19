//! The timeline line for an invite the Hagency service account sent (Create
//! agent's "Add to rooms", Add agent): "Hagency added test-helper to this
//! room." instead of the raw "_hagency_service invited test-helper to this
//! room." Only the service account on the invitee's own server counts, so a
//! remote account named `_hagency_service` keeps its plain line. Every other
//! inviter is unchanged.
use matrix_sdk::ruma::{UserId, events::room::member::MembershipState};
use matrix_sdk_ui::timeline::MembershipChange;

/// Whether the change is an invite, as the membership line reads it: ruma's
/// `Invited`, or an unclassified change whose new membership is `invite`.
pub fn is_invite(change: Option<MembershipChange>, membership: &MembershipState) -> bool {
    match change {
        Some(MembershipChange::Invited) => true,
        None | Some(MembershipChange::NotImplemented | MembershipChange::None | MembershipChange::Error) => *membership == MembershipState::Invite,
        Some(_) => false,
    }
}

/// The whole line, sender included, for this invite; `None` when the sender
/// is not the invitee's own Hagency service account.
pub fn service_invite_line(sender: &UserId, invitee: &UserId, invitee_name: &str) -> Option<String> {
    let service = crate::shared::mention_candidates::is_hagency_service_account(sender);
    (service && sender.server_name() == invitee.server_name())
        .then(|| crate::i18n::format("Hagency added {invitee} to this room.", &[("invitee", invitee_name.to_owned())]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    /// Regression (Nov 18 screen pass): "_hagency_service invited test-helper to this room."
    #[test]
    fn the_service_account_reads_as_hagency_added() {
        let line = service_invite_line(user_id!("@_hagency_service:s"), user_id!("@_hagency_ann_test-helper:s"), "test-helper");
        assert_eq!(line.as_deref(), Some("Hagency added test-helper to this room."));
    }
    #[test]
    fn only_invites_are_reworded() {
        assert!(is_invite(Some(MembershipChange::Invited), &MembershipState::Invite));
        assert!(is_invite(None, &MembershipState::Invite));
        assert!(!is_invite(Some(MembershipChange::Kicked), &MembershipState::Leave));
        assert!(!is_invite(Some(MembershipChange::InvitationRevoked), &MembershipState::Leave));
    }
    #[test]
    fn people_agents_and_remote_service_accounts_keep_their_line() {
        let invitee = user_id!("@bob:s");
        assert_eq!(service_invite_line(user_id!("@alice:s"), invitee, "bob"), None);
        assert_eq!(service_invite_line(user_id!("@_hagency_agt_01x:s"), invitee, "bob"), None, "an Agent puppet is not the service");
        assert_eq!(service_invite_line(user_id!("@_hagency_service:other"), invitee, "bob"), None, "a remote look-alike");
    }
}
