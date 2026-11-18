//! What an Agent's own run cards in a room say about it: whose Agent it is,
//! its runtime, and whether it is working on a request right now.
//!
//! The room's project roster only lists Agents of this server's projects, so
//! another server's Agent (federation) has no roster row and its owner was
//! "not shared". Its run cards are room messages every member already sees
//! (they say "Waiting for <owner>'s approval"), admitted only from a joined
//! Agent whose owner is on the Agent's own server (`pending_questions`). The
//! owner they name is still the Agent's own claim, and is shown as such.

use std::time::Duration;
use matrix_sdk::ruma::{OwnedUserId, RoomId, UserId};
use ::hagency::agent_run_controls::Card;

/// How long a running card may go without an update and still count as
/// work: a run abandoned without its final edit is not "working" forever.
pub const STALE_RUN: Duration = Duration::from_secs(30 * 60);

/// What the cards tell; `None` fields are not known from them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CardFacts {
    pub owner: Option<OwnedUserId>,
    /// The cards name more than one owner, so none of them is shown.
    pub owner_conflict: bool,
    /// "Claude", "Codex" or "Octos", from the newest card naming one.
    pub runtime: Option<&'static str>,
    /// The Agent's newest run in this room is still going, and updated lately.
    pub working: bool,
}

/// [`card_facts`] for the cards `agent` posted in `room`.
pub fn in_room(room: &RoomId, agent: &UserId) -> CardFacts {
    card_facts(&crate::hagency::run_controls::cards_of(room, agent.as_str()), agent)
}

/// The owner every card agrees on (on the Agent's own server; two owners
/// claimed means none is trusted), the newest runtime, and whether the newest
/// run (cards are oldest first, each with how long ago it last changed) is
/// still going: an older run left "running" does not outlive a newer one.
pub fn card_facts(cards: &[(Card, Duration)], agent: &UserId) -> CardFacts {
    let mut owners = cards.iter()
        .filter_map(|(card, _)| OwnedUserId::try_from(card.owner.as_str()).ok())
        .filter(|owner| owner.server_name() == agent.server_name());
    let first = owners.next();
    let owner_conflict = first.as_ref().is_some_and(|first| owners.any(|other| other != *first));
    let owner = first.filter(|_| !owner_conflict);
    let runtime = cards.iter().rev().map(|(card, _)| card.runtime.as_str()).find(|r| !r.is_empty())
        .map(crate::hagency::agent_panel::mapping::harness_label);
    let working = cards.last().is_some_and(|(card, idle)| card.state.active() && *idle < STALE_RUN);
    CardFacts { owner, owner_conflict, runtime, working }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hagency::run_controls::fixture;
    use ::hagency::agent_run_controls::State;
    use matrix_sdk::ruma::user_id;

    fn card(owner: &str, runtime: &str, state: State) -> (Card, Duration) {
        let mut card = fixture("$a", state, "").card;
        (card.owner, card.runtime) = (owner.into(), runtime.into());
        (card, Duration::ZERO)
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
        assert!(card_facts(&two, agent).owner_conflict, "and said so, not \"not shared\"");
        assert!(!card_facts(&two[..1], agent).owner_conflict);
        assert_eq!(card_facts(&[card("@bird:beta", "", State::Completed)], agent).runtime, None, "no runtime named");
    }

    /// GPT integration review: a run abandoned without its final edit kept
    /// the Agent "Working on a request" for good, even after a newer run
    /// completed. Only the newest run counts, and only while it updates.
    #[test]
    fn only_the_newest_recently_updated_run_is_work() {
        let agent = user_id!("@_hagency_agt_b:beta");
        let abandoned = card("@bird:beta", "", State::Running);
        let newer = card("@bird:beta", "", State::Completed);
        assert!(!card_facts(&[abandoned.clone(), newer], agent).working, "a newer completed run clears it");
        assert!(card_facts(std::slice::from_ref(&abandoned), agent).working, "a live run is work");
        let silent = (abandoned.0, STALE_RUN + Duration::from_secs(1));
        assert!(!card_facts(&[silent], agent).working, "a run silent past the bound is not");
    }
}
