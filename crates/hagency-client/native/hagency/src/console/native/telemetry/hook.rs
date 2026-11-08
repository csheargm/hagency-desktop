//! Shared-execution turn hook. Bound once per issued model call, after the
//! store issued the call and the sink bound the actual process.
//!
//! Protocol per runtime event:
//! 1. `pending(&event)` copies only usage evidence and its sequence before
//!    the event moves into `SharedCallSink::observe`.
//! 2. `accept(pending)` runs only when `observe` returned `Ok`, i.e. the sink
//!    verified the same process source and the contiguous sequence.
//! 3. `quota(&process)` reads the process's own sanitized rate-limit
//!    snapshot (from `account/rateLimits/read` or `/updated`).
//!
//! Telemetry failures are logged and counted, never returned: they cannot
//! fail, retry or alter the model turn, its receipt or budget accounting.

use super::{CODEX_READ, CODEX_UPDATED, OwnerTelemetry, codex_account, now_ms};
use hagency_metering::dashboard::ledger::{CountMode, ExecutionBinding, Runtime, UsageEvent};
use hagency_metering::observation::UsageObservation;
use hagency_metering::runtime_usage::{CodexUsage, CounterBreakdown, ProjectionDiagnostics};
use hagency_runtime::codex::session::{
    ObservationKind, RateLimitBucket, RateLimitOrigin, RateLimitSnapshot, RateLimitWindow, UsageBreakdown,
    UsageEvidence,
};
use hagency_runtime::owned::{OwnedSession, SharedTextEvent};
use hagency_store::owner_resources::shared_execution::{SharedRuntimeCommand, SharedWorkItem};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

/// Stream label for the single Codex thread a shared work item runs.
pub(super) const CODEX_THREAD_STREAM: &str = "codex-thread";

/// Evidence copied before the sink consumes the event.
pub(in crate::console::native) struct PendingUsage {
    sequence: u64,
    kind: PendingKind,
}
enum PendingKind {
    Usage(UsageEvidence),
    Ended(Option<UsageEvidence>),
    Dirty,
    Other,
}

pub(in crate::console::native) struct TurnTelemetry {
    telemetry: OwnerTelemetry,
    execution_id: Option<String>,
    epoch: String,
    account_id: Option<String>,
    quota_revision: Option<u64>,
    last_usage: Option<(u64, UsageObservation)>,
    clean: bool,
    finished: bool,
}

fn epoch(call_id: &str) -> String {
    use sha2::{Digest, Sha256};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seed = format!(
        "{call_id}\0{}\0{nanos}\0{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    format!("p{}", &format!("{:x}", Sha256::digest(seed.as_bytes()))[..24])
}

fn breakdown(v: &UsageBreakdown) -> CounterBreakdown {
    CounterBreakdown {
        total_tokens: v.total_tokens(),
        input_tokens: v.input_tokens(),
        cached_input_tokens: v.cached_input_tokens(),
        cache_write_input_tokens: v.cache_write_input_tokens(),
        output_tokens: v.output_tokens(),
        reasoning_output_tokens: v.reasoning_output_tokens(),
    }
}

/// Same projection the budget ledger uses (`ManagedUsageObservation`).
pub(super) fn codex_observation(e: &UsageEvidence) -> Option<UsageObservation> {
    let d = e.diagnostics();
    UsageObservation::codex_runtime(CodexUsage {
        total: breakdown(e.total()),
        last: breakdown(e.last()),
        context_window: e.model_context_window(),
        diagnostics: ProjectionDiagnostics {
            missing: d.has_missing_fields(),
            invalid: d.has_invalid_fields(),
            unsupported: d.has_unsupported_fields(),
        },
    })
    .ok()
}

fn window(w: Option<&RateLimitWindow>) -> Value {
    match w {
        Some(w) => json!({
            "usedPercent": w.used_percent(),
            "windowDurationMins": w.window_minutes(),
            "resetsAt": w.resets_at_seconds(),
        }),
        None => Value::Null,
    }
}

fn bucket(b: &RateLimitBucket) -> Value {
    json!({
        "limitId": b.limit_id(),
        "limitName": b.limit_name(),
        "primary": window(b.primary()),
        "secondary": window(b.secondary()),
        "planType": b.plan_type(),
        "rateLimitReachedType": b.reached_type(),
    })
}

/// Sanitized metering payload and its source label.
pub(super) fn quota_payload(snapshot: &RateLimitSnapshot) -> (&'static str, Value) {
    let primary = snapshot.bucket().map(bucket).unwrap_or(Value::Null);
    match snapshot.origin() {
        RateLimitOrigin::Read => {
            let by_id: serde_json::Map<String, Value> = snapshot
                .by_limit_id()
                .iter()
                .map(|(k, b)| (k.clone(), bucket(b)))
                .collect();
            let by_id = if by_id.is_empty() { Value::Null } else { Value::Object(by_id) };
            (CODEX_READ, json!({"rateLimits": primary, "rateLimitsByLimitId": by_id}))
        }
        RateLimitOrigin::Updated => (CODEX_UPDATED, json!({"rateLimits": primary})),
    }
}

impl OwnerTelemetry {
    /// Bind one issued shared model call. Attribution comes from the claimed
    /// runtime command and admitted work; never from runtime output.
    pub(in crate::console::native) fn shared_turn(
        &self,
        command: &SharedRuntimeCommand,
        work: &SharedWorkItem,
        call_id: &str,
        process: &OwnedSession,
    ) -> TurnTelemetry {
        let reserve = &command.reserve_command;
        let plan = process
            .provider_rate_limits()
            .and_then(RateLimitSnapshot::bucket)
            .and_then(RateLimitBucket::plan_type);
        let registration = codex_account(process.provider_account_digest(), &reserve.resource_id, plan);
        let mut turn = TurnTelemetry {
            telemetry: self.clone(),
            execution_id: None,
            epoch: epoch(call_id),
            account_id: None,
            quota_revision: None,
            last_usage: None,
            clean: true,
            finished: false,
        };
        let Ok(mut inner) = self.0.lock() else {
            return turn;
        };
        let account_id = match inner.account(registration) {
            Ok(id) => Some(id),
            Err(error) => {
                inner.fail(&error);
                None
            }
        };
        let binding = ExecutionBinding {
            execution_id: format!("shared:{call_id}"),
            resource_id: reserve.resource_id.clone(),
            account_id: account_id.clone(),
            allocation_id: Some(reserve.allocation_id.clone()),
            agent_id: Some(reserve.proposed_agent_id.clone()),
            requester_id: Some(work.requester_mxid.clone()),
            room_id: Some(work.room_id.clone()),
            runtime: Runtime::Codex,
            runtime_version: None,
            model: process.running_model().map(str::to_owned),
            parent_execution_id: None,
            started_at_ms: now_ms(),
        };
        let execution_id = binding.execution_id.clone();
        match inner.bind(binding) {
            Ok(_) => turn.execution_id = Some(execution_id),
            Err(error) => {inner.mark_gap();inner.fail(&error)},
        }
        turn.account_id = account_id;
        drop(inner);
        turn.quota(process);
        turn
    }
}

impl TurnTelemetry {
    pub(in crate::console::native) fn pending(&self, event: &SharedTextEvent) -> PendingUsage {
        let observation = event.observation();
        let kind = match observation.kind() {
            ObservationKind::Usage(e) => PendingKind::Usage(e.clone()),
            ObservationKind::TurnEnded(_) => PendingKind::Ended(observation.terminal_usage().cloned()),
            ObservationKind::Tool(_) if event.is_authorized_tool() => PendingKind::Other,
            ObservationKind::Invalidated | ObservationKind::Tool(_) => PendingKind::Dirty,
            ObservationKind::Ignored => PendingKind::Other,
        };
        let kind = if observation.source().is_retired() { PendingKind::Dirty } else { kind };
        PendingUsage {
            sequence: observation.sequence(),
            kind,
        }
    }

    /// Call only after the sink accepted the event (source and sequence).
    pub(in crate::console::native) fn accept(&mut self, pending: PendingUsage) {
        let Some(execution_id) = self.execution_id.clone() else {
            return;
        };
        if self.finished {
            return;
        }
        match pending.kind {
            PendingKind::Usage(evidence) => match codex_observation(&evidence) {
                Some(observation) => {
                    self.record(&execution_id, pending.sequence, &observation, false);
                    self.last_usage = Some((pending.sequence, observation));
                }
                None => {
                    // Arithmetic refusal: no zero is recorded; the turn stays partial.
                    self.clean = false;
                    if let Ok(mut inner) = self.telemetry.0.lock() {
                        inner.fail(&"codex_usage_normalization_refused");
                    }
                }
            },
            PendingKind::Ended(terminal_usage) => {
                self.finished = true;
                if let Some(evidence) = terminal_usage {
                    self.last_usage = codex_observation(&evidence).map(|usage| (pending.sequence, usage));
                    self.clean &= !evidence.diagnostics().has_missing_fields()
                        && !evidence.diagnostics().has_invalid_fields()
                        && !evidence.diagnostics().has_unsupported_fields();
                }
                // Mirrors the sink's completeness rule: clean, terminal, live.
                if self.clean
                    && let Some((_, observation)) = self.last_usage.clone()
                {
                    self.record(&execution_id, pending.sequence, &observation, true);
                }
            }
            PendingKind::Dirty => self.clean = false,
            PendingKind::Other => {}
        }
    }

    /// The sink refused or the turn ended without terminal evidence.
    pub(in crate::console::native) fn abandon(&mut self) {
        self.clean = false;
        self.finished = true;
    }

    fn record(&self, execution_id: &str, sequence: u64, observation: &UsageObservation, complete: bool) {
        let event = match UsageEvent::from_observation(
            observation,
            execution_id,
            CODEX_THREAD_STREAM,
            self.epoch.as_str(),
            now_ms(),
        ) {
            Ok(mut event) => {
                event.sequence = Some(sequence);
                // Sequence is the verified identity; the content digest would
                // make a later identical total look like a duplicate event.
                event.event_id = None;
                event.complete = complete;
                debug_assert!(matches!(event.mode, CountMode::Cumulative { .. }));
                event
            }
            Err(_) => return,
        };
        if let Ok(mut inner) = self.telemetry.0.lock()
            && let Err(error) = inner.event(event, &self.epoch)
        {
            inner.fail(&error);
        }
    }

    /// Apply the process's latest account rate-limit snapshot once.
    pub(in crate::console::native) fn quota(&mut self, process: &OwnedSession) {
        let (Some(account), Some(snapshot)) = (self.account_id.as_deref(), process.provider_rate_limits()) else {
            return;
        };
        if self.quota_revision == Some(snapshot.revision()) {
            return;
        }
        self.quota_revision = Some(snapshot.revision());
        let (source, payload) = quota_payload(snapshot);
        if let Ok(mut inner) = self.telemetry.0.lock() {
            inner.quota(account, source, payload, snapshot.received_at_ms());
        }
    }
}

impl Drop for TurnTelemetry {
    fn drop(&mut self) {
        if let Some(id) = &self.execution_id { self.telemetry.finish_execution(id); }
    }
}
