//! What the auto-merge gate reads from GitHub, bound to exact commits.
//!
//! The pull request view gives the head and base commit SHAs; the changed
//! files then come from `compare/<base>...<head>`, so the files judged belong
//! to the head the merge is pinned to (`--match-head-commit`), and the policy
//! is read at the base commit, not the moving branch name.
use super::gate::{repo_name, GhPr};
use super::{ChangedFile, Check, PrFacts, POLICY_PATH};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

const GH_TIMEOUT: Duration = Duration::from_secs(20);
/// GitHub's compare API lists at most this many files; a full page may be truncated.
const COMPARE_FILE_LIMIT: usize = 300;
const VIEW_FIELDS: &str = "url,state,baseRefOid,headRefOid,statusCheckRollup,author,body";
const FILE_FIELDS: &str = "[.files[] | {filename, status, previous_filename, additions, deletions, changes, has_patch: (.patch != null)}]";

/// One pull request as the gate judges it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrView {
    pub url: String,
    pub base_sha: String,
    pub head_sha: String,
    /// The GitHub login that opened the pull request ("" when not reported).
    pub author: String,
    /// The Agent its body names: "Opened by agent <name> (" (Hagency's attribution line).
    pub opened_by: Option<String>,
    pub facts: PrFacts,
}

/// A full 40-hex commit SHA, lower case.
pub fn commit_sha(text: &str) -> Option<&str> {
    (text.len() == 40 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(text)
}

/// The Agent a pull request body names in Hagency's attribution line
/// (`Opened by agent <name> (<mxid>) on request from …`).
pub fn opened_by(body: &str) -> Option<String> {
    let rest = &body[body.find(OPENED_BY)? + OPENED_BY.len()..];
    let name = rest.split(" (").next()?.trim();
    (!name.is_empty() && !name.contains(char::is_whitespace)).then(|| name.to_owned())
}
const OPENED_BY: &str = "Opened by agent ";

/// The view of `gh pr view --json VIEW_FIELDS` plus the compare API's files.
pub fn view_from_json(view: &Value, files: &Value) -> Option<PrView> {
    let url = view["url"].as_str()?.to_owned();
    let base_sha = commit_sha(view["baseRefOid"].as_str()?)?.to_owned();
    let head_sha = commit_sha(view["headRefOid"].as_str()?)?.to_owned();
    let files = files.as_array()?;
    if files.len() >= COMPARE_FILE_LIMIT {
        return None;
    }
    let files = files.iter().map(changed_file).collect::<Option<_>>()?;
    // Missing or unreadable CI evidence is "still running", never "no checks" (GPT review P2).
    let checks = view["statusCheckRollup"].as_array().map_or_else(|| vec![Check::Pending], |all| all.iter().map(check_state).collect());
    let author = view["author"]["login"].as_str().unwrap_or_default().to_owned();
    let opened_by = view["body"].as_str().and_then(opened_by);
    Some(PrView { url, base_sha, head_sha, author, opened_by, facts: PrFacts { open: view["state"] == "OPEN", files, checks } })
}

fn changed_file(file: &Value) -> Option<ChangedFile> {
    let status = file["status"].as_str()?;
    let additions = file["additions"].as_u64()?;
    let deletions = file["deletions"].as_u64()?;
    let lines = file["changes"].as_u64()?;
    let plain = matches!(status, "added" | "modified" | "removed");
    let opaque = !plain || lines == 0 || file["has_patch"] != true;
    let previous_path = file["previous_filename"].as_str().map(str::to_owned);
    Some(ChangedFile { path: file["filename"].as_str()?.to_owned(), previous_path, additions, deletions, opaque })
}

fn check_state(check: &Value) -> Check {
    let word = check["conclusion"].as_str().filter(|c| !c.is_empty()).or(check["state"].as_str()).unwrap_or("");
    match word {
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => Check::Passed,
        "" | "PENDING" | "QUEUED" | "IN_PROGRESS" | "EXPECTED" => Check::Pending,
        _ => Check::Failed,
    }
}

/// `owner/name` of a github.com pull request URL for pull request `number`.
pub fn repo_of_url(url: &str, number: u64) -> Option<&str> {
    let rest = url.strip_prefix("https://github.com/")?;
    let (repo, tail) = rest.split_once("/pull/")?;
    (tail == number.to_string()).then_some(repo).and_then(repo_name)
}

/// `gh <args>` as the Agent: its token only, never the daemon's stored
/// login; no token reads nothing (GPT review, Nov 16).
fn gh_command(dir: &Path, args: &[&str], token: Option<&str>) -> Option<tokio::process::Command> {
    let token = token.filter(|t| !t.is_empty())?;
    let mut command = tokio::process::Command::new("gh");
    command.args(args).current_dir(dir).kill_on_drop(true);
    command.env_remove("GITHUB_TOKEN").env(crate::agent_env::GITHUB_TOKEN_VARIABLE, token).env("GH_CONFIG_DIR", dir.join(".hagency-no-gh-config"));
    Some(command)
}

async fn gh(dir: &Path, args: &[&str], token: Option<&str>) -> Option<String> {
    let run = gh_command(dir, args, token)?.output();
    let output = tokio::time::timeout(GH_TIMEOUT, run).await.ok()?.ok()?;
    output.status.success().then(|| String::from_utf8(output.stdout).ok()).flatten()
}

/// Where the gate reads pull requests and policies (`gh` here; fakes in tests).
pub trait PrSource {
    fn pull_request(&self, command: &GhPr) -> impl std::future::Future<Output = Option<PrView>>;
    /// The policy file at `base_sha`, the base commit the pull request was judged on.
    fn policy(&self, command: &GhPr, repo: &str, base_sha: &str) -> impl std::future::Future<Output = Option<String>>;
    /// The policy file at `base_sha`: `Some(None)` when the repository has
    /// none (GitHub answered 404), `None` when it cannot be read.
    fn policy_file(&self, command: &GhPr, repo: &str, base_sha: &str) -> impl std::future::Future<Output = Option<Option<String>>> {
        async move { self.policy(command, repo, base_sha).await.map(Some) }
    }
    /// The GitHub login this device's `gh` uses; `None` (the default) when unknown.
    fn viewer_login(&self, _command: &GhPr) -> impl std::future::Future<Output = Option<String>> {
        async { None }
    }
    /// GitHub's reviews of the pull request as `(login, state, commit)` in
    /// order; `None` (the default) when they cannot be read, so a review
    /// recorded on another device never counts unverified.
    fn reviews(&self, _command: &GhPr, _repo: &str) -> impl std::future::Future<Output = Option<Vec<(String, String, String)>>> {
        async { None }
    }
    /// When (Unix seconds) GitHub received each review of the pull request
    /// that it attached to commit `head` and whose body holds
    /// `<APPROVE_MARKER> <head>`; `None` (the default) when unreadable.
    fn approvals_on(&self, _command: &GhPr, _repo: &str, _head: &str) -> impl std::future::Future<Output = Option<Vec<i64>>> {
        async { None }
    }
}

/// Whether some GitHub user's latest decisive review (APPROVED, UNBOUND,
/// CHANGES_REQUESTED or DISMISSED; comments do not change it) approves
/// exactly `head`.
pub fn approved_on(reviews: &[(String, String, String)], head: &str) -> bool {
    let mut latest: std::collections::HashMap<&str, (&str, &str)> = std::collections::HashMap::new();
    for (login, state, commit) in reviews {
        if matches!(state.as_str(), "APPROVED" | "UNBOUND" | "CHANGES_REQUESTED" | "DISMISSED") {
            latest.insert(login, (state, commit));
        }
    }
    latest.values().any(|(state, commit)| *state == "APPROVED" && *commit == head)
}

/// `login<TAB>state<TAB>commit` lines from `gh api --jq` into tuples.
pub fn parse_reviews(text: &str) -> Option<Vec<(String, String, String)>> {
    text.lines().filter(|l| !l.is_empty()).map(|line| {
        let mut parts = line.split('\t');
        Some((parts.next()?.to_owned(), parts.next()?.to_owned(), parts.next()?.to_owned()))
    }).collect()
}

/// `gh` as one Agent: its own GitHub token (see [`gh_command`]).
pub struct GhCli {
    pub token: Option<String>,
}
impl GhCli {
    fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }
}
impl PrSource for GhCli {
    async fn pull_request(&self, command: &GhPr) -> Option<PrView> {
        pull_request(command, self.token()).await
    }
    async fn policy(&self, command: &GhPr, repo: &str, base_sha: &str) -> Option<String> {
        let path = format!("repos/{repo}/contents/{POLICY_PATH}?ref={base_sha}");
        gh(&command.dir, &["api", &path, "-H", "Accept: application/vnd.github.raw"], self.token()).await
    }
    async fn policy_file(&self, command: &GhPr, repo: &str, base_sha: &str) -> Option<Option<String>> {
        let path = format!("repos/{repo}/contents/{POLICY_PATH}?ref={base_sha}");
        let run = gh_command(&command.dir, &["api", &path, "-H", "Accept: application/vnd.github.raw"], self.token())?.output();
        let output = tokio::time::timeout(GH_TIMEOUT, run).await.ok()?.ok()?;
        if output.status.success() {
            return String::from_utf8(output.stdout).ok().map(Some);
        }
        // Only GitHub's own "no such file" means absent; anything else is unknown.
        String::from_utf8_lossy(&output.stderr).contains("(HTTP 404)").then_some(None)
    }
    async fn viewer_login(&self, command: &GhPr) -> Option<String> {
        let login = gh(&command.dir, &["api", "user", "--jq", ".login"], self.token()).await?;
        Some(login.trim().to_owned()).filter(|l| !l.is_empty())
    }
    async fn approvals_on(&self, command: &GhPr, repo: &str, head: &str) -> Option<Vec<i64>> {
        // `head` is a checked 40-hex commit and the marker a constant: neither can break the filter.
        let head = commit_sha(head)?;
        let path = format!("repos/{repo}/pulls/{}/reviews?per_page=100", command.number);
        let filter = format!(".[] | select(.commit_id == \"{head}\" and ((.body // \"\") | contains(\"{} {head}\"))) | .submitted_at | fromdateiso8601", super::gate::APPROVE_MARKER);
        gh(&command.dir, &["api", &path, "--paginate", "--jq", &filter], self.token()).await?.lines().map(|line| line.trim().parse().ok()).collect()
    }
    async fn reviews(&self, command: &GhPr, repo: &str) -> Option<Vec<(String, String, String)>> {
        let path = format!("repos/{repo}/pulls/{}/reviews?per_page=100", command.number);
        parse_reviews(&gh(&command.dir, &["api", &path, "--paginate", "--jq", REVIEW_FIELDS], self.token()).await?)
    }
}

/// One review per line for [`parse_reviews`].
/// An APPROVED review whose body does not name the commit GitHub attached it
/// to reads as `UNBOUND`: the reviewer may have read another head (a push
/// can land between its read and its `gh pr review`), so it never counts.
const REVIEW_FIELDS: &str = ".[] | . as $r | [$r.user.login, (if $r.state == \"APPROVED\" and (($r.body // \"\") | contains($r.commit_id)) then \"APPROVED\" elif $r.state == \"APPROVED\" then \"UNBOUND\" else $r.state end), $r.commit_id] | @tsv";

async fn pull_request(command: &GhPr, token: Option<&str>) -> Option<PrView> {
    let number = command.number.to_string();
    let mut args = vec!["pr", "view", number.as_str(), "--json", VIEW_FIELDS];
    if let Some(repo) = &command.repo {
        args.extend(["--repo", repo.as_str()]);
    }
    let view: Value = serde_json::from_str(&gh(&command.dir, &args, token).await?).ok()?;
    let url = view["url"].as_str()?;
    let repo = repo_of_url(url, command.number)?;
    let (base, head) = (commit_sha(view["baseRefOid"].as_str()?)?, commit_sha(view["headRefOid"].as_str()?)?);
    let compare = format!("repos/{repo}/compare/{base}...{head}");
    let files: Value = serde_json::from_str(&gh(&command.dir, &["api", &compare, "--jq", FILE_FIELDS], token).await?).ok()?;
    view_from_json(&view, &files)
}
