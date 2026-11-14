//! GitHub issue watch for desktop callers: the engine's types and selection
//! (`hagency_agent_local::issue_watch`) plus the HTTP read of a repository's
//! open issues, sent with this crate's TLS client so the desktop needs no
//! second HTTP stack. Nothing here writes to GitHub.
pub use hagency_agent_local::issue_watch::wire::{FetchError, GITHUB_API};
pub use hagency_agent_local::issue_watch::{DEFAULT_LABEL, Issue, IssueWatch, WatchMode, WatchSetting, select};
use hagency_agent_local::issue_watch::wire::{USER_AGENT, next_page, page_url, parse_issues};

/// The HTTP client `fetch_open_issues` takes.
pub type Client = reqwest::Client;

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
    parse_issues(&body)
}
