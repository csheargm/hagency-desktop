//! The auto-merge gate for a Codex Agent (ADR 0016): Codex asks to run a
//! command through `item/commandExecution/requestApproval`, which reaches the
//! very gate a Claude `Bash` call does ([`gate::auto_answer`], the proposal
//! read the same way by [`gate::parse_command`]). An approving review allowed
//! there is held under the command's exact item and counts only once Codex
//! reports that item completed with exit code 0, as Claude's
//! `tool_finished` settles it ([`Ledger::settle_agent_review`]).
use super::gate;
use super::github::PrSource;
use crate::{Ledger, Result, ToolProposal};
use serde_json::Value;

/// The one Codex approval method that runs a shell command.
pub const COMMAND_APPROVAL: &str = "item/commandExecution/requestApproval";

/// The key a Codex command's held review waits under: that item in that
/// thread and turn, never one of another turn or a Claude tool call.
pub fn call_key(thread: &str, turn: &str, item: &str) -> String {
    format!("codex:{thread}:{turn}:{item}")
}

/// The gate's answer to a Codex approval request, or `None` when the normal
/// approval flow (and its card) must run unchanged. `proposal.arguments` are
/// the request's params: their `itemId` names the command. A request without
/// one is still judged, but an approving review in it never counts.
/// `(thread, turn)` is the turn the request belongs to.
pub async fn auto_answer(ledger: &mut Ledger, owner: &str, method: &str, proposal: &ToolProposal, (thread, turn): (&str, &str), now: i64, source: &impl PrSource) -> Option<bool> {
    if method != COMMAND_APPROVAL || proposal.tool != "codex.command" {
        return None;
    }
    let call = proposal.arguments["itemId"].as_str().filter(|item| !item.is_empty()).map(|item| call_key(thread, turn, item)).unwrap_or_default();
    gate::auto_answer(ledger, owner, proposal, &call, now, source).await
}

/// A Codex item finished: a review held under it counts only if it is the
/// command itself and it completed with exit code 0 (failed, declined or
/// unknown outcomes drop the held review).
pub fn settle(ledger: &mut Ledger, thread: &str, turn: &str, item: &Value) -> Result<()> {
    if item["type"] != "commandExecution" {
        return Ok(());
    }
    let Some(id) = item["id"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(());
    };
    let succeeded = item["status"] == "completed" && item["exitCode"] == 0;
    ledger.settle_agent_review(&call_key(thread, turn, id), succeeded)
}

#[cfg(test)]
#[path = "codex_review_tests.rs"]
mod tests;
