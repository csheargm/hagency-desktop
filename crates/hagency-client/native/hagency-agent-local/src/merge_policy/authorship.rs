//! Which of this device's Agents wrote a pull request, from this ledger's own
//! evidence (GPT batch review P1-1, Nov 18). Every Agent of an owner posts as
//! one GitHub login, so GitHub cannot say which one opened a pull request,
//! and the desktop tracker's link (closing keyword or branch name) is a
//! guess: another Agent may open the pull request that closes a tracked
//! issue. What this device authorized is not a guess: each `gh pr create
//! --head <branch>` and each push of a branch an Agent was allowed to run
//! is recorded here, and a pull request's authors are every Agent recorded
//! for its head branch. Authorship counts as known only when a `gh pr
//! create` of that branch that is known to have SUCCEEDED (held under its
//! tool call, settled with its outcome: [`Ledger::settle_publish`]) ran just
//! before GitHub's opening time; GitHub keeps one open pull request per head
//! branch, so that create opened this one (GPT review of these fixes: a
//! failed create must not vouch for another Agent's pull request).
//!
//! Known limit: a step the ledger never authorizes (a Claude run under full
//! trust, or a card whose `gh pr create` names no `--repo`/`--head`) leaves
//! no evidence; without a matching create the gate counts no local approval.
use super::github::PrView;
use crate::outside::shell;
use crate::watch_trust::{TrustedStep, host_git::HostStep};
use crate::{Ledger, Result, ToolProposal};
use rusqlite::Connection;

/// `call`: the tool call a create is held under ('' when none); `ran`: 1 once that call succeeded.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pr_branch_publishes (agent TEXT NOT NULL, repo TEXT NOT NULL, branch TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('create','push')), at INTEGER NOT NULL, call TEXT NOT NULL, ran INTEGER NOT NULL CHECK(ran IN (0,1)), PRIMARY KEY(agent,repo,branch,kind,at,call)) STRICT;";

/// How long before GitHub's opening time a recorded `gh pr create` may be
/// authorized: a card's wait and the step's own run, with room to spare.
const CREATE_LEAD_SECS: i64 = 30 * 60;
/// How far GitHub's clock may run behind this device's.
const CLOCK_SKEW_SECS: i64 = 120;

/// Record what `proposal`'s Agent was just allowed to publish: the host's
/// push of a branch, or a `gh pr create` naming its repository and head.
/// Each only names an author, never vouches (no call to settle it).
pub(crate) fn note(db: &Connection, proposal: &ToolProposal, step: Option<&TrustedStep>, now: i64) -> Result<()> {
    let published = match step {
        Some(TrustedStep::Host(HostStep::Push { repo, branch, .. })) => Some(("push", repo.clone(), branch.clone())),
        _ => proposal.arguments["command"].as_str().and_then(created_branch).map(|(repo, branch)| ("create", repo, branch)),
    };
    if let Some((kind, repo, branch)) = published {
        insert(db, &proposal.scope.agent, (kind, &repo, &branch), (now, ""))?;
    }
    Ok(())
}

fn insert(db: &Connection, agent: &str, (kind, repo, branch): (&str, &str, &str), (at, call): (i64, &str)) -> Result<()> {
    db.execute("INSERT OR IGNORE INTO pr_branch_publishes VALUES (?,lower(?),?,?,?,?,0)", (agent, repo, branch, kind, at, call))?;
    Ok(())
}

/// `(repo, branch)` of a `gh pr create` with `-R|--repo` and `-H|--head` in
/// any segment of `line` (a card may run it after a `cd`). A fork's
/// `owner:branch` head is not this repository's branch.
fn created_branch(line: &str) -> Option<(String, String)> {
    shell::parse(line)?.segments.iter().find_map(|words| {
        let start = words.windows(3).position(|w| w[0] == "gh" && w[1] == "pr" && w[2] == "create")?;
        let (mut repo, mut head) = (None, None);
        let mut rest = words[start + 3..].iter();
        while let Some(word) = rest.next() {
            let (name, inline) = word.split_once('=').filter(|(n, _)| n.starts_with("--")).map_or((word.as_str(), None), |(n, v)| (n, Some(v)));
            // A title's or body's value is skipped, never read as an option.
            let valued = matches!(name, "-R" | "--repo") || crate::watch_trust::gh::CREATE_VALUED.contains(&name);
            let value = if valued { inline.map(str::to_owned).or_else(|| rest.next().cloned()) } else { None };
            match name {
                "-R" | "--repo" => repo = value,
                "-H" | "--head" => head = value,
                _ => {}
            }
        }
        let repo = super::gate::repo_name(repo.as_deref()?)?.to_owned();
        head.filter(|h| !h.is_empty() && !h.contains(':')).map(|head| (repo, head))
    })
}

impl Ledger {
    /// Tool call `call` of `proposal`'s Agent may run a `gh pr create`: hold
    /// it, so it vouches for its pull request once the call succeeded.
    pub fn hold_create(&mut self, call: &str, proposal: &ToolProposal, now: i64) -> Result<()> {
        let created = proposal.arguments["command"].as_str().and_then(created_branch);
        match created {
            Some((repo, branch)) if !call.is_empty() => insert(&self.db, &proposal.scope.agent, ("create", &repo, &branch), (now, call)),
            _ => Ok(()),
        }
    }
    /// Tool call `call` finished: a create held under it vouches only if it succeeded.
    pub(crate) fn settle_publish(&mut self, call: &str, succeeded: bool) -> Result<()> {
        if succeeded && !call.is_empty() {
            self.db.execute("UPDATE pr_branch_publishes SET ran=1 WHERE call=?", [call])?;
        }
        Ok(())
    }
    /// The Agents recorded for `view`'s head branch in `repo`, and whether
    /// one of them demonstrably created a pull request of it just before
    /// GitHub opened this one (else authorship is unknown).
    pub(crate) fn branch_authors(&self, repo: &str, view: &PrView) -> Result<(Vec<String>, bool)> {
        let Some(head) = &view.head_ref else { return Ok((Vec::new(), false)) };
        let mut statement = self.db.prepare("SELECT agent, kind = 'create' AND ran = 1, at FROM pr_branch_publishes WHERE repo=lower(?) AND branch=?")?;
        let rows: Vec<(String, bool, i64)> = statement.query_map((repo, &head.branch), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        let window = head.created_at - CREATE_LEAD_SECS..=head.created_at + CLOCK_SKEW_SECS;
        let known = rows.iter().any(|(_, ran_create, at)| *ran_create && window.contains(at));
        let mut agents: Vec<String> = rows.into_iter().map(|(agent, _, _)| agent).collect();
        agents.sort();
        agents.dedup();
        Ok((agents, known))
    }
}

#[cfg(test)]
#[path = "authorship_tests.rs"]
mod tests;
