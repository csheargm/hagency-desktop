//! An Agent's owner, published in the Agent's own Matrix profile.
//!
//! The Agent's profile field [`OWNER_FIELD`] holds its owner's Matrix ID. Any
//! member of a room it is in, on any server, can read it (the reader's server
//! asks the Agent's server over federation), and only the Agent's account (or
//! its server's Appservice) can write it. A server that holds the Agent's
//! account also holds its owner's (the claim counts only when both are on one
//! server), so the claim is as strong as the room roster that server keeps.
//!
//! A profile field, not room state: it needs no power level in any room, adds
//! no timeline event, and does not move the Agent's join event (where its
//! room history starts), so it is set once and holds in every room it joins.

use ruma_common::UserId;
use serde_json::Value;

/// The profile field naming the Agent's owner.
pub const OWNER_FIELD: &str = "io.hagency.owner";

/// Whether `agent` may publish `owner` as its owner: an Agent puppet naming
/// a person on its own server.
pub fn may_claim(agent: &str, owner: &str) -> bool {
    match (UserId::parse(agent), UserId::parse(owner)) {
        (Ok(agent), Ok(owner)) => may_own(&agent, &owner),
        _ => false,
    }
}

/// The owner `agent`'s profile field `value` names, if it is a person on the
/// Agent's own server.
pub fn verified_owner(agent: &str, value: &Value) -> Option<String> {
    let owner = value.as_str()?;
    may_claim(agent, owner).then(|| owner.to_owned())
}

fn may_own(agent: &UserId, owner: &UserId) -> bool {
    crate::puppet::is_puppet_localpart(agent.localpart())
        && !owner.localpart().starts_with(crate::puppet::RESERVED_LOCALPART_PREFIX)
        && agent.server_name() == owner.server_name()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AGENT: &str = "@_hagency_agt_01k:localhost:8452";

    /// Nov 18: Anna on alpha could not tell whose bird-claude (on beta) was
    /// until it had posted a run card; its profile now says from the start.
    #[test]
    fn a_federated_agents_profile_names_its_owner() {
        assert!(may_claim(AGENT, "@bird:localhost:8452"));
        assert_eq!(verified_owner(AGENT, &json!("@bird:localhost:8452")).as_deref(), Some("@bird:localhost:8452"));
        assert!(may_claim("@_hagency_bird_claude:beta", "@bird:beta"), "the readable puppet form too");
    }

    #[test]
    fn only_a_person_on_the_agents_own_server_is_accepted() {
        assert_eq!(verified_owner(AGENT, &json!("@anna:localhost:8450")), None, "owner on another server");
        assert_eq!(verified_owner(AGENT, &json!("@_hagency_service:localhost:8452")), None, "a service account owns nothing");
        assert_eq!(verified_owner(AGENT, &json!("@_hagency_agt_02:localhost:8452")), None, "an Agent owns no Agent");
        assert_eq!(verified_owner("@eve:localhost:8452", &json!("@bird:localhost:8452")), None, "a person cannot pose as an Agent");
        assert_eq!(verified_owner(AGENT, &json!({"owner": "@bird:localhost:8452"})), None, "not a string");
        assert_eq!(verified_owner(AGENT, &json!("bird")), None, "not a Matrix ID");
        assert!(!may_claim("not an id", "@bird:beta"));
    }
}
