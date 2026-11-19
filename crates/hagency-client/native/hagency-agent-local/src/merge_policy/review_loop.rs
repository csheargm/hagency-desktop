//! The review loop's gate side (ADR 0016): which approvals count for a merge,
//! the dry-run the desktop asks before it tells an Agent to merge, and the
//! once-per-head record of each loop step.
//!
//! GitHub is the record: besides this device's own Agents' approvals of the
//! exact head (the ledger), a GitHub review counts when its login is one the
//! owner trusts ([`super::work::OwnerRules::trusted_reviewers`]) and
//! that login's latest decisive review APPROVES exactly the head being merged.
use super::gate::GhPr;
use super::github::{repo_of_url, PrSource, PrView};
use super::{evaluate, Decision};
use crate::{Ledger, Result};

/// `loop_step_dispatches`: the step each received loop notice dispatches and the Agent it addresses;
/// `review_loop_resends`: per dispatched step claimed, how often it was given
/// back (`again`), when (`released_at`), to which Agent (`runner`), and the
/// rounds whose room line was said (`said`), see [`super::step_resend`];
/// `review_loop_unseen`: when a claimed round was first found with no
/// dispatch on this device (`first_at`, -1 once one was seen).
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS review_loop_sent (agent TEXT NOT NULL, step TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, PRIMARY KEY(agent,step,pr,head)) STRICT; \
CREATE TABLE IF NOT EXISTS review_loop_unseen (agent TEXT NOT NULL, step TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, first_at INTEGER NOT NULL, PRIMARY KEY(agent,step,pr,head)) STRICT; \
CREATE TABLE IF NOT EXISTS loop_step_dispatches (dispatch TEXT PRIMARY KEY, step TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, addressee TEXT NOT NULL) STRICT; \
CREATE TABLE IF NOT EXISTS review_loop_resends (agent TEXT NOT NULL, step TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, again INTEGER NOT NULL, released_at INTEGER NOT NULL, said INTEGER NOT NULL, runner TEXT NOT NULL, PRIMARY KEY(agent,step,pr,head)) STRICT;";

/// A review-loop step that posts one "can't merge yet" notice per head and
/// set of missing things: this prefix, then lowercase words joined by `-`.
pub const HOLD_STEP: &str = "hold-";

/// The prefix that marks a GitHub reviewer among Agent approvers.
pub const GITHUB_REVIEWER: &str = "github:";

impl Ledger {
    /// Take one review-loop step (`step`: "review" or "merge") for `pr` at
    /// `head`; false when it was already taken, so each step runs once per head.
    pub fn claim_loop_step(&mut self, agent: &str, step: &str, pr: &str, head: &str) -> Result<bool> {
        Ok(self.db.execute("INSERT OR IGNORE INTO review_loop_sent VALUES (?,?,?,?)", (agent, step, pr, head))? == 1)
    }
    pub fn loop_step_taken(&self, agent: &str, step: &str, pr: &str, head: &str) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM review_loop_sent WHERE agent=? AND step=? AND pr=? AND head=?)", (agent, step, pr, head), |r| r.get(0))?)
    }
}

/// The approvers the merge gate counts for `view`: this device's Agents that
/// approved its head, plus `github:<login>` for each trusted reviewer whose
/// latest decisive GitHub review approves exactly that head (never the
/// pull request's own author). `None` when something cannot be read.
pub(crate) async fn approvers(command: &GhPr, repo: &str, view: &PrView, trusted: &[String], (own_login, local): (Option<&str>, Option<Vec<String>>), source: &impl PrSource) -> Option<Vec<String>> {
    let mut all = local.unwrap_or_default();
    if trusted.is_empty() {
        return Some(all);
    }
    let reviews = source.reviews(command, repo).await?;
    // This device's own login is already counted as its Agent's local approval.
    for login in trusted.iter().filter(|l| !own_login.is_some_and(|own| own.eq_ignore_ascii_case(l))) {
        let theirs: Vec<_> = reviews.iter().filter(|(who, _, _)| who.eq_ignore_ascii_case(login)).cloned().collect();
        if !login.eq_ignore_ascii_case(&view.author) && super::github::approved_on(&theirs, &view.head_sha) {
            all.push(format!("{GITHUB_REVIEWER}{}", login.to_ascii_lowercase()));
        }
    }
    Some(all)
}

/// What a merge of `command`'s pull request by `merger` from `room` would
/// get now, without taking the one automatic merge: only approvals given in
/// `room` count, as for the gate's own answer (integration GPT review: an
/// approval from another room passed here, then the gate refused the merge).
/// `None` when GitHub or a policy cannot be read (the loop then waits and the owner decides).
pub async fn merge_readiness(ledger: &mut Ledger, command: &GhPr, who: (&str, &str), source: &impl PrSource) -> Option<(PrView, Decision)> {
    merge_readiness_counted(ledger, command, who, source).await.map(|(view, decision, _)| (view, decision))
}

/// [`merge_readiness`], with how many approvals the judge counted (never
/// the pull request's author Agents or the merger: round-2 GPT review, a
/// self-verdict made readiness say 1 while the judge saw 0).
pub async fn merge_readiness_counted(ledger: &mut Ledger, command: &GhPr, who: (&str, &str), source: &impl PrSource) -> Option<(PrView, Decision, usize)> {
    merge_readiness_full(ledger, command, who, source).await.map(|r| (r.view, r.decision, r.counted))
}

/// The gate's answer for one pull request, and what it would be with one
/// more Agent approval than it counted (`one_more`): the desktop tells a
/// review only another owner's device can record (Nov 18, PR #121) from a
/// hold no approval could lift. `one_more` never merges anything.
#[derive(Debug)]
pub struct Readiness {
    pub view: PrView,
    pub decision: Decision,
    pub counted: usize,
    pub one_more: Decision,
}

/// [`merge_readiness_counted`] with [`Readiness::one_more`].
pub async fn merge_readiness_full(ledger: &mut Ledger, command: &GhPr, (merger, room): (&str, &str), source: &impl PrSource) -> Option<Readiness> {
    let view = source.pull_request(command).await?;
    let repo = repo_of_url(&view.url, command.number)?.to_owned();
    let (decision, counted, one_more) = judge_counted(ledger, command, &repo, &view, (merger, room), source).await?;
    Some(Readiness { view, decision, counted, one_more })
}

/// This device's Agents that approved `view`'s head, counted for a merge from
/// `room` (any room, `None`), never one
/// that may have written the pull request: the Agent the issue tracker handed it to,
/// the one its body names as "Opened by agent <name>" (GPT review: a shared
/// login lets the author post a marker comment), or any recorded as
/// publishing its head branch ([`super::authorship`]).
fn local_approvers(ledger: &Ledger, repo: &str, number: u64, view: &PrView, room: Option<&str>) -> Result<Vec<String>> {
    let mut authors = ledger.pr_author_agents(repo, number)?;
    authors.extend(view.opened_by.clone());
    authors.extend(ledger.branch_authors(repo, view)?.0);
    let all = ledger.agent_reviewers_in(&view.url, &view.head_sha, room)?;
    Ok(all.into_iter().filter(|reviewer| !authors.iter().any(|a| a.eq_ignore_ascii_case(reviewer))).collect())
}

impl Ledger {
    /// How many Agents' approvals of `view`'s head given in `room` are
    /// recorded and could count (never an author's or the merger's, GPT batch
    /// review P2-4): such a review is never asked for again.
    pub fn recorded_room_reviews(&self, view: &PrView, number: u64, (merger, room): (&str, &str)) -> Result<usize> {
        let Some(repo) = repo_of_url(&view.url, number) else { return Ok(0) };
        Ok(local_approvers(self, repo, number, view, Some(room))?.iter().filter(|r| !r.eq_ignore_ascii_case(merger)).count())
    }
    /// The Agents the issue tracker handed issues to whose pull request is `number` in `repo`.
    pub(crate) fn pr_author_agents(&self, repo: &str, number: u64) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT DISTINCT agent FROM issue_track WHERE lower(repo)=lower(?) AND pr=?")?;
        let rows = statement.query_map((repo, number as i64), |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// The merge decision for `view` under every policy that applies (the
/// room's rules, required, and the owner's; each can only tighten).
/// `room` is the merge's room: its rules judge, and only approvals given there count.
pub(crate) async fn judge(ledger: &mut Ledger, command: &GhPr, repo: &str, view: &PrView, who: (&str, &str), source: &impl PrSource) -> Option<Decision> {
    judge_counted(ledger, command, repo, view, who, source).await.map(|(decision, _, _)| decision)
}

/// Stands for one more Agent's approval in [`Readiness::one_more`]: never a
/// login or Agent id (it holds a NUL), so it never matches the merger.
const ONE_MORE: &str = "\0one-more-agent";

/// [`judge`], with the number of approvals it counted ([`super::counted_approvals`])
/// and the decision with one more ([`Readiness::one_more`]).
async fn judge_counted(ledger: &mut Ledger, command: &GhPr, repo: &str, view: &PrView, (merger, room): (&str, &str), source: &impl PrSource) -> Option<(Decision, usize, Decision)> {
    let judged = super::work::judged(ledger, (merger, room), command, repo, view, source).await?;
    let local = local_approvers(ledger, repo, command.number, view, Some(room)).ok()?;
    // Decided before the approvals: the recorded count, as before, so a hold
    // that no review can lift is never read as "no review yet" (round-2 GPT review).
    let early = ledger.agent_reviewers_in(&view.url, &view.head_sha, Some(room)).ok()?.len();
    if let Some(reason) = judged.author_reason {
        return Some((Decision::NeedsHuman(reason.clone()), early, Decision::NeedsHuman(reason)));
    }
    // The room's rules are required (ADR 0029).
    let room_rules = match &judged.room {
        super::room_rules::RoomRules::Rules(policy) => policy,
        super::room_rules::RoomRules::Missing(reason) => return Some((Decision::NeedsHuman((*reason).to_owned()), early, Decision::NeedsHuman((*reason).to_owned()))),
    };
    // Without the owner's rules: another of this device's Agents than the
    // merger, local only. With them, the approvals the owner's rules count
    // (an outside author's pull request may be approved by the maintainer
    // that merges it; trusted GitHub reviewers count), so both judge alike.
    let (approvers, excluded) = match &judged.rules {
        None => (local, merger.to_owned()),
        Some(_) => {
            // A shared login wrote it, but this ledger recorded which Agent
            // created it: the others' approvals count (live PR #109, Nov 18).
            // Never the tracker's guess alone (GPT batch review P1-1).
            let author_known = ledger.branch_authors(repo, view).ok()?.1;
            let counted = (judged.count_local || author_known).then(|| local.clone());
            (approvers(command, repo, view, &judged.trusted_reviewers, (judged.own_login.as_deref(), counted), source).await?, judged.excluded.clone())
        }
    };
    let counted = super::counted_approvals(&approvers, &excluded).len();
    let policies: Vec<&super::MergePolicy> = std::iter::once(room_rules).chain(judged.rules.iter()).collect();
    // The room's autonomy level (ADR 0030): only an Autonomous, running room
    // delivers without a person, and only by an Agent that may work there.
    let delivered = ledger.loop_may_deliver(merger, room);
    let decision = decide(&policies, view, (&approvers, &excluded), delivered);
    let mut more = approvers;
    more.push(ONE_MORE.to_owned());
    Some((decision, counted, decide(&policies, view, (&more, &excluded), delivered)))
}

/// Every policy in order, then the room's autonomy (`delivered`).
fn decide(policies: &[&super::MergePolicy], view: &PrView, (approvers, excluded): (&[String], &str), delivered: std::result::Result<(), &'static str>) -> Decision {
    for policy in policies {
        if let decision @ Decision::NeedsHuman(_) = evaluate(policy, &view.facts, approvers, excluded) {
            return decision;
        }
    }
    delivered.map_or_else(|reason| Decision::NeedsHuman(reason.to_owned()), |()| Decision::Merge)
}

#[cfg(test)]
#[path = "review_loop/readiness_tests.rs"]
mod readiness_tests;

#[cfg(test)]
#[path = "review_loop/one_more_tests.rs"]
mod one_more_tests;

#[cfg(test)]
pub(crate) mod tests;
