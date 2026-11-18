//! A reviewer verdict GitHub shows but this device never recorded (live PR
//! #105, Nov 18: bird-codex's "Hagency verdict: APPROVE <head>" is on GitHub,
//! no approval is in the ledger, and the loop held the pull request for a
//! person). Before the merge step holds, the loop asks the same reviewer
//! again, a bounded number of times per head and spaced out, so a verdict
//! the gate missed once is posted again through it. Only after that does
//! the hold notice ask a person. And a draft whose fixer's run ended
//! without `gh pr ready` (live PR #104) would never be reviewed: the fixer
//! is asked once per head to mark it ready.
use super::loop_notice::{LoopNotice, Target};
use super::review_loop::{LoopIo, post_then_claim};
use ::hagency::issue_watch::APPROVE_VERDICT;
use matrix_sdk::ruma::OwnedUserId;
use serde_json::{Value, json};

/// One once-per-head step per re-request (hold steps: the owner API keeps them once per head).
pub const REREVIEW_STEPS: [&str; 2] = ["hold-rereview-one", "hold-rereview-two"];
/// The least time between two re-requests of one head, and after the last one before the hold.
pub const REREVIEW_WAIT_SECS: i64 = 10 * 60;
/// Re-requests post as new messages, apart from `step_resend`'s re-sends (`again` 1..=MAX_RESENDS).
const AGAIN_BASE: u32 = 100;

/// What the re-request step did this check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Again {
    /// The reviewer was asked again just now.
    Asked,
    /// Asked recently: wait for its verdict.
    Waiting,
    /// Asked the most times already: the merge step may hold.
    Exhausted,
}

/// Whether GitHub holds an approving verdict of exactly `head` while the
/// gate, judging that same head, counts no approval at all.
pub fn unrecorded_verdict(readiness: &Value, reviews: &[(String, String, String)], head: &str) -> bool {
    readiness["kind"] == "approvals"
        && readiness["approvers"].as_u64() == Some(0)
        && readiness["head"].as_str() == Some(head)
        && reviews.iter().any(|(_, state, commit)| state == APPROVE_VERDICT && commit == head)
}

/// Ask `reviewer` to review `target` again, unless it was asked in the last
/// [`REREVIEW_WAIT_SECS`] (`since_last`: seconds since this process last
/// asked for this head) or every [`REREVIEW_STEPS`] step is taken.
pub async fn ask_again(io: &impl LoopIo, target: &Target, reviewer: &OwnedUserId, since_last: Option<i64>) -> Result<Again, String> {
    let recent = since_last.is_some_and(|secs| secs < REREVIEW_WAIT_SECS);
    for (n, step) in (0u32..).zip(REREVIEW_STEPS) {
        let key = json!({"step": step, "pr": target.url, "head": target.head});
        if io.review(json!({"stepTaken": key.clone()})).await?["taken"] == true {
            continue;
        }
        if recent {
            return Ok(Again::Waiting);
        }
        let notice = LoopNotice::ReviewRequested { target: target.clone(), reviewer: reviewer.clone() };
        post_then_claim(io, &notice, key, AGAIN_BASE + n).await?;
        return Ok(Again::Asked);
    }
    Ok(if recent { Again::Waiting } else { Again::Exhausted })
}

/// When this process last asked again for each pull request head (by URL).
/// In memory: a restart may ask once sooner, never more than the steps allow.
fn lock_asked() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, (String, i64)>> {
    static ASKED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, (String, i64)>>> = std::sync::LazyLock::new(Default::default);
    // Plain timestamps: still usable after a panic elsewhere held the lock.
    ASKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The merge step's re-request for `target` at `now`: true when the step
/// waits on the reviewer (asked now or recently) instead of holding.
pub async fn asks_again(io: &impl LoopIo, target: &Target, (readiness, reviews): (&Value, &[(String, String, String)]), reviewer: &OwnedUserId, now: i64) -> Result<bool, String> {
    if !unrecorded_verdict(readiness, reviews, &target.head) {
        return Ok(false);
    }
    let since = lock_asked().get(&target.url).filter(|(head, _)| *head == target.head).map(|(_, at)| now - at);
    let again = ask_again(io, target, reviewer, since).await?;
    if again == Again::Asked {
        lock_asked().insert(target.url.clone(), (target.head.clone(), now));
    }
    Ok(again != Again::Exhausted)
}

/// A draft whose run never reads as ended is asked to be readied after this long.
pub const DRAFT_WAIT_SECS: i64 = 30 * 60;

/// A draft nobody will ready: the fixer's run ended (or the draft is
/// [`DRAFT_WAIT_SECS`] old), so no review can ever start unless the Agent
/// marks it ready. An unreadable run waits for the deadline, never fails the step.
pub(super) async fn ready_draft(io: &impl super::tracker::TrackIo, track: &::hagency::issue_watch::issue_track::Track, pull: &::hagency::issue_watch::PullRequest, now: i64) -> Result<(), String> {
    let ended = io.run_end(track).await.is_ok_and(|view| view.run.ended());
    if ended || now - pull.created_at >= DRAFT_WAIT_SECS {
        io.ask_ready(pull).await?;
    }
    Ok(())
}

/// The "still a draft" notice for `fixer` (display name `name`), once per head.
pub async fn ask_ready(io: &impl LoopIo, target: &Target, fixer: &OwnedUserId, name: &str) -> Result<(), String> {
    let notice = LoopNotice::MarkReady { target: target.clone(), fixer: fixer.clone(), name: name.to_owned() };
    let key = json!({"step": notice.step(), "pr": target.url, "head": target.head});
    if io.review(json!({"stepTaken": key.clone()})).await?["taken"] == true {
        return Ok(());
    }
    post_then_claim(io, &notice, key, 0).await
}

#[cfg(test)]
#[path = "review_again_tests.rs"]
mod tests;
