//! The timeline line for an invite the Hagency service account sent (Create
//! agent's "Add to rooms", Add agent): "Hagency added test-helper to this
//! room." instead of the raw "_hagency_service invited test-helper to this
//! room." The service account on the viewer's own server reads "Hagency";
//! one on another server names it, "Hagency (other.example) added …", so a
//! remote look-alike never passes for the local one. Every other inviter is
//! unchanged.
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
/// is not a Hagency service account. `viewer` is the signed-in account.
pub fn service_invite_line(sender: &UserId, viewer: Option<&UserId>, invitee_name: &str) -> Option<String> {
    if !crate::shared::mention_candidates::is_hagency_service_account(sender) { return None; }
    let invitee = ("invitee", invitee_name.to_owned());
    Some(if viewer.is_some_and(|v| v.server_name() == sender.server_name()) {
        crate::i18n::format("Hagency added {invitee} to this room.", &[invitee])
    } else {
        crate::i18n::format("Hagency ({server}) added {invitee} to this room.", &[("server", sender.server_name().to_string()), invitee])
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::ruma::user_id;

    /// Regression (Nov 18 screen pass): "_hagency_service invited test-helper to this room."
    #[test]
    fn the_service_account_reads_as_hagency_added() {
        let line = service_invite_line(user_id!("@_hagency_service:s"), Some(user_id!("@ann:s")), "test-helper");
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
        let viewer = Some(user_id!("@ann:s"));
        assert_eq!(service_invite_line(user_id!("@alice:s"), viewer, "bob"), None);
        assert_eq!(service_invite_line(user_id!("@_hagency_agt_01x:s"), viewer, "bob"), None, "an Agent puppet is not the service");
        // GPT review: a remote service is named by its server, never as the local one.
        assert_eq!(service_invite_line(user_id!("@_hagency_service:other"), viewer, "bob").as_deref(), Some("Hagency (other) added bob to this room."));
    }
}
