//! The one rule for recognising a Hagency Agent puppet by its Matrix ID.
//!
//! Hagency servers reserve every `_hagency_` localpart for their Appservice
//! (exclusive namespace `^@_hagency_[a-z0-9_-]+:<server>$`). Inside it, the
//! service account is `_hagency_service`; every other account is an Agent:
//! older ones are `_hagency_agt_<ulid>`, new ones `_hagency_<owner>_<name>[-N]`
//! (server ADR 0002). Never derive an Agent ID from the puppet ID or the
//! reverse: read the pairing from the server's Agent listing.
//!
//! This is a naming test, not authority: another server can name any account
//! this way, so callers still check the server or the owner's roster.

/// Localpart prefix the server's Appservice owns exclusively.
pub const RESERVED_LOCALPART_PREFIX: &str = "_hagency_";
/// The Appservice's own sender, the one reserved account that is not an Agent.
pub const SERVICE_LOCALPART: &str = "_hagency_service";

/// Whether `localpart` names an Agent puppet in the reserved namespace.
pub fn is_puppet_localpart(localpart: &str) -> bool {
    localpart
        .strip_prefix(RESERVED_LOCALPART_PREFIX)
        .is_some_and(|rest| {
            !rest.is_empty()
                && rest.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        })
        && localpart != SERVICE_LOCALPART
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_puppet_forms_are_agents_and_the_service_is_not() {
        for agent in [
            "_hagency_agt_01m7k5g5n36gcamvq8f8m07e6n",
            "_hagency_csheargm_anna-claude",
            "_hagency_csheargm_anna-claude-2",
            "_hagency_a-b_c",
            "_hagency_user_agent",
            "_hagency_service_bot",
        ] {
            assert!(is_puppet_localpart(agent), "{agent}");
        }
        for other in [
            "_hagency_service",
            "_hagency_",
            "alice",
            "hagency_agt_x",
            "_Hagency_agt_x",
            "_hagency_Anna",
            "_hagency_anna.claude",
            "ac_hagency_x",
        ] {
            assert!(!is_puppet_localpart(other), "{other}");
        }
    }
}
