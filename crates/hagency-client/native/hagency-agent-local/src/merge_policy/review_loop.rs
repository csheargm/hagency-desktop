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

pub(crate) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS review_loop_sent (agent TEXT NOT NULL, step TEXT NOT NULL, pr TEXT NOT NULL, head TEXT NOT NULL, PRIMARY KEY(agent,step,pr,head)) STRICT;";

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
pub(crate) async fn approvers(ledger: &mut Ledger, command: &GhPr, repo: &str, view: &PrView, trusted: &[String], (own_login, count_local): (Option<&str>, bool), source: &impl PrSource) -> Option<Vec<String>> {
    let mut all = if count_local { ledger.agent_reviewers(&view.url, &view.head_sha).ok()? } else { Vec::new() };
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
    let decision = judge(ledger, command, &repo, &view, merger, source).await?;
    Some((view, decision))
}

/// The merge decision for `view` under every policy that applies (the
/// owner's rules and the repository's file; each can only tighten).
pub(crate) async fn judge(ledger: &mut Ledger, command: &GhPr, repo: &str, view: &PrView, merger: &str, source: &impl PrSource) -> Option<Decision> {
    let judged = super::work::judged(ledger, merger, command, repo, view, source).await?;
    if judged.review_only {
        return Some(Decision::NeedsHuman("this Agent does not maintain this repository".into()));
    }
    if let Some(reason) = judged.author_reason {
        return Some(Decision::NeedsHuman(reason));
    }
    // The file keeps its own rule: another Agent than the merger, local only.
    if let Some(file) = &judged.file {
        let local = ledger.agent_reviewers(&view.url, &view.head_sha).ok()?;
        if let decision @ Decision::NeedsHuman(_) = evaluate(file, &view.facts, &local, merger) {
            return Some(decision);
        }
    }
    if let Some(rules) = &judged.rules {
        let approvers = approvers(ledger, command, repo, view, &judged.trusted_reviewers, (judged.own_login.as_deref(), judged.count_local), source).await?;
        if let decision @ Decision::NeedsHuman(_) = evaluate(rules, &view.facts, &approvers, &judged.excluded) {
            return Some(decision);
        }
    }
    Some(Decision::Merge)
}

#[cfg(test)]
pub(crate) mod tests;
