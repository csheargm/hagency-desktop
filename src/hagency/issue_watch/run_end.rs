//! How the issue loop sees that a hand-off's run is over (ADR 0020): the
//! Agent's own Activity record of the hand-off request (this device's
//! ledger), and, when it ended with no pull request, an issue branch it
//! pushed. The tracker then retries at once instead of waiting out the
//! attempt's deadline, and says why in one room line.
use serde_json::Value;

/// A request received this long before the hand-off was recorded still
/// counts as its run (the record is written right after the post).
pub const RUN_SLACK_SECS: i64 = 60;

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
}

impl RunEnd {
    pub fn ended(self) -> bool {
        self != Self::Running
    }
}

/// The hand-off's run among `requests` (the Activity view's `requests`,
/// newest first): the earliest request in `room` received at or after
/// `handed_at` less [`RUN_SLACK_SECS`].
pub fn run_end_of(requests: &Value, room: &str, handed_at: i64) -> RunEnd {
    let ours = requests.as_array().into_iter().flatten().filter(|r| {
        r["roomId"].as_str() == Some(room) && r["receivedAt"].as_i64().is_some_and(|at| at >= handed_at - RUN_SLACK_SECS)
    });
    match ours.min_by_key(|r| r["receivedAt"].as_i64()).and_then(|r| r["outcome"].as_str()) {
        Some("answered" | "declined") => RunEnd::Ended,
        Some("failed") => RunEnd::Lost,
        _ => RunEnd::Running,
    }
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
/// names it (more than one: the loop cannot tell which, so it just retries).
pub fn issue_branch(branches: &[String], number: i64) -> Option<String> {
    let mut named = branches.iter().filter(|b| branch_names_issue(b, number));
    match (named.next(), named.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

pub const PUSHED_NO_PR: &str = "🔁 #{number}: {agent} pushed {branch} but opened no pull request — retrying (attempt {attempt} of {max})";
pub const ENDED_NO_PR: &str = "🔁 #{number}: the run ended without a pull request — retrying (attempt {attempt} of {max})";
pub const RUN_LOST: &str = "🔁 #{number}: the run was lost before it finished — retrying (attempt {attempt} of {max})";
