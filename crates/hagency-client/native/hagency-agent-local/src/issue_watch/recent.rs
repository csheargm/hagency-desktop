//! GitHub's recently closed issues and pull requests of a repository, for
//! the room's repository status table (ADR 0023). Read-only: one page of
//! `issues?state=closed&since=…`, which lists pull requests too.
use super::wire::{FetchError, PAGE_SIZE};
use hagency_runtime::repo_status::table::{GithubItem, GithubState};
use serde::Deserialize;

#[derive(Deserialize)]
struct WireClosed {
    number: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    pull_request: Option<WirePull>,
    #[serde(default)]
    closed_at: Option<String>,
}
#[derive(Deserialize)]
struct WirePull {
    #[serde(default)]
    merged_at: Option<String>,
}

fn unix(text: &str) -> Result<i64, FetchError> {
    time::OffsetDateTime::parse(text, &time::format_description::well_known::Rfc3339)
        .map(|t| t.unix_timestamp())
        .map_err(|e| FetchError::Parse(format!("closed_at `{text}`: {e}")))
}

/// The URL of the closed items updated since `since` (unix seconds), newest first.
pub fn closed_url(api: &str, repo: &str, since: i64) -> Result<String, FetchError> {
    if !super::valid_repo(repo) {
        return Err(FetchError::Repo(repo.to_owned()));
    }
    let since = time::OffsetDateTime::from_unix_timestamp(since).map_err(|e| FetchError::Parse(e.to_string()))?;
    let since = since.format(&time::format_description::well_known::Rfc3339).map_err(|e| FetchError::Parse(e.to_string()))?;
    Ok(format!("{api}/repos/{repo}/issues?state=closed&sort=updated&direction=desc&per_page={PAGE_SIZE}&since={since}"))
}

/// Closed items that closed at or after `since`; a pull request merged
/// (GitHub's `merged_at`) is `Merged`, any other close is `Closed`.
pub fn parse_closed(body: &str, since: i64) -> Result<Vec<GithubItem>, FetchError> {
    let wire: Vec<WireClosed> = serde_json::from_str(body).map_err(|e| FetchError::Parse(e.to_string()))?;
    let mut out = Vec::new();
    for item in wire {
        let Some(closed) = item.closed_at.as_deref() else { continue };
        let closed_at = unix(closed)?;
        if closed_at < since {
            continue;
        }
        let merged = item.pull_request.as_ref().is_some_and(|p| p.merged_at.is_some());
        out.push(GithubItem {
            number: item.number, title: item.title, is_pr: item.pull_request.is_some(),
            state: if merged { GithubState::Merged } else { GithubState::Closed }, closed_at: Some(closed_at),
        });
    }
    Ok(out)
}

/// GitHub's open issue list entries as table items.
pub fn open_items(issues: &[super::Issue]) -> Vec<GithubItem> {
    issues.iter().map(|i| GithubItem { number: i.number, title: i.title.clone(), is_pr: i.is_pr, state: GithubState::Open, closed_at: None }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_pulls_closed_issues_and_the_window_are_told_apart() {
        let body = r#"[
            {"number": 32, "title": "Fix", "pull_request": {"merged_at": "2026-11-16T10:00:00Z"}, "closed_at": "2026-11-16T10:00:00Z"},
            {"number": 30, "title": "Rejected", "pull_request": {"merged_at": null}, "closed_at": "2026-11-16T09:00:00Z"},
            {"number": 31, "title": "Typo", "closed_at": "2026-11-16T10:01:00Z"},
            {"number": 5, "title": "Old", "closed_at": "2026-10-01T00:00:00Z"}
        ]"#;
        let since = unix("2026-11-10T00:00:00Z").unwrap();
        let items = parse_closed(body, since).unwrap();
        let states: Vec<(i64, bool, GithubState)> = items.iter().map(|i| (i.number, i.is_pr, i.state)).collect();
        assert_eq!(states, vec![(32, true, GithubState::Merged), (30, true, GithubState::Closed), (31, false, GithubState::Closed)]);
        assert!(parse_closed("{}", since).is_err());
    }

    #[test]
    fn the_url_names_only_a_valid_repository() {
        let url = closed_url("https://api.github.com", "acme/web", 0).unwrap();
        assert!(url.ends_with("&since=1970-01-01T00:00:00Z"), "{url}");
        assert!(closed_url("https://api.github.com", "acme/web?x=1", 0).is_err());
    }
}
