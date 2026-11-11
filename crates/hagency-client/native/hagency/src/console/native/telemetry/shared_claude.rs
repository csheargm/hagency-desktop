//! Pooled Claude usage for the owner's report.
//!
//! A pooled Claude process serves one call and the CLI states that process's
//! whole usage in its final result, so one delta per call is the measurement.
//! Attribution is the same as for a pooled Codex call (`hook::shared_turn`):
//! the claimed runtime command and the admitted work, never runtime output.
//! No provider account is registered: the launch credential is not an
//! identity this report may name.
//!
//! Like every telemetry write, a failure here is counted and logged only; it
//! cannot change the call, its receipt or budget accounting.
use super::*;
use hagency_store::owner_resources::shared_execution::{SharedRuntimeCommand, SharedWorkItem};

/// Stream label of the single final result a pooled Claude call reports.
pub(super) const CLAUDE_RESULT_STREAM: &str = "claude-result";

/// Attribution of one pooled Claude call, from trusted execution records.
pub(super) fn pooled_claude_binding(
    resource_id: &str,
    allocation_id: &str,
    agent_id: &str,
    requester: &str,
    room: &str,
    call_id: &str,
    model: &str,
) -> ExecutionBinding {
    ExecutionBinding {
        execution_id: format!("shared:{call_id}"),
        resource_id: resource_id.to_owned(),
        account_id: None,
        allocation_id: Some(allocation_id.to_owned()),
        agent_id: Some(agent_id.to_owned()),
        requester_id: Some(requester.to_owned()),
        room_id: Some(room.to_owned()),
        runtime: Runtime::Claude,
        runtime_version: None,
        model: (!model.is_empty()).then(|| model.to_owned()),
        parent_execution_id: None,
        started_at_ms: now_ms(),
    }
}

impl OwnerTelemetry {
    /// `usage` is the call's reported usage when the durable receipt holds it
    /// complete. `None` lists the call without a measurement, so the pool's
    /// known totals stay and read as partial; an all-unknown measurement
    /// would blank them for the whole period.
    pub(in crate::console::native) fn shared_claude_usage(
        &self,
        command: &SharedRuntimeCommand,
        work: &SharedWorkItem,
        call_id: &str,
        model: &str,
        usage: Option<&hagency_agent_local::Usage>,
    ) {
        let reserve = &command.reserve_command;
        let binding = pooled_claude_binding(
            &reserve.resource_id,
            &reserve.allocation_id,
            &reserve.proposed_agent_id,
            &work.requester_mxid,
            &work.room_id,
            call_id,
            model,
        );
        if self.pooled_claude_call(binding, usage).is_err() {
            tracing::warn!(
                code = "shared_claude_usage_unavailable",
                "Pooled Claude measurement could not be recorded in the owner's report"
            );
        }
    }

    pub(super) fn pooled_claude_call(
        &self,
        binding: ExecutionBinding,
        usage: Option<&hagency_agent_local::Usage>,
    ) -> Result<(), TelemetryError> {
        match usage.filter(|usage| usage.is_reported()) {
            Some(usage) => self.terminal_usage(&binding, usage, CLAUDE_RESULT_STREAM),
            None => {
                let id = binding.execution_id.clone();
                let bound = self.bind_execution(binding);
                self.finish_execution(&id);
                if let Err(error) = &bound
                    && let Ok(mut inner) = self.lock()
                {
                    inner.mark_gap();
                    inner.fail(error);
                }
                bound.map(|_| ())
            }
        }
    }
}
