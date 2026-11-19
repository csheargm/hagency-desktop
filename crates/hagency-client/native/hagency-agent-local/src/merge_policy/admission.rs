//! Whether the merge gate may answer a proposal at all, before it judges the
//! pull request (split from `gate.rs`, R4): the room's autonomy (ADR 0030),
//! the owner's layers and review grant (ADR 0025), and who may review.
use crate::{Ledger, ToolProposal};

/// Whether the room lets `proposal` skip the card at all: it runs, and a
/// merge or ready needs its Agent to work in this room's loop (ADR 0030; a
/// review is checked once the pull request is read: [`works_or_was_asked`]).
/// A member's request under a layer the owner never saved is the owner's
/// card (ADR 0025), and a member's review needs the owner's review grant there.
pub(super) fn admits(ledger: &Ledger, owner: &str, proposal: &ToolProposal, review: bool, now: i64) -> bool {
    let (agent, room) = (&proposal.scope.agent, &proposal.scope.room);
    let runs = if review { ledger.loop_may_review(room) } else { ledger.loop_may_work(agent, room) };
    let member = proposal.scope.requester != owner;
    runs.is_ok()
        && ledger.member_default_asks(proposal, now).is_ok_and(|asks| !asks)
        && (!review || !member || ledger.room_share(agent, room).is_ok_and(|share| share.review))
}

/// A review counts from an Agent that works in this room's loop, or from
/// one the owner's loop asked, in this very request, to review exactly this
/// pull request at this head (live PR #115, Nov 18: bird-codex reviews
/// there, only bird-claude works; its verdict was posted but never recorded).
pub(super) fn works_or_was_asked(ledger: &Ledger, proposal: &ToolProposal, view: &super::github::PrView) -> bool {
    let (agent, room) = (&proposal.scope.agent, &proposal.scope.room);
    if ledger.loop_may_work(agent, room).is_ok() {
        return true;
    }
    let Some(mxid) = crate::attribution::agent_mxid(agent) else { return false };
    proposal.scope.requester == ledger.owner && super::step_resend::asked_to_review(&ledger.db, &proposal.dispatch, (&view.url, &view.head_sha), &mxid).unwrap_or(false)
}
