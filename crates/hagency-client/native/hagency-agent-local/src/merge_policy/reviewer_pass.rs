//! A review request whose reviewer never answered moves on to the next
//! reviewer (GPT batch review): the desktop picks any joined remote Agent
//! whose profile publishes a runtime, with no sign that it is active, so a
//! paused remote Codex Agent kept being picked and every re-send of the
//! head's review went to it. Each reviewer the head's review was posted to
//! is noted with the round its requests started at. Once a reviewer's own
//! rounds are spent ([`super::step_resend`]) it is recorded as passed; the
//! desktop then picks among the others (`stepPassed`), and the first request
//! to a reviewer never asked for this head is given back as a fresh round of
//! its own. A reviewer is passed only after a request went to it, never on
//! another reviewer's timer, and a used-up budget never passes one.
use super::{Say, StepDue, UNSEEN_AFTER_SECS, Why};
use crate::{Ledger, Result};

/// `review_loop_asked`: per `agent`'s pull request head, each reviewer its
/// review was posted to and the round that started; `review_loop_passed`:
/// each one that never answered, and the round its requests ended at.
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS review_loop_asked (agent TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, reviewer TEXT NOT NULL, base INTEGER NOT NULL, PRIMARY KEY(agent,pr,head,reviewer)) STRICT; \
CREATE TABLE IF NOT EXISTS review_loop_passed (agent TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, reviewer TEXT NOT NULL, round INTEGER NOT NULL, PRIMARY KEY(agent,pr,head,reviewer)) STRICT;";

const KEY: &str = "agent=? AND lower(pr)=lower(?) AND head=?";

impl Ledger {
    /// The reviewers that never answered `agent`'s review request for `pr`
    /// at `head`, in the order they were passed over.
    pub fn passed_reviewers(&self, agent: &str, pr: &str, head: &str) -> Result<Vec<String>> {
        let mut statement = self.db.prepare(&format!("SELECT reviewer FROM review_loop_passed WHERE {KEY} ORDER BY round, reviewer"))?;
        let rows = statement.query_map((agent, pr, head), |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// `reviewer`'s rounds for this head are spent at `round`.
    pub(super) fn pass_reviewer(&self, agent: &str, (pr, head): (&str, &str), reviewer: &str, round: u32) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO review_loop_passed VALUES (?,?,?,?,?)", (agent, pr, head, reviewer, round))?;
        Ok(())
    }

    /// The head's review is posted to `reviewer` from round `base` on. Its
    /// first round stays while it is the newest reviewer asked; a reviewer
    /// the review returns to starts a stint of its own (round-2 GPT review
    /// P2-4: it never inherits another reviewer's rounds or unseen timer).
    pub(super) fn note_asked(&self, agent: &str, (pr, head): (&str, &str), reviewer: &str, base: u32) -> Result<()> {
        if self.newest_asked(agent, (pr, head))?.as_deref() != Some(reviewer) {
            self.db.execute("INSERT OR REPLACE INTO review_loop_asked VALUES (?,?,?,?,?)", (agent, pr, head, reviewer, base))?;
        }
        Ok(())
    }

    /// The first round of `reviewer`'s current stint (0 while none was noted).
    pub(super) fn reviewer_base(&self, agent: &str, (pr, head): (&str, &str), reviewer: &str) -> Result<u32> {
        let sql = format!("SELECT COALESCE(MAX(base), 0) FROM review_loop_asked WHERE {KEY} AND reviewer=?");
        Ok(self.db.query_row(&sql, (agent, pr, head, reviewer), |r| r.get(0))?)
    }

    /// The reviewer the head's review was last handed to.
    fn newest_asked(&self, agent: &str, (pr, head): (&str, &str)) -> Result<Option<String>> {
        use rusqlite::OptionalExtension;
        let sql = format!("SELECT reviewer FROM review_loop_asked WHERE {KEY} ORDER BY base DESC, rowid DESC LIMIT 1");
        Ok(self.db.query_row(&sql, (agent, pr, head), |r| r.get(0)).optional()?)
    }

    /// Whether the review goes to `to` afresh: `to` was never passed over,
    /// and the review was last handed to another reviewer (or, with none
    /// noted, one was passed over).
    pub(super) fn hands_on(&self, agent: &str, (pr, head): (&str, &str), to: &str) -> Result<bool> {
        let passed = |who: Option<&str>| -> Result<bool> {
            let sql = format!("SELECT COUNT(*) FROM review_loop_passed WHERE {KEY} AND (?4 IS NULL OR reviewer=?4)");
            Ok(self.db.query_row(&sql, rusqlite::params![agent, pr, head, who], |r| r.get::<_, i64>(0))? > 0)
        };
        if passed(Some(to))? {
            return Ok(false);
        }
        Ok(match self.newest_asked(agent, (pr, head))? {
            Some(newest) => newest != to,
            None => passed(None)?,
        })
    }
    /// [`Ledger::step_due`] of the `review` step for `to`, another owner's
    /// Agent whose runs only its owner's device sees (live PR #121, Nov 18:
    /// "no run of it reached this device" was said of bird-codex, and the
    /// review was sent again). No dispatch is looked for and nothing is
    /// re-sent: once [`UNSEEN_AFTER_SECS`] passed without the desktop seeing
    /// its verdict on GitHub, `to` is passed over and the round's one line
    /// is [`Why::Unanswered`].
    pub fn remote_review_due(&mut self, agent: &str, (pr, head): (&str, &str), to: &str, now: i64) -> Result<StepDue> {
        let round = self.round(agent, "review", pr, head)?;
        let kept = StepDue { taken: true, again: round.again, say: None };
        if !self.loop_step_taken(agent, "review", pr, head)? {
            self.note_asked(agent, (pr, head), to, round.again)?;
            return Ok(StepDue { taken: false, ..kept });
        }
        if self.hands_on(agent, (pr, head), to)? {
            self.note_asked(agent, (pr, head), to, round.again + 1)?;
            return Ok(StepDue { taken: false, again: self.give_back(agent, ("review", pr, head), now, String::new(), round.again + 1)?, say: None });
        }
        let line = (round.said <= round.again).then_some(Say { why: Why::Unanswered, gave_up: true, round: round.again });
        if self.passed_reviewers(agent, pr, head)?.iter().any(|r| r == to) {
            return Ok(StepDue { say: line, ..kept });
        }
        // Its own clock: a dispatch an own reviewer's stint saw (-1) never stops it.
        self.db.execute("INSERT OR IGNORE INTO review_loop_unseen VALUES (?,'review',?,?,?)", (agent, pr, head, now))?;
        self.db.execute("UPDATE review_loop_unseen SET first_at=? WHERE agent=? AND step='review' AND pr=? AND head=? AND first_at<0", (now, agent, pr, head))?;
        let first: i64 = self.db.query_row("SELECT first_at FROM review_loop_unseen WHERE agent=? AND step='review' AND pr=? AND head=?", (agent, pr, head), |r| r.get(0))?;
        if now - first < UNSEEN_AFTER_SECS {
            return Ok(kept);
        }
        self.pass_reviewer(agent, (pr, head), to, round.again)?;
        Ok(StepDue { say: line, ..kept })
    }
}
