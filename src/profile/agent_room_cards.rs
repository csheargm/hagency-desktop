//! What an Agent's own run cards in a room say about it: whose Agent it is,
//! its runtime, and whether it is working on a request right now.
//!
//! The room's project roster only lists Agents of this server's projects, so
//! another server's Agent (federation) has no roster row and its owner was
//! "not shared". Its run cards are room messages every member already sees
//! (they say "Waiting for <owner>'s approval"), admitted only from a joined
//! Agent whose owner is on the Agent's own server (`pending_questions`), so
//! the profile may name that owner too.

use matrix_sdk::ruma::{OwnedUserId, RoomId, UserId};
use ::hagency::agent_run_controls::Card;

/// What the cards tell; `None` fields are not known from them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardFacts {
    pub owner: Option<OwnedUserId>,
    /// "Claude", "Codex" or "Octos", from the newest card naming one.
    pub runtime: Option<&'static str>,
    /// A run of this Agent is still going in this room.
    pub working: bool,
}

/// [`card_facts`] for the cards `agent` posted in `room`.
pub fn in_room(room: &RoomId, agent: &UserId) -> CardFacts {
    card_facts(&crate::hagency::run_controls::cards_of(room, agent.as_str()), agent)
}

/// The owner every card agrees on (on the Agent's own server; two owners
/// claimed means none is trusted), the newest runtime, and whether one runs.
pub fn card_facts(cards: &[Card], agent: &UserId) -> CardFacts {
    let mut owners = cards.iter()
        .filter_map(|card| OwnedUserId::try_from(card.owner.as_str()).ok())
        .filter(|owner| owner.server_name() == agent.server_name());
    let first = owners.next();
    let owner = first.filter(|first| owners.all(|other| other == *first));
    let runtime = cards.iter().rev().map(|card| card.runtime.as_str()).find(|r| !r.is_empty())
        .map(crate::hagency::agent_panel::mapping::harness_label);
    let working = cards.iter().any(|card| card.state.active());
    CardFacts { owner, runtime, working }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::run_controls::fixture;
    use ::hagency::agent_run_controls::State;
    use matrix_sdk::ruma::user_id;

    fn card(owner: &str, runtime: &str, state: State) -> Card {
        let mut card = fixture("$a", state, "").card;
        (card.owner, card.runtime) = (owner.into(), runtime.into());
        card
    }

    /// Live finding (Nov 18): Anna's profile of Betty's federated Agent said
    /// "Owner not shared in this room"; its own run cards name Betty.
    #[test]
    fn a_federated_agents_cards_name_its_owner_runtime_and_work() {
        let agent = user_id!("@_hagency_agt_b:beta");
        let cards = [card("@bird:beta", "codex", State::Completed), card("@bird:beta", "claude", State::Running)];
        let facts = card_facts(&cards, agent);
        assert_eq!(facts.owner.as_deref(), Some(user_id!("@bird:beta")));
        assert_eq!(facts.runtime, Some("Claude"), "the newest card's runtime");
        assert!(facts.working);
        assert!(!card_facts(&cards[..1], agent).working, "a finished run is not work");
        assert_eq!(card_facts(&[], agent), CardFacts::default(), "no cards: nothing claimed");
    }

    #[test]
    fn an_owner_on_another_server_or_two_owners_are_not_trusted() {
        let agent = user_id!("@_hagency_agt_b:beta");
        assert_eq!(card_facts(&[card("@anna:alpha", "", State::Completed)], agent).owner, None, "an owner off the Agent's server");
        let two = [card("@bird:beta", "", State::Completed), card("@eve:beta", "", State::Completed)];
        assert_eq!(card_facts(&two, agent).owner, None, "conflicting owners");
        assert_eq!(card_facts(&[card("@bird:beta", "", State::Completed)], agent).runtime, None, "no runtime named");
    }
}
