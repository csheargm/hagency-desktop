//! The issue loop's follow-through (ADR 0020): one tracked record per issue
//! an Agent was handed, so a hand-off is no longer "seen = done". The desktop
//! poller reads GitHub (read-only) into [`Facts`] each check, asks
//! [`decide`] for at most one [`Action`], performs it, then records the new
//! state here. Records live in this device's ledger, so they survive app and
//! guardian restarts. The ledger refuses to leave a terminal state and never
//! lowers an attempt count, so a replayed or stale write cannot restart work.
use crate::{key, Error, Ledger, Result};
use serde::{Deserialize, Serialize};

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS issue_track (agent TEXT NOT NULL, repo TEXT NOT NULL, number INTEGER NOT NULL, state TEXT NOT NULL, attempts INTEGER NOT NULL, handed_at INTEGER NOT NULL, pr INTEGER, notified INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(agent,repo,number)) STRICT;";

/// How long one attempt may take to open a linked pull request before the
/// issue is handed off again (demo value; later attempts wait longer).
pub const HANDOFF_TIMEOUT_SECS: i64 = 30 * 60;
/// Hand-offs per issue, the first one included; then the issue is `failed`.
pub const MAX_ATTEMPTS: u32 = 3;
/// Re-hand-offs per issue after a run that stopped on something other than
/// the Agent: a restart interrupted it (the owner's app or Agent host
/// stopped), or it waited on an approval card nobody answered. They never
/// count as attempts, and the cap ends a crash or no-owner loop, after which
/// such a run counts like any ended one.
pub const MAX_FREE_RETRIES: u32 = 2;

/// Adds the free-retry count to a ledger created before it existed.
pub(crate) fn ensure_schema(db: &rusqlite::Connection) -> Result<()> {
    db.execute_batch(SCHEMA)?;
    let has: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('issue_track') WHERE name='free_retries')", [], |r| r.get(0))?;
    if !has {
        db.execute_batch("ALTER TABLE issue_track ADD COLUMN free_retries INTEGER NOT NULL DEFAULT 0")?;
    }
    digest::ensure_schema(db)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Handed to the Agent; no linked pull request yet.
    HandedOff,
    /// The Agent's pull request "Closes #N" is open.
    PrOpened,
    /// Its review was requested (or a person was asked for it).
    Reviewed,
    /// The issue closed as completed: the pull request merged.
    Merged,
    /// The issue closed some other way.
    Closed,
    /// Every attempt timed out without a pull request.
    Failed,
    /// The Agent said it cannot fix the issue: a person decides.
    NeedsHuman,
}
impl State {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Merged | Self::Closed | Self::Failed | Self::NeedsHuman)
    }
    fn wire(self) -> &'static str {
        match self {
            Self::HandedOff => "handed_off",
            Self::PrOpened => "pr_opened",
            Self::Reviewed => "reviewed",
            Self::Merged => "merged",
            Self::Closed => "closed",
            Self::Failed => "failed",
            Self::NeedsHuman => "needs_human",
        }
    }
    fn parse(text: &str) -> Result<Self> {
        serde_json::from_value(serde_json::Value::String(text.to_owned())).map_err(|_| Error::Invalid("stored issue track state is unreadable"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Track {
    pub repo: String,
    pub number: i64,
    pub state: State,
    pub attempts: u32,
    /// Unix seconds of the latest hand-off.
    pub handed_at: i64,
    /// The linked pull request, once one was seen.
    pub pr: Option<u64>,
    /// The one room notice of the current state was posted.
    pub notified: bool,
    /// Re-hand-offs after restart-interrupted runs (not attempts).
    #[serde(default)]
    pub free_retries: u32,
    /// Unix seconds of the record's last move (0: before it was kept), for the daily digest.
    #[serde(default)]
    pub changed_at: i64,
}

/// What GitHub says about one tracked issue right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IssueFact {
    Open,
    /// Closed as completed.
    Done,
    /// Closed as not planned, or deleted.
    ClosedOther,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Facts {
    pub now: i64,
    pub issue: IssueFact,
    /// The Agent's open pull request that closes this issue.
    pub open_pr: Option<u64>,
    /// The Agent commented that it cannot fix the issue.
    pub cannot_fix: bool,
    /// The current hand-off's run already ended (answered, failed or lost)
    /// with no pull request: retry now instead of at the deadline.
    pub run_ended: bool,
    /// That run was interrupted by a restart of the owner's app or Agent
    /// host (the ledger marked it unknown on recovery), not ended by the Agent.
    pub interrupted: bool,
    /// That run stopped at an approval card its owner never answered (e2e,
    /// Nov 17 23:32: #69 used all three attempts on unanswered cards).
    pub unanswered: bool,
    /// That run ended with no model call: an owner-policy notice (budget,
    /// denial, sign-in) answered it, so the Agent never ran (Nov 18, #90-#92).
    pub no_run: bool,
    /// The Agent's token budget for this period is used up: any hand-off
    /// now would be refused the same way, so none is posted.
    pub budget_out: bool,
}

/// How the Agent tells the loop it cannot fix an issue: an issue comment
/// opening with this, then the reason (the hand-off asks for it).
pub const CANNOT_FIX_MARKER: &str = "🤖 Hagency: cannot fix";

/// The exact cannot-fix comment "push freely" posts without a card
/// (`watch_trust::cannot_fix`), for issue `number` of `repo` (or
/// placeholders): the hand-off and the Codex step tool both state it.
pub fn cannot_fix_step(number: &str, repo: &str) -> String {
    format!("gh issue comment {number} --repo {repo} --body '{CANNOT_FIX_MARKER} - <the reason on one line, without quotes, $, backticks or backslashes>'")
}

/// Whether `agent` was handed issue `number` of `repo` and that record is
/// still open (not merged, closed, failed or given up). Records are keyed by
/// exact spelling: when spellings differ only in case, any finished one wins
/// (GPT review, Nov 18).
pub(crate) fn is_handed(db: &rusqlite::Connection, agent: &str, repo: &str, number: u64) -> Result<bool> {
    let mut statement = db.prepare("SELECT state FROM issue_track WHERE agent=? AND repo=? COLLATE NOCASE AND number=?")?;
    let states = statement.query_map((agent, repo, number as i64), |r| r.get::<_, String>(0))?;
    let mut open = false;
    for state in states {
        if State::parse(&state?)?.is_terminal() {
            return Ok(false);
        }
        open = true;
    }
    Ok(open)
}

/// Whether `login` (the Agent's GitHub identity) posted the cannot-fix
/// comment during the current attempt (at or after `since`).
pub fn says_cannot_fix(comments: &[crate::issue_watch::wire::Comment], login: &str, since: i64) -> bool {
    comments.iter().any(|c| c.author.eq_ignore_ascii_case(login) && c.created_at >= since && c.body.trim_start().starts_with(CANNOT_FIX_MARKER))
}

pub use decide::{Action, REHAND_BACKOFF_SECS, deadline, decide};

/// One owner-API change to a record (validated by the ledger).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TrackChange {
    pub repo: String,
    pub number: i64,
    /// A hand-off of attempt `handed` was posted (1: the first).
    #[serde(default)]
    pub handed: Option<u32>,
    #[serde(default)]
    pub state: Option<State>,
    #[serde(default)]
    pub pr: Option<u64>,
    #[serde(default)]
    pub notified: Option<bool>,
    /// The current attempt was handed off again after an interrupted run, as
    /// its free retry number `rehanded`: a replayed save is a no-op (GPT review).
    #[serde(default)]
    pub rehanded: Option<u32>,
    /// With `state: merged`: GitHub says the owner's own login merged the
    /// pull request, the evidence a watch's trial counts (ADR 0027). Not stored.
    #[serde(default)]
    pub owner_merged: Option<bool>,
}

impl Ledger {
    /// Every record of `agent`, oldest issue first.
    pub fn issue_tracks(&self, agent: &str) -> Result<Vec<Track>> {
        let mut statement = self.db.prepare("SELECT repo, number, state, attempts, handed_at, pr, notified, free_retries, changed_at FROM issue_track WHERE agent=? ORDER BY repo, number")?;
        let rows = statement.query_map([agent], |r| Ok((r.get::<_, String>(0)?, r.get(1)?, r.get::<_, String>(2)?, r.get::<_, i64>(3)?, r.get(4)?, r.get::<_, Option<i64>>(5)?, r.get::<_, i64>(6)?, r.get::<_, i64>(7)?, r.get(8)?)))?;
        rows.map(|row| {
            let (repo, number, state, attempts, handed_at, pr, notified, free, changed_at) = row?;
            Ok(Track { repo, number, state: State::parse(&state)?, attempts: attempts as u32, handed_at, pr: pr.map(|p| p as u64), notified: notified != 0, free_retries: free as u32, changed_at })
        })
        .collect()
    }

    /// The record of issue `number` of `repo`, whatever the case of its
    /// spelling (a watch edited from "Owner/Repo" to "owner/repo" keeps its
    /// records, integration GPT review): the exact spelling first, then an
    /// open record, so a change is saved under the spelling it was stored with.
    fn issue_track(&self, agent: &str, repo: &str, number: i64) -> Result<Option<Track>> {
        let mut same: Vec<Track> = self.issue_tracks(agent)?.into_iter().filter(|t| t.repo.eq_ignore_ascii_case(repo) && t.number == number).collect();
        same.sort_by_key(|t| (t.repo != repo, t.state.is_terminal()));
        Ok(same.into_iter().next())
    }

    /// Apply one change (see [`apply_change`]) and store the result.
    pub fn track_issue(&mut self, agent: &str, change: &TrackChange, now: i64) -> Result<()> {
        key(agent)?;
        let current = self.issue_track(agent, &change.repo, change.number)?;
        let track = apply_change(current, change, now)?;
        self.save_track(agent, &track)
    }

    fn save_track(&mut self, agent: &str, track: &Track) -> Result<()> {
        self.db.execute(
            "INSERT INTO issue_track (agent, repo, number, state, attempts, handed_at, pr, notified, free_retries, changed_at) VALUES (?,?,?,?,?,?,?,?,?,?) ON CONFLICT(agent,repo,number) DO UPDATE SET state=excluded.state, attempts=excluded.attempts, handed_at=excluded.handed_at, pr=excluded.pr, notified=excluded.notified, free_retries=excluded.free_retries, changed_at=excluded.changed_at",
            (agent, &track.repo, track.number, track.state.wire(), track.attempts as i64, track.handed_at, track.pr.map(|p| p as i64), track.notified as i64, track.free_retries as i64, track.changed_at),
        )?;
        Ok(())
    }
}

/// One change to the stored record `current`. A hand-off only counts when
/// it is the next attempt (a replay is a no-op) and never revives a terminal
/// record; a state change never leaves a terminal state; a record starts at
/// its first hand-off; a move stamps `changed_at`. Pure: the desktop's tests share it.
pub fn apply_change(current: Option<Track>, change: &TrackChange, now: i64) -> Result<Track> {
    let before = current.as_ref().map(|t| (t.state, t.attempts, t.pr, t.notified, t.free_retries));
    let mut track = apply_fields(current, change, now)?;
    if before != Some((track.state, track.attempts, track.pr, track.notified, track.free_retries)) {
        track.changed_at = now;
    }
    Ok(track)
}

fn apply_fields(current: Option<Track>, change: &TrackChange, now: i64) -> Result<Track> {
    if !crate::issue_watch::valid_repo(&change.repo) || change.number <= 0 {
        return Err(Error::Invalid("an issue track needs owner/name and an issue number"));
    }
    let mut track = match (current, change.handed) {
        (None, Some(1)) => Track { repo: change.repo.clone(), number: change.number, state: State::HandedOff, attempts: 0, handed_at: now, pr: None, notified: false, free_retries: 0, changed_at: now },
        (None, _) => return Err(Error::Invalid("an issue is tracked from its first hand-off")),
        (Some(track), _) => track,
    };
    if track.state.is_terminal() {
        // Final: a late write changes nothing but the once-only notice flag.
        track.notified |= change.notified == Some(true);
        return Ok(track);
    }
    if let Some(attempt) = change.handed {
        // Only a record still waiting for a pull request is retried: a late
        // retry write never clears a pull request found since (GPT review).
        if track.state == State::HandedOff && attempt == track.attempts + 1 && attempt <= MAX_ATTEMPTS {
            track = Track { state: State::HandedOff, attempts: attempt, handed_at: now, pr: None, notified: false, ..track };
        }
    }
    if change.rehanded == Some(track.free_retries + 1) && track.state == State::HandedOff && track.free_retries < MAX_FREE_RETRIES {
        // Same attempt, a new run: its own clock, never a new attempt.
        track = Track { handed_at: now, free_retries: track.free_retries + 1, notified: false, ..track };
    }
    if let Some(state) = change.state {
        if state != track.state {
            track.notified = false;
        }
        track.state = state;
    }
    track.pr = change.pr.or(track.pr);
    track.notified = change.notified.unwrap_or(track.notified);
    Ok(track)
}

mod decide;
pub mod digest;
#[cfg(test)]
mod tests;
