//! The auto-merge gate for a Codex Agent (ADR 0016): Codex asks to run a
//! command through `item/commandExecution/requestApproval`, which reaches the
//! very gate a Claude `Bash` call does ([`gate::auto_answer`], the proposal
//! read the same way by [`gate::parse_command`]). An approving review allowed
//! there is held under the command's exact item and counts only once Codex
//! reports that item completed with exit code 0, as Claude's
//! `tool_finished` settles it ([`Ledger::settle_agent_review`]), and only when
//! this turn actually sent Codex the gate's accept for it (GPT review: a
//! withdrawn, stopped or unsent answer never counts).
use super::gate;
use super::github::PrSource;
use crate::{Ledger, Result, Scope, ToolProposal};
use serde_json::Value;
use std::collections::HashSet;

/// The one Codex approval method that runs a shell command.
pub const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";

/// The key a Codex command's held review waits under: that item in that
/// thread and turn, never one of another turn or a Claude tool call. JSON, so
/// no choice of ids can make two triples share a key (GPT review).
pub fn call_key(thread: &str, turn: &str, item: &str) -> String {
    format!("{KEY_PREFIX}{}", serde_json::json!([thread, turn, item]))
}
/// Every Codex key starts with this; a Claude call id never settles one.
pub const KEY_PREFIX: &str = "codex:";

/// The gate's answer to a Codex approval request, or `None` when the normal
/// approval flow (and its card) must run unchanged. `proposal.arguments` are
/// the request's params: their `itemId` names the command. A request without
/// one is still judged, but an approving review in it never counts.
/// `(thread, turn)` is the turn the request belongs to. A nested `execve`
/// approval (it names an `approvalId` inside an enclosing command) is never
/// answered here: the enclosing item's exit code is not its outcome (GPT review).
pub async fn auto_answer(ledger: &mut Ledger, owner: &str, method: &str, proposal: &ToolProposal, (thread, turn): (&str, &str), now: i64, source: &impl PrSource) -> Option<bool> {
    if method != COMMAND_APPROVAL || proposal.tool != "codex.command" || !proposal.arguments["approvalId"].is_null() {
        return None;
    }
    let call = proposal.arguments["itemId"].as_str().filter(|item| !item.is_empty()).map(|item| call_key(thread, turn, item)).unwrap_or_default();
    gate::auto_answer(ledger, owner, proposal, &call, now, source).await
}

/// A Codex item finished: a review held under it counts only if it is the
/// command itself, its accept was sent (`sent`, the keys this turn answered
/// through the gate) and it completed with exit code 0. Failed, declined,
/// unsent or unknown outcomes drop the held review.
/// True when the item was a command whose accept the gate sent this turn.
pub fn settle(ledger: &mut Ledger, (thread, turn): (&str, &str), item: &Value, sent: &mut HashSet<String>) -> Result<bool> {
    if item["type"] != "commandExecution" {
        return Ok(false);
    }
    let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(false);
    };
    let key = call_key(thread, turn, id);
    let ran = item["status"] == "completed" && item["exitCode"] == 0;
    // A request for changes withdraws approvals once it ran, whoever allowed it.
    ledger.settle_withdrawal(&key, ran)?;
    let gated = sent.remove(&key);
    ledger.settle_agent_review(&key, gated && ran)?;
    Ok(gated)
}

/// A command Codex ran without asking (the owner's full trust turns
/// approvals off, so the gate never saw it): an approving review of the
/// exact current head is recorded when the gate would have allowed it —
/// same parse (one `gh pr review <N> --repo <owner/repo>` with the
/// marker), auto-merge switch, scope and policy checks, and the repository's
/// owner rules (if any) bound to this room. A blocking review that ran
/// withdraws this Agent's approvals. Nothing is authorized here: this only
/// records what demonstrably ran (live PR #50, Nov 17). A newline inside the
/// review body reads as a space: any word it would start outside the body
/// makes the command unreadable, so nothing is recorded.
pub async fn record_observed(ledger: &mut Ledger, scope: &Scope, dispatch: &str, cwd: &str, item: &Value, now: i64, source: &impl PrSource) -> Option<()> {
    if item["type"] != "commandExecution" || item["status"] != "completed" || item["exitCode"] != 0 {
        return None;
    }
    let line = item["command"].as_str()?.replace(['\n', '\r'], " ");
    let directory = item["cwd"].as_str().filter(|path| std::path::Path::new(path).is_absolute()).unwrap_or(cwd);
    let versions = ledger.policy_snapshot(scope).ok()?;
    let proposal = ToolProposal {
        scope: scope.clone(),
        dispatch: dispatch.into(),
        tool: "codex.command".into(),
        arguments: serde_json::json!({ "command": line }),
        canonical_directory: directory.into(),
        risk: "high".into(),
        policy_revision: versions.map(|v| v.revision),
        expires: now + OBSERVED_LIFETIME_SECS,
    };
    let command = gate::parse_command(&proposal)?;
    let repo = command.repo.clone()?;
    if !matches!(command.kind, gate::Kind::Review | gate::Kind::RequestChanges) || !ledger.auto_merge(&scope.agent).ok()? {
        return None;
    }
    if matches!(ledger.tool_disposition(&proposal, now).ok()?, crate::ToolPolicy::Deny) {
        return None;
    }
    if ledger.owner_rules(&scope.agent, &repo).ok()?.is_some_and(|rules| rules.room_id != scope.room) {
        return None;
    }
    let view = source.pull_request(&command).await?;
    if command.kind == gate::Kind::RequestChanges {
        return ledger.withdraw_agent_review(&view.url, &scope.agent).ok();
    }
    if !gate::qualifies(ledger, &command, &view, &scope.agent, source).await? {
        return None;
    }
    ledger.record_agent_review(&view.url, &view.head_sha, &scope.agent).ok()
}

/// Seconds the reconstructed proposal of an observed command stays valid.
const OBSERVED_LIFETIME_SECS: i64 = 300;

#[cfg(test)]
#[path = "codex_review_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "codex_observed_tests.rs"]
mod observed_tests;
