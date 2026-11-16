//! What the issue-watch section may do and why not, without widgets: the
//! saved watch (or Off), whether to read it, and the reasons shown for every
//! disabled control.
use serde_json::Value;

/// Where the public token page is; a classic token with `repo` scope works.
pub(crate) const TOKENS_URL: &str = "https://github.com/settings/tokens";

/// The section's inputs that decide what is enabled.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct State {
    /// The Agent panel has handed over this Agent's status.
    pub known: bool,
    /// This device runs the Agent, so its ledger holds the watch.
    pub editable: bool,
    /// The saved watch (or Off) has been read.
    pub read: bool,
    pub pending: bool,
    pub no_rooms: bool,
    /// `Some(false)`: neither the Agent nor the environment has a token.
    pub token: Option<bool>,
}

impl State {
    /// Mode, repository and Save are usable.
    pub(crate) fn usable(&self) -> bool {
        self.known && self.editable && self.read && !self.pending
    }
    /// The lines that explain each disabled control or missing piece (English keys).
    pub(crate) fn reasons(&self) -> Vec<&'static str> {
        let blocking = if !self.known {
            Some("Waiting for this Agent's status…")
        } else if !self.editable {
            Some("Issue watching runs on the device that runs this Agent.")
        } else if self.pending && !self.read {
            Some("Reading issue watching…")
        } else if !self.read {
            Some("Issue watching has not been read yet; it is read again when this panel refreshes.")
        } else {
            None
        };
        if let Some(reason) = blocking {
            return vec![reason];
        }
        let mut lines = Vec::new();
        if self.no_rooms {
            lines.push("Add this Agent to a Room first; the Rooms it is in are listed here.");
        }
        if self.token == Some(false) {
            lines.push(super::NO_TOKEN);
        }
        lines
    }
}

/// The saved watch, or "off" for an Agent that never saved one: a missing
/// watch (`issueWatch: null`) left every control disabled with no reason shown.
pub(crate) fn saved_or_off(reply: &Value) -> Value {
    match &reply["issueWatch"] {
        watch if watch.is_object() => watch.clone(),
        _ => serde_json::json!({"mode": "off"}),
    }
}

/// Whether the section should read the saved watch now.
pub(crate) fn needs_read(editable: bool, agent_id: &str, watch: &Value, pending: bool) -> bool {
    editable && !agent_id.is_empty() && !watch.is_object() && !pending
}

/// `Some(false)` only when no token exists anywhere; a store that cannot be
/// read or does not answer in time is "unknown", not "missing".
pub(crate) async fn token_known(agent_id: &str) -> Option<bool> {
    match tokio::time::timeout(TOKEN_TIMEOUT, super::github_token(agent_id)).await {
        Ok(Ok(_)) => Some(true),
        Ok(Err(error)) if error == crate::i18n::tr(super::NO_TOKEN) => Some(false),
        _ => None,
    }
}
const TOKEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);


#[cfg(test)]
mod tests {
    use super::State;
    use serde_json::{Value, json};
    /// Regression (live 2026-11-15): a fresh local Agent's watch controls stayed greyed out.
    #[test]
    fn an_agent_without_a_saved_watch_is_editable_as_off() {
        assert_eq!(super::saved_or_off(&json!({"issueWatch":null})), json!({"mode":"off"}));
        assert_eq!(super::saved_or_off(&json!({})), json!({"mode":"off"}));
        let saved = json!({"issueWatch":{"mode":"trusted","repo":"o/r"}});
        assert_eq!(super::saved_or_off(&saved)["repo"], "o/r");
    }
    /// Regression (live 2026-11-15): a flip of the device check restarted the read forever.
    #[test]
    fn a_pending_or_finished_read_is_not_restarted() {
        assert!(super::needs_read(true, "a1", &Value::Null, false));
        assert!(!super::needs_read(true, "a1", &Value::Null, true), "pending: wait for it");
        assert!(!super::needs_read(true, "a1", &json!({"mode":"off"}), false), "already read");
        assert!(!super::needs_read(false, "a1", &Value::Null, false), "not on this device");
    }
    /// Regression (live 2026-11-16, anna-fixer): greyed controls with no reason shown.
    #[test]
    fn every_disabled_state_says_why() {
        let states = [
            State::default(),
            State { known: true, ..State::default() },
            State { known: true, editable: true, pending: true, ..State::default() },
            State { known: true, editable: true, ..State::default() },
        ];
        for state in states {
            assert!(!state.usable());
            assert_eq!(state.reasons().len(), 1, "{state:?}");
        }
    }
    #[test]
    fn a_read_watch_is_usable_and_names_missing_rooms_and_token() {
        let ready = State { known: true, editable: true, read: true, ..State::default() };
        assert!(ready.usable());
        assert!(ready.reasons().is_empty());
        let bare = State { no_rooms: true, token: Some(false), ..ready };
        assert!(bare.usable(), "Off can still be saved");
        assert_eq!(bare.reasons().len(), 2);
        assert!(bare.reasons()[1].contains(super::TOKENS_URL));
    }
}
