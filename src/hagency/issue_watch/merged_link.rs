//! The pull request that fixed a tracked issue when no check saw it open:
//! the Agent opened it and the owner merged it between two checks, so the
//! record never linked it and the merge ended as "closed", never counting
//! toward the watch's trial or the digest (integration GPT review). Linked
//! the same way an open one is (`tracker::links_issue`): a closing keyword
//! or a branch naming the issue, opened by this Agent's GitHub login.
//! Only one opened during the current hand-off counts: a reopened issue's
//! earlier fix is never announced (or counted) again (GPT review).
use super::tracker::links_issue;
use super::work::Worker;
use ::hagency::issue_watch::issue_track::Track;
use ::hagency::issue_watch::{GITHUB_API, PullRequest, fetch_merged_pulls};

/// GitHub's and this device's clocks may differ by this much.
const CLOCK_SLACK_SECS: i64 = 120;

/// The newest of `pulls` that `login` opened, during `track`'s current hand-off, linking its issue in `repo`.
pub(super) fn pick(pulls: &[PullRequest], (repo, login): (&str, &str), track: &Track) -> Option<u64> {
    let current = |p: &&PullRequest| p.created_at >= track.handed_at - CLOCK_SLACK_SECS;
    pulls.iter().filter(current).find(|p| p.author.eq_ignore_ascii_case(login) && links_issue(p, repo, track.number)).map(|p| p.number)
}

/// GitHub's recently merged pull requests of `repo`, read with the owner's token.
pub(super) async fn find(worker: &Worker<'_>, (repo, login): (&str, &str), track: &Track) -> Result<Option<u64>, String> {
    let pulls = fetch_merged_pulls(worker.client, GITHUB_API, repo, worker.token).await.map_err(|e| e.to_string())?;
    Ok(pick(&pulls, (repo, login), track))
}
