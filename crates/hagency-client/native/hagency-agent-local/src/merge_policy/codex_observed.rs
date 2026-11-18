//! Codex reviews the gate never answered (live PRs #50, #55, Nov 17): Codex
//! ran `gh pr review` without an approval request, or the owner's card or a
//! saved rule allowed it. The verdict is recorded only from evidence, and
//! nothing is authorized here (GPT security review of daca7ddc):
//!
//! - the owner allowed that exact command: a saved rule or the owner's card
//!   answered its request (its real directory and risk), or, with no request,
//!   a saved allow rule covers the command in the directory it ran in.
//!   Asking the owner without an answer records nothing;
//! - the command, read exactly as the gate reads it, completed with exit 0,
//!   and GitHub holds a review it attached to the current head whose body
//!   names that head after the marker, received after the command started;
//! - the gate's checks pass: auto-merge switch, scope not Deny, owner rules
//!   bound to this room, member review sharing, low-risk files and CI, and the
//!   reviewer is not the Agent that wrote the pull request;
//! - the approval is stored for this room only, and each item counts once:
//!   a repeated completion, or an item the gate already answered, is ignored.
use super::codex_review::{call_key, settle};
use super::gate;
use super::github::PrSource;
use crate::{Ledger, Result, Scope, ToolPolicy, ToolProposal};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Seconds GitHub's clock may run behind this device's when a review's
/// receipt is compared with the command's start.
const CLOCK_SKEW_SECS: i64 = 120;
/// Seconds the proposal rebuilt for an unasked command stays valid.
const OBSERVED_LIFETIME_SECS: i64 = 300;

/// What [`record_observed`] did with a `gh pr review` it saw run.
pub type Observed = std::result::Result<Recorded, &'static str>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    Approval,
    Withdrawn,
}

/// One root turn's review evidence, kept by the Codex turn loop.
#[derive(Default)]
pub struct TurnReviews {
    /// Command items whose gate accept was sent.
    pub sent: HashSet<String>,
    /// Command items a saved rule or the owner's card allowed: the exact proposal.
    decided: HashMap<String, ToolProposal>,
    /// When each command item started on this device.
    started: HashMap<String, i64>,
    /// Command items already completed: a repeated completion is ignored.
    done: HashSet<String>,
}
impl TurnReviews {
    /// An accept a rule or the owner's card answered was sent for `key`.
    pub fn decided(&mut self, key: String, proposal: ToolProposal) {
        self.decided.insert(key, proposal);
    }
    /// An item started in `(thread, turn)` at `now`.
    pub fn started(&mut self, (thread, turn): (&str, &str), item: &Value, now: i64) {
        if let (Some("commandExecution"), Some(id)) = (item["type"].as_str(), item["id"].as_str()) {
            self.started.entry(call_key(thread, turn, id)).or_insert(now);
        }
    }
    /// An item completed: settle what the gate held, then (root turn, not
    /// gate-answered, first completion only) record a review it demonstrably posted.
    #[allow(clippy::too_many_arguments)]
    pub async fn completed(&mut self, ledger: &mut Ledger, scope: &Scope, dispatch: &str, (thread, turn): (&str, &str), item: &Value, (root, by_gate): (bool, bool), now: i64, source: &impl PrSource) -> Result<Option<Observed>> {
        let Some(id) = item["id"].as_str().filter(|id| item["type"] == "commandExecution" && !id.is_empty()) else {
            return Ok(None);
        };
        let key = call_key(thread, turn, id);
        if !self.done.insert(key.clone()) {
            return Ok(None);
        }
        if settle(ledger, (thread, turn), item, &mut self.sent)? || !root || by_gate {
            return Ok(None);
        }
        let seen = Seen { started: self.started.get(&key).copied(), decided: self.decided.get(&key) };
        Ok(record_observed(ledger, scope, dispatch, item, seen, now, source).await)
    }
}

/// What the turn saw of one command before it completed.
pub struct Seen<'a> {
    pub started: Option<i64>,
    pub decided: Option<&'a ToolProposal>,
}

/// Record the verdict of a completed `gh pr review` the gate did not answer
/// (see the module docs). `None` for any other command (not logged).
pub async fn record_observed(ledger: &mut Ledger, scope: &Scope, dispatch: &str, item: &Value, seen: Seen<'_>, now: i64, source: &impl PrSource) -> Option<Observed> {
    if item["type"] != "commandExecution" || item["status"] != "completed" || item["exitCode"] != 0 {
        return None;
    }
    let raw = item["command"].as_str()?;
    if !raw.contains("gh pr review") {
        return None;
    }
    let skip = |reason| Some(Err(reason));
    let Some(proposal) = allowed_proposal(ledger, scope, dispatch, item, seen.decided, now) else { return skip("not_allowed_by_owner") };
    let mut read = proposal.clone();
    // A newline inside the body reads as a space: any word it would start outside the body makes the command unreadable.
    read.arguments = serde_json::json!({ "command": raw.replace(['\n', '\r'], " ") });
    let Some(command) = gate::parse_command(&read) else { return skip("unreadable_command") };
    let Some(repo) = command.repo.clone() else { return skip("no_repo") };
    if !matches!(command.kind, gate::Kind::Review | gate::Kind::RequestChanges) {
        return skip("not_a_verdict");
    }
    if let Err(reason) = reviewer_may_review(ledger, scope, &repo) {
        return skip(reason);
    }
    let Some(view) = source.pull_request(&command).await else { return skip("pull_request_unreadable") };
    if command.kind == gate::Kind::RequestChanges {
        return Some(ledger.withdraw_agent_review(&view.url, &scope.agent).map(|_| Recorded::Withdrawn).map_err(|_| "ledger"));
    }
    if let Err(reason) = names_current_head(ledger, raw, &repo, command.number, &view, &scope.agent) {
        return skip(reason);
    }
    if gate::qualifies(ledger, &command, &view, (&scope.agent, &scope.room), source).await != Some(true) {
        return skip("policy_or_ci");
    }
    let Some(started) = seen.started else { return skip("start_unknown") };
    match source.approvals_on(&command, &repo, &view.head_sha).await {
        None => return skip("github_reviews_unreadable"),
        Some(received) if !received.iter().any(|at| *at >= started - CLOCK_SKEW_SECS) => return skip("not_on_github_for_this_head"),
        Some(_) => {}
    }
    Some(ledger.record_agent_review(&view.url, &view.head_sha, &scope.agent, &scope.room).map(|_| Recorded::Approval).map_err(|_| "ledger"))
}

/// The proposal the owner allowed for this command: the one a rule or the
/// owner's card decided, or, when Codex asked nothing, the command as it ran
/// in its own directory if a saved allow rule covers exactly that.
fn allowed_proposal(ledger: &mut Ledger, scope: &Scope, dispatch: &str, item: &Value, decided: Option<&ToolProposal>, now: i64) -> Option<ToolProposal> {
    if let Some(proposal) = decided {
        // The decision was for this very command in this very directory, and
        // the scope and policy still stand now (GPT review).
        let same_command = proposal.arguments["command"] == item["command"];
        let same_directory = item["cwd"].as_str().is_none_or(|cwd| cwd == proposal.canonical_directory);
        let standing = !matches!(ledger.tool_disposition(proposal, now), Ok(ToolPolicy::Deny) | Err(_));
        return (same_command && same_directory && standing).then(|| proposal.clone());
    }
    let directory = item["cwd"].as_str().filter(|path| std::path::Path::new(path).is_absolute())?;
    let versions = ledger.policy_snapshot(scope).ok()?;
    let proposal = ToolProposal {
        scope: scope.clone(),
        dispatch: dispatch.into(),
        tool: "codex.command".into(),
        arguments: serde_json::json!({ "command": item["command"] }),
        canonical_directory: directory.into(),
        risk: "high".into(),
        policy_revision: versions.map(|v| v.revision),
        expires: now + OBSERVED_LIFETIME_SECS,
    };
    match ledger.tool_disposition(&proposal, now).ok()? {
        ToolPolicy::AllowWithRules { tools, directories } if tools.contains(&proposal.tool) && directories.contains(&proposal.canonical_directory) => Some(proposal),
        _ => None,
    }
}

/// The gate's per-Agent checks: switch on, owner rules (if any) bound to
/// this room, and a room member's review only where the owner shares review.
fn reviewer_may_review(ledger: &Ledger, scope: &Scope, repo: &str) -> std::result::Result<(), &'static str> {
    if ledger.auto_merge(&scope.agent).ok() != Some(true) {
        return Err("auto_merge_off");
    }
    match ledger.owner_rules(&scope.agent, repo) {
        Ok(Some(rules)) if rules.room_id != scope.room => return Err("repo_in_another_room"),
        Err(_) => return Err("rules_unreadable"),
        _ => {}
    }
    if scope.requester != ledger.owner && !ledger.room_share(&scope.agent, &scope.room).is_ok_and(|share| share.review) {
        return Err("member_review_not_shared");
    }
    Ok(())
}

/// The text that ran names the current head right after the marker, and the
/// reviewer is not the Agent that wrote the pull request.
fn names_current_head(ledger: &Ledger, raw: &str, repo: &str, number: u64, view: &super::github::PrView, reviewer: &str) -> std::result::Result<(), &'static str> {
    if !raw.contains(&format!("{} {}", gate::APPROVE_MARKER, view.head_sha)) {
        return Err("not_the_current_head");
    }
    let mut authors = ledger.pr_author_agents(repo, number).map_err(|_| "authors_unreadable")?;
    authors.extend(view.opened_by.clone());
    if authors.iter().any(|author| author.eq_ignore_ascii_case(reviewer)) {
        return Err("reviewer_wrote_it");
    }
    Ok(())
}

#[cfg(test)]
#[path = "codex_observed_tests.rs"]
mod tests;
