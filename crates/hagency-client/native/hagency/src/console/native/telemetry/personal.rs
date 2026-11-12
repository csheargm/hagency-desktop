//! Personal usage comes from the already authenticated native execution path.
//! Its resource namespace is separate from rentals; no account is guessed from
//! a credential directory, email, model name or local provider reference.
use super::*;
use hagency_metering::dashboard::ledger::{CountMode, CountScope, UsageSource};

impl OwnerTelemetry {
    pub(crate) fn personal_usage(&self, binding: &ExecutionBinding, usage: &hagency_agent_local::Usage) -> Result<(), TelemetryError> {
        self.terminal_usage(binding, usage, "personal-terminal")
    }

    /// One execution whose runtime reports its usage once, at its end.
    pub(super) fn terminal_usage(&self, binding: &ExecutionBinding, usage: &hagency_agent_local::Usage, stream: &'static str) -> Result<(), TelemetryError> {
        let (input, cache_read, cache_write, output) = usage.category_counts();
        let event = UsageEvent {
            execution_id: binding.execution_id.clone(),
            stream: stream.into(),
            event_id: Some(binding.execution_id.clone()),
            sequence: Some(1),
            observed_at_ms: now_ms(),
            measured_at_ms: None,
            mode: CountMode::Delta,
            scope: CountScope::Own,
            counts: TokenCounts { input, cache_read, cache_write, output },
            reasoning_output: if binding.runtime == Runtime::Codex { output.map(|_| usage.reasoning_output) } else { None },
            complete: [input, cache_read, cache_write, output].iter().all(Option::is_some),
            source: match binding.runtime {
                Runtime::Codex => UsageSource::CodexRuntime,
                Runtime::Claude => UsageSource::ClaudeRuntime,
                Runtime::Octos => UsageSource::OctosRuntime,
            },
        };
        let mut inner = self.lock()?;
        let result = inner.bind(binding.clone()).and_then(|_| inner.event(event, stream).map(|_| ()));
        inner.active.remove(&binding.execution_id);
        if let Err(error) = result {
            inner.mark_gap();
            inner.fail(&error);
            return Err(error);
        }
        Ok(())
    }

    /// Historical personal Agent consumption is shown separately from pools.
    /// Hosts may enrich these names from an authenticated Agent roster.
    pub fn personal_resources(&self, at_ms: u64, options: SnapshotOptions) -> Result<Vec<ResourceUsage>, TelemetryError> {
        let parts = self.parts(at_ms, options)?;
        self.read(|collector| parts.resources.into_iter()
            .filter(|resource| resource.resource_id.starts_with("personal-agent:"))
            .map(|resource| {
                let bindings: Vec<_> = collector.ledger().bindings()
                    .filter(|binding| binding.resource_id == resource.resource_id).collect();
                let runtimes: std::collections::BTreeSet<_> = bindings.iter().map(|b| b.runtime.as_str()).collect();
                let models: std::collections::BTreeSet<_> = bindings.iter().filter_map(|b| b.model.as_deref()).collect();
                ResourceUsage {
                    id: resource.resource_id,
                    name: "Personal Agent".into(),
                    account_id: None,
                    provider: if runtimes.len() == 1 { runtimes.into_iter().next().unwrap().into() } else { "mixed".into() },
                    model: if models.len() == 1 { models.into_iter().next().unwrap().into() } else { String::new() },
                    agent_count: Some(1),
                    budget_tokens: None,
                    state: resource.state.as_str().into(),
                    counts: resource.counts,
                }
            }).collect())
    }
}
