//! A review request whose reviewer never answered moves on to the next
//! reviewer (GPT batch review): the desktop picks any joined remote Agent
//! whose profile publishes a runtime, with no sign that it is active, so a
//! paused remote Codex Agent kept being picked and every re-send of the
//! head's review went to it. Once a reviewer's rounds are spent
//! ([`super::step_resend`]), it is recorded as passed for that head; the
//! desktop then picks among the others (`stepPassed`), and the first request
//! to a reviewer not yet asked is given back as a fresh round of its own.
use crate::{Ledger, Result};

/// `review_loop_passed`: per `agent`'s pull request head, each reviewer
/// that never answered, and the round its requests ended at.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS review_loop_passed (agent TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, reviewer TEXT NOT NULL, round INTEGER NOT NULL, PRIMARY KEY(agent,pr,head,reviewer)) STRICT;";

impl Ledger {
    /// The reviewers that never answered `agent`'s review request for `pr`
    /// at `head`, in the order they were passed over.
    pub fn passed_reviewers(&self, agent: &str, pr: &str, head: &str) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT reviewer FROM review_loop_passed WHERE agent=? AND lower(pr)=lower(?) AND head=? ORDER BY round, reviewer")?;
        let rows = statement.query_map((agent, pr, head), |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `reviewer`'s rounds for this head are spent at `round`.
    pub(super) fn pass_reviewer(&self, agent: &str, (pr, head): (&str, &str), reviewer: &str, round: u32) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO review_loop_passed VALUES (?,?,?,?,?)", (agent, pr, head, reviewer, round))?;
        Ok(())
    }

    /// The first round of the current reviewer's requests: one past the
    /// newest pass, 0 while nobody was passed over.
    pub(super) fn reviewer_base(&self, agent: &str, (pr, head): (&str, &str)) -> Result<u32> {
        const BASE: &str = "SELECT COALESCE(MAX(round)+1, 0) FROM review_loop_passed WHERE agent=? AND lower(pr)=lower(?) AND head=?";
        Ok(self.db.query_row(BASE, (agent, pr, head), |r| r.get(0))?)
    }

    /// Whether the review goes to `to` afresh at round `again`: some reviewer
    /// was passed over at exactly this round and `to` was never passed.
    pub(super) fn hands_on(&self, agent: &str, (pr, head): (&str, &str), to: &str, again: u32) -> Result<bool> {
        let passed = self.passed_reviewers(agent, pr, head)?;
        Ok(!passed.is_empty() && !passed.iter().any(|p| p == to) && self.reviewer_base(agent, (pr, head))? == again + 1)
    }
}
