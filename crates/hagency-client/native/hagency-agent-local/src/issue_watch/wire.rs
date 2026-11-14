//! GitHub's REST issue list, without the HTTP: the request URL, the page
//! rule and the JSON parser. The caller (the desktop poller, through the
//! `hagency` crate) sends the requests with the Agent's token. Every failure
//! comes back as a `FetchError`; nothing is swallowed.
use super::Issue;
use serde::Deserialize;

/// GitHub's REST API root.
pub const GITHUB_API: &str = "https://api.github.com";
/// Newest issues first, `PAGE_SIZE` per page, following pages until a short
/// one so a burst of more than one page between polls is not missed.
pub const PAGE_SIZE: u32 = 100;
/// Upper bound on pages per check (rate-limit guard); older issues beyond it
/// are not listed, which only matters for repositories with >1000 open items.
pub const MAX_PAGES: u32 = 10;
/// The `User-Agent` GitHub requires on every request.
pub const USER_AGENT: &str = "hagency-issue-watch";

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("invalid repository `{0}`: expected owner/name")]
    Repo(String),
    #[error("GitHub request failed: {0}")]
    Http(String),
    #[error("GitHub answered {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unreadable GitHub issue list: {0}")]
    Parse(String),
}

#[derive(Deserialize)]
struct WireLabel {
    name: String,
}
#[derive(Deserialize)]
struct WireIssue {
    number: i64,
    html_url: String,
    #[serde(default)]
    labels: Vec<WireLabel>,
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
    created_at: String,
    #[serde(default)]
    author_association: String,
}

/// Turn GitHub's JSON issue list into `Issue`s.
pub fn parse_issues(body: &str) -> Result<Vec<Issue>, FetchError> {
    let wire: Vec<WireIssue> = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    wire.into_iter()
        .map(|issue| {
            let created = time::OffsetDateTime::parse(&issue.created_at, &time::format_description::well_known::Rfc3339)
                .map_err(|e| FetchError::Parse(format!("created_at `{}`: {e}", issue.created_at)))?;
            Ok(Issue {
                number: issue.number,
                url: issue.html_url,
                labels: issue.labels.into_iter().map(|l| l.name).collect(),
                is_pr: issue.pull_request.is_some(),
                author_association: issue.author_association,
                created_at: created.unix_timestamp(),
            })
        })
        .collect()
}

/// The page to read after `page` returned `count` items, if any.
pub fn next_page(page: u32, count: usize) -> Option<u32> {
    (count >= PAGE_SIZE as usize && page < MAX_PAGES).then_some(page + 1)
}

/// The URL of one page of `repo`'s open issues, newest first; `api` is
/// `GITHUB_API` outside tests. A repository that is not `owner/name` is refused
/// here, so nothing else can reach the URL path.
pub fn page_url(api: &str, repo: &str, page: u32) -> Result<String, FetchError> {
    if !super::valid_repo(repo) {
        return Err(FetchError::Repo(repo.to_owned()));
    }
    Ok(format!("{api}/repos/{repo}/issues?state=open&sort=created&direction=desc&per_page={PAGE_SIZE}&page={page}"))
}
