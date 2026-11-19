//! Which of this device's Agents wrote a pull request, from this ledger's own
//! evidence (GPT batch review P1-1, Nov 18). Every Agent of an owner posts as
//! one GitHub login, so GitHub cannot say which one opened a pull request,
//! and the desktop tracker's link (closing keyword or branch name) is a
//! guess: another Agent may open the pull request that closes a tracked
//! issue. What this device authorized is not a guess: each `gh pr create
//! --head <branch>` and each push of a branch an Agent was allowed to run
//! is recorded here, and a pull request's authors are every Agent recorded
//! for its head branch. Authorship counts as known only when a recorded
//! `gh pr create` of that branch falls just before GitHub's opening time.
//!
//! Known limit: a step the ledger never authorizes (a Claude run under full
//! trust, or a card whose `gh pr create` names no `--repo`/`--head`) leaves
//! no evidence; without a matching create the gate counts no local approval.
use super::github::PrView;
use crate::outside::shell;
use crate::watch_trust::{TrustedStep, host_git::HostStep};
use crate::{Ledger, Result, ToolProposal};
use rusqlite::Connection;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS pr_branch_authors (agent TEXT NOT NULL, repo TEXT NOT NULL, branch TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('create','push')), at INTEGER NOT NULL, PRIMARY KEY(agent,repo,branch,kind,at)) STRICT;";

/// How long before GitHub's opening time a recorded `gh pr create` may be
/// authorized: a card's wait and the step's own run, with room to spare.
const CREATE_LEAD_SECS: i64 = 30 * 60;
/// How far GitHub's clock may run behind this device's.
const CLOCK_SKEW_SECS: i64 = 120;

/// Record what `proposal`'s Agent was just allowed to publish: the host's
/// push of a branch, or a `gh pr create` naming its repository and head.
pub(crate) fn note(db: &Connection, proposal: &ToolProposal, step: Option<&TrustedStep>, now: i64) -> Result<()> {
    let published = match step {
        Some(TrustedStep::Host(HostStep::Push { repo, branch, .. })) => Some(("push", repo.clone(), branch.clone())),
        _ => proposal.arguments["command"].as_str().and_then(created_branch).map(|(repo, branch)| ("create", repo, branch)),
    };
    if let Some((kind, repo, branch)) = published {
        db.execute("INSERT OR IGNORE INTO pr_branch_authors VALUES (?,lower(?),?,?,?)", (&proposal.scope.agent, &repo, &branch, kind, now))?;
    }
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
    /// The Agents recorded for `view`'s head branch in `repo`, and whether
    /// one of them created a pull request of it just before GitHub opened
    /// this one (else authorship is unknown).
    pub(crate) fn branch_authors(&self, repo: &str, view: &PrView) -> Result<(Vec<String>, bool)> {
        let Some(head) = &view.head_ref else { return Ok((Vec::new(), false)) };
        let mut statement = self.db.prepare("SELECT agent, kind, at FROM pr_branch_authors WHERE repo=lower(?) AND branch=?")?;
        let rows: Vec<(String, String, i64)> = statement.query_map((repo, &head.branch), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        let window = head.created_at - CREATE_LEAD_SECS..=head.created_at + CLOCK_SKEW_SECS;
        let known = rows.iter().any(|(_, kind, at)| kind == "create" && window.contains(at));
        let mut agents: Vec<String> = rows.into_iter().map(|(agent, _, _)| agent).collect();
        agents.sort();
        agents.dedup();
        Ok((agents, known))
    }
}

#[cfg(test)]
#[path = "authorship_tests.rs"]
mod tests;
