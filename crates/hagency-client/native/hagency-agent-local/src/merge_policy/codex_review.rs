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
use crate::{Ledger, Result, ToolProposal};
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

#[cfg(test)]
#[path = "codex_review_tests.rs"]
mod tests;
