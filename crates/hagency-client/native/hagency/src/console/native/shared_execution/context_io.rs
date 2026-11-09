//! Closed versioned context routes; descriptions never replace live activity.
use super::*;
use serde_json::{Value, json};
pub(super) enum ContextAction {
    Mode {
        work: String,
        input: SharedWorkEvidenceInput,
    },
    Freeze {
        work: String,
        input: SharedContextInput,
    },
    Recover {
        work: String,
        input: SharedContextRecoverInput,
    },
    Start {
        work: String,
        input: SharedContextStartInput,
    },
    Delivery {
        work: String,
        input: SharedWorkEvidenceInput,
    },
}
impl ContextAction {
    pub fn validate(&self) -> Result<(), NativeError> {
        if !cfg!(feature = "shared-execution") {
            return Err(failure(409, "shared_execution_disabled"));
        }
        let (work, work_digest) = match self {
            Self::Mode { work, input } | Self::Delivery { work, input } => {
                hash(&input.runtime_command_digest)?;
                (work, &input.work_digest)
            }
            Self::Freeze { work, input } => {
                input.authority.validate().map_err(store)?;
                if input.version != 3 { return Err(invalid()); }
                (work, &input.work_digest)
            }
            Self::Recover { work, input } => {
                hash(&input.runtime_command_digest)?;
                hash(&input.context_digest)?;
                (work, &input.work_digest)
            }
            Self::Start { work, input } => {
                input.authority.validate().map_err(store)?;
                hash(&input.context_digest)?;
                hash(&input.input_digest)?;
                (work, &input.work_digest)
            }
        };
        super::super::key(work)?;
        hash(work_digest)
    }
    pub fn body(self) -> (String, Value) {
        match self {
            Self::Mode { work, input } => (format!("work/{work}/input-mode"), json!(input)),
            Self::Freeze { work, input } => (format!("work/{work}/context"), json!(input)),
            Self::Recover { work, input } => (format!("work/{work}/context/recover"), json!(input)),
            Self::Start { work, input } => (format!("work/{work}/context/start"), json!(input)),
            Self::Delivery { work, input } => (format!("work/{work}/reply/observe"), json!(input)),
        }
    }
}
impl From<ContextAction> for Action {
    fn from(v: ContextAction) -> Self {
        Self::Context(Box::new(v))
    }
}

/// Require the active native context contract; no protocol downgrade.
pub(super) async fn paired(b: &Bound) -> Result<(), NativeError> {
    let d = b.demand_discovery().await?;
    if d.capabilities.len() > 64 || d.capabilities.iter().any(|c| c.len() > 128) {
        return Err(invalid());
    }
    if !d.capabilities.iter().any(|c| c == SHARED_CONTEXT_CAPABILITY_V3) {
        return Err(failure(409, "native_context_protocol_required"));
    }
    Ok(())
}
pub(super) async fn prepare(
    io: &Io,
    b: &Bound,
    r: &OwnerResourceRegistry,
    a: VerifiedSharedActivity,
    w: &SharedWorkItem,
    authority: SharedActivityRef,
    until: Instant,
) -> Result<SharedPreparedInput, NativeError> {
    if w.encrypted {
        let text = super::e2ee::input(io, b, w).await?;
        return r.prepare_shared_decrypted_input(a, w.clone(), text).await.map_err(store);
    }
    paired(b).await?;
    let mode: Timed<SharedWorkInputMode> = b
        .upload(
            ContextAction::Mode {
                work: w.id.clone(),
                input: SharedWorkEvidenceInput {
                    work_digest: digest(w)?,
                    runtime_command_digest: w.runtime_command_digest.clone(),
                },
            }
            .into(),
            until,
        )
        .await?;
    mode.value.validate().map_err(store)?;
    let mode = mode.value;
    if mode.mode != "thread_context_v1" {
        return Err(failure(409, "native_context_protocol_required"));
    }
    let context = {
        let action = if let Some(hash) = &mode.context_digest {
            ContextAction::Recover {
                work: w.id.clone(),
                input: SharedContextRecoverInput {
                    work_digest: digest(w)?,
                    runtime_command_digest: w.runtime_command_digest.clone(),
                    context_digest: hash.clone(),
                },
            }
        } else {
            ContextAction::Freeze {
                work: w.id.clone(),
                input: SharedContextInput {
                    authority,
                    work_digest: digest(w)?,
                    version: 3,
                },
            }
        };
        let c: Timed<SharedContextRecord> = b.upload(action.into(), until).await?;
        c.value.render_for(w).map_err(store)?;
        c.value
    };
    r.prepare_shared_input(a, w.clone(), mode, Some(context))
        .await
        .map_err(store)
}
pub(super) async fn start(
    b: &Bound,
    r: &OwnerResourceRegistry,
    attempt: SharedStartAttempt,
    w: &SharedWorkItem,
    input: &SharedPreparedInput,
    authority: SharedActivityRef,
    until: Instant,
) -> Result<SharedCallCustody, NativeError> {
    if let Some(c) = input.context() {
        let start: Timed<SharedContextStart> = b
            .upload(
                ContextAction::Start {
                    work: w.id.clone(),
                    input: SharedContextStartInput {
                        authority,
                        work_digest: digest(w)?,
                        context_digest: c.context_digest.clone(),
                        input_digest: c.input_digest.clone(),
                    },
                }
                .into(),
                until,
            )
            .await?;
        r.issue_shared_context_call(attempt, start.value)
            .await
            .map_err(store)
    } else {
        if !w.encrypted {
            return Err(failure(409, "native_context_protocol_required"));
        }
        let start: Timed<SharedWorkStart> = b
            .upload(
                Action::WorkStart(SharedWorkInput {
                    authority,
                    work_id: w.id.clone(),
                    work_digest: digest(w)?,
                }),
                until,
            )
            .await?;
        r.issue_shared_call(attempt, start.value)
            .await
            .map_err(store)
    }
}
pub(super) async fn observe_delivery(
    b: &Bound,
    r: &OwnerResourceRegistry,
    c: &SharedRuntimeCommand,
    w: &SharedWorkItem,
) -> Result<(), NativeError> {
    paired(b).await?;
    let observed: Timed<SharedReplyObservation> = b
        .request(
            ContextAction::Delivery {
                work: w.id.clone(),
                input: SharedWorkEvidenceInput {
                    work_digest: digest(w)?,
                    runtime_command_digest: w.runtime_command_digest.clone(),
                },
            }
            .into(),
            false,
        )
        .await?;
    observed.value.validate_for(w).map_err(store)?;
    if let Some(record) = observed.value.delivery {
        let a = b.cleanup(r, c).await?;
        r.record_shared_reply_delivery(a, w.clone(), record)
            .await
            .map_err(store)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_execution_feature_enables_the_current_native_context_contract() {
        let manifest: toml::Value = toml::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/Cargo.toml"
        )))
        .unwrap();
        let features = manifest["features"].as_table().unwrap();
        for entry in [
            "shared-execution",
            "shared-successors",
            "shared-intake",
            "shared-context",
            "shared-budget-execution",
        ] {
            let mut pending = vec![entry];
            let mut reached = std::collections::BTreeSet::new();
            while let Some(feature) = pending.pop() {
                if !reached.insert(feature) {
                    continue;
                }
                if let Some(dependencies) = features.get(feature) {
                    pending.extend(
                        dependencies
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|value| value.as_str().unwrap()),
                    );
                }
            }
            for required in [
                "shared-execution",
                "hagency-store/shared-context",
                "hagency-runtime/shared-context",
            ] {
                assert!(
                    reached.contains(required),
                    "{entry} must require {required}"
                );
            }
        }
        if cfg!(feature = "shared-execution") && !cfg!(feature = "shared-intake") {
            use super::super::demand_io::DemandAction;
            assert!(DemandAction::Discovery.validate().is_ok());
            let stop = DemandAction::Stop {
                assignment: "assignment".into(),
                input: super::SharedDemandStopInput {
                    command_id: "stop".into(),
                    expected_revision: 1,
                    mode: "paused".into(),
                },
            };
            assert_eq!(stop.validate().unwrap_err().code, "shared_intake_disabled");
        }
    }
}
