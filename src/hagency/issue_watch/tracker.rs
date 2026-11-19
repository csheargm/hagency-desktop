//! The issue loop's follow-through (ADR 0020), run each check for one
//! repository: every tracked issue moves by at most one step of the state
//! machine in `::hagency::issue_watch::issue_track` (retry a lost hand-off,
//! follow the Agent's pull request to review and the merge gate, announce
//! the merge, give up, or stop for a person). Every room line is posted
//! before its state is recorded, with a transaction id fixed by issue, step
//! and attempt, so a check that dies in between repeats a duplicate the
//! homeserver drops. The I/O is a trait so the whole flow runs on fakes.
use super::merge_hold::Missing;
use super::run_end::{Again, RunEnd, RunView, branch_names_issue, hand_again, hold_for_budget};
use super::work_slots::closes_issue;
use ::hagency::issue_watch::issue_track::{Action, Facts, IssueFact, MAX_ATTEMPTS, State, Track, decide};
use ::hagency::issue_watch::{Issue, PullRequest};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(crate) use super::tracker_lines::{hold_of, terminal_line};
use super::tracker_lines::{pr_line, waiting_line};

/// GitHub's and this device's clocks may differ by this much (a cannot-fix
/// comment or pull request this much older than the hand-off still counts).
pub(super) const CLOCK_SLACK_SECS: i64 = 120;

/// What following a pull request led to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Followed {
    /// Another Agent was asked to review; its verdict is not in yet.
    Requested,
    /// Reviewed; CI is still running under a policy that requires it.
    WaitingCi,
    /// Every rule passes: the Agent was told to run the pinned merge.
    Merging,
    /// It cannot merge yet: one room notice said what is missing (ADR 0020).
    Held(Vec<Missing>),
    /// The merge step failed to reach GitHub this many checks in a row; retried next check.
    Unreachable(u32),
    /// No other Agent can review: a person must review and merge.
    NeedsPerson,
    /// Still a draft: the Agent has not marked it ready yet.
    Draft,
}

pub(crate) trait TrackIo {
    /// One `track` change in the owner API (`issue_track::TrackChange`).
    async fn record(&self, change: Value) -> Result<(), String>;
    /// One room line about issue `number`, keyed so the same `key` is never posted twice.
    async fn say(&self, number: i64, key: &str, text: String) -> Result<(), String>;
    /// Hand `issue` off again as `attempt` (claim, post, record `handed`),
    /// asking to open the pull request from `branch` when one was pushed;
    /// `again` > 0 re-posts the same attempt after an interrupted run (its
    /// own message, recorded `rehanded`). `Ok(false)` when another Agent now
    /// holds the issue's claim.
    async fn hand_off(&self, issue: &Issue, attempt: u32, branch: Option<&str>, again: u32) -> Result<bool, String>;
    /// Whether the current hand-off's run is over (its Activity record).
    async fn run_end(&self, track: &Track) -> Result<RunView, String>;
    /// The one branch the Agent pushed for issue `number`, if any.
    async fn pushed_branch(&self, number: i64) -> Result<Option<String>, String>;
    /// The Agent's name for room lines.
    fn agent_label(&self) -> String;
    /// Whether pull request `pr` was merged (GitHub's own answer): `Some(true)`
    /// when this owner's own GitHub login merged it.
    async fn pr_merged(&self, pr: u64) -> Result<Option<bool>, String>;
    /// The Agent's merged pull request linking `track`'s issue, opened during
    /// its current hand-off, for a record that never saw one open (opened and merged between two checks).
    async fn merged_linking_pr(&self, track: &Track) -> Result<Option<u64>, String>;
    /// A tracked issue missing from the open list: GitHub's own answer, and
    /// the issue itself (`None`: unreadable).
    async fn closed_state(&self, number: i64) -> Result<(IssueFact, Option<Issue>), String>;
    /// The Agent's cannot-fix comment during the current attempt.
    async fn cannot_fix(&self, track: &Track) -> Result<bool, String>;
    async fn follow_pr(&self, pr: &PullRequest) -> Result<Followed, String>;
    /// Ask the Agent once per head to mark its draft pull request ready (live PR #104).
    async fn ask_ready(&self, pr: &PullRequest) -> Result<(), String>;
    /// The room line saying the reviewer is a fallback ("No Codex reviewer
    /// in the room; asked anna-claude", ADR 0025), when it is one.
    fn reviewer_notice(&self) -> Option<String>;
    fn paused(&self) -> bool;
}

/// One repository's inputs, read once per check.
pub(crate) struct Inputs<'a> {
    pub repo: &'a str,
    pub now: i64,
    pub tracks: &'a [Track],
    /// Issue numbers already handed off (`issueSeen`), to adopt older hand-offs.
    pub seen: &'a HashSet<i64>,
    /// The work list entry's start: an issue opened before it is never adopted.
    pub since: i64,
    pub open_issues: &'a [Issue],
    /// The Agent's own open pull requests (its GitHub login's).
    pub own_pulls: &'a [PullRequest],
}

/// The status parts and failures of one pass.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Followups {
    pub lines: Vec<String>,
    pub errors: Vec<String>,
    /// Why a followed pull request waits, by issue (room-shareable, ADR 0023).
    pub holds: Vec<(i64, String)>,
}

pub(crate) async fn follow(io: &impl TrackIo, inputs: &Inputs<'_>) -> Followups {
    let mut out = Followups::default();
    // A hand-off from before tracking (or whose record was lost) is adopted
    // as attempt 1 from now, so it is followed and retried like a new one.
    // An open issue opened before the entry is neither adopted nor advanced (live #31, Nov 18).
    let current = |n: i64| inputs.open_issues.iter().any(|i| i.number == n && !i.is_pr && i.created_at >= inputs.since);
    let predates = |n: i64| !current(n) && inputs.open_issues.iter().any(|i| i.number == n);
    for number in inputs.seen.iter().filter(|n| current(**n)) {
        if !inputs.tracks.iter().any(|t| t.repo.eq_ignore_ascii_case(inputs.repo) && t.number == *number) {
            if let Err(error) = io.record(json!({"repo": inputs.repo, "number": number, "handed": 1})).await {
                out.errors.push(error);
            }
        }
    }
    for track in inputs.tracks.iter().filter(|t| t.repo.eq_ignore_ascii_case(inputs.repo) && !predates(t.number)) {
        match step(io, inputs, track, &mut out.holds).await {
            Ok(Some(line)) => out.lines.push(line),
            Ok(None) => {}
            Err(error) => out.errors.push(format!("#{}: {error}", track.number)),
        }
    }
    out
}

/// Advance one record by one step; the status part it shows afterwards.
async fn step(io: &impl TrackIo, inputs: &Inputs<'_>, track: &Track, holds: &mut Vec<(i64, String)>) -> Result<Option<String>, String> {
    if track.state.is_terminal() {
        return Ok(terminal_line(track));
    }
    let Some((fact, issue)) = issue_of(io, inputs, track.number).await? else { return Ok(None) };
    let issue = issue.as_ref();
    let open_pr = inputs.own_pulls.iter().find(|p| links_issue(p, inputs.repo, track.number));
    let cannot_fix = fact == IssueFact::Open && open_pr.is_none() && io.cannot_fix(track).await?;
    // A run that already ended without a pull request is retried now (e2e, Nov 17).
    // An unreadable run only falls back to the deadline (GPT review): never stuck.
    let RunView { run, budget_out } = if fact == IssueFact::Open && open_pr.is_none() && !cannot_fix && track.state == State::HandedOff {
        io.run_end(track).await.unwrap_or_else(|e| {
            makepad_widgets::log!("WARN issue_track_run_unreadable #{}: {e}", track.number);
            RunView::default()
        })
    } else {
        RunView::default()
    };
    let facts = Facts { now: inputs.now, issue: fact, open_pr: open_pr.map(|p| p.number), cannot_fix, run_ended: run.ended(), interrupted: run == RunEnd::Interrupted, unanswered: run == RunEnd::Unanswered, no_run: run == RunEnd::NoRun, budget_out };
    if io.paused() {
        return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
    }
    let n = track.number;
    // The record's own spelling: the ledger keys it exactly (GPT review).
    let change = |extra: Value| change_of(&track.repo, n, extra);
    match decide(track, &facts) {
        Action::Wait => Ok(Some(waiting_line(track))),
        Action::Retry { attempt } => hand_again(io, issue.ok_or("the issue is not open")?, track, run, Again::Attempt(attempt)).await,
        Action::Rehand => hand_again(io, issue.ok_or("the issue is not open")?, track, run, Again::Rehand).await,
        Action::HoldBudget => hold_for_budget(io, track).await,
        Action::GiveUp => {
            io.say(n, "failed", crate::i18n::format(GAVE_UP, &[("number", n.to_string()), ("max", MAX_ATTEMPTS.to_string())])).await?;
            io.record(change(json!({"state": "failed", "notified": true}))).await?;
            Ok(terminal_line(&Track { state: State::Failed, ..track.clone() }))
        }
        Action::NeedsHuman => {
            let url = issue.map(|i| i.url.as_str()).unwrap_or_default();
            io.say(n, "needs-human", crate::i18n::format(CANNOT_FIX, &[("number", n.to_string()), ("url", url.to_owned())])).await?;
            io.record(change(json!({"state": "needs_human", "notified": true}))).await?;
            Ok(terminal_line(&Track { state: State::NeedsHuman, ..track.clone() }))
        }
        Action::FollowPr { pr } => follow_one(io, (&track.repo, inputs.now), track, open_pr.ok_or("the pull request is gone")?, pr, holds).await.map(Some),
        Action::AnnounceFixed => announce(io, &track.repo, track).await.map(|_| None),
        Action::Close => io.record(change(json!({"state": "closed"}))).await.map(|_| None),
    }
}

/// The tracked issue as GitHub has it: from the open list, else looked up
/// alone (beyond the list's page cap, or closed). `None`: open but opened
/// before the work-list entry, or of unknown age, so never advanced (GPT batch review).
async fn issue_of(io: &impl TrackIo, inputs: &Inputs<'_>, number: i64) -> Result<Option<(IssueFact, Option<Issue>)>, String> {
    if let Some(open) = inputs.open_issues.iter().find(|i| i.number == number && !i.is_pr) {
        return Ok(Some((IssueFact::Open, Some(open.clone()))));
    }
    Ok(match io.closed_state(number).await? {
        (IssueFact::Open, issue) if issue.as_ref().map_or(i64::MIN, |i| i.created_at) < inputs.since => None,
        found => Some(found),
    })
}

pub(super) fn change_of(repo: &str, number: i64, extra: Value) -> Value {
    let mut change = json!({"repo": repo, "number": number});
    if let (Some(c), Some(extra)) = (change.as_object_mut(), extra.as_object()) {
        c.extend(extra.clone());
    }
    change
}

/// The Agent's pull request is open: record it, ask for its review (or the
/// merge gate), and tell a person once when no other Agent can review.
async fn follow_one(io: &impl TrackIo, (repo, now): (&str, i64), track: &Track, pull: &PullRequest, pr: u64, holds: &mut Vec<(i64, String)>) -> Result<String, String> {
    let n = track.number;
    if track.state == State::HandedOff || track.pr != Some(pr) {
        io.record(change_of(repo, n, json!({"state": "pr_opened", "pr": pr}))).await?;
    }
    let followed = io.follow_pr(pull).await?;
    if followed == Followed::Draft {
        super::review_again::ready_draft(io, track, pull, now).await?;
    }
    if let Some(hold) = hold_of(&followed) {
        holds.push((n, hold));
    }
    let told = track.state == State::Reviewed && track.notified && track.pr == Some(pr);
    if followed == Followed::NeedsPerson && !told {
        io.say(n, &format!("needs-review-{pr}"), crate::i18n::format(NEEDS_YOU, &[("pr", pr.to_string()), ("number", n.to_string()), ("url", pull.url.clone())])).await?;
        io.record(change_of(repo, n, json!({"state": "reviewed", "notified": true}))).await?;
    } else if matches!(followed, Followed::Held(_)) && !told {
        // The review loop posted the notice (once per head and set of missing things).
        io.record(change_of(repo, n, json!({"state": "reviewed", "notified": true}))).await?;
    } else if matches!(followed, Followed::Requested | Followed::WaitingCi | Followed::Merging) && (track.state != State::Reviewed || track.pr != Some(pr)) {
        // Said once, when the review is first asked for, before it is recorded.
        if followed == Followed::Requested && let Some(text) = io.reviewer_notice() {
            io.say(n, &format!("reviewer-fallback-{pr}"), text).await?;
        }
        io.record(change_of(repo, n, json!({"state": "reviewed"}))).await?;
    }
    Ok(pr_line(n, pr, followed))
}

async fn announce(io: &impl TrackIo, repo: &str, track: &Track) -> Result<(), String> {
    let n = track.number;
    // Only a pull request GitHub reports merged is announced as the fix (GPT
    // review); one merged before any check saw it open is looked up (integration GPT review).
    let pr = match track.pr {
        Some(pr) => Some(pr),
        None => io.merged_linking_pr(track).await?,
    };
    let (merged, by_owner) = match pr {
        Some(pr) => io.pr_merged(pr).await?.map_or((None, false), |own| (Some(pr), own)),
        None => (None, false),
    };
    let text = match merged {
        Some(pr) => crate::i18n::format(FIXED, &[("number", n.to_string()), ("pr", pr.to_string())]),
        None => crate::i18n::format("✅ #{number} closed as completed", &[("number", n.to_string())]),
    };
    io.say(n, "fixed", text).await?;
    // "merged" only for a pull request GitHub reports merged; an issue closed
    // as completed without it is "closed" (integration GPT review).
    let state = if merged.is_some() { "merged" } else { "closed" };
    // The owner's own merge of exactly this pull request is what a new
    // watch's trial counts (ADR 0027, GPT review).
    let pr = merged.map_or(Value::Null, Value::from);
    io.record(change_of(repo, n, json!({"state": state, "notified": true, "pr": pr, "ownerMerged": by_owner}))).await
}
/// Whether pull request `pr` is linked to issue `number`: a closing keyword
/// in its description, or a head branch naming the issue ("issue-33",
/// "issue33", "33-fix-typo", "fix-typo-33"); a bare number elsewhere in a branch is not enough.
pub(crate) fn links_issue(pr: &PullRequest, repo: &str, number: i64) -> bool {
    if closes_issue(&pr.body, repo, number) {
        return true;
    }
    branch_names_issue(&pr.head_ref, number)
}

pub const RETRYING: &str = "🔁 Retrying #{number} (attempt {attempt} of {max}): no pull request from the last hand-off";
pub const GAVE_UP: &str = "⚠️ #{number}: no pull request after {max} hand-offs — it needs a person";
pub const CANNOT_FIX: &str = "🙋 #{number}: the Agent says it cannot fix this — see its comment on the issue: {url}";
pub const NEEDS_YOU: &str = "👀 PR #{pr} for #{number} needs your review and merge approval: {url}";
pub const FIXED: &str = "✅ #{number} fixed → PR #{pr} merged";
