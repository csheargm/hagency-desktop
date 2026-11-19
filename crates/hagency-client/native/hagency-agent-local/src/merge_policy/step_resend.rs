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
//! its verdict); only the notice addressed to the step's Agent in the step's
//! room counts. A claimed step whose notice never reached an Agent of this
//! device (its binding inactive, or a reviewer on another device that
//! refused it there) is given back once after [`UNSEEN_AFTER_SECS`]; a merge
//! stays guarded by the one automatic merge per head.
use crate::activity::Outcome;
use crate::{Ledger, Result};
use rusqlite::OptionalExtension;

/// The once-per-head step of the "still a draft" notice, which asks the
/// fixer to mark its draft ready (a hold step's name, dispatched all the same).
pub const READY_STEP: &str = "hold-draft";
/// The loop steps a notice dispatches to an Agent (the other hold steps never do).
/// The ready request is one too (round-2 GPT review: a ready request the
/// Agent never ran was taken for good, and the draft never reviewed).
pub const DISPATCHED_STEPS: [&str; 3] = ["review", "merge", READY_STEP];
/// The fewest seconds between a refused dispatch and its re-send.
pub const RESEND_AFTER_SECS: i64 = 60;
/// Re-sends of one step per head before the loop gives up and says so.
pub const MAX_RESENDS: u32 = 3;
/// How long a claimed step may show no dispatch on this device before it is given back.
pub const UNSEEN_AFTER_SECS: i64 = 30 * 60;
/// Give-backs of a step whose notice never showed a dispatch here (counted in its rounds).
pub const MAX_UNSEEN_RESENDS: u32 = 1;

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
    /// No dispatch of its notice reached this device within [`UNSEEN_AFTER_SECS`].
    Unseen,
    /// Another owner's reviewer, whose runs this device never sees, gave no
    /// verdict within [`UNSEEN_AFTER_SECS`] (`reviewer_pass::remote_review_due`).
    Unanswered,
}
impl Why {
    pub fn word(self) -> &'static str {
        match self {
            Self::Budget => "budget",
            Self::NoRun => "noRun",
            Self::Lost => "lost",
            Self::Unrecorded => "unrecorded",
            Self::Unseen => "unseen",
            Self::Unanswered => "unanswered",
        }
    }
    /// The rounds a step is given back for this reason before the loop gives up.
    fn cap(self) -> u32 {
        if self == Self::Unseen { MAX_UNSEEN_RESENDS } else { MAX_RESENDS }
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
/// Who sent a step's notice, the Matrix ID it addresses, and the room it went to.
pub type Sender<'a> = (&'a str, &'a str, &'a str);

#[derive(Clone, Default)]
struct Round {
    known: bool,
    again: u32,
    released_at: i64,
    said: u32,
    /// The Agent the given-back step goes to again (its budget holds the re-post).
    runner: String,
}

impl Ledger {
    /// Take a review-loop step as [`Ledger::claim_loop_step`] does, and mark a
    /// dispatched step as one whose dispatch is tagged from now on.
    pub fn claim_dispatched_step(&mut self, agent: &str, step: &str, pr: &str, head: &str) -> Result<bool> {
        let claimed = self.claim_loop_step(agent, step, pr, head)?;
        if DISPATCHED_STEPS.contains(&step) {
            self.db.execute("INSERT OR IGNORE INTO review_loop_resends VALUES (?,?,?,?,0,0,0,'')", (agent, step, pr, head))?;
        }
        Ok(claimed)
    }

    /// Whether `agent`'s `step` of `pr` at `head` stays taken at `now`, giving
    /// the claim back when the newest notice `requester` sent `to` the step's
    /// Agent in `room` never ran (see the module documentation). A line said
    /// while the step is held is offered every check until [`Ledger::step_line_said`].
    pub fn step_due(&mut self, agent: &str, (step, pr, head): (&str, &str, &str), (requester, to, room): Sender<'_>, now: i64) -> Result<StepDue> {
        let round = self.round(agent, step, pr, head)?;
        let kept = StepDue { taken: true, again: round.again, say: None };
        // Every budget a run in this room would be refused for (integration GPT review).
        let out = |ledger: &Self, runner: &str| -> Result<bool> { Ok(!runner.is_empty() && ledger.room_budget_used_up(&ledger.owner, runner, room, requester, now)?) };
        if !self.loop_step_taken(agent, step, pr, head)? {
            // A given-back step not re-posted yet waits while its Agent's budget is used up (GPT review).
            let held = round.again > 0 && out(self, &round.runner)?;
            if step == "review" && !held {
                self.note_asked(agent, (pr, head), to, round.again)?;
            }
            return Ok(StepDue { taken: held, ..kept });
        }
        // A reviewer never asked for this head gets a round of its own (`reviewer_pass`).
        if step == "review" && self.hands_on(agent, (pr, head), to)? {
            self.note_asked(agent, (pr, head), to, round.again + 1)?;
            return Ok(StepDue { taken: false, again: self.give_back(agent, (step, pr, head), now, String::new(), round.again + 1)?, say: None });
        }
        if !DISPATCHED_STEPS.contains(&step) || (step == "merge" && self.head_used(pr, head)?) {
            return Ok(kept);
        }
        let Some((why, runner, at)) = self.unrun(agent, (step, pr, head), (requester, to, room), (&round, now))? else { return Ok(kept) };
        let why = if out(self, &runner)? { Why::Budget } else { why };
        // Only this reviewer's own rounds count toward its cap (round-2 GPT review P2-4).
        let spent = round.again - if step == "review" { self.reviewer_base(agent, (pr, head), to)?.min(round.again) } else { 0 };
        let say = (round.said <= round.again).then_some(Say { why, gave_up: spent >= why.cap(), round: round.again });
        // A used-up budget is a hold, never a pass (GPT review), and a reviewer
        // is passed only once the back-off elapsed (round-2 GPT review P2-6).
        if why == Why::Budget {
            return Ok(StepDue { say, ..kept });
        }
        // Inside the back-off the last round says nothing yet: it gives up only after.
        if now - at < RESEND_AFTER_SECS {
            return Ok(StepDue { say: say.filter(|s| !s.gave_up), ..kept });
        }
        if spent >= why.cap() {
            if step == "review" {
                self.pass_reviewer(agent, (pr, head), to, round.again)?;
            }
            return Ok(StepDue { say, ..kept });
        }
        // Given back: the re-post itself shows the retry, so its line counts as said.
        Ok(StepDue { taken: false, again: self.give_back(agent, (step, pr, head), now, runner, round.again + 1)?, say })
    }

    /// Give the step back as round `again`, re-posted to `runner`'s Agent; returns `again`.
    fn give_back(&mut self, agent: &str, (step, pr, head): (&str, &str, &str), now: i64, runner: String, again: u32) -> Result<u32> {
        self.atomically(|ledger| {
            ledger.db.execute("DELETE FROM review_loop_sent WHERE agent=? AND step=? AND pr=? AND head=?", (agent, step, pr, head))?;
            ledger.db.execute("DELETE FROM review_loop_unseen WHERE agent=? AND step=? AND pr=? AND head=?", (agent, step, pr, head))?;
            ledger.set_round(agent, (step, pr, head), &Round { known: true, again, released_at: now, said: again, runner })
        })?;
        Ok(again)
    }

    /// The room accepted the line of `round` for this step: it is not offered again.
    pub fn step_line_said(&mut self, agent: &str, (step, pr, head): (&str, &str, &str), round: u32) -> Result<()> {
        let current = self.round(agent, step, pr, head)?;
        let said = current.said.max(round.saturating_add(1));
        self.set_round(agent, (step, pr, head), &Round { known: true, said, ..current })
    }

    /// Why the step's newest dispatch did not run, the Agent it went to and
    /// when; `None` while it runs, after it ran, or when nothing is known.
    fn unrun(&self, agent: &str, (step, pr, head): (&str, &str, &str), sender: Sender<'_>, (round, now): (&Round, i64)) -> Result<Option<(Why, String, i64)>> {
        let Some((dispatch, runner, at)) = self.newest_sent((step, pr, head), sender, round.released_at)? else {
            // Claimed before tagging (no round row): a merge that never happened, or a
            // ready request claimed before it was a dispatched step (round-2 GPT review).
            if !round.known {
                return Ok((step == "merge" || step == READY_STEP).then(|| (Why::Unrecorded, agent.to_owned(), 0)));
            }
            return self.unseen(agent, (step, pr, head), sender.2, now);
        };
        // Seen once: a dispatch pruned from the inbox later never reads as unseen.
        self.db.execute("INSERT OR REPLACE INTO review_loop_unseen VALUES (?,?,?,?,-1)", (agent, step, pr, head))?;
        let Some(row) = self.dispatch_activity(&runner, &dispatch)? else { return Ok(None) };
        let why = match row.outcome {
            Outcome::Answered if row.no_run => Why::NoRun,
            // A lost review may have posted its verdict; a lost merge is guarded by the one automatic merge.
            Outcome::Failed if step == "merge" => Why::Lost,
            _ => return Ok(None),
        };
        Ok(Some((why, runner, at)))
    }

    /// The claimed round shows no dispatch here: [`Why::Unseen`] once
    /// [`UNSEEN_AFTER_SECS`] passed since that was first found. A review is
    /// never asked again once it is known done here (an approval of the head
    /// recorded in `room` that could count, or its merge step taken).
    fn unseen(&self, agent: &str, (step, pr, head): (&str, &str, &str), room: &str, now: i64) -> Result<Option<(Why, String, i64)>> {
        self.db.execute("INSERT OR IGNORE INTO review_loop_unseen VALUES (?,?,?,?,?)", (agent, step, pr, head, now))?;
        let first: i64 = self.db.query_row("SELECT first_at FROM review_loop_unseen WHERE agent=? AND step=? AND pr=? AND head=?", (agent, step, pr, head), |r| r.get(0))?;
        if first < 0 || now - first < UNSEEN_AFTER_SECS {
            return Ok(None);
        }
        if step == "review" && (self.eligible_recorded(agent, (pr, head), room)? > 0 || self.loop_step_taken(agent, "merge", pr, head)?) {
            return Ok(None);
        }
        // No runner is known: the re-post is never held for a budget.
        Ok(Some((Why::Unseen, String::new(), first)))
    }

    /// Approvals of the head recorded in `room` that could count, as
    /// [`Ledger::recorded_room_reviews`] counts them without GitHub's view:
    /// never the step's Agent (the merger) nor an Agent the tracker handed
    /// the pull request to (round-2 GPT review P2-5: an author's or merger's
    /// approval stopped the rotation while readiness counted zero). Residual:
    /// an author known only from GitHub (its body's "Opened by", its branch's
    /// publishes) still stops it; that only waits for a person, never merges.
    fn eligible_recorded(&self, agent: &str, (pr, head): (&str, &str), room: &str) -> Result<usize> {
        let number = pr.rsplit('/').next().and_then(|n| n.parse::<u64>().ok());
        let authors = match number.and_then(|n| Some((super::github::repo_of_url(pr, n)?, n))) {
            Some((repo, n)) => self.pr_author_agents(repo, n)?,
            None => Vec::new(),
        };
        let reviewers = self.agent_reviewers_in(pr, head, Some(room))?;
        let excluded = |r: &String| r.eq_ignore_ascii_case(agent) || authors.iter().any(|a| a.eq_ignore_ascii_case(r));
        Ok(reviewers.iter().filter(|r| !excluded(r)).count())
    }

    fn newest_sent(&self, (step, pr, head): (&str, &str, &str), (requester, to, room): Sender<'_>, since: i64) -> Result<Option<Sent>> {
        const NEWEST: &str = "SELECT t.dispatch,b.agent,i.received_at FROM loop_step_dispatches t JOIN local_inbox i ON i.id=t.dispatch \
JOIN bindings b ON b.id=i.binding WHERE t.step=? AND lower(t.pr)=lower(?) AND t.head=? AND json_extract(i.envelope,'$.requesterMxid')=? \
AND t.addressee=? AND b.room=? AND i.received_at>=? ORDER BY i.received_at DESC,i.id DESC LIMIT 1";
        Ok(self.db.query_row(NEWEST, (step, pr, head, requester, to, room, since), |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional()?)
    }

    fn head_used(&self, pr: &str, head: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM auto_merge_head_used WHERE lower(pr)=lower(?) AND head=?)", (pr, head), |r| r.get(0))?)
    }

    fn round(&self, agent: &str, step: &str, pr: &str, head: &str) -> Result<Round> {
        const ROUND: &str = "SELECT again,released_at,said,runner FROM review_loop_resends WHERE agent=? AND step=? AND pr=? AND head=?";
        let row = self.db.query_row(ROUND, (agent, step, pr, head), |r| Ok(Round { known: true, again: r.get(0)?, released_at: r.get(1)?, said: r.get(2)?, runner: r.get(3)? })).optional()?;
        Ok(row.unwrap_or_default())
    }

    fn set_round(&self, agent: &str, (step, pr, head): (&str, &str, &str), round: &Round) -> Result<()> {
        self.db.execute("INSERT OR REPLACE INTO review_loop_resends VALUES (?,?,?,?,?,?,?,?)", (agent, step, pr, head, round.again, round.released_at, round.said, &round.runner))?;
        Ok(())
    }
}

#[path = "step_resend_notice.rs"]
mod notice;
pub use notice::{LoopStep, MERGE_NOTICE_LEAD, READY_NOTICE_LEAD, REVIEW_NOTICE_LEAD, loop_step_of, merge_command, ready_ask, review_ask};
pub(crate) use notice::{asked_to_review, remember_loop_step};
#[path = "reviewer_pass.rs"]
mod reviewer_pass;
pub(crate) use reviewer_pass::SCHEMA as PASSED_SCHEMA;

#[cfg(test)]
#[path = "step_resend_tests.rs"]
mod tests;
