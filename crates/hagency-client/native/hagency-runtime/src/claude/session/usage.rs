//! Numeric projection only; no provider, task, source or billing authority.
use serde_json::Value;
use std::collections::BTreeMap;
const MAX_COUNT: u64 = 9_007_199_254_740_991;
const MAX_STEPS: usize = 1024;
const MAX_MODELS: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum UsageCoverage {
    MainLoopSteps,
    MainLoopResult,
    /// Sum of native per-result deltas belonging to this host turn, including
    /// any subsequent background-usage query. Never a restored session total.
    ReportedModelsResult,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageDiagnostics {
    missing: bool,
    invalid: bool,
    unsupported: bool,
}
impl UsageDiagnostics {
    pub fn has_missing_fields(self) -> bool {
        self.missing
    }
    pub fn has_invalid_fields(self) -> bool {
        self.invalid
    }
    pub fn has_unsupported_fields(self) -> bool {
        self.unsupported
    }
    fn merge(&mut self, other: Self) {
        self.missing |= other.missing;
        self.invalid |= other.invalid;
        self.unsupported |= other.unsupported;
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageCounts {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_write: Option<u64>,
}
impl UsageCounts {
    pub fn input(self) -> Option<u64> {
        self.input
    }
    pub fn output(self) -> Option<u64> {
        self.output
    }
    pub fn cache_read(self) -> Option<u64> {
        self.cache_read
    }
    pub fn cache_write(self) -> Option<u64> {
        self.cache_write
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct UsageEvidence {
    counts: UsageCounts,
    coverage: UsageCoverage,
    diagnostics: UsageDiagnostics,
    steps: u32,
    models: u16,
}
impl UsageEvidence {
    pub fn counts(&self) -> UsageCounts {
        self.counts
    }
    pub fn coverage(&self) -> UsageCoverage {
        self.coverage
    }
    pub fn diagnostics(&self) -> UsageDiagnostics {
        self.diagnostics
    }
    pub fn steps(&self) -> u32 {
        self.steps
    }
    pub fn models(&self) -> u16 {
        self.models
    }
}
fn counter(value: Option<&Value>, diagnostics: &mut UsageDiagnostics) -> Option<u64> {
    match value {
        None | Some(Value::Null) => {
            diagnostics.missing = true;
            None
        }
        Some(value) => match value.as_u64().filter(|v| *v <= MAX_COUNT) {
            Some(v) => Some(v),
            None => {
                diagnostics.invalid = true;
                None
            }
        },
    }
}
fn project(value: &Value, model: bool, step: bool) -> (UsageCounts, UsageDiagnostics) {
    let keys = if model {
        [
            "inputTokens",
            "outputTokens",
            "cacheReadInputTokens",
            "cacheCreationInputTokens",
        ]
    } else {
        [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]
    };
    let mut d = UsageDiagnostics::default();
    if !value.is_object() && !value.is_null() {
        d.invalid = true;
    }
    // Native reports also contain cost, model/provider and counter breakdown
    // metadata. Only these four disjoint billed counters enter the projection:
    // thinking is part of output; cache TTL buckets are part of cache creation.
    // Additional metadata cannot invalidate otherwise complete native usage.
    let counts = UsageCounts {
        input: counter(value.get(keys[0]), &mut d),
        // Placeholder output never participates in duplicate identity or sums.
        output: if step {
            d.missing = true;
            None
        } else {
            counter(value.get(keys[1]), &mut d)
        },
        cache_read: counter(value.get(keys[2]), &mut d),
        cache_write: counter(value.get(keys[3]), &mut d),
    };
    (counts, d)
}
#[derive(Default)]
struct Sum {
    known: u64,
    missing: bool,
}
impl Sum {
    fn add(&mut self, value: Option<u64>) -> Result<(), ()> {
        match value {
            Some(value) => {
                self.known = self
                    .known
                    .checked_add(value)
                    .filter(|n| *n <= MAX_COUNT)
                    .ok_or(())?
            }
            None => self.missing = true,
        };
        Ok(())
    }
    fn value(&self) -> Option<u64> {
        (!self.missing).then_some(self.known)
    }
}
#[derive(Default)]
struct Totals {
    input: Sum,
    output: Sum,
    read: Sum,
    write: Sum,
}
impl Totals {
    fn add(&mut self, c: UsageCounts) -> Result<(), ()> {
        self.input.add(c.input)?;
        self.output.add(c.output)?;
        self.read.add(c.cache_read)?;
        self.write.add(c.cache_write)?;
        // Unknown fields must not hide known combined overflow either.
        self.input
            .known
            .checked_add(self.output.known)
            .and_then(|n| n.checked_add(self.read.known))
            .and_then(|n| n.checked_add(self.write.known))
            .filter(|n| *n <= MAX_COUNT)
            .ok_or(())?;
        Ok(())
    }
    fn counts(&self) -> UsageCounts {
        UsageCounts {
            input: self.input.value(),
            output: self.output.value(),
            cache_read: self.read.value(),
            cache_write: self.write.value(),
        }
    }
}
#[derive(Default)]
pub(super) struct Tracker {
    seen: BTreeMap<String, (UsageCounts, UsageDiagnostics)>,
    totals: Totals,
    diagnostics: UsageDiagnostics,
    invalidated: bool,
    result_ids: BTreeMap<String, [u8; 32]>,
    result_totals: Totals,
    result_diagnostics: UsageDiagnostics,
    result_models: u16,
}
impl Tracker {
    pub(super) fn project(
        &mut self,
        kind: super::EventKind,
        payload: &Value,
    ) -> super::ObservationKind {
        use super::ObservationKind as Kind;
        if self.invalidated {
            return Kind::Invalidated;
        }
        let result = match kind {
            super::EventKind::Assistant => self
                .step(payload)
                .map(|e| e.map_or(Kind::Ignored, Kind::Usage)),
            super::EventKind::Result => self.result(payload).map(|usage| Kind::Result {
                usage,
                is_error: payload["is_error"] == true,
            }),
            _ => Ok(Kind::Ignored),
        };
        result.unwrap_or_else(|()| {
            self.invalidated = true;
            Kind::Invalidated
        })
    }
    fn step(&mut self, payload: &Value) -> Result<Option<UsageEvidence>, ()> {
        if payload["local_command_run"].is_object() {
            return Ok(None);
        }
        // A nested agent is not part of the main-loop partial sum.
        if let Some(parent) = payload.get("parent_tool_use_id").filter(|v| !v.is_null()) {
            return if parent.as_str().is_some_and(|s| !s.is_empty()) {
                Ok(None)
            } else {
                Err(())
            };
        }
        let message = &payload["message"];
        let id = message["id"]
            .as_str()
            .filter(|s| !s.is_empty() && s.len() <= 512)
            .ok_or(())?;
        let (counts, diagnostics) = project(&message["usage"], false, true);
        if let Some(original) = self.seen.get(id) {
            return if original == &(counts, diagnostics) {
                Ok(None)
            } else {
                Err(())
            };
        }
        if self.seen.len() >= MAX_STEPS {
            // This is only a partial main-loop lower bound. Freeze it once
            // the replay cache is full; evicting IDs could charge a duplicate
            // again. The native result's per-result delta remains independent and
            // can still settle the complete call, including subagents.
            return Ok(None);
        }
        self.totals.add(counts)?;
        self.diagnostics.merge(diagnostics);
        self.seen.insert(id.into(), (counts, diagnostics));
        Ok(Some(UsageEvidence {
            counts: self.totals.counts(),
            coverage: UsageCoverage::MainLoopSteps,
            diagnostics: self.diagnostics,
            steps: self.seen.len() as u32,
            models: 0,
        }))
    }
    fn result(&mut self, payload: &Value) -> Result<UsageEvidence, ()> {
        use sha2::{Digest, Sha256};
        let Some(models) = payload.get("model_usage_since_last_result") else {
            // The current CLI's failure frames can omit model deltas. Main
            // loop usage is only a lower bound, never a restored-total guess.
            self.result_diagnostics.missing = true;
            self.result_totals.add(UsageCounts::default())?;
            let (counts, diagnostics) = project(&payload["usage"], false, false);
            let mut checked = Totals::default();
            checked.add(counts)?;
            return Ok(UsageEvidence {
                counts,
                coverage: UsageCoverage::MainLoopResult,
                diagnostics,
                steps: self.seen.len() as u32,
                models: 0,
            });
        };
        let id = payload["uuid"]
            .as_str()
            .filter(|id| super::super::identifier(id))
            .ok_or(())?;
        let fingerprint: [u8; 32] =
            Sha256::digest(serde_json::to_vec(payload).map_err(|_| ())?).into();
        if let Some(previous) = self.result_ids.get(id) {
            if previous != &fingerprint {
                return Err(());
            }
        } else {
            if self.result_ids.len() >= 64 {
                return Err(());
            }
            let models = models.as_object().ok_or(())?;
            if models.len() > MAX_MODELS {
                return Err(());
            }
            let mut totals = Totals::default();
            let mut diagnostics = UsageDiagnostics::default();
            for value in models.values() {
                let (counts, next) = project(value, true, false);
                totals.add(counts)?;
                diagnostics.merge(next);
            }
            self.result_totals.add(totals.counts())?;
            self.result_diagnostics.merge(diagnostics);
            self.result_models = self.result_models.max(models.len() as u16);
            self.result_ids.insert(id.into(), fingerprint);
        }
        Ok(UsageEvidence {
            counts: self.result_totals.counts(),
            coverage: UsageCoverage::ReportedModelsResult,
            diagnostics: self.result_diagnostics,
            steps: self.seen.len() as u32,
            models: self.result_models,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn result(id: &str, input: u64, total: u64) -> Value {
        json!({"type":"result","uuid":id,"is_error":false,"result":"answer",
            "modelUsage":{"main":{"inputTokens":total,"outputTokens":total,
                "cacheReadInputTokens":total,"cacheCreationInputTokens":total}},
            "model_usage_since_last_result":{"main":{"inputTokens":input,"outputTokens":9,
                "cacheReadInputTokens":3,"cacheCreationInputTokens":7,"thinkingTokens":4}}})
    }
    #[test]
    fn native_result_deltas_ignore_restored_totals_and_count_each_uuid_once() {
        for restored in [0, 200] {
            let mut tracker = Tracker::default();
            let first = result("original", 17, restored + 17);
            let initial = tracker.result(&first).unwrap();
            assert_eq!(initial.counts().input(), Some(17));
            assert_eq!(tracker.result(&first).unwrap().counts().input(), Some(17));
            let snapshot = result("background-snapshot", 5, restored + 22);
            let final_usage = tracker.result(&snapshot).unwrap();
            assert_eq!(final_usage.counts().input(), Some(22));
            assert_eq!(final_usage.counts().output(), Some(18));
            assert_eq!(final_usage.counts().cache_read(), Some(6));
            assert_eq!(final_usage.counts().cache_write(), Some(14));
            let replay = tracker.result(&snapshot).unwrap();
            assert_eq!(replay.counts().input(), Some(22));
            assert!(
                tracker
                    .result(&result("original", 18, restored + 17))
                    .is_err()
            );
        }
    }
    #[test]
    fn missing_delta_cannot_be_replaced_by_a_restored_total_or_later_snapshot() {
        let mut tracker = Tracker::default();
        let mut original = result("original", 17, 400);
        original
            .as_object_mut()
            .unwrap()
            .remove("model_usage_since_last_result");
        let partial = tracker.result(&original).unwrap();
        assert!(partial.coverage() == UsageCoverage::MainLoopResult);
        let later = tracker.result(&result("snapshot", 5, 405)).unwrap();
        assert_eq!(later.counts().input(), None);
        assert!(later.diagnostics().has_missing_fields());
        let mut tracker = Tracker::default();
        let mut zero = result("zero", 0, 400);
        zero["model_usage_since_last_result"] = json!({});
        assert_eq!(tracker.result(&zero).unwrap().counts().input(), Some(0));
    }
}
