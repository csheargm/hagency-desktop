//! Recent requests for one Agent, newest first, for the owner's Activity view.
//! Read-only: room, requester, receipt time, a coarse outcome and the tokens
//! the settled call charged. Request and reply text never leave this module.
use crate::inbox::Dispatch;
use crate::{Error, Ledger, Result, Usage, key};
use serde::Serialize;

/// Largest page the Activity view may ask for.
pub const MAX_RECENT: usize = 100;

/// What became of one request, as the owner reads it.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Received, prepared or running; no result yet.
    Running,
    /// A reply was produced (delivered or waiting to be delivered).
    Answered,
    /// Turned away without a model call.
    Declined,
    /// Failed, or ended with an unknown result.
    Failed,
}
impl Outcome {
    /// Inbox `state` (schema.sql `local_inbox.state`) to outcome.
    pub fn of_state(state: &str) -> Self {
        match state {
            "reply_ready" | "replied" => Self::Answered,
            "rejected" => Self::Declined,
            "failed" | "unknown" => Self::Failed,
            _ => Self::Running,
        }
    }
}

/// One recent request.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub dispatch_id: String,
    pub binding_id: String,
    pub room_id: String,
    pub requester: String,
    /// Unix seconds when this device durably received the request.
    pub received_at: i64,
    pub outcome: Outcome,
    /// Input plus output tokens of the settled call; `None` when unknown.
    pub tokens: Option<u64>,
    /// The issue an issue-watch hand-off asks about (ADR 0020), so the loop
    /// can tell which run is which issue's. Only the repository and number
    /// leave this module, never the request text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue: Option<IssueRef>,
    /// Interrupted, not finished: the ledger marked the run unknown when this
    /// device's app or Agent host restarted (or could not tell its result).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    /// A hand-off's run asked its owner to approve a tool call and nobody
    /// answered the card: the run stopped on a person, not on the Agent, so
    /// the issue loop does not count it as an attempt (e2e, Nov 17 23:32).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unanswered: bool,
    /// Answered with no model call: an owner-policy notice (a used-up
    /// budget, a denial, a signed-out provider) replied instead of the
    /// Agent, so the issue loop does not count it as an attempt (Nov 18).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub no_run: bool,
}

/// Which issue each received hand-off names, kept apart from the request
/// body (compaction clears that body, GPT review): only repo and number.
/// `handoff_unanswered` holds the hand-offs whose run left an approval card
/// unanswered (see [`Ledger::note_unanswered_approval`]).
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS handoff_issues (dispatch TEXT PRIMARY KEY, repo TEXT NOT NULL, number INTEGER NOT NULL) STRICT; \
CREATE TABLE IF NOT EXISTS handoff_unanswered (dispatch TEXT PRIMARY KEY) STRICT;";

/// Remember the issue a just-received request `dispatch` hands off, if any;
/// rows of requests no longer in the inbox are dropped.
pub(crate) fn remember_handoff(db: &rusqlite::Connection, dispatch: &str, body: &str) -> Result<()> {
    if let Some(issue) = handoff_issue(body) {
        db.execute("INSERT OR IGNORE INTO handoff_issues VALUES (?,?,?)", (dispatch, &issue.repo, issue.number))?;
    }
    db.execute("DELETE FROM handoff_issues WHERE dispatch NOT IN (SELECT id FROM local_inbox)", [])?;
    db.execute("DELETE FROM handoff_unanswered WHERE dispatch NOT IN (SELECT dispatch FROM handoff_issues)", [])?;
    crate::merge_policy::step_resend::remember_loop_step(db, dispatch, body)
}

/// The repository and number a hand-off names.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct IssueRef {
    pub repo: String,
    pub number: i64,
}

/// `🔔 New GitHub issue #N in owner/name …` on any line of `body`.
pub fn handoff_issue(body: &str) -> Option<IssueRef> {
    body.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix(hagency_runtime::attribution::ISSUE_NOTICE_LEAD)?.strip_prefix('#')?;
        let (number, rest) = rest.split_once(" in ")?;
        let repo = rest.split_whitespace().next()?;
        let number: i64 = number.parse().ok().filter(|n| *n > 0)?;
        crate::issue_watch::valid_repo(repo).then(|| IssueRef { repo: repo.to_owned(), number })
    })
}

/// Tokens a settled call charged; unreported or unsettled usage is unknown.
fn charged(call_state: Option<&str>, usage: Option<&str>) -> Option<u64> {
    if call_state != Some("settled") {
        return None;
    }
    let usage: Usage = serde_json::from_str(usage?).ok()?;
    usage.category_counts().3?;
    Some(usage.input.saturating_add(usage.output))
}

/// Column 8 is the zero-call proof `Ledger::persist_no_provider_reply`
/// settles: nothing reserved, no policy snapshots, no-provider accounting.
const RECENT: &str = "SELECT i.envelope,i.state,i.received_at,c.state,c.usage,h.repo,h.number,u.dispatch IS NOT NULL,\
coalesce(c.state='settled' AND c.reserved=0 AND c.snapshots='[]' AND json_extract(c.usage,'$.accounting_version')=?3,0) FROM local_inbox i \
JOIN bindings b ON b.id=i.binding LEFT JOIN calls c ON c.id=i.execution_id LEFT JOIN handoff_issues h ON h.dispatch=i.id \
LEFT JOIN handoff_unanswered u ON u.dispatch=i.id WHERE b.agent=?1 ORDER BY i.received_at DESC,i.id DESC LIMIT ?2";

impl Ledger {
    /// The run of `dispatch` asked its owner to approve a tool call and the
    /// card ended unanswered (or could not be shown). Kept only for issue
    /// hand-offs; any other request is left alone.
    pub fn note_unanswered_approval(&mut self, dispatch: &str) -> Result<()> {
        self.db.execute("INSERT OR IGNORE INTO handoff_unanswered SELECT dispatch FROM handoff_issues WHERE dispatch=?", [dispatch])?;
        Ok(())
    }

    /// Whether `agent`'s own token budget for the period of `now` is used
    /// up, so `begin_postpaid` refuses every new task with `Error::Budget`
    /// until the period turns or the owner raises it (Nov 18, #90-#92).
    pub fn agent_budget_used_up(&self, owner: &str, agent: &str, now: i64) -> Result<bool> {
        let budget = self.agent_policy(owner, agent)?.policy.budget;
        let crate::Limit::Tokens(limit) = budget.limit else { return Ok(false) };
        let (spent, _) = self.agent_account(owner, agent, budget.period, now)?;
        Ok(spent >= limit)
    }

    /// The last `limit` requests for `agent`, newest first.
    pub fn recent_activity(&self, owner: &str, agent: &str, limit: usize) -> Result<Vec<Activity>> {
        self.owner(owner)?;
        key(agent)?;
        if !(1..=MAX_RECENT).contains(&limit) {
            return Err(Error::Invalid("activity query limit"));
        }
        let mut stmt = self.db.prepare(RECENT)?;
        let rows = stmt.query_map((agent, limit, crate::inbox::NO_PROVIDER_CALL), |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
                r.get::<_, bool>(7)?,
                r.get::<_, bool>(8)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (envelope, state, received_at, call_state, usage, repo, number, unanswered, no_call) = row?;
            let dispatch: Dispatch = serde_json::from_str(&envelope)?;
            let outcome = Outcome::of_state(&state);
            out.push(Activity {
                dispatch_id: dispatch.id,
                binding_id: dispatch.binding_id,
                room_id: dispatch.room_id,
                requester: dispatch.requester_mxid,
                received_at,
                outcome,
                tokens: charged(call_state.as_deref(), usage.as_deref()),
                issue: repo.zip(number).map(|(repo, number)| IssueRef { repo, number }).or_else(|| handoff_issue(&dispatch.body)),
                interrupted: state == "unknown",
                unanswered,
                // A declined request also settles the zero-call proof; only a reply is a no-run.
                no_run: no_call && outcome == Outcome::Answered,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests;
