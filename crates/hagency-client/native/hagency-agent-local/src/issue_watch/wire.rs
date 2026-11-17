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
    title: String,
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
                title: issue.title,
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

/// One open pull request as the review loop reads it (ADR 0015).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub draft: bool,
    pub head_sha: String,
    /// The GitHub login that opened it.
    pub author: String,
    /// Unix seconds.
    pub created_at: i64,
}

#[derive(Deserialize)]
struct WireUser {
    login: String,
}
#[derive(Deserialize)]
struct WireHead {
    sha: String,
}
#[derive(Deserialize)]
struct WirePull {
    number: u64,
    html_url: String,
    #[serde(default)]
    draft: bool,
    head: WireHead,
    user: WireUser,
    created_at: String,
}

/// Turn GitHub's JSON pull request list into `PullRequest`s.
pub fn parse_pulls(body: &str) -> Result<Vec<PullRequest>, FetchError> {
    let wire: Vec<WirePull> = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    wire.into_iter()
        .map(|pr| {
            let created = time::OffsetDateTime::parse(&pr.created_at, &time::format_description::well_known::Rfc3339)
                .map_err(|e| FetchError::Parse(format!("created_at `{}`: {e}", pr.created_at)))?;
            Ok(PullRequest { number: pr.number, url: pr.html_url, draft: pr.draft, head_sha: pr.head.sha, author: pr.user.login, created_at: created.unix_timestamp() })
        })
        .collect()
}

/// The newest open pull requests of `repo` (one page: the loop only needs recent ones).
pub fn pulls_url(api: &str, repo: &str) -> Result<String, FetchError> {
    if !super::valid_repo(repo) {
        return Err(FetchError::Repo(repo.to_owned()));
    }
    Ok(format!("{api}/repos/{repo}/pulls?state=open&sort=created&direction=desc&per_page={PAGE_SIZE}"))
}

/// Other people's ready pull requests opened since `since`: never one
/// `login` (this device's own GitHub login) wrote.
pub fn others_ready_pulls<'a>(pulls: &'a [PullRequest], login: &str, since: i64) -> Vec<&'a PullRequest> {
    pulls.iter().filter(|pr| !pr.draft && !pr.author.is_empty() && !pr.author.eq_ignore_ascii_case(login) && pr.created_at >= since).collect()
}

/// The first line of a claim comment (ADR 0016); GitHub is the claim record.
pub const CLAIM_MARKER: &str = "🤖 Hagency: working on this";
/// A claim with no pull request after this long no longer holds.
pub const CLAIM_EXPIRY_SECS: i64 = 24 * 3600;

/// One issue comment: author login, body, unix seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub created_at: i64,
}

#[derive(Deserialize)]
struct WireComment {
    user: WireUser,
    #[serde(default)]
    body: String,
    created_at: String,
}

pub fn parse_comments(body: &str) -> Result<Vec<Comment>, FetchError> {
    let wire: Vec<WireComment> = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    wire.into_iter().map(|c| {
        let created = time::OffsetDateTime::parse(&c.created_at, &time::format_description::well_known::Rfc3339)
            .map_err(|e| FetchError::Parse(format!("created_at `{}`: {e}", c.created_at)))?;
        Ok(Comment { author: c.user.login, body: c.body, created_at: created.unix_timestamp() })
    }).collect()
}

/// Where a claim names the Agent that made it (Agents may share a login).
const CLAIM_AGENT_PREFIX: &str = "[agent:";

/// The claim comment Agent `agent_id` posts: the marker, its stable id,
/// then `label` (its quoted name).
pub fn claim_body(agent_id: &str, label: &str) -> String {
    format!("{CLAIM_MARKER} {CLAIM_AGENT_PREFIX}{agent_id}] — {label}")
}

/// The Agent id a claim comment names, if it names one.
fn claim_agent(body: &str) -> Option<&str> {
    let rest = body.strip_prefix(CLAIM_MARKER)?.trim_start().strip_prefix(CLAIM_AGENT_PREFIX)?;
    let (id, _) = rest.split_once(']')?;
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')).then_some(id)
}

/// Who holds an issue: the claim's GitHub login and, when it names one, Agent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Claim<'a> {
    pub login: &'a str,
    pub agent: Option<&'a str>,
}

impl Claim<'_> {
    /// Whether this is Agent `agent_id` on `login`'s account.
    pub fn is(&self, login: &str, agent_id: &str) -> bool {
        self.login.eq_ignore_ascii_case(login) && self.agent == Some(agent_id)
    }
}

/// The logins whose claim comments need an eligibility check, besides `own`.
pub fn claim_authors<'a>(comments: &'a [Comment], own: &str, now: i64) -> Vec<&'a str> {
    let mut authors: Vec<&str> = comments.iter().filter(|c| live_claim(c, now) && !c.author.eq_ignore_ascii_case(own)).map(|c| c.author.as_str()).collect();
    authors.sort_unstable_by_key(|a| a.to_ascii_lowercase());
    authors.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    authors
}

fn live_claim(c: &Comment, now: i64) -> bool {
    c.body.starts_with(CLAIM_MARKER) && now - c.created_at <= CLAIM_EXPIRY_SECS
}

/// The claim holding the issue: the earliest unexpired claim comment by an
/// `eligible` author (ties: the lower login), so every host reading the same
/// comments agrees. A claim from anyone else (anyone can comment) is ignored,
/// so it can neither take nor block an issue (GPT review, Nov 16).
pub fn claim_holder<'a>(comments: &'a [Comment], now: i64, eligible: impl Fn(&str) -> bool) -> Option<Claim<'a>> {
    comments.iter()
        .filter(|c| live_claim(c, now) && eligible(&c.author))
        .min_by(|a, b| (a.created_at, a.author.to_ascii_lowercase()).cmp(&(b.created_at, b.author.to_ascii_lowercase())))
        .map(|c| Claim { login: c.author.as_str(), agent: claim_agent(&c.body) })
}

/// The URL of an issue's comments (`repo` checked as everywhere here).
pub fn comments_url(api: &str, repo: &str, number: i64, page: u32) -> Result<String, FetchError> {
    if !super::valid_repo(repo) || number <= 0 {
        return Err(FetchError::Repo(repo.to_owned()));
    }
    Ok(format!("{api}/repos/{repo}/issues/{number}/comments?per_page={PAGE_SIZE}&page={page}"))
}

/// `login` and `permission` of `GET /repos/{repo}/collaborators/{login}/permission`.
pub fn parse_permission(body: &str) -> Result<String, FetchError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    // `role_name` names maintain/triage, which the legacy `permission` folds into write/read.
    value["role_name"].as_str().or(value["permission"].as_str()).map(str::to_owned).ok_or_else(|| FetchError::Parse("no permission".into()))
}

/// The login of `GET /user`.
pub fn parse_login(body: &str) -> Result<String, FetchError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    value["login"].as_str().map(str::to_owned).ok_or_else(|| FetchError::Parse("no login".into()))
}
