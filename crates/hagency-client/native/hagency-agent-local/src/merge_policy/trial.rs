//! Trial mode for a new watch (ADR 0026): the first [`TRIAL_PRS`] pull
//! requests of a newly created watch need the owner to merge, even where the
//! merge policy would let an Agent merge them. The Agent still opens and
//! reviews them; the gate holds only the merge ([`hold`]), with a reason the
//! room notice quotes as "PR k of N".
//!
//! The count is the owner's: a pull request counts once GitHub reports it
//! merged for an issue this watch tracked, recorded through the owner API
//! ([`Ledger::count_trial_merge`]). A merge the gate answered itself (the one
//! automatic merge) never counts, so an Agent cannot earn its own way out.
//! Only the owner ends a trial early ([`Ledger::end_watch_trial`]).
//!
//! A watch with no trial row is past its trial: every watch created before
//! this existed, so the running watches keep merging as before.
use super::Decision;
use crate::issue_track::State;
use crate::{key, Ledger, Result};

/// Pull requests the owner merges before a new watch merges by itself.
pub const TRIAL_PRS: u32 = 3;

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS watch_trial (agent TEXT NOT NULL, repo TEXT NOT NULL, required INTEGER NOT NULL, ended INTEGER NOT NULL DEFAULT 0 CHECK(ended IN (0,1)), PRIMARY KEY(agent,repo)) STRICT;
CREATE TABLE IF NOT EXISTS watch_trial_merges (agent TEXT NOT NULL, repo TEXT NOT NULL, pr INTEGER NOT NULL, PRIMARY KEY(agent,repo,pr)) STRICT;";

const REASON_PREFIX: &str = "trial: pull request ";
const REASON_SUFFIX: &str = " needs the owner to merge";

/// The gate's reason while a trial holds the merge of pull request `next` of `required`.
pub fn trial_reason(next: u32, required: u32) -> String {
    format!("{REASON_PREFIX}{next} of {required}{REASON_SUFFIX}")
}

/// `(next, required)` of exactly a [`trial_reason`]; anything else is `None`,
/// so a file or author name in another reason never passes for it.
pub fn trial_counts(reason: &str) -> Option<(u32, u32)> {
    let digits = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten();
    let middle = reason.strip_prefix(REASON_PREFIX)?.strip_suffix(REASON_SUFFIX)?;
    let (next, required) = middle.split_once(" of ")?;
    Some((digits(next)?, digits(required)?))
}

/// The trial's hold on a merge by `agent` in `repo`, when one is running.
pub(crate) fn hold(ledger: &Ledger, agent: &str, repo: &str) -> Result<Option<Decision>> {
    Ok(ledger.trial_status(agent, repo)?.map(|(merged, required)| Decision::NeedsHuman(trial_reason(merged + 1, required))))
}

impl Ledger {
    /// `(merged by the owner, required)` while `agent`'s watch of `repo` is
    /// in its trial; `None` once it ended, or for a watch without one.
    pub fn trial_status(&self, agent: &str, repo: &str) -> Result<Option<(u32, u32)>> {
        use rusqlite::OptionalExtension;
        let row: Option<(i64, i64, i64)> = self
            .db
            .query_row(
                "SELECT required, ended, (SELECT COUNT(*) FROM watch_trial_merges m WHERE m.agent=t.agent AND m.repo=t.repo) FROM watch_trial t WHERE agent=? AND repo=?",
                (agent, repo.to_ascii_lowercase()),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(row.and_then(|(required, ended, merged)| {
            let (required, merged) = (u32::try_from(required).ok()?, u32::try_from(merged).ok()?);
            (ended == 0 && merged < required).then_some((merged, required))
        }))
    }
    /// The owner ends the trial of `agent`'s watch of `repo` early.
    pub fn end_watch_trial(&mut self, owner: &str, agent: &str, repo: &str) -> Result<()> {
        self.owner(owner)?;
        key(agent)?;
        self.db.execute("UPDATE watch_trial SET ended=1 WHERE agent=? AND repo=?", (agent, repo.to_ascii_lowercase()))?;
        Ok(())
    }
    /// A tracked issue of `agent` in `repo` was recorded merged: its pull
    /// request counts toward the trial unless the gate merged it itself.
    /// True when it counted.
    pub fn count_trial_merge(&mut self, owner: &str, agent: &str, repo: &str, issue: i64) -> Result<bool> {
        self.owner(owner)?;
        let track = self.issue_tracks(agent)?.into_iter().find(|t| t.repo.eq_ignore_ascii_case(repo) && t.number == issue);
        let Some(pr) = track.filter(|t| t.state == State::Merged).and_then(|t| t.pr) else { return Ok(false) };
        if self.trial_status(agent, repo)?.is_none() || self.auto_merged(repo, pr)? {
            return Ok(false);
        }
        Ok(self.db.execute("INSERT OR IGNORE INTO watch_trial_merges VALUES (?,?,?)", (agent, repo.to_ascii_lowercase(), pr as i64))? == 1)
    }
    /// Whether the gate took the one automatic merge of pull request `pr` of `repo`.
    fn auto_merged(&self, repo: &str, pr: u64) -> Result<bool> {
        let url = format!("https://github.com/{repo}/pull/{pr}");
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM auto_merge_head_used WHERE lower(pr)=lower(?))", [url], |r| r.get(0))?)
    }
    /// A new watch of `repo` starts its trial; an existing trial row is kept,
    /// so removing and re-adding a watch never resets its count.
    pub(crate) fn start_watch_trial(&mut self, agent: &str, repo: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO watch_trial (agent, repo, required) VALUES (?,?,?)", (agent, repo.to_ascii_lowercase(), TRIAL_PRS as i64))?;
        Ok(())
    }
    /// The repositories on `agent`'s work list now (lowercase, as stored).
    pub(crate) fn work_repos(&self, agent: &str) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT repo FROM agent_work WHERE agent=?")?;
        let rows = statement.query_map([agent], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
    /// Start a trial for each work-list repository not in `before`.
    pub(crate) fn start_new_watch_trials(&mut self, agent: &str, before: &[String]) -> Result<()> {
        for repo in self.work_repos(agent)?.into_iter().filter(|r| !before.contains(r)) {
            self.start_watch_trial(agent, &repo)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "trial_tests.rs"]
mod tests;
