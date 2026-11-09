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
        if !cfg!(feature = "shared-context") {
            return Err(failure(409, "shared_context_disabled"));
        }
        let (work, work_digest) = match self {
            Self::Mode { work, input } | Self::Delivery { work, input } => {
                hash(&input.runtime_command_digest)?;
                (work, &input.work_digest)
            }
            Self::Freeze { work, input } => {
                input.authority.validate().map_err(store)?;
                if let Some(b) = &input.budget {
                    b.validate().map_err(store)?;
                }
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

/// The highest context record version the endpoint advertises, if any.
/// Later versions require all earlier capabilities; lone flags are not trusted.
pub(super) async fn paired(b: &Bound) -> Result<Option<u16>, NativeError> {
    let d = b.demand_discovery().await?;
    if d.capabilities.len() > 64 || d.capabilities.iter().any(|c| c.len() > 128) {
        return Err(invalid());
    }
    let has = |name: &str| d.capabilities.iter().any(|c| c == name);
    Ok(match (has(SHARED_CONTEXT_CAPABILITY), has(SHARED_CONTEXT_CAPABILITY_V2)) {
        (true, true) if has(SHARED_CONTEXT_CAPABILITY_V3) => Some(3),
        (true, true) => Some(2),
        (true, false) => Some(1),
        _ => None,
    })
}
/// A version 2 freeze names the model window this Agent last reported so the
/// server can budget history against it. Nothing here grants model custody.
pub(super) async fn budget(
    r: &OwnerResourceRegistry,
    a: &VerifiedSharedActivity,
    version: u16,
) -> Result<Option<SharedContextBudgetInput>, NativeError> {
    if version < 2 {
        return Ok(None);
    }
    if version == 3 {
        return Ok(Some(SharedContextBudgetInput { version: 3, model_context_window: None }));
    }
    Ok(Some(SharedContextBudgetInput {
        version: 2,
        model_context_window: r.shared_context_window(a).await.map_err(store)?,
    }))
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
    let Some(version) = paired(b).await? else {
        return r
            .prepare_shared_legacy_input(a, w.clone())
            .await
            .map_err(store);
    };
    let budget = budget(r, &a, version).await?;
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
    let context = if mode.mode == "thread_context_v1" {
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
                    budget,
                },
            }
        };
        let c: Timed<SharedContextRecord> = b.upload(action.into(), until).await?;
        c.value.render_for(w).map_err(store)?;
        Some(c.value)
    } else {
        None
    };
    r.prepare_shared_input(a, w.clone(), mode, context)
        .await
        .map_err(store)
}
pub(super) async fn start(
    b: &Bound,
    r: &OwnerResourceRegistry,
    attempt: SharedStartAttempt,
    w: &SharedWorkItem,
    input: Option<&SharedPreparedInput>,
    authority: SharedActivityRef,
    until: Instant,
) -> Result<SharedCallCustody, NativeError> {
    if let Some(c) = input.and_then(SharedPreparedInput::context) {
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
    if !cfg!(feature = "shared-context") || paired(b).await?.is_none() {
        return Ok(());
    }
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
