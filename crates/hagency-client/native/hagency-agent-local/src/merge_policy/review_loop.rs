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
/// rounds whose room line was said (`said`), see [`super::step_resend`].
pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS review_loop_sent (agent TEXT NOT NULL, step TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, PRIMARY KEY(agent,step,pr,head)) STRICT; \
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
    /// How many of this device's Agents approved `pr` at `head` (unverified).
    pub fn agent_reviewers_count(&self, pr: &str, head: &str) -> Result<usize> {
        Ok(self.agent_reviewers(pr, head)?.len())
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

/// What a merge of `command`'s pull request by `merger` would get now,
/// without taking the one automatic merge. `None` when GitHub or a policy
/// cannot be read (the loop then waits and the owner decides).
pub async fn merge_readiness(ledger: &mut Ledger, command: &GhPr, merger: &str, source: &impl PrSource) -> Option<(PrView, Decision)> {
    let view = source.pull_request(command).await?;
    let repo = repo_of_url(&view.url, command.number)?.to_owned();
    let decision = judge(ledger, command, &repo, &view, (merger, None), source).await?;
    Some((view, decision))
}

/// This device's Agents that approved `view`'s head, counted for a merge from
/// `room` (any room for an advisory readiness check, `None`), never one
/// that wrote the pull request: the Agent the issue tracker handed it to, or
/// the one its body names as "Opened by agent <name>" (GPT review: a shared
/// login lets the author post a marker comment).
fn local_approvers(ledger: &Ledger, repo: &str, number: u64, view: &PrView, room: Option<&str>) -> Result<Vec<String>> {
    let mut authors = ledger.pr_author_agents(repo, number)?;
    authors.extend(view.opened_by.clone());
    let all = ledger.agent_reviewers_in(&view.url, &view.head_sha, room)?;
    Ok(all.into_iter().filter(|reviewer| !authors.iter().any(|a| a.eq_ignore_ascii_case(reviewer))).collect())
}

impl Ledger {
    /// The Agents the issue tracker handed issues to whose pull request is `number` in `repo`.
    pub(crate) fn pr_author_agents(&self, repo: &str, number: u64) -> Result<Vec<String>> {
        let mut statement = self.db.prepare("SELECT DISTINCT agent FROM issue_track WHERE lower(repo)=lower(?) AND pr=?")?;
        let rows = statement.query_map((repo, number as i64), |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

/// The merge decision for `view` under every policy that applies (the
/// room's rules, required, and the owner's; each can only tighten).
/// `room` is the merge's room: only approvals given there count.
pub(crate) async fn judge(ledger: &mut Ledger, command: &GhPr, repo: &str, view: &PrView, (merger, room): (&str, Option<&str>), source: &impl PrSource) -> Option<Decision> {
    let judged = super::work::judged(ledger, merger, command, repo, view, source).await?;
    if let Some(reason) = judged.author_reason {
        return Some(Decision::NeedsHuman(reason));
    }
    let local = local_approvers(ledger, repo, command.number, view, room).ok()?;
    // The room's rules are required (ADR 0029).
    let room_rules = match &judged.room {
        super::room_rules::RoomRules::Rules(policy) => policy,
        super::room_rules::RoomRules::Missing(reason) => return Some(Decision::NeedsHuman((*reason).to_owned())),
    };
    // Without the owner's rules: another of this device's Agents than the
    // merger, local only. With them, the approvals the owner's rules count
    // (an outside author's pull request may be approved by the maintainer
    // that merges it; trusted GitHub reviewers count), so both judge alike.
    let (approvers, excluded) = match &judged.rules {
        None => (local, merger.to_owned()),
        Some(_) => {
            let counted = judged.count_local.then(|| local.clone());
            (approvers(command, repo, view, &judged.trusted_reviewers, (judged.own_login.as_deref(), counted), source).await?, judged.excluded.clone())
        }
    };
    for policy in std::iter::once(room_rules).chain(judged.rules.iter()) {
        if let decision @ Decision::NeedsHuman(_) = evaluate(policy, &view.facts, &approvers, &excluded) {
            return Some(decision);
        }
    }
    // A new watch's trial (ADR 0027): the owner merges its first pull requests.
    super::trial::hold(ledger, repo).ok()?.or(Some(Decision::Merge))
}

#[cfg(test)]
pub(crate) mod tests;
