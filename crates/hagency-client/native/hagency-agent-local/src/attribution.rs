//! Which Agent a runtime serves, as the host learned it from the server
//! (display name and puppet Matrix ID), and the identity text and per-turn
//! context the runtimes give it. Wording: `hagency_runtime::attribution`.
use crate::Scope;
use hagency_runtime::attribution;
use std::collections::BTreeMap;
use std::sync::RwLock;

static IDENTITIES: RwLock<BTreeMap<String, (String, String)>> = RwLock::new(BTreeMap::new());

/// Records the display name and Matrix ID of Agent `agent_id`.
pub fn register_agent_identity(agent_id: &str, label: &str, mxid: &str) {
    if agent_id.is_empty() || label.trim().is_empty() {
        return;
    }
    // A poisoned lock still holds a whole map: inserts are single operations.
    IDENTITIES.write().unwrap_or_else(|p| p.into_inner()).insert(agent_id.to_owned(), (label.trim().to_owned(), mxid.to_owned()));
}

/// The display name the host registered for Agent `agent_id`.
pub fn agent_label(agent_id: &str) -> Option<String> {
    IDENTITIES.read().unwrap_or_else(|p| p.into_inner()).get(agent_id).map(|(label, _)| label.clone())
}

/// The identity rules for Agent `agent_id`, once the host registered it.
pub fn identity_instructions(agent_id: &str) -> Option<String> {
    let identities = IDENTITIES.read().unwrap_or_else(|p| p.into_inner());
    identities.get(agent_id).map(|(label, mxid)| attribution::identity_note(label, mxid))
}

/// `base` followed by the Agent's identity rules, when it has any.
pub fn with_identity(base: &str, agent_id: &str) -> String {
    match identity_instructions(agent_id) {
        Some(identity) => format!("{base}\n\n{identity}"),
        None => base.to_owned(),
    }
}

/// The model-facing turn text for a registered Agent: who asked and where,
/// then the input. Ledger digests keep the original body.
pub fn turn_context(owner: &str, scope: &Scope, input: &str) -> String {
    if identity_instructions(&scope.agent).is_none() {
        return input.to_owned();
    }
    let requester = attribution::requester_label(owner, &scope.requester, input);
    attribution::with_request_context(&requester, &scope.room, input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(agent: &str, requester: &str) -> Scope {
        Scope { agent: agent.into(), binding: "b".into(), room: "!r:t".into(), requester: requester.into(), thread: "$t".into() }
    }

    #[test]
    fn registered_agent_gets_identity_and_turn_context() {
        register_agent_identity("agt_attr1", "ann", "@_hagency_agt_attr1:t");
        let text = with_identity("BASE", "agt_attr1");
        assert!(text.starts_with("BASE\n\n"));
        assert!(text.contains("Co-authored-by: ann <_hagency_agt_attr1@agents.hagency.invalid>"));
        assert_eq!(turn_context("@o:t", &scope("agt_attr1", "@bob:t"), "go"), "[Hagency request context: requester @bob:t, room !r:t]\n\ngo");
        let notice = format!("{}#1 in a/b", attribution::ISSUE_NOTICE_LEAD);
        assert!(turn_context("@o:t", &scope("agt_attr1", "@o:t"), &notice).contains("requester issue watch for @o:t,"));
    }

    #[test]
    fn unregistered_agent_is_unchanged() {
        assert_eq!(with_identity("BASE", "agt_unknown"), "BASE");
        assert_eq!(turn_context("@o:t", &scope("agt_unknown", "@bob:t"), "go"), "go");
    }
}
