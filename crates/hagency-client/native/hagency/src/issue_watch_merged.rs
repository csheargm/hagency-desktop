//! The pull requests of a repository GitHub reports merged, newest update
//! first (one page): the issue loop finds the pull request that fixed a
//! tracked issue when it was opened and merged between two checks, so the
//! open list never showed it (integration GPT review: such an owner merge
//! ended the record "closed" and never counted toward a trial or digest).
use super::{Client, FetchError, PullRequest, get};
use hagency_agent_local::issue_watch::wire::{PAGE_SIZE, parse_pulls};

/// The URL of `repo`'s closed pull requests, most recently updated first.
pub fn closed_pulls_url(api: &str, repo: &str) -> Result<String, FetchError> {
    if !super::valid_repo(repo) {
        return Err(FetchError::Repo(repo.to_owned()));
    }
    Ok(format!("{api}/repos/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page={PAGE_SIZE}"))
}

/// The merged ones (a `merged_at` time) of a closed pull request list.
pub fn parse_merged_pulls(body: &str) -> Result<Vec<PullRequest>, FetchError> {
    let wire: Vec<serde_json::Value> = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    let merged: Vec<u64> = wire.iter().filter(|p| p["merged_at"].is_string()).filter_map(|p| p["number"].as_u64()).collect();
    Ok(parse_pulls(body)?.into_iter().filter(|p| merged.contains(&p.number)).collect())
}

/// `repo`'s recently merged pull requests, read with `token`.
pub async fn fetch_merged_pulls(client: &Client, api: &str, repo: &str, token: &str) -> Result<Vec<PullRequest>, FetchError> {
    parse_merged_pulls(&get(client, &closed_pulls_url(api, repo)?, token).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(number: u64, merged_at: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"number": number, "html_url": format!("https://github.com/a/b/pull/{number}"), "head": {"sha": "s", "ref": format!("issue-{number}")},
            "user": {"login": "betty"}, "created_at": "2026-11-18T10:00:00Z", "body": "Closes #3", "merged_at": merged_at})
    }

    #[test]
    fn only_merged_pull_requests_are_kept_and_the_repository_is_checked() {
        let body = serde_json::json!([pull(5, "2026-11-18T11:00:00Z".into()), pull(6, serde_json::Value::Null)]).to_string();
        let merged = parse_merged_pulls(&body).unwrap();
        assert_eq!(merged.iter().map(|p| p.number).collect::<Vec<_>>(), [5], "closed unmerged is not a fix");
        assert_eq!(merged[0].body, "Closes #3");
        assert!(closed_pulls_url("https://api", "a/b").unwrap().contains("/repos/a/b/pulls?state=closed&sort=updated"));
        assert!(matches!(closed_pulls_url("https://api", "a/b/../c"), Err(FetchError::Repo(_))));
    }
}
