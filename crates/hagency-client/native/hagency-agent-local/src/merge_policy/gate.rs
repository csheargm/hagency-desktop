//! The auto-merge gate in front of the owner's approval card.
//!
//! OFF by default, per Agent on this device. When an Agent with the switch on
//! asks to run exactly `gh pr merge <N> --squash --match-head-commit <sha>` (or posts an approving
//! `gh pr review`), the gate reads the pull request with this device's `gh`
//! login, judges it with [`super::evaluate`] and, only on `Merge`, answers the
//! card the way the owner's click would, once per pull request. `gh pr ready`
//! and approving reviews skip the card only for a pull request that is a
//! low-risk change under the same policy (everything but the approvals). Every other
//! case returns `None`: the normal approval flow (and its card) runs unchanged,
//! so this never weakens the "ask only for outside changes" classifier.
//!
//! Approvals are recorded by Hagency itself, keyed by pull request URL, head
//! commit and reviewing Agent (a new push voids them), because every Agent here shares one GitHub identity and
//! GitHub refuses (and would not count) a self-approval.
use super::github::PrSource;
use super::Decision;
use crate::outside::{resolves_inside, shell};
use crate::{key, Ledger, Result, ToolPolicy, ToolProposal};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// A `--comment` review whose body starts with this counts as an approval
/// (GitHub forbids `--approve` on a pull request from the same account).
pub const APPROVE_MARKER: &str = "Hagency verdict: APPROVE";

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS auto_merge (agent TEXT PRIMARY KEY, enabled INTEGER NOT NULL CHECK(enabled IN (0,1))) STRICT;
DROP TABLE IF EXISTS auto_merge_reviews;
CREATE TABLE IF NOT EXISTS auto_merge_head_reviews (pr TEXT NOT NULL, head TEXT NOT NULL, reviewer TEXT NOT NULL, PRIMARY KEY(pr,head,reviewer)) STRICT;
CREATE TABLE IF NOT EXISTS auto_merge_pending_reviews (call TEXT PRIMARY KEY, pr TEXT NOT NULL, head TEXT NOT NULL, reviewer TEXT NOT NULL) STRICT;";

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
    /// `reviewer` asked for changes: none of its approvals of `pr` count any more.
    pub(crate) fn withdraw_agent_review(&mut self, pr: &str, reviewer: &str) -> Result<()> {
        self.db.execute("DELETE FROM auto_merge_head_reviews WHERE pr=? AND reviewer=?", (pr, reviewer))?;
        // A held approval must not settle afterwards and restore it.
        self.db.execute("DELETE FROM auto_merge_pending_reviews WHERE pr=? AND reviewer=?", (pr, reviewer))?;
        Ok(())
    }
    pub(crate) fn record_agent_review(&mut self, pr: &str, head: &str, reviewer: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO auto_merge_head_reviews VALUES (?,?,?)", (pr, head, reviewer))?;
        Ok(())
    }
    /// An allowed review not yet run: it counts only once tool call `call`
    /// reports success ([`Ledger::settle_agent_review`]).
    pub(crate) fn hold_agent_review(&mut self, call: &str, pr: &str, head: &str, reviewer: &str) -> Result<()> {
        self.db.execute("INSERT OR REPLACE INTO auto_merge_pending_reviews VALUES (?,?,?,?)", (call, pr, head, reviewer))?;
        Ok(())
    }
    /// Tool call `call` finished: a held review counts only if it succeeded.
    /// A merge claim held under it is given back unless it succeeded ([`super::pending`]).
    pub fn settle_agent_review(&mut self, call: &str, succeeded: bool) -> Result<()> {
        use rusqlite::OptionalExtension;
        self.settle_auto_merge(call, succeeded)?;
        self.atomically(|ledger| {
            let held: Option<(String, String, String)> = ledger
                .db
                .query_row("DELETE FROM auto_merge_pending_reviews WHERE call=? RETURNING pr, head, reviewer", [call], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .optional()?;
            match held {
                Some((pr, head, reviewer)) if succeeded => ledger.record_agent_review(&pr, &head, &reviewer),
                _ => Ok(()),
            }
        })
    }
    /// The Agents that approved `pr` at exactly commit `head`.
    pub(crate) fn agent_reviewers(&self, pr: &str, head: &str) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT reviewer FROM auto_merge_head_reviews WHERE pr=? AND head=?")?;
        let rows = statement.query_map([pr, head], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
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
    /// A review's `-b` body: an approval must name the head commit it reviewed.
    pub body: String,
}

/// What the command does to the pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `gh pr merge`: judged by the policy.
    Merge,
    /// An approving `gh pr review`: recorded as this Agent's approval.
    Review,
    /// `gh pr review --request-changes`: a verdict that blocks, never an approval.
    RequestChanges,
    /// `gh pr ready`: a draft is marked ready so it can be reviewed and merged.
    Ready,
}

/// Read `gh pr merge <N> --squash --match-head-commit <sha> -R o/r`, `gh pr ready <N> -R o/r` or an
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
        Kind::Review => [flags.approve, flags.comment, flags.request_changes].iter().filter(|f| **f).count() == 1
            && (flags.approve || flags.request_changes || flags.body.starts_with(APPROVE_MARKER)),
        Kind::Ready => flags.repo.is_some(),
        Kind::RequestChanges => false,
    };
    if !ok {
        return None;
    }
    let kind = if flags.request_changes { Kind::RequestChanges } else { kind };
    Some(GhPr { dir, number: number.parse().ok()?, repo: flags.repo, head: flags.head, kind, body: flags.body })
}

/// Characters that would chain, redirect or substitute; any one refuses the command.
const SHELL_METACHARACTERS: [char; 7] = [';', '|', '\n', '`', '$', '<', '>'];

/// The directory and words of a command proposal. `claude.Bash` and Codex's
/// `codex.command` give one shell line; Codex may instead give an argv array,
/// read as `/bin/bash|/bin/sh -c <line>` (never a login `-lc`) or else as plain words that must hold no
/// shell metacharacter. Every other tool (`codex.network` included) is `None`.
fn command_words(proposal: &ToolProposal) -> Option<(PathBuf, Vec<String>)> {
    let cwd = Path::new(&proposal.canonical_directory);
    let command = &proposal.arguments["command"];
    match (proposal.tool.as_str(), command) {
        ("claude.Bash", Value::String(line)) => shell_words(line, cwd),
        // The app-server renders Codex's argv as one shell-quoted line: a
        // `<system shell> -c '<line>'` wrapper is read exactly as the argv is.
        // TODO(R4): gate.rs is over budget; split command reading into gate/command.rs.
        ("codex.command", Value::String(line)) => match shell_words(line, cwd)? {
            // One outer segment only: never after a `cd` (GPT review: `cd . &&`).
            (_, words) if words.len() == 3 && words[1] == "-c" && is_shell(&words[0]) && shell::parse(line)?.segments.len() == 1 => shell_words(&words[2], cwd),
            read => Some(read),
        },
        ("codex.command", Value::Array(argv)) => {
            let argv: Vec<&str> = argv.iter().map(Value::as_str).collect::<Option<_>>()?;
            match argv.as_slice() {
                [sh, "-c", line] if is_shell(sh) => shell_words(line, cwd),
                words if words.iter().all(|w| !w.contains(SHELL_METACHARACTERS)) => Some((cwd.to_path_buf(), words.iter().map(|w| (*w).to_owned()).collect())),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A system shell that reads no startup file under `-c` (the launch env holds no
/// BASH_ENV/ENV): an exact absolute path only, never a bare name looked up on
/// `PATH` or a workspace path. zsh is out: `zsh -c` sources `~/.zshenv`.
fn is_shell(program: &str) -> bool {
    matches!(program, "/bin/bash" | "/bin/sh" | "/usr/bin/bash")
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
    request_changes: bool,
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
                "--match-head-commit" if merge => flags.head = Some(super::github::commit_sha(words.next()?)?.to_owned()),
                "--approve" | "-a" if review => flags.approve = true,
                "--comment" | "-c" if review => flags.comment = true,
                "--request-changes" | "-r" if review => flags.request_changes = true,
                "--body" | "-b" if review => flags.body = (*words.next()?).to_owned(),
                "--repo" | "-R" => flags.repo = Some(repo_name(words.next()?)?.to_owned()),
                _ => return None,
            }
        }
        Some(flags)
    }
}

/// Whether an approving review's body names exactly `head` (as a word), so
/// the approval is of the commit the reviewer read, not one pushed since.
pub fn review_names_head(body: &str, head: &str) -> bool {
    body.split(|c: char| !c.is_ascii_hexdigit()).any(|word| word == head)
}

/// `owner/name` on github.com and nothing else (no host, no options).
pub(crate) fn repo_name(text: &str) -> Option<&str> {
    let (owner, name) = text.split_once('/')?;
    let part = |s: &str| !s.is_empty() && !s.starts_with(['-', '.']) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    (part(owner) && part(name)).then_some(text)
}

/// Answer `proposal` without a card when the policy allows it, else `None`.
/// `call` is the tool call's id; an allowed review is held under it and
/// counts only after that call succeeds (an empty id never counts).
pub async fn auto_answer(ledger: &mut Ledger, owner: &str, proposal: &ToolProposal, call: &str, now: i64, source: &impl PrSource) -> Option<bool> {
    // A background call's launch result is not the command's outcome, so it
    // must never settle a review, merge or ready (GPT review): the card asks.
    if proposal.arguments["run_in_background"] == true {
        return None;
    }
    let command = parse_command(proposal)?;
    let agent = proposal.scope.agent.clone();
    if command.kind == Kind::RequestChanges {
        // Whatever answers it (this gate or the owner's card), a blocking
        // verdict withdraws this Agent's approvals once it has run ([`super::pending`]).
        // An unreadable pull request leaves the card to the owner.
        let url = source.pull_request(&command).await?.url;
        ledger.hold_withdrawal(call, &url, &agent).ok()?;
    }
    if !ledger.auto_merge(&agent).ok()? {
        return None;
    }
    let disposition = ledger.tool_disposition(proposal, now).ok()?;
    if matches!(disposition, ToolPolicy::Deny) {
        return None;
    }
    let view = source.pull_request(&command).await?;
    if !qualifies(ledger, &command, &view, &agent, source).await? {
        return None;
    }
    let allowed = match disposition {
        ToolPolicy::AskOwner => ledger.approve_tool(owner, proposal, now).and_then(|_| ledger.authorize_tool(proposal, now)).is_ok(),
        _ => ledger.authorize_tool(proposal, now).is_ok(),
    };
    if command.kind == Kind::Merge {
        // The claim `qualifies` took counts once this merge has run ([`super::pending`]).
        if !allowed {
            ledger.release_auto_merge(&view.url, &view.head_sha).ok()?;
        } else if !call.is_empty() {
            ledger.hold_auto_merge(call, &view.url, &view.head_sha).ok()?;
        }
    }
    if allowed && command.kind == Kind::Review && !call.is_empty() {
        // Residual race: a push between this read and the review lands an
        // approval on the old head; it then never matches the new one.
        ledger.hold_agent_review(call, &view.url, &view.head_sha, &agent).ok()?;
    }
    Some(allowed)
}

/// Whether `command` may skip the card. Every kind needs the pull request at
/// its current head to be a low-risk change under the policy at its base
/// commit; a review also needs the policy's CI; a merge also needs the head it names and the policy's approvals
/// (and takes the one automatic merge). `None` when facts cannot be read.
async fn qualifies(ledger: &mut Ledger, command: &GhPr, view: &super::github::PrView, agent: &str, source: &impl PrSource) -> Option<bool> {
    let repo = super::github::repo_of_url(&view.url, command.number)?;
    if command.repo.as_deref().is_some_and(|named| !named.eq_ignore_ascii_case(repo)) {
        return Some(false);
    }
    let judged = super::work::judged(ledger, agent, command, repo, view, source).await?;
    let merge = command.kind == Kind::Merge;
    let scoped: Vec<&super::MergePolicy> = if merge { judged.policies().collect() } else { judged.review_policies().collect() };
    if scoped.iter().any(|policy| super::file_reason(policy, &view.facts).is_some()) {
        return Some(false);
    }
    // An approval is public and counts toward a merge, so CI must pass for
    // it as for a merge; readying a draft (CI may not have run yet) need not.
    match command.kind {
        Kind::Ready | Kind::RequestChanges => return Some(true),
        // An approval names the head it reviewed, must be of a head that is
        // still current, and (under the owner's rules) of a trusted author.
        Kind::Review => {
            let bound = review_names_head(&command.body, &view.head_sha);
            return Some(bound && scoped.iter().all(|policy| super::ci_reason(policy, &view.facts).is_none()));
        }
        Kind::Merge => {}
    }
    // Unbound merges always go to the owner: without the flag GitHub
    // would merge whatever the head is when the command runs.
    if command.head.as_deref() != Some(view.head_sha.as_str()) {
        return Some(false);
    }
    let decision = super::review_loop::judge(ledger, command, repo, view, agent, source).await?;
    Some(decision == Decision::Merge && ledger.claim_auto_merge(&view.url, &view.head_sha, agent).ok()?)
}
