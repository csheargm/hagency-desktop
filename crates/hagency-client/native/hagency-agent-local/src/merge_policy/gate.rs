//! The auto-merge gate in front of the owner's approval card.
//!
//! OFF by default, per Agent on this device. When an Agent with the switch on
//! asks to run exactly `gh pr merge <N> --squash` (or posts an approving
//! `gh pr review`), the gate reads the pull request with this device's `gh`
//! login, judges it with [`super::evaluate`] and, only on `Merge`, answers the
//! card the way the owner's click would, once per pull request. Every other
//! case returns `None`: the normal approval flow (and its card) runs unchanged,
//! so this never weakens the "ask only for outside changes" classifier.
//!
//! Approvals are recorded by Hagency itself, keyed by pull request URL and
//! reviewing Agent, because every Agent here shares one GitHub identity and
//! GitHub refuses (and would not count) a self-approval.
use super::{evaluate, parse_policy, ChangedFile, Check, Decision, PrFacts, POLICY_PATH};
use crate::outside::{resolves_inside, shell};
use crate::{key, Ledger, Result, ToolPolicy, ToolProposal};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A `--comment` review whose body starts with this counts as an approval
/// (GitHub forbids `--approve` on a pull request from the same account).
pub const APPROVE_MARKER: &str = "Hagency verdict: APPROVE";
const GH_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS auto_merge (agent TEXT PRIMARY KEY, enabled INTEGER NOT NULL CHECK(enabled IN (0,1))) STRICT;
CREATE TABLE IF NOT EXISTS auto_merge_reviews (pr TEXT NOT NULL, reviewer TEXT NOT NULL, PRIMARY KEY(pr,reviewer)) STRICT;
CREATE TABLE IF NOT EXISTS auto_merge_used (pr TEXT PRIMARY KEY, agent TEXT NOT NULL) STRICT;";

impl Ledger {
    /// Switch agent auto-merge on or off for one Agent (off unless set).
    pub fn set_auto_merge(&mut self, owner: &str, agent: &str, enabled: bool) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        self.db.execute("INSERT INTO auto_merge VALUES (?,?) ON CONFLICT(agent) DO UPDATE SET enabled=excluded.enabled", (agent, enabled as i64))?;
        Ok(())
    }
    pub fn auto_merge(&self, agent: &str) -> Result<bool> {
        use rusqlite::OptionalExtension;
        Ok(self.db.query_row("SELECT enabled FROM auto_merge WHERE agent=?", [agent], |r| r.get::<_, i64>(0)).optional()? == Some(1))
    }
    pub(crate) fn record_agent_review(&mut self, pr: &str, reviewer: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO auto_merge_reviews VALUES (?,?)", (pr, reviewer))?;
        Ok(())
    }
    pub(crate) fn agent_reviewers(&self, pr: &str) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT reviewer FROM auto_merge_reviews WHERE pr=?")?;
        let rows = statement.query_map([pr], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// Take the one automatic merge of `pr`; false when it was already taken.
    pub(crate) fn claim_auto_merge(&mut self, pr: &str, agent: &str) -> Result<bool> {
        Ok(self.db.execute("INSERT OR IGNORE INTO auto_merge_used VALUES (?,?)", (pr, agent))? == 1)
    }
}

/// A `gh pr` command the gate understands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GhPr {
    pub dir: PathBuf,
    pub number: u64,
    pub repo: Option<String>,
    pub kind: Kind,
}

/// What the command does to the pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `gh pr merge`: judged by the policy.
    Merge,
    /// An approving `gh pr review`: recorded as this Agent's approval.
    Review,
    /// `gh pr ready`: a draft is marked ready so it can be reviewed and merged.
    Ready,
}

/// Read `gh pr merge <N> --squash -R o/r`, `gh pr ready <N> -R o/r` or an
/// approving `gh pr review <N> (--approve | --comment -b "<marker>...") [-R o/r]`,
/// optionally after one `cd <dir> &&` that stays in the workspace. Claude's Bash
/// and Codex's command approval are read the same way; see [`command_words`].
pub fn parse_command(proposal: &ToolProposal) -> Option<GhPr> {
    if proposal.risk == "permission_expansion" {
        return None;
    }
    let (dir, words) = command_words(proposal)?;
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let (kind, number, rest) = match words.as_slice() {
        ["gh", "pr", "merge", n, rest @ ..] => (Kind::Merge, *n, rest),
        ["gh", "pr", "review", n, rest @ ..] => (Kind::Review, *n, rest),
        ["gh", "pr", "ready", n, rest @ ..] => (Kind::Ready, *n, rest),
        _ => return None,
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let flags = Flags::read(rest, kind)?;
    let ok = match kind {
        Kind::Merge => flags.squash && flags.repo.is_some(),
        Kind::Review => flags.approve != flags.comment && (flags.approve || flags.body.starts_with(APPROVE_MARKER)),
        Kind::Ready => flags.repo.is_some(),
    };
    if !ok {
        return None;
    }
    Some(GhPr { dir, number: number.parse().ok()?, repo: flags.repo, kind })
}

/// Characters that would chain, redirect or substitute; any one refuses the command.
const SHELL_METACHARACTERS: [char; 7] = [';', '|', '\n', '`', '$', '<', '>'];

/// The directory and words of a command proposal. `claude.Bash` and Codex's
/// `codex.command` give one shell line; Codex may instead give an argv array,
/// read as `<shell> -c|-lc <line>` or else as plain words that must hold no
/// shell metacharacter. Every other tool (`codex.network` included) is `None`.
fn command_words(proposal: &ToolProposal) -> Option<(PathBuf, Vec<String>)> {
    let cwd = Path::new(&proposal.canonical_directory);
    let command = &proposal.arguments["command"];
    match (proposal.tool.as_str(), command) {
        ("claude.Bash" | "codex.command", Value::String(line)) => shell_words(line, cwd),
        ("codex.command", Value::Array(argv)) => {
            let argv: Vec<&str> = argv.iter().map(Value::as_str).collect::<Option<_>>()?;
            match argv.as_slice() {
                [sh, "-c" | "-lc", line] if is_shell(sh) => shell_words(line, cwd),
                words if words.iter().all(|w| !w.contains(SHELL_METACHARACTERS)) => Some((cwd.to_path_buf(), words.iter().map(|w| (*w).to_owned()).collect())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A system shell: an exact absolute path, or a bare name Codex resolves on
/// its fixed `PATH`; never a relative or workspace path (`./sh`, `repo/zsh`).
fn is_shell(program: &str) -> bool {
    matches!(program, "bash" | "sh" | "zsh" | "/bin/bash" | "/bin/sh" | "/bin/zsh" | "/usr/bin/bash" | "/usr/bin/zsh")
}

/// One shell line: a single command, or one in-workspace `cd <dir> &&` then one command.
fn shell_words(line: &str, cwd: &Path) -> Option<(PathBuf, Vec<String>)> {
    if line.contains(SHELL_METACHARACTERS) {
        return None;
    }
    let mut segments = shell::parse(line)?.segments;
    match segments.len() {
        1 => Some((cwd.to_path_buf(), segments.pop()?)),
        2 if line.contains("&&") => {
            let words = segments.pop()?;
            match segments[0].as_slice() {
                [c, target] if c == "cd" && !target.starts_with('-') && resolves_inside(target, cwd) => Some((shell::lexical_join(cwd, target), words)),
                _ => None,
            }
        }
        _ => None,
    }
}

#[derive(Default)]
struct Flags {
    squash: bool,
    approve: bool,
    comment: bool,
    body: String,
    repo: Option<String>,
}
impl Flags {
    fn read(words: &[&str], kind: Kind) -> Option<Self> {
        let merge = kind == Kind::Merge;
        let review = kind == Kind::Review;
        let mut flags = Self::default();
        let mut words = words.iter();
        while let Some(word) = words.next() {
            match *word {
                "--squash" if merge => flags.squash = true,
                "--approve" | "-a" if review => flags.approve = true,
                "--comment" | "-c" if review => flags.comment = true,
                "--body" | "-b" if review => flags.body = (*words.next()?).to_owned(),
                "--repo" | "-R" => flags.repo = Some(repo_name(words.next()?)?.to_owned()),
                _ => return None,
            }
        }
        Some(flags)
    }
}

/// `owner/name` on github.com and nothing else (no host, no options).
fn repo_name(text: &str) -> Option<&str> {
    let (owner, name) = text.split_once('/')?;
    let part = |s: &str| !s.is_empty() && !s.starts_with(['-', '.']) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    (part(owner) && part(name)).then_some(text)
}

/// The URL, base branch and facts of `gh pr view --json url,state,baseRefName,files,statusCheckRollup`.
pub fn facts_from_json(value: &Value) -> Option<(String, String, PrFacts)> {
    let url = value["url"].as_str()?.to_owned();
    let base = value["baseRefName"].as_str().filter(|b| !b.is_empty() && b.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'/')))?;
    let files = value["files"].as_array()?.iter().map(|f| Some(ChangedFile { path: f["path"].as_str()?.to_owned(), additions: f["additions"].as_u64()?, deletions: f["deletions"].as_u64()? })).collect::<Option<_>>()?;
    let checks = value["statusCheckRollup"].as_array().map(|all| all.iter().map(check_state).collect()).unwrap_or_default();
    Some((url, base.to_owned(), PrFacts { open: value["state"] == "OPEN", files, checks }))
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

async fn gh(dir: &Path, args: &[&str]) -> Option<String> {
    let run = tokio::process::Command::new("gh").args(args).current_dir(dir).kill_on_drop(true).output();
    let output = tokio::time::timeout(GH_TIMEOUT, run).await.ok()?.ok()?;
    output.status.success().then(|| String::from_utf8(output.stdout).ok()).flatten()
}

/// Where the gate reads pull requests and policies (`gh` here; fakes in tests).
pub trait PrSource {
    fn pull_request(&self, command: &GhPr) -> impl std::future::Future<Output = Option<(String, String, PrFacts)>>;
    fn policy(&self, command: &GhPr, repo: &str, base: &str) -> impl std::future::Future<Output = Option<String>>;
}

/// This device's `gh` login.
pub struct GhCli;
impl PrSource for GhCli {
    async fn pull_request(&self, command: &GhPr) -> Option<(String, String, PrFacts)> {
        pull_request(command).await
    }
    async fn policy(&self, command: &GhPr, repo: &str, base: &str) -> Option<String> {
        let path = format!("repos/{repo}/contents/{POLICY_PATH}?ref={base}");
        gh(&command.dir, &["api", &path, "-H", "Accept: application/vnd.github.raw"]).await
    }
}

async fn pull_request(command: &GhPr) -> Option<(String, String, PrFacts)> {
    let number = command.number.to_string();
    let mut args = vec!["pr", "view", number.as_str(), "--json", "url,state,baseRefName,files,statusCheckRollup"];
    if let Some(repo) = &command.repo {
        args.extend(["--repo", repo.as_str()]);
    }
    let value: Value = serde_json::from_str(&gh(&command.dir, &args).await?).ok()?;
    let found = facts_from_json(&value)?;
    repo_of_url(&found.0, command.number)?;
    Some(found)
}

/// Answer `proposal` without a card when the policy allows it, else `None`.
pub async fn auto_answer(ledger: &mut Ledger, owner: &str, proposal: &ToolProposal, now: i64, source: &impl PrSource) -> Option<bool> {
    let command = parse_command(proposal)?;
    let agent = proposal.scope.agent.clone();
    if !ledger.auto_merge(&agent).ok()? {
        return None;
    }
    let disposition = ledger.tool_disposition(proposal, now).ok()?;
    if matches!(disposition, ToolPolicy::Deny) {
        return None;
    }
    let (url, base, facts) = source.pull_request(&command).await?;
    if command.kind == Kind::Merge {
        let repo = repo_of_url(&url, command.number)?;
        let text = source.policy(&command, repo, &base).await?;
        let policy = parse_policy(&text)?;
        let approvers = ledger.agent_reviewers(&url).ok()?;
        if evaluate(&policy, &facts, &approvers, &agent) != Decision::Merge || !ledger.claim_auto_merge(&url, &agent).ok()? {
            return None;
        }
    }
    let allowed = match disposition {
        ToolPolicy::AskOwner => ledger.approve_tool(owner, proposal, now).and_then(|_| ledger.authorize_tool(proposal, now)).is_ok(),
        _ => ledger.authorize_tool(proposal, now).is_ok(),
    };
    if allowed && command.kind == Kind::Review {
        ledger.record_agent_review(&url, &agent).ok()?;
    }
    Some(allowed)
}
