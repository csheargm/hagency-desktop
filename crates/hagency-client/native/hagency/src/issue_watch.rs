//! GitHub issue watch for desktop callers: the engine's types and selection
//! (`hagency_agent_local::issue_watch`) plus the HTTP read of a repository's
//! open issues, sent with this crate's TLS client so the desktop needs no
//! second HTTP stack. Nothing here writes to GitHub.
pub use hagency_agent_local::issue_watch::wire::{FetchError, GITHUB_API};
pub use hagency_agent_local::issue_watch::{DEFAULT_LABEL, Issue, IssueWatch, WatchMode, WatchSetting, select};
use hagency_agent_local::issue_watch::wire::{USER_AGENT, next_page, page_url, parse_issues, parse_login, parse_comments, parse_permission, parse_pulls, pulls_url, comments_url};
pub use hagency_agent_local::issue_watch::wire::{CLAIM_MARKER, Comment, PullRequest, claim_holder, others_ready_pulls};

/// The HTTP client `fetch_open_issues` takes.
pub type Client = reqwest::Client;

/// The owner's work list types (ADR 0016), for the desktop's Work block and poller.
pub use hagency_agent_local::merge_policy::work::{CAN_MERGE, IssueFilter, OwnerRules, Role};

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
