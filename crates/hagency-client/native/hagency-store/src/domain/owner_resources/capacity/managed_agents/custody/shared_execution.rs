//! First shared text execution protocol. Wire values confer no native authority.
use super::*;

pub(super) fn check_period_account(db:&Connection, child:&str, binding:&Binding)->Result<(),Error>{
    attempt_work::account_budget(db,child,binding)
}
/// Whether a qualified process ran on an acceptable provider account. A Codex
/// process must run on the resource's recorded account. A pooled Claude
/// process runs on whichever verified Claude account the owner's Claude Code
/// is signed in to now (owners switch between their accounts); custody,
/// receipts and pins keep the resource's recorded identity.
fn provider_accepted(runtime: &str, recorded: &str, observed: Option<&str>) -> bool {
    match observed {
        Some(observed) if observed == recorded => true,
        Some(observed) => runtime == "claude" && types::valid_digest(observed),
        None => false,
    }
}
// V2 claim and actual-process evidence. Its model-call adapter remains separate.
mod tool_policy;
pub use tool_policy::SharedAgentToolPolicy;
use tool_policy::owner_tool_policy;
mod web_policy;
use web_policy::owner_live_web;
mod attempt;
mod attempt_fence;
mod attempt_process;
mod attempt_work;
mod attempt_recovery;
mod attempt_unstarted;
mod attempt_timeout;
mod process_binding;
use process_binding::ProcessBinding;
pub use attempt::*;
pub use attempt_fence::SharedAttemptFence;
pub use attempt_process::SharedAttemptProcessSink;
mod capture;
mod start_failure;
mod claim;
mod initial_unstarted;
mod process;
mod recovery;
mod successor;
mod start_intent;
mod start_selection;
mod start_inventory;
pub use start_inventory::{SharedSuccessorPage, SharedSuccessorSummary};
pub use start_selection::SharedSuccessorSelection;
pub use start_intent::{SharedAttemptStartInput,SharedAttemptStartIntent};
mod work;
pub use capture::{SharedCallSink, SharedNativeSession, SharedClaudeSession, SharedOctosSession};
pub use process::SharedProcessSink;
pub use recovery::SharedSavedWork;
pub use successor::{SharedSuccessorBudget, SharedSuccessorBudgetState};
pub use work::{SharedCallCustody, SharedStartAttempt, VerifiedSharedActivity};
mod types;
mod native_session_types;
pub use native_session_types::*;
pub use claim::{
    SharedRuntimeClaim, SharedRuntimeCustody, SharedRuntimeHistory, VerifiedSharedRuntimeDelivery,
};
pub use types::*;
#[cfg(test)]
mod tests;

mod demand;
pub use demand::*;

mod context;
mod context_types;
pub use context::SharedPreparedInput;
pub use context_types::*;

#[cfg(test)]
mod context_tests;

/// Shared bounded call accounting for additive budget evidence. No issuance rights.
pub(super) fn budget_liability(db: &Connection, child: &str, binding: &str) -> Result<(u64,u64,bool,bool),Error> {
    let calls=successor::calls(db,child,binding)?;
    Ok((calls.consumed,calls.retained,calls.complete,calls.unrepresentable))
}

#[cfg(feature="agent-budget")]
pub(super) fn budget_account_capacity(db:&Connection,child:&str,binding:&Binding)->Result<(),Error>{
    attempt_work::account_budget(db,child,binding)
}

mod budget_types;
pub use budget_types::*;

#[cfg(feature="shared-budget-execution")]
mod budget_snapshot;
#[cfg(feature="shared-budget-execution")]
mod budget_attempt;
#[cfg(feature="shared-budget-execution")]
mod budget_process;
#[cfg(feature="shared-budget-execution")]
mod budget_fence;
#[cfg(feature="shared-budget-execution")]
mod budget_unstarted;
#[cfg(feature="shared-budget-execution")]
mod budget_timeout;
#[cfg(feature="shared-budget-execution")]
pub use budget_attempt::*;
#[cfg(feature="shared-budget-execution")]
pub use budget_process::SharedBudgetAttemptProcessSink;
#[cfg(feature="shared-budget-execution")]
pub use budget_snapshot::SharedBudgetSuccessorSelection;

#[cfg(feature = "shared-budget-execution")]
mod budget_work;

#[cfg(feature = "shared-budget-execution")]
mod budget_refusal;
#[cfg(feature = "shared-budget-execution")]
pub use budget_refusal::VerifiedSharedBudgetWorkRefusal;

#[cfg(feature = "shared-budget-execution")]
mod budget_recovery;

#[cfg(feature = "shared-budget-execution")]
mod budget_start;
#[cfg(feature = "shared-budget-execution")]
pub use budget_start::SharedBudgetAttemptStartIntent;

mod budget_demand;
pub use budget_demand::SharedBudgetDemandIntent;
#[cfg(feature="shared-budget-execution")]
pub use budget_demand::VerifiedSharedBudgetDemand;
#[cfg(feature="shared-budget-execution")]
pub use budget_demand::SharedBudgetDemandSubmission;
#[cfg(test)]
#[test]
fn claude_runs_on_any_verified_account_and_codex_on_its_own() {
    let (a, b) = ("a".repeat(64), "b".repeat(64));
    assert!(provider_accepted("codex", &a, Some(&a)));
    assert!(!provider_accepted("codex", &a, Some(&b)));
    assert!(provider_accepted("claude", &a, Some(&a)));
    assert!(provider_accepted("claude", &a, Some(&b)));
    assert!(!provider_accepted("claude", &a, Some("not-a-digest")));
    assert!(!provider_accepted("claude", &a, None));
    assert!(!provider_accepted("codex", &a, None));
}
