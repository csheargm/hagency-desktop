//! A `watched_repo_step` call that nothing lets run card-free asks the
//! Agent's owner on a card, exactly as a Claude `Bash` call does, when it is
//! a review verdict on a repository of this room (e2e scenario 8, Nov 18,
//! 18:07: bird-codex refused Anna's review outright, while bird-claude's
//! owner got a card for the same request) or one read-only GitHub query
//! ([`read_only::named_repo`]) of a repository outside the room's card-free
//! set (e2e scenario 8, Nov 18, 20:06: refused here, the model retried the
//! read in its own shell, which has no network, so one approved card ran
//! nothing and five cards followed for one read). Either runs as the pinned
//! command ([`ReviewPost::command`], [`read_only::command`]), never the
//! model's own text, so the owner approves exactly what runs. Any other step
//! stays refused.
//!
//! The card waits off the turn's loop ([`Running::cards`]); the loop takes
//! each answer ([`decided`]), records the approval in the ledger and runs
//! the step. A turn that ends drops the waits, and with them the cards.
use super::repo_steps::{Call, Launch, Running, Turn, launch, refuse};
use crate::merge_policy::{gate, github::PrSource};
use crate::watch_trust::{TrustedStep, read_only, review_post::ReviewPost};
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
    /// The pull request whose earlier approval a posted request for changes withdraws.
    withdraws: Option<String>,
    /// The verdict as checked (`None`: a read): an approval posts only on the head it names.
    post: Option<ReviewPost>,
}

/// Ask the owner about `call` (a step nothing lets run card-free), or
/// refuse it at once: only a review verdict on a repository of this room or
/// a read-only query asks, only where a card can be shown and the owner's
/// rules do not refuse it.
pub(super) fn ask(ledger: &mut Ledger, turn: &Turn<'_>, call: Call, proposal: ToolProposal, cards: &mut tokio::task::JoinSet<Decided>) {
    let (step, post, withdraws) = match card_step(ledger, &proposal, &call.command) {
        Ok(found) => found,
        Err(why) => return refuse(call, why),
    };
    let card = match ledger.tool_disposition(&proposal, super::now()) {
        Ok(ToolPolicy::AskOwner) => true,
        // A saved rule naming this tool and workspace runs it, as for Claude
        // (its approval would never be spent: GPT review P2).
        Ok(ToolPolicy::AllowWithRules { .. }) => false,
        // The ledger says Deny by refusing the check itself.
        Ok(_) | Err(crate::Error::Denied) => return refuse(call, "your owner's rules refuse this step here"),
        Err(_) => return refuse(call, "this step cannot be checked now"),
    };
    if !card {
        cards.spawn(std::future::ready(Decided { call, proposal, step, card, allowed: true, withdraws, post }));
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
        Decided { call, proposal, step, card, allowed, withdraws, post }
    });
}

/// What a card for `command` would run: the pinned step, the verdict (for a
/// review) and the pull request whose earlier approval a request for changes
/// withdraws; `Err` is why it is refused without a card.
fn card_step(ledger: &Ledger, proposal: &ToolProposal, command: &str) -> Result<(TrustedStep, Option<ReviewPost>, Option<String>), &'static str> {
    let Some(post) = ReviewPost::parse(command) else {
        // Never a write: only the closed read-only shapes, run pinned.
        return match read_only::named_repo(command) {
            Some(_) => Ok((TrustedStep::Command(read_only::command(command)), None, None)),
            None => Err("neither a read of this room's repositories nor this room's work loop covers this step here"),
        };
    };
    // Only a repository of this room, as the room's own grants (GPT review):
    // a saved rule must never post to any repository the token reaches.
    let repo = crate::watch_trust::room_repos(&ledger.db, proposal).ok().and_then(|repos| repos.into_iter().find(|r| r.eq_ignore_ascii_case(&post.repo)));
    let Some(repo) = repo else { return Err("this repository is not one of this room's") };
    // A request for changes withdraws this Agent's earlier approval once it
    // posted, as the merge gate's does (GPT review); keyed by the room's spelling.
    let withdraws = (post.mode == "--request-changes").then(|| format!("https://github.com/{repo}/pull/{}", post.number));
    Ok((TrustedStep::Command(post.command()), Some(post), withdraws))
}

/// Take one answered card off the turn's waits (`None`: none waits, or the
/// wait was stopped).
pub(super) async fn next(running: &mut Running) -> Option<Decided> {
    running.cards.join_next().await?.ok()
}

/// Act on a card's answer: refused, or the approval recorded in the ledger
/// (it must still validate: the dispatch's rules may have changed meanwhile)
/// and the pinned step run.
pub(super) async fn decided(ledger: &mut Ledger, turn: &Turn<'_>, answer: Decided, running: &mut Running) {
    let github = crate::merge_policy::github::GhCli { token: turn.token.clone() };
    decided_with(ledger, turn, answer, running, &github).await
}

/// [`decided`], reading the pull request's head through `source`.
pub(super) async fn decided_with(ledger: &mut Ledger, turn: &Turn<'_>, answer: Decided, running: &mut Running, source: &impl PrSource) {
    let Decided { call, proposal, step, card, allowed, withdraws, post } = answer;
    if call.reply.is_closed() {
        return;
    }
    if !allowed {
        return refuse(call, "your owner did not approve this step");
    }
    // An approval posts only while the head is the commit its verdict names,
    // as the merge gate's own (GPT batch review P2-3): a push during the
    // card's wait must not land it on code nobody reviewed. A push between
    // this read and the post cannot move it: the post is pinned to the
    // commit its body names ([`ReviewPost::command`], round-2 GPT review P2-3).
    if let Some(post) = &post
        && let Err(why) = head_still_reviewed(post, turn, source).await
    {
        return refuse(call, why);
    }
    if call.reply.is_closed() {
        return;
    }
    // The slot first: an approval is never spent on a step that cannot start.
    let Ok(slot) = running.slots.clone().try_acquire_owned() else { return refuse(call, "another step is still running; send it again") };
    let now = super::now();
    let authorized = if card { ledger.approve_tool(turn.owner, &proposal, now).and_then(|_| ledger.authorize_tool(&proposal, now)) } else { ledger.authorize_tool(&proposal, now) };
    if authorized.is_err() {
        return refuse(call, "your owner's approval no longer holds for this step");
    }
    let key = running.key(turn.dispatch);
    // Held under the step's key: settled with its outcome ([`Running::settle`]).
    if let Some(pr) = withdraws
        && let Err(error) = ledger.hold_withdrawal(&key, &pr, &proposal.scope.agent)
    {
        return refuse(call, &format!("the request for changes cannot be recorded: {error}"));
    }
    launch(ledger, turn, Launch { call, step, proposal, key, slot }, running);
}

/// `Ok` unless `post` approves and the pull request's head is not the
/// commit its body names ([`gate::review_names_head`]), or cannot be read.
async fn head_still_reviewed(post: &ReviewPost, turn: &Turn<'_>, source: &impl PrSource) -> Result<(), &'static str> {
    if post.mode != "--approve" && !post.body.starts_with(gate::APPROVE_MARKER) {
        return Ok(());
    }
    let command = gate::GhPr { dir: turn.workspace.into(), number: post.number, repo: Some(post.repo.clone()), head: None, kind: gate::Kind::Review, body: post.body.clone() };
    match source.pull_request(&command).await {
        Some(view) if gate::review_names_head(&post.body, &view.head_sha) => Ok(()),
        Some(_) => Err("the pull request has a new commit since this verdict: review its current head again"),
        None => Err("the pull request cannot be read now to check its head"),
    }
}

#[cfg(test)]
#[path = "repo_cards_tests.rs"]
mod tests;
