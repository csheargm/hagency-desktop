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

/// One once-per-head step per re-request (hold steps: the owner API keeps them
/// once per head). One only: a reviewer that answered on GitHub is asked once
/// more, never in a loop (live PR #115 got three identical verdicts).
pub const REREVIEW_STEPS: [&str; 1] = ["hold-rereview-one"];
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

/// Whether GitHub holds an approving verdict of exactly `head`, posted by
/// `login` (the GitHub account this owner's Agents post as), while the gate,
/// judging that same head, counts no approval at all and the ledger recorded
/// none (`recorded`; live PR #109: a recorded review was asked for again).
pub fn unrecorded_verdict(readiness: &Value, (reviews, login): (&[(String, String, String)], &str), head: &str) -> bool {
    readiness["kind"] == "approvals"
        && readiness["approvers"].as_u64() == Some(0)
        && readiness["recorded"].as_u64().unwrap_or(0) == 0
        && readiness["head"].as_str() == Some(head)
        && reviews.iter().any(|(who, state, commit)| who.eq_ignore_ascii_case(login) && state == APPROVE_VERDICT && commit == head)
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
/// `login` is the GitHub account this owner's Agents post as.
pub async fn asks_again(io: &impl LoopIo, target: &Target, (readiness, reviews, login): (&Value, &[(String, String, String)], &str), reviewer: &OwnedUserId, now: i64) -> Result<bool, String> {
    if !unrecorded_verdict(readiness, (reviews, login), &target.head) {
        return Ok(false);
    }
    let since = lock_asked().get(&target.url).filter(|(head, _)| *head == target.head).map(|(_, at)| now - at);
    let again = ask_again(io, target, reviewer, since).await?;
    // Every request already sent before a restart: its wait starts now (GPT review).
    if again == Again::Asked || (again == Again::Exhausted && since.is_none()) {
        lock_asked().insert(target.url.clone(), (target.head.clone(), now));
        return Ok(true);
    }
    Ok(again != Again::Exhausted)
}

/// A draft whose run never reads as ended is asked to be readied after this long.
pub const DRAFT_WAIT_SECS: i64 = 30 * 60;

/// The head each pull request was last asked to be readied at, and when (by
/// URL). In memory: a restart forgets it, so a head moved just before then is reviewed at once.
fn lock_ready() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, (String, i64)>> {
    static READY: std::sync::LazyLock<std::sync::Mutex<std::collections::HashMap<String, (String, i64)>>> = std::sync::LazyLock::new(Default::default);
    // Plain heads and timestamps: still usable after a panic elsewhere held the lock.
    READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// When each moved head was first seen held ([`ready_moved`]); apart from the
/// merge step's own clock, whose review wait must start only once the review is asked.
fn lock_moved() -> std::sync::MutexGuard<'static, super::merge_hold::HeadClock> {
    static MOVED: std::sync::LazyLock<std::sync::Mutex<super::merge_hold::HeadClock>> = std::sync::LazyLock::new(Default::default);
    MOVED.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Whether ready pull request `url` (not a draft) is at another head than
/// the one its fixer was asked to ready (round-2 GPT review: a request run
/// late readies whatever was pushed since). Ready at the asked head: the
/// request is done and forgotten.
pub(super) fn ready_moved(url: &str, head: &str, draft: bool) -> bool {
    let mut asked = lock_ready();
    match asked.get(url) {
        Some((at, _)) if at == head => {
            if !draft {
                asked.remove(url);
            }
            false
        }
        Some(_) => !draft,
        None => false,
    }
}

/// A draft nobody will ready: the fixer's run ended (or the draft is
/// [`DRAFT_WAIT_SECS`] old), so no review can ever start unless the Agent
/// marks it ready. An unreadable run waits for the deadline, never fails the
/// step. A pull request readied at a moved head ([`ready_moved`]) waits the
/// same way before its review starts: the fixer's run may still be pushing.
pub(super) async fn ready_draft(io: &impl super::tracker::TrackIo, track: &::hagency::issue_watch::issue_track::Track, pull: &::hagency::issue_watch::PullRequest, now: i64) -> Result<(), String> {
    let ended = io.run_end(track).await.is_ok_and(|view| view.run.ended());
    if !pull.draft {
        let seen = lock_moved().seen_at(&pull.url, &pull.head_sha, now);
        if ended || now - seen >= DRAFT_WAIT_SECS {
            lock_ready().remove(&pull.url);
        }
        return Ok(());
    }
    if ended || now - pull.created_at >= DRAFT_WAIT_SECS {
        io.ask_ready(pull).await?;
        let mut asked = lock_ready();
        let entry = asked.entry(pull.url.clone()).or_insert_with(|| (pull.head_sha.clone(), now));
        if entry.0 != pull.head_sha {
            *entry = (pull.head_sha.clone(), now);
        }
    }
    Ok(())
}

/// The "still a draft" notice for `fixer` (display name `name`), once per
/// head, given back like a merge when its dispatch never ran (`step_resend`).
pub async fn ask_ready(io: &impl LoopIo, target: &Target, fixer: &OwnedUserId, name: &str) -> Result<(), String> {
    let notice = LoopNotice::MarkReady { target: target.clone(), fixer: fixer.clone(), name: name.to_owned() };
    let due = super::step_resend::due(io, target, notice.step(), notice.mention()).await?;
    if due.taken {
        return Ok(());
    }
    let key = json!({"step": notice.step(), "pr": target.url, "head": target.head});
    post_then_claim(io, &notice, key, due.again).await
}

/// Said in the hold line when the reviewer's verdict of this head is on
/// GitHub but was never recorded, after it was asked again (live PR #115).
pub const UNRECORDED_VERDICT: &str = "a \"Hagency verdict: APPROVE\" of this head is on GitHub, but Hagency did not record it (it was not posted through the merge gate), so it does not count";

/// `readiness` for the hold line: an unrecorded GitHub verdict of `head`
/// ([`unrecorded_verdict`]) is named plainly as what is missing.
pub fn noted_unrecorded(mut readiness: Value, (reviews, login): (&[(String, String, String)], &str), head: &str) -> Value {
    if unrecorded_verdict(&readiness, (reviews, login), head) {
        let reason = readiness["reason"].as_str().unwrap_or("no approval").to_owned();
        readiness["reason"] = format!("{reason}; {}", crate::i18n::tr(UNRECORDED_VERDICT)).into();
    }
    readiness
}

/// Said in the hold line when the reviewer is another owner's Agent: its
/// GitHub login is not known here, so only this owner's login's verdict is
/// matched (round-2 GPT review). It claims no verdict and no retry history.
pub const OTHER_OWNER_VERDICT: &str = "the reviewer is another owner's Agent, whose GitHub login this device does not know, so a verdict it may have posted on GitHub cannot be matched here";

/// `readiness` for the hold line: a missing approval from `other_owner`'s
/// reviewer says why no unrecorded GitHub verdict was asked for again.
pub fn noted_other_owner(mut readiness: Value, other_owner: bool) -> Value {
    if other_owner && readiness["kind"] == "approvals" && readiness["approvers"].as_u64() == Some(0) {
        let reason = readiness["reason"].as_str().unwrap_or("no approval").to_owned();
        readiness["reason"] = format!("{reason}; {}", crate::i18n::tr(OTHER_OWNER_VERDICT)).into();
    }
    readiness
}

#[cfg(test)]
#[path = "review_again_tests.rs"]
mod tests;
