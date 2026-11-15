//! The auto-merge gate in front of the owner's approval card.
//!
//! OFF by default, per Agent on this device. When an Agent with the switch on
//! asks to run exactly `gh pr merge <N> --squash --match-head-commit <sha>` (or posts an approving
//! `gh pr review`), the gate reads the pull request with this device's `gh`
//! login, judges it with [`super::evaluate`] and, only on `Merge`, answers the
//! card the way the owner's click would, once per pull request. Every other
//! case returns `None`: the normal approval flow (and its card) runs unchanged,
//! so this never weakens the "ask only for outside changes" classifier.
//!
//! Approvals are recorded by Hagency itself, keyed by pull request URL, head
//! commit and reviewing Agent (a new push voids them), because every Agent here shares one GitHub identity and
//! GitHub refuses (and would not count) a self-approval.
use super::github::PrSource;
use super::{evaluate, parse_policy, Decision};
use crate::outside::{resolves_inside, shell};
use crate::{key, Ledger, Result, ToolPolicy, ToolProposal};
use std::path::{Path, PathBuf};

/// A `--comment` review whose body starts with this counts as an approval
/// (GitHub forbids `--approve` on a pull request from the same account).
pub const APPROVE_MARKER: &str = "Hagency verdict: APPROVE";

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS auto_merge (agent TEXT PRIMARY KEY, enabled INTEGER NOT NULL CHECK(enabled IN (0,1))) STRICT;
DROP TABLE IF EXISTS auto_merge_reviews;
CREATE TABLE IF NOT EXISTS auto_merge_head_reviews (pr TEXT NOT NULL, head TEXT NOT NULL, reviewer TEXT NOT NULL, PRIMARY KEY(pr,head,reviewer)) STRICT;
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
    pub(crate) fn record_agent_review(&mut self, pr: &str, head: &str, reviewer: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO auto_merge_head_reviews VALUES (?,?,?)", (pr, head, reviewer))?;
        Ok(())
    }
    /// The Agents that approved `pr` at exactly commit `head`.
    pub(crate) fn agent_reviewers(&self, pr: &str, head: &str) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT reviewer FROM auto_merge_head_reviews WHERE pr=? AND head=?")?;
        let rows = statement.query_map([pr, head], |r| r.get(0))?;
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
    /// `--match-head-commit <sha>` of a merge: GitHub refuses the merge when
    /// the head moved, so the commit judged is the commit merged.
    pub head: Option<String>,
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

/// Read `gh pr merge <N> --squash [--match-head-commit <sha>] [-d] [-R o/r]`, `gh pr ready <N> [-R o/r]` or an
/// approving `gh pr review <N> (--approve | --comment -b "<marker>...") [-R o/r]`,
/// optionally after one `cd <dir> &&` that stays in the workspace.
pub fn parse_command(proposal: &ToolProposal) -> Option<GhPr> {
    if proposal.tool != "claude.Bash" || proposal.risk == "permission_expansion" {
        return None;
    }
    let line = proposal.arguments["command"].as_str()?;
    if line.contains([';', '|', '\n', '`', '$', '<', '>']) {
        return None;
    }
    let cwd = Path::new(&proposal.canonical_directory);
    let parsed = shell::parse(line)?;
    let (dir, words) = match parsed.segments.as_slice() {
        [words] => (cwd.to_path_buf(), words),
        [cd, words] if line.contains("&&") => match cd.as_slice() {
            [c, target] if c == "cd" && !target.starts_with('-') && resolves_inside(target, cwd) => (shell::lexical_join(cwd, target), words),
            _ => return None,
        },
        _ => return None,
    };
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
        Kind::Merge => flags.squash,
        Kind::Review => flags.approve != flags.comment && (flags.approve || flags.body.starts_with(APPROVE_MARKER)),
        Kind::Ready => true,
    };
    if !ok {
        return None;
    }
    Some(GhPr { dir, number: number.parse().ok()?, repo: flags.repo, head: flags.head, kind })
}

#[derive(Default)]
struct Flags {
    squash: bool,
    approve: bool,
    comment: bool,
    body: String,
    repo: Option<String>,
    head: Option<String>,
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
                "--delete-branch" | "-d" if merge => {}
                "--match-head-commit" if merge => flags.head = Some(super::github::commit_sha(words.next()?)?.to_owned()),
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
pub(crate) fn repo_name(text: &str) -> Option<&str> {
    let (owner, name) = text.split_once('/')?;
    let part = |s: &str| !s.is_empty() && !s.starts_with(['-', '.']) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    (part(owner) && part(name)).then_some(text)
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
    let view = source.pull_request(&command).await?;
    if command.kind == Kind::Merge {
        // Unbound merges always go to the owner: without the flag GitHub
        // would merge whatever the head is when the command runs.
        if command.head.as_deref() != Some(view.head_sha.as_str()) {
            return None;
        }
        let repo = super::github::repo_of_url(&view.url, command.number)?;
        let policy = parse_policy(&source.policy(&command, repo, &view.base_sha).await?)?;
        let approvers = ledger.agent_reviewers(&view.url, &view.head_sha).ok()?;
        if evaluate(&policy, &view.facts, &approvers, &agent) != Decision::Merge || !ledger.claim_auto_merge(&view.url, &agent).ok()? {
            return None;
        }
    }
    if command.kind == Kind::Review && !view.facts.open {
        return None;
    }
    let allowed = match disposition {
        ToolPolicy::AskOwner => ledger.approve_tool(owner, proposal, now).and_then(|_| ledger.authorize_tool(proposal, now)).is_ok(),
        _ => ledger.authorize_tool(proposal, now).is_ok(),
    };
    if allowed && command.kind == Kind::Review {
        // Residual race: a push between this read and the review lands an
        // approval on the old head; it then never matches the new one.
        ledger.record_agent_review(&view.url, &view.head_sha, &agent).ok()?;
    }
    Some(allowed)
}
