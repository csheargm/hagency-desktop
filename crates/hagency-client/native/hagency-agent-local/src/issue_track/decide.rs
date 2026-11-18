//! The issue loop's state machine step (ADR 0020): given one record and
//! what GitHub and the Activity record say now, the single next action.
use super::{Facts, HANDOFF_TIMEOUT_SECS, IssueFact, MAX_ATTEMPTS, MAX_FREE_RETRIES, State, Track};

/// The shortest time between two hand-offs of one issue after its run
/// ended early (Nov 18: #90-#92 were re-handed 50 s apart, three times in
/// three minutes, while every run was refused before the model started).
pub const REHAND_BACKOFF_SECS: i64 = 60;

/// The one step the poller takes for a record this check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Wait,
    /// Hand the issue off again as attempt `attempt`.
    Retry { attempt: u32 },
    /// A restart interrupted the run, it waited on an unanswered approval
    /// card, or it ended with no model call: hand it off again, same attempt.
    Rehand,
    /// A hand-off is due but the Agent's token budget is used up: hand
    /// nothing off (it would be refused too), say so once, and wait.
    HoldBudget,
    /// Out of attempts: record `failed` and say so once.
    GiveUp,
    /// A linked pull request is open: request its review, or carry it to the merge gate.
    FollowPr { pr: u64 },
    /// Merged: record it and post "#N fixed → PR #M merged" once.
    AnnounceFixed,
    /// Closed without a merge: record it quietly.
    Close,
    /// The Agent gave up: record `needs_human` and say so once.
    NeedsHuman,
}

/// The deadline of the current attempt: the timeout doubles per attempt.
pub fn deadline(track: &Track) -> i64 {
    let doubling = 1i64 << track.attempts.saturating_sub(1).min(8);
    track.handed_at.saturating_add(HANDOFF_TIMEOUT_SECS.saturating_mul(doubling))
}

/// The next step for `track` given `facts`. Terminal records never act; a
/// closed issue wins over everything; an open pull request stops retries.
pub fn decide(track: &Track, facts: &Facts) -> Action {
    if track.state.is_terminal() {
        return Action::Wait;
    }
    match facts.issue {
        IssueFact::Done => return Action::AnnounceFixed,
        IssueFact::ClosedOther => return Action::Close,
        IssueFact::Open => {}
    }
    if facts.cannot_fix {
        return Action::NeedsHuman;
    }
    if let Some(pr) = facts.open_pr.or(track.pr.filter(|_| track.state != State::HandedOff)) {
        // A pull request seen once is followed until the issue closes; if it
        // was closed unmerged, the record waits for a person (no retry).
        return if facts.open_pr.is_some() { Action::FollowPr { pr } } else { Action::Wait };
    }
    let ended = facts.run_ended && track.state == State::HandedOff;
    let next = if ended && facts.no_run && facts.budget_out {
        // Refused for the budget, not run: never a give-up or a counted
        // attempt, whatever the caps (GPT review); resumes after the turn.
        Action::HoldBudget
    } else if ended {
        after_ended_run(track, facts)
    } else if facts.now < deadline(track) {
        Action::Wait
    } else {
        next_attempt(track)
    };
    match next {
        // Every hand-off would be refused before the model starts until the
        // budget allows again (Nov 18): hold instead of burning attempts.
        Action::Retry { .. } | Action::Rehand if facts.budget_out => Action::HoldBudget,
        next => next,
    }
}

/// A run that ended without a pull request. Never charged to the Agent,
/// and never the final give-up: a restart (Nov 17 22:35), a person who did
/// not answer its card (Nov 17 23:32), or no model call at all (Nov 18).
fn after_ended_run(track: &Track, facts: &Facts) -> Action {
    if facts.now < track.handed_at.saturating_add(REHAND_BACKOFF_SECS) {
        return Action::Wait;
    }
    let free = facts.interrupted || facts.unanswered || facts.no_run;
    if free && track.free_retries < MAX_FREE_RETRIES {
        return Action::Rehand;
    }
    next_attempt(track)
}

fn next_attempt(track: &Track) -> Action {
    if track.attempts >= MAX_ATTEMPTS {
        Action::GiveUp
    } else {
        Action::Retry { attempt: track.attempts + 1 }
    }
}
