//! A review-loop step whose dispatch never ran is sent again (Nov 18, PR
//! #94): the loop records each `review` / `merge` step once per head
//! (post, then claim), so a merge notice the merging Agent answered with an
//! owner-policy notice (a used-up token budget) or lost left the pull
//! request stuck. Each received loop notice is tagged with its step, pull
//! request and head; when the newest one this owner's account sent ended
//! without the Agent running, the claim is given back so the next check
//! posts the step again: never while the Agent's budget is used up, at
//! least [`RESEND_AFTER_SECS`] after the refused dispatch, and at most
//! [`MAX_RESENDS`] times per head. A step that ran is never sent again, and
//! a merge whose one automatic merge was taken never is either. A review is
//! sent again only after the zero-call proof (a lost review may have posted
//! its verdict); only the notice addressed to the step's Agent counts.
use crate::activity::Outcome;
use crate::merge_policy::github::commit_sha;
use crate::{Ledger, Result};
use rusqlite::OptionalExtension;

/// The loop steps a notice dispatches to an Agent (the hold steps never do).
pub const DISPATCHED_STEPS: [&str; 2] = ["review", "merge"];
/// The fewest seconds between a refused dispatch and its re-send.
pub const RESEND_AFTER_SECS: i64 = 60;
/// Re-sends of one step per head before the loop gives up and says so.
pub const MAX_RESENDS: u32 = 3;

/// The exact merge command a "✅ Approved" notice tells the merger to run.
pub fn merge_command(number: u64, head: &str, repo: &str) -> String {
    format!("gh pr merge {number} --squash --match-head-commit {head} --repo {repo}")
}
/// The part of a "🔎 Review requested" notice naming the pull request and head.
pub fn review_ask(number: u64, repo: &str, head: &str) -> String {
    format!("please review PR #{number} in {repo} at head commit {head}")
}

/// What one loop notice dispatches: step, pull request URL, head, and the
/// Matrix ID it addresses.
pub type LoopStep = (&'static str, String, String, String);

/// The step a loop notice's `body` dispatches: only an exact
/// [`merge_command`] or [`review_ask`], addressed to the Matrix ID just before it.
pub fn loop_step_of(body: &str) -> Option<LoopStep> {
    let addressee = |lead: &str| body[..body.find(lead)?].split_whitespace().rev().find(|w| w.starts_with('@')).map(str::to_owned);
    let parse = |lead: &str, words: &[&str]| -> Option<(u64, String, String)> {
        let rest = &body[body.find(lead)? + lead.len()..];
        let mut it = rest.split_whitespace();
        let number: u64 = it.next()?.trim_start_matches('#').parse().ok().filter(|n| *n > 0)?;
        let mut out = Vec::new();
        for word in words {
            if word.is_empty() { out.push(it.next()?.trim_end_matches(['`', ':'])) } else if it.next()? != *word { return None }
        }
        let (head, repo) = if lead.starts_with("gh") { (out[0], out[1]) } else { (out[1], out[0]) };
        let ok = commit_sha(head).is_some() && crate::issue_watch::valid_repo(repo);
        ok.then(|| (number, repo.to_owned(), head.to_owned()))
    };
    let pick = |step: &'static str, (n, repo, head): (u64, String, String)| {
        let exact = if step == "merge" { merge_command(n, &head, &repo) } else { review_ask(n, &repo, &head) };
        let to = addressee(if step == "merge" { "gh pr merge " } else { "please review PR " })?;
        body.contains(&exact).then(|| (step, format!("https://github.com/{repo}/pull/{n}"), head, to))
    };
    if let Some(found) = parse("gh pr merge ", &["--squash", "--match-head-commit", "", "--repo", ""]) {
        return pick("merge", found);
    }
    pick("review", parse("please review PR ", &["in", "", "at", "head", "commit", ""])?)
}

/// Tag a just-received request `dispatch` with the loop step its `body`
/// dispatches, if any; tags of requests no longer in the inbox are dropped.
pub(crate) fn remember_loop_step(db: &rusqlite::Connection, dispatch: &str, body: &str) -> Result<()> {
    if let Some((step, pr, head, to)) = loop_step_of(body) {
        db.execute("INSERT OR IGNORE INTO loop_step_dispatches VALUES (?,?,?,?,?)", (dispatch, step, pr, head, to))?;
    }
    db.execute("DELETE FROM loop_step_dispatches WHERE dispatch NOT IN (SELECT id FROM local_inbox)", [])?;
    Ok(())
}

/// Why a dispatched step did not run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The Agent's token budget is used up: the re-send waits for it.
    Budget,
    /// An owner-policy notice answered before the model started.
    NoRun,
    /// The run failed, or a restart left its result unknown.
    Lost,
    /// Claimed before steps were tagged, and its merge never happened.
    Unrecorded,
}
impl Why {
    pub fn word(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::NoRun => "noRun",
            Self::Lost => "lost",
            Self::Unrecorded => "unrecorded",
        }
    }
}

/// The one room line of a round: why the step did not run, whether this
/// was the last round (`gave_up`), and the round (the `again` it was sent as).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Say {
    pub why: Why,
    pub gave_up: bool,
    pub round: u32,
}

/// One check's answer for a step: whether it stays taken, how many times it
/// was given back (the re-post's `again`), and the round's line, said once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepDue {
    pub taken: bool,
    pub again: u32,
    pub say: Option<Say>,
}

/// The newest dispatch of one step's notice: id, the Agent it went to, receipt time.
type Sent = (String, String, i64);

#[derive(Clone, Copy, Default)]
struct Round {
    known: bool,
    again: u32,
    released_at: i64,
    said: u32,
}

impl Ledger {
    /// Take a review-loop step as [`Ledger::claim_loop_step`] does, and mark a
    /// dispatched step as one whose dispatch is tagged from now on.
    pub fn claim_dispatched_step(&mut self, agent: &str, step: &str, pr: &str, head: &str) -> Result<bool> {
        let claimed = self.claim_loop_step(agent, step, pr, head)?;
        if DISPATCHED_STEPS.contains(&step) {
            self.db.execute("INSERT OR IGNORE INTO review_loop_resends VALUES (?,?,?,?,0,0,0)", (agent, step, pr, head))?;
        }
        Ok(claimed)
    }

    /// Whether `agent`'s `step` of `pr` at `head` stays taken at `now`, giving
    /// the claim back when the newest notice `requester` sent `to` the step's
    /// Agent never ran (see the module documentation). A line said while the
    /// step is held is offered every check until [`Ledger::step_line_said`].
    pub fn step_due(&mut self, agent: &str, (step, pr, head): (&str, &str, &str), (requester, to): (&str, &str), now: i64) -> Result<StepDue> {
        let round = self.round(agent, step, pr, head)?;
        let kept = StepDue { taken: true, again: round.again, say: None };
        if !self.loop_step_taken(agent, step, pr, head)? {
            return Ok(StepDue { taken: false, ..kept });
        }
        if !DISPATCHED_STEPS.contains(&step) || (step == "merge" && self.head_used(pr, head)?) {
            return Ok(kept);
        }
        let Some((why, runner, at)) = self.unrun(agent, (step, pr, head), (requester, to), round)? else { return Ok(kept) };
        let why = if self.agent_budget_used_up(&self.owner, &runner, now)? { Why::Budget } else { why };
        let say = (round.said <= round.again).then_some(Say { why, gave_up: round.again >= MAX_RESENDS, round: round.again });
        if round.again >= MAX_RESENDS || why == Why::Budget || now - at < RESEND_AFTER_SECS {
            return Ok(StepDue { say, ..kept });
        }
        // Given back: the re-post itself shows the retry, so its line counts as said.
        let said = round.again + 1;
        self.atomically(|ledger| {
            ledger.db.execute("DELETE FROM review_loop_sent WHERE agent=? AND step=? AND pr=? AND head=?", (agent, step, pr, head))?;
            ledger.set_round(agent, (step, pr, head), Round { known: true, again: said, released_at: now, said })
        })?;
        Ok(StepDue { taken: false, again: said, say })
    }

    /// The room accepted the line of `round` for this step: it is not offered again.
    pub fn step_line_said(&mut self, agent: &str, (step, pr, head): (&str, &str, &str), round: u32) -> Result<()> {
        let current = self.round(agent, step, pr, head)?;
        let said = current.said.max(round.saturating_add(1));
        self.set_round(agent, (step, pr, head), Round { known: true, said, ..current })
    }

    /// Why the step's newest dispatch did not run, the Agent it went to and
    /// when; `None` while it runs, after it ran, or when nothing is known.
    fn unrun(&self, agent: &str, (step, pr, head): (&str, &str, &str), (requester, to): (&str, &str), round: Round) -> Result<Option<(Why, String, i64)>> {
        let Some((dispatch, runner, at)) = self.newest_sent((step, pr, head), (requester, to), round.released_at)? else {
            // Claimed before tagging (no round row): only a merge that never happened.
            return Ok((!round.known && step == "merge").then(|| (Why::Unrecorded, agent.to_owned(), 0)));
        };
        let Some(row) = self.dispatch_activity(&runner, &dispatch)? else { return Ok(None) };
        let why = match row.outcome {
            Outcome::Answered if row.no_run => Why::NoRun,
            // A lost review may have posted its verdict; a lost merge is guarded by the one automatic merge.
            Outcome::Failed if step == "merge" => Why::Lost,
            _ => return Ok(None),
        };
        Ok(Some((why, runner, at)))
    }

    fn newest_sent(&self, (step, pr, head): (&str, &str, &str), (requester, to): (&str, &str), since: i64) -> Result<Option<Sent>> {
        const NEWEST: &str = "SELECT t.dispatch,b.agent,i.received_at FROM loop_step_dispatches t JOIN local_inbox i ON i.id=t.dispatch \
JOIN bindings b ON b.id=i.binding WHERE t.step=? AND lower(t.pr)=lower(?) AND t.head=? AND json_extract(i.envelope,'$.requesterMxid')=? \
AND t.addressee=? AND i.received_at>=? ORDER BY i.received_at DESC,i.id DESC LIMIT 1";
        Ok(self.db.query_row(NEWEST, (step, pr, head, requester, to, since), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?)
    }

    fn head_used(&self, pr: &str, head: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM auto_merge_head_used WHERE lower(pr)=lower(?) AND head=?)", (pr, head), |r| r.get(0))?)
    }

    fn round(&self, agent: &str, step: &str, pr: &str, head: &str) -> Result<Round> {
        const ROUND: &str = "SELECT again,released_at,said FROM review_loop_resends WHERE agent=? AND step=? AND pr=? AND head=?";
        let row = self.db.query_row(ROUND, (agent, step, pr, head), |r| Ok(Round { known: true, again: r.get(0)?, released_at: r.get(1)?, said: r.get(2)? })).optional()?;
        Ok(row.unwrap_or_default())
    }

    fn set_round(&self, agent: &str, (step, pr, head): (&str, &str, &str), round: Round) -> Result<()> {
        self.db.execute("INSERT OR REPLACE INTO review_loop_resends VALUES (?,?,?,?,?,?,?)", (agent, step, pr, head, round.again, round.released_at, round.said))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "step_resend_tests.rs"]
mod tests;
