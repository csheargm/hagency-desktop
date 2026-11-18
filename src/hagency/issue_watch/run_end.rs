//! How the issue loop sees that a hand-off's run is over (ADR 0020): the
//! Agent's own Activity record of the hand-off request (this device's
//! ledger), and, when it ended with no pull request, an issue branch it
//! pushed. The tracker then retries at once instead of waiting out the
//! attempt's deadline, and says why in one room line.
use super::notice::quoted_title;
use super::tracker::{RETRYING, TrackIo, change_of};
use ::hagency::issue_watch::Issue;
use ::hagency::issue_watch::issue_track::{MAX_ATTEMPTS, MAX_FREE_RETRIES, Track};
use serde_json::{Value, json};

/// A hand-off's request received this long before its attempt was recorded
/// still counts (the record is written right after the post). Short, so the
/// previous attempt's request (received a whole check earlier) never does.
pub const RUN_SLACK_SECS: i64 = 5;

/// The current hand-off's run, as the Activity record says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunEnd {
    /// Running, not received yet, or nothing readable: wait for the deadline.
    Running,
    /// The run answered (or declined) and ended.
    Ended,
    /// The run failed or was lost (for example a host that could not renew
    /// its lease: the request is never replayed).
    Lost,
    /// A restart of this device's app or Agent host interrupted the run (the
    /// ledger marked it unknown): not the Agent's failure (Nov 17 22:35).
    Interrupted,
}

impl RunEnd {
    pub fn ended(self) -> bool {
        self != Self::Running
    }
}

/// The run of issue `number`'s current hand-off among `requests` (the
/// Activity view's `requests`): the newest request whose `issue` is this
/// one, received at or after `handed_at` less [`RUN_SLACK_SECS`]. Other
/// issues' hand-offs and plain requests in the same room never count
/// (integration GPT review).
pub fn run_end_of(requests: &Value, repo: &str, number: i64, handed_at: i64) -> RunEnd {
    let ours = requests.as_array().into_iter().flatten().filter(|r| {
        r["issue"]["number"].as_i64() == Some(number)
            && r["issue"]["repo"].as_str().is_some_and(|x| x.eq_ignore_ascii_case(repo))
            && r["receivedAt"].as_i64().is_some_and(|at| at >= handed_at - RUN_SLACK_SECS)
    });
    let Some(newest) = ours.max_by_key(|r| r["receivedAt"].as_i64()) else { return RunEnd::Running };
    match newest["outcome"].as_str() {
        Some("answered" | "declined") => RunEnd::Ended,
        Some("failed") if newest["interrupted"] == true => RunEnd::Interrupted,
        Some("failed") => RunEnd::Lost,
        _ => RunEnd::Running,
    }
}

/// The longest branch name the loop repeats.
const MAX_BRANCH_CHARS: usize = 100;

/// A branch name the loop repeats to the Agent exactly: plain ref
/// characters only, so quoting can never change or hide it.
fn plain_branch(branch: &str) -> bool {
    !branch.is_empty() && branch.len() <= MAX_BRANCH_CHARS && !branch.starts_with('-')
        && branch.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

/// Whether a branch name names issue `number` ("issue-33", "issue33",
/// "33-fix-typo" as its last part); a bare number elsewhere is not enough.
pub fn branch_names_issue(branch: &str, number: i64) -> bool {
    let n = number.to_string();
    let branch = branch.to_ascii_lowercase();
    let tokens: Vec<&str> = branch.split(|c: char| !c.is_ascii_alphanumeric()).filter(|t| !t.is_empty()).collect();
    let last = branch.rsplit('/').next().unwrap_or_default();
    tokens.windows(2).any(|w| matches!(w[0], "issue" | "issues" | "gh") && w[1] == n)
        || tokens.iter().any(|t| *t == format!("issue{n}"))
        || last.strip_prefix(&n).is_some_and(|rest| rest.starts_with('-'))
}

/// The pushed branch for issue `number` among `branches`, if exactly one
/// plain-named branch names it (more than one: the loop cannot tell which,
/// so it just retries). Every Agent shares one GitHub login, so the branch's
/// author is not known: the retry asks to use it only if it holds the fix.
pub fn issue_branch(branches: &[String], number: i64) -> Option<String> {
    let mut named = branches.iter().filter(|b| plain_branch(b) && branch_names_issue(b, number));
    match (named.next(), named.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

pub const PUSHED_NO_PR: &str = "🔁 #{number}: {agent} pushed {branch} but opened no pull request — retrying (attempt {attempt} of {max})";
pub const ENDED_NO_PR: &str = "🔁 #{number}: the run ended without a pull request — retrying (attempt {attempt} of {max})";
pub const RUN_LOST: &str = "🔁 #{number}: the run was lost before it finished — retrying (attempt {attempt} of {max})";
pub const RESTARTED: &str = "🔁 #{number}: the run was interrupted (this app or its Agent host restarted or lost its connection) — handing it off again (attempt {attempt} of {max}, not counted)";

/// Which hand-off a retry is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Again {
    /// The next attempt (counted).
    Attempt(u32),
    /// The current attempt again, after a restart interrupted its run.
    Rehand,
}

/// Hand `issue` off again and say why in one room line; the status part.
/// A claim lost to another Agent ends the record instead (GPT review).
pub async fn hand_again(io: &impl TrackIo, issue: &Issue, track: &Track, run: RunEnd, again: Again) -> Result<Option<String>, String> {
    let n = track.number;
    // No branch list: a plain retry still goes out (GPT review).
    let branch = if run.ended() {
        io.pushed_branch(n).await.unwrap_or_else(|e| {
            makepad_widgets::log!("WARN issue_track_branches_unreadable #{n}: {e}");
            None
        })
    } else {
        None
    };
    let (attempt, free) = match again {
        Again::Attempt(attempt) => (attempt, 0),
        Again::Rehand => (track.attempts, (track.free_retries + 1).min(MAX_FREE_RETRIES)),
    };
    if !io.hand_off(issue, attempt, branch.as_deref(), free).await? {
        io.record(change_of(&track.repo, n, json!({"state": "closed"}))).await?;
        return Ok(Some(crate::i18n::format("#{number} taken by another Agent", &[("number", n.to_string())])));
    }
    let mut args = vec![("number", n.to_string()), ("attempt", attempt.to_string()), ("max", MAX_ATTEMPTS.to_string())];
    let status = crate::i18n::format("#{number} retrying ({attempt}/{max})", &args);
    let (key, template) = match (again, run, branch) {
        (Again::Rehand, ..) => (format!("rehand-{attempt}-{free}"), RESTARTED),
        (_, RunEnd::Running, _) => (format!("retry-{attempt}"), RETRYING),
        (_, _, Some(branch)) => {
            args.extend([("agent", io.agent_label()), ("branch", quoted_title(&branch))]);
            (format!("retry-{attempt}"), PUSHED_NO_PR)
        }
        (_, RunEnd::Lost | RunEnd::Interrupted, None) => (format!("retry-{attempt}"), RUN_LOST),
        (_, RunEnd::Ended, None) => (format!("retry-{attempt}"), ENDED_NO_PR),
    };
    io.say(n, &key, crate::i18n::format(template, &args)).await?;
    Ok(Some(status))
}
