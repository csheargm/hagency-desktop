//! "Check": who the token signs in as (`GET /user`) and what it may do in
//! each watched repository (`GET /repos/{owner}/{name}`, whose `permissions`
//! say push and pull). Read-only; the API base is a parameter so tests can
//! answer for GitHub.
use super::{Token, redact};
use hagency_agent_local::issue_watch::wire::USER_AGENT;
use serde_json::Value;

/// The `GET /user` failure, or a network error (already redacted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckError {
    /// 401: wrong, expired or revoked token.
    Unauthorized,
    Status(u16),
    Http(String),
}

/// What the token may do in one repository.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RepoAccess {
    Granted { push: bool, pull: bool },
    /// 404: the repository is private and not shared with the token, or gone.
    NotReachable,
    Status(u16),
    Http(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckReport {
    pub login: String,
    pub repos: Vec<(String, RepoAccess)>,
}

/// Signs in with `token` at `api` (`GITHUB_API` outside tests) and reads
/// each of `repos`; one unreachable repository does not stop the others.
pub async fn check(client: &reqwest::Client, api: &str, token: &Token, repos: &[String]) -> Result<CheckReport, CheckError> {
    let user = match get(client, &format!("{api}/user"), token).await {
        Ok((200, body)) => body,
        Ok((401, _)) => return Err(CheckError::Unauthorized),
        Ok((status, _)) => return Err(CheckError::Status(status)),
        Err(error) => return Err(CheckError::Http(error)),
    };
    let login = user["login"].as_str().unwrap_or_default().to_owned();
    let mut checked = Vec::with_capacity(repos.len());
    for repo in repos {
        checked.push((repo.clone(), repo_access(client, api, token, repo).await));
    }
    Ok(CheckReport { login, repos: checked })
}

async fn repo_access(client: &reqwest::Client, api: &str, token: &Token, repo: &str) -> RepoAccess {
    match get(client, &format!("{api}/repos/{repo}"), token).await {
        Ok((200, body)) => {
            let allowed = |name: &str| body["permissions"][name].as_bool().unwrap_or(false);
            RepoAccess::Granted { push: allowed("push"), pull: allowed("pull") }
        }
        Ok((404, _)) => RepoAccess::NotReachable,
        Ok((status, _)) => RepoAccess::Status(status),
        Err(error) => RepoAccess::Http(error),
    }
}

/// The token's own permission on `repo` (the highest of `admin`,
/// `maintain`, `write`, `triage`, `read`), or `None` on a 404: missing, or
/// private and not shared with the token.
pub async fn permission(client: &reqwest::Client, api: &str, token: &Token, repo: &str) -> Result<Option<String>, String> {
    match get(client, &format!("{api}/repos/{repo}"), token).await? {
        (200, body) => Ok(Some(highest_permission(&body).to_owned())),
        (404, _) => Ok(None),
        (status, _) => Err(format!("GitHub answered {status} for {repo}")),
    }
}

/// The highest permission in a repository reply's `permissions` object, named
/// like the collaborator endpoint; a reply without one (public read) is `read`.
pub fn highest_permission(body: &Value) -> &'static str {
    let order = [("admin", "admin"), ("maintain", "maintain"), ("push", "write"), ("triage", "triage"), ("pull", "read")];
    order.iter().find(|(key, _)| body["permissions"][key] == true).map_or("read", |(_, name)| *name)
}

/// Status and JSON body (`Null` when not JSON); errors never hold the token.
async fn get(client: &reqwest::Client, url: &str, token: &Token) -> Result<(u16, Value), String> {
    let failed = |e: reqwest::Error| redact(&e.to_string(), token.expose());
    let response = client
        .get(url)
        .bearer_auth(token.expose())
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status().as_u16();
    let body = response.text().await.map_err(failed)?;
    Ok((status, serde_json::from_str(&body).unwrap_or(Value::Null)))
}
