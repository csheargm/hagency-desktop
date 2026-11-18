//! Whose Agent another server's Agent is, as far as anyone can tell: with no
//! roster row on this server, only the Agent itself says so, in its profile
//! field ([`super::agent_profile_owner`]) and its run cards
//! ([`super::agent_room_cards`]). Neither is verified, so the card words it
//! as the Agent's statement, and shows no owner when the two disagree.

use matrix_sdk::ruma::OwnedUserId;
use super::agent_room_cards::CardFacts;

/// How the shown owner is known, so a card never states a claim as fact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OwnerBasis {
    /// This account's own Agent, or this server's roster row: established.
    #[default]
    Listed,
    /// Only the Agent itself says so (its profile field or its run cards).
    StatedByAgent,
    /// The Agent's claims name different owners: none is shown.
    Conflicting,
}

/// What the Agent's profile publishes merged into its run cards: the owner
/// as [`with_profile_owner`], and the profile's runtime (set at each start)
/// before the newest card's. The runtime is display-only.
pub fn with_profile(mut cards: CardFacts, profile: super::agent_profile_owner::Published) -> CardFacts {
    cards.runtime = profile.runtime.or(cards.runtime);
    with_profile_owner(cards, profile.owner)
}

/// The Agent's two self-claims of its owner merged: its profile field and its
/// run cards. Where they name different people, neither is shown.
pub fn with_profile_owner(mut cards: CardFacts, profile: Option<OwnedUserId>) -> CardFacts {
    let Some(profile) = profile else { return cards };
    if cards.owner.as_ref().is_some_and(|owner| *owner != profile) { cards.owner_conflict = true; }
    cards.owner = (!cards.owner_conflict).then_some(profile);
    cards
}

/// How the owner of another owner's Agent is known: a roster row (`listed`)
/// establishes it; otherwise it is the Agent's word, or conflicting.
pub fn owner_basis(listed: bool, cards: &CardFacts) -> OwnerBasis {
    match (listed, cards.owner_conflict) {
        (true, _) => OwnerBasis::Listed,
        (false, true) => OwnerBasis::Conflicting,
        (false, false) if cards.owner.is_some() => OwnerBasis::StatedByAgent,
        (false, false) => OwnerBasis::Listed,
    }
}
