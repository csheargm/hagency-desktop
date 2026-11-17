//! The review loop (ADR 0016), run by the poller for each repository where
//! this device's Agent is a reviewer or maintainer: for every ready pull
//! request someone else opened, ask the Agent to review it once per head;
//! as maintainer, once an approval qualifies that exact head under the merge
//! gate, tell it to run the pinned merge (or say a maintainer or the owner is
//! needed). The gate still judges the merge
//! command itself; this only removes the typed prompts. Every step is checked
//! against Stop All right before it posts, and recorded once per head after.
use super::loop_notice::{LoopNotice, Target};
use ::hagency::issue_watch::PullRequest;
use ::hagency::issue_watch::CAN_MERGE;
use matrix_sdk::ruma::OwnedUserId;
use serde_json::{Value, json};

/// Posts notices and talks to this device's owner API (`review` part).
pub(crate) trait LoopIo {
    async fn post(&self, notice: &LoopNotice) -> Result<(), String>;
    /// One `review` change; answers the reply's `review` part.
    async fn review(&self, change: Value) -> Result<Value, String>;
    fn paused(&self) -> bool;
}

/// Who acts on one pull request and what this owner's GitHub allows.
pub(crate) struct LoopCtx<'a> {
    /// This device's own Agent: it reviews, and (maintainer) merges.
    pub agent: &'a OwnedUserId,
    /// Maintainer role: carry an approved head on to the merge.
    pub maintainer: bool,
    /// This owner's own GitHub permission on the repository, when read.
    pub permission: Option<&'a str>,
}

/// Whether this owner's own GitHub login may try a merge (GitHub decides).
pub(crate) fn can_merge(permission: Option<&str>) -> bool {
    permission.is_some_and(|p| CAN_MERGE.contains(&p))
}

/// Shown when Stop All paused the Agent during the loop.
const PAUSED: &str = "Agent paused by the owner: the review loop waits until it is resumed";

fn fence(io: &impl LoopIo) -> Result<(), String> {
    if io.paused() { Err(crate::i18n::tr(PAUSED).to_owned()) } else { Ok(()) }
}

pub(crate) fn target_of(repo: &str, pr: &PullRequest) -> Target {
    Target { repo: repo.to_owned(), number: pr.number, url: pr.url.clone(), head: pr.head_sha.clone() }
}

/// Advance one pull request head by at most one step.
pub(crate) async fn advance(io: &impl LoopIo, target: &Target, ctx: &LoopCtx<'_>) -> Result<(), String> {
    let step = |name: &str| json!({"step": name, "pr": target.url, "head": target.head});
    let taken = |reply: Value| reply["taken"] == true;
    fence(io)?;
    if !taken(io.review(json!({"stepTaken": step("review")})).await?) {
        return post_then_claim(io, &LoopNotice::ReviewRequested { target: target.clone(), reviewer: ctx.agent.clone() }, step("review")).await;
    }
    if !ctx.maintainer {
        return Ok(());
    }
    advance_merge(io, target, ctx).await
}

/// The merge step alone: for the owner's own pull request, which this
/// owner's Agent never reviews (another account's approval stands in).
pub(crate) async fn advance_merge(io: &impl LoopIo, target: &Target, ctx: &LoopCtx<'_>) -> Result<(), String> {
    let step = |name: &str| json!({"step": name, "pr": target.url, "head": target.head});
    let taken = |reply: Value| reply["taken"] == true;
    fence(io)?;
    if taken(io.review(json!({"stepTaken": step("merge")})).await?) {
        return Ok(());
    }
    let readiness = io.review(json!({"readiness": {"repo": target.repo, "number": target.number}})).await?["readiness"].clone();
    let Some(notice) = merge_notice(target, &readiness, ctx) else { return Ok(()) };
    // A hold has its own once-per-head step, so it never takes `merge`
    // and the merge is still told once the owner fixes it (GPT review).
    if notice.step() != "merge" && taken(io.review(json!({"stepTaken": step(notice.step())})).await?) {
        return Ok(());
    }
    post_then_claim(io, &notice, step(notice.step())).await
}

/// The merge step's notice, or `None` to wait (CI running, no review yet, or the head moved).
pub(crate) fn merge_notice(target: &Target, readiness: &Value, ctx: &LoopCtx<'_>) -> Option<LoopNotice> {
    // Unreadable facts or a moved head: try again next check.
    if readiness["head"].as_str() != Some(target.head.as_str()) {
        return None;
    }
    let target = target.clone();
    match readiness["decision"].as_str() {
        // The gate never answers a merge with the switch off (or unknown): say so instead (GPT review).
        Some("merge") if readiness["autoMerge"] != true => Some(super::merge_hold::merges_off(&target)),
        Some("merge") if can_merge(ctx.permission) => Some(LoopNotice::Approved { target, fixer: ctx.agent.clone() }),
        Some("merge") => Some(LoopNotice::NeedsMaintainer { target }),
        Some("wait") => None,
        _ if readiness["approvers"].as_u64() == Some(0) => None,
        _ => Some(LoopNotice::NeedsOwner { target, reason: readiness["reason"].as_str().unwrap_or("the merge could not be judged").to_owned() }),
    }
}

/// Post first (a fixed transaction id makes a retry a duplicate the server
/// drops), then record the step so it is not posted again for this head.
pub(crate) async fn post_then_claim(io: &impl LoopIo, notice: &LoopNotice, step: Value) -> Result<(), String> {
    io.post(notice).await?;
    fence(io)?;
    io.review(json!({"claimStep": step})).await.map(|_| ())
}
