//! What a gate-judged command does to the ledger once it has RUN, not when
//! it is asked (integration GPT review, Nov 17):
//!
//! - a `--request-changes` review withdraws this Agent's recorded approvals
//!   of the pull request only when that command completes successfully,
//!   whoever authorized it (the gate, a rule or the owner's card). A denied,
//!   failed or never-run request no longer removes an approval another Agent
//!   needs. Approvals still waiting to settle are dropped at once: the
//!   reviewer's latest word is no longer an approval.
//! - the one automatic merge is taken per pull request AND head commit, and
//!   given back when the merge it was taken for was not authorized or did not
//!   succeed (GitHub refused it, the head moved, the call failed). A merge whose
//!   outcome is never reported keeps its claim; a new head can still merge.
use crate::{Ledger, Result};

pub(crate) const SCHEMA: &str = "DROP TABLE IF EXISTS auto_merge_used;
CREATE TABLE IF NOT EXISTS auto_merge_head_used (pr TEXT NOT NULL, head TEXT NOT NULL, agent TEXT NOT NULL, PRIMARY KEY(pr,head)) STRICT;
CREATE TABLE IF NOT EXISTS auto_merge_pending_claims (call TEXT PRIMARY KEY, pr TEXT NOT NULL, head TEXT NOT NULL) STRICT;
CREATE TABLE IF NOT EXISTS auto_merge_pending_withdrawals (call TEXT PRIMARY KEY, pr TEXT NOT NULL, reviewer TEXT NOT NULL) STRICT;";

impl Ledger {
    /// Take the one automatic merge of `pr` at `head`; false when it was already taken.
    pub(crate) fn claim_auto_merge(&mut self, pr: &str, head: &str, agent: &str) -> Result<bool> {
        Ok(self.db.execute("INSERT OR IGNORE INTO auto_merge_head_used VALUES (?,?,?)", (pr, head, agent))? == 1)
    }
    /// Give back the automatic merge of `pr` at `head`: the merge never happened.
    pub(crate) fn release_auto_merge(&mut self, pr: &str, head: &str) -> Result<()> {
        self.db.execute("DELETE FROM auto_merge_head_used WHERE pr=? AND head=?", (pr, head))?;
        Ok(())
    }
    /// The merge claim taken for tool call `call`, settled by [`Ledger::settle_auto_merge`].
    pub(crate) fn hold_auto_merge(&mut self, call: &str, pr: &str, head: &str) -> Result<()> {
        self.db.execute("INSERT OR REPLACE INTO auto_merge_pending_claims VALUES (?,?,?)", (call, pr, head))?;
        Ok(())
    }
    /// Tool call `call` finished: a merge claim held under it is kept only if it succeeded.
    pub(crate) fn settle_auto_merge(&mut self, call: &str, succeeded: bool) -> Result<()> {
        use rusqlite::OptionalExtension;
        let held: Option<(String, String)> = self
            .db
            .query_row("DELETE FROM auto_merge_pending_claims WHERE call=? RETURNING pr, head", [call], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match held {
            Some((pr, head)) if !succeeded => self.release_auto_merge(&pr, &head),
            _ => Ok(()),
        }
    }
    /// `reviewer` asks for changes under tool call `call`: its unsettled
    /// approvals of `pr` stop at once, its recorded ones once the call succeeds.
    /// Without a call id nothing could settle it, so all are withdrawn now.
    pub(crate) fn hold_withdrawal(&mut self, call: &str, pr: &str, reviewer: &str) -> Result<()> {
        if call.is_empty() {
            return self.withdraw_agent_review(pr, reviewer);
        }
        self.db.execute("DELETE FROM auto_merge_pending_reviews WHERE pr=? AND reviewer=?", (pr, reviewer))?;
        self.db.execute("INSERT OR REPLACE INTO auto_merge_pending_withdrawals VALUES (?,?,?)", (call, pr, reviewer))?;
        Ok(())
    }
    /// Tool call `call` finished: a request for changes held under it
    /// withdraws the reviewer's approvals only if it succeeded.
    pub fn settle_withdrawal(&mut self, call: &str, succeeded: bool) -> Result<()> {
        use rusqlite::OptionalExtension;
        let held: Option<(String, String)> = self
            .db
            .query_row("DELETE FROM auto_merge_pending_withdrawals WHERE call=? RETURNING pr, reviewer", [call], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        match held {
            Some((pr, reviewer)) if succeeded => self.withdraw_agent_review(&pr, &reviewer),
            _ => Ok(()),
        }
    }
    /// A Claude tool call finished: everything the gate held under it settles
    /// with its outcome (Claude reports one outcome for every call).
    pub fn settle_tool_call(&mut self, call: &str, succeeded: bool) -> Result<()> {
        self.settle_withdrawal(call, succeeded)?;
        self.settle_agent_review(call, succeeded)
    }
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod tests;
