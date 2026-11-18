//! A room member's harmless look inside the Agent's Room workspace runs
//! without a card (ADR 0025): `pwd`, `ls`, `cat`, a `Read` of a file there
//! ([`crate::outside::inspects_workspace`]). Checked against the Room
//! workspace, never the directory the permission callback derives from the
//! call's own path argument. A shell line runs from the workspace itself
//! (`cd '<workspace>' && ( <line> )`), so a directory the shell moved to earlier
//! never widens it.
//!
//! The dispatch must still check out (binding, expiry, no policy saved since
//! admission, requests not refused), and a Deny the owner SAVED on any layer
//! that applies (or a saved tool rule leaving this tool out) ends the
//! shortcut: then the owner's rule refuses as before.
//! The owner's own requests keep the owner's approval mode.
use crate::{Error, Ledger, Result, ToolPolicy, ToolProposal, admitted_policies, applicable_marked, outside, tool_layers, watch_trust};
use rusqlite::TransactionBehavior;

impl Ledger {
    /// How `proposal` (rooted at the Room workspace) runs when it is a room
    /// member's look inside that workspace; `None`: the ordinary rules decide.
    pub fn member_inspection(&mut self, proposal: &ToolProposal, now: i64) -> Result<Option<watch_trust::TrustedStep>> {
        if proposal.scope.requester == self.owner || !outside::inspects_workspace(proposal) {
            return Ok(None);
        }
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (original, current) = match admitted_policies(&tx, proposal, now) {
            Ok(layers) => layers,
            Err(Error::Denied) => return Ok(None),
            Err(other) => return Err(other),
        };
        let unchanged = original.iter().zip(&current).all(|(o, c)| o.0 == c.0);
        // A saved Deny, or a saved tool rule that leaves this tool out, refuses.
        let refuses = |rule: &ToolPolicy| match rule {
            ToolPolicy::Deny => true,
            // The directories too, as the ordinary validator reads them (GPT review).
            ToolPolicy::AllowWithRules { tools, directories } => {
                !rule.is_full_trust() && !(tools.contains(&proposal.tool) && directories.contains(&proposal.canonical_directory))
            }
            ToolPolicy::AskOwner => false,
        };
        let saved_deny = applicable_marked(&tool_layers(&original, &current), false).iter().any(|(rule, _)| refuses(rule));
        if !unchanged || saved_deny {
            return Ok(None);
        }
        Ok(Some(match proposal.tool.as_str() {
            "claude.Bash" => {
                let line = proposal.arguments["command"].as_str().unwrap_or_default();
                watch_trust::TrustedStep::Command(format!("cd {} && ( {line} )", watch_trust::quote(&proposal.canonical_directory)))
            }
            _ => watch_trust::TrustedStep::Allow,
        }))
    }
}

impl Ledger {
    /// Whether a layer the owner never saved makes `proposal` (a room
    /// member's request) ask: that card is the owner's alone, so no automatic
    /// path may answer it in the owner's name (GPT review, Nov 17).
    pub(crate) fn member_default_asks(&self, proposal: &ToolProposal, now: i64) -> Result<bool> {
        if proposal.scope.requester == self.owner {
            return Ok(false);
        }
        let (original, current) = admitted_policies(&self.db, proposal, now)?;
        Ok(applicable_marked(&tool_layers(&original, &current), false).iter().any(|(_, member_default)| *member_default))
    }
}

#[cfg(test)]
pub(crate) mod tests;
