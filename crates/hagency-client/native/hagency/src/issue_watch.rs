//! GitHub issue watch for desktop callers: the engine's types and selection
//! (`hagency_agent_local::issue_watch`) plus the HTTP read of a repository's
//! open issues, sent with this crate's TLS client so the desktop needs no
//! second HTTP stack. Nothing here writes to GitHub.
pub use hagency_agent_local::issue_watch::wire::{FetchError, GITHUB_API, MAX_PAGES, PAGE_SIZE};
/// What an Agent watches, for the room info "Watches" tab (ADR 0019).
pub use hagency_agent_local::watch_info::{RoomWatch, WatchEntry, entries as watch_entries};
pub use hagency_agent_local::issue_watch::{DEFAULT_LABEL, Issue, IssueWatch, WatchMode, WatchSetting, select, valid_repo};
use hagency_agent_local::issue_watch::wire::{USER_AGENT, next_page, page_url, parse_issues, parse_login, parse_comments, parse_permission, parse_pulls, parse_reviews, pulls_url, comments_url, reviews_url};
pub use hagency_agent_local::issue_watch::wire::{Claim, Comment, PullRequest, claim_authors, claim_body, claim_holder, approved_by_other, others_ready_pulls, own_ready_pulls};

/// The issue loop's follow-through records and state machine (ADR 0020).
pub use hagency_agent_local::issue_track;

/// The HTTP client `fetch_open_issues` takes.
pub type Client = reqwest::Client;

/// The one GitHub token rule, shared with the Agent runtime (create, poller, run time).
pub use hagency_agent_local::demo_env::{TOKEN_VARIABLES, resolve as resolve_github_token};

/// The owner's work list types (ADR 0016), for the desktop's Work block and poller.
pub use hagency_agent_local::merge_policy::work::{CAN_MERGE, IssueFilter, MAX_WORK_LIMIT, OwnerRules, Role};

/// The open issues (and pull requests, flagged `is_pr`) of `repo`, newest
/// first, following full pages up to the engine's page cap. `api` is
/// `GITHUB_API` outside tests.
pub async fn fetch_open_issues(client: &Client, api: &str, repo: &str, token: &str) -> Result<Vec<Issue>, FetchError> {
    let mut issues = Vec::new();
    let mut page = Some(1);
    while let Some(current) = page {
        let batch = fetch_page(client, &page_url(api, repo, current)?, token).await?;
        page = next_page(current, batch.len());
        issues.extend(batch);
    }
    Ok(issues)
}

async fn fetch_page(client: &Client, url: &str, token: &str) -> Result<Vec<Issue>, FetchError> {
    parse_issues(&get(client, url, token).await?)
}

/// The newest open pull requests of `repo` (review loop, ADR 0015).
pub async fn fetch_open_pulls(client: &Client, api: &str, repo: &str, token: &str) -> Result<Vec<PullRequest>, FetchError> {
    parse_pulls(&get(client, &pulls_url(api, repo)?, token).await?)
}

/// Every review of pull request `number` as `(login, state, commit)`,
/// oldest first, following full pages up to the engine's page cap.
pub async fn fetch_reviews(client: &Client, api: &str, repo: &str, number: u64, token: &str) -> Result<Vec<(String, String, String)>, FetchError> {
    let mut reviews = Vec::new();
    let mut page = Some(1);
    while let Some(current) = page {
        let batch = parse_reviews(&get(client, &reviews_url(api, repo, number, current)?, token).await?)?;
        page = next_page(current, batch.len());
        reviews.extend(batch);
    }
    Ok(reviews)
}

/// The GitHub login `token` belongs to.
pub async fn fetch_login(client: &Client, api: &str, token: &str) -> Result<String, FetchError> {
    parse_login(&get(client, &format!("{api}/user"), token).await?)
}

/// `login`'s own permission on `repo` (`admin`, `maintain`, `write`, ...),
/// read with that login's own token: never another person's.
pub async fn fetch_permission(client: &Client, api: &str, repo: &str, login: &str, token: &str) -> Result<String, FetchError> {
    pulls_url(api, repo)?; // refuses anything but owner/name
    let safe = !login.is_empty() && login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !safe {
        return Err(FetchError::Parse(format!("unexpected GitHub login `{login}`")));
    }
    parse_permission(&get(client, &format!("{api}/repos/{repo}/collaborators/{login}/permission"), token).await?)
}

/// How many pull requests `login` has open in `repo`, from GitHub's search
/// (one count, however many pull requests the repository has open).
pub async fn fetch_open_pr_count(client: &Client, api: &str, repo: &str, login: &str, token: &str) -> Result<u64, FetchError> {
    pulls_url(api, repo)?; // refuses anything but owner/name
    if login.is_empty() || !login.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(FetchError::Parse(format!("unexpected GitHub login `{login}`")));
    }
    let body = get(client, &format!("{api}/search/issues?q=repo:{repo}+is:pr+is:open+author:{login}&per_page=1"), token).await?;
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;
    value["total_count"].as_u64().ok_or_else(|| FetchError::Parse("no total_count in the search reply".into()))
}

/// Files one issue on `repo` with the owner's own token (a planner card the
/// owner approved); answers its number.
pub async fn create_issue(client: &Client, api: &str, repo: &str, token: &str, title: &str, body: &str) -> Result<u64, FetchError> {
    pulls_url(api, repo)?; // refuses anything but owner/name
    let http = |e: reqwest::Error| FetchError::Http(e.to_string());
    let response = client.post(format!("{api}/repos/{repo}/issues")).bearer_auth(token).header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28").header("User-Agent", USER_AGENT)
        .json(&serde_json::json!({"title": title, "body": body})).send().await.map_err(http)?;
    let status = response.status();
    let text = response.text().await.map_err(http)?;
    if !status.is_success() {
        return Err(FetchError::Status { status: status.as_u16(), body: text });
    }
    let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| FetchError::Parse(e.to_string()))?;
    value["number"].as_u64().ok_or_else(|| FetchError::Parse("no number in the created issue".into()))
}

/// The issue `creator` filed on `repo` whose body carries `marker` (a plan
/// step's line), open or closed, newest first; `None` when there is none.
/// Only `creator`'s own issues count, so nobody else can plant a match.
pub async fn find_issue_by_marker(client: &Client, api: &str, repo: &str, creator: &str, marker: &str, token: &str) -> Result<Option<u64>, FetchError> {
    pulls_url(api, repo)?;
    if creator.is_empty() || !creator.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return Err(FetchError::Parse(format!("unexpected GitHub login `{creator}`")));
    }
    let mut page = Some(1);
    while let Some(current) = page {
        let url = format!("{api}/repos/{repo}/issues?state=all&creator={creator}&sort=created&direction=desc&per_page={PAGE_SIZE}&page={current}");
        let batch: Vec<serde_json::Value> = serde_json::from_str(&get(client, &url, token).await?).map_err(|e| FetchError::Parse(e.to_string()))?;
        let found = batch.iter().find(|i| i["pull_request"].is_null() && i["user"]["login"].as_str().is_some_and(|l| l.eq_ignore_ascii_case(creator)) && i["body"].as_str().is_some_and(|b| b.contains(marker)));
        if let Some(issue) = found {
            return issue["number"].as_u64().map(Some).ok_or_else(|| FetchError::Parse("no number in the issue".into()));
        }
        page = next_page(current, batch.len());
        if page.is_none() && batch.len() >= PAGE_SIZE as usize {
            // Stopped at the page cap: absence is not established (GPT review).
            return Err(FetchError::Parse("too many issues to search for the plan step".into()));
        }
    }
    Ok(None)
}

/// Whether issue `number` is closed as completed (its pull request merged
/// with "Closes #N", or closed by hand as done).
pub async fn fetch_issue_done(client: &Client, api: &str, repo: &str, number: u64, token: &str) -> Result<bool, FetchError> {
    pulls_url(api, repo)?;
    let body = get(client, &format!("{api}/repos/{repo}/issues/{number}"), token).await?;
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;
    Ok(value["state"] == "closed" && value["state_reason"] == "completed")
}

/// Issue `number`'s `(state, state_reason)` as GitHub reports them
/// ("open"/"closed", "completed"/"not_planned"/""), for the issue loop's
/// tracker (ADR 0020), which must tell a closed issue from one that only
/// fell off the open listing.
pub async fn fetch_issue_state(client: &Client, api: &str, repo: &str, number: i64, token: &str) -> Result<(String, String), FetchError> {
    pulls_url(api, repo)?;
    let body = get(client, &format!("{api}/repos/{repo}/issues/{number}"), token).await?;
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;
    let text = |k: &str| value[k].as_str().unwrap_or_default().to_owned();
    Ok((text("state"), text("state_reason")))
}

/// Whether pull request `number` of `repo` was merged.
pub async fn fetch_pull_merged(client: &Client, api: &str, repo: &str, number: u64, token: &str) -> Result<bool, FetchError> {
    pulls_url(api, repo)?;
    let body = get(client, &format!("{api}/repos/{repo}/pulls/{number}"), token).await?;
    let value: serde_json::Value = serde_json::from_str(&body).map_err(|e| FetchError::Parse(e.to_string()))?;
    Ok(value["merged"] == true)
}

/// All of an issue's comments, oldest first, following full pages up to the
/// engine's page cap: a claim may sit beyond the first page.
pub async fn fetch_comments(client: &Client, api: &str, repo: &str, number: i64, token: &str) -> Result<Vec<Comment>, FetchError> {
    let mut comments = Vec::new();
    let mut page = Some(1);
    while let Some(current) = page {
        let batch = parse_comments(&get(client, &comments_url(api, repo, number, current)?, token).await?)?;
        page = next_page(current, batch.len());
        comments.extend(batch);
    }
    Ok(comments)
}

/// Post a comment on an issue with the owner's own token (the claim).
pub async fn post_comment(client: &Client, api: &str, repo: &str, number: i64, token: &str, body: &str) -> Result<(), FetchError> {
    let http = |e: reqwest::Error| FetchError::Http(e.to_string());
    let url = comments_url(api, repo, number, 1)?;
    let url = url.split('?').next().unwrap_or_default();
    let response = client.post(url).bearer_auth(token).header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28").header("User-Agent", USER_AGENT)
        .json(&serde_json::json!({"body": body})).send().await.map_err(http)?;
    let status = response.status();
    if !status.is_success() {
        return Err(FetchError::Status { status: status.as_u16(), body: response.text().await.unwrap_or_default() });
    }
    Ok(())
}

async fn get(client: &Client, url: &str, token: &str) -> Result<String, FetchError> {
    let http = |e: reqwest::Error| FetchError::Http(e.to_string());
    let response = client
        .get(url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(http)?;
    let status = response.status();
    let body = response.text().await.map_err(http)?;
    if !status.is_success() {
        return Err(FetchError::Status { status: status.as_u16(), body });
    }
    Ok(body)
}
