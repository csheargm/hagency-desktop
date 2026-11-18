//! The issue loop's follow-through (ADR 0020), run each check for one
//! repository: every tracked issue moves by at most one step of the state
//! machine in `::hagency::issue_watch::issue_track` (retry a lost hand-off,
//! follow the Agent's pull request to review and the merge gate, announce
//! the merge, give up, or stop for a person). Every room line is posted
//! before its state is recorded, with a transaction id fixed by issue, step
//! and attempt, so a check that dies in between repeats a duplicate the
//! homeserver drops. The I/O is a trait so the whole flow runs on fakes.
use super::merge_hold::Missing;
use super::notice::quoted_title;
use super::run_end::{ENDED_NO_PR, PUSHED_NO_PR, RUN_LOST, RunEnd, branch_names_issue};
use super::work_slots::closes_issue;
use ::hagency::issue_watch::issue_track::{Action, Facts, IssueFact, MAX_ATTEMPTS, State, Track, decide};
use ::hagency::issue_watch::{Issue, PullRequest};
use serde_json::{Value, json};
use std::collections::HashSet;

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
    /// `Ok(false)` when another Agent now holds the issue's claim.
    async fn hand_off(&self, issue: &Issue, attempt: u32, branch: Option<&str>) -> Result<bool, String>;
    /// Whether the current hand-off's run is over (its Activity record).
    async fn run_end(&self, track: &Track) -> Result<RunEnd, String>;
    /// The one branch the Agent pushed for issue `number`, if any.
    async fn pushed_branch(&self, number: i64) -> Result<Option<String>, String>;
    /// The Agent's name for room lines.
    fn agent_label(&self) -> String;
    /// Whether pull request `pr` was merged (GitHub's own answer).
    async fn pr_merged(&self, pr: u64) -> Result<bool, String>;
    /// A tracked issue missing from the open list: GitHub's own answer.
    async fn closed_state(&self, number: i64) -> Result<IssueFact, String>;
    /// The Agent's cannot-fix comment during the current attempt.
    async fn cannot_fix(&self, track: &Track) -> Result<bool, String>;
    async fn follow_pr(&self, pr: &PullRequest) -> Result<Followed, String>;
    fn paused(&self) -> bool;
}

/// One repository's inputs, read once per check.
pub(crate) struct Inputs<'a> {
    pub repo: &'a str,
    pub now: i64,
    pub tracks: &'a [Track],
    /// Issue numbers already handed off (`issueSeen`), to adopt older hand-offs.
    pub seen: &'a HashSet<i64>,
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
    for number in inputs.seen.iter().filter(|n| inputs.open_issues.iter().any(|i| i.number == **n && !i.is_pr)) {
        if !inputs.tracks.iter().any(|t| t.repo.eq_ignore_ascii_case(inputs.repo) && t.number == *number) {
            if let Err(error) = io.record(json!({"repo": inputs.repo, "number": number, "handed": 1})).await {
                out.errors.push(error);
            }
        }
    }
    for track in inputs.tracks.iter().filter(|t| t.repo.eq_ignore_ascii_case(inputs.repo)) {
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
    let issue = inputs.open_issues.iter().find(|i| i.number == track.number && !i.is_pr);
    let fact = match issue {
        Some(_) => IssueFact::Open,
        None => io.closed_state(track.number).await?,
    };
    let open_pr = inputs.own_pulls.iter().find(|p| links_issue(p, inputs.repo, track.number));
    let cannot_fix = fact == IssueFact::Open && open_pr.is_none() && io.cannot_fix(track).await?;
    // A run that already ended without a pull request is retried now (e2e, Nov 17).
    // An unreadable run only falls back to the deadline (GPT review): never stuck.
    let run = if fact == IssueFact::Open && open_pr.is_none() && !cannot_fix && track.state == State::HandedOff {
        io.run_end(track).await.unwrap_or_else(|e| {
            makepad_widgets::log!("WARN issue_track_run_unreadable #{}: {e}", track.number);
            RunEnd::Running
        })
    } else {
        RunEnd::Running
    };
    let facts = Facts { now: inputs.now, issue: fact, open_pr: open_pr.map(|p| p.number), cannot_fix, run_ended: run.ended() };
    if io.paused() {
        return Err(crate::i18n::tr("Agent paused by the owner").to_owned());
    }
    let n = track.number;
    // The record's own spelling: the ledger keys it exactly (GPT review).
    let change = |extra: Value| change_of(&track.repo, n, extra);
    match decide(track, &facts) {
        Action::Wait => Ok(Some(waiting_line(track))),
        Action::Retry { attempt } => {
            let issue = issue.ok_or("the issue is not open")?;
            // Handed off first: a claim lost to another Agent ends the record
            // instead of repeating this attempt every check (GPT review).
            // No branch list: a plain retry still goes out (GPT review).
            let branch = if run.ended() {
                io.pushed_branch(n).await.unwrap_or_else(|e| {
                    makepad_widgets::log!("WARN issue_track_branches_unreadable #{n}: {e}");
                    None
                })
            } else {
                None
            };
            if !io.hand_off(issue, attempt, branch.as_deref()).await? {
                io.record(change(json!({"state": "closed"}))).await?;
                return Ok(Some(crate::i18n::format("#{number} taken by another Agent", &[("number", n.to_string())])));
            }
            let args = [("number", n.to_string()), ("attempt", attempt.to_string()), ("max", MAX_ATTEMPTS.to_string())];
            io.say(n, &format!("retry-{attempt}"), retry_text(run, branch.as_deref(), &io.agent_label(), &args)).await?;
            Ok(Some(crate::i18n::format("#{number} retrying ({attempt}/{max})", &args)))
        }
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
        Action::FollowPr { pr } => follow_one(io, &track.repo, track, open_pr.ok_or("the pull request is gone")?, pr, holds).await.map(Some),
        Action::AnnounceFixed => announce(io, &track.repo, track).await.map(|_| None),
        Action::Close => io.record(change(json!({"state": "closed"}))).await.map(|_| None),
    }
}

/// Why attempt `args.attempt` was handed off: the deadline passed, or the run ended without a pull request.
fn retry_text(run: RunEnd, branch: Option<&str>, agent: &str, args: &[(&str, String); 3]) -> String {
    let mut args = args.to_vec();
    let template = match (run, branch) {
        (RunEnd::Running, _) => RETRYING,
        (_, Some(branch)) => {
            args.extend([("agent", agent.to_owned()), ("branch", quoted_title(branch))]);
            PUSHED_NO_PR
        }
        (RunEnd::Lost, None) => RUN_LOST,
        (RunEnd::Ended, None) => ENDED_NO_PR,
    };
    crate::i18n::format(template, &args)
}

fn change_of(repo: &str, number: i64, extra: Value) -> Value {
    let mut change = json!({"repo": repo, "number": number});
    if let (Some(c), Some(extra)) = (change.as_object_mut(), extra.as_object()) {
        c.extend(extra.clone());
    }
    change
}

/// The Agent's pull request is open: record it, ask for its review (or the
/// merge gate), and tell a person once when no other Agent can review.
async fn follow_one(io: &impl TrackIo, repo: &str, track: &Track, pull: &PullRequest, pr: u64, holds: &mut Vec<(i64, String)>) -> Result<String, String> {
    let n = track.number;
    if track.state == State::HandedOff || track.pr != Some(pr) {
        io.record(change_of(repo, n, json!({"state": "pr_opened", "pr": pr}))).await?;
    }
    let followed = io.follow_pr(pull).await?;
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
        io.record(change_of(repo, n, json!({"state": "reviewed"}))).await?;
    }
    Ok(pr_line(n, pr, followed))
}

async fn announce(io: &impl TrackIo, repo: &str, track: &Track) -> Result<(), String> {
    let n = track.number;
    // Only a pull request GitHub reports merged is announced as the fix (GPT review).
    let merged = match track.pr {
        Some(pr) if io.pr_merged(pr).await? => Some(pr),
        _ => None,
    };
    let text = match merged {
        Some(pr) => crate::i18n::format(FIXED, &[("number", n.to_string()), ("pr", pr.to_string())]),
        None => crate::i18n::format("✅ #{number} closed as completed", &[("number", n.to_string())]),
    };
    io.say(n, "fixed", text).await?;
    // "merged" only for a pull request GitHub reports merged; an issue closed
    // as completed without it is "closed" (integration GPT review).
    let state = if merged.is_some() { "merged" } else { "closed" };
    io.record(change_of(repo, n, json!({"state": state, "notified": true}))).await
}
/// Whether pull request `pr` is linked to issue `number`: a closing keyword
/// in its description, or a head branch naming the issue ("issue-33",
/// "issue33", "33-fix-typo"); a bare number elsewhere in a branch is not enough.
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

/// What a followed pull request waits for, in the words the room already saw.
pub(crate) fn hold_of(followed: &Followed) -> Option<String> {
    match followed {
        Followed::Held(missing) => Some(missing.iter().map(|m| m.label()).collect::<Vec<_>>().join("; ")),
        Followed::NeedsPerson => Some("a person's review and merge approval".to_owned()),
        Followed::WaitingCi => Some("CI".to_owned()),
        _ => None,
    }
}

fn waiting_line(track: &Track) -> String {
    let n = track.number.to_string();
    match (track.state, track.pr) {
        (State::HandedOff, _) if track.attempts > 1 => crate::i18n::format("#{number} retrying ({attempt}/{max})", &[("number", n), ("attempt", track.attempts.to_string()), ("max", MAX_ATTEMPTS.to_string())]),
        (State::HandedOff, _) => crate::i18n::format("#{number} waiting for a pull request", &[("number", n)]),
        (_, Some(pr)) => crate::i18n::format("#{number} PR #{pr} closed unmerged — needs a person", &[("number", n), ("pr", pr.to_string())]),
        _ => crate::i18n::format("#{number} waiting", &[("number", n)]),
    }
}
fn pr_line(number: i64, pr: u64, followed: Followed) -> String {
    let args = [("number", number.to_string()), ("pr", pr.to_string())];
    let template = match followed {
        Followed::Held(missing) => {
            let needs = missing.iter().map(|m| crate::i18n::tr(m.label())).collect::<Vec<_>>().join("; ");
            let [number, pr] = args;
            return crate::i18n::format("#{number} PR #{pr} needs: {needs}", &[number, pr, ("needs", needs)]);
        }
        Followed::Unreachable(failures) => {
            let [number, pr] = args;
            return crate::i18n::format("#{number} PR #{pr} can't reach GitHub ({failures} checks in a row), retrying", &[number, pr, ("failures", failures.to_string())]);
        }
        Followed::WaitingCi => "#{number} PR #{pr} waiting for CI",
        Followed::Merging => "#{number} PR #{pr} approved, merging",
        Followed::Requested => "#{number} PR #{pr} in review",
        Followed::NeedsPerson => "#{number} PR #{pr} waiting for your review",
        Followed::Draft => "#{number} PR #{pr} is a draft",
    };
    crate::i18n::format(template, &args)
}
pub(crate) fn terminal_line(track: &Track) -> Option<String> {
    let n = track.number.to_string();
    match track.state {
        State::Failed => Some(crate::i18n::format("#{number} gave up after {max} attempts", &[("number", n), ("max", MAX_ATTEMPTS.to_string())])),
        State::NeedsHuman => Some(crate::i18n::format("#{number} needs a person", &[("number", n)])),
        _ => None,
    }
}
