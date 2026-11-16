//! Whether a timeline event's claimed sender can be believed (ADR 0013). The
//! memory reader and the proposal cards share this rule: in an encrypted room
//! only a decrypted event whose encrypting identity matches its sender counts;
//! a plaintext event there could have been injected by the homeserver.
use matrix_sdk::deserialized_responses::{DeviceLinkProblem, EncryptionInfo, VerificationLevel, VerificationState};

/// Decrypted under a different identity than its sender, or by a device whose
/// link to that identity is missing or broken.
pub(crate) fn identity_untrusted(key_sender: &str, verification: &VerificationState, sender: &str) -> bool {
    key_sender != sender
        || match verification {
            VerificationState::Verified => false,
            VerificationState::Unverified(level) => matches!(
                level,
                VerificationLevel::None(DeviceLinkProblem::MissingDevice | DeviceLinkProblem::InsecureSource)
                    | VerificationLevel::MismatchedSender
                    | VerificationLevel::VerificationViolation
            ),
        }
}

/// `room_encrypted` is `None` while the room's encryption state is unknown,
/// which is never trusted.
pub(crate) fn sender_authentic(room_encrypted: Option<bool>, info: Option<&EncryptionInfo>, sender: &str) -> bool {
    match (room_encrypted, info) {
        (None, _) => false,
        (Some(_), Some(info)) => !identity_untrusted(info.sender.as_str(), &info.verification_state, sender),
        (Some(encrypted), None) => !encrypted,
    }
}

/// The timeline item's sender is authentic in `room` as the client sees it now.
/// An edited item is never authentic: its encryption info is the original
/// event's, while its JSON is the (unchecked) replacement's.
pub(crate) fn event_authentic(event: &matrix_sdk_ui::timeline::EventTimelineItem, room: &matrix_sdk::ruma::RoomId) -> bool {
    if event.latest_edit_json().is_some() {
        return false;
    }
    let encrypted = crate::sliding_sync::get_client().and_then(|c| c.get_room(room)).and_then(|r| {
        let state = r.encryption_state();
        (!state.is_unknown()).then(|| state.is_encrypted())
    });
    sender_authentic(encrypted, event.encryption_info(), event.sender().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::deserialized_responses::AlgorithmInfo;
    use matrix_sdk::ruma::owned_user_id;

    const AGENT: &str = "@_hagency_agt_01:hs.example";

    fn info(sender: &str, verification: VerificationState) -> EncryptionInfo {
        EncryptionInfo {
            sender: sender.try_into().expect("user id"),
            sender_device: None,
            forwarder: None,
            algorithm_info: AlgorithmInfo::MegolmV1AesSha2 { curve25519_key: String::new(), sender_claimed_keys: Default::default(), session_id: None },
            verification_state: verification,
        }
    }

    #[test]
    fn plaintext_in_an_encrypted_room_is_not_authentic() {
        assert!(!sender_authentic(Some(true), None, AGENT));
    }
    #[test]
    fn plaintext_in_a_plaintext_room_is_authentic() {
        assert!(sender_authentic(Some(false), None, AGENT));
    }
    #[test]
    fn unknown_room_encryption_is_not_authentic() {
        assert!(!sender_authentic(None, None, AGENT));
    }
    #[test]
    fn mismatched_encrypting_identity_is_not_authentic() {
        let other = owned_user_id!("@mallory:hs.example");
        assert!(!sender_authentic(Some(true), Some(&info(other.as_str(), VerificationState::Verified)), AGENT));
    }
    #[test]
    fn mismatched_sender_verification_is_not_authentic() {
        let bad = VerificationState::Unverified(VerificationLevel::MismatchedSender);
        assert!(!sender_authentic(Some(true), Some(&info(AGENT, bad)), AGENT));
    }
    #[test]
    fn matching_decrypted_sender_is_authentic() {
        assert!(sender_authentic(Some(true), Some(&info(AGENT, VerificationState::Verified)), AGENT));
    }
}
