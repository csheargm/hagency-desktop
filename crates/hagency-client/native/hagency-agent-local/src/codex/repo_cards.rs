//! A `watched_repo_step` review verdict that nothing lets post card-free
//! (no room work loop, no room review grant, a requester who is not the
//! owner) asks the Agent's owner on a card, exactly as a Claude `Bash` call
//! does (e2e scenario 8, Nov 18: bird-codex refused Anna's review outright,
//! while bird-claude's owner got a card for the same request). Only a review
//! verdict asks here: it runs as the pinned post ([`ReviewPost::command`]),
//! never the model's own text, so the owner approves exactly what runs. Any
//! other step stays refused (its own shell asks for local commands).
//!
//! The card waits off the turn's loop ([`Running::cards`]); the loop takes
//! each answer ([`decided`]), records the approval in the ledger and runs
//! the step. A turn that ends drops the waits, and with them the cards.
use super::repo_steps::{Call, Launch, Running, Turn, launch, refuse};
use crate::watch_trust::{TrustedStep, review_post::ReviewPost};
use crate::{Ledger, ToolPolicy, ToolProposal};
use std::time::Duration;

/// How long the owner's card waits: inside the proposal's own lifetime
/// (`repo_steps::PROPOSAL_LIFETIME_SECS`), so an approval still validates.
pub(super) const CARD_WAIT: Duration = Duration::from_secs(290);

/// A card's answer, back on the turn's loop.
pub(super) struct Decided {
    call: Call,
    proposal: ToolProposal,
    step: TrustedStep,
    /// The owner's card answered (false: a saved rule allowed it).
    card: bool,
    allowed: bool,
}

/// Ask the owner about `call` (a step nothing lets run card-free), or
/// refuse it at once: only a review verdict asks, only where a card can be
/// shown and the owner's rules ask rather than refuse.
pub(super) fn ask(ledger: &mut Ledger, turn: &Turn<'_>, call: Call, proposal: ToolProposal, cards: &mut tokio::task::JoinSet<Decided>) {
    let Some(post) = ReviewPost::parse(&call.command) else {
        return refuse(call, "neither a read of this room's repositories nor this room's work loop covers this step here");
    };
    let step = TrustedStep::Command(post.command());
    let card = match ledger.tool_disposition(&proposal, super::now()) {
        Ok(ToolPolicy::AskOwner) => true,
        Ok(ToolPolicy::AllowWithRules { .. }) => false,
        // The ledger says Deny by refusing the check itself.
        Ok(_) | Err(crate::Error::Denied) => return refuse(call, "your owner's rules refuse this step here"),
        Err(_) => return refuse(call, "this step cannot be checked now"),
    };
    if !card {
        cards.spawn(std::future::ready(Decided { call, proposal, step, card, allowed: true }));
        return;
    }
    let Some(queue) = turn.queue else { return refuse(call, "no card can be shown to your owner here") };
    let (pending, receiver) = super::pending_approval(proposal.clone());
    if !queue.offer(pending) {
        return refuse(call, "your owner's cards are full; try again later");
    }
    let waiting = queue.show_approval_wait();
    cards.spawn(async move {
        let _waiting = waiting;
        let mut call = call;
        let allowed = tokio::select! {
            answer = tokio::time::timeout(CARD_WAIT, receiver) => matches!(answer, Ok(Ok(true))),
            // Given up (the MCP wait expired): the card goes away unanswered.
            () = call.reply.closed() => false,
        };
        Decided { call, proposal, step, card, allowed }
    });
}

/// Take one answered card off the turn's waits (`None`: none waits, or the
/// wait was stopped).
pub(super) async fn next(running: &mut Running) -> Option<Decided> {
    running.cards.join_next().await?.ok()
}

/// Act on a card's answer: refused, or the approval recorded in the ledger
/// (it must still validate: the dispatch's rules may have changed meanwhile)
/// and the pinned step run.
pub(super) fn decided(ledger: &mut Ledger, turn: &Turn<'_>, answer: Decided, running: &mut Running) {
    let Decided { call, proposal, step, card, allowed } = answer;
    if call.reply.is_closed() {
        return;
    }
    if !allowed {
        return refuse(call, "your owner did not approve this step");
    }
    // The slot first: an approval is never spent on a step that cannot start.
    let Ok(slot) = running.slots.clone().try_acquire_owned() else { return refuse(call, "another step is still running; send it again") };
    let now = super::now();
    let authorized = if card { ledger.approve_tool(turn.owner, &proposal, now).and_then(|_| ledger.authorize_tool(&proposal, now)) } else { ledger.authorize_tool(&proposal, now) };
    if authorized.is_err() {
        return refuse(call, "your owner's approval no longer holds for this step");
    }
    let key = running.key(turn.dispatch);
    launch(ledger, turn, Launch { call, step, proposal, key, slot }, running);
}

#[cfg(test)]
#[path = "repo_cards_tests.rs"]
mod tests;
