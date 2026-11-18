//! The in-process read of one Agent's waiting tool requests, off the owner
//! command line.
//!
//! `execute` runs every owner command one at a time (`calls`) and first
//! re-checks the Matrix account with the server (`sync_matrix`, which takes
//! the same renewal line as the Agent's own turn). The desktop reads each of
//! its Agents' queues on every poll; behind a slow listing, decision or
//! renewal those reads missed their 2 s bound, and the requests an Agent was
//! waiting on mid-turn never reached a card (Nov 18, `approvals_read_timeout`).
//! The queue is in this process's memory: reading it needs the owner's
//! admission and a proof that is still valid (`live_device`), nothing more.
//! Deciding a request still goes through `execute`.
use super::*;

impl NativeOwner {
    /// `{"approvals": [...]}` for `agent_id`, as `GET runtime/approvals`
    /// answers it, without waiting for other owner commands.
    pub async fn local_approvals(&self, agent_id: &str) -> Result<Value, NativeError> {
        key(agent_id)?;
        let command = Command::Local {
            agent_id: agent_id.into(),
            action: LocalAction::Approvals,
            input: None,
            binding_id: None,
            requester: None,
        };
        self.admit_owner_command(&command).await?;
        let console = &self.0.host.console;
        let approvals = console
            .0
            .owned_runtime
            .pending(console, agent_id)
            .await
            .map_err(|e| {
                let e = super::super::owned_agents::runtime_error(e);
                failure(e.status, &e.code)
            })?;
        Ok(json!({ "approvals": approvals }))
    }

    /// Test hook: hold the owner command line, as a slow command does.
    #[cfg(test)]
    pub(in crate::console) async fn hold_owner_commands(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.0.calls.lock().await
    }
}
