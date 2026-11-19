//! The review step of one pull request head for its picked reviewer (live
//! PRs #119–#121, Nov 18, dev room at Assist): once the room was told the
//! head "was reviewed by bird-codex (bird's agent) and is ready … a person
//! merges it" (the `hold-autonomy-assist` step), the loop still judged the
//! review step, said it "could not start", and asked bird-claude to review
//! the same heads again. A head the Assist notice settled is now left alone
//! until it changes or the room's level does. And another owner's Agent
//! (bird-codex) runs on its owner's device, which this one never sees: it
//! is judged by its verdict on GitHub, never by "no run reached this device".
use super::super::loop_notice::{LoopNotice, Target};
use super::super::reviewer_pick::{Peers, Pick};
use super::super::work::Worker;
use super::{LoopCtx, LoopIo, post_then_claim, readiness_ask};
use ::hagency::issue_watch::safeguards::room_autonomy::ASSIST_REASON;
use ::hagency::issue_watch::{APPROVE_VERDICT, GITHUB_API, fetch_reviews};
use matrix_sdk::ruma::OwnedRoomId;
use serde_json::{Value, json};

/// What the review step needs to know of another owner's reviewer.
pub(crate) trait PeerFacts {
    /// Whether GitHub holds a verdict (approve or request changes) of `target`'s head.
    async fn answered(&self, target: &Target) -> Result<bool, String>;
    /// The reviewer as the room reads it: "bird-codex (bird's agent)".
    async fn label(&self, peer: &Pick) -> String;
}

/// GitHub's review states that answer a review request of a head.
const VERDICTS: [&str; 4] = [APPROVE_VERDICT, "APPROVED", "UNBOUND", "CHANGES_REQUESTED"];

/// Whether `reviews` (`(login, state, commit)`) hold a verdict of `head`.
pub(crate) fn verdict_of(reviews: &[(String, String, String)], head: &str) -> bool {
    reviews.iter().any(|(_, state, commit)| commit == head && VERDICTS.contains(&state.as_str()))
}

/// The live facts: GitHub's reviews with the owner's token, the room's names.
pub(crate) struct LiveFacts<'a> {
    pub worker: &'a Worker<'a>,
    pub peers: &'a Peers,
    pub room: &'a OwnedRoomId,
}

impl PeerFacts for LiveFacts<'_> {
    async fn answered(&self, target: &Target) -> Result<bool, String> {
        let reviews = fetch_reviews(self.worker.client, GITHUB_API, &target.repo, target.number, self.worker.token).await.map_err(|e| e.to_string())?;
        Ok(verdict_of(&reviews, &target.head))
    }
    async fn label(&self, peer: &Pick) -> String {
        let reviewer = super::super::approval_note::reviewer_of(self.peers, Some(peer), self.room).await;
        reviewer.map_or_else(|| peer.reviewer.localpart().to_owned(), |r| r.label())
    }
}

/// Whether `readiness` still leaves only a person (the room's Assist level) for `head`.
fn still_assist(readiness: &Value, head: &str) -> bool {
    let only_level = readiness["decision"] == "human" && readiness["reason"] == ASSIST_REASON;
    readiness["head"].as_str() == Some(head) && (only_level || super::super::approval_note::only_assist_left(readiness))
}

/// Whether `target`'s head is settled for the loop: the Assist notice was
/// posted for it and the room's level still leaves only a person. The gate
/// is read only once that notice is known.
pub(crate) async fn settled(io: &impl LoopIo, target: &Target) -> Result<bool, String> {
    let key = json!({"step": super::super::merge_hold::assist_step(), "pr": target.url, "head": target.head});
    if io.review(json!({"stepTaken": key})).await?["taken"] != true {
        return Ok(false);
    }
    let readiness = io.review(readiness_ask(io, &target.repo, target.number)).await?["readiness"].clone();
    Ok(still_assist(&readiness, &target.head))
}

/// Advance `target`'s review step for `peer`, unless the head is settled;
/// `true` when it is (nobody is asked to review it again this check).
pub(crate) async fn advance(io: &impl LoopIo, target: &Target, peer: Option<&Pick>, facts: &impl PeerFacts) -> Result<bool, String> {
    let Some(peer) = peer else { return Ok(false) };
    if settled(io, target).await? {
        return Ok(true);
    }
    if peer.own {
        super::advance(io, target, &LoopCtx { agent: &peer.reviewer, name: "", maintainer: false, permission: None }).await?;
    } else {
        advance_remote(io, target, peer, facts).await?;
    }
    Ok(false)
}

/// The review step for another owner's Agent: asked once per head; once its
/// verdict of the head is on GitHub it answered (the merge step judges it);
/// else the ledger says once that it has not answered and passes it over.
async fn advance_remote(io: &impl LoopIo, target: &Target, peer: &Pick, facts: &impl PeerFacts) -> Result<(), String> {
    super::fence(io)?;
    let key = json!({"step": "review", "pr": target.url, "head": target.head});
    if io.review(json!({"stepTaken": key.clone()})).await?["taken"] == true && facts.answered(target).await? {
        return Ok(());
    }
    let label = facts.label(peer).await;
    let due = super::super::step_resend::due_remote(io, target, &peer.reviewer, &label).await?;
    if due.taken {
        return Ok(());
    }
    post_then_claim(io, &LoopNotice::ReviewRequested { target: target.clone(), reviewer: peer.reviewer.clone() }, key, due.again).await
}

#[cfg(test)]
#[path = "peer_review_tests.rs"]
mod tests;
