//! The pull request that fixed a tracked issue when no check saw it open:
//! the Agent opened it and the owner merged it between two checks, so the
//! record never linked it and the merge ended as "closed", never counting
//! toward the watch's trial or the digest (integration GPT review). Linked
//! the same way an open one is (`tracker::links_issue`): a closing keyword
//! or a branch naming the issue, opened by this Agent's GitHub login.
use super::tracker::links_issue;
use super::work::Worker;
use ::hagency::issue_watch::{GITHUB_API, PullRequest, fetch_merged_pulls};

/// The newest of `pulls` that `login` opened and that links issue `number` of `repo`.
pub(super) fn pick(pulls: &[PullRequest], (repo, login): (&str, &str), number: i64) -> Option<u64> {
    pulls.iter().find(|p| p.author.eq_ignore_ascii_case(login) && links_issue(p, repo, number)).map(|p| p.number)
}

/// GitHub's recently merged pull requests of `repo`, read with the owner's token.
pub(super) async fn find(worker: &Worker<'_>, (repo, login): (&str, &str), number: i64) -> Result<Option<u64>, String> {
    let pulls = fetch_merged_pulls(worker.client, GITHUB_API, repo, worker.token).await.map_err(|e| e.to_string())?;
    Ok(pick(&pulls, (repo, login), number))
}
